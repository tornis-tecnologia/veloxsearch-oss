// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Server-only self-bootstrap (ADR-014, ADR-022).
//!
//! "The only prerequisite is a Kubernetes cluster": on first launch the app
//! checks for cert-manager and the OpenSearch operator and installs whatever is
//! missing from manifests vendored at build time (`deploy/bootstrap/*.yaml`,
//! pinned to the versions validated on the dev cluster). Installation runs as a
//! detached tokio task — server fns return immediately and the UI polls
//! `bootstrap_status` — so no HTTP/proxy timeout can kill a half-done install.
//! Everything is applied with server-side apply and is safe to re-run.

use anyhow::{bail, Context, Result};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, ListParams, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::discovery::{Discovery, Scope};
use kube::Client;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::k8s::ns;

/// Vendored install bundles (see deploy/bootstrap/README note in DECISIONS ADR-022).
const CERT_MANAGER_BUNDLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/deploy/bootstrap/cert-manager.yaml"
));
const OPERATOR_BUNDLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/deploy/bootstrap/operator.yaml"
));
const LONGHORN_BUNDLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/deploy/bootstrap/longhorn.yaml"
));

/// The operator bundle was helm-rendered into `veloxsearch-test`; every
/// occurrence of that token semantically means "the operator's namespace", so
/// retargeting it to wherever the app actually runs is a plain token replace
/// (a no-op on a cluster that already has it, ADR-027).
fn operator_bundle() -> String {
    OPERATOR_BUNDLE.replace("veloxsearch-test", ns())
}

const CERT_MANAGER_CRD: &str = "certificates.cert-manager.io";
/// The `OpenSearchCluster` CRD names the operator chart really installs. It
/// ships BOTH group variants — the legacy `opensearch.opster.io` and the
/// current `opensearch.org` — and older charts shipped only the legacy one, so
/// a guard that tests a single name is half a guard (`#115`: the old R7
/// predicate tested "opensearchclusters.opster.io", a name no chart has ever
/// installed, which made the check silently always-false).
const OPERATOR_CRDS: &[&str] = &[
    "opensearchclusters.opensearch.org",
    "opensearchclusters.opensearch.opster.io",
];
const CERT_MANAGER_NS: &str = "cert-manager";
const CERT_MANAGER_DEPLOYS: &[&str] = &[
    "cert-manager",
    "cert-manager-webhook",
    "cert-manager-cainjector",
];
const OPERATOR_DEPLOY: &str = "opensearch-operator";

// --- Storage self-bootstrap (Longhorn, ADR-031/043) -------------------------
const LONGHORN_NS: &str = "longhorn-system";
/// StorageClass the bundle's driver-deployer creates, annotated default. The
/// OpenSearchCluster CR pins its PVCs to this class (ADR-043, k8s.rs).
pub const LONGHORN_SC: &str = "longhorn";
/// The CSI provisioner backing [`LONGHORN_SC`] — an SC merely *named* longhorn
/// but backed by something else does not count as Longhorn.
const LONGHORN_PROVISIONER: &str = "driver.longhorn.io";
const LONGHORN_MANAGER_DS: &str = "longhorn-manager";
const LONGHORN_DRIVER_DEPLOYER: &str = "longhorn-driver-deployer";
/// Replicas per volume the vendored bundle pins (the `longhorn` StorageClass
/// parameter the driver-deployer creates). The sizing reconcile (#26) lowers
/// it on clusters too small to ever place three copies.
const LONGHORN_DEFAULT_REPLICAS: u32 = 3;

/// Provisioners whose volumes don't survive a pod reschedule — "not real"
/// persistent storage (ADR-031), so a default backed by one triggers the
/// Longhorn self-install. Any provisioner containing "hostpath" also counts.
const NODE_LOCAL_PROVISIONERS: &[&str] = &[
    "rancher.io/local-path",
    "kubernetes.io/no-provisioner",
    "openebs.io/local",
];

fn provisioner_is_node_local(p: &str) -> bool {
    p.contains("hostpath") || NODE_LOCAL_PROVISIONERS.contains(&p)
}

/// True if this StorageClass carries either form of the default-class annotation.
fn sc_is_default(sc: &k8s_openapi::api::storage::v1::StorageClass) -> bool {
    sc.metadata.annotations.as_ref().is_some_and(|a| {
        a.get("storageclass.kubernetes.io/is-default-class")
            .or_else(|| a.get("storageclass.beta.kubernetes.io/is-default-class"))
            .map(|v| v == "true")
            .unwrap_or(false)
    })
}

/// Storage classification for the self-bootstrap (ADR-031, amended by
/// ADR-043): **Longhorn is the only supported deployment storage**. The gate
/// no longer asks "is there a real default StorageClass?" but "is the Longhorn
/// StorageClass present and usable?" — a foreign real CSI default (EBS, Ceph…)
/// no longer satisfies it; Longhorn still gets installed and the CR pins its
/// PVCs to [`LONGHORN_SC`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeploymentStorage {
    /// The `longhorn` SC (provisioner `driver.longhorn.io`) exists — ready.
    /// `default` records whether it also carries the default annotation
    /// (informational: PVCs are pinned, so being default is not required).
    Longhorn { default: bool },
    /// A real (non-node-local) CSI default exists, but it is not Longhorn —
    /// still install Longhorn (ADR-043); the foreign class is left untouched.
    ForeignDefault(String),
    /// The default is node-local (won't survive reschedule) — install Longhorn.
    NodeLocal(String),
    /// No default StorageClass at all — install Longhorn.
    Absent,
}

impl DeploymentStorage {
    /// The storage-ready signal cluster creation gates on: the Longhorn SC is
    /// in place (ADR-043).
    pub fn longhorn_ready(&self) -> bool {
        matches!(self, DeploymentStorage::Longhorn { .. })
    }
    /// Whether the Longhorn self-install needs to run.
    pub fn needs_longhorn(&self) -> bool {
        !self.longhorn_ready()
    }
}

/// Pure classification over a StorageClass list (unit-testable without a
/// cluster): Longhorn presence wins; otherwise fall back to describing the
/// default class so the UX can say what will be installed over what.
fn classify_storage_classes(
    items: &[k8s_openapi::api::storage::v1::StorageClass],
) -> DeploymentStorage {
    if let Some(sc) = items.iter().find(|sc| {
        sc.metadata.name.as_deref() == Some(LONGHORN_SC) && sc.provisioner == LONGHORN_PROVISIONER
    }) {
        return DeploymentStorage::Longhorn {
            default: sc_is_default(sc),
        };
    }
    match items.iter().find(|sc| sc_is_default(sc)) {
        Some(sc) => {
            let name = sc.metadata.name.clone().unwrap_or_default();
            if provisioner_is_node_local(&sc.provisioner) {
                DeploymentStorage::NodeLocal(name)
            } else {
                DeploymentStorage::ForeignDefault(name)
            }
        }
        None => DeploymentStorage::Absent,
    }
}

/// Inspect the cluster's StorageClasses and classify them (ADR-031/043). This
/// is the detection trigger for the storage self-install and the source of the
/// storage-ready signal (`.longhorn_ready()`).
pub async fn classify_storage(client: &Client) -> Result<DeploymentStorage> {
    use k8s_openapi::api::storage::v1::StorageClass;
    let api: Api<StorageClass> = Api::all(client.clone());
    let list = api
        .list(&ListParams::default())
        .await
        .context("listing storageclasses")?;
    Ok(classify_storage_classes(&list.items))
}

/// Whether the StorageClass backing deployment PVCs permits online volume
/// expansion (`allowVolumeExpansion: true`). Used by the disk-resize guard:
/// growing a deployment's PVC only takes effect if the backing class allows it;
/// otherwise the CR change applies but the volume never grows. PVCs are pinned
/// to the `longhorn` SC (ADR-043), so that class is consulted first; the
/// default class stands in for pre-pin deployments. Returns `None` when
/// neither exists (caller stays permissive — `ensure_storage_ready` already
/// gates creation on Longhorn being ready).
pub async fn deployment_sc_allows_expansion(client: &Client) -> Result<Option<bool>> {
    use k8s_openapi::api::storage::v1::StorageClass;
    let api: Api<StorageClass> = Api::all(client.clone());
    let list = api
        .list(&ListParams::default())
        .await
        .context("listing storageclasses")?;
    Ok(list
        .items
        .iter()
        .find(|sc| sc.metadata.name.as_deref() == Some(LONGHORN_SC))
        .or_else(|| list.items.iter().find(|sc| sc_is_default(sc)))
        .map(|sc| sc.allow_volume_expansion.unwrap_or(false)))
}

/// Storage-ready gate for cluster creation (`#14`, ADR-031/043). OpenSearch
/// node pools claim a PVC (`#11`) pinned to the `longhorn` StorageClass
/// (ADR-043), so creation must never proceed until Longhorn is usable — a
/// foreign CSI default does not satisfy the gate. Longhorn already present
/// passes immediately; otherwise VeloxSearch auto-installs it and only returns
/// `Ok` once the `longhorn` SC is in place.
///
/// Longhorn is auto-installed, not prompted (operator decision, 2026-06-30:
/// "install it and inform" — the user is told, not asked). Progress is surfaced
/// through the shared install-job snapshot (`status().installing` /
/// `storage_status().installing`), so the create flow shows an "installing
/// Longhorn storage…" step and a completion notice instead of an opaque spinner.
/// The bootstrap cluster-admin binding is still present here because the
/// self-revoke is deferred until storage is durable (ADR-027/031) — installing
/// Longhorn applies CRDs + broad ClusterRoles and needs that binding; once the
/// install makes storage durable we drop it. If a node can't run Longhorn
/// (`#15`), the install's own error is surfaced so the caller refuses with a
/// clear message rather than provisioning into Pending storage.
pub async fn ensure_storage_ready(client: &Client) -> Result<()> {
    if classify_storage(client).await?.longhorn_ready() {
        // Pre-existing Longhorn still gets its replica sizing reconciled: a
        // single-node cluster carrying the bundle default of three faults
        // every volume (#26), however Longhorn got there.
        reconcile_longhorn_sizing(client).await?;
        return Ok(());
    }
    match install_longhorn(client)
        .await
        .context("cluster creation refused: Longhorn is unavailable and could not be installed — Longhorn is the only supported deployment storage (ADR-043)")
    {
        Ok(()) => {
            // Surface "done" on the shared progress channel, then drop the
            // bootstrap binding we kept for exactly this install.
            job_set(Job::Idle);
            revoke_bootstrap(client).await;
        }
        Err(e) => {
            // Make the failure visible to a concurrent status poll, then bubble.
            job_set(Job::Failed(format!("{e:#}")));
            return Err(e);
        }
    }
    // `install_longhorn` already asserts the `longhorn` SC before returning;
    // this re-check keeps the gate's contract explicit and independent.
    if classify_storage(client).await?.longhorn_ready() {
        Ok(())
    } else {
        bail!(
            "cluster creation refused: the `longhorn` StorageClass is still missing after the \
             Longhorn install — Longhorn is the only supported deployment storage (ADR-043)"
        )
    }
}

/// Read-only storage classification for the "auto-install + notify" Longhorn
/// flow (ADR-031, 2026-06-30). The create flow polls this to learn whether an
/// auto-install will be triggered (`needs_longhorn`) and, while one runs, to
/// render progress from the shared install-job snapshot (`installing`/`error`) —
/// no separate progress channel. Purely informational: reading it never installs
/// anything.
#[derive(Clone, Debug, Default)]
pub struct StorageState {
    /// Longhorn is in place (ADR-043) — clusters can be created.
    pub durable: bool,
    /// The `longhorn` SC is missing — creation auto-installs Longhorn.
    pub needs_longhorn: bool,
    /// Name of the StorageClass deployments will use (Longhorn once ready),
    /// or the current default while Longhorn is still missing.
    pub default_class: Option<String>,
    /// Human description of the current state ("node-local default 'local-path'").
    pub detail: String,
    /// The install job's current step (e.g. "longhorn") while a job runs.
    pub installing: Option<String>,
    /// The last install job's error, if it failed.
    pub error: Option<String>,
    /// Node packages Longhorn reports missing (`MissingDependency`), mapped to
    /// per-distro install commands the frontend renders verbatim (`#15`).
    pub missing_packages: Vec<MissingPackage>,
}

/// Map a storage classification into the read-only fields the create flow
/// shows: the `durable`/`needs_longhorn` flags, the class name deployments
/// (will) use, and a human description. Pure (no I/O) so the notify-flow
/// decision logic is unit tested without a cluster.
fn describe_storage(ds: &DeploymentStorage) -> (bool, bool, Option<String>, String) {
    match ds {
        DeploymentStorage::Longhorn { default } => (
            true,
            false,
            Some(LONGHORN_SC.to_string()),
            if *default {
                format!("Longhorn ready (default '{LONGHORN_SC}')")
            } else {
                format!("Longhorn ready (PVCs pinned to '{LONGHORN_SC}')")
            },
        ),
        DeploymentStorage::ForeignDefault(n) => (
            false,
            true,
            Some(n.clone()),
            format!("foreign CSI default '{n}' — Longhorn required (ADR-043)"),
        ),
        DeploymentStorage::NodeLocal(n) => (
            false,
            true,
            Some(n.clone()),
            format!("node-local default '{n}'"),
        ),
        DeploymentStorage::Absent => (false, true, None, "no default StorageClass".to_string()),
    }
}

/// Classify the cluster's storage WITHOUT installing anything (ADR-031/043).
/// Backs the read-only `storage_status` endpoint the create flow polls to decide
/// whether to show the "installing Longhorn storage…" progress + completion
/// notice. Reuses the shared job snapshot so an in-flight auto-install reports
/// progress here too, and surfaces the structured missing-node-package list
/// while Longhorn can't come up (`#15`).
pub async fn storage_status() -> Result<StorageState> {
    let client = crate::k8s::client().await?;
    let ds = classify_storage(&client).await?;
    let (installing, error) = job_snapshot();
    let (durable, needs_longhorn, default_class, detail) = describe_storage(&ds);
    let missing_packages = if durable {
        Vec::new()
    } else {
        longhorn_missing_packages(&client).await
    };
    Ok(StorageState {
        durable,
        needs_longhorn,
        default_class,
        detail,
        installing,
        error,
        missing_packages,
    })
}

/// Install job state, shared between the spawned installer and `status()`.
enum Job {
    Idle,
    Running(String),
    Failed(String),
}
static JOB: Mutex<Job> = Mutex::new(Job::Idle);

fn job_snapshot() -> (Option<String>, Option<String>) {
    match &*JOB.lock().unwrap() {
        Job::Idle => (None, None),
        Job::Running(step) => (Some(step.clone()), None),
        Job::Failed(e) => (None, Some(e.clone())),
    }
}
fn job_set(j: Job) {
    *JOB.lock().unwrap() = j;
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub cert_manager_installed: bool,
    pub cert_manager_ready: bool,
    pub operator_installed: bool,
    pub operator_ready: bool,
    /// Everything present and serving — "in conformity, ready to deploy".
    pub ready: bool,
    /// Step currently being installed by the background job, if any.
    pub installing: Option<String>,
    pub error: Option<String>,
    /// REQUIREMENTS.md R1–R8 evaluated against this cluster (probe v2).
    pub requirements: Vec<Requirement>,
    /// Any requirement failed — the installer refuses to run (ADR-026:
    /// unsupported clusters get a clear refusal, never a half-install).
    pub unsupported: bool,
}

/// One evaluated requirement row (id matches docs/REQUIREMENTS.md).
#[derive(Clone, Debug)]
pub struct Requirement {
    pub id: &'static str,
    /// "pass" | "warn" | "fail"
    pub status: &'static str,
    pub detail: String,
}

fn req(id: &'static str, status: &'static str, detail: impl Into<String>) -> Requirement {
    Requirement {
        id,
        status,
        detail: detail.into(),
    }
}

/// Parse a Kubernetes resource Quantity ("12251496Ki", "4", "500m") into a
/// plain f64 (bytes for memory, cores for cpu). Good enough for floor checks.
fn parse_qty(s: &str) -> f64 {
    let cut = s
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(s.len());
    let n: f64 = s[..cut].parse().unwrap_or(0.0);
    let mult = match &s[cut..] {
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "k" | "K" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "m" => 1e-3,
        _ => 1.0,
    };
    n * mult
}

const GI: f64 = 1024.0 * 1024.0 * 1024.0;
/// Floor for the smallest preset: 3×2Gi OpenSearch requests + dashboards +
/// operator + cert-manager + agent (REQUIREMENTS.md R4).
const MIN_MEM_GI: f64 = 8.0;
const MIN_CPU: f64 = 2.0;

/// Evaluate REQUIREMENTS.md against the live cluster. `conformant` flips R2
/// to informational (bootstrap powers are *supposed* to be revoked by then).
/// `operators` is the already-taken cluster-wide operator probe (`#115`) — passed
/// in so a `status()` call probes for operators once, not twice.
async fn check_requirements(
    client: &Client,
    conformant: bool,
    installing: bool,
    operators: &OperatorPresence,
) -> Vec<Requirement> {
    use k8s_openapi::api::authorization::v1::{
        ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
    };
    use k8s_openapi::api::core::v1::Node;
    use kube::api::ListParams;

    let mut out = Vec::new();

    // R1 — Kubernetes version ≥ 1.30
    match client.apiserver_version().await {
        Ok(v) => {
            let major: u32 = v
                .major
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .unwrap_or(0);
            let minor: u32 = v
                .minor
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .unwrap_or(0);
            let ok = (major, minor) >= (1, 30);
            out.push(req("r1", if ok { "pass" } else { "fail" }, v.git_version));
        }
        Err(e) => out.push(req("r1", "warn", format!("version probe failed: {e}"))),
    }

    // R2 — cluster-admin at install time (moot once bootstrapped + revoked)
    if conformant {
        out.push(req("r2", "pass", "bootstrap complete (powers revoked)"));
    } else {
        let ssar = SelfSubjectAccessReview {
            spec: SelfSubjectAccessReviewSpec {
                resource_attributes: Some(ResourceAttributes {
                    verb: Some("create".into()),
                    group: Some("apiextensions.k8s.io".into()),
                    resource: Some("customresourcedefinitions".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let api: Api<SelfSubjectAccessReview> = Api::all(client.clone());
        match api.create(&Default::default(), &ssar).await {
            Ok(r) => {
                let allowed = r.status.map(|s| s.allowed).unwrap_or(false);
                out.push(if allowed {
                    req("r2", "pass", "cluster-admin available")
                } else {
                    req("r2", "fail", "cannot create CRDs")
                });
            }
            Err(e) => out.push(req("r2", "warn", format!("permission probe failed: {e}"))),
        }
    }

    // R3 — Longhorn storage (ADR-031/043). A missing `longhorn` SC is NOT a
    // hard fail: it is a remediation step — VeloxSearch will install Longhorn
    // and gate cluster creation on it being ready (`#14`). A foreign CSI
    // default no longer passes (ADR-043). Reuse the shared classifier so this
    // row and the gate agree on what "ready" means.
    match classify_storage(client).await {
        Ok(DeploymentStorage::Longhorn { .. }) => out.push(req("r3", "pass", LONGHORN_SC)),
        Ok(other) => {
            let what = match &other {
                DeploymentStorage::ForeignDefault(name) => {
                    format!("foreign CSI default '{name}' (only Longhorn is supported, ADR-043)")
                }
                DeploymentStorage::NodeLocal(name) => format!("node-local default '{name}'"),
                DeploymentStorage::Absent => "no default StorageClass".to_string(),
                DeploymentStorage::Longhorn { .. } => unreachable!("Longhorn handled above"),
            };
            let verb = if installing {
                "installing"
            } else {
                "will install"
            };
            out.push(req(
                "r3",
                "warn",
                format!("{what} — {verb} Longhorn (needs open-iscsi on every node)"),
            ));
        }
        Err(e) => out.push(req("r3", "warn", format!("storage probe failed: {e}"))),
    }

    // R4 + R5 — resource floor and architecture, from the same node list
    {
        let api: Api<Node> = Api::all(client.clone());
        match api.list(&ListParams::default()).await {
            Ok(list) => {
                let (mut mem, mut cpu) = (0.0_f64, 0.0_f64);
                let mut archs: Vec<String> = Vec::new();
                for n in &list.items {
                    let schedulable = !n
                        .spec
                        .as_ref()
                        .and_then(|s| s.unschedulable)
                        .unwrap_or(false);
                    if let Some(st) = &n.status {
                        if let Some(info) = &st.node_info {
                            if !archs.contains(&info.architecture) {
                                archs.push(info.architecture.clone());
                            }
                        }
                        if schedulable {
                            if let Some(alloc) = &st.allocatable {
                                mem += alloc.get("memory").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                                cpu += alloc.get("cpu").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                            }
                        }
                    }
                }
                let mem_gi = mem / GI;
                let st = if mem_gi >= MIN_MEM_GI && cpu >= MIN_CPU {
                    "pass"
                } else if mem_gi >= MIN_MEM_GI * 0.75 {
                    "warn"
                } else {
                    "fail"
                };
                out.push(req(
                    "r4",
                    st,
                    format!("{mem_gi:.1}Gi / {cpu:.1} vCPU allocatable (floor {MIN_MEM_GI}Gi / {MIN_CPU})"),
                ));
                let amd64_only = !archs.is_empty() && archs.iter().all(|a| a == "amd64");
                out.push(req(
                    "r5",
                    if amd64_only { "pass" } else { "fail" },
                    archs.join(", "),
                ));
            }
            Err(e) => {
                out.push(req("r4", "warn", format!("node probe failed: {e}")));
                out.push(req("r5", "warn", "unknown"));
            }
        }
    }

    // R6 — registry egress: exercised by the install itself (image pulls).
    out.push(req("r6", "pass", "exercised during install (image pulls)"));

    // R7 — no conflicting pre-existing stack. The operator half is cluster-scoped
    // (`#115`): an operator running in ANY namespace conflicts with ours, so it is
    // classified from a cluster-wide probe, not from our own namespace.
    {
        if let Some(row) = operator_r7(operators, allow_foreign_operator()) {
            out.push(row);
        } else {
            let cm = crd_present(client, CERT_MANAGER_CRD).await;
            let mut cm_ready = cm;
            if cm {
                for d in CERT_MANAGER_DEPLOYS {
                    if !deploy_ready(client, CERT_MANAGER_NS, d).await {
                        cm_ready = false;
                        break;
                    }
                }
            }
            if cm && !cm_ready && !installing {
                out.push(req("r7", "warn", "pre-existing cert-manager not ready"));
            } else {
                out.push(req("r7", "pass", "clean"));
            }
        }
    }

    // R8 — ingress (optional, informational): decides which access modes exist.
    match crate::access::ingress_classes().await {
        Ok(c) if c.is_empty() => out.push(req("r8", "pass", "none — port-forward only")),
        Ok(c) => out.push(req("r8", "pass", c.join(", "))),
        Err(e) => out.push(req("r8", "warn", format!("ingress probe failed: {e}"))),
    }

    out
}

async fn deploy_ready(client: &Client, ns: &str, name: &str) -> bool {
    let api: Api<Deployment> = Api::namespaced(client.clone(), ns);
    match api.get_opt(name).await {
        Ok(Some(d)) => d
            .status
            .and_then(|s| s.ready_replicas)
            .map(|r| r >= 1)
            .unwrap_or(false),
        _ => false,
    }
}

async fn crd_present(client: &Client, name: &str) -> bool {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());
    matches!(api.get_opt(name).await, Ok(Some(_)))
}

// --- Pre-existing OpenSearch operator detection (R7, `#115`) ----------------
//
// Two cluster-wide operators reconciling the same `OpenSearchCluster` CRs is a
// silent corruption, so the guard that prevents it must be loud and must look
// at the WHOLE cluster: the operator sidecar installs (sidecar!7) lands in
// namespace `opensearch`, which a probe scoped to `ns()` cannot see.

/// Label the opensearch-operator helm chart puts on its controller Deployment
/// whatever the release name or namespace is.
const OPERATOR_LABEL_KEY: &str = "app.kubernetes.io/name";
const OPERATOR_LABEL_VALUE: &str = "opensearch-operator";

/// Explicit override: proceed even though a foreign operator was found.
const ALLOW_FOREIGN_OPERATOR_ENV: &str = "VELOX_ALLOW_FOREIGN_OPERATOR";

fn allow_foreign_operator() -> bool {
    matches!(
        std::env::var(ALLOW_FOREIGN_OPERATOR_ENV)
            .ok()
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// One operator controller Deployment seen somewhere in the cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OperatorDeploy {
    namespace: String,
    name: String,
    ready: bool,
}

/// What the cluster says about OpenSearch operators, cluster-wide.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OperatorPresence {
    /// No operator CRDs and no operator Deployment anywhere.
    Absent,
    /// An operator runs in our own namespace — the one we installed.
    Ours { ready: bool },
    /// An operator runs in a namespace we do not own: a conflict, because both
    /// controllers watch the same cluster-scoped CRDs.
    Foreign {
        namespace: String,
        name: String,
        ready: bool,
    },
    /// Operator CRDs exist but nothing is running them. Not a live conflict
    /// (nothing reconciles), but our bundle would apply its pinned CRDs over
    /// whatever version is there — worth saying out loud. Deliberately NOT a
    /// refusal: an interrupted install of our own leaves exactly this state,
    /// and failing it would wedge the retry.
    UnmanagedCrds,
    /// The probe itself failed. Reported, never silently treated as "clean" —
    /// an unprovable guard is what `#115` was.
    Unknown(String),
}

/// True if this Deployment is an OpenSearch operator controller. Matches the
/// chart label first (survives any helm release name — sidecar's release is
/// `opensearch-operator`, ours is too) and falls back to the Deployment name,
/// which covers hand-rolled and kustomize installs.
fn deploy_is_operator(name: &str, labels: Option<&BTreeMap<String, String>>) -> bool {
    let labelled = labels
        .and_then(|l| l.get(OPERATOR_LABEL_KEY))
        .is_some_and(|v| v == OPERATOR_LABEL_VALUE);
    labelled || name.contains(OPERATOR_LABEL_VALUE)
}

/// Pure classification over what the cluster returned — unit-testable without a
/// cluster, same shape as [`classify_storage_classes`]. `crds` is the list of
/// CRD names present; only the names in [`OPERATOR_CRDS`] count.
fn classify_operator(
    crds: &[String],
    deploys: &[OperatorDeploy],
    own_ns: &str,
) -> OperatorPresence {
    // Pick deterministically so the remediation message names the same operator
    // on every poll instead of flapping with list order.
    let foreign = deploys
        .iter()
        .filter(|d| d.namespace != own_ns)
        .min_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    if let Some(d) = foreign {
        return OperatorPresence::Foreign {
            namespace: d.namespace.clone(),
            name: d.name.clone(),
            ready: d.ready,
        };
    }
    if let Some(d) = deploys.iter().find(|d| d.namespace == own_ns) {
        return OperatorPresence::Ours { ready: d.ready };
    }
    if crds.iter().any(|c| OPERATOR_CRDS.contains(&c.as_str())) {
        return OperatorPresence::UnmanagedCrds;
    }
    OperatorPresence::Absent
}

/// `(installed, ready)` as the UI and the install gate see it. A foreign
/// operator counts as installed AND (if it is serving) ready: it really is an
/// operator, and pretending otherwise is how we ended up installing a second.
/// Whether we may *use* it is decided by [`operator_r7`] / [`bootstrap_ready`].
fn operator_flags(p: &OperatorPresence) -> (bool, bool) {
    match p {
        OperatorPresence::Absent | OperatorPresence::Unknown(_) => (false, false),
        OperatorPresence::UnmanagedCrds => (true, false),
        OperatorPresence::Ours { ready } => (true, *ready),
        OperatorPresence::Foreign { ready, .. } => (true, *ready),
    }
}

/// The R7 row the operator half implies, or `None` when the operator half is
/// clean and the cert-manager half decides the row.
fn operator_r7(p: &OperatorPresence, allow_foreign: bool) -> Option<Requirement> {
    match p {
        // The refusal text is `foreign_operator_block`'s, verbatim — the row the
        // user reads and the error the installer bails with are one string.
        OperatorPresence::Foreign {
            namespace, name, ..
        } => Some(match foreign_operator_block(p, allow_foreign) {
            Some(why) => req("r7", "fail", why),
            None => req(
                "r7",
                "warn",
                format!(
                    "foreign OpenSearch operator {namespace}/{name} — adopted because \
                         {ALLOW_FOREIGN_OPERATOR_ENV} is set; VeloxSearch will not install its own"
                ),
            ),
        }),
        OperatorPresence::UnmanagedCrds => Some(req(
            "r7",
            "warn",
            "OpenSearch operator CRDs present with no operator running — the install \
             will apply its pinned CRDs over them",
        )),
        OperatorPresence::Unknown(e) => Some(req(
            "r7",
            "warn",
            format!("operator probe failed: {e} — a pre-existing operator cannot be ruled out"),
        )),
        OperatorPresence::Absent | OperatorPresence::Ours { .. } => None,
    }
}

/// Why a foreign operator forbids installing ours, or `None` when we may
/// proceed. THE single predicate behind the refusal: `bootstrap_ready` (the
/// conformity verdict) and the gate in `run_install` (the destructive act) both
/// call it, so the screen and the installer can never disagree about whether
/// the override was set. Note what it does NOT look at: operator *readiness*.
/// A foreign operator that is merely starting is the same conflict as one that
/// is serving, and gating on readiness is how the check gets skipped exactly
/// when it matters.
fn foreign_operator_block(p: &OperatorPresence, allow_foreign: bool) -> Option<String> {
    match p {
        OperatorPresence::Foreign {
            namespace, name, ..
        } if !allow_foreign => Some(format!(
            "foreign OpenSearch operator {namespace}/{name} — installing ours would leave \
             two cluster-wide operators reconciling the same OpenSearchCluster CRs. \
             Remove it (`helm uninstall` in namespace {namespace}), or set \
             {ALLOW_FOREIGN_OPERATOR_ENV}=1 to use it instead of installing ours"
        )),
        _ => None,
    }
}

/// "In conformity, ready to deploy". A blocking foreign operator is never
/// ready: reporting green here is what would let the wizard drive CRs against
/// an operator we neither own nor version-match.
fn bootstrap_ready(
    cert_manager_ready: bool,
    operator_ready: bool,
    p: &OperatorPresence,
    allow_foreign: bool,
) -> bool {
    cert_manager_ready && operator_ready && foreign_operator_block(p, allow_foreign).is_none()
}

/// Probe the cluster for OpenSearch operators: the CRDs (cluster-scoped) plus
/// controller Deployments in EVERY namespace. The all-namespace Deployment list
/// mirrors what `discovery.rs` already does and needs no RBAC beyond the
/// `apps/deployments` read the runtime ClusterRole already grants.
async fn scan_operators(client: &Client) -> Result<OperatorPresence, kube::Error> {
    let mut crds = Vec::new();
    for name in OPERATOR_CRDS {
        if crd_present(client, name).await {
            crds.push((*name).to_string());
        }
    }
    let api: Api<Deployment> = Api::all(client.clone());
    let deploys: Vec<OperatorDeploy> = api
        .list(&ListParams::default())
        .await?
        .items
        .into_iter()
        .filter_map(|d| {
            let name = d.metadata.name.clone().unwrap_or_default();
            if !deploy_is_operator(&name, d.metadata.labels.as_ref()) {
                return None;
            }
            Some(OperatorDeploy {
                namespace: d.metadata.namespace.clone().unwrap_or_default(),
                name,
                ready: d
                    .status
                    .as_ref()
                    .and_then(|s| s.ready_replicas)
                    .is_some_and(|r| r >= 1),
            })
        })
        .collect();
    Ok(classify_operator(&crds, &deploys, ns()))
}

/// Current conformity of the cluster + install-job state.
pub async fn status() -> Result<State> {
    let client = crate::k8s::client().await?;
    let cert_manager_installed = crd_present(&client, CERT_MANAGER_CRD).await;
    let mut cert_manager_ready = cert_manager_installed;
    if cert_manager_installed {
        for d in CERT_MANAGER_DEPLOYS {
            if !deploy_ready(&client, CERT_MANAGER_NS, d).await {
                cert_manager_ready = false;
                break;
            }
        }
    }
    // Cluster-scoped, not `ns()`-scoped (`#115`): an operator in any namespace is
    // the fact we care about, and a probe failure is reported, never assumed clean.
    let operators = scan_operators(&client)
        .await
        .unwrap_or_else(|e| OperatorPresence::Unknown(e.to_string()));
    let (operator_installed, operator_ready) = operator_flags(&operators);
    let (installing, error) = job_snapshot();
    let ready = bootstrap_ready(
        cert_manager_ready,
        operator_ready,
        &operators,
        allow_foreign_operator(),
    );
    let requirements = check_requirements(&client, ready, installing.is_some(), &operators).await;
    let unsupported = requirements.iter().any(|r| r.status == "fail");
    Ok(State {
        ready,
        cert_manager_installed,
        cert_manager_ready,
        operator_installed,
        operator_ready,
        installing,
        error,
        requirements,
        unsupported,
    })
}

/// Kick off installation of whatever is missing (idempotent, returns
/// immediately; progress is visible via `status()`). No-op when a job is
/// already running or the cluster is already in conformity.
pub async fn ensure() -> Result<State> {
    let st = status().await?;
    if st.ready || st.installing.is_some() {
        if st.ready {
            // Covers "bootstrapped earlier but the revoke never ran" (e.g. the
            // app restarted between install and revoke, or pre-ADR-027 installs).
            // Keep the deferred-revoke contract (ADR-031): only drop the binding
            // once Longhorn is in place, since any cluster without it still
            // needs cluster-admin for the create-flow Longhorn auto-install.
            if let Ok(client) = crate::k8s::client().await {
                if classify_storage(&client)
                    .await
                    .map(|d| d.longhorn_ready())
                    .unwrap_or(false)
                {
                    revoke_bootstrap(&client).await;
                }
            }
        }
        return Ok(st);
    }
    // ADR-026: unsupported clusters get a clear refusal, never a half-install.
    if st.unsupported {
        return Ok(st);
    }
    job_set(Job::Running("starting".into()));
    tokio::spawn(async {
        match run_install().await {
            Ok(()) => job_set(Job::Idle),
            Err(e) => {
                tracing::error!("bootstrap install failed: {e:#}");
                job_set(Job::Failed(format!("{e:#}")));
            }
        }
    });
    status().await
}

async fn run_install() -> Result<()> {
    let client = crate::k8s::client().await?;

    // 1. cert-manager (the operator's webhook certs depend on it).
    let st = status().await?;
    if !st.cert_manager_ready {
        job_set(Job::Running("cert-manager".into()));
        apply_bundle(&client, CERT_MANAGER_BUNDLE, BOOTSTRAP_BINDING)
            .await
            .context("installing cert-manager")?;
        for d in CERT_MANAGER_DEPLOYS {
            wait_deploy(&client, CERT_MANAGER_NS, d, 300).await?;
        }
    }

    // 2. our namespace must exist before the operator lands in it.
    let ns_doc = serde_json::json!({
        "apiVersion": "v1", "kind": "Namespace", "metadata": { "name": ns() }
    });
    apply_doc(
        &client,
        &Discovery::new(client.clone()).run().await?,
        ns_doc,
        BOOTSTRAP_BINDING,
    )
    .await
    .context("ensuring app namespace")?;

    // 3. OpenSearch operator (bundle contains cert-manager Certificate/Issuer
    //    CRs, which is why cert-manager must be serving first).
    // `#115` — the guard at the moment of the destructive act, BEFORE the
    // readiness question. `ensure()` already refuses on R7, but that verdict is a
    // poll old, so re-probe here: no race may land us in a dual-operator cluster.
    // Deliberately outside the `if !operator_ready` below — a *serving* foreign
    // operator reports `operator_ready == true`, so a guard nested in that branch
    // would be skipped in precisely the case `#115` is about. Fail CLOSED: a probe
    // error means we cannot prove the cluster is clean, and applying the bundle is
    // not undoable.
    let presence = scan_operators(&client)
        .await
        .context("probing for a pre-existing OpenSearch operator")?;
    if let Some(why) = foreign_operator_block(&presence, allow_foreign_operator()) {
        bail!("{why}");
    }

    let st = status().await?;
    if !st.operator_ready {
        job_set(Job::Running("opensearch-operator".into()));
        apply_bundle(&client, &operator_bundle(), BOOTSTRAP_BINDING)
            .await
            .context("installing opensearch-operator")?;
        wait_deploy(&client, ns(), OPERATOR_DEPLOY, 300).await?;
    }

    // 4. Storage (ADR-031/043): Longhorn install is DEFERRED to first cluster
    //    creation (operator decision, 2026-06-30: "install + notify"). Installing
    //    it here would block bootstrap on a multi-minute storage install the user
    //    can't see; instead the create flow's storage-ready gate auto-installs it
    //    and surfaces progress + a completion notice. A cluster that already has
    //    Longhorn (prod) needs nothing. So this step installs nothing.

    // 5. Bootstrap powers are one-shot (ADR-027) — BUT the deferred Longhorn
    //    install still needs cluster-admin (it applies CRDs + broad ClusterRoles).
    //    Revoking now, before that install can run, would make it fail. So only
    //    revoke once Longhorn is already in place; any other cluster keeps the
    //    binding until the create-flow auto-install makes storage durable and
    //    revokes it then (`ensure_storage_ready`).
    if classify_storage(&client).await?.longhorn_ready() {
        revoke_bootstrap(&client).await;
    }
    Ok(())
}

const BOOTSTRAP_BINDING: &str = "veloxsearch-bootstrap";

/// Delete the `veloxsearch-bootstrap` ClusterRoleBinding if it still exists.
/// Best-effort: a failure only means the binding lingers (= today's manual
/// "delete me afterwards" state), so log and move on.
async fn revoke_bootstrap(client: &Client) {
    use k8s_openapi::api::rbac::v1::ClusterRoleBinding;
    let api: Api<ClusterRoleBinding> = Api::all(client.clone());
    match api.get_opt(BOOTSTRAP_BINDING).await {
        Ok(Some(_)) => match api.delete(BOOTSTRAP_BINDING, &Default::default()).await {
            Ok(_) => tracing::info!("bootstrap complete — revoked the {BOOTSTRAP_BINDING} cluster-admin binding (ADR-027)"),
            Err(e) => tracing::warn!("could not revoke {BOOTSTRAP_BINDING}: {e}"),
        },
        Ok(None) => {}
        Err(e) => tracing::warn!("could not check {BOOTSTRAP_BINDING}: {e}"),
    }
}

pub async fn wait_deploy(client: &Client, ns: &str, name: &str, secs: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if deploy_ready(client, ns, name).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    bail!("timed out waiting for deployment {ns}/{name} to become ready")
}

async fn daemonset_ready(client: &Client, ns: &str, name: &str) -> bool {
    use k8s_openapi::api::apps::v1::DaemonSet;
    let api: Api<DaemonSet> = Api::namespaced(client.clone(), ns);
    match api.get_opt(name).await {
        Ok(Some(d)) => d.status.map(|s| s.number_ready >= 1).unwrap_or(false),
        _ => false,
    }
}

async fn wait_daemonset(client: &Client, ns: &str, name: &str, secs: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if daemonset_ready(client, ns, name).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    bail!("timed out waiting for daemonset {ns}/{name} to become ready")
}

/// A `nodes.longhorn.io` node that can't host Longhorn volumes (`#15`).
/// `fatal` marks a deterministic, operator-action-required prerequisite gap
/// (e.g. `open-iscsi`/`iscsiadm` missing → Longhorn reason `MissingDependency`)
/// that won't clear on its own, so the wait fails fast instead of timing out.
#[derive(Debug, PartialEq, Eq)]
struct NodeIssue {
    fatal: bool,
    msg: String,
}

/// Longhorn reason codes that mean a node is missing a hard prerequisite — these
/// never resolve without operator action, so they are safe to fail fast on.
fn reason_is_missing_prereq(reason: &str) -> bool {
    reason.eq_ignore_ascii_case("MissingDependency")
}

// --- Structured missing-node-package surfacing (`#15`, ADR-043) --------------

/// Per-distro one-line install commands for a missing node package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallCommands {
    pub debian: &'static str,
    pub ubuntu: &'static str,
    pub arch: &'static str,
}

/// One node package Longhorn reports missing (`MissingDependency`), with the
/// raw condition message and, when the message is recognised, the package name
/// plus copy-pasteable install commands. `package: None` means "unmapped —
/// show the raw reason verbatim".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingPackage {
    pub node: String,
    pub package: Option<&'static str>,
    pub reason: String,
    pub install: Option<InstallCommands>,
}

/// Extensible mapping from Longhorn condition-message substrings (matched
/// case-insensitively, first hit wins) to the package that fixes them. Add a
/// row here when Longhorn grows a new node prerequisite.
struct PkgRule {
    needles: &'static [&'static str],
    package: &'static str,
    install: InstallCommands,
}
const PKG_RULES: &[PkgRule] = &[
    PkgRule {
        needles: &["iscsiadm", "iscsi"],
        package: "open-iscsi",
        install: InstallCommands {
            debian: "sudo apt-get install -y open-iscsi && sudo systemctl enable --now iscsid",
            ubuntu: "sudo apt-get install -y open-iscsi && sudo systemctl enable --now iscsid",
            arch: "sudo pacman -S --needed open-iscsi && sudo systemctl enable --now iscsid.socket",
        },
    },
    PkgRule {
        needles: &["nfs"],
        package: "nfs-common",
        install: InstallCommands {
            debian: "sudo apt-get install -y nfs-common",
            ubuntu: "sudo apt-get install -y nfs-common",
            arch: "sudo pacman -S --needed nfs-utils",
        },
    },
    PkgRule {
        needles: &["dmsetup", "device-mapper"],
        package: "dmsetup",
        install: InstallCommands {
            debian: "sudo apt-get install -y dmsetup",
            ubuntu: "sudo apt-get install -y dmsetup",
            arch: "sudo pacman -S --needed device-mapper",
        },
    },
];

/// Map one Longhorn `MissingDependency` message to a structured missing
/// package. Unmapped messages keep node + raw reason with `package: None` so
/// the frontend can still show them verbatim. Pure — unit tested.
fn map_missing_package(node: &str, message: &str) -> MissingPackage {
    let lower = message.to_ascii_lowercase();
    let rule = PKG_RULES
        .iter()
        .find(|r| r.needles.iter().any(|n| lower.contains(n)));
    MissingPackage {
        node: node.to_string(),
        package: rule.map(|r| r.package),
        reason: message.to_string(),
        install: rule.map(|r| r.install.clone()),
    }
}

/// Extract the missing-package report from one `nodes.longhorn.io` status, if
/// its Ready condition names a `MissingDependency`. Pure — unit tested.
fn missing_package_from_status(name: &str, data: &serde_json::Value) -> Option<MissingPackage> {
    let conds = data.pointer("/status/conditions")?.as_array()?;
    conds.iter().find_map(|c| {
        let typ = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let reason = c.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        if typ.eq_ignore_ascii_case("Ready")
            && status.eq_ignore_ascii_case("False")
            && reason_is_missing_prereq(reason)
        {
            let msg = c.get("message").and_then(|v| v.as_str()).unwrap_or("");
            Some(map_missing_package(name, msg))
        } else {
            None
        }
    })
}

/// Every node currently blocked on a missing package, per the per-node
/// `nodes.longhorn.io` CRs. Empty when all nodes are fine or the CRs aren't
/// readable yet (pre-install). Feeds `storage_status().missing_packages`.
async fn longhorn_missing_packages(client: &Client) -> Vec<MissingPackage> {
    let gvk = GroupVersionKind::gvk("longhorn.io", "v1beta2", "Node");
    let ar = ApiResource::from_gvk(&gvk);
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), LONGHORN_NS, &ar);
    let Ok(list) = api.list(&ListParams::default()).await else {
        return Vec::new();
    };
    list.items
        .iter()
        .filter_map(|n| {
            let name = n.metadata.name.as_deref().unwrap_or("<unknown>");
            missing_package_from_status(name, &n.data)
        })
        .collect()
}

/// Inspect one `nodes.longhorn.io` object's `.status` and decide whether the
/// node can host Longhorn volumes. Pure (no I/O) so the condition logic is unit
/// tested without a cluster. Returns the first blocking issue found, if any.
fn node_issue_from_status(name: &str, data: &serde_json::Value) -> Option<NodeIssue> {
    // Node-level Ready=False — `reason`/`message` name the cause. The classic
    // prereq gap is `MissingDependency` ("iscsiadm not found", i.e. no
    // open-iscsi), which is fatal: it never clears without installing the pkg.
    if let Some(conds) = data
        .pointer("/status/conditions")
        .and_then(|c| c.as_array())
    {
        for c in conds {
            let typ = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if typ.eq_ignore_ascii_case("Ready") && status.eq_ignore_ascii_case("False") {
                let reason = c
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("NotReady");
                let detail = c.get("message").and_then(|v| v.as_str()).unwrap_or("");
                return Some(NodeIssue {
                    fatal: reason_is_missing_prereq(reason),
                    msg: format!("node '{name}' is not Ready ({reason}): {detail}"),
                });
            }
        }
    }
    // No schedulable disk — every disk's `Schedulable` condition is False (e.g.
    // reason `DiskPressure`/`DiskNotReady`), or there is no disk at all. Not
    // fatal-fast (a fresh disk can take a moment to register), but it's the
    // honest explanation if the wait times out: PVCs would stay Pending.
    if let Some(disks) = data
        .pointer("/status/diskStatus")
        .and_then(|d| d.as_object())
    {
        let mut any_schedulable = false;
        let mut last_unschedulable: Option<String> = None;
        for (disk, st) in disks {
            if let Some(conds) = st.get("conditions").and_then(|c| c.as_array()) {
                for c in conds {
                    if c.get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .eq_ignore_ascii_case("Schedulable")
                    {
                        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
                        if status.eq_ignore_ascii_case("True") {
                            any_schedulable = true;
                        } else {
                            let reason = c.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                            let detail = c.get("message").and_then(|v| v.as_str()).unwrap_or("");
                            last_unschedulable = Some(format!(
                                "disk '{disk}' on node '{name}' is not schedulable ({reason}): {detail}"
                            ));
                        }
                    }
                }
            }
        }
        if !any_schedulable {
            return Some(NodeIssue {
                fatal: false,
                msg: last_unschedulable.unwrap_or_else(|| {
                    format!("node '{name}' has no schedulable disk for Longhorn")
                }),
            });
        }
    }
    None
}

/// Probe the per-node `nodes.longhorn.io` CRs (created once longhorn-manager is
/// up) and return the first node that can't host Longhorn volumes (`#15`).
/// Returns `None` when every node is healthy or the CR isn't readable yet.
async fn longhorn_node_issue(client: &Client) -> Option<NodeIssue> {
    let gvk = GroupVersionKind::gvk("longhorn.io", "v1beta2", "Node");
    let ar = ApiResource::from_gvk(&gvk);
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), LONGHORN_NS, &ar);
    let list = api.list(&ListParams::default()).await.ok()?;
    list.items.iter().find_map(|n| {
        let name = n.metadata.name.as_deref().unwrap_or("<unknown>");
        node_issue_from_status(name, &n.data)
    })
}

/// Wait until Longhorn's `longhorn` SC exists (the driver-deployer creates it
/// once the CSI registers, not the bundle directly). `#15`: fail fast and
/// informatively the moment a node reports a hard prerequisite gap (no
/// open-iscsi), and if the wait does time out, name the node/disk cause instead
/// of a bare "timed out" so the operator knows what to fix.
async fn wait_longhorn_storage_ready(client: &Client, sc: &str, secs: u64) -> Result<()> {
    use k8s_openapi::api::storage::v1::StorageClass;
    let api: Api<StorageClass> = Api::all(client.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if let Some(issue) = longhorn_node_issue(client).await {
            if issue.fatal {
                bail!(
                    "cluster creation refused — Longhorn (the only supported deployment \
                     storage, ADR-043) cannot run on this cluster: {}. Install the missing \
                     node packages (shown in the create screen with per-distro commands) \
                     plus a usable disk on every node, then retry.",
                    issue.msg
                );
            }
        }
        if matches!(api.get_opt(sc).await, Ok(Some(_))) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    // Timed out — explain why, if a node tells us, rather than a bare timeout.
    if let Some(issue) = longhorn_node_issue(client).await {
        bail!(
            "cluster creation refused — Longhorn (the only supported deployment storage, \
             ADR-043) did not become ready within {secs}s: {}. The cluster's nodes can't \
             host Longhorn volumes (missing packages / a schedulable disk) — fix that, \
             then retry.",
            issue.msg
        );
    }
    bail!("timed out waiting for StorageClass {sc} to be created (Longhorn never became ready)")
}

/// Clear the default-class annotation from any *node-local* StorageClass still
/// flagged default, so the freshly-installed `longhorn` SC is the cluster's
/// sole default (two defaults make PVC binding ambiguous). Best-effort.
async fn demote_node_local_defaults(client: &Client) -> Result<()> {
    use k8s_openapi::api::storage::v1::StorageClass;
    let api: Api<StorageClass> = Api::all(client.clone());
    let list = api
        .list(&ListParams::default())
        .await
        .context("listing storageclasses")?;
    for sc in &list.items {
        let name = match sc.metadata.name.as_deref() {
            Some(n) if n != LONGHORN_SC => n,
            _ => continue,
        };
        if sc_is_default(sc) && provisioner_is_node_local(&sc.provisioner) {
            let patch = serde_json::json!({
                "metadata": { "annotations": {
                    "storageclass.kubernetes.io/is-default-class": "false",
                    "storageclass.beta.kubernetes.io/is-default-class": "false",
                }}
            });
            api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
                .await
                .with_context(|| format!("demoting default StorageClass {name}"))?;
            tracing::info!(
                "demoted node-local default StorageClass {name} in favour of {LONGHORN_SC} (ADR-031)"
            );
        }
    }
    Ok(())
}

/// Install Longhorn and wait until its StorageClass is ready to back PVCs
/// (ADR-031/043). The callable storage step `ensure()` invokes and `#14`
/// drives from its R3 remediation. Server-side apply makes it safe to re-run.
/// Callers gate on [`classify_storage`] — this assumes Longhorn is not already
/// present and requires cluster-admin (run before `revoke_bootstrap`).
pub async fn install_longhorn(client: &Client) -> Result<()> {
    job_set(Job::Running("longhorn".into()));
    apply_bundle(client, LONGHORN_BUNDLE, BOOTSTRAP_BINDING)
        .await
        .context("installing longhorn")?;
    // The manager DaemonSet + driver-deployer must come up before the CSI
    // driver registers and creates the `longhorn` StorageClass.
    wait_daemonset(client, LONGHORN_NS, LONGHORN_MANAGER_DS, 600).await?;
    wait_deploy(client, LONGHORN_NS, LONGHORN_DRIVER_DEPLOYER, 600).await?;
    // The driver-deployer creates the `longhorn` SC (annotated default in the
    // bundle); wait for it — but fail informatively the moment a node proves it
    // can't run Longhorn (`#15`: missing open-iscsi, no schedulable disk) rather
    // than hanging until timeout and leaving PVCs Pending forever. Then make
    // `longhorn` the sole default by demoting any node-local SC still flagged.
    wait_longhorn_storage_ready(client, LONGHORN_SC, 300).await?;
    demote_node_local_defaults(client).await?;
    // Size replicas to the cluster that just got Longhorn (#26): a sub-3-node
    // cluster cannot place the bundle's default of three copies per volume.
    reconcile_longhorn_sizing(client).await?;
    // Gate: the `longhorn` SC must now be in place and ready (ADR-043).
    match classify_storage(client).await? {
        DeploymentStorage::Longhorn { .. } => Ok(()),
        other => bail!("longhorn installed but its StorageClass is still not usable: {other:?}"),
    }
}

/// Replicas a cluster of `nodes` schedulable nodes can actually place: the
/// bundle default while it fits, otherwise one per node (#26). One copy on
/// one node is exactly the durability of the node-local storage the cluster
/// arrived with — the R3 remediation's starting point, not a silent downgrade
/// from a working state: with the bundle default, such a cluster has NO
/// working volumes at all, only faulted ones.
pub(crate) fn replicas_for_nodes(nodes: usize) -> u32 {
    (nodes.max(1) as u32).min(LONGHORN_DEFAULT_REPLICAS)
}

/// Schedulable node count — the same population R4 sums over (unschedulable
/// nodes excluded; `node_info` presence proxies a reported node).
async fn schedulable_node_count(client: &Client) -> Result<usize> {
    use k8s_openapi::api::core::v1::Node;
    let api: Api<Node> = Api::all(client.clone());
    let list = api
        .list(&ListParams::default())
        .await
        .context("listing nodes for storage sizing")?;
    Ok(list
        .items
        .iter()
        .filter(|n| {
            let schedulable = !n
                .spec
                .as_ref()
                .and_then(|s| s.unschedulable)
                .unwrap_or(false);
            schedulable
                && n.status
                    .as_ref()
                    .and_then(|st| st.node_info.as_ref())
                    .is_some()
        })
        .count())
}

/// The two Longhorn settings a sub-default cluster needs (#26): the default
/// replica count itself, and soft anti-affinity so a transiently cordoned
/// node degrades replication instead of faulting the volume outright.
fn longhorn_sizing_settings(replicas: u32) -> String {
    format!(
        r#"apiVersion: longhorn.io/v1beta2
kind: Setting
metadata:
  name: default-replica-count
  namespace: {ns}
spec:
  value: "{replicas}"
---
apiVersion: longhorn.io/v1beta2
kind: Setting
metadata:
  name: replica-soft-anti-affinity
  namespace: {ns}
spec:
  value: "true"
"#,
        ns = LONGHORN_NS,
        replicas = replicas
    )
}

/// Bring Longhorn's replica sizing in line with the cluster it must live on
/// (#26). The count reaches volumes three ways, so the reconcile touches all
/// three: the `longhorn` StorageClass parameter (what our PVCs inherit), the
/// `default-replica-count` setting (volumes claimed outside our SC), and
/// already-created volumes stuck above the schedulable count — the upgrade
/// path for clusters that faulted before this reconcile existed. Clusters
/// that fit the bundle default are left untouched, which is every ≥3-node
/// deployment's steady state.
pub(crate) async fn reconcile_longhorn_sizing(client: &Client) -> Result<()> {
    let replicas = replicas_for_nodes(schedulable_node_count(client).await?);
    if replicas == LONGHORN_DEFAULT_REPLICAS {
        return Ok(()); // the cluster places the bundle default — nothing to size
    }

    // 1. The StorageClass parameter — the path our own PVCs inherit. SC
    //    `parameters` are IMMUTABLE in Kubernetes, so the class is replaced
    //    wholesale: read the current one, swap the replica count, delete,
    //    re-apply. Bound PVCs are unaffected (their Longhorn volumes carry
    //    their own counts, healed below) and the brief absent window only
    //    delays brand-new claims momentarily — the create gate runs before
    //    this deployment's PVCs exist.
    use k8s_openapi::api::storage::v1::StorageClass;
    let sc: Api<StorageClass> = Api::all(client.clone());
    let current = sc
        .get(LONGHORN_SC)
        .await
        .context("reading the longhorn StorageClass to resize")?;
    let mut parameters = current.parameters.clone().unwrap_or_default();
    parameters.insert("numberOfReplicas".into(), replicas.to_string());
    let replacement = serde_json::json!({
        "apiVersion": "storage.k8s.io/v1",
        "kind": "StorageClass",
        "metadata": {
            "name": LONGHORN_SC,
            "annotations": current.metadata.annotations.clone().unwrap_or_default(),
        },
        "provisioner": current.provisioner,
        "reclaimPolicy": current.reclaim_policy,
        "volumeBindingMode": current.volume_binding_mode,
        "allowVolumeExpansion": current.allow_volume_expansion,
        "parameters": parameters,
    });
    let _ = sc.delete(LONGHORN_SC, &DeleteParams::default()).await;
    // Deletion is asynchronous: the object lingers with a deletionTimestamp
    // until the registry drops it, and an apply that lands in that window is
    // refused as an immutable-parameter change against the OLD object. Wait
    // it out (bounded — a plain SC carries no finalizers).
    for _ in 0..30 {
        if sc.get(LONGHORN_SC).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    apply_bundle(
        client,
        &serde_yaml::to_string(&replacement).context("serializing the resized StorageClass")?,
        BOOTSTRAP_BINDING,
    )
    .await
    .context("re-applying the longhorn StorageClass at the schedulable replica count")?;

    // 2. The default setting — volumes claimed outside our StorageClass.
    apply_bundle(
        client,
        &longhorn_sizing_settings(replicas),
        BOOTSTRAP_BINDING,
    )
    .await
    .context("sizing longhorn's default replica setting")?;

    // 3. Volumes already asking for more than the cluster can place.
    heal_overscheduled_volumes(client, replicas).await?;
    Ok(())
}

/// Patch Longhorn volumes asking for more replicas than the cluster can
/// schedule down to `replicas` (#26). Only volumes ABOVE the schedulable
/// count are touched — a volume at or below it is placeable as asked.
async fn heal_overscheduled_volumes(client: &Client, replicas: u32) -> Result<()> {
    let gvk = GroupVersionKind::gvk("longhorn.io", "v1beta2", "Volume");
    let ar = ApiResource::from_gvk(&gvk);
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), LONGHORN_NS, &ar);
    let list = api
        .list(&ListParams::default())
        .await
        .context("listing longhorn volumes to heal")?;
    let patch = Patch::Merge(serde_json::json!({
        "spec": { "numberOfReplicas": replicas }
    }));
    for v in &list.items {
        let asked = v
            .data
            .get("spec")
            .and_then(|s| s.get("numberOfReplicas"))
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        if asked > replicas as u64 {
            let name = v.metadata.name.as_deref().unwrap_or("<unknown>");
            api.patch(name, &PatchParams::default(), &patch)
                .await
                .with_context(|| format!("healing longhorn volume {name}"))?;
        }
    }
    Ok(())
}

/// Kind ordering inside a bundle: namespaces and CRDs must land before
/// anything that lives in / is typed by them.
fn kind_priority(kind: &str) -> u8 {
    match kind {
        "Namespace" => 0,
        "CustomResourceDefinition" => 1,
        _ => 2,
    }
}

/// Apply a multi-doc YAML bundle with server-side apply, in rounds: docs that
/// fail (kind not yet discoverable, webhook still starting…) are retried with
/// a fresh API discovery until none remain or rounds are exhausted.
pub async fn apply_bundle(client: &Client, bundle: &str, field_manager: &str) -> Result<()> {
    let mut docs: Vec<serde_json::Value> = Vec::new();
    for de in serde_yaml::Deserializer::from_str(bundle) {
        let v: serde_json::Value =
            serde::Deserialize::deserialize(de).context("parsing bundle YAML")?;
        if v.is_object() {
            docs.push(v);
        }
    }
    docs.sort_by_key(|d| kind_priority(d.get("kind").and_then(|k| k.as_str()).unwrap_or("")));

    let mut last_err: Option<anyhow::Error> = None;
    for round in 0..10u32 {
        if docs.is_empty() {
            return Ok(());
        }
        if round > 0 {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let discovery = Discovery::new(client.clone())
            .run()
            .await
            .context("API discovery")?;
        let mut remaining = Vec::new();
        for doc in docs {
            match apply_doc(client, &discovery, doc.clone(), field_manager).await {
                Ok(()) => {}
                Err(e) => {
                    last_err = Some(e);
                    remaining.push(doc);
                }
            }
        }
        docs = remaining;
    }
    let n = docs.len();
    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("bundle apply did not converge"))
        .context(format!("{n} docs still failing after retries")))
}

/// Server-side apply one manifest object, resolving its API via discovery.
pub async fn apply_doc(
    client: &Client,
    discovery: &Discovery,
    doc: serde_json::Value,
    field_manager: &str,
) -> Result<()> {
    let api_version = doc
        .get("apiVersion")
        .and_then(|v| v.as_str())
        .context("doc missing apiVersion")?;
    let kind = doc
        .get("kind")
        .and_then(|v| v.as_str())
        .context("doc missing kind")?;
    let name = doc
        .pointer("/metadata/name")
        .and_then(|v| v.as_str())
        .context("doc missing metadata.name")?
        .to_string();
    let (group, version) = match api_version.split_once('/') {
        Some((g, v)) => (g, v),
        None => ("", api_version), // core group, e.g. "v1"
    };
    let gvk = GroupVersionKind::gvk(group, version, kind);
    let (ar, caps) = discovery
        .resolve_gvk(&gvk)
        .with_context(|| format!("kind {kind} ({api_version}) not known to the API server yet"))?;
    let api: Api<DynamicObject> = if caps.scope == Scope::Cluster {
        Api::all_with(client.clone(), &ar)
    } else {
        let target_ns = doc
            .pointer("/metadata/namespace")
            .and_then(|v| v.as_str())
            .unwrap_or(ns());
        Api::namespaced_with(client.clone(), target_ns, &ar)
    };
    api.patch(
        &name,
        &PatchParams::apply(field_manager).force(),
        &Patch::Apply(&doc),
    )
    .await
    .with_context(|| format!("applying {kind}/{name}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- Longhorn replica sizing (#26) ---------------------------------------

    #[test]
    fn replica_sizing_never_exceeds_the_bundle_default() {
        assert_eq!(replicas_for_nodes(0), 1); // degenerate cluster: at least one
        assert_eq!(replicas_for_nodes(1), 1); // the single-node greenfield shape
        assert_eq!(replicas_for_nodes(2), 2);
        assert_eq!(replicas_for_nodes(3), 3); // the bundle default, untouched
        assert_eq!(replicas_for_nodes(64), 3); // big clusters keep the default
    }

    #[test]
    fn sizing_settings_target_both_longhorn_knobs() {
        let yaml = longhorn_sizing_settings(1);
        assert!(yaml.contains("name: default-replica-count"));
        assert!(yaml.contains("namespace: longhorn-system"));
        assert!(yaml.contains(r#"value: "1""#));
        assert!(yaml.contains("name: replica-soft-anti-affinity"));
        assert!(yaml.contains(r#"value: "true""#));
    }

    // --- R7 operator guard (`#115`) -----------------------------------------

    /// The `OpenSearchCluster` CRD names the operator chart REALLY installs,
    /// copied verbatim out of `deploy/bootstrap/operator.yaml` (chart
    /// opensearch-operator-3.0.2) — the same name sidecar waits on in
    /// `ansible/roles/opensearch_stack/tasks/main.yml`.
    const REAL_OPERATOR_CRDS: &[&str] = &[
        "opensearchclusters.opensearch.org",
        "opensearchclusters.opensearch.opster.io",
    ];
    /// What the pre-`#115` guard tested. No chart has ever installed it, which
    /// is why the guard was silently always-false.
    const PRE_115_CRD_NAME: &str = "opensearchclusters.opster.io";

    const OWN_NS: &str = "veloxsearch-system";

    fn crds(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    fn op_deploy(namespace: &str, name: &str, ready: bool) -> OperatorDeploy {
        OperatorDeploy {
            namespace: namespace.into(),
            name: name.into(),
            ready,
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// The regression: the predicate must match the names the chart installs.
    /// Fails against the pre-`#115` predicate, which matched none of them.
    #[test]
    fn operator_crds_match_the_names_the_chart_really_installs() {
        for name in REAL_OPERATOR_CRDS {
            assert_eq!(
                classify_operator(&crds(&[name]), &[], OWN_NS),
                OperatorPresence::UnmanagedCrds,
                "{name} is shipped by the operator chart and must be recognised"
            );
        }
        // Both group variants are covered, not just the current one.
        assert!(OPERATOR_CRDS.contains(&"opensearchclusters.opensearch.org"));
        assert!(OPERATOR_CRDS.contains(&"opensearchclusters.opensearch.opster.io"));
        // And the name the old guard tested is not one of them.
        assert!(!OPERATOR_CRDS.contains(&PRE_115_CRD_NAME));
        assert_eq!(
            classify_operator(&crds(&[PRE_115_CRD_NAME]), &[], OWN_NS),
            OperatorPresence::Absent
        );
    }

    /// The sidecar shape (sidecar!7): helm release `opensearch-operator` in
    /// namespace `opensearch`, real CRDs registered. Must be a hard refusal.
    #[test]
    fn a_foreign_operator_is_detected_cluster_wide_and_refuses() {
        let presence = classify_operator(
            &crds(REAL_OPERATOR_CRDS),
            &[op_deploy("opensearch", "opensearch-operator", true)],
            OWN_NS,
        );
        assert_eq!(
            presence,
            OperatorPresence::Foreign {
                namespace: "opensearch".into(),
                name: "opensearch-operator".into(),
                ready: true,
            },
            "an operator outside our namespace must not be invisible"
        );

        let row = operator_r7(&presence, false).expect("a foreign operator owes an R7 row");
        assert_eq!(
            row.status, "fail",
            "silence is the worst property a guard can have"
        );
        assert!(
            row.detail.contains("opensearch/opensearch-operator"),
            "{}",
            row.detail
        );
        assert!(
            row.detail.contains(ALLOW_FOREIGN_OPERATOR_ENV),
            "the refusal must name its escape hatch: {}",
            row.detail
        );

        // ...and it must never read as "in conformity, ready to deploy", even
        // though the foreign operator itself is serving.
        let (installed, ready) = operator_flags(&presence);
        assert!(
            installed && ready,
            "the foreign operator really is installed and serving"
        );
        assert!(
            !bootstrap_ready(true, ready, &presence, false),
            "dual-operator must be unreachable"
        );
        assert!(
            bootstrap_ready(true, ready, &presence, true),
            "the override adopts it"
        );
    }

    /// Only the explicit override downgrades the refusal.
    #[test]
    fn the_override_is_the_only_way_past_a_foreign_operator() {
        let presence = classify_operator(
            &[],
            &[op_deploy("opensearch", "opensearch-operator", true)],
            OWN_NS,
        );
        let row = operator_r7(&presence, true).expect("still reported, just not fatal");
        assert_eq!(row.status, "warn");
        assert!(row.detail.contains("adopted"), "{}", row.detail);
    }

    /// The gate at the destructive act must not depend on the foreign operator's
    /// READINESS. A serving foreign operator reports `operator_ready == true`, so
    /// a guard nested inside `if !operator_ready` is skipped in exactly the case
    /// `#115` is about — the sidecar cluster, where the operator is up and
    /// working. `run_install` therefore calls this predicate unconditionally.
    #[test]
    fn the_install_gate_ignores_the_foreign_operators_readiness() {
        for ready in [true, false] {
            let presence = classify_operator(
                &crds(REAL_OPERATOR_CRDS),
                &[op_deploy("opensearch", "opensearch-operator", ready)],
                OWN_NS,
            );
            let why = foreign_operator_block(&presence, false)
                .unwrap_or_else(|| panic!("a foreign operator (ready={ready}) must block"));
            assert!(why.contains("opensearch/opensearch-operator"), "{why}");
            assert!(why.contains(ALLOW_FOREIGN_OPERATOR_ENV), "{why}");
            // ...and it is the SAME string the R7 row shows, so the screen and the
            // installer cannot drift apart.
            let row = operator_r7(&presence, false).expect("an R7 row");
            assert_eq!(row.status, "fail");
            assert_eq!(row.detail, why);
            // The override is the only thing that clears it.
            assert!(foreign_operator_block(&presence, true).is_none());
        }
    }

    /// Nothing except a foreign operator blocks the install: an interrupted
    /// install of ours (orphan CRDs) and a clean cluster must both proceed, or
    /// the guard wedges the retry it was meant to protect.
    #[test]
    fn only_a_foreign_operator_blocks_the_install() {
        for presence in [
            OperatorPresence::Absent,
            OperatorPresence::UnmanagedCrds,
            OperatorPresence::Ours { ready: false },
            OperatorPresence::Ours { ready: true },
        ] {
            assert!(
                foreign_operator_block(&presence, false).is_none(),
                "{presence:?} is not a dual-operator conflict"
            );
        }
    }

    /// Our own operator is not a conflict, and leaves R7 to the cert-manager half.
    #[test]
    fn our_own_operator_is_not_foreign() {
        let presence = classify_operator(
            &crds(REAL_OPERATOR_CRDS),
            &[op_deploy(OWN_NS, OPERATOR_DEPLOY, true)],
            OWN_NS,
        );
        assert_eq!(presence, OperatorPresence::Ours { ready: true });
        assert!(
            operator_r7(&presence, false).is_none(),
            "no operator row — cert-manager decides"
        );
        assert_eq!(operator_flags(&presence), (true, true));
        assert!(bootstrap_ready(true, true, &presence, false));

        // Not yet serving: installed, but not ready.
        let starting = classify_operator(&[], &[op_deploy(OWN_NS, OPERATOR_DEPLOY, false)], OWN_NS);
        assert_eq!(operator_flags(&starting), (true, false));
        assert!(!bootstrap_ready(true, false, &starting, false));
    }

    /// A clean cluster stays clean, and orphan CRDs warn rather than wedge the
    /// retry of our own interrupted install.
    #[test]
    fn clean_cluster_passes_and_orphan_crds_only_warn() {
        let clean = classify_operator(&crds(&["certificates.cert-manager.io"]), &[], OWN_NS);
        assert_eq!(clean, OperatorPresence::Absent);
        assert!(operator_r7(&clean, false).is_none());
        assert_eq!(operator_flags(&clean), (false, false));

        let orphan = classify_operator(&crds(REAL_OPERATOR_CRDS), &[], OWN_NS);
        let row = operator_r7(&orphan, false).expect("an orphan-CRD row");
        assert_eq!(
            row.status, "warn",
            "an interrupted install of OURS looks like this"
        );
        assert_eq!(operator_flags(&orphan), (true, false));
    }

    /// A probe that could not run is reported, never assumed clean — assuming
    /// clean is exactly the `#115` failure mode.
    #[test]
    fn an_unprovable_probe_is_reported_not_assumed_clean() {
        let unknown = OperatorPresence::Unknown("deployments is forbidden".into());
        let row = operator_r7(&unknown, false).expect("an unknown-probe row");
        assert_eq!(row.status, "warn");
        assert!(row.detail.contains("forbidden"), "{}", row.detail);
        assert_eq!(operator_flags(&unknown), (false, false));
    }

    /// Operator Deployments are recognised by chart label first (any helm
    /// release name) and by deployment name second (kustomize/hand-rolled).
    #[test]
    fn operator_deployments_are_recognised_by_label_or_name() {
        let chart = labels(&[
            ("app.kubernetes.io/name", "opensearch-operator"),
            ("app.kubernetes.io/instance", "os-op-prod"),
        ]);
        assert!(
            deploy_is_operator("os-op-prod", Some(&chart)),
            "renamed helm release"
        );
        assert!(
            deploy_is_operator("opensearch-operator-controller-manager", None),
            "by name"
        );
        assert!(
            !deploy_is_operator("opensearch-dashboards", None),
            "dashboards is not an operator"
        );
        assert!(!deploy_is_operator(
            "cert-manager",
            Some(&labels(&[("app.kubernetes.io/name", "cert-manager")]))
        ));
    }

    /// Several foreign operators: the message must name the same one every poll.
    #[test]
    fn the_named_foreign_operator_is_stable_across_polls() {
        let deploys = [
            op_deploy("zzz-ops", "opensearch-operator", true),
            op_deploy("opensearch", "opensearch-operator", true),
        ];
        let forward = classify_operator(&[], &deploys, OWN_NS);
        let reversed: Vec<OperatorDeploy> = deploys.iter().rev().cloned().collect();
        assert_eq!(forward, classify_operator(&[], &reversed, OWN_NS));
        assert!(
            matches!(forward, OperatorPresence::Foreign { ref namespace, .. } if namespace == "opensearch")
        );
    }

    #[test]
    fn node_local_provisioners_are_classified() {
        assert!(provisioner_is_node_local("rancher.io/local-path")); // k3s default
        assert!(provisioner_is_node_local("kubernetes.io/no-provisioner"));
        assert!(provisioner_is_node_local("openebs.io/local"));
        assert!(provisioner_is_node_local("k8s.io/minikube-hostpath")); // any "hostpath"
        assert!(!provisioner_is_node_local("driver.longhorn.io")); // prod's real default
        assert!(!provisioner_is_node_local("ebs.csi.aws.com"));
    }

    /// Build a StorageClass fixture for the pure classifier.
    fn sc(
        name: &str,
        provisioner: &str,
        default: bool,
    ) -> k8s_openapi::api::storage::v1::StorageClass {
        let mut s = k8s_openapi::api::storage::v1::StorageClass::default();
        s.metadata.name = Some(name.to_string());
        s.provisioner = provisioner.to_string();
        if default {
            s.metadata.annotations = Some(
                [(
                    "storageclass.kubernetes.io/is-default-class".to_string(),
                    "true".to_string(),
                )]
                .into(),
            );
        }
        s
    }

    #[test]
    fn only_longhorn_satisfies_the_storage_gate() {
        // ADR-043: Longhorn present → ready, whether default or merely pinned.
        for default in [true, false] {
            let ds = classify_storage_classes(&[sc("longhorn", "driver.longhorn.io", default)]);
            assert_eq!(ds, DeploymentStorage::Longhorn { default });
            assert!(ds.longhorn_ready() && !ds.needs_longhorn());
        }
        // A foreign real CSI default (EBS gp2) no longer satisfies the gate.
        let ds = classify_storage_classes(&[sc("gp2", "ebs.csi.aws.com", true)]);
        assert_eq!(ds, DeploymentStorage::ForeignDefault("gp2".into()));
        // An SC merely *named* longhorn with a foreign provisioner doesn't count.
        let ds = classify_storage_classes(&[sc("longhorn", "ebs.csi.aws.com", true)]);
        assert_eq!(ds, DeploymentStorage::ForeignDefault("longhorn".into()));
        // Node-local default / nothing at all — same remediation as before.
        let ds = classify_storage_classes(&[sc("local-path", "rancher.io/local-path", true)]);
        assert_eq!(ds, DeploymentStorage::NodeLocal("local-path".into()));
        assert_eq!(classify_storage_classes(&[]), DeploymentStorage::Absent);
        // Everything that isn't Longhorn triggers the install.
        for ds in [
            DeploymentStorage::ForeignDefault("gp2".into()),
            DeploymentStorage::NodeLocal("local-path".into()),
            DeploymentStorage::Absent,
        ] {
            assert!(!ds.longhorn_ready(), "{ds:?} must not read as ready");
            assert!(
                ds.needs_longhorn(),
                "{ds:?} must trigger the Longhorn install"
            );
        }
    }

    #[test]
    fn storage_status_describes_each_classification() {
        // Longhorn ready → durable, no install, named.
        let (durable, needs, name, detail) =
            describe_storage(&DeploymentStorage::Longhorn { default: true });
        assert!(durable && !needs);
        assert_eq!(name.as_deref(), Some("longhorn"));
        assert!(detail.contains("Longhorn") && detail.contains("longhorn"));

        // Foreign CSI default → not durable, auto-install (ADR-043).
        let (durable, needs, name, detail) =
            describe_storage(&DeploymentStorage::ForeignDefault("gp2".into()));
        assert!(!durable && needs);
        assert_eq!(name.as_deref(), Some("gp2"));
        assert!(detail.contains("foreign") && detail.contains("gp2"));

        // Node-local default → not durable, auto-install, named, "node-local".
        let (durable, needs, name, detail) =
            describe_storage(&DeploymentStorage::NodeLocal("local-path".into()));
        assert!(!durable && needs);
        assert_eq!(name.as_deref(), Some("local-path"));
        assert!(detail.contains("node-local") && detail.contains("local-path"));

        // Absent default → not durable, auto-install, no name.
        let (durable, needs, name, detail) = describe_storage(&DeploymentStorage::Absent);
        assert!(!durable && needs);
        assert!(name.is_none());
        assert!(detail.contains("no default"));
    }

    #[test]
    fn missing_package_table_maps_known_messages() {
        // iscsi → open-iscsi, with per-distro commands (enable the daemon too).
        let m = map_missing_package("vm1", "iscsiadm not found");
        assert_eq!(m.node, "vm1");
        assert_eq!(m.package, Some("open-iscsi"));
        assert_eq!(m.reason, "iscsiadm not found");
        let inst = m.install.expect("install commands");
        assert!(
            inst.debian.contains("apt-get install -y open-iscsi") && inst.debian.contains("iscsid")
        );
        assert_eq!(inst.debian, inst.ubuntu);
        assert!(
            inst.arch.contains("pacman -S --needed open-iscsi")
                && inst.arch.contains("iscsid.socket")
        );

        // nfs → nfs-common on debian/ubuntu, nfs-utils on arch.
        let m = map_missing_package("vm1", "NFS client utilities are not found");
        assert_eq!(m.package, Some("nfs-common"));
        let inst = m.install.expect("install commands");
        assert!(inst.debian.contains("nfs-common"));
        assert!(inst.arch.contains("nfs-utils"));

        // dmsetup / device-mapper → dmsetup.
        for msg in ["dmsetup not found", "device-mapper missing"] {
            let m = map_missing_package("vm1", msg);
            assert_eq!(m.package, Some("dmsetup"), "message {msg:?}");
            assert!(m
                .install
                .expect("install commands")
                .arch
                .contains("device-mapper"));
        }

        // Unknown message → package null, raw node + reason kept verbatim.
        let m = map_missing_package("vm9", "some future dependency is missing");
        assert_eq!(m.package, None);
        assert!(m.install.is_none());
        assert_eq!(m.node, "vm9");
        assert_eq!(m.reason, "some future dependency is missing");
    }

    #[test]
    fn missing_package_extracted_only_from_missing_dependency_conditions() {
        // MissingDependency → structured report.
        let status = json!({
            "status": { "conditions": [
                { "type": "Ready", "status": "False", "reason": "MissingDependency",
                  "message": "iscsiadm not found" }
            ]}
        });
        let m = missing_package_from_status("vm1", &status).expect("a report");
        assert_eq!((m.node.as_str(), m.package), ("vm1", Some("open-iscsi")));

        // Other not-ready reasons and healthy nodes yield nothing.
        let status = json!({
            "status": { "conditions": [
                { "type": "Ready", "status": "False", "reason": "KernelModulesNotLoaded",
                  "message": "module gone" }
            ]}
        });
        assert_eq!(missing_package_from_status("vm2", &status), None);
        let status =
            json!({ "status": { "conditions": [ { "type": "Ready", "status": "True" } ] } });
        assert_eq!(missing_package_from_status("node-3", &status), None);
    }

    #[test]
    fn missing_open_iscsi_is_a_fatal_prereq() {
        let status = json!({
            "status": { "conditions": [
                { "type": "Ready", "status": "False", "reason": "MissingDependency",
                  "message": "iscsiadm not found" }
            ]}
        });
        let issue = node_issue_from_status("vm1", &status).expect("an issue");
        assert!(issue.fatal, "MissingDependency must fail fast, not hang");
        assert!(issue.msg.contains("vm1") && issue.msg.contains("iscsiadm"));
    }

    #[test]
    fn no_schedulable_disk_is_reported_but_not_fatal() {
        let status = json!({
            "status": { "diskStatus": {
                "default-disk": { "conditions": [
                    { "type": "Schedulable", "status": "False", "reason": "DiskPressure",
                      "message": "no space" }
                ]}
            }}
        });
        let issue = node_issue_from_status("vm2", &status).expect("an issue");
        assert!(!issue.fatal, "a disk gap can clear; don't fail fast on it");
        assert!(issue.msg.contains("vm2") && issue.msg.contains("not schedulable"));
    }

    #[test]
    fn healthy_node_has_no_issue() {
        let status = json!({
            "status": {
                "conditions": [ { "type": "Ready", "status": "True" } ],
                "diskStatus": { "default-disk": { "conditions": [
                    { "type": "Schedulable", "status": "True" }
                ]}}
            }
        });
        assert_eq!(node_issue_from_status("node-3", &status), None);
    }
}
