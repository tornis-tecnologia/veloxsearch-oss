// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The backend half of a Spec 2.0 activation (ADR-074 decisions 3–4, guide
//! §8): the OpenSearch objects one package needs on the deployment, as data.
//! Pure, like [`crate::compile2`]; the core's `integrations.rs` performs the
//! writes, in the order this returns, **before** the revision that makes
//! agents write — documents that arrive before their template get the mapping
//! the template exists to prevent.
//!
//! Per enabled `logs`/`traces` stream, all named `N` =
//! `veloxagent-<stream>.<integration>-v<major>`:
//!
//! 1. component template `N@package` — the package's `index-template.json`
//!    with the `template` bindings applied;
//! 2. component template `N@custom` and ingest pipeline `N@custom`, created
//!    empty **only if absent** — they are the operator's, never overwritten;
//! 3. ingest pipeline `N` — the package's, with `pipeline_toggle` /
//!    `pipeline_param` applied, ending in a `pipeline` processor that calls
//!    `N@custom`;
//! 4. ISM policy `N` from `lifecycle` (rollover, delete), bound to the data
//!    stream's backing indices by `ism_template`;
//! 5. index template `N` — `composed_of: [N@package, N@custom]`,
//!    `data_stream: {}`, `index.default_pipeline: N`;
//! 6. index pattern `N` (packages ship none);
//!
//! then the manifest's dashboards, ids made `veloxagent-<integration>-v<major>-<id>`
//! and `stream:<id>` references resolved to that stream's `N`. Metric streams
//! create nothing (their data goes to the OTel stack).
//!
//! The registry's `scripts/render.py --backend` is the executable spec; the
//! gate at the bottom compares the two on every package.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use velox_integrations::spec2::{self, Activation, Bind, Package};

use crate::compile2::{field_values, visible, yaml_to_json, Checks, Error};

/// What one object is, which decides the endpoint and the write rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `PUT _component_template/<id>` — always written.
    ComponentTemplate,
    /// `PUT _component_template/<id>?create=true` — written only if absent.
    ComponentTemplateIfAbsent,
    /// `PUT _ingest/pipeline/<id>` — always written.
    IngestPipeline,
    /// Written only if `GET _ingest/pipeline/<id>` finds nothing.
    IngestPipelineIfAbsent,
    /// `PUT _plugins/_ism/policies/<id>` (update with seq_no when present).
    IsmPolicy,
    /// `PUT _index_template/<id>`.
    IndexTemplate,
    /// A Dashboards index pattern saved object.
    IndexPattern,
    /// A Dashboards saved object of this type.
    SavedObject(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    pub kind: Kind,
    pub id: String,
    pub body: Value,
}

impl Object {
    /// The key `render.py --backend` prints for the same object — what the
    /// equivalence gate compares.
    pub fn reference_key(&self) -> String {
        match &self.kind {
            Kind::ComponentTemplate => format!("_component_template/{}", self.id),
            Kind::ComponentTemplateIfAbsent => {
                format!("_component_template/{} (create if absent)", self.id)
            }
            Kind::IngestPipeline => format!("_ingest/pipeline/{}", self.id),
            Kind::IngestPipelineIfAbsent => {
                format!("_ingest/pipeline/{} (create if absent)", self.id)
            }
            Kind::IsmPolicy => format!("_plugins/_ism/policies/{}", self.id),
            Kind::IndexTemplate => format!("_index_template/{}", self.id),
            Kind::IndexPattern => format!("index-pattern/{}", self.id),
            Kind::SavedObject(ty) => format!("saved_object/{ty}/{}", self.id),
        }
    }
}

/// The backend of one package as one activation selects it.
#[derive(Debug, Clone, PartialEq)]
pub struct Backend {
    pub package_id: String,
    pub package_version: String,
    pub major: u64,
    /// `N` of every stream that got objects, in package order.
    pub artifacts: Vec<String>,
    /// In installation order.
    pub objects: Vec<Object>,
}

/// Compile the backend objects of one activation. `checks` gates fields by
/// the agents' Fluent Bit (`since`) like the engine compiler does.
pub fn compile(pkg: &Package, act: &Activation, checks: &Checks<'_>) -> Result<Backend, Error> {
    let fail = |w: &str, r: String| Error(format!("{w}: {r}"));
    let major = pkg.manifest.major();
    let mut objects = Vec::new();
    let mut artifacts = Vec::new();
    let mut unused_secrets = BTreeSet::new();

    for stream in &pkg.streams {
        let where_ = format!("{}/{}", pkg.manifest.id, stream.id);
        let sa = act.streams.get(&stream.id);
        if !sa.map(|s| s.enabled).unwrap_or(stream.enabled) {
            continue;
        }
        if !stream.event_type.has_opensearch_backend() {
            continue;
        }
        let Some(be) = &stream.backend else { continue };
        let n = pkg.artifact_name(stream);
        let vals = field_values(
            &be.fields,
            sa.map(|s| &s.backend),
            checks,
            &format!("{where_}.backend"),
            &mut unused_secrets,
        )?;
        let it_name = be
            .index_template
            .as_deref()
            .ok_or_else(|| fail(&where_, "no index_template".into()))?;
        let mut tpl: Value = serde_json::from_str(
            &pkg.stream_asset(stream, it_name)
                .map_err(|e| Error(e.to_string()))?,
        )
        .map_err(|e| fail(&where_, format!("{it_name}: {e}")))?;
        let mut pipe: Option<Value> = match &be.ingest_pipeline {
            Some(p) => Some(
                serde_json::from_str(
                    &pkg.stream_asset(stream, p)
                        .map_err(|e| Error(e.to_string()))?,
                )
                .map_err(|e| fail(&where_, format!("{p}: {e}")))?,
            ),
            None => None,
        };
        let mut life = serde_json::Map::new();
        if let Some(lc) = &be.lifecycle {
            if let Some(v) = &lc.rollover_max_size {
                life.insert("rollover_max_size".into(), json!(v));
            }
            if let Some(v) = &lc.rollover_max_age {
                life.insert("rollover_max_age".into(), json!(v));
            }
            if let Some(v) = lc.rollover_max_docs {
                life.insert("rollover_max_docs".into(), json!(v));
            }
            if let Some(v) = &lc.delete_after {
                life.insert("delete_after".into(), json!(v));
            }
        }
        for f in &be.fields {
            let Some(v) = vals.get(&f.name) else { continue };
            if !visible(f, &vals) {
                continue;
            }
            let v = yaml_to_json(v);
            match &f.bind {
                Bind::Template { template } => {
                    let inner = tpl
                        .get_mut("template")
                        .ok_or_else(|| fail(&where_, "index template has no `template`".into()))?;
                    set_path(inner, template, v);
                }
                Bind::Lifecycle { lifecycle } => {
                    life.insert(lifecycle.name().to_string(), v);
                }
                Bind::PipelineToggle { pipeline_toggle } => {
                    if let (Some(p), Value::Bool(false)) = (pipe.as_mut(), &v) {
                        if let Some(procs) = p.get_mut("processors").and_then(Value::as_array_mut) {
                            procs.retain(|proc_| {
                                processor_tag(proc_) != Some(pipeline_toggle.as_str())
                            });
                        }
                    }
                }
                Bind::PipelineParam { pipeline_param } => {
                    if let Some(p) = pipe.as_mut() {
                        let (tag, param) = pipeline_param.split_once('.').ok_or_else(|| {
                            fail(&where_, format!("pipeline_param {pipeline_param:?}"))
                        })?;
                        if let Some(procs) = p.get_mut("processors").and_then(Value::as_array_mut) {
                            for proc_ in procs.iter_mut() {
                                if processor_tag(proc_) == Some(tag) {
                                    if let Some(body) =
                                        proc_.as_object_mut().and_then(|o| o.values_mut().next())
                                    {
                                        set_path(body, param, v.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                Bind::Key { .. } | Bind::Merge { .. } => {}
            }
        }

        let hooks = be.custom_hooks;
        let custom = format!("{n}@custom");
        objects.push(Object {
            kind: Kind::ComponentTemplate,
            id: format!("{n}@package"),
            body: json!({ "template": tpl.get("template").cloned().unwrap_or(json!({})) }),
        });
        if hooks {
            objects.push(Object {
                kind: Kind::ComponentTemplateIfAbsent,
                id: custom.clone(),
                body: json!({ "template": {} }),
            });
            objects.push(Object {
                kind: Kind::IngestPipelineIfAbsent,
                id: custom.clone(),
                body: json!({ "processors": [] }),
            });
        }
        let has_pipeline = pipe.is_some();
        if let Some(mut p) = pipe {
            if hooks {
                if let Some(procs) = p.get_mut("processors").and_then(Value::as_array_mut) {
                    // No `ignore_missing_pipeline`: that is Elasticsearch's
                    // and OpenSearch refuses the whole pipeline over it (400,
                    // "doesn't support one or more provided configuration
                    // parameters"). Nor is it needed — `N@custom` is created
                    // (if absent) before this pipeline, in the same install.
                    procs.push(json!({ "pipeline": { "name": custom } }));
                }
            }
            objects.push(Object {
                kind: Kind::IngestPipeline,
                id: n.clone(),
                body: p,
            });
        }
        if !life.is_empty() {
            objects.push(Object {
                kind: Kind::IsmPolicy,
                id: n.clone(),
                body: ism_policy(&pkg.manifest.id, &stream.id, major, &n, &life),
            });
        }
        let mut composed = vec![json!(format!("{n}@package"))];
        if hooks {
            composed.push(json!(custom));
        }
        objects.push(Object {
            kind: Kind::IndexTemplate,
            id: n.clone(),
            body: json!({
                "index_patterns": [n],
                "data_stream": {},
                "priority": 200,
                "composed_of": composed,
                "template": if has_pipeline {
                    json!({ "settings": { "index.default_pipeline": n } })
                } else {
                    json!({})
                },
            }),
        });
        objects.push(Object {
            kind: Kind::IndexPattern,
            id: n.clone(),
            body: json!({ "title": n, "timeFieldName": "@timestamp" }),
        });
        artifacts.push(n);
    }

    // Dashboards: package-wide, ids namespaced, stream references resolved.
    let prefix = format!("{}-{}-v{major}-", spec2::ARTIFACT_PREFIX, pkg.manifest.id);
    for rel in &pkg.manifest.dashboards {
        let text = pkg.text(rel).map_err(|e| Error(e.to_string()))?;
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut obj: Value = serde_json::from_str(line)
                .map_err(|e| Error(format!("{rel} line {}: {e}", i + 1)))?;
            let ty = obj
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| Error(format!("{rel} line {}: no type", i + 1)))?
                .to_string();
            let id = obj
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error(format!("{rel} line {}: no id", i + 1)))?;
            let new_id = format!("{prefix}{id}");
            obj["id"] = json!(new_id);
            if let Some(refs) = obj.get_mut("references").and_then(Value::as_array_mut) {
                for r in refs {
                    let rid = r.get("id").map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    });
                    let Some(rid) = rid else { continue };
                    let resolved = match rid.strip_prefix("stream:") {
                        Some(sid) => {
                            let s = pkg.stream(sid).ok_or_else(|| {
                                Error(format!("{rel}: reference to unknown stream {sid:?}"))
                            })?;
                            pkg.artifact_name(s)
                        }
                        None => format!("{prefix}{rid}"),
                    };
                    r["id"] = json!(resolved);
                }
            }
            objects.push(Object {
                kind: Kind::SavedObject(ty),
                id: new_id,
                body: obj,
            });
        }
    }

    Ok(Backend {
        package_id: pkg.manifest.id.clone(),
        package_version: pkg.manifest.version.clone(),
        major,
        artifacts,
        objects,
    })
}

/// The `tag` of an ingest-pipeline processor (`{"<kind>": {"tag": …}}`).
fn processor_tag(p: &Value) -> Option<&str> {
    p.as_object()?.values().next()?.get("tag")?.as_str()
}

/// Set a dotted path, creating objects on the way (render.py's `set_path`).
fn set_path(obj: &mut Value, dotted: &str, value: Value) {
    let parts: Vec<&str> = dotted.split('.').collect();
    let mut cur = obj;
    for p in &parts[..parts.len() - 1] {
        if !cur.get(*p).is_some_and(Value::is_object) {
            cur[*p] = json!({});
        }
        cur = cur.get_mut(*p).expect("just inserted");
    }
    cur[parts[parts.len() - 1]] = value;
}

/// A package's lifecycle key as the OpenSearch ISM `rollover` condition it
/// means. The package speaks Elasticsearch's thresholds ("roll over at most
/// at this size"), ISM names the same trigger by its lower bound ("once the
/// index is at least this big"); a mechanical `min_` + key produced
/// `min_max_size`, which ISM refuses for every policy.
fn ism_rollover_condition(key: &str) -> Option<&'static str> {
    match key {
        "rollover_max_size" => Some("min_size"),
        "rollover_max_age" => Some("min_index_age"),
        "rollover_max_docs" => Some("min_doc_count"),
        _ => None,
    }
}

/// The ISM policy for one stream: hot with rollover, then delete.
fn ism_policy(
    integration: &str,
    stream: &str,
    major: u64,
    n: &str,
    life: &serde_json::Map<String, Value>,
) -> Value {
    let mut actions = Vec::new();
    let roll: serde_json::Map<String, Value> = life
        .iter()
        .filter_map(|(k, v)| ism_rollover_condition(k).map(|c| (c.to_string(), v.clone())))
        .collect();
    if !roll.is_empty() {
        actions.push(json!({ "rollover": roll }));
    }
    let mut states = vec![json!({ "name": "hot", "actions": actions, "transitions": [] })];
    if let Some(after) = life.get("delete_after") {
        states[0]["transitions"] =
            json!([{ "state_name": "delete", "conditions": { "min_index_age": after } }]);
        states.push(json!({ "name": "delete", "actions": [{ "delete": {} }], "transitions": [] }));
    }
    json!({
        "policy": {
            "description": format!("{integration}/{stream} v{major}"),
            "default_state": "hot",
            "states": states,
            "ism_template": [{ "index_patterns": [format!(".ds-{n}-*")], "priority": 100 }],
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// OpenSearch ISM refuses `min_max_size` & co. (400 "Invalid field ... in
    /// RolloverAction") — measured against OpenSearch 3.3 for every package.
    /// The package's thresholds map onto ISM's own condition names.
    #[test]
    fn ism_rollover_uses_opensearch_condition_names() {
        let mut life = serde_json::Map::new();
        life.insert("rollover_max_size".into(), json!("50gb"));
        life.insert("rollover_max_age".into(), json!("1d"));
        life.insert("rollover_max_docs".into(), json!(1000));
        life.insert("delete_after".into(), json!("30d"));
        let p = ism_policy("system", "syslog", 1, "veloxagent-syslog.system-v1", &life);
        let roll = &p["policy"]["states"][0]["actions"][0]["rollover"];
        assert_eq!(roll["min_size"], "50gb");
        assert_eq!(roll["min_index_age"], "1d");
        assert_eq!(roll["min_doc_count"], 1000);
        let text = p.to_string();
        assert!(
            !text.contains("min_max_") && !text.contains("min_rollover"),
            "{text}"
        );
        assert_eq!(
            p["policy"]["states"][0]["transitions"][0]["conditions"]["min_index_age"],
            "30d"
        );
    }

    fn registry_root() -> Option<PathBuf> {
        match std::env::var("VELOX_REGISTRY_PATH") {
            Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
            _ => {
                eprintln!("SKIPPED backend2 equivalence gate: VELOX_REGISTRY_PATH is not set");
                None
            }
        }
    }

    fn render_backend(root: &Path, dir: &Path, act: &Activation) -> Option<Value> {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("velox-backend2-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("activation.json");
        std::fs::write(&p, serde_json::to_string(act).unwrap()).unwrap();
        let out = std::process::Command::new("python3")
            .arg("-I")
            .arg(root.join("scripts/render.py"))
            .arg(dir)
            .args(["--backend", "--format", "json", "--activation"])
            .arg(&p)
            .output()
            .ok()?;
        let _ = std::fs::remove_dir_all(&tmp);
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("No module named") {
                eprintln!("SKIPPED render.py --backend comparison: {err}");
                return None;
            }
            panic!("render.py --backend failed for {}: {err}", dir.display());
        }
        Some(serde_json::from_slice(&out.stdout).unwrap())
    }

    fn as_reference(b: &Backend) -> Value {
        Value::Object(
            b.objects
                .iter()
                .map(|o| (o.reference_key(), o.body.clone()))
                .collect(),
        )
    }

    /// Every package, every stream on, equals `render.py --backend`.
    #[test]
    fn every_registry_backend_matches_the_reference() {
        let Some(root) = registry_root() else { return };
        let index = spec2::Index::parse(
            &std::fs::read_to_string(root.join("packages/index.json")).unwrap(),
        )
        .unwrap();
        let (mut n, mut compared) = (0, 0);
        for p in &index.packages {
            for v in &p.versions {
                let dir = root.join("packages").join(&v.path);
                let pkg = Package::load_from_dir(&dir).unwrap();
                let act = Activation {
                    package_id: pkg.manifest.id.clone(),
                    package_version: pkg.manifest.version.clone(),
                    spec_version: "2.0".into(),
                    streams: pkg
                        .streams
                        .iter()
                        .map(|s| {
                            (
                                s.id.clone(),
                                spec2::StreamActivation {
                                    enabled: true,
                                    ..Default::default()
                                },
                            )
                        })
                        .collect(),
                };
                let ours = compile(&pkg, &act, &Checks::default())
                    .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
                n += 1;
                if let Some(theirs) = render_backend(&root, &dir, &act) {
                    assert_eq!(
                        as_reference(&ours),
                        theirs,
                        "{} differs from render.py --backend",
                        dir.display()
                    );
                    compared += 1;
                }
            }
        }
        eprintln!("backend2 gate: {n} packages, {compared} compared with render.py --backend");
        assert!(n >= 30);
    }

    /// Backend fields: a lifecycle override, a pipeline toggle off, and the
    /// order the core installs in.
    #[test]
    fn backend_fields_apply_and_order_holds() {
        let Some(root) = registry_root() else { return };
        let dir = root.join("packages/nginx/1.0.0");
        let pkg = Package::load_from_dir(&dir).unwrap();
        let act: Activation = serde_json::from_str(
            r#"{"package_id":"nginx","package_version":"1.0.0","spec_version":"2.0","streams":{
               "access":{"enabled":true,"backend":{"delete_after":"14d","parse_user_agent":false}},"error":{"enabled":false}}}"#,
        )
        .unwrap();
        let b = compile(&pkg, &act, &Checks::default()).unwrap();
        assert_eq!(b.artifacts, vec!["veloxagent-access.nginx-v1"]);
        let kinds: Vec<String> = b
            .objects
            .iter()
            .take(7)
            .map(|o| o.reference_key())
            .collect();
        assert_eq!(
            kinds,
            [
                "_component_template/veloxagent-access.nginx-v1@package",
                "_component_template/veloxagent-access.nginx-v1@custom (create if absent)",
                "_ingest/pipeline/veloxagent-access.nginx-v1@custom (create if absent)",
                "_ingest/pipeline/veloxagent-access.nginx-v1",
                "_plugins/_ism/policies/veloxagent-access.nginx-v1",
                "_index_template/veloxagent-access.nginx-v1",
                "index-pattern/veloxagent-access.nginx-v1",
            ]
        );
        let ism = &b.objects[4].body;
        assert_eq!(
            ism["policy"]["states"][0]["transitions"][0]["conditions"]["min_index_age"],
            "14d"
        );
        let pipe = b.objects[3].body["processors"].as_array().unwrap();
        assert!(pipe.iter().all(|p| processor_tag(p) != Some("user_agent")));
        assert_eq!(
            pipe.last().unwrap()["pipeline"]["name"],
            "veloxagent-access.nginx-v1@custom"
        );
        if let Some(theirs) = render_backend(&root, &dir, &act) {
            assert_eq!(as_reference(&b), theirs);
        }
    }
}
