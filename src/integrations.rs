// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! ADR-039 integration apply-engine — the half that touches the cluster.
//!
//! One fixed engine that installs / uninstalls a **data-only** integration
//! package (a `manifest.yaml` + four assets) against a deployment's OpenSearch,
//! reusing the exact plumbing the built-in recipes use: `recipes::os_base` /
//! `http` / `ensure_tenant` / `dashboards_base`, `k8s::admin_creds`, and the
//! agent apply in `agents`. It generalizes the per-recipe `configure_*`
//! branching (the `match recipe` arm) into a single sequence fed by verified
//! data, and — unlike the old `disable()` — tears down exactly what a package
//! creates, so a clean install yields a clean uninstall.
//!
//! The contract itself — manifest, closed token set, renderer, resource
//! accounting — lives in the shared `velox-integrations` crate
//! (`crates/velox-integrations`, ADR-068) and is re-exported here unchanged,
//! so `integrations::render` / `integrations::CLOSED_TOKENS` keep their
//! names. Velox Mesh renders policies with that same crate; this module is
//! what only the core does: resolve the variables off the live deployment and
//! perform the OpenSearch writes. The signature-verification and registry-load
//! layers (#71/#72, `catalog.rs`) sit *in front* of this engine (a `Package`
//! here is already verified + parsed).

use crate::scope::Deployment;
use anyhow::{Context, Result};
use std::collections::BTreeMap;

pub use velox_integrations::{
    apply_variables, created_resources, parse_manifest, parse_saved_objects, render,
    resolve_variables, saved_objects_bulk, teardown_resources, Assets, Manifest, Package,
    ResourceKey, ResourceKind, Teardown, VarType, Variable, Vars, CLOSED_TOKENS,
};

/// The closed variable set for one deployment × package, resolved off the
/// `Deployment` handle. The namespace comes off the handle rather than being
/// passed alongside it (#80): with per-tenant namespaces two sources could
/// disagree, and the one that wins would decide which cluster a collector
/// ships to. `os_host` and `tenant` are derived inside the shared crate, the
/// same way for the DaemonSet path and for Velox Mesh.
pub fn vars(
    deployment: &Deployment,
    index: &str,
    recipe_id: &str,
    os_user: &str,
    os_password: &str,
) -> Vars {
    Vars::new(
        &deployment.to_string(),
        deployment.namespace(),
        index,
        recipe_id,
        os_user,
        os_password,
    )
}

// ---------------------------------------------------------------------------
// Apply / uninstall executors.
//
// Both collection paths install through here, under the same names (ADR-071):
// every artifact is `veloxagent-<integration>-<signal>-v<major>`, and every
// collector writes with `veloxagent-user` (ADR-070). The admin login is the
// core's, used for the writes below and never handed to a collector.
// ---------------------------------------------------------------------------

/// Install the backend half of a package — ingest pipeline and index
/// template per enabled input, index patterns, dashboards — under the renamed
/// ids, and return the renaming so the caller's collector config points at
/// the same indices.
///
/// Every asset is rendered (and therefore interpolation-checked) before the
/// first write: a foreign `{token}` aborts the whole install with nothing
/// half-applied.
async fn install_backend(
    deployment: &Deployment,
    pkg: &Package,
    version: &str,
    selection: &velox_integrations::InputSelection,
) -> Result<velox_integrations::AgentRenaming> {
    let renaming = velox_integrations::agent_renaming(pkg, version)?;
    // Backend assets carry no credential tokens that reach a collector — the
    // admin login renders only into requests this process makes — but the
    // closed set is rendered in full so a package cannot smuggle a token past
    // the check by putting it in a file the collector never reads.
    let (u, p) = crate::k8s::admin_creds(deployment).await;

    // Per enabled input: (artifact name, pipeline body, template body).
    let mut per_input = Vec::new();
    for (input, _) in selection.enabled(pkg)? {
        let name = velox_integrations::input_artifact_name(pkg, &input, version);
        let vars = vars(deployment, &name, &pkg.manifest.id, &u, &p);
        let pipeline = match input.pipeline {
            Some(s) => Some(render(s, &vars)?),
            None => None,
        };
        let index_template = match input.index_template {
            Some(s) => Some(velox_integrations::agent_index_template_for(
                &render(s, &vars)?,
                &renaming,
                &name,
            )?),
            None => None,
        };
        per_input.push((name, pipeline, index_template));
    }
    let base = vars(deployment, &renaming.name, &pkg.manifest.id, &u, &p);
    let mut saved_objects =
        velox_integrations::agent_saved_objects(&render(&pkg.saved_objects, &base)?, &renaming)?;
    // Each enabled input's own saved objects, rendered with that input's
    // index as `{index}`.
    for (input, _) in selection.enabled(pkg)? {
        if let Some(nd) = input.saved_objects {
            let name = velox_integrations::input_artifact_name(pkg, &input, version);
            let v = vars(deployment, &name, &pkg.manifest.id, &u, &p);
            saved_objects.push_str(&velox_integrations::agent_saved_objects(
                &render(nd, &v)?,
                &renaming,
            )?);
        }
    }

    let base = crate::recipes::os_base(deployment);
    let c = crate::recipes::http()?;

    for (name, pipeline, index_template) in &per_input {
        if let Some(body) = pipeline {
            let json: serde_json::Value =
                serde_json::from_str(body).context("pipeline.json is not valid JSON")?;
            c.put(format!("{base}/_ingest/pipeline/{name}"))
                .basic_auth(&u, Some(&p))
                .json(&json)
                .send()
                .await
                .context("creating ingest pipeline")?
                .error_for_status()
                .context("ingest pipeline rejected")?;
        }
        if let Some(body) = index_template {
            let json: serde_json::Value =
                serde_json::from_str(body).context("index-template.json is not valid JSON")?;
            c.put(format!("{base}/_index_template/{name}"))
                .basic_auth(&u, Some(&p))
                .json(&json)
                .send()
                .await
                .context("creating index template")?
                .error_for_status()
                .context("index template rejected")?;
        }
    }

    // Saved objects into the deployment's own Dashboards tenant (best-effort,
    // exactly like ensure_tenant/ensure_dashboard: a cluster without
    // multitenancy still gets the data, it just lands in Global). When the
    // ADR-049 stack owns the Dashboards, scoping is by workspace instead and
    // there is no tenant to create or name (see `recipes::tenant_scope`).
    let scope = crate::recipes::tenant_scope(deployment).await;
    if scope.is_some() {
        crate::recipes::ensure_tenant(deployment).await;
    }
    let objs = saved_objects_bulk(&saved_objects)?;
    let dash = crate::recipes::dashboards_base(deployment);
    let mut req = c
        .post(format!(
            "{dash}/api/saved_objects/_bulk_create?overwrite=true"
        ))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true");
    if let Some(t) = &scope {
        req = req.header("securitytenant", t);
    }
    let _ = req.json(&objs).send().await;
    Ok(renaming)
}

/// The collector config a package renders to on the in-cluster path: every
/// enabled input's cluster template, assembled under one `[SERVICE]`, each
/// writing to its renamed index as `veloxagent-user`, with the password as
/// the `${VAR}` reference the pod resolves from its Secret — the value is
/// never rendered into text that could reach a ConfigMap.
///
/// `None` when no enabled input collects in the cluster (a package whose
/// inputs are host-only): nothing to deploy, and not an error.
pub(crate) fn collector_config(
    deployment: &Deployment,
    pkg: &Package,
    version: &str,
    selection: &velox_integrations::InputSelection,
) -> Result<Option<String>> {
    let vars_for = |index: &str| {
        vars(
            deployment,
            index,
            &pkg.manifest.id,
            crate::os_agent_user::AGENT_USER,
            crate::agents::PASSWORD_REF,
        )
    };
    // Variables: the operator's values for this cluster, defaults for the
    // rest; a `required` variable with neither refuses the install by name,
    // before anything is written.
    let rendered = velox_integrations::render_inputs(
        pkg,
        version,
        selection,
        velox_integrations::TargetKind::Cluster,
        &vars_for,
    )?;
    let bodies: Vec<&str> = rendered
        .iter()
        .filter(|r| r.engine == velox_integrations::EngineKind::FluentBit)
        .map(|r| r.body.as_str())
        .collect();
    if bodies.is_empty() {
        return Ok(None);
    }
    Ok(Some(velox_integrations::assemble_fluent_bit(
        &velox_integrations::fluent_bit_service(5, None),
        &bodies,
    )))
}

/// Install a package end-to-end on the in-cluster path: the backend half under
/// the `veloxagent-` names, then the collector, writing with `veloxagent-user`.
/// `selection` is what the operator turned on for this cluster (ADR-073).
pub async fn apply_package(
    deployment: &Deployment,
    pkg: &Package,
    version: &str,
    selection: &velox_integrations::InputSelection,
) -> Result<()> {
    // The collector config first, so a package that cannot render fails
    // before the backend half is touched.
    let agent_config = collector_config(deployment, pkg, version, selection)?;
    // The account before the collector that logs in with it.
    crate::os_agent_user::ensure(deployment).await?;
    install_backend(deployment, pkg, version, selection).await?;

    match agent_config {
        Some(conf) => {
            // A config that tails host files uses the Fluent Bit `tail` input
            // (DaemonSet); an API-server watcher (`kubernetes_events`) does
            // not (single-replica Deployment) — read off the config.
            let tails_logs = conf.contains("Name tail");
            crate::agents::deploy_rendered_agent(deployment, &pkg.manifest.id, &conf, tails_logs)
                .await?;
        }
        // Nothing of this selection collects in the cluster: a collector left
        // from an earlier selection would keep collecting what was turned off.
        None => {
            let _ = crate::agents::remove_agent(deployment, &pkg.manifest.id).await;
        }
    }
    Ok(())
}

/// Install the **backend half** of an activation for a Velox Mesh policy: the
/// same artifacts as [`apply_package`], and no collector — the edge half runs
/// on a host, from the compiled policy the Mesh serves. Deploying a DaemonSet
/// here would collect everything twice.
pub async fn apply_package_for_agent(
    deployment: &Deployment,
    pkg: &Package,
    version: &str,
    selection: &velox_integrations::InputSelection,
) -> Result<()> {
    install_backend(deployment, pkg, version, selection)
        .await
        .map(|_| ())
}

/// What installing `pkg` at `version` put in OpenSearch, as a teardown set
/// under the renamed ids — every input's, whether or not it was enabled, so a
/// selection that changed between install and uninstall leaves nothing
/// behind — with each saved object's type for the Dashboards DELETE API.
fn renamed_teardown(pkg: &Package, version: &str) -> Result<(Teardown, BTreeMap<String, String>)> {
    let r = velox_integrations::agent_renaming(pkg, version)?;
    let mut kinds = BTreeMap::new();
    let mut saved = Vec::new();
    for o in velox_integrations::all_saved_objects(pkg)? {
        let id = r.map.get(&o.id).cloned().unwrap_or(o.id);
        if o.kind != "index-pattern" {
            saved.push(id.clone());
        }
        kinds.insert(id, o.kind);
    }
    let mut pipelines = Vec::new();
    let mut templates = Vec::new();
    let mut patterns = Vec::new();
    for i in pkg.inputs() {
        let name = velox_integrations::input_artifact_name(pkg, &i, version);
        if i.pipeline.is_some() {
            pipelines.push(name.clone());
        }
        if i.index_template.is_some() {
            templates.push(name.clone());
        }
        if name != r.name {
            patterns.push(name);
        }
    }
    Ok((
        Teardown {
            ingest_pipeline: None,
            index_template: None,
            ingest_pipelines: pipelines,
            index_templates: templates,
            index_pattern: r.name.clone(),
            index_patterns: patterns,
            saved_objects: saved,
            ism_policies: Vec::new(),
            datasources: Vec::new(),
        },
        kinds,
    ))
}

/// Uninstall a package from the cluster target: remove its collector and,
/// when `backend` is set, delete what it created.
///
/// The backend half is shared by both targets (ADR-073): a policy whose hosts
/// still write through this integration's pipeline needs it to stay, so the
/// caller decides `backend` from what else is still using the integration.
///
/// Two teardown sets, and both run: the renamed one this core installs, and
/// the package's **declared** one, which is what a deployment installed before
/// ADR-071 still carries (`nginx` pipeline, `nginx-logs` template,
/// `velox-nginx-*` dashboards). Deleting only the new set would leave the old
/// one behind forever; best-effort per object means the set that is absent
/// costs a 404 and nothing else. Indices are never deleted — they are data,
/// and retention owns them.
pub async fn uninstall_package(
    deployment: &Deployment,
    pkg: &Package,
    version: &str,
    backend: bool,
) -> Result<()> {
    let _ = crate::agents::remove_agent(deployment, &pkg.manifest.id).await;
    if backend {
        teardown_backend(deployment, pkg, version).await?;
    }
    Ok(())
}

/// Delete the backend half of a package: the renamed set and the declared
/// one. No collector is touched — see [`uninstall_package`].
pub async fn teardown_backend(deployment: &Deployment, pkg: &Package, version: &str) -> Result<()> {
    let (renamed, renamed_kinds) = renamed_teardown(pkg, version)?;
    teardown_os(deployment, &renamed, &renamed_kinds).await;
    let legacy_kinds: BTreeMap<String, String> = velox_integrations::all_saved_objects(pkg)?
        .into_iter()
        .map(|o| (o.id, o.kind))
        .collect();
    teardown_os(deployment, &pkg.manifest.teardown, &legacy_kinds).await;
    Ok(())
}

/// Delete exactly a teardown set from a deployment's OpenSearch (pipeline,
/// template, saved objects, index-pattern). Best-effort per object — one
/// missing object never blocks the rest, so an idempotent re-uninstall or a
/// partially-installed package still converges to clean. `kinds` maps each
/// saved-object id to its type for the Dashboards DELETE path.
///
/// Shared by `uninstall_package` (manifest-driven) and `recipes::disable`
/// (in-binary recipe teardown) so both remove objects identically.
pub(crate) async fn teardown_os(
    deployment: &Deployment,
    t: &Teardown,
    kinds: &BTreeMap<String, String>,
) {
    let base = crate::recipes::os_base(deployment);
    let dash = crate::recipes::dashboards_base(deployment);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    // `None` once the stack owns the Dashboards: the objects are workspace-
    // scoped and a `securitytenant` header would point the delete at a tenant
    // that was never created, leaving the real objects behind.
    let tenant = crate::recipes::tenant_scope(deployment).await;

    for id in t.ingest_pipeline.iter().chain(t.ingest_pipelines.iter()) {
        let _ = c
            .delete(format!("{base}/_ingest/pipeline/{id}"))
            .basic_auth(&u, Some(&p))
            .send()
            .await;
    }
    for id in t.index_template.iter().chain(t.index_templates.iter()) {
        let _ = c
            .delete(format!("{base}/_index_template/{id}"))
            .basic_auth(&u, Some(&p))
            .send()
            .await;
    }
    // Dashboard + visualizations (type from the package's ndjson; default to
    // "visualization" if unknown — the common case for a viz id).
    for id in &t.saved_objects {
        let kind = kinds.get(id).map(String::as_str).unwrap_or("visualization");
        let req = c
            .delete(format!("{dash}/api/saved_objects/{kind}/{id}"))
            .basic_auth(&u, Some(&p))
            .header("osd-xsrf", "true");
        let req = match &tenant {
            Some(t) => req.header("securitytenant", t),
            None => req,
        };
        let _ = req.send().await;
    }
    // Index-patterns (their own saved-object type). `index_pattern` is the
    // recipe-era single one; `index_patterns` carries any extras (ADR-053).
    for id in std::iter::once(&t.index_pattern).chain(t.index_patterns.iter()) {
        let req = c
            .delete(format!("{dash}/api/saved_objects/index-pattern/{id}"))
            .basic_auth(&u, Some(&p))
            .header("osd-xsrf", "true");
        let req = match &tenant {
            Some(t) => req.header("securitytenant", t),
            None => req,
        };
        let _ = req.send().await;
    }
    // ISM policies: detach from the live indices first, or the DELETE leaves
    // every matched index still pointing at a policy that no longer exists.
    for id in &t.ism_policies {
        let _ = c
            .post(format!("{base}/_plugins/_ism/remove/{id}"))
            .basic_auth(&u, Some(&p))
            .send()
            .await;
        let _ = c
            .delete(format!("{base}/_plugins/_ism/policies/{id}"))
            .basic_auth(&u, Some(&p))
            .send()
            .await;
    }
    // SQL-plugin datasources. A datasource reports a `resultIndex` name
    // (`query_execution_result_<name>`) but does NOT create that index on
    // registration — it is materialized lazily by async query execution, which
    // this feature never issues. Verified live on OpenSearch 3.8.0: create,
    // then `_cat/indices/query_execution_result*` is still empty.
    for name in &t.datasources {
        let _ = c
            .delete(format!("{base}/_plugins/_query/_datasources/{name}"))
            .basic_auth(&u, Some(&p))
            .send()
            .await;
    }
}

// ---------------------------------------------------------------------------
// Spec 2.0 backend (ADR-074 decisions 3–4): the objects `velox_policy::backend2`
// computes, written in its order with the admin login, before the revision.
// ---------------------------------------------------------------------------

/// Install one activation's Spec 2.0 backend. Idempotent: every write is a
/// PUT of the same body, the operator's `@custom` objects are created only
/// when absent, and the data stream is created only when missing. The data
/// streams are created **here**, with the admin login, so the collector
/// account needs no data-stream admin action — only writes on the backing
/// indices (`os_agent_user::role_body`).
///
/// Stops at the first failed write: a half-installed backend is reported, and
/// the save that called this does not store the revision that would make
/// agents write into it.
pub async fn install_backend_v2(
    deployment: &Deployment,
    backend: &velox_policy::backend2::Backend,
) -> Result<()> {
    use velox_policy::backend2::Kind;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let base = crate::recipes::os_base(deployment);
    let c = crate::recipes::http()?;
    let what = |k: &Kind, id: &str| {
        format!(
            "{k:?} {id} ({}@{})",
            backend.package_id, backend.package_version
        )
    };

    let mut saved_objects = Vec::new();
    for o in &backend.objects {
        let path = match &o.kind {
            Kind::ComponentTemplate | Kind::ComponentTemplateIfAbsent => {
                format!("{base}/_component_template/{}", o.id)
            }
            Kind::IngestPipeline | Kind::IngestPipelineIfAbsent => {
                format!("{base}/_ingest/pipeline/{}", o.id)
            }
            Kind::IsmPolicy => format!("{base}/_plugins/_ism/policies/{}", o.id),
            Kind::IndexTemplate => format!("{base}/_index_template/{}", o.id),
            Kind::IndexPattern => {
                saved_objects.push(serde_json::json!({
                    "type": "index-pattern",
                    "id": o.id,
                    "attributes": o.body,
                }));
                continue;
            }
            Kind::SavedObject(_) => {
                saved_objects.push(o.body.clone());
                continue;
            }
        };
        let mut url = path.clone();
        match &o.kind {
            // The operator's hooks: never overwritten once they exist.
            Kind::ComponentTemplateIfAbsent | Kind::IngestPipelineIfAbsent => {
                let probe = c
                    .get(&path)
                    .basic_auth(&u, Some(&p))
                    .send()
                    .await
                    .with_context(|| format!("reading {}", what(&o.kind, &o.id)))?;
                if probe.status().is_success() {
                    continue;
                }
            }
            // ISM refuses a blind PUT over an existing policy; update it
            // under optimistic concurrency with what is there.
            Kind::IsmPolicy => {
                let probe = c.get(&path).basic_auth(&u, Some(&p)).send().await;
                if let Ok(r) = probe {
                    if r.status().is_success() {
                        let cur: serde_json::Value = r.json().await.unwrap_or_default();
                        if let (Some(seq), Some(term)) =
                            (cur["_seq_no"].as_u64(), cur["_primary_term"].as_u64())
                        {
                            url = format!("{path}?if_seq_no={seq}&if_primary_term={term}");
                        }
                    }
                }
            }
            _ => {}
        }
        let res = c
            .put(&url)
            .basic_auth(&u, Some(&p))
            .json(&o.body)
            .send()
            .await
            .with_context(|| format!("writing {}", what(&o.kind, &o.id)))?;
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            anyhow::bail!("writing {}: {status} {body}", what(&o.kind, &o.id));
        }
        // After its template exists, the data stream itself — created now so
        // the first bulk from an agent never needs to create it.
        if o.kind == Kind::IndexTemplate {
            let ds = format!("{base}/_data_stream/{}", o.id);
            let exists = c
                .get(&ds)
                .basic_auth(&u, Some(&p))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            if !exists {
                let res = c
                    .put(&ds)
                    .basic_auth(&u, Some(&p))
                    .send()
                    .await
                    .with_context(|| format!("creating data stream {}", o.id))?;
                if !res.status().is_success() {
                    let status = res.status();
                    let body = res.text().await.unwrap_or_default();
                    // A race with another save is the only benign failure.
                    if !body.contains("already_exists") {
                        anyhow::bail!("creating data stream {}: {status} {body}", o.id);
                    }
                }
            }
        }
    }

    // Index patterns and dashboards into the deployment's Dashboards, the
    // same tenant rule as the 1.x path (best-effort: the data lands either
    // way, and a Dashboards that is restarting must not fail a save).
    if !saved_objects.is_empty() {
        let scope = crate::recipes::tenant_scope(deployment).await;
        if scope.is_some() {
            crate::recipes::ensure_tenant(deployment).await;
        }
        let dash = crate::recipes::dashboards_base(deployment);
        let mut req = c
            .post(format!(
                "{dash}/api/saved_objects/_bulk_create?overwrite=true"
            ))
            .basic_auth(&u, Some(&p))
            .header("osd-xsrf", "true");
        if let Some(t) = &scope {
            req = req.header("securitytenant", t);
        }
        let _ = req.json(&saved_objects).send().await;
    }
    Ok(())
}

/// Remove one activation's Spec 2.0 backend once nothing on the deployment
/// uses that package's major any more (ADR-074 decision 4). The data and the
/// operator's `@custom` objects stay; the data stream keeps its index
/// template and `@package` component (OpenSearch refuses to delete a
/// template a live data stream matches, and the data is the operator's to
/// delete). Best-effort per object, like the 1.x teardown.
pub async fn teardown_backend_v2(
    deployment: &Deployment,
    backend: &velox_policy::backend2::Backend,
) -> Result<()> {
    use velox_policy::backend2::Kind;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let base = crate::recipes::os_base(deployment);
    let dash = crate::recipes::dashboards_base(deployment);
    let c = crate::recipes::http()?;
    let scope = crate::recipes::tenant_scope(deployment).await;
    for o in &backend.objects {
        let req = match &o.kind {
            Kind::IngestPipeline => c.delete(format!("{base}/_ingest/pipeline/{}", o.id)),
            Kind::IsmPolicy => c.delete(format!("{base}/_plugins/_ism/policies/{}", o.id)),
            Kind::IndexPattern => c
                .delete(format!("{dash}/api/saved_objects/index-pattern/{}", o.id))
                .header("osd-xsrf", "true"),
            Kind::SavedObject(ty) => c
                .delete(format!("{dash}/api/saved_objects/{ty}/{}", o.id))
                .header("osd-xsrf", "true"),
            Kind::ComponentTemplate
            | Kind::ComponentTemplateIfAbsent
            | Kind::IngestPipelineIfAbsent
            | Kind::IndexTemplate => continue,
        };
        let mut req = req.basic_auth(&u, Some(&p));
        if let (Some(t), Kind::IndexPattern | Kind::SavedObject(_)) = (&scope, &o.kind) {
            req = req.header("securitytenant", t);
        }
        let _ = req.send().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    /// What an in-cluster collector is configured with: the collector account,
    /// the password as a reference the pod resolves, and the renamed index.
    /// The admin login appearing here is the bug this exists to prevent.
    #[test]
    fn the_in_cluster_collector_never_sees_the_admin_login() {
        let dep = Deployment::for_test("logs-a1b2", "veloxsearch", None);
        for r in crate::recipes::RECIPES {
            let pkg = crate::recipes::bootstrap_package(r).unwrap();
            let conf = collector_config(&dep, &pkg, "1.0.0", &Default::default())
                .unwrap()
                .expect("every built-in recipe collects in the cluster");
            // Assembly reproduces the recipe's own text: one SERVICE block,
            // the recipe's, then its stanzas untouched.
            assert!(
                conf.starts_with("[SERVICE]\n    Flush 5\n    Log_Level info\n[INPUT]"),
                "{r}: {conf}"
            );
            assert_eq!(conf.matches("[SERVICE]").count(), 1, "{r}");
            assert!(conf.contains("HTTP_User veloxagent-user"), "{r}: {conf}");
            assert!(
                conf.contains("HTTP_Passwd ${VELOX_OS_PASSWORD}"),
                "{r}: {conf}"
            );
            assert!(!conf.contains("HTTP_User admin"), "{r}: {conf}");
            let want = velox_integrations::agent_artifact_name(r, &pkg.manifest.index, "1.0.0");
            assert!(conf.contains(&format!("Index {want}")), "{r}: {conf}");
        }
    }

    /// Uninstall removes the renamed set, and the renamed set is the same size
    /// as what the package declares — a rename that dropped an object would
    /// leave it behind forever.
    #[test]
    fn the_renamed_teardown_covers_everything_installed() {
        for r in crate::recipes::RECIPES {
            let pkg = crate::recipes::bootstrap_package(r).unwrap();
            let (t, kinds) = renamed_teardown(&pkg, "1.0.0").unwrap();
            let got = teardown_resources(&t);
            let want = velox_integrations::agent_created_resources(&pkg, "1.0.0").unwrap();
            assert_eq!(got, want, "{r}: teardown != install");
            for id in &t.saved_objects {
                assert!(
                    kinds.contains_key(id),
                    "{r}: {id} has no type for the DELETE API"
                );
            }
        }
    }
    use super::*;

    /// Path to the CANONICAL nginx registry package (#74) — the frozen
    /// extraction of the built-in nginx recipe, kept byte-equivalent to the
    /// Rust output by the `registry_golden` tests. The engine tests exercise
    /// the real shipped package rather than a separate fixture. The package
    /// tree left this repo for the external veloxsearch-registry repo (#105),
    /// so the path comes from `VELOX_REGISTRY_PATH` (checkout root) like the
    /// `registry_golden` gates; `None` = loud skip when unset (CI's
    /// test:registry-gates job always provides the checkout).
    ///
    /// The pure renderer tests (closed set, foreign token, grok/JSON braces,
    /// escaping) moved to `crates/velox-integrations` with the code.
    fn nginx_fixture() -> Option<std::path::PathBuf> {
        Some(crate::registry_golden::registry_dir()?.join("nginx"))
    }

    fn nginx_vars() -> Vars {
        vars(
            &Deployment::for_test("logs-ab12", "veloxsearch-test", None),
            "nginx-logs",
            "nginx",
            "admin",
            "s3cr3t-pw",
        )
    }

    /// The handle, not a second argument, decides the namespace (#80): the
    /// derived tokens come out exactly as the live plumbing computes them.
    #[test]
    fn vars_resolve_off_the_deployment_handle() {
        let v = nginx_vars();
        assert_eq!(v.get("deployment"), Some("logs-ab12"));
        assert_eq!(v.get("ns"), Some("veloxsearch-test"));
        assert_eq!(v.get("os_host"), Some("logs-ab12.veloxsearch-test.svc"));
        assert_eq!(
            v.get("tenant").map(str::to_string),
            Some(crate::recipes::tenant_name(&Deployment::for_test(
                "logs-ab12",
                "veloxsearch-test",
                None
            )))
        );
        for name in CLOSED_TOKENS {
            assert!(v.get(name).is_some(), "no value resolves for {{{name}}}");
        }
    }

    /// The nginx fixture parses and its manifest names the expected ids.
    #[test]
    fn nginx_fixture_parses() {
        let Some(dir) = nginx_fixture() else { return };
        let pkg = Package::load_from_dir(&dir).expect("load nginx fixture");
        assert_eq!(pkg.manifest.id, "nginx");
        assert_eq!(pkg.manifest.index, "nginx-logs");
        assert!(pkg.pipeline.is_some(), "nginx ships a pipeline");
        assert!(
            pkg.index_template.is_some(),
            "nginx ships an index template"
        );
    }

    /// THE round-trip property: everything a clean install of the nginx package
    /// creates is exactly what its teardown removes — no orphaned pipeline /
    /// template / saved-objects / index-pattern, and no phantom teardown entry.
    #[test]
    fn nginx_clean_install_is_clean_uninstall() {
        let Some(dir) = nginx_fixture() else { return };
        let pkg = Package::load_from_dir(&dir).expect("load nginx fixture");
        let created = created_resources(&pkg).expect("account created");
        let removed = teardown_resources(&pkg.manifest.teardown);

        let orphaned: Vec<_> = created.difference(&removed).collect();
        assert!(orphaned.is_empty(), "install leaves orphans: {orphaned:?}");
        let phantom: Vec<_> = removed.difference(&created).collect();
        assert!(
            phantom.is_empty(),
            "teardown removes uncreated: {phantom:?}"
        );
        assert_eq!(created, removed);
        // The logs input: index-pattern + dashboard + 9 visualizations +
        // pipeline + template; the 1.2 package adds the metrics input's
        // pipeline, template and index-pattern.
        assert!(
            created.len() >= 13,
            "unexpected resource count {}",
            created.len()
        );
    }

    /// The fixture's agent config renders cleanly with the closed set and tails
    /// host logs (nginx is a DaemonSet collector).
    #[test]
    fn nginx_agent_config_renders() {
        let Some(dir) = nginx_fixture() else { return };
        let pkg = Package::load_from_dir(&dir).expect("load nginx fixture");
        let conf = render(&pkg.agent_config, &nginx_vars()).unwrap();
        assert!(conf.contains("Host logs-ab12.veloxsearch-test.svc"));
        assert!(conf.contains("HTTP_User admin"));
        assert!(conf.contains("HTTP_Passwd s3cr3t-pw"));
        assert!(conf.contains("Index nginx-logs"));
        assert!(!conf.contains('{'), "unrendered token in agent config");
        assert!(conf.contains("Name tail"), "nginx tails host logs");
    }

    /// The whole ndjson renders (the index-pattern title uses `{index}`) and
    /// stays valid JSON per line.
    #[test]
    fn nginx_saved_objects_render_and_stay_json() {
        let Some(dir) = nginx_fixture() else { return };
        let pkg = Package::load_from_dir(&dir).expect("load nginx fixture");
        let rendered = render(&pkg.saved_objects, &nginx_vars()).unwrap();
        assert!(
            rendered.contains(r#""title":"nginx-logs*""#),
            "index-pattern title unrendered"
        );
        let objs = saved_objects_bulk(&rendered).expect("ndjson stays valid JSON");
        assert_eq!(
            objs.len(),
            11,
            "11 saved objects (index-pattern + dashboard + 9 viz)"
        );
    }
}
