// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! JSON HTTP API for the React SPA (epic: react-rewrite).
//!
//! One handler per former Leptos `#[server]` fn (see git history of `app.rs`).
//! The path under `/api` is the old `endpoint=` string verbatim, so the
//! frontend contract is unchanged at the URL level — only the transport moves
//! from Leptos server-fn POSTs to plain JSON.
//!
//! Conventions for the React side (Lane B):
//!   * No-argument reads are `GET`; everything else is `POST` with a JSON body
//!     whose field names match the old server-fn parameters.
//!   * Success returns the DTO as JSON (200), or an empty 200 for unit results.
//!   * Errors return `{ "error": "<message>" }` with a 4xx/5xx status —
//!     400 validation, 401 bad credentials, 500 from the K8s/OS layer.
//!   * `login`/`logout`/`setup_admin` set the session cookie directly on the
//!     response (no Leptos `ResponseOptions` context anymore).
//!   * SSE deployment stream stays at `GET /api/events`.

use serde::{Deserialize, Serialize};

// ─────────────────────────────── shared DTOs ───────────────────────────────
// Moved here from `app.rs` so they outlive the Leptos frontend.

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterStatus {
    pub name: String,
    pub phase: String,
    pub health: String,
    pub nodes_ready: i32,
    pub nodes_desired: i32,
    pub size: String,
    pub purpose: String,
    pub monitors: Vec<String>,
    /// Non-empty (the stack version) when the OTel observability stack is
    /// installed on this deployment — ADR-053. Rides the SSE stream, so the
    /// Integrations tab needs no extra request to pick which panel to render.
    /// Deliberately not folded into `monitors`: see `k8s::set_otel_stack`.
    #[serde(default)]
    pub otel_stack: String,
    /// The next-generation UI (workspaces + new navigation) is on. A
    /// deployment-level choice; the observability stack requires it but does
    /// not own it.
    #[serde(default)]
    pub next_ui: bool,
    /// The user asked for the new UI rather than the stack having required it.
    /// The UI uses this to say whether uninstalling the stack would revert it.
    #[serde(default)]
    pub next_ui_chosen: bool,
    /// CIDRs allowed on the public routes. Empty = open (the default).
    #[serde(default)]
    pub ip_allow_list: Vec<String>,
    pub replicas: i64,
    pub heap: String,
    /// Node memory (request = limit, e.g. "4Gi"); the operator derives the
    /// JVM heap as half of this (ADR-035).
    pub memory: String,
    pub disk: String,
    pub extra_config: String,
    pub dashboard_url: Option<String>,
    /// Public address of the OpenSearch API (ingress mode, cluster up).
    #[serde(default)]
    pub opensearch_url: Option<String>,
    /// Portforward-mode alternative to `dashboard_url` (ADR-027).
    pub dashboard_portforward: Option<String>,
    /// Identity provider in effect for this deployment's OpenSearch users
    /// (`internal` when none is configured) — ADR-045.
    #[serde(default)]
    pub auth_kind: String,
    /// OpenSearch version running now (ADR-048).
    #[serde(default)]
    pub version: String,
    /// Version the spec asks for while it differs from `version`; empty when
    /// nothing is in flight.
    #[serde(default)]
    pub target_version: String,
    /// Dashboards version — phase 2 of an upgrade lags the nodes.
    #[serde(default)]
    pub dashboards_version: String,
    /// Live upgrade state, reduced from the CR (never in-memory state, so it
    /// survives a reload and a backend restart).
    #[serde(default)]
    pub upgrade: UpgradeStatus,
    /// Newest upstream version found by the hourly check, when it is a legal
    /// upgrade for this deployment — the "Upgrade v3.8.0" tag (ADR-048 rev. 2).
    #[serde(default)]
    pub suggested_version: String,
    /// CR creation timestamp (RFC 3339); the list renders it as an age.
    #[serde(default)]
    pub created_at: String,
    /// Snapshot repository + scheduled policy (ADR-049). `configured: false`
    /// is the default and a valid state.
    #[serde(default)]
    pub snapshot: SnapshotStatus,
    /// What this deployment is doing and whether it has settled (ADR-050).
    /// The UI hides the provisioning panel and locks controls on this — it
    /// never re-derives the rule from `health`.
    #[serde(default)]
    pub activity: ActivityStatus,
    /// Whether the purpose profile and the selected monitors were actually
    /// applied (ADR-052). Defaults to `complete`, so an older client and a
    /// deployment that never deferred anything both read as fine.
    #[serde(default)]
    pub provisioning: ProvisioningStatus,
}

/// The deferred half of a create/save — did it happen? (ADR-052)
///
/// This exists because it used to be answerable only from the server log. A
/// deployment whose profile and monitors were never applied looked, in the API
/// and therefore in the UI, exactly like one that was fully configured: the CR
/// still carried the `monitors` annotation and the `purpose` label the wizard
/// wrote, `health` was green and `activity` was idle. Two production
/// deployments shipped that way in two days.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProvisioningStatus {
    /// `complete` — nothing outstanding; `pending` — work is still owed and a
    /// retry is scheduled; `failed` — the schedule is spent and nothing further
    /// happens without the user. The UI renders the words; it does not derive
    /// the state.
    pub state: String,
    /// The purpose profile has not been applied.
    pub profile_pending: bool,
    /// Selected monitor ids that are not installed. Ids, so the UI can name
    /// them the same way the wizard did.
    pub monitors_pending: Vec<String>,
    /// Attempts consumed so far.
    pub attempts: u32,
    /// Why the last attempt did not finish — the cluster's own words, rendered
    /// verbatim like `snapshot.last_error` and `upgrade.reason`.
    pub last_error: String,
    /// RFC 3339 timestamp of the last attempt.
    pub updated_at: String,
}

impl Default for ProvisioningStatus {
    /// A deployment we know nothing about is complete, never pending: the
    /// absent-record case is every deployment created before ADR-052, and a
    /// missing field must not flag a healthy estate as broken.
    fn default() -> Self {
        Self {
            state: "complete".into(),
            profile_pending: false,
            monitors_pending: Vec::new(),
            attempts: 0,
            last_error: String::new(),
            updated_at: String::new(),
        }
    }
}

/// Live activity of one deployment (ADR-050). Replaces `health == "green"` as
/// the readiness answer: a rolling restart reports every pod ready and the
/// cluster green while nodes are still being replaced, so "ready" is a
/// composed predicate the server owns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActivityStatus {
    /// `idle` | `creating` | `upgrading` | `restarting`.
    pub kind: String,
    /// Ladder rung: `storage`|`accepted`|`volumes`|`nodes`|`security`|
    /// `dashboards`|`settling`, or `ready` when idle. The UI translates it —
    /// never prose from the server.
    pub stage: String,
    /// Overall progress, monotonic, and never 100 while `settled` is false.
    pub percent: u8,
    /// Sub-progress in words ("2/3"); empty when the stage has none.
    pub detail: String,
    /// The predicate. Everything else here is presentation.
    pub settled: bool,
    /// Whether this deployment's mutating controls must refuse.
    pub locks_edits: bool,
    /// The ONE node pair every counter on the screen divides (issue #131):
    /// ready, clamped, over what the CR asks for.
    #[serde(default)]
    pub nodes_ready: i32,
    #[serde(default)]
    pub nodes_total: i32,
    /// Seconds since the node pool last moved, measured from the cluster. The
    /// panel's clock renders this instead of counting from its own mount, which
    /// is why switching tabs no longer resets it to zero.
    #[serde(default)]
    pub since_secs: i64,
    /// Nothing has advanced for `activity::STALL_AFTER_SECS`. The server owns
    /// the threshold so the SPA keeps rendering verdicts rather than deriving
    /// them (ADR-050 invariant 3).
    #[serde(default)]
    pub stalled: bool,
    /// Primaries up and the cluster answering — true independently of
    /// `settled`, because "it works" and "it finished" are different claims and
    /// the 16-hour stall was both.
    #[serde(default = "default_true")]
    pub serving: bool,
    /// Structured facts about the block, for the SPA to word in the user's
    /// language. Meaningful only while `stalled`.
    #[serde(default)]
    pub blocked: BlockedStatus,
}

/// Why a stalled deployment is stuck (ADR-050, issue #131). Facts, never a
/// sentence: the only prose here is the cluster's own vocabulary — a health
/// colour, an operator component, a recovery stage, an index name — reproduced
/// verbatim the way `upgrade.reason` is (ADR-045 UI rule 5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedStatus {
    pub health: String,
    /// `-1` when we could not ask or got no answer. Never conflated with `0`.
    pub unassigned_shards: i32,
    /// The operator's own pending work ("RollingRestart" / "Running").
    pub component: String,
    pub component_status: String,
    /// The oldest active shard recovery, when the cluster reported one.
    pub recovery_index: String,
    pub recovery_stage: String,
    pub recovery_secs: i64,
    /// The pod the stall remediation bounced (#27) — `None` until it happened.
    #[serde(default)]
    pub remediated_node: Option<String>,
}

impl Default for BlockedStatus {
    fn default() -> Self {
        blocked_status_of(&crate::activity::Blocked::default())
    }
}

fn blocked_status_of(b: &crate::activity::Blocked) -> BlockedStatus {
    BlockedStatus {
        health: b.health.clone(),
        unassigned_shards: b.unassigned_shards,
        component: b.component.clone(),
        component_status: b.component_status.clone(),
        recovery_index: b.recovery_index.clone(),
        recovery_stage: b.recovery_stage.clone(),
        recovery_secs: b.recovery_secs,
        remediated_node: b.remediated_node.clone(),
    }
}

/// `serde(default)` for a bool is `false`, and a missing `serving` must not
/// make a working deployment look broken — same defensive direction as
/// `ActivityStatus::default`.
fn default_true() -> bool {
    true
}

impl Default for ActivityStatus {
    /// A deployment we know nothing about reads as settled, so a missing field
    /// can never lock the UI shut — the same defensive default as
    /// `upgrade: { state: "idle" }`.
    fn default() -> Self {
        Self {
            kind: "idle".into(),
            stage: "ready".into(),
            percent: 100,
            detail: String::new(),
            settled: true,
            locks_edits: false,
            nodes_ready: 0,
            nodes_total: 0,
            since_secs: 0,
            stalled: false,
            serving: true,
            blocked: BlockedStatus::default(),
        }
    }
}

/// One line of the activity accordion (ADR-050): a Kubernetes Event, a pod's
/// container state, or a row of the operator's `componentsStatus`. Deliberately
/// NOT container logs — see the ADR.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityLine {
    /// RFC 3339, empty when the source carries no timestamp.
    pub at: String,
    /// `info` | `warn` | `error`.
    pub severity: String,
    /// Where it came from: `event` | `pod` | `operator`.
    pub source: String,
    /// The object it is about (`teste-uu9d-nodes-0`, `Upgrader`, …).
    pub object: String,
    /// Short reason (`Pulling`, `ImagePullBackOff`, `Finished`).
    pub title: String,
    /// The upstream message, verbatim.
    pub detail: String,
}

/// Live snapshot state for the deployment chip (ADR-049). Read-only: the
/// editable configuration travels as `crate::snapshot::SnapshotConfig`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapshotStatus {
    pub configured: bool,
    /// Destination bucket, so the chip can name where backups land.
    pub repo: String,
    /// Human-readable schedule ("02:00"), empty when nothing is scheduled.
    pub schedule: String,
    /// The operator's own vocabulary: `PENDING | CREATED | ERROR | IGNORED`.
    pub policy_state: String,
    /// The operator's `reason`, verbatim, when the state is `ERROR`.
    pub last_error: String,
}

/// Where a version upgrade is (ADR-048). `state` is one of `idle`, `pending`,
/// `upgrading`, `finished`, `failed`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UpgradeStatus {
    pub state: String,
    /// Node pool being rolled, when one is.
    pub pool: String,
    pub from: String,
    pub to: String,
    /// The operator's own refusal, verbatim (ADR-045 UI rule 5).
    pub reason: String,
    /// Nodes already rolled onto the new revision, and how many there are —
    /// the "nó N de 3" of the progress line.
    pub nodes_updated: i32,
    pub nodes_total: i32,
}

/// What the UI needs to offer (or refuse) an upgrade of one deployment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UpgradeOptions {
    pub version: String,
    pub dashboards_version: String,
    /// Tested targets that are a legal upgrade from `version`, newest first.
    pub targets: Vec<UpgradeTarget>,
    /// Why an upgrade cannot start right now (cluster not green, one already
    /// running, nothing ahead). Empty = ready.
    pub blocked_reason: String,
    /// The nodes are already on a version the Dashboards have not reached —
    /// the retry-phase-2 case.
    pub dashboards_behind: bool,
    pub upgrade: UpgradeStatus,
    /// The hourly check's find for this deployment (ADR-048 rev. 2), "" when
    /// there is none. Always also present in `targets`.
    #[serde(default)]
    pub suggested_version: String,
}

/// Versions offered by the create wizard (ADR-048 rev. 2).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AvailableVersions {
    /// Newest first. Every entry has both images published.
    pub versions: Vec<String>,
    /// Preselected entry (the newest offered).
    pub default: String,
    /// True when this list came from the upstream check, false when it is the
    /// in-binary catalog fallback (offline install).
    pub discovered: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UpgradeTarget {
    pub version: String,
    /// Stable id (`current`, `lts`) the UI translates — never prose.
    pub note: String,
}

/// Access configuration + what the cluster offers (Settings tab, ADR-027).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccessSettings {
    pub mode: String,
    pub base_domain: String,
    pub ingress_class: String,
    /// Client-provided TLS secret terminating the dashboards Ingresses
    /// (issue #54); empty = controller/edge default TLS (historical behavior).
    pub tls_secret: String,
    /// IngressClasses detected on the cluster (empty = ingress mode unavailable).
    pub available_classes: Vec<String>,
    /// What an empty `base_domain` will resolve to — `<ingress-ip>.sslip.io`.
    /// Empty when no ingress address could be detected, in which case the
    /// screen has to insist on a real domain.
    #[serde(default)]
    pub default_base_domain: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeploymentInfo {
    pub name: String,
    pub namespace: String,
    pub health: String,
    pub phase: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Detected {
    pub kind: String,
    pub namespace: String,
    pub workload: String,
    pub image: String,
    pub recipe: Option<String>,
}

// ─── #65: telemetry a cluster already emits ────────────────────────────────
/// One telemetry endpoint found in the host cluster — a Prometheus, a Hubble
/// relay, an OTEL collector. Produced by `crate::telemetry`, either from the
/// sidecar provisioning manifest (`origin: "sidecar-manifest"`, which also
/// carries `component`/`version`) or from a generic Service scan
/// (`origin: "cluster-scan"`), so a cluster VeloxSearch did not provision
/// still lights up.
///
/// `recipe` is the pre-baked ingest recipe id, and is `None` when the runtime
/// catalog does not currently carry that package — a discovered source is
/// still reported, just not yet offerable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TelemetrySource {
    /// `prometheus` | `hubble` | `otel`.
    pub kind: String,
    pub namespace: String,
    pub service: String,
    pub port: i32,
    /// `<service>.<namespace>.svc:<port>` — no scheme, because we do not know
    /// one for gRPC endpoints and will not guess.
    pub address: String,
    pub recipe: Option<String>,
    pub origin: String,
    /// The component serving it, when the sidecar manifest names one.
    pub component: Option<String>,
    pub version: Option<String>,
}
// ─── end #65 ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Discovery {
    pub deployments: Vec<DeploymentInfo>,
    pub detected: Vec<Detected>,
    /// Telemetry the cluster already emits (#65). Additive: absent/empty means
    /// exactly what it meant before this field existed.
    #[serde(default)]
    pub telemetry: Vec<TelemetrySource>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MonitoringStatus {
    pub doc_count: u64,
    pub receiving: bool,
}

/// What the OTel stack costs and what it is made of — read once by the panel so
/// the UI never hardcodes an image tag or a resource number (ADR-053). Same
/// reason `sizing_presets` is a GET rather than a table in the SPA.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OtelStackInfo {
    pub version: String,
    pub components: Vec<OtelComponentInfo>,
    pub cpu_millis: u64,
    pub mem_mib: u64,
    pub disk_gib: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OtelComponentInfo {
    /// Stable key the SPA maps to an i18n string (`otel_comp_<key>`).
    pub key: String,
    pub image: String,
}

/// One OpenSearch node's live stats (`_nodes/stats`) for the Overview blocks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeStat {
    pub name: String,
    pub roles: Vec<String>,
    pub cpu_percent: i64,
    pub heap_percent: i64,
    pub heap_used_bytes: u64,
    pub heap_max_bytes: u64,
    pub disk_total_bytes: u64,
    pub disk_available_bytes: u64,
    /// Binding phase of this node's data-volume PVC ("Bound"/"Pending"), or ""
    /// when no PVC info is available (ADR-031). An unbound volume reads as
    /// "Pending" so the meter never shows a misleading zeroed/node-sized bar.
    #[serde(default)]
    pub pvc_phase: String,
    /// PVC capacity in bytes (status.capacity, else the requested size). 0 when
    /// unknown — the data-path `fs.data` total is then used for the meter.
    #[serde(default)]
    pub pvc_capacity_bytes: u64,
    pub docs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterMetrics {
    pub nodes: Vec<NodeStat>,
    pub total_docs: u64,
    pub store_size_bytes: u64,
}

/// One downsampled time-bucket of cluster-aggregate health (#9, the "second
/// moment" the Overview's instantaneous bars can't show). Produced by
/// `metrics::downsample` from the samples recorded into `velox-metrics-*`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricPoint {
    /// Bucket start, epoch milliseconds (UTC).
    pub ts: i64,
    /// Mean CPU utilization across nodes in the bucket, percent (0–100).
    pub cpu_percent: f64,
    /// Mean JVM heap utilization across nodes in the bucket, percent (0–100).
    pub heap_percent: f64,
    /// Cluster disk used at the bucket's last sample, bytes.
    pub disk_used_bytes: u64,
    /// Total document count at the bucket's last sample.
    pub docs: u64,
    /// Indexing throughput, documents/sec, derived from the monotonic
    /// `index_total` delta between this bucket and the previous one (0 for the
    /// first bucket, and clamped to 0 across a counter reset / node restart).
    pub indexing_rate: f64,
}

/// A deployment's recent cluster-health time-series (#9). Empty until the
/// sampler has written at least one point — the SPA treats that as "still
/// collecting" rather than an error.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricSeries {
    pub deployment: String,
    pub points: Vec<MetricPoint>,
}

// ───────────────── host-cluster capacity & health (Capacidade panel) ────────
// The K3S cluster *underneath* the deployments: per-node CPU/mem/disk and how
// much room is left for more OpenSearch clusters. Built by `capacity.rs`.

/// One resource axis. CPU is in **millicores**, memory/disk in **bytes**.
/// `used` is the live figure (None when metrics-server is unavailable);
/// `requested` is what the scheduler has already committed (None where N/A,
/// e.g. host disk). `total` is the allocatable/capacity denominator.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResUse {
    pub total: u64,
    pub used: Option<u64>,
    pub requested: Option<u64>,
}

/// One K3S node's capacity & health. `host_disk`/`storage` are `None` when the
/// kubelet Summary API / Longhorn are unavailable (panel shows "n/d").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeCapacity {
    pub name: String,
    pub roles: Vec<String>,
    pub ready: bool,
    /// Active node pressures (MemoryPressure / DiskPressure / PIDPressure).
    pub pressures: Vec<String>,
    pub cpu: ResUse,
    pub mem: ResUse,
    pub host_disk: Option<ResUse>,
    pub storage: Option<ResUse>,
}

/// Cluster-wide persistent storage pool (Longhorn), bytes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoragePool {
    pub total: u64,
    pub used: u64,
    pub available: u64,
}

/// "How many more deployments of this preset fit, and what runs out first."
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeploymentFit {
    pub size: String,
    pub count: u64,
    /// The binding constraint: "cpu" | "mem" | "disk".
    pub limited_by: String,
}

/// The whole Capacidade payload. `metrics_available=false` means live CPU/mem
/// could not be read — the SPA then shows requests-only bars + a notice.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterCapacity {
    pub nodes: Vec<NodeCapacity>,
    /// Cluster-aggregate CPU (millicores) and memory (bytes).
    pub cpu: ResUse,
    pub mem: ResUse,
    pub storage: Option<StoragePool>,
    pub fit: Vec<DeploymentFit>,
    pub metrics_available: bool,
}

/// Cluster conformity per ADR-014: which prerequisites are present/serving.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BootstrapStatus {
    pub cert_manager_installed: bool,
    pub cert_manager_ready: bool,
    pub operator_installed: bool,
    pub operator_ready: bool,
    pub ready: bool,
    pub installing: Option<String>,
    pub error: Option<String>,
    /// REQUIREMENTS.md R1–R8 evaluated live (probe v2, ADR-026).
    pub requirements: Vec<ReqCheck>,
    /// Any hard failure — the installer refuses to run on this cluster.
    pub unsupported: bool,
}

/// Read-only storage classification for the "auto-install + notify" Longhorn
/// flow (ADR-031/043). The create flow polls this to learn whether creating a
/// cluster will auto-install Longhorn (`needs_longhorn`) and, while one runs, to
/// show progress (`installing`) / a completion notice. Reading it installs nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StorageStatus {
    pub durable: bool,
    pub needs_longhorn: bool,
    pub default_class: Option<String>,
    pub detail: String,
    pub installing: Option<String>,
    pub error: Option<String>,
    /// Node packages Longhorn reports missing (`#15`, ADR-043) — the create
    /// flow renders one panel per entry (package + per-distro commands) and
    /// blocks creation while any remain.
    pub missing_packages: Vec<MissingPackage>,
}

/// One node package Longhorn needs but the node lacks. `package`/`install` are
/// `None` when the condition message wasn't recognised — the frontend then
/// shows `node` + `reason` verbatim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MissingPackage {
    pub node: String,
    pub package: Option<String>,
    pub reason: String,
    pub install: Option<InstallCommands>,
}

/// Copy-pasteable one-line install commands per distro.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstallCommands {
    pub debian: String,
    pub ubuntu: String,
    pub arch: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReqCheck {
    pub id: String,
    /// "pass" | "warn" | "fail"
    pub status: String,
    pub detail: String,
}

// ─────────────────────────────── server side ───────────────────────────────
// Everything below talks to Axum / the K8s + OpenSearch layer and only compiles
// for the server target.

#[cfg(feature = "ssr")]
mod server {
    use super::*;
    use axum::{
        extract::Json,
        http::{header, HeaderMap, HeaderValue, StatusCode},
        response::{IntoResponse, Response},
        routing::{get, post},
        Router,
    };

    use crate::scope::{Scope, ScopeError};

    // ───────────────────────── error envelope ──────────────────────────

    /// Uniform error response: `{ "error": "<message>" }` + a status code.
    pub struct ApiError {
        status: StatusCode,
        message: String,
    }

    impl ApiError {
        fn new(status: StatusCode, message: impl Into<String>) -> Self {
            Self {
                status,
                message: message.into(),
            }
        }
        /// Anything bubbling up from the K8s / OpenSearch layer → 500.
        ///
        /// `{e:#}` — the ALTERNATE form — so an `anyhow` error arrives with its
        /// full context chain ("applying OpenSearchCluster CR: <what the API
        /// server actually said>") instead of only the outermost sentence,
        /// which is unactionable in a 500 body.
        fn internal(e: impl std::fmt::Display) -> Self {
            Self::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
        }
        fn bad_request(message: impl Into<String>) -> Self {
            Self::new(StatusCode::BAD_REQUEST, message)
        }
        fn unauthorized(message: impl Into<String>) -> Self {
            Self::new(StatusCode::UNAUTHORIZED, message)
        }
    }

    /// A refusal from the ownership layer is an API error verbatim — same
    /// status, same message — so a scoped handler can `?` it without inventing
    /// a second vocabulary for "not yours".
    impl From<ScopeError> for ApiError {
        fn from(e: ScopeError) -> Self {
            Self::new(e.status(), e.message())
        }
    }

    impl IntoResponse for ApiError {
        fn into_response(self) -> Response {
            (
                self.status,
                Json(serde_json::json!({ "error": self.message })),
            )
                .into_response()
        }
    }

    // ───────────────────────── request / response shapes ───────────────

    /// Reported to the SPA so it can decide which screen to mount without
    /// relying on server-side redirects.
    #[derive(Serialize)]
    pub struct AuthStateDto {
        /// No admin account exists yet → the SPA must show /setup.
        pub first_run: bool,
        pub authenticated: bool,
        pub username: Option<String>,
        /// `tenants.id` when the session is a multi-tenant one (#79); `None`
        /// for the installation admin, who is not a member of any org.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tenant: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct LoginReq {
        pub username: String,
        pub password: String,
    }

    #[derive(Deserialize)]
    pub struct SetupReq {
        pub username: String,
        pub password: String,
        pub confirm: String,
    }

    /// Shared by `create_cluster` and `save_cluster` (same params as the old
    /// server fns; `nodes`/`memory`/`disk`/`config` are free-form strings the
    /// backend parses, `monitors` is a comma-separated list or null).
    #[derive(Deserialize)]
    pub struct ClusterReq {
        pub name: String,
        pub size: String,
        pub purpose: String,
        pub nodes: String,
        pub memory: String,
        pub disk: String,
        pub config: String,
        pub monitors: Option<String>,
        /// OpenSearch version to create the deployment with (wizard, ADR-048
        /// rev. 2). Honored by `create_cluster` only — `save_cluster` ignores
        /// it, so an edit can never move a live deployment's version.
        #[serde(default)]
        pub version: Option<String>,
        /// Optional snapshot repository chosen in the wizard (ADR-049).
        /// `create_cluster` only, for the same reason as `version`: the slice
        /// has its own write path, and a save must never touch it.
        #[serde(default)]
        pub snapshot: Option<crate::snapshot::SnapshotConfig>,
    }

    #[derive(Deserialize)]
    pub struct NameReq {
        pub name: String,
    }

    /// Recent cluster-health series request (#9). `window_minutes` is the look-back
    /// and `buckets` the target resolution; both optional with sane defaults so the
    /// frontend can poll with a bare `{ name }`.
    #[derive(Deserialize)]
    pub struct MetricSeriesReq {
        pub name: String,
        #[serde(default)]
        pub window_minutes: Option<i64>,
        #[serde(default)]
        pub buckets: Option<usize>,
    }

    /// Custom-size inputs (ADR-016): the wizard's Advanced path posts a node
    /// memory and/or disk; the backend resolves the rest (3 nodes, heap = half
    /// the memory). Blank fields fall back to the smallest preset.
    #[derive(Deserialize)]
    pub struct CustomSizingReq {
        #[serde(default)]
        pub memory: String,
        #[serde(default)]
        pub disk: String,
    }

    #[derive(Deserialize)]
    pub struct RecipeReq {
        pub deployment: String,
        pub recipe: String,
    }

    /// Install/uninstall/status of the OTel observability stack (ADR-053).
    /// The two scrape targets are optional overrides: absent means the
    /// corresponding Prometheus job is omitted rather than pointed at a guess.
    #[derive(Deserialize)]
    pub struct OtelStackReq {
        pub deployment: String,
        #[serde(default)]
        pub kube_state_metrics: Option<String>,
        #[serde(default)]
        pub node_exporter: Option<String>,
        /// Uninstall only: also drop the telemetry indices. Defaults to false —
        /// removing the machinery must never remove the data by accident.
        #[serde(default)]
        pub delete_indices: bool,
    }

    #[derive(Deserialize)]
    pub struct ResetPasswordReq {
        pub name: String,
        pub new_password: String,
    }

    #[derive(Deserialize)]
    pub struct AccessSettingsReq {
        pub mode: String,
        pub base_domain: String,
        pub ingress_class: String,
        /// Name of a `kubernetes.io/tls` Secret for the dashboards Ingresses
        /// (issue #54). Empty/absent = no `spec.tls` (historical behavior).
        #[serde(default)]
        pub tls_secret: String,
        /// Optional PEM pair — when both are set the app creates/updates the
        /// Secret itself (defaults to `veloxsearch-dashboards-tls` when no
        /// name is given). Clients with an issuer (cert-manager, external PKI)
        /// leave these empty and just name the Secret the issuer maintains.
        #[serde(default)]
        pub tls_cert: String,
        #[serde(default)]
        pub tls_key: String,
    }

    /// Body of `set_next_ui`.
    #[derive(Deserialize)]
    pub struct NextUiReq {
        pub deployment: String,
        pub enabled: bool,
    }

    /// Body of `set_ip_allow_list`.
    #[derive(Deserialize)]
    pub struct IpAllowReq {
        pub deployment: String,
        #[serde(default)]
        pub cidrs: Vec<String>,
    }

    /// `dashboard_credentials` returned a `(String, String)` tuple; the JSON API
    /// names the fields so the frontend doesn't index a positional array.
    #[derive(Serialize)]
    pub struct DashCreds {
        pub username: String,
        pub password: String,
    }

    // ── auth provider (ADR-045, #56) ───────────────────────────────────

    /// What the Authentication screen needs to render itself: the saved spec
    /// with credentials redacted, plus the constraints it must respect.
    #[derive(Serialize)]
    pub struct AuthProviderDto {
        pub spec: crate::auth_provider::AuthProviderSpec,
        /// The deployment's public origin, or null in port-forward mode.
        pub public_url: Option<String>,
        /// Kinds this deployment can currently use. `oidc`/`saml` drop out
        /// without a public HTTPS origin — the screen disables those cards and
        /// shows `redirect_blocked_reason` on them instead of failing on save.
        pub kinds_available: Vec<&'static str>,
        pub redirect_blocked_reason: Option<String>,
        /// Roles the group→role table offers (static roles of the plugin).
        pub builtin_roles: Vec<&'static str>,
        /// The account that keeps working if the identity provider is down.
        pub break_glass_user: String,
        /// Sentinel meaning "keep the stored credential" on the way back in.
        pub secret_kept: &'static str,
    }

    /// Start a version upgrade (ADR-048). `allow_untested` is the advanced
    /// free-text override; `confirm_unverified` is the user accepting that the
    /// image registry could not be reached.
    #[derive(Deserialize)]
    pub struct UpgradeReq {
        pub name: String,
        pub version: String,
        #[serde(default)]
        pub allow_untested: bool,
        #[serde(default)]
        pub confirm_unverified: bool,
    }

    #[derive(Deserialize)]
    pub struct AuthProviderReq {
        pub name: String,
        pub spec: crate::auth_provider::AuthProviderSpec,
    }

    /// Configure a deployment's snapshot repository + scheduled policy
    /// (ADR-049). Credentials arrive either as new key material or as the
    /// `secret_kept` sentinel meaning "keep what is stored".
    #[derive(Deserialize)]
    pub struct SnapshotReq {
        pub name: String,
        pub config: crate::snapshot::SnapshotConfig,
    }

    /// What the Backup tab renders: the saved configuration, its live state,
    /// and whether saving THIS configuration would restart the nodes — decided
    /// server-side (ADR-049 invariant 4).
    #[derive(Serialize)]
    pub struct SnapshotConfigDto {
        pub config: crate::snapshot::SnapshotConfig,
        pub state: SnapshotStatus,
        /// True once a credentials Secret exists — the UI then stops demanding
        /// the keys on every save.
        pub has_credentials: bool,
        /// Default `base_path` for a fresh configuration.
        pub deployment: String,
    }

    #[derive(Serialize)]
    pub struct ProbeDto {
        pub ok: bool,
        pub checks: Vec<String>,
        pub error: Option<String>,
    }

    // ───────────────────────── domain mappers ──────────────────────────

    pub fn to_dto(s: crate::k8s::Status) -> ClusterStatus {
        ClusterStatus {
            name: s.name,
            phase: s.phase,
            health: s.health,
            nodes_ready: s.nodes_ready,
            nodes_desired: s.nodes_desired,
            size: s.size,
            purpose: s.purpose,
            monitors: s.monitors,
            otel_stack: s.otel_stack,
            next_ui: s.next_ui,
            next_ui_chosen: s.next_ui_chosen,
            ip_allow_list: s.ip_allow_list,
            replicas: s.replicas,
            heap: s.heap,
            memory: s.memory,
            disk: s.disk,
            extra_config: s.extra_config,
            dashboard_url: s.dashboard_url,
            opensearch_url: s.opensearch_url,
            dashboard_portforward: s.dashboard_portforward,
            auth_kind: s.auth_kind,
            version: s.version,
            target_version: s.target_version,
            dashboards_version: s.dashboards_version,
            upgrade: upgrade_dto(&s.upgrade, s.nodes_updated, s.nodes_desired),
            suggested_version: s.suggested_version,
            created_at: s.created_at,
            snapshot: snapshot_dto(&s.snapshot),
            activity: activity_dto(&s.activity),
            provisioning: provisioning_dto(&s.provisioning),
        }
    }

    /// Flatten the deferred-provisioning state into the DTO the SPA reads
    /// (ADR-052).
    pub fn provisioning_dto(p: &crate::provisioning::ProvisioningState) -> ProvisioningStatus {
        ProvisioningStatus {
            state: p.state.to_string(),
            profile_pending: p.profile_pending,
            monitors_pending: p.monitors_pending.clone(),
            attempts: p.attempts,
            last_error: p.last_error.clone(),
            updated_at: p.updated_at.clone(),
        }
    }

    /// Flatten the activity verdict into the DTO the SPA reads (ADR-050).
    pub fn activity_dto(a: &crate::activity::Activity) -> ActivityStatus {
        ActivityStatus {
            kind: a.kind.to_string(),
            stage: a.stage.to_string(),
            percent: a.percent,
            detail: a.detail.clone(),
            settled: a.settled,
            locks_edits: a.locks_edits,
            nodes_ready: a.nodes_ready,
            nodes_total: a.nodes_total,
            since_secs: a.since_secs,
            stalled: a.stalled,
            serving: a.serving,
            blocked: blocked_status_of(&a.blocked),
        }
    }

    /// Flatten the snapshot state into the DTO the SPA reads (ADR-049).
    pub fn snapshot_dto(s: &crate::snapshot::SnapshotState) -> SnapshotStatus {
        SnapshotStatus {
            configured: s.configured,
            repo: s.repo.clone(),
            schedule: s.schedule.clone(),
            policy_state: s.policy_state.clone(),
            last_error: s.last_error.clone(),
        }
    }

    /// Flatten the upgrade state into the DTO the SPA reads (ADR-048).
    pub fn upgrade_dto(
        u: &crate::upgrade::UpgradeState,
        nodes_updated: i32,
        nodes_total: i32,
    ) -> UpgradeStatus {
        use crate::upgrade::UpgradeState as U;
        let mut d = UpgradeStatus {
            state: u.kind().to_string(),
            nodes_updated,
            nodes_total,
            ..Default::default()
        };
        match u {
            U::Idle => {}
            U::Pending { from, to } => {
                d.from = from.clone();
                d.to = to.clone();
            }
            U::Upgrading { pool, from, to } => {
                d.pool = pool.clone();
                d.from = from.clone();
                d.to = to.clone();
            }
            U::Finished { version } => d.to = version.clone(),
            U::Failed { reason } => d.reason = reason.clone(),
        }
        d
    }

    pub fn storage_dto(s: crate::bootstrap::StorageState) -> StorageStatus {
        StorageStatus {
            durable: s.durable,
            needs_longhorn: s.needs_longhorn,
            default_class: s.default_class,
            detail: s.detail,
            installing: s.installing,
            error: s.error,
            missing_packages: s
                .missing_packages
                .into_iter()
                .map(missing_package_dto)
                .collect(),
        }
    }

    fn missing_package_dto(m: crate::bootstrap::MissingPackage) -> MissingPackage {
        MissingPackage {
            node: m.node,
            package: m.package.map(str::to_string),
            reason: m.reason,
            install: m.install.map(|i| InstallCommands {
                debian: i.debian.to_string(),
                ubuntu: i.ubuntu.to_string(),
                arch: i.arch.to_string(),
            }),
        }
    }

    pub fn bootstrap_dto(s: crate::bootstrap::State) -> BootstrapStatus {
        BootstrapStatus {
            cert_manager_installed: s.cert_manager_installed,
            cert_manager_ready: s.cert_manager_ready,
            operator_installed: s.operator_installed,
            operator_ready: s.operator_ready,
            ready: s.ready,
            installing: s.installing,
            error: s.error,
            requirements: s
                .requirements
                .into_iter()
                .map(|r| ReqCheck {
                    id: r.id.to_string(),
                    status: r.status.to_string(),
                    detail: r.detail,
                })
                .collect(),
            unsupported: s.unsupported,
        }
    }

    /// Turn the wizard's free-form override fields into typed `CreateOverrides`.
    ///
    /// A non-empty `nodes` that isn't a number is a REFUSAL, not a fallback
    /// (#52): silently dropping it would reset an edited deployment's node
    /// count back to its preset — a change the user never asked for.
    pub fn parse_overrides(
        nodes: String,
        memory: String,
        disk: String,
        config: String,
        monitors: Option<String>,
    ) -> Result<crate::k8s::CreateOverrides, String> {
        let opt = |s: String| {
            let t = s.trim().to_string();
            (!t.is_empty()).then_some(t)
        };
        let replicas =
            match nodes.trim() {
                "" => None,
                n => Some(n.parse::<u32>().map_err(|_| {
                    format!("invalid node count '{n}': use a whole number of nodes")
                })?),
            };
        let mut additional = serde_json::Map::new();
        for line in config.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim();
                if !k.is_empty() {
                    additional.insert(
                        k.to_string(),
                        serde_json::Value::String(v.trim().to_string()),
                    );
                }
            }
        }
        let monitors = monitors
            .map(|m| {
                m.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(crate::k8s::CreateOverrides {
            replicas,
            memory: opt(memory),
            disk: opt(disk),
            additional_config: additional,
            monitors,
            // Set by the create handler only; a save never carries a version
            // (ADR-048 invariant 1).
            version: None,
        })
    }

    /// Baseline collection agent that ships on EVERY non-search deployment.
    /// `kubernetes` (all cluster/pod logs) is the always-on default — see
    /// `with_baseline_monitors`.
    pub const BASELINE_MONITOR: &str = "kubernetes";

    /// Guarantee a pre-configured Fluent Bit collector out of the box (#53).
    ///
    /// A non-search deployment must never come up "integrated" yet collecting
    /// nothing. The frontend wizard defaults the `kubernetes` source on, but the
    /// operator can uncheck it (or create via the API with no monitors at all) —
    /// which previously left the deployment with zero collection until someone
    /// later clicked Enable in the Integrations tab. Seed the always-on baseline
    /// when the selection is empty so the agent is deployed FROM creation by the
    /// existing deferred recipe machinery (ADR-018), never deferred to a manual
    /// step. An explicit non-empty selection is respected verbatim; `search`
    /// installs no agents (ADR-028), so it is left empty.
    pub fn with_baseline_monitors(mut monitors: Vec<String>, purpose: &str) -> Vec<String> {
        if purpose != "search" && monitors.is_empty() {
            monitors.push(BASELINE_MONITOR.to_string());
        }
        monitors
    }

    // ───────────────────────── auth helpers ────────────────────────────

    /// The verified session on this request, if any.
    ///
    /// Delegates to `auth::session_from_cookies` rather than re-splitting the
    /// cookie here: since #79 a token has two shapes (admin, and tenant-
    /// carrying) and a second parser would be a second thing to get wrong.
    /// This is also the extraction point handlers use to scope by tenant —
    /// `session(headers).and_then(|s| s.tenant)` — which is the seam #80's
    /// ownership enforcement builds on.
    fn session(headers: &HeaderMap) -> Option<crate::auth::Session> {
        crate::auth::session_from_cookies(headers.get(header::COOKIE)?.to_str().ok()?)
    }

    /// A bare 200 carrying a single `Set-Cookie` header.
    fn cookie_response(cookie: String) -> Response {
        let mut resp = StatusCode::OK.into_response();
        if let Ok(hv) = HeaderValue::from_str(&cookie) {
            resp.headers_mut().insert(header::SET_COOKIE, hv);
        }
        resp
    }

    // ───────────────────────── handlers: auth ──────────────────────────

    async fn auth_state(headers: HeaderMap) -> Json<AuthStateDto> {
        let first_run = crate::auth::is_first_run().await;
        let session = session(&headers);
        Json(AuthStateDto {
            first_run,
            authenticated: session.is_some(),
            username: session.as_ref().map(|s| s.user.clone()),
            tenant: session.and_then(|s| s.tenant),
        })
    }

    async fn login(Json(req): Json<LoginReq>) -> Result<Response, ApiError> {
        // The installation admin is tried first and unchanged: with the #79
        // flag off `tenants::authenticate` returns `None` without touching the
        // datastore, so this handler behaves exactly as it did before.
        if crate::auth::check_credentials(&req.username, &req.password).await {
            let token = crate::auth::make_token(&req.username);
            return Ok(cookie_response(crate::auth::session_cookie(&token)));
        }
        match crate::tenants::authenticate(&req.username, &req.password).await {
            Ok(Some(principal)) => {
                let token = crate::auth::make_tenant_token(&principal.email, &principal.tenant_id);
                return Ok(cookie_response(crate::auth::session_cookie(&token)));
            }
            Ok(None) => {}
            // A datastore failure is a failed login, never a bypass.
            Err(e) => tracing::error!("control-plane account lookup failed: {e:#}"),
        }
        Err(ApiError::unauthorized("Invalid username or password"))
    }

    async fn logout() -> Response {
        cookie_response(crate::auth::clear_cookie())
    }

    async fn setup_admin(Json(req): Json<SetupReq>) -> Result<Response, ApiError> {
        if req.password != req.confirm {
            return Err(ApiError::bad_request("passwords do not match"));
        }
        crate::auth::complete_setup(&req.username, &req.password)
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        // Auto-login: hand the new admin a session straight away.
        let token = crate::auth::make_token(&req.username);
        Ok(cookie_response(crate::auth::session_cookie(&token)))
    }

    // ── tenant auth handlers (#79) ─────────────────────────────────────
    //
    // Self-serve account flows on the ADR-041 datastore. All four are
    // unauthenticated by necessity — a stranger has no session — and all four
    // 404 unless `VELOX_MULTITENANT_AUTH` is on, so with the flag off this
    // surface does not exist rather than merely refusing.
    //
    // The responses are shaped by one rule: **a stranger must not learn which
    // addresses have accounts.** Signup answers `202 accepted` whether or not
    // the address was free; a reset request answers `202 accepted` whether or
    // not the account exists; a bad verification or reset link answers the same
    // way whether it is unknown, expired or already spent. The only 400s are
    // for input the user can see is wrong in their own form (a malformed
    // address, a free mailbox, a short password). The domain logic and the
    // reasoning behind it live in `src/tenants.rs`.

    #[derive(Deserialize)]
    pub struct SignupReq {
        pub email: String,
        pub password: String,
        /// Optional human name for the tenant; the slug is derived from it.
        #[serde(default)]
        pub tenant_name: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct TokenReq {
        pub token: String,
    }

    #[derive(Deserialize)]
    pub struct EmailReq {
        pub email: String,
    }

    #[derive(Deserialize)]
    pub struct ResetReq {
        pub token: String,
        pub password: String,
    }

    /// 404 when the flag is off — the endpoint is absent, not forbidden.
    fn require_tenant_auth() -> Result<(), ApiError> {
        if crate::tenants::enabled() {
            Ok(())
        } else {
            Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "self-serve accounts are not enabled on this installation",
            ))
        }
    }

    /// The one answer every enumeration-safe path gives.
    fn accepted(message: &str) -> Response {
        (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "status": "accepted", "message": message })),
        )
            .into_response()
    }

    async fn signup(Json(req): Json<SignupReq>) -> Result<Response, ApiError> {
        require_tenant_auth()?;
        crate::tenants::signup(&req.email, &req.password, req.tenant_name.as_deref())
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        Ok(accepted(
            "If that address can be registered, a confirmation link is on its way.",
        ))
    }

    async fn verify_email(Json(req): Json<TokenReq>) -> Result<Response, ApiError> {
        require_tenant_auth()?;
        let verified = crate::tenants::verify_email(&req.token)
            .await
            .map_err(ApiError::internal)?;
        if verified {
            Ok(Json(serde_json::json!({ "verified": true })).into_response())
        } else {
            Err(ApiError::bad_request(
                "That confirmation link is no longer valid. Request a new one.",
            ))
        }
    }

    async fn request_password_reset(Json(req): Json<EmailReq>) -> Result<Response, ApiError> {
        require_tenant_auth()?;
        crate::tenants::request_password_reset(&req.email)
            .await
            .map_err(ApiError::internal)?;
        Ok(accepted(
            "If that address has an account, a reset link is on its way.",
        ))
    }

    async fn reset_password(Json(req): Json<ResetReq>) -> Result<Response, ApiError> {
        require_tenant_auth()?;
        let reset = crate::tenants::reset_password(&req.token, &req.password)
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if reset {
            Ok(Json(serde_json::json!({ "reset": true })).into_response())
        } else {
            Err(ApiError::bad_request(
                "That reset link is no longer valid. Request a new one.",
            ))
        }
    }

    // ───────────────────────── handlers: bootstrap ─────────────────────

    async fn bootstrap_status(scope: Scope) -> Result<Json<BootstrapStatus>, ApiError> {
        scope.require_admin()?;
        crate::bootstrap::status()
            .await
            .map(bootstrap_dto)
            .map(Json)
            .map_err(ApiError::internal)
    }

    async fn bootstrap_ensure(scope: Scope) -> Result<Json<BootstrapStatus>, ApiError> {
        scope.require_admin()?;
        crate::bootstrap::ensure()
            .await
            .map(bootstrap_dto)
            .map(Json)
            .map_err(ApiError::internal)
    }

    /// Read-only storage classification (ADR-031). The create flow polls this to
    /// know whether creating a cluster will auto-install Longhorn and to render
    /// the install progress + completion notice. Never triggers an install.
    async fn storage_status(scope: Scope) -> Result<Json<StorageStatus>, ApiError> {
        scope.require_admin()?;
        crate::bootstrap::storage_status()
            .await
            .map(storage_dto)
            .map(Json)
            .map_err(ApiError::internal)
    }

    // ───────────────────────── handlers: deployments ───────────────────

    /// What is running on the HOST cluster that could be monitored. Admin-only
    /// — it enumerates workloads across namespaces, which for a tenant would be
    /// a window onto other tenants' (and the operator's) deployments.
    async fn discover(scope: Scope) -> Result<Json<Discovery>, ApiError> {
        scope.require_admin()?;
        crate::discovery::discover()
            .await
            .map(Json)
            .map_err(ApiError::internal)
    }

    /// Only the caller's deployments: a tenant sees its own namespace filtered
    /// by its owner label, the admin sees everything (`k8s::list_clusters`).
    async fn list_deployments(scope: Scope) -> Result<Json<Vec<ClusterStatus>>, ApiError> {
        let v = crate::k8s::list_deployments(&scope)
            .await
            .map_err(ApiError::internal)?;
        Ok(Json(v.into_iter().map(to_dto).collect()))
    }

    /// Host-cluster capacity & health for the Capacidade panel (node CPU/mem/
    /// disk + "how many more deployments fit"). No-arg read → GET.
    ///
    /// Admin-only: it reports the whole cluster's free resources, which is
    /// aggregate information about every other tenant's footprint. A tenant's
    /// view of headroom is its ADR-041 quota (#84), not the host's node list.
    async fn cluster_capacity(scope: Scope) -> Result<Json<ClusterCapacity>, ApiError> {
        scope.require_admin()?;
        crate::capacity::cluster_capacity()
            .await
            .map(Json)
            .map_err(ApiError::internal)
    }

    /// One deployment's detail, or `null`.
    ///
    /// Anti-enumeration: `scope.resolve` answers `None` identically for a name
    /// that does not exist and for one that belongs to another tenant, so this
    /// route's response cannot be used to discover that a deployment exists.
    /// The `Option` shape is unchanged — the frontend still reads `null`.
    async fn get_deployment(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<Json<Option<ClusterStatus>>, ApiError> {
        let Some(dep) = scope.resolve(&req.name).await.map_err(ApiError::internal)? else {
            return Ok(Json(None));
        };
        let d = crate::k8s::get_deployment(&dep)
            .await
            .map_err(ApiError::internal)?;
        Ok(Json(d.map(to_dto)))
    }

    /// Deleting something that is not yours is a 404, exactly like deleting
    /// something that does not exist — including the previously silent no-op
    /// on an unknown name, which now says so.
    async fn delete_cluster(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.name).await?;
        crate::k8s::delete_cluster(&dep)
            .await
            .map_err(ApiError::internal)?;
        Ok(StatusCode::OK)
    }

    /// Create a NEW deployment. Generates a unique `<name>-<suffix>` (ADR-020)
    /// and returns the generated name (a bare JSON string) so the UI can link to it.
    async fn create_cluster(
        scope: Scope,
        Json(req): Json<ClusterReq>,
    ) -> Result<Json<String>, ApiError> {
        let purpose = req.purpose;
        let mut ov = parse_overrides(req.nodes, req.memory, req.disk, req.config, req.monitors)
            .map_err(ApiError::bad_request)?;
        // The search profile installs NO agents (ADR-028) — enforce server-side,
        // the UI hiding the checkboxes is just presentation.
        if purpose == "search" {
            ov.monitors.clear();
        }
        // Pre-configured collection out of the box (#53): never let a non-search
        // deployment come up integrated yet collecting nothing — seed the
        // always-on baseline collector when no integration was selected.
        ov.monitors = with_baseline_monitors(ov.monitors, &purpose);
        // The wizard's version choice (ADR-048 rev. 2). Only the create path
        // passes it on; `save_cluster` deliberately does not.
        ov.version = req.version.clone();
        // Refuse a malformed snapshot configuration BEFORE the cluster is
        // created (#52): the repository is only registered minutes later, once
        // the cluster is green, and a create that silently drops the backup the
        // user configured is worse than a create that never happened.
        let snapshot = req.snapshot.filter(|s| s.enabled);
        if let Some(s) = snapshot.as_ref() {
            crate::snapshot::validate(s).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        }
        let final_name = crate::k8s::unique_name(&scope, &req.name)
            .await
            .map_err(ApiError::internal)?;
        // The new deployment is claimed INTO the caller's namespace, carrying
        // the caller's owner label — which is what makes it resolvable (and
        // only by them) on every later request.
        let dep = scope.claim(&final_name)?;
        crate::k8s::create_cluster(&dep, &req.size, &purpose, ov)
            .await
            .map_err(ApiError::internal)?;
        // Monitor-at-creation (ADR-018) and the purpose profile (ADR-028) are
        // DEFERRED: OpenSearch isn't answering yet, so the work waits for the
        // cluster to settle (ADR-050) and happens in the background.
        //
        // ADR-052 — the record is written HERE, in the request path, before the
        // task exists. That ordering is the fix: a deferred apply that never
        // ran used to leave no trace but a log line, so a green deployment with
        // no agents and no profile was indistinguishable from a finished one.
        // Marking first means the failure is visible even if the backend dies
        // during the wait. It is best-effort — refusing a create because an
        // annotation would not patch is the worse trade — but a create whose
        // mark failed is loud in the log rather than silent in the product.
        if let Err(e) = crate::k8s::begin_deferred_provisioning(&dep).await {
            tracing::error!(
                "could not record the deferred provisioning of {dep}: {e:#} — the profile \
                 and monitors are still being applied, but a failure will not be reported \
                 on the deployment"
            );
        }
        // The task inherits the handle the ownership check already produced —
        // it never re-derives a target from a request body. What it applies is
        // re-read from the CR on every attempt (ADR-052), so nothing but the
        // snapshot credentials travels in memory.
        crate::k8s::spawn_deferred_provisioning(dep, snapshot);
        Ok(Json(final_name))
    }

    /// Save (upsert) an EXISTING deployment — uses the name verbatim (no suffix).
    async fn save_cluster(
        scope: Scope,
        Json(req): Json<ClusterReq>,
    ) -> Result<StatusCode, ApiError> {
        // Save edits an EXISTING deployment, so it must already resolve inside
        // the caller's scope — otherwise a save is an unauthorized create in
        // someone else's namespace.
        let dep = scope.require(&req.name).await?;
        let purpose = req.purpose;
        let mut ov = parse_overrides(req.nodes, req.memory, req.disk, req.config, req.monitors)
            .map_err(ApiError::bad_request)?;
        if purpose == "search" {
            ov.monitors.clear();
        }
        crate::k8s::create_cluster(&dep, &req.size, &purpose, ov)
            .await
            .map_err(ApiError::internal)?;
        // Re-apply the purpose profile (idempotent). A save can change purpose
        // on a live deployment: retention windows move, and switching TO search
        // must honor "no agents" by removing any that are running (data kept).
        //
        // Same defect, same fix (ADR-052): this wait could time out too, and
        // then a save reported 200 with the profile unapplied and nothing on
        // the deployment to say so. The bitter joke is that clicking Save was
        // also the only known recovery from the create-path bug — an operator
        // stumbled into it — which it can only be if the save path itself does
        // not have the same hole.
        //
        // A save deliberately re-marks the record from scratch even when the
        // previous one completed: the purpose or the monitor list may have just
        // changed, and `Record::pending` re-derives from the CR the save wrote.
        if let Err(e) = crate::k8s::begin_deferred_provisioning(&dep).await {
            tracing::error!(
                "could not record the deferred re-apply on {dep}: {e:#} — profile \
                 '{purpose}' is still being re-applied, but a failure will not be \
                 reported on the deployment"
            );
        }
        crate::k8s::spawn_deferred_provisioning(dep, None);
        Ok(StatusCode::OK)
    }

    /// Re-run the deferred profile + monitor application on a deployment that
    /// is reporting `pending` or `failed` (ADR-052).
    ///
    /// The button behind "monitores não aplicados". It starts a FRESH attempt
    /// schedule — the persisted counter is what stops an automatic retry loop,
    /// and a person asking for one is not that loop. Returns as soon as the
    /// work is accepted, because settling takes minutes; the deployment's
    /// `provisioning` field is where the outcome shows up.
    ///
    /// Safe to call on a deployment with nothing outstanding, and safe to call
    /// twice: every item applied is an idempotent upsert, and the applier
    /// re-reads what is owed from the CR rather than from this request.
    async fn retry_provisioning(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.name).await?;
        crate::k8s::begin_deferred_provisioning(&dep)
            .await
            .map_err(ApiError::internal)?;
        crate::k8s::spawn_deferred_provisioning(dep, None);
        Ok(StatusCode::ACCEPTED)
    }

    // ─────────────────── handlers: version upgrade (ADR-048) ───────────

    /// Versions the create wizard offers: the newest few the hourly upstream
    /// check confirmed (both images published), falling back to the pinned
    /// catalog when the check never answered. No args → GET.
    async fn available_versions() -> Json<AvailableVersions> {
        let versions = crate::version_feed::create_choices();
        Json(AvailableVersions {
            default: versions
                .first()
                .cloned()
                .unwrap_or_else(|| crate::upgrade::DEFAULT_VERSION.to_string()),
            versions,
            discovered: !crate::version_feed::latest().is_empty(),
        })
    }

    /// What this deployment could upgrade to, and why it can't right now.
    /// Everything here is read from the CR — the UI never computes the rules.
    async fn upgrade_options(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<Json<UpgradeOptions>, ApiError> {
        let dep = scope.require(&req.name).await?;
        let s = crate::k8s::get_deployment(&dep)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::bad_request(format!("no deployment named '{}'", req.name)))?;

        let mut targets: Vec<UpgradeTarget> = crate::upgrade::targets_for(&s.version)
            .into_iter()
            .map(|e| UpgradeTarget {
                version: e.version.to_string(),
                note: e.note.to_string(),
            })
            .collect();
        // The hourly check's find leads the list when it is ahead of everything
        // we ship — it is the version the "Upgrade vX" tag names, so the modal
        // must offer exactly it (ADR-048 rev. 2).
        if !s.suggested_version.is_empty()
            && !targets.iter().any(|t| t.version == s.suggested_version)
        {
            targets.insert(
                0,
                UpgradeTarget {
                    version: s.suggested_version.clone(),
                    note: "latest".into(),
                },
            );
        }
        let dashboards_behind =
            !s.dashboards_version.is_empty() && s.dashboards_version != s.version;

        // Blocking reasons in the order the user can act on them. A blocked
        // state explains itself in the modal (ADR-045 UI rule 1) — never a
        // disabled button with no reason.
        let blocked_reason = if s.upgrade.in_flight() {
            format!(
                "an upgrade to {} is already in progress",
                if s.target_version.is_empty() {
                    "a new version".into()
                } else {
                    s.target_version.clone()
                }
            )
        } else if s.health != "green" {
            format!(
                "the cluster is {} — a rolling upgrade needs it green",
                s.health
            )
        } else if targets.is_empty() && !dashboards_behind {
            format!("{} is the newest version we know of", s.version)
        } else {
            String::new()
        };

        Ok(Json(UpgradeOptions {
            upgrade: upgrade_dto(&s.upgrade, s.nodes_updated, s.nodes_desired),
            suggested_version: s.suggested_version,
            version: s.version,
            dashboards_version: s.dashboards_version,
            targets,
            blocked_reason,
            dashboards_behind,
        }))
    }

    /// Start the upgrade. Returns as soon as the operator has accepted phase 1;
    /// progress is read back from the CR (`get_deployment` / the SSE stream).
    ///
    /// A failed pre-flight is a 400 with the reason — the CR is untouched.
    async fn upgrade_cluster(
        scope: Scope,
        Json(req): Json<UpgradeReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.name).await?;
        crate::k8s::upgrade_cluster(
            &dep,
            &req.version,
            req.allow_untested,
            req.confirm_unverified,
        )
        .await
        // The pre-flight's refusals ARE the user-facing message (invariant 4):
        // `{e:#}` keeps the context chain, so an operator string arrives whole.
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(StatusCode::OK)
    }

    // ──────────────── handlers: activity (ADR-050) ─────────────────────

    /// What the cluster is saying about this deployment right now — the
    /// "Detalhes" accordion. Read-only, no new RBAC: Kubernetes Events, pod
    /// container state and the operator's `componentsStatus`. Not container
    /// logs; the ADR explains why they would be both broader and less useful.
    async fn deployment_activity_log(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<Json<Vec<ActivityLine>>, ApiError> {
        let dep = scope.require(&req.name).await?;
        // Capped so a long-lived deployment cannot grow the response without
        // bound. This is a live window, not an audit trail — Kubernetes expires
        // its own events after an hour anyway.
        let lines = crate::k8s::activity_log(&dep, 60)
            .await
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(Json(
            lines
                .into_iter()
                .map(|l| ActivityLine {
                    at: l.at,
                    severity: l.severity.to_string(),
                    source: l.source.to_string(),
                    object: l.object,
                    title: l.title,
                    detail: l.detail,
                })
                .collect(),
        ))
    }

    // ──────────────── handlers: snapshots (ADR-049) ────────────────────

    /// The saved snapshot configuration of one deployment. Credentials come
    /// back as the `secret_kept` sentinel, never in clear.
    async fn snapshot_config(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<Json<SnapshotConfigDto>, ApiError> {
        let dep = scope.require(&req.name).await?;
        let (config, state, has_credentials) = crate::k8s::get_snapshot_config(&dep)
            .await
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(Json(SnapshotConfigDto {
            config,
            state: snapshot_dto(&state),
            has_credentials,
            deployment: req.name,
        }))
    }

    /// Dry-run: would saving this configuration restart the nodes, and would it
    /// be refused? Read-only — nothing is written, so the UI can ask before it
    /// shows the confirmation.
    async fn plan_snapshot_config(
        scope: Scope,
        Json(req): Json<SnapshotReq>,
    ) -> Result<Json<crate::k8s::SnapshotPlan>, ApiError> {
        let dep = scope.require(&req.name).await?;
        crate::k8s::plan_snapshot_config(&dep, &req.config)
            .await
            .map(Json)
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))
    }

    /// Write it. A failed pre-flight is a 400 whose body is the refusal itself
    /// — no Secret, no CR slice and no policy are left behind (invariant 3).
    async fn save_snapshot_config(
        scope: Scope,
        Json(req): Json<SnapshotReq>,
    ) -> Result<Json<crate::k8s::SnapshotPlan>, ApiError> {
        let dep = scope.require(&req.name).await?;
        crate::k8s::set_snapshot_config(&dep, req.config)
            .await
            .map(Json)
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))
    }

    /// Ask OpenSearch to reach the bucket from every node. Writes nothing —
    /// the analogue of `test_auth_provider`. The S3 reason on failure (403,
    /// wrong endpoint, missing bucket) is the whole value of this call, so it
    /// is passed through verbatim.
    async fn verify_snapshot_repo(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<Json<ProbeDto>, ApiError> {
        let dep = scope.require(&req.name).await?;
        match crate::k8s::verify_snapshot_repo(&dep).await {
            Ok(nodes) => Ok(Json(ProbeDto {
                ok: true,
                checks: vec![format!("nodes:{nodes}")],
                error: None,
            })),
            Err(e) => Ok(Json(ProbeDto {
                ok: false,
                checks: vec![],
                error: Some(format!("{e:#}")),
            })),
        }
    }

    // ───────────────────────── handlers: sizing (ADR-016) ──────────────

    /// The preset sizing tiers, resolved from `k8s::sizing()` (the single
    /// source). No args → GET. The wizard renders its size cards from this
    /// instead of a hardcoded list.
    async fn sizing_presets() -> Json<Vec<crate::k8s::SizingProfile>> {
        Json(crate::k8s::sizing_presets())
    }

    /// Resolve the wizard's custom-size inputs into a full profile — 3 nodes,
    /// heap auto-derived as half the memory (ADR-016). Has a body → POST.
    async fn custom_sizing(Json(req): Json<CustomSizingReq>) -> Json<crate::k8s::SizingProfile> {
        Json(crate::k8s::custom_sizing(
            Some(&req.memory),
            Some(&req.disk),
        ))
    }

    // ───────────────────────── handlers: recipes / monitoring ──────────

    async fn apply_recipe(
        scope: Scope,
        Json(req): Json<RecipeReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        crate::recipes::apply(&dep, &req.recipe)
            .await
            .map_err(ApiError::internal)?;
        crate::k8s::set_monitor(&dep, &req.recipe, true)
            .await
            .map_err(ApiError::internal)?;
        Ok(StatusCode::OK)
    }

    async fn disable_recipe(
        scope: Scope,
        Json(req): Json<RecipeReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        crate::recipes::disable(&dep, &req.recipe)
            .await
            .map_err(ApiError::internal)?;
        crate::k8s::set_monitor(&dep, &req.recipe, false)
            .await
            .map_err(ApiError::internal)?;
        Ok(StatusCode::OK)
    }

    // ─────────────────── handlers: OTel observability stack ────────────

    /// Static description of the stack: components, images, and the resource
    /// bill — everything the panel must state *before* the user clicks Install.
    async fn otel_stack_info() -> Json<OtelStackInfo> {
        use crate::otel_stack as os;
        let cost = os::resource_cost();
        let images = [
            ("cortex", os::CORTEX_IMAGE),
            ("alertmanager", os::AM_IMAGE),
            ("os-exporter", os::OSEXP_IMAGE),
            ("data-prepper", os::DP_IMAGE),
            ("collector", os::OTEL_IMAGE),
        ];
        Json(OtelStackInfo {
            version: os::STACK_VERSION.to_string(),
            components: images
                .iter()
                .map(|(k, i)| OtelComponentInfo {
                    key: k.to_string(),
                    image: i.to_string(),
                })
                .collect(),
            cpu_millis: cost.cpu_millis,
            mem_mib: cost.mem_mib,
            disk_gib: cost.disk_gib,
        })
    }

    async fn install_otel_stack(
        scope: Scope,
        Json(req): Json<OtelStackReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        let targets = crate::otel_stack::ScrapeTargets {
            kube_state_metrics: req.kube_state_metrics,
            node_exporter: req.node_exporter,
        };
        crate::otel_stack::install(&dep, targets)
            .await
            .map_err(ApiError::internal)?;
        Ok(StatusCode::OK)
    }

    async fn uninstall_otel_stack(
        scope: Scope,
        Json(req): Json<OtelStackReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        crate::otel_stack::uninstall(&dep, req.delete_indices)
            .await
            .map_err(ApiError::internal)?;
        Ok(StatusCode::OK)
    }

    /// Best-effort, like `node_stats`: a component still pulling its image is
    /// `ready: 0`, not a 5xx, so the panel keeps rendering while it starts.
    async fn otel_stack_status(
        scope: Scope,
        Json(req): Json<OtelStackReq>,
    ) -> Json<crate::otel_stack::StackState> {
        // Unowned reads exactly like a stack that is not installed — the same
        // degradation `node_stats` uses, so an unauthorised name is not even a
        // timing oracle.
        let Ok(dep) = scope.require(&req.deployment).await else {
            return Json(crate::otel_stack::StackState::default());
        };
        match crate::otel_stack::status(&dep).await {
            Ok(s) => Json(s),
            Err(e) => Json(crate::otel_stack::StackState {
                error: format!("{e:#}"),
                ..Default::default()
            }),
        }
    }

    /// The credential for the published OTLP / Alertmanager routes. Its own
    /// call, never a field on the status poll — same rule as the cluster admin
    /// password.
    ///
    /// ---
    /// Set (or clear) a deployment's IP allow-list for its public routes.
    ///
    /// An empty list is the DEFAULT and means open: a customer opts in to
    /// restriction. Validation happens server-side, so a typo in a security
    /// control fails at save time rather than quietly changing who gets in.
    async fn set_ip_allow_list(
        scope: Scope,
        Json(req): Json<IpAllowReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        let cidrs: Vec<String> = req
            .cidrs
            .iter()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
        crate::k8s::set_ip_allow_list(&dep, &cidrs)
            .await
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(StatusCode::OK)
    }

    /// Turn the next-generation UI on or off for one deployment.
    ///
    /// Always `chosen: true` here — this route only ever runs because a user
    /// asked. The observability stack calls `k8s::set_next_ui` directly with
    /// `chosen: false` when it needs workspaces for itself.
    ///
    /// Rolls the Dashboards pod (the operator does), and changes where saved
    /// objects are scoped: on = workspaces, off = the deployment's tenant. The
    /// UI states both consequences before asking.
    async fn set_next_ui(scope: Scope, Json(req): Json<NextUiReq>) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        crate::k8s::set_next_ui(&dep, req.enabled, true)
            .await
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        // Multi-tenancy is a consequence of workspaces, not a separate setting:
        // upstream requires it off when workspaces are on, and back on when
        // they are not, or saved objects land where nothing resolves them.
        crate::otel_stack::set_multitenancy(&dep, !req.enabled).await;
        Ok(StatusCode::OK)
    }

    /// Rotate the telemetry credential. Rolls the collector and Alertmanager
    /// pods, and breaks every exporter still holding the old password — which
    /// is why the UI confirms before calling this.
    async fn reset_otel_credentials(
        scope: Scope,
        Json(req): Json<OtelStackReq>,
    ) -> Result<Json<DashCreds>, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        let (username, password) = crate::otel_stack::reset_credentials(&dep)
            .await
            .map_err(ApiError::internal)?;
        Ok(Json(DashCreds { username, password }))
    }

    async fn otel_stack_credentials(
        scope: Scope,
        Json(req): Json<OtelStackReq>,
    ) -> Result<Json<DashCreds>, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        let (username, password) = crate::otel_stack::credentials(&dep)
            .await
            .map_err(ApiError::internal)?;
        Ok(Json(DashCreds { username, password }))
    }

    async fn monitoring_status(
        scope: Scope,
        Json(req): Json<RecipeReq>,
    ) -> Result<Json<MonitoringStatus>, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        let n = crate::recipes::doc_count(&dep, &req.recipe)
            .await
            .map_err(ApiError::internal)?;
        Ok(Json(MonitoringStatus {
            doc_count: n,
            receiving: n > 0,
        }))
    }

    // -- catalog routes (#75) -------------------------------------------
    // Thin shims only: the client, the trust boundary and the degraded-registry
    // policy all live in `crate::catalog` (ADR-039 / ADR-047).

    /// The Integrations tab's read (#76). ALWAYS 200 — an unreachable registry
    /// comes back as a stale/bootstrap catalog with `error` set, so the
    /// deployment screen never breaks because the registry is down.
    async fn catalog(
        scope: Scope,
        Json(req): Json<crate::catalog::CatalogReq>,
    ) -> Json<crate::catalog::CatalogView> {
        // The catalog itself is installation-wide (the registry's package
        // list), but the *installed* overlay is per deployment. A deployment
        // the caller does not own resolves to `None` and is answered exactly
        // like "no deployment given": the package list, nothing installed.
        let dep = match req.deployment.as_deref() {
            Some(name) => scope.resolve(name).await.unwrap_or(None),
            None => None,
        };
        Json(crate::catalog::view(dep.as_ref()).await)
    }

    /// Install an integration from the registry: download → verify signature →
    /// apply through the fixed engine → record `id@version` on the deployment.
    async fn catalog_install(
        scope: Scope,
        Json(req): Json<crate::catalog::CatalogInstallReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.deployment).await?;
        crate::catalog::install(&dep, &req.id, req.version.as_deref())
            .await
            .map_err(ApiError::internal)?;
        Ok(StatusCode::OK)
    }
    // -- end catalog routes (#75) ---------------------------------------

    // -- integrations tab support (#76) ---------------------------------

    /// Uninstall an integration: tear down exactly the package's own
    /// `teardown` set through the engine, then clear the monitor + version.
    /// NOT `disable_recipe` — that resolves its teardown from the in-binary
    /// recipe table, which defaults to nginx for a registry-only id.
    async fn catalog_uninstall(
        scope: Scope,
        Json(req): Json<crate::catalog::CatalogUninstallReq>,
    ) -> Result<StatusCode, ApiError> {
        // Resolve before mutate — the mirror of catalog_install. Teardown
        // DELETES objects out of a deployment's OpenSearch, so an unowned name
        // must 404 here for exactly the reason it does there (#80).
        let dep = scope.require(&req.deployment).await?;
        crate::catalog::uninstall(&dep, &req.id)
            .await
            .map_err(ApiError::internal)?;
        Ok(StatusCode::OK)
    }

    // -- end integrations tab support (#76) -----------------------------

    /// Live per-node stats for the Overview blocks (meeting 3). Best-effort:
    /// while a deployment is still provisioning OpenSearch isn't answering, so
    /// the caller treats an error as "no stats yet" and keeps the spinner.
    async fn node_stats(scope: Scope, Json(req): Json<NameReq>) -> Json<ClusterMetrics> {
        // A not-yet-green deployment has no metrics to report. Return an empty
        // snapshot (200) rather than a 5xx so the SPA's network panel stays
        // clean (issue #32 gate) — the metric bars just render zero until the
        // cluster comes up. A deployment the caller does not own takes the
        // same branch, so "not yours" is indistinguishable from "not ready".
        let Some(dep) = scope.resolve(&req.name).await.unwrap_or(None) else {
            return Json(ClusterMetrics::default());
        };
        Json(crate::metrics::node_stats(&dep).await.unwrap_or_default())
    }

    /// Recent cluster-health time-series for the Overview (#9). Like
    /// `node_stats`, best-effort: a not-yet-green deployment (or one whose
    /// `velox-metrics-*` index doesn't exist yet) has no series, so an error
    /// degrades to an empty 200 and the SPA shows "still collecting" instead of
    /// surfacing a console error (issue #32 gate).
    async fn metrics_series(scope: Scope, Json(req): Json<MetricSeriesReq>) -> Json<MetricSeries> {
        let window = req.window_minutes.unwrap_or(60);
        let buckets = req.buckets.unwrap_or(60);
        // Same degradation as `node_stats`: unowned reads like not-collecting.
        let Some(dep) = scope.resolve(&req.name).await.unwrap_or(None) else {
            return Json(MetricSeries::default());
        };
        Json(
            crate::metrics::series(&dep, window, buckets)
                .await
                .unwrap_or_default(),
        )
    }

    // ───────────────────────── handlers: access / security ─────────────

    /// Installation-wide dashboard-access configuration (ingress class, base
    /// domain, TLS Secret name). Admin-only: it is the operator's config, not a
    /// tenant's, and it names cluster-level objects.
    async fn get_access_settings(scope: Scope) -> Result<Json<AccessSettings>, ApiError> {
        scope.require_admin()?;
        let cfg = crate::access::get().await.map_err(ApiError::internal)?;
        let available_classes = crate::access::ingress_classes().await.unwrap_or_default();
        Ok(Json(AccessSettings {
            mode: cfg.mode,
            base_domain: cfg.base_domain,
            ingress_class: cfg.ingress_class,
            tls_secret: cfg.tls_secret,
            available_classes,
            default_base_domain: crate::access::default_base_domain()
                .await
                .unwrap_or_default(),
        }))
    }

    async fn save_access_settings(
        scope: Scope,
        Json(req): Json<AccessSettingsReq>,
    ) -> Result<StatusCode, ApiError> {
        scope.require_admin()?;
        // BYO TLS (issue #54): a pasted PEM pair becomes a kubernetes.io/tls
        // Secret the Ingresses reference. Cert and key only make sense together.
        let (tls_cert, tls_key) = (req.tls_cert.trim(), req.tls_key.trim());
        if tls_cert.is_empty() != tls_key.is_empty() {
            return Err(ApiError::bad_request(
                "TLS certificate and private key must be provided together",
            ));
        }
        let mut tls_secret = req.tls_secret.trim().to_string();
        if !tls_cert.is_empty() {
            if tls_secret.is_empty() {
                tls_secret = "veloxsearch-dashboards-tls".into();
            }
            // Malformed paste = client error; a failed Secret apply = ours.
            crate::k8s::validate_tls_pem(tls_cert, tls_key)
                .map_err(|e| ApiError::bad_request(e.to_string()))?;
            let client = crate::k8s::client().await.map_err(ApiError::internal)?;
            crate::k8s::ensure_tls_secret(&client, &tls_secret, tls_cert, tls_key)
                .await
                .map_err(ApiError::internal)?;
        }
        let cfg = crate::access::AccessConfig {
            mode: req.mode,
            base_domain: req.base_domain,
            ingress_class: req.ingress_class,
            tls_secret,
        };
        crate::access::set(&cfg).await.map_err(ApiError::internal)?;
        // Switching to ingress mode must cover deployments that already exist —
        // their status URLs flip immediately, so the Ingress objects must too.
        if cfg.ingress_enabled() {
            let client = crate::k8s::client().await.map_err(ApiError::internal)?;
            for dep in crate::k8s::scoped_deployments(&scope)
                .await
                .unwrap_or_default()
            {
                if let Err(e) = crate::k8s::ensure_opensearch_ingress(&client, &cfg, &dep).await {
                    tracing::warn!("opensearch ingress for {dep}: {e:#}");
                }
                if let Err(e) = crate::k8s::ensure_dashboards_ingress(&client, &cfg, &dep).await {
                    tracing::warn!("backfilling dashboards ingress for {dep}: {e:#}");
                }
            }
        }
        Ok(StatusCode::OK)
    }

    /// The OpenSearch Dashboards login for a deployment — the single most
    /// sensitive read in the API, and the one the acceptance test aims at: a
    /// caller who does not own the deployment gets 404, never a credential.
    async fn dashboard_credentials(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<Json<DashCreds>, ApiError> {
        let dep = scope.require(&req.name).await?;
        let (username, password) = crate::k8s::dashboard_credentials(&dep)
            .await
            .map_err(ApiError::internal)?;
        Ok(Json(DashCreds { username, password }))
    }

    /// Current auth provider of a deployment, with credentials redacted, plus
    /// the constraints the screen needs (ADR-045).
    async fn auth_provider(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<Json<AuthProviderDto>, ApiError> {
        let dep = scope.require(&req.name).await?;
        let spec = crate::k8s::get_auth_provider(&dep)
            .await
            .map_err(ApiError::internal)?;
        let access = crate::access::get().await.map_err(ApiError::internal)?;
        let public_url =
            crate::auth_provider::public_url(dep.name(), &access.mode, &access.base_domain);

        // Redirect flows need a stable HTTPS origin — say so up front rather
        // than letting the user fill a form that cannot be saved.
        let redirect_ok = public_url.is_some();
        let mut kinds_available = vec!["internal", "ldap"];
        if redirect_ok {
            kinds_available.push("oidc");
            kinds_available.push("saml");
        }
        kinds_available.push("jwt");
        kinds_available.push("proxy");

        Ok(Json(AuthProviderDto {
            spec: crate::auth_provider::redacted(&spec),
            public_url,
            kinds_available,
            redirect_blocked_reason: (!redirect_ok).then(|| {
                "Single sign-on sends the browser back to this deployment, so it needs a public \
                 HTTPS address. Set a base domain under Settings → dashboard access first."
                    .to_string()
            }),
            builtin_roles: crate::auth_provider::BUILTIN_ROLES.to_vec(),
            break_glass_user: crate::k8s::ADMIN_USER.to_string(),
            secret_kept: crate::auth_provider::SECRET_KEPT,
        }))
    }

    /// Apply (or remove, with `kind: "internal"`) a deployment's auth provider.
    /// Rejections from the generator are user errors, not server faults.
    async fn save_auth_provider(
        scope: Scope,
        Json(req): Json<AuthProviderReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.name).await?;
        crate::k8s::set_auth_provider(&dep, req.spec)
            .await
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(StatusCode::OK)
    }

    /// Reachability probe for a provider the user is about to save. Never
    /// writes: a failed probe leaves the deployment exactly as it was.
    async fn test_auth_provider(
        scope: Scope,
        Json(req): Json<AuthProviderReq>,
    ) -> Result<Json<ProbeDto>, ApiError> {
        // Scoped even though it only probes: it resolves `SECRET_KEPT` against
        // the STORED credential, so an unscoped probe would be a read oracle
        // for another tenant's bind password.
        let dep = scope.require(&req.name).await?;
        let stored = crate::k8s::get_auth_provider(&dep)
            .await
            .unwrap_or_default();
        let spec = crate::auth_provider::merge_secrets(&req.spec, &stored);
        let r = crate::auth_probe::probe(&spec).await;
        Ok(Json(ProbeDto {
            ok: r.ok,
            checks: r.checks,
            error: r.error,
        }))
    }

    /// Reset a deployment's OpenSearch admin password (Security tab).
    /// Reset the cluster admin password to a generated one and hand it back.
    ///
    /// The only reset path the UI offers. There is no "choose your own": for a
    /// machine account a typed password is weaker in practice than a generated
    /// one, and having a single path means the strength rules cannot be
    /// bypassed by a client that skips the form.
    async fn reset_admin_password_random(
        scope: Scope,
        Json(req): Json<NameReq>,
    ) -> Result<Json<DashCreds>, ApiError> {
        let dep = scope.require(&req.name).await?;
        let password = crate::k8s::reset_admin_password_random(&dep)
            .await
            .map_err(ApiError::internal)?;
        let (username, _) = crate::k8s::admin_creds(&dep).await;
        Ok(Json(DashCreds { username, password }))
    }

    async fn reset_admin_password(
        scope: Scope,
        Json(req): Json<ResetPasswordReq>,
    ) -> Result<StatusCode, ApiError> {
        let dep = scope.require(&req.name).await?;
        crate::k8s::reset_admin_password(&dep, &req.new_password)
            .await
            .map_err(ApiError::internal)?;
        Ok(StatusCode::OK)
    }

    // ───────────────────────── SSE status stream (ADR-005) ─────────────

    /// Streams the full deployment-status snapshot every 3s as SSE. The browser
    /// holds one EventSource instead of hammering the list endpoint; the backend
    /// keeps polling K8s (detection is fundamentally poll, see ADR-005). Sits
    /// behind the auth middleware like every other `/api` route.
    pub fn sse_events(
        scope: Scope,
    ) -> impl std::future::Future<
        Output = axum::response::sse::Sse<
            impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
        >,
    > + Send {
        use axum::response::sse::{Event, KeepAlive, Sse};
        async move {
            // The scope is captured ONCE, from the cookie that opened the
            // stream, and every tick re-lists through it — a long-lived stream
            // is scoped exactly like a one-shot read.
            let stream = futures::stream::unfold((0u64, scope), |(i, scope)| async move {
                if i > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                let ev = match crate::k8s::list_deployments(&scope).await {
                    Ok(v) => {
                        let dtos: Vec<ClusterStatus> = v.into_iter().map(to_dto).collect();
                        Event::default()
                            .data(serde_json::to_string(&dtos).unwrap_or_else(|_| "[]".into()))
                    }
                    // Comment frames are ignored by EventSource — keeps the pipe
                    // warm through transient K8s errors instead of killing it.
                    Err(e) => Event::default().comment(e.to_string()),
                };
                Some((Ok::<_, std::convert::Infallible>(ev), (i + 1, scope)))
            });
            Sse::new(stream).keep_alive(KeepAlive::default())
        }
    }

    // ───────────────────────── router ──────────────────────────────────

    /// The ownership rule a route carries. There is no default and no
    /// "unreviewed" variant: [`ROUTES`] must name every mounted path, and
    /// [`p`] refuses to mount one that is missing — so adding a route without
    /// deciding how it is scoped panics at startup and reddens the tests,
    /// instead of shipping a quiet hole (#80's whole point).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Policy {
        /// Reachable with no session at all (the auth guard allowlists it).
        Public,
        /// Needs a session; touches nothing that belongs to a tenant.
        Authenticated,
        /// Installation-level. Tenants get the 404 of `Scope::require_admin`.
        AdminOnly,
        /// Names or lists deployment state. Every access goes through
        /// `Scope::resolve`/`require`, i.e. the caller's namespace + owner
        /// label; unowned reads as not-found.
        TenantScoped,
    }

    /// One row per `/api` route: the audit table for the M3 security review
    /// (#87/#100). `note` says WHY, so a reviewer can check the decision rather
    /// than just the mechanism.
    pub struct RoutePolicy {
        pub path: &'static str,
        pub policy: Policy,
        pub note: &'static str,
    }

    use Policy::{AdminOnly, Authenticated, Public, TenantScoped};

    pub const ROUTES: &[RoutePolicy] = &[
        // -- auth / account ------------------------------------------------
        RoutePolicy { path: "/auth_state", policy: Public, note: "the SPA's pre-login probe: first-run state + whether THIS cookie is valid. Returns the caller's own identity and nothing else." },
        RoutePolicy { path: "/login", policy: Public, note: "credential exchange; mints the session the whole scope layer reads." },
        RoutePolicy { path: "/logout", policy: Authenticated, note: "behind the auth guard like every other /api route; clears the caller's OWN cookie and touches nothing else." },
        RoutePolicy { path: "/setup_admin", policy: Public, note: "pre-session by necessity, but reachable ONLY in first-run mode (auth::is_setup) and refused once an admin exists (auth::complete_setup)." },
        RoutePolicy { path: "/signup", policy: Public, note: "#79 self-serve; 404 unless VELOX_MULTITENANT_AUTH. Enumeration-safe by design." },
        RoutePolicy { path: "/verify_email", policy: Public, note: "#79; single-use token IS the authorization." },
        RoutePolicy { path: "/request_password_reset", policy: Public, note: "#79; answers 202 whether or not the account exists." },
        RoutePolicy { path: "/reset_password", policy: Public, note: "#79; single-use token IS the authorization." },
        // -- installation-level --------------------------------------------
        RoutePolicy { path: "/bootstrap_status", policy: AdminOnly, note: "operator/cert-manager/Longhorn state of the HOST cluster." },
        RoutePolicy { path: "/bootstrap_ensure", policy: AdminOnly, note: "installs cluster-wide components; never a tenant action." },
        RoutePolicy { path: "/storage_status", policy: AdminOnly, note: "host StorageClass classification (ADR-043)." },
        RoutePolicy { path: "/discover", policy: AdminOnly, note: "enumerates workloads across host namespaces — a window onto other tenants." },
        RoutePolicy { path: "/cluster_capacity", policy: AdminOnly, note: "host node CPU/mem/disk; aggregate info about everyone's footprint. A tenant's headroom is its quota (#84)." },
        RoutePolicy { path: "/access_settings", policy: AdminOnly, note: "installation ingress class / base domain / TLS Secret name." },
        RoutePolicy { path: "/save_access_settings", policy: AdminOnly, note: "writes that config and backfills Ingresses; admin-scoped backfill." },
        // -- deployment surface --------------------------------------------
        RoutePolicy { path: "/list_deployments", policy: TenantScoped, note: "namespace + owner-label filtered (k8s::list_clusters)." },
        RoutePolicy { path: "/get_deployment", policy: TenantScoped, note: "unowned answers null, identically to nonexistent." },
        RoutePolicy { path: "/delete_cluster", policy: TenantScoped, note: "resolve-before-mutate; unowned/unknown → 404." },
        RoutePolicy { path: "/create_cluster", policy: TenantScoped, note: "claims a name INTO the caller's namespace with the caller's owner label." },
        RoutePolicy { path: "/save_cluster", policy: TenantScoped, note: "edits an existing CR — must resolve first, or a save is a cross-tenant create." },
        RoutePolicy { path: "/retry_provisioning", policy: TenantScoped, note: "re-runs the deferred profile + monitors on an existing CR (ADR-052) — writes into a deployment, so it resolves first." },
        RoutePolicy { path: "/apply_recipe", policy: TenantScoped, note: "writes pipelines/dashboards INTO the deployment's OpenSearch." },
        RoutePolicy { path: "/disable_recipe", policy: TenantScoped, note: "removes them; same write surface." },
        RoutePolicy { path: "/monitoring_status", policy: TenantScoped, note: "doc counts are data about the deployment's contents." },
        RoutePolicy { path: "/catalog", policy: TenantScoped, note: "package list is installation-wide; the installed overlay is per deployment, and an unowned name reads as no-deployment." },
        RoutePolicy { path: "/catalog_install", policy: TenantScoped, note: "applies a package to a deployment; resolve-before-mutate." },
        RoutePolicy { path: "/catalog_uninstall", policy: TenantScoped, note: "DELETES the package's teardown set from the deployment's OpenSearch; resolve-before-mutate, same surface as catalog_install." },
        RoutePolicy { path: "/node_stats", policy: TenantScoped, note: "live cluster stats; unowned degrades to the empty snapshot." },
        RoutePolicy { path: "/metrics_series", policy: TenantScoped, note: "historical series out of the deployment's own index." },
        RoutePolicy { path: "/dashboard_credentials", policy: TenantScoped, note: "THE credential read — unowned is 404, never a password." },
        RoutePolicy { path: "/reset_admin_password", policy: TenantScoped, note: "rewrites the deployment's admin Secret." },
        RoutePolicy { path: "/auth_provider", policy: TenantScoped, note: "reads the deployment's IdP config (redacted, but still its shape)." },
        RoutePolicy { path: "/save_auth_provider", policy: TenantScoped, note: "replaces the deployment's security configuration." },
        RoutePolicy { path: "/test_auth_provider", policy: TenantScoped, note: "probes with STORED credentials when the form echoes SECRET_KEPT — an unscoped probe would be a read oracle." },
        RoutePolicy { path: "/events", policy: TenantScoped, note: "SSE re-lists through the scope captured when the stream opened." },
        RoutePolicy { path: "/upgrade_options", policy: TenantScoped, note: "reads the deployment's running version + upgrade state off its CR (ADR-048)." },
        RoutePolicy { path: "/upgrade_cluster", policy: TenantScoped, note: "patches spec.general.version — an irreversible write; resolve-before-mutate." },
        RoutePolicy { path: "/snapshot_config", policy: TenantScoped, note: "the deployment's backup target: bucket, endpoint, schedule (ADR-049)." },
        RoutePolicy { path: "/plan_snapshot_config", policy: TenantScoped, note: "dry run over the STORED config — unscoped it would leak whether a bucket is set." },
        RoutePolicy { path: "/save_snapshot_config", policy: TenantScoped, note: "writes the repository slice, its Secret and the policy CR; resolve-before-mutate." },
        RoutePolicy { path: "/verify_snapshot_repo", policy: TenantScoped, note: "makes the deployment's nodes reach the bucket with its stored credentials." },
        RoutePolicy { path: "/deployment_activity_log", policy: TenantScoped, note: "Events, pod state and componentsStatus OF that deployment (ADR-050)." },
        // -- OTel observability stack (ADR-053) ---------------------------
        RoutePolicy { path: "/otel_stack_info", policy: Authenticated, note: "static component/image/resource-cost table out of the binary (otel_stack::resource_cost) — names no deployment and reads no cluster." },
        RoutePolicy { path: "/install_otel_stack", policy: TenantScoped, note: "creates the stack's 17 objects in the deployment's namespace and writes its OpenSearch; resolve-before-mutate." },
        RoutePolicy { path: "/uninstall_otel_stack", policy: TenantScoped, note: "deletes those objects and optionally the indices — the destructive half of the same surface." },
        RoutePolicy { path: "/otel_stack_status", policy: TenantScoped, note: "per-component readiness of THAT deployment's stack; unowned degrades to the empty state, never a 5xx." },
        RoutePolicy { path: "/otel_stack_credentials", policy: TenantScoped, note: "THE telemetry credential read — unowned is 404, never a password. Same rule as /dashboard_credentials." },
        RoutePolicy { path: "/reset_otel_credentials", policy: TenantScoped, note: "rotates that credential and rolls the pods holding it; a write to the deployment." },
        RoutePolicy { path: "/set_next_ui", policy: TenantScoped, note: "flips workspaces on the deployment's Dashboards and moves where its saved objects are scoped; a write to that deployment, so resolve-before-mutate." },
        // -- public per-deployment routes (ADR-053) ------------------------
        RoutePolicy { path: "/set_ip_allow_list", policy: TenantScoped, note: "writes the deployment's own network allow-list — a security control, so an unowned name must 404 rather than widen someone else's exposure." },
        RoutePolicy { path: "/reset_admin_password_random", policy: TenantScoped, note: "rewrites the deployment's admin Secret with a generated value; same surface as /reset_admin_password." },
        // -- stateless helpers ---------------------------------------------
        RoutePolicy { path: "/sizing_presets", policy: Authenticated, note: "static preset table (k8s::sizing) — no cluster read." },
        RoutePolicy { path: "/custom_sizing", policy: Authenticated, note: "pure arithmetic on the submitted numbers." },
        RoutePolicy { path: "/available_versions", policy: Authenticated, note: "the pinned catalog + the hourly upstream check — installation-wide data, no cluster read (ADR-048 rev. 2)." },
    ];

    /// Mount-time gate: a path with no [`ROUTES`] entry cannot be mounted.
    ///
    /// A panic here is deliberate. `routes()` runs at process start and in
    /// every test that builds the router, so "someone added a route and forgot
    /// to decide its scoping" is a crash on the first boot, not a finding in
    /// the next security review.
    fn p(path: &'static str) -> &'static str {
        assert!(
            ROUTES.iter().any(|r| r.path == path),
            "route {path} has no entry in api::ROUTES — declare its ownership \
             policy before mounting it (#80)",
        );
        path
    }

    /// All JSON endpoints, to be mounted under `/api` by `main`.
    pub fn routes() -> Router {
        Router::new()
            .route(p("/auth_state"), get(auth_state))
            .route(p("/login"), post(login))
            .route(p("/logout"), post(logout))
            .route(p("/setup_admin"), post(setup_admin))
            // -- tenant auth routes (#79)
            .route(p("/signup"), post(signup))
            .route(p("/verify_email"), post(verify_email))
            .route(p("/request_password_reset"), post(request_password_reset))
            .route(p("/reset_password"), post(reset_password))
            // -- end tenant auth routes (#79)
            .route(p("/bootstrap_status"), get(bootstrap_status))
            .route(p("/bootstrap_ensure"), post(bootstrap_ensure))
            .route(p("/storage_status"), get(storage_status))
            .route(p("/discover"), get(discover))
            .route(p("/list_deployments"), get(list_deployments))
            .route(p("/cluster_capacity"), get(cluster_capacity))
            .route(p("/get_deployment"), post(get_deployment))
            .route(p("/delete_cluster"), post(delete_cluster))
            .route(p("/create_cluster"), post(create_cluster))
            .route(p("/save_cluster"), post(save_cluster))
            .route(p("/retry_provisioning"), post(retry_provisioning))
            // -- version upgrade (ADR-048, #109)
            .route(p("/available_versions"), get(available_versions))
            .route(p("/upgrade_options"), post(upgrade_options))
            .route(p("/upgrade_cluster"), post(upgrade_cluster))
            .route(p("/sizing_presets"), get(sizing_presets))
            .route(p("/custom_sizing"), post(custom_sizing))
            .route(p("/apply_recipe"), post(apply_recipe))
            .route(p("/disable_recipe"), post(disable_recipe))
            .route(p("/monitoring_status"), post(monitoring_status))
            // -- catalog routes (#75)
            .route(p("/catalog"), post(catalog))
            .route(p("/catalog_install"), post(catalog_install))
            // -- integrations tab support (#76)
            .route(p("/catalog_uninstall"), post(catalog_uninstall))
            .route(p("/node_stats"), post(node_stats))
            .route(p("/metrics_series"), post(metrics_series))
            .route(p("/access_settings"), get(get_access_settings))
            .route(p("/save_access_settings"), post(save_access_settings))
            .route(p("/dashboard_credentials"), post(dashboard_credentials))
            .route(p("/reset_admin_password"), post(reset_admin_password))
            .route(p("/auth_provider"), post(auth_provider))
            .route(p("/save_auth_provider"), post(save_auth_provider))
            .route(p("/test_auth_provider"), post(test_auth_provider))
            // -- snapshot repository + policy (ADR-049, #52)
            .route(p("/snapshot_config"), post(snapshot_config))
            .route(p("/plan_snapshot_config"), post(plan_snapshot_config))
            .route(p("/save_snapshot_config"), post(save_snapshot_config))
            .route(p("/verify_snapshot_repo"), post(verify_snapshot_repo))
            // -- deployment activity (ADR-050)
            .route(p("/deployment_activity_log"), post(deployment_activity_log))
            // -- OTel observability stack (ADR-053)
            .route(p("/otel_stack_info"), get(otel_stack_info))
            .route(p("/install_otel_stack"), post(install_otel_stack))
            .route(p("/uninstall_otel_stack"), post(uninstall_otel_stack))
            .route(p("/otel_stack_status"), post(otel_stack_status))
            .route(p("/otel_stack_credentials"), post(otel_stack_credentials))
            .route(p("/reset_otel_credentials"), post(reset_otel_credentials))
            // -- next-generation Dashboards UI, a per-deployment opt-in (ADR-053 rev. 9-10)
            .route(p("/set_next_ui"), post(set_next_ui))
            // -- public per-deployment routes (ADR-053)
            .route(p("/set_ip_allow_list"), post(set_ip_allow_list))
            .route(
                p("/reset_admin_password_random"),
                post(reset_admin_password_random),
            )
            .route(p("/events"), get(sse_events))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// #53: an observability/security deployment created with no integration
        /// selected still ships the always-on baseline collector, so its Fluent
        /// Bit agent is deployed from creation rather than left for a manual click.
        #[test]
        fn empty_selection_seeds_baseline_for_non_search() {
            for purpose in ["observability", "security"] {
                assert_eq!(
                    with_baseline_monitors(vec![], purpose),
                    vec![BASELINE_MONITOR.to_string()],
                    "{purpose} with no selection must seed the baseline collector",
                );
            }
        }

        /// An explicit selection is respected verbatim — the baseline is a floor,
        /// not an override (never steals the per-deployment×recipe choice).
        #[test]
        fn explicit_selection_is_left_untouched() {
            let chosen = vec!["nginx".to_string(), "postgres".to_string()];
            assert_eq!(
                with_baseline_monitors(chosen.clone(), "observability"),
                chosen,
            );
        }

        /// `search` installs no agents (ADR-028): an empty selection stays empty.
        #[test]
        fn search_never_seeds_an_agent() {
            assert!(with_baseline_monitors(vec![], "search").is_empty());
        }

        fn overrides(nodes: &str) -> Result<crate::k8s::CreateOverrides, String> {
            parse_overrides(
                nodes.to_string(),
                String::new(),
                String::new(),
                String::new(),
                None,
            )
        }

        /// #52: an empty node count means "keep the preset"; a number is used;
        /// garbage is a refusal — NOT a silent fallback that would reset an
        /// edited deployment's node count to its preset.
        #[test]
        fn node_count_parses_or_refuses() {
            assert_eq!(overrides("").unwrap().replicas, None);
            assert_eq!(overrides(" 4 ").unwrap().replicas, Some(4));
            let err = overrides("abc").unwrap_err();
            assert!(err.contains("abc"), "names the bad value: {err}");
            assert!(overrides("-1").is_err(), "negative counts refused");
            assert!(overrides("3.5").is_err(), "fractional counts refused");
        }
    }
}

// `parse_overrides`/`bootstrap_dto` are re-exported only so the transitional
// Leptos `#[server]` fns in `app.rs` can borrow them until `app.rs` is deleted
// (#26); `routes`/`sse_events`/`to_dto` are the lasting public surface.
#[cfg(feature = "ssr")]
pub use server::{
    bootstrap_dto, parse_overrides, routes, sse_events, to_dto, Policy, RoutePolicy, ROUTES,
};
