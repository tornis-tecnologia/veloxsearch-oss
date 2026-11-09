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
    created_resources, parse_manifest, parse_saved_objects, render, saved_objects_bulk,
    teardown_resources, Assets, Manifest, Package, ResourceKey, ResourceKind, Teardown, Vars,
    CLOSED_TOKENS,
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

/// Resolve the variable set for a live deployment: its admin credentials
/// (from the per-cluster Secret) and its own namespace.
async fn resolve_vars(deployment: &Deployment, manifest: &Manifest) -> Vars {
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    vars(deployment, &manifest.index, &manifest.id, &u, &p)
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
    let vars = resolve_vars(deployment, &pkg.manifest).await;

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
