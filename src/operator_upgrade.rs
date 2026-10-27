// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Operator upgrade from Settings (#176, ADR-066).
//!
//! ADR-057 keeps an upgrade of VeloxSearch from ever touching the OpenSearch
//! operator: a release that vendors a newer operator leaves the running one in
//! place and reports R9. This module is the explicit step that closes the gap,
//! in two halves:
//!
//! 1. **The impact report** — read-only, on the rights the app already holds.
//!    What the vendored bundle changes in the operator's CRDs (a removed field
//!    that a live `OpenSearchCluster` still sets is breaking), what each
//!    deployment goes through (every node pool may restart once; a pool of one
//!    is down for its restart), and what blocks the upgrade outright.
//! 2. **The upgrade** — behind a one-time grant. The app holds no standing
//!    cluster-admin (ADR-027), and updating CRDs and the operator's ClusterRoles
//!    needs it, so a cluster admin creates the [`GRANT`] binding by hand. The
//!    upgrade applies the vendored bundle exactly as bootstrap does, waits for
//!    the controller, and deletes the grant — on success and on failure. An
//!    unused grant is deleted by the revoke watch after [`GRANT_TTL_SECS`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use anyhow::{Context, Result};
use k8s_openapi::api::rbac::v1::ClusterRoleBinding;
use kube::api::{Api, DynamicObject, ListParams};
use kube::Client;
use serde::Serialize;
use serde_json::Value;

/// The one-time grant a cluster admin creates to arm the button.
pub const GRANT: &str = "veloxsearch-operator-upgrade";

/// An unused grant lives this long before the revoke watch deletes it.
pub const GRANT_TTL_SECS: i64 = 30 * 60;

/// How long the upgraded controller gets to become Ready — the same budget
/// bootstrap gives a fresh install.
const OPERATOR_READY_SECS: u64 = 300;

// ───────────────────────────── report DTOs ─────────────────────────────────

/// One reason the upgrade cannot run, or one thing it will cost. Facts with a
/// stable `code`; the UI owns the sentence (ADR-019), `detail` is the English
/// fallback and the log line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub code: &'static str,
    /// `namespace/name` of the deployment, when the finding is about one.
    pub deployment: String,
    /// Node pool, CRD name or field path — whatever the code is about.
    pub subject: String,
    pub detail: String,
}

impl Finding {
    fn new(code: &'static str, deployment: &str, subject: &str, detail: String) -> Self {
        Finding {
            code,
            deployment: deployment.to_string(),
            subject: subject.to_string(),
            detail,
        }
    }
}

/// What the vendored bundle changes in one operator CRD.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CrdChange {
    pub name: String,
    /// Not installed yet — the bundle adds the whole CRD.
    pub new: bool,
    pub removed_versions: Vec<String>,
    pub added_versions: Vec<String>,
    pub removed_fields: Vec<String>,
    pub added_fields: usize,
}

impl CrdChange {
    fn is_noop(&self) -> bool {
        !self.new
            && self.removed_versions.is_empty()
            && self.added_versions.is_empty()
            && self.removed_fields.is_empty()
            && self.added_fields == 0
    }
}

/// One node pool of one deployment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PoolImpact {
    pub name: String,
    pub replicas: i64,
    /// A pool of one has no second pod to serve while its only pod restarts.
    pub downtime: bool,
}

/// One `OpenSearchCluster` the operator reconciles — every one of them, not
/// only the caller's: the operator is cluster-wide, so is its upgrade.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeploymentImpact {
    pub namespace: String,
    pub name: String,
    pub health: String,
    pub pools: Vec<PoolImpact>,
    pub snapshot_policy: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GrantStatus {
    pub present: bool,
    /// Seconds until the revoke watch deletes an unused grant.
    pub expires_in_secs: i64,
    /// The command a cluster admin runs to arm the button.
    pub command: String,
}

/// Where the upgrade job is: `idle`, `running`, `done` or `failed`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct JobStatus {
    pub state: String,
    pub step: String,
    pub from: String,
    pub to: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub running: String,
    pub vendored: String,
    /// `up_to_date`, `available`, or `unmanaged` (not the operator VeloxSearch
    /// installed, or it could not be read — nothing here applies).
    pub state: String,
    pub crds: Vec<CrdChange>,
    pub deployments: Vec<DeploymentImpact>,
    pub blockers: Vec<Finding>,
    pub warnings: Vec<Finding>,
    pub grant: GrantStatus,
    pub job: JobStatus,
    /// Every input is in and nothing blocks: the button may be offered.
    pub can_upgrade: bool,
}

// ─────────────────────────────── CRD diff ──────────────────────────────────

/// Every field path an `openAPIV3Schema` declares: `.spec.nodePools[].replicas`.
/// Descends `properties` and array `items`; stops at a node that preserves
/// unknown fields or takes `additionalProperties`, because below it any path is
/// legal and nothing can be "removed".
pub fn schema_paths(schema: &Value) -> BTreeSet<String> {
    fn walk(node: &Value, path: &str, out: &mut BTreeSet<String>) {
        if node
            .get("x-kubernetes-preserve-unknown-fields")
            .and_then(Value::as_bool)
            == Some(true)
            || node.get("additionalProperties").is_some()
        {
            return;
        }
        if let Some(props) = node.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                let p = format!("{path}.{k}");
                out.insert(p.clone());
                walk(v, &p, out);
            }
        }
        if let Some(items) = node.get("items") {
            walk(items, &format!("{path}[]"), out);
        }
    }
    let mut out = BTreeSet::new();
    walk(schema, "", &mut out);
    out
}

/// Every field path a live object sets, in [`schema_paths`]' notation.
pub fn value_paths(v: &Value) -> BTreeSet<String> {
    fn walk(node: &Value, path: &str, out: &mut BTreeSet<String>) {
        match node {
            Value::Object(m) => {
                for (k, v) in m {
                    let p = format!("{path}.{k}");
                    out.insert(p.clone());
                    walk(v, &p, out);
                }
            }
            Value::Array(a) => {
                for v in a {
                    walk(v, &format!("{path}[]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(v, "", &mut out);
    out
}

/// `version name → openAPIV3Schema` of a CRD document.
fn crd_versions(crd: &Value) -> BTreeMap<String, Value> {
    crd.pointer("/spec/versions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let name = v.get("name")?.as_str()?.to_string();
            let schema = v
                .pointer("/schema/openAPIV3Schema")
                .cloned()
                .unwrap_or(Value::Null);
            Some((name, schema))
        })
        .collect()
}

/// What replacing `installed` by `vendored` changes. Fields are compared per
/// version both CRDs serve; a version only one side serves is reported as a
/// version change, not as a pile of field changes.
pub fn diff_crd(name: &str, installed: Option<&Value>, vendored: &Value) -> CrdChange {
    let Some(installed) = installed else {
        return CrdChange {
            name: name.to_string(),
            new: true,
            ..Default::default()
        };
    };
    let old = crd_versions(installed);
    let new = crd_versions(vendored);
    let mut change = CrdChange {
        name: name.to_string(),
        removed_versions: old
            .keys()
            .filter(|v| !new.contains_key(*v))
            .cloned()
            .collect(),
        added_versions: new
            .keys()
            .filter(|v| !old.contains_key(*v))
            .cloned()
            .collect(),
        ..Default::default()
    };
    let mut removed = BTreeSet::new();
    for (v, old_schema) in &old {
        let Some(new_schema) = new.get(v) else {
            continue;
        };
        let (a, b) = (schema_paths(old_schema), schema_paths(new_schema));
        removed.extend(a.difference(&b).cloned());
        change.added_fields += b.difference(&a).count();
    }
    // A removed object takes its children with it: report the top of each
    // removed subtree only.
    change.removed_fields = removed
        .iter()
        .filter(|p| !removed.iter().any(|q| q != *p && is_under(p, q)))
        .cloned()
        .collect();
    change
}

/// `p` is `q` itself or a field below it.
fn is_under(p: &str, q: &str) -> bool {
    p == q
        || p.strip_prefix(q)
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
}

/// The removed fields this live object still sets.
pub fn breaking_uses(removed_fields: &[String], obj: &Value) -> Vec<String> {
    let used = value_paths(obj);
    removed_fields
        .iter()
        .filter(|r| used.iter().any(|u| is_under(u, r)))
        .cloned()
        .collect()
}

// ────────────────────────────── deployments ────────────────────────────────

/// Node pools of a CR, as the operator reads them.
pub fn pools_of(cr: &Value) -> Vec<PoolImpact> {
    cr.pointer("/spec/nodePools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|p| {
            let replicas = p.get("replicas").and_then(Value::as_i64).unwrap_or(0);
            PoolImpact {
                name: p
                    .get("component")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                replicas,
                downtime: replicas == 1,
            }
        })
        .collect()
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> &'a str {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or_default()
}

/// A CR mid version-upgrade (ADR-048) — the operator restarting under it would
/// stack two rolls on one cluster.
fn version_upgrade_in_flight(cr: &Value) -> bool {
    crate::upgrade::upgrade_state(
        str_at(cr, "/spec/general/version"),
        str_at(cr, "/status/version"),
        cr.pointer("/status/componentsStatus")
            .unwrap_or(&Value::Null),
        None,
    )
    .in_flight()
}

// ──────────────────────────────── verdict ──────────────────────────────────

/// Everything the verdict is computed from, already read off the cluster.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    /// `Some` only when the running operator is the one VeloxSearch installed.
    pub running: Option<String>,
    pub vendored: String,
    pub operator_ready: bool,
    pub drift: bool,
    /// Why the operator is not ours / could not be read, when it is not.
    pub unmanaged_reason: Option<String>,
    /// `(crd name, installed doc or None, vendored doc)`.
    pub crds: Vec<(String, Option<Value>, Value)>,
    /// Every OpenSearchCluster, raw.
    pub clusters: Vec<Value>,
    /// `namespace/name` of clusters a snapshot policy covers.
    pub snapshot_covered: BTreeSet<String>,
    pub bootstrap_running: bool,
}

fn key_of(cr: &Value) -> (String, String) {
    (
        str_at(cr, "/metadata/namespace").to_string(),
        str_at(cr, "/metadata/name").to_string(),
    )
}

/// The report, minus grant and job (those are live state, not a verdict).
pub fn assess(i: &Inputs) -> Report {
    let mut r = Report {
        running: i.running.clone().unwrap_or_default(),
        vendored: i.vendored.clone(),
        ..Default::default()
    };
    if let Some(why) = &i.unmanaged_reason {
        r.state = "unmanaged".into();
        r.blockers
            .push(Finding::new("unmanaged", "", "", why.clone()));
        return r;
    }
    r.state = if i.drift { "available" } else { "up_to_date" }.into();

    // CRDs, and the live objects that set a field the vendored CRD drops.
    for (name, installed, vendored) in &i.crds {
        let c = diff_crd(name, installed.as_ref(), vendored);
        if c.is_noop() {
            continue;
        }
        if !c.removed_fields.is_empty() && name.starts_with("opensearchclusters.") {
            for cr in &i.clusters {
                let (ns, n) = key_of(cr);
                for f in breaking_uses(&c.removed_fields, cr) {
                    r.blockers.push(Finding::new(
                        "crd_field_removed",
                        &format!("{ns}/{n}"),
                        &f,
                        format!("{ns}/{n} sets {f}, which the new {name} no longer has"),
                    ));
                }
            }
        }
        r.crds.push(c);
    }

    for cr in &i.clusters {
        let (ns, n) = key_of(cr);
        let dep = format!("{ns}/{n}");
        let health = match str_at(cr, "/status/health") {
            "" => "unknown",
            h => h,
        }
        .to_string();
        let pools = pools_of(cr);
        if health != "green" {
            r.blockers.push(Finding::new(
                "deployment_not_green",
                &dep,
                &health,
                format!("{dep} is {health} — restarting its nodes needs it green"),
            ));
        }
        if version_upgrade_in_flight(cr) {
            r.blockers.push(Finding::new(
                "version_upgrade_in_flight",
                &dep,
                "",
                format!("{dep} is mid version upgrade"),
            ));
        }
        for p in pools.iter().filter(|p| p.downtime) {
            r.warnings.push(Finding::new(
                "single_replica_pool",
                &dep,
                &p.name,
                format!(
                    "{dep}: pool {} has one node and is down while it restarts",
                    p.name
                ),
            ));
        }
        let snapshot_policy = i.snapshot_covered.contains(&dep);
        if !snapshot_policy {
            r.warnings.push(Finding::new(
                "no_snapshot_policy",
                &dep,
                "",
                format!("{dep} has no scheduled snapshot to fall back on"),
            ));
        }
        r.deployments.push(DeploymentImpact {
            namespace: ns,
            name: n,
            health,
            pools,
            snapshot_policy,
        });
    }

    if !i.operator_ready {
        r.blockers.push(Finding::new(
            "operator_not_ready",
            "",
            "",
            "the running operator is not Ready — wait for it before replacing it".into(),
        ));
    }
    if i.bootstrap_running {
        r.blockers.push(Finding::new(
            "bootstrap_running",
            "",
            "",
            "a bootstrap install is running".into(),
        ));
    }
    r
}

/// Whether the button may be offered: something to do, nothing blocking, the
/// grant in place, and no job already running.
fn can_upgrade(r: &Report) -> bool {
    r.state == "available" && r.blockers.is_empty() && r.grant.present && r.job.state != "running"
}

/// Seconds until an unused grant expires; `<= 0` means the watch deletes it.
pub fn grant_expires_in(age_secs: i64) -> i64 {
    GRANT_TTL_SECS - age_secs
}

/// The revoke watch's decision: never while the upgrade runs (it needs the
/// grant to finish), otherwise once the grant has outlived its TTL.
pub fn grant_due_for_revoke(age_secs: i64, upgrading: bool) -> bool {
    !upgrading && grant_expires_in(age_secs) <= 0
}

pub fn grant_command(namespace: &str) -> String {
    format!(
        "kubectl create clusterrolebinding {GRANT} --clusterrole=cluster-admin --serviceaccount={namespace}:{}",
        crate::bootstrap::APP_SERVICE_ACCOUNT
    )
}

// ──────────────────────────────── job state ────────────────────────────────

static JOB: Mutex<JobStatus> = Mutex::new(JobStatus {
    state: String::new(),
    step: String::new(),
    from: String::new(),
    to: String::new(),
    error: String::new(),
});

fn job() -> JobStatus {
    let j = JOB.lock().unwrap().clone();
    if j.state.is_empty() {
        JobStatus {
            state: "idle".into(),
            ..j
        }
    } else {
        j
    }
}

fn job_set(f: impl FnOnce(&mut JobStatus)) {
    f(&mut JOB.lock().unwrap());
}

/// The upgrade is running — the revoke watch must leave the grant alone.
pub fn upgrading() -> bool {
    JOB.lock().unwrap().state == "running"
}

// ─────────────────────────────── cluster I/O ───────────────────────────────

/// `(present, age_secs)` of the grant, if it names this very app.
async fn read_grant(client: &Client) -> Result<Option<i64>> {
    let api: Api<ClusterRoleBinding> = Api::all(client.clone());
    let Some(b) = api
        .get_opt(GRANT)
        .await
        .context("reading the upgrade grant")?
    else {
        return Ok(None);
    };
    if !crate::bootstrap::binding_is_ours(&b, crate::k8s::ns()) {
        return Ok(None);
    }
    let age = b
        .metadata
        .creation_timestamp
        .map(|t| (k8s_openapi::chrono::Utc::now() - t.0).num_seconds())
        .unwrap_or(0);
    Ok(Some(age))
}

/// Delete the grant. Holding it is what makes this possible: the runtime role
/// has no `delete` on bindings, cluster-admin has (the ADR-027 self-revoke).
async fn revoke_grant(client: &Client) {
    let api: Api<ClusterRoleBinding> = Api::all(client.clone());
    match api.delete(GRANT, &Default::default()).await {
        Ok(_) => tracing::info!("operator upgrade: revoked the {GRANT} grant"),
        Err(kube::Error::Api(e)) if e.code == 404 => {}
        Err(e) => tracing::warn!("operator upgrade: could not revoke {GRANT}: {e}"),
    }
}

/// Called by the revoke watch every tick: delete a grant nobody used.
pub async fn expire_grant(client: &Client) {
    if let Ok(Some(age)) = read_grant(client).await {
        if grant_due_for_revoke(age, upgrading()) {
            tracing::info!("operator upgrade: {GRANT} unused for {age}s — expiring it");
            revoke_grant(client).await;
        }
    }
}

/// `OpensearchSnapshotPolicy` (ADR-049) — what `k8s` creates per deployment.
fn policy_resource() -> kube::api::ApiResource {
    kube::api::ApiResource::from_gvk(&kube::api::GroupVersionKind::gvk(
        "opensearch.org",
        "v1",
        "OpensearchSnapshotPolicy",
    ))
}

/// The CRD documents of the vendored operator bundle, by name.
fn vendored_crds() -> Result<Vec<(String, Value)>> {
    Ok(
        crate::bootstrap::bundle_docs(&crate::bootstrap::operator_bundle())?
            .into_iter()
            .filter(|d| str_at(d, "/kind") == "CustomResourceDefinition")
            .map(|d| (str_at(&d, "/metadata/name").to_string(), d))
            .collect(),
    )
}

async fn read_inputs(client: &Client) -> Result<Inputs> {
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;

    let op = crate::bootstrap::operator_state(client).await?;
    let mut i = Inputs {
        vendored: op.vendored.clone().unwrap_or_default(),
        bootstrap_running: crate::bootstrap::install_running(),
        ..Default::default()
    };
    match op.ours {
        Some((image, ready)) => {
            i.running = Some(image.unwrap_or_default());
            i.operator_ready = ready;
            i.drift = op.drift;
        }
        None => {
            i.unmanaged_reason = Some(op.note);
            return Ok(i);
        }
    }

    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    for (name, vendored) in vendored_crds()? {
        let installed = crd_api
            .get_opt(&name)
            .await
            .with_context(|| format!("reading CRD {name}"))?
            .map(|c| serde_json::to_value(c).unwrap_or(Value::Null));
        i.crds.push((name, installed, vendored));
    }

    let clusters: Api<DynamicObject> =
        Api::all_with(client.clone(), &crate::k8s::cluster_resource());
    i.clusters = clusters
        .list(&ListParams::default())
        .await
        .context("listing OpenSearchClusters")?
        .items
        .into_iter()
        .map(|o| serde_json::to_value(o).unwrap_or(Value::Null))
        .collect();

    // A missing policy CRD (never created one) reads as "no cluster covered".
    let policies: Api<DynamicObject> = Api::all_with(client.clone(), &policy_resource());
    if let Ok(list) = policies.list(&ListParams::default()).await {
        for p in list.items {
            let v = serde_json::to_value(&p).unwrap_or(Value::Null);
            i.snapshot_covered.insert(format!(
                "{}/{}",
                str_at(&v, "/metadata/namespace"),
                str_at(&v, "/spec/opensearchCluster/name")
            ));
        }
    }
    Ok(i)
}

/// The full report: verdict, grant and job.
pub async fn report() -> Result<Report> {
    let client = crate::k8s::client().await?;
    let mut r = assess(&read_inputs(&client).await?);
    let ns = crate::k8s::ns();
    r.grant = match read_grant(&client).await? {
        Some(age) => GrantStatus {
            present: true,
            expires_in_secs: grant_expires_in(age).max(0),
            command: grant_command(ns),
        },
        None => GrantStatus {
            command: grant_command(ns),
            ..Default::default()
        },
    };
    r.job = job();
    r.can_upgrade = can_upgrade(&r);
    Ok(r)
}

/// Start the upgrade, or say why not. The verdict is recomputed here, never
/// trusted from the page that rendered the button.
pub async fn start() -> std::result::Result<(), String> {
    let r = report().await.map_err(|e| format!("{e:#}"))?;
    if !can_upgrade(&r) {
        let why = if r.job.state == "running" {
            "an operator upgrade is already running".to_string()
        } else if r.state != "available" {
            format!("nothing to upgrade ({})", r.state)
        } else if let Some(b) = r.blockers.first() {
            b.detail.clone()
        } else {
            format!("the {GRANT} grant is not in place")
        };
        return Err(why);
    }
    job_set(|j| {
        *j = JobStatus {
            state: "running".into(),
            step: "applying".into(),
            from: r.running.clone(),
            to: r.vendored.clone(),
            error: String::new(),
        }
    });
    tokio::spawn(async move {
        let outcome = run().await;
        if let Ok(client) = crate::k8s::client().await {
            revoke_grant(&client).await;
        }
        job_set(|j| match outcome {
            Ok(()) => {
                j.state = "done".into();
                j.step.clear();
            }
            Err(e) => {
                tracing::error!("operator upgrade failed: {e:#}");
                j.state = "failed".into();
                j.error = format!("{e:#}");
            }
        });
    });
    Ok(())
}

async fn run() -> Result<()> {
    let client = crate::k8s::client().await?;
    crate::bootstrap::apply_bundle(
        &client,
        &crate::bootstrap::operator_bundle(),
        crate::bootstrap::BOOTSTRAP_BINDING,
    )
    .await
    .context("applying the vendored operator bundle")?;
    job_set(|j| j.step = "waiting".into());
    wait_rollout(&client).await
}

/// The operator Deployment has rolled out its new spec: observed, every pod
/// updated and available, no pod of the old ReplicaSet left. "Ready" alone is
/// not it — the old controller stays Ready through the whole rollout, which on
/// the fabric reported a 6-second "done" while 3.0.0-alpha still ran.
pub fn rollout_complete(d: &k8s_openapi::api::apps::v1::Deployment) -> bool {
    let (Some(spec), Some(st)) = (d.spec.as_ref(), d.status.as_ref()) else {
        return false;
    };
    let want = spec.replicas.unwrap_or(1);
    st.observed_generation.unwrap_or(0) >= d.metadata.generation.unwrap_or(1)
        && st.updated_replicas.unwrap_or(0) == want
        && st.available_replicas.unwrap_or(0) == want
        && st.replicas.unwrap_or(0) == want
}

async fn wait_rollout(client: &Client) -> Result<()> {
    use k8s_openapi::api::apps::v1::Deployment;
    let ns = crate::k8s::ns();
    let name = crate::bootstrap::OPERATOR_DEPLOY;
    let api: Api<Deployment> = Api::namespaced(client.clone(), ns);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(OPERATOR_READY_SECS);
    loop {
        if let Ok(Some(d)) = api.get_opt(name).await {
            if rollout_complete(&d) {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("{ns}/{name} did not finish rolling out within {OPERATOR_READY_SECS}s");
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn crd(versions: Value) -> Value {
        json!({ "spec": { "versions": versions } })
    }

    fn schema(spec_props: Value) -> Value {
        json!({ "properties": { "spec": { "properties": spec_props } } })
    }

    fn v1(spec_props: Value) -> Value {
        crd(json!([{ "name": "v1", "schema": { "openAPIV3Schema": schema(spec_props) } }]))
    }

    #[test]
    fn schema_paths_walks_properties_and_items_and_stops_at_open_nodes() {
        let s = schema(json!({
            "nodePools": { "items": { "properties": { "replicas": {} } } },
            "free": { "x-kubernetes-preserve-unknown-fields": true, "properties": { "x": {} } },
            "map": { "additionalProperties": {} }
        }));
        let p = schema_paths(&s);
        assert!(p.contains(".spec.nodePools"));
        assert!(p.contains(".spec.nodePools[].replicas"));
        assert!(p.contains(".spec.free"));
        assert!(
            !p.contains(".spec.free.x"),
            "below preserve-unknown anything goes"
        );
        assert!(p.contains(".spec.map"));
    }

    #[test]
    fn a_new_crd_is_reported_as_new() {
        let c = diff_crd("x.opensearch.org", None, &v1(json!({})));
        assert!(c.new);
        assert!(!c.is_noop());
    }

    #[test]
    fn an_identical_crd_is_a_noop() {
        let d = v1(json!({ "a": {} }));
        assert!(diff_crd("x", Some(&d), &d).is_noop());
    }

    #[test]
    fn added_and_removed_fields_and_versions_are_counted() {
        let old = crd(json!([
            { "name": "v1", "schema": { "openAPIV3Schema": schema(json!({
                "gone": { "properties": { "child": {} } }, "kept": {} })) } },
            { "name": "v1beta1", "schema": { "openAPIV3Schema": {} } }
        ]));
        let new = crd(json!([
            { "name": "v1", "schema": { "openAPIV3Schema": schema(json!({
                "kept": {}, "fresh": {} })) } },
            { "name": "v2", "schema": { "openAPIV3Schema": {} } }
        ]));
        let c = diff_crd("x", Some(&old), &new);
        // The removed subtree is reported once, at its top.
        assert_eq!(c.removed_fields, vec![".spec.gone".to_string()]);
        assert_eq!(c.added_fields, 1);
        assert_eq!(c.removed_versions, vec!["v1beta1".to_string()]);
        assert_eq!(c.added_versions, vec!["v2".to_string()]);
    }

    #[test]
    fn breaking_uses_finds_only_removed_fields_the_object_sets() {
        let removed = vec![".spec.gone".to_string(), ".spec.unused".to_string()];
        let cr = json!({ "spec": { "gone": { "child": 1 }, "kept": true } });
        assert_eq!(breaking_uses(&removed, &cr), vec![".spec.gone".to_string()]);
    }

    #[test]
    fn a_prefix_that_is_not_a_parent_is_not_a_use() {
        // `.spec.general` is not under a removed `.spec.gen`.
        let cr = json!({ "spec": { "general": 1 } });
        assert!(breaking_uses(&[".spec.gen".to_string()], &cr).is_empty());
    }

    fn cluster(health: &str, replicas: &[i64]) -> Value {
        let pools: Vec<Value> = replicas
            .iter()
            .enumerate()
            .map(|(n, r)| json!({ "component": format!("p{n}"), "replicas": r }))
            .collect();
        json!({
            "metadata": { "namespace": "ns", "name": "logs" },
            "spec": { "general": { "version": "3.0.0" }, "nodePools": pools },
            "status": { "health": health, "version": "3.0.0" }
        })
    }

    fn inputs(clusters: Vec<Value>) -> Inputs {
        Inputs {
            running: Some("op:old".into()),
            vendored: "op:new".into(),
            operator_ready: true,
            drift: true,
            clusters,
            ..Default::default()
        }
    }

    #[test]
    fn a_green_multi_replica_cluster_has_no_blockers() {
        let mut i = inputs(vec![cluster("green", &[3])]);
        i.snapshot_covered.insert("ns/logs".into());
        let r = assess(&i);
        assert_eq!(r.state, "available");
        assert!(r.blockers.is_empty(), "{:?}", r.blockers);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn a_single_node_pool_warns_of_downtime_and_no_policy_warns_too() {
        let r = assess(&inputs(vec![cluster("green", &[1, 2])]));
        let codes: Vec<_> = r
            .warnings
            .iter()
            .map(|w| (w.code, w.subject.as_str()))
            .collect();
        assert!(codes.contains(&("single_replica_pool", "p0")));
        assert!(!codes.contains(&("single_replica_pool", "p1")));
        assert!(codes.iter().any(|(c, _)| *c == "no_snapshot_policy"));
        assert!(r.deployments[0].pools[0].downtime);
    }

    #[test]
    fn a_yellow_cluster_blocks() {
        let r = assess(&inputs(vec![cluster("yellow", &[3])]));
        assert!(r.blockers.iter().any(|b| b.code == "deployment_not_green"));
    }

    #[test]
    fn a_version_upgrade_in_flight_blocks() {
        let mut c = cluster("green", &[3]);
        c["spec"]["general"]["version"] = json!("3.1.0");
        let r = assess(&inputs(vec![c]));
        assert!(r
            .blockers
            .iter()
            .any(|b| b.code == "version_upgrade_in_flight"));
    }

    #[test]
    fn a_removed_field_a_cluster_sets_blocks() {
        let old = v1(json!({ "gone": {}, "nodePools": {} }));
        let new = v1(json!({ "nodePools": {} }));
        let mut c = cluster("green", &[3]);
        c["spec"]["gone"] = json!(true);
        let mut i = inputs(vec![c]);
        i.crds = vec![("opensearchclusters.opensearch.org".into(), Some(old), new)];
        let r = assess(&i);
        let b = r
            .blockers
            .iter()
            .find(|b| b.code == "crd_field_removed")
            .unwrap();
        assert_eq!(b.subject, ".spec.gone");
        assert_eq!(b.deployment, "ns/logs");
    }

    #[test]
    fn an_operator_not_ready_or_a_running_bootstrap_blocks() {
        let mut i = inputs(vec![]);
        i.operator_ready = false;
        i.bootstrap_running = true;
        let codes: Vec<_> = assess(&i).blockers.iter().map(|b| b.code).collect();
        assert!(codes.contains(&"operator_not_ready"));
        assert!(codes.contains(&"bootstrap_running"));
    }

    #[test]
    fn an_unmanaged_operator_reports_nothing_else() {
        let mut i = inputs(vec![cluster("yellow", &[1])]);
        i.unmanaged_reason = Some("foreign".into());
        let r = assess(&i);
        assert_eq!(r.state, "unmanaged");
        assert_eq!(r.blockers.len(), 1);
        assert!(r.deployments.is_empty());
    }

    #[test]
    fn no_drift_is_up_to_date_and_never_offers_the_button() {
        let mut i = inputs(vec![]);
        i.drift = false;
        let mut r = assess(&i);
        r.grant.present = true;
        assert_eq!(r.state, "up_to_date");
        assert!(!can_upgrade(&r));
    }

    #[test]
    fn the_button_needs_the_grant_and_no_running_job() {
        let mut r = assess(&inputs(vec![]));
        assert!(!can_upgrade(&r), "no grant");
        r.grant.present = true;
        assert!(can_upgrade(&r));
        r.job.state = "running".into();
        assert!(!can_upgrade(&r));
    }

    #[test]
    fn the_grant_expires_after_its_ttl_but_never_mid_upgrade() {
        assert!(!grant_due_for_revoke(GRANT_TTL_SECS - 1, false));
        assert!(grant_due_for_revoke(GRANT_TTL_SECS, false));
        assert!(!grant_due_for_revoke(GRANT_TTL_SECS * 4, true));
    }

    fn deploy(
        generation: i64,
        observed: i64,
        want: i32,
        total: i32,
        updated: i32,
        available: i32,
    ) -> k8s_openapi::api::apps::v1::Deployment {
        serde_json::from_value(json!({
            "metadata": { "generation": generation },
            "spec": { "replicas": want, "selector": {}, "template": {} },
            "status": { "observedGeneration": observed, "replicas": total,
                        "updatedReplicas": updated, "availableReplicas": available }
        }))
        .unwrap()
    }

    #[test]
    fn a_rollout_is_complete_only_when_the_old_pod_is_gone() {
        // The fabric's 6-second "done": new spec observed, old pod still Ready.
        assert!(!rollout_complete(&deploy(2, 2, 1, 2, 1, 1)));
        // Spec not observed yet.
        assert!(!rollout_complete(&deploy(2, 1, 1, 1, 1, 1)));
        // New pod not available yet.
        assert!(!rollout_complete(&deploy(2, 2, 1, 1, 1, 0)));
        assert!(rollout_complete(&deploy(2, 2, 1, 1, 1, 1)));
    }

    #[test]
    fn the_grant_command_names_this_app() {
        let c = grant_command("veloxsearch-system");
        assert!(c.contains(GRANT));
        assert!(c.contains("--clusterrole=cluster-admin"));
        assert!(c.ends_with("--serviceaccount=veloxsearch-system:veloxsearch"));
    }

    /// The vendored bundle really carries the operator CRDs this diffs —
    /// otherwise the report would claim "no CRD change" on every cluster.
    #[test]
    fn the_vendored_bundle_carries_the_cluster_crd() {
        let names: Vec<_> = vendored_crds()
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n == "opensearchclusters.opensearch.org"),
            "{names:?}"
        );
    }
}
