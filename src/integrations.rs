// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! ADR-039 integration apply-engine.
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
//! Scope (#73): the engine + the uninstall fix + interpolation safety. The
//! signature-verification and registry-load layers (#71/#72) sit *in front* of
//! this engine (a `Package` here is already verified + parsed). Extracting the
//! 12 built-in recipes into shipped packages is #74 — the built-in path in
//! `recipes.rs` stays authoritative and unchanged; this module only adds a
//! reusable engine and lets `disable()` reuse the same teardown.

use crate::scope::Deployment;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};

/// The closed, engine-owned interpolation variable set (`docs/integrations/
/// interpolation.md`, ADR-039). Exactly these eight names may appear as
/// `{name}` in an asset; any other `{lower_case_token}` is a hard error.
/// Extending it is a core change (this table + a test), never something a
/// package can introduce.
pub const CLOSED_TOKENS: [&str; 8] = [
    "deployment",
    "index",
    "os_user",
    "os_password",
    "os_host",
    "ns",
    "tenant",
    "recipe_id",
];

// ---------------------------------------------------------------------------
// Manifest (the subset the engine consumes — see manifest.schema.json for the
// full validated contract, enforced by #71 before a package reaches here).
// ---------------------------------------------------------------------------

/// The parsed `manifest.yaml`. Only the fields the apply-engine reads are
/// modeled; serde_yaml ignores the rest (title/summary/signature/discovery),
/// which the schema validator (#71) has already checked.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Manifest {
    /// Package id — also the ingest-pipeline id PUT and the `{recipe_id}` value.
    pub id: String,
    /// The OpenSearch index the package writes to — the `{index}` value and the
    /// index-template / index-pattern id.
    pub index: String,
    /// Saved-object id of the out-of-the-box dashboard (informational here).
    #[allow(dead_code)]
    pub dashboard: String,
    /// Asset filenames by role.
    pub assets: Assets,
    /// The explicit inventory uninstall removes.
    pub teardown: Teardown,
}

/// Asset filenames shipped in the package directory. `pipeline`/`index_template`
/// are absent for packages that need neither (e.g. kubernetes: dynamic mapping).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Assets {
    #[serde(default)]
    pub pipeline: Option<String>,
    #[serde(default)]
    pub index_template: Option<String>,
    pub saved_objects: String,
    pub agent_config: String,
}

/// Everything a package creates in the customer's OpenSearch, so uninstall
/// removes exactly these (ADR-039 "Uninstall becomes real"). Also constructed
/// in-binary by `recipes::recipe_teardown` so the built-in `disable()` reuses
/// the same teardown path.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Teardown {
    /// Ingest-pipeline id to DELETE. Present iff `assets.pipeline` is.
    #[serde(default)]
    pub ingest_pipeline: Option<String>,
    /// Index-template id to DELETE. Present iff `assets.index_template` is.
    #[serde(default)]
    pub index_template: Option<String>,
    /// Index-pattern saved-object id to DELETE (equals `index` today).
    pub index_pattern: String,
    /// Dashboard + visualization saved-object ids to DELETE.
    pub saved_objects: Vec<String>,
    /// Extra index-pattern ids beyond `index_pattern`. A recipe creates exactly
    /// one; the OTel stack (ADR-053) creates three, and reuses this same
    /// teardown machinery rather than growing a parallel one.
    #[serde(default)]
    pub index_patterns: Vec<String>,
    /// ISM policy ids to detach (`_ism/remove`) and DELETE.
    #[serde(default)]
    pub ism_policies: Vec<String>,
    /// SQL-plugin datasource names to DELETE (`_plugins/_query/_datasources`).
    #[serde(default)]
    pub datasources: Vec<String>,
}

/// A verified, parsed package: the manifest plus the raw bytes of each asset it
/// names. Construction (registry pull / disk load / signature check) happens
/// upstream; the engine only consumes it.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    /// Raw `pipeline.json` (None when the package ships none).
    pub pipeline: Option<String>,
    /// Raw `index-template.json` (None when the package ships none).
    pub index_template: Option<String>,
    /// Raw `saved-objects.ndjson` (index-pattern + dashboard + visualizations).
    pub saved_objects: String,
    /// Raw `agent.conf.tmpl` (Fluent Bit config template).
    pub agent_config: String,
}

/// Parse a `manifest.yaml` into the fields the engine reads.
pub fn parse_manifest(yaml: &str) -> Result<Manifest> {
    serde_yaml::from_str(yaml).context("parsing integration manifest.yaml")
}

impl Package {
    /// Load a package from a directory: `manifest.yaml` plus each asset file it
    /// names. Used by the engine's tests against the nginx fixture, and by the
    /// registry/offline loaders (#72) once those land.
    pub fn load_from_dir(dir: &std::path::Path) -> Result<Package> {
        let manifest_yaml = std::fs::read_to_string(dir.join("manifest.yaml"))
            .with_context(|| format!("reading {}/manifest.yaml", dir.display()))?;
        let manifest = parse_manifest(&manifest_yaml)?;
        let read = |name: &str| -> Result<String> {
            std::fs::read_to_string(dir.join(name))
                .with_context(|| format!("reading asset '{name}'"))
        };
        let pipeline = match &manifest.assets.pipeline {
            Some(f) => Some(read(f)?),
            None => None,
        };
        let index_template = match &manifest.assets.index_template {
            Some(f) => Some(read(f)?),
            None => None,
        };
        let saved_objects = read(&manifest.assets.saved_objects)?;
        let agent_config = read(&manifest.assets.agent_config)?;
        Ok(Package {
            manifest,
            pipeline,
            index_template,
            saved_objects,
            agent_config,
        })
    }
}

// ---------------------------------------------------------------------------
// Interpolation — the closed variable set (fail closed on foreign tokens).
// ---------------------------------------------------------------------------

/// Resolved values for the closed variable set, for one deployment × package.
/// `new` is pure (unit-testable without a cluster); `resolve` builds it from the
/// live deployment (its admin creds + namespace).
pub struct Vars {
    deployment: String,
    index: String,
    os_user: String,
    os_password: String,
    os_host: String,
    ns: String,
    tenant: String,
    recipe_id: String,
}

impl Vars {
    /// Build the variable set from primitives. `os_host` and `tenant` are
    /// derived exactly as the live plumbing derives them (`{deployment}.{ns}.svc`
    /// and `recipes::tenant_name`), so a rendered asset matches what the
    /// built-in recipes produce.
    ///
    /// The namespace comes off the deployment rather than being passed
    /// alongside it (#80): with per-tenant namespaces two sources could
    /// disagree, and the one that wins would decide which cluster a collector
    /// ships to.
    pub fn new(
        deployment: &Deployment,
        index: &str,
        recipe_id: &str,
        os_user: &str,
        os_password: &str,
    ) -> Vars {
        let ns = deployment.namespace();
        Vars {
            os_host: format!("{deployment}.{ns}.svc"),
            tenant: crate::recipes::tenant_name(deployment),
            deployment: deployment.to_string(),
            ns: ns.to_string(),
            index: index.to_string(),
            recipe_id: recipe_id.to_string(),
            os_user: os_user.to_string(),
            os_password: os_password.to_string(),
        }
    }

    /// Resolve the variable set for a live deployment: its admin credentials
    /// (from the per-cluster Secret) and its own namespace.
    async fn resolve(deployment: &Deployment, manifest: &Manifest) -> Vars {
        let (u, p) = crate::k8s::admin_creds(deployment).await;
        Vars::new(deployment, &manifest.index, &manifest.id, &u, &p)
    }

    fn get(&self, name: &str) -> Option<&str> {
        Some(match name {
            "deployment" => &self.deployment,
            "index" => &self.index,
            "os_user" => &self.os_user,
            "os_password" => &self.os_password,
            "os_host" => &self.os_host,
            "ns" => &self.ns,
            "tenant" => &self.tenant,
            "recipe_id" => &self.recipe_id,
            _ => return None,
        })
    }
}

/// Substitute the closed variable set into an asset, **failing closed** on any
/// foreign token. A *token* is `{` + a lowercase-initial identifier
/// (`[a-z][A-Za-z0-9_]*`) + `}`:
///   - a known name (CLOSED_TOKENS) → its value;
///   - any other lowercase-initial name → `Err` (a package cannot invent a hole).
///
/// Everything else passes through untouched: grok syntax (`%{COMBINEDAPACHELOG}`,
/// `%{POSINT:pid}` — uppercase / colon, not a token) and JSON structural braces
/// (`{"type":…}` — `{` not followed by a bare identifier). `{{` escapes a literal
/// `{` (so `{{index}` yields the text `{index}` without substitution); a `}` is
/// always literal. Escaping is deliberately asymmetric — a `}}` escape would be
/// impossible to distinguish from two adjacent JSON closing braces (`{}}`), and
/// a lone `}` is never ambiguous because only a full `{name}` is a token. This
/// lets one scanner validate JSON, pipeline and Fluent-Bit assets without
/// choking on their non-template braces.
pub fn render(template: &str, vars: &Vars) -> Result<String> {
    let chars: Vec<char> = template.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '{' if i + 1 < n && chars[i + 1] == '{' => {
                out.push('{');
                i += 2;
            }
            '{' => {
                // Read a candidate token name: identifier chars up to a `}`.
                let start = i + 1;
                let mut j = start;
                while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                if j < n && j > start && chars[j] == '}' {
                    let name: String = chars[start..j].iter().collect();
                    if let Some(v) = vars.get(&name) {
                        out.push_str(v);
                        i = j + 1;
                        continue;
                    }
                    // A lowercase-initial `{name}` that isn't known is a template
                    // hole the package tried to invent — reject (fail closed).
                    // Uppercase-initial (grok `%{DATA}`) is not a token attempt.
                    if name.starts_with(|ch: char| ch.is_ascii_lowercase()) {
                        anyhow::bail!(
                            "foreign interpolation token {{{name}}} is not in the \
                             closed variable set (allowed: {})",
                            CLOSED_TOKENS.join(", ")
                        );
                    }
                }
                out.push('{');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Resource accounting — proves clean-install ⇒ clean-uninstall as set equality.
// ---------------------------------------------------------------------------

/// A class of OpenSearch object an integration creates. Paired with an id, this
/// keys the "what a package installs" set against "what its teardown removes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    IngestPipeline,
    IndexTemplate,
    IndexPattern,
    SavedObject,
    /// ISM retention policy (ADR-053; recipes never create one — the purpose
    /// profile owns `velox-retention`, which is not a per-package object).
    IsmPolicy,
    /// SQL-plugin datasource (ADR-053).
    Datasource,
}

/// `(kind, id)` — one installed / torn-down object.
pub type ResourceKey = (ResourceKind, String);

/// Everything applying `pkg` creates in the customer's OpenSearch, derived from
/// the package itself (assets present + the ids inside `saved-objects.ndjson`).
pub fn created_resources(pkg: &Package) -> Result<BTreeSet<ResourceKey>> {
    let mut set = BTreeSet::new();
    if pkg.pipeline.is_some() {
        set.insert((ResourceKind::IngestPipeline, pkg.manifest.id.clone()));
    }
    if pkg.index_template.is_some() {
        set.insert((ResourceKind::IndexTemplate, pkg.manifest.index.clone()));
    }
    for o in parse_saved_objects(&pkg.saved_objects)? {
        if o.kind == "index-pattern" {
            set.insert((ResourceKind::IndexPattern, o.id));
        } else {
            set.insert((ResourceKind::SavedObject, o.id));
        }
    }
    Ok(set)
}

/// Everything the manifest's `teardown` will remove.
pub fn teardown_resources(t: &Teardown) -> BTreeSet<ResourceKey> {
    let mut set = BTreeSet::new();
    if let Some(id) = &t.ingest_pipeline {
        set.insert((ResourceKind::IngestPipeline, id.clone()));
    }
    if let Some(id) = &t.index_template {
        set.insert((ResourceKind::IndexTemplate, id.clone()));
    }
    set.insert((ResourceKind::IndexPattern, t.index_pattern.clone()));
    for id in &t.index_patterns {
        set.insert((ResourceKind::IndexPattern, id.clone()));
    }
    for id in &t.saved_objects {
        set.insert((ResourceKind::SavedObject, id.clone()));
    }
    for id in &t.ism_policies {
        set.insert((ResourceKind::IsmPolicy, id.clone()));
    }
    for id in &t.datasources {
        set.insert((ResourceKind::Datasource, id.clone()));
    }
    set
}

/// One saved object's type + id (from an ndjson line).
struct SavedObj {
    kind: String,
    id: String,
}

/// Parse `saved-objects.ndjson` into `(type, id)` pairs (one JSON object per
/// non-blank line). Used both to account for created objects and to recover the
/// object *type* the Dashboards DELETE API needs at teardown.
fn parse_saved_objects(ndjson: &str) -> Result<Vec<SavedObj>> {
    let mut out = Vec::new();
    for (n, line) in ndjson.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("saved-objects.ndjson: invalid JSON on line {}", n + 1))?;
        let kind = v
            .get("type")
            .and_then(|x| x.as_str())
            .with_context(|| format!("saved-objects.ndjson line {}: missing \"type\"", n + 1))?
            .to_string();
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .with_context(|| format!("saved-objects.ndjson line {}: missing \"id\"", n + 1))?
            .to_string();
        out.push(SavedObj { kind, id });
    }
    Ok(out)
}

/// Parse `saved-objects.ndjson` into the JSON array `_bulk_create` expects.
fn saved_objects_bulk(ndjson: &str) -> Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for (n, line) in ndjson.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .with_context(|| format!("saved-objects.ndjson: invalid JSON on line {}", n + 1))?,
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Apply / uninstall executors — reuse the recipe plumbing verbatim.
// ---------------------------------------------------------------------------

/// Install a package end-to-end (the generalized `recipes::apply` sequence):
///  1. render + interpolation-check every asset (foreign token ⇒ abort before
///     anything touches the cluster);
///  2. PUT the ingest pipeline (if present);
///  3. PUT the index template (if present);
///  4. `_bulk_create` the saved objects into the deployment's Dashboards tenant;
///  5. deploy the agent from the rendered config.
pub async fn apply_package(deployment: &Deployment, pkg: &Package) -> Result<()> {
    let vars = Vars::resolve(deployment, &pkg.manifest).await;

    // Interpolation safety first: render (⇒ validate) EVERY asset up front so a
    // foreign `{token}` fails the whole install before a single write.
    let pipeline = match &pkg.pipeline {
        Some(s) => Some(render(s, &vars)?),
        None => None,
    };
    let index_template = match &pkg.index_template {
        Some(s) => Some(render(s, &vars)?),
        None => None,
    };
    let saved_objects = render(&pkg.saved_objects, &vars)?;
    let agent_config = render(&pkg.agent_config, &vars)?;

    let base = crate::recipes::os_base(deployment);
    let c = crate::recipes::http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;

    // 2. Ingest pipeline (hard-fail like configure_*): PUT under the package id.
    if let Some(body) = &pipeline {
        let json: serde_json::Value =
            serde_json::from_str(body).context("pipeline.json is not valid JSON")?;
        c.put(format!("{base}/_ingest/pipeline/{}", pkg.manifest.id))
            .basic_auth(&u, Some(&p))
            .json(&json)
            .send()
            .await
            .context("creating ingest pipeline")?
            .error_for_status()
            .context("ingest pipeline rejected")?;
    }

    // 3. Index template (hard-fail): PUT under the index name.
    if let Some(body) = &index_template {
        let json: serde_json::Value =
            serde_json::from_str(body).context("index-template.json is not valid JSON")?;
        c.put(format!("{base}/_index_template/{}", pkg.manifest.index))
            .basic_auth(&u, Some(&p))
            .json(&json)
            .send()
            .await
            .context("creating index template")?
            .error_for_status()
            .context("index template rejected")?;
    }

    // 4. Saved objects into the deployment's own Dashboards tenant (best-effort,
    //    exactly like ensure_tenant/ensure_dashboard: a cluster without
    //    multitenancy still gets the data, it just lands in Global). When the
    //    ADR-049 stack owns the Dashboards, scoping is by workspace instead and
    //    there is no tenant to create or name (see `recipes::tenant_scope`).
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

    // 5. Agent from the rendered config. A config that tails host files uses the
    //    Fluent Bit `tail` input (DaemonSet); an API-server watcher
    //    (`kubernetes_events`) does not (single-replica Deployment) — the same
    //    split `deploy_agent` makes for `k8s-events`, here read off the config.
    let tails_logs = agent_config.contains("Name tail");
    crate::agents::deploy_rendered_agent(deployment, &pkg.manifest.id, &agent_config, tails_logs)
        .await?;
    Ok(())
}

/// Uninstall a package: remove its agent, then delete exactly the manifest's
/// `teardown` set from the deployment's OpenSearch — the fix for the historical
/// `disable()` leak (ADR-039). Saved-object *types* (needed by the Dashboards
/// DELETE API) are recovered from the package's own ndjson.
pub async fn uninstall_package(deployment: &Deployment, pkg: &Package) -> Result<()> {
    let _ = crate::agents::remove_agent(deployment, &pkg.manifest.id).await;
    let kinds: BTreeMap<String, String> = parse_saved_objects(&pkg.saved_objects)?
        .into_iter()
        .map(|o| (o.id, o.kind))
        .collect();
    teardown_os(deployment, &pkg.manifest.teardown, &kinds).await;
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

    if let Some(id) = &t.ingest_pipeline {
        let _ = c
            .delete(format!("{base}/_ingest/pipeline/{id}"))
            .basic_auth(&u, Some(&p))
            .send()
            .await;
    }
    if let Some(id) = &t.index_template {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Path to the CANONICAL nginx registry package (#74) — the frozen
    /// extraction of the built-in nginx recipe, kept byte-equivalent to the
    /// Rust output by the `registry_golden` tests. The engine tests exercise
    /// the real shipped package rather than a separate fixture. The package
    /// tree left this repo for the external veloxsearch-registry repo (#105),
    /// so the path comes from `VELOX_REGISTRY_PATH` (checkout root) like the
    /// `registry_golden` gates; `None` = loud skip when unset (CI's
    /// test:registry-gates job always provides the checkout).
    fn nginx_fixture() -> Option<std::path::PathBuf> {
        Some(crate::registry_golden::registry_dir()?.join("nginx"))
    }

    fn nginx_vars() -> Vars {
        Vars::new(
            &Deployment::for_test("logs-ab12", "veloxsearch-test", None),
            "nginx-logs",
            "nginx",
            "admin",
            "s3cr3t-pw",
        )
    }

    /// The substitution table has EXACTLY the closed set — no more, no fewer —
    /// and every name resolves (interpolation.md test obligation).
    #[test]
    fn closed_token_table_matches_contract() {
        let expected: BTreeSet<&str> = [
            "deployment",
            "index",
            "os_user",
            "os_password",
            "os_host",
            "ns",
            "tenant",
            "recipe_id",
        ]
        .into_iter()
        .collect();
        let actual: BTreeSet<&str> = CLOSED_TOKENS.into_iter().collect();
        assert_eq!(actual, expected, "CLOSED_TOKENS drifted from the contract");

        let v = nginx_vars();
        for name in CLOSED_TOKENS {
            assert!(v.get(name).is_some(), "no value resolves for {{{name}}}");
        }
        // Derived tokens computed exactly as the live plumbing does.
        assert_eq!(v.get("os_host"), Some("logs-ab12.veloxsearch-test.svc"));
        assert_eq!(v.get("tenant"), Some("velox-logs-ab12"));
    }

    /// Every closed token substitutes to its value.
    #[test]
    fn render_substitutes_closed_tokens() {
        let v = nginx_vars();
        let t = "d={deployment} ns={ns} host={os_host} u={os_user} p={os_password} \
                 i={index} t={tenant} r={recipe_id}";
        let got = render(t, &v).unwrap();
        assert_eq!(
            got,
            "d=logs-ab12 ns=veloxsearch-test host=logs-ab12.veloxsearch-test.svc \
             u=admin p=s3cr3t-pw i=nginx-logs t=velox-logs-ab12 r=nginx"
        );
        // Nothing left unrendered.
        assert!(!got.contains('{'), "unrendered token remained: {got}");
    }

    /// A package referencing a token outside the closed set is REJECTED, not
    /// silently passed through (the interpolation.md golden token).
    #[test]
    fn render_rejects_foreign_token() {
        let v = nginx_vars();
        let err = render("HTTP_Passwd {cluster_admin_ssh}", &v).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cluster_admin_ssh"), "unhelpful error: {msg}");
        assert!(
            msg.contains("closed variable set"),
            "unhelpful error: {msg}"
        );
    }

    /// Grok syntax and JSON structural braces are NOT tokens — they pass through
    /// untouched (so pipeline/template/ndjson assets survive the scan).
    #[test]
    fn render_preserves_grok_and_json_braces() {
        let v = nginx_vars();
        let grok = r#"{ "grok": { "patterns": ["%{COMBINEDAPACHELOG}", "%{POSINT:pid}"] } }"#;
        assert_eq!(render(grok, &v).unwrap(), grok);
        let json = r#"{"type":"visualization","id":"velox-nginx-reqs","attributes":{}}"#;
        assert_eq!(render(json, &v).unwrap(), json);
    }

    /// `{{` escapes a literal `{`, so a brace-word can be emitted verbatim
    /// WITHOUT substitution and WITHOUT tripping the foreign-token guard.
    #[test]
    fn render_escapes_open_brace() {
        let v = nginx_vars();
        // `{{index}` → literal `{index}` (not substituted to the index name).
        assert_eq!(render("a {{index} b", &v).unwrap(), "a {index} b");
        // A brace-word that would otherwise be a foreign token is safe when the
        // opening brace is escaped.
        assert_eq!(
            render("literal {{cluster_admin_ssh} here", &v).unwrap(),
            "literal {cluster_admin_ssh} here"
        );
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
        // The index-pattern + dashboard + 9 visualizations + pipeline + template.
        assert_eq!(created.len(), 13, "unexpected resource count");
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
