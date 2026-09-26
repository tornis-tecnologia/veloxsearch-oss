// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The cluster profile (ADR-060): one bounded, derived document that says how
//! big an installation is, what shape it has and how loaded it has been — the
//! read-only capacity export behind `GET /api/cluster_profile`.
//!
//! Two halves, split the way `activity::evaluate` and `Scope::adopt` are:
//!
//! * **Deciding — [`build`], pure.** Takes [`ProfileInputs`] (raw facts,
//!   including the raw OpenSearch responses, names and all) and a
//!   [`Redaction`], and returns a [`ClusterProfile`] built from this module's
//!   own DTOs. It never serializes a UI DTO (`ClusterStatus`,
//!   `ClusterCapacity`, `NodeStat`): a field added to one of those must not
//!   silently appear in an export.
//! * **Gathering — [`gather`], async.** Reads the existing sources and fills
//!   the inputs. It talks to the Kubernetes API and each deployment's own
//!   OpenSearch and to nothing else — there is no outbound path, no push and
//!   no beacon (ADR-060 rule 8).
//!
//! What may appear is fixed by [`FIELDS`], one row per JSON leaf with the
//! reason it is safe; the exact-set and canary tests below hold the builder to
//! it in both directions.

use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde_json::Value;
use sha2::Sha256;
use std::collections::{BTreeMap, BTreeSet};

use crate::api::ClusterCapacity;
use crate::bootstrap::DeploymentStorage;
use crate::metrics::{counter_rate, downsample, sample_from_nodes_stats, Sample, STALL_KIND};

/// Bumped on a removal, rename or unit change; additive fields keep it.
pub const SCHEMA_VERSION: u32 = 1;
/// Fixed `date_histogram` bucket of the window (≤ 2,016 buckets over 7 days).
pub const WINDOW_BUCKET_SECS: i64 = 300;
/// How far back the window reads — the metrics index's own retention.
const WINDOW_DAYS: i64 = 7;
/// Domain separator of every pseudonym, versioned with the schema.
const PSEUDONYM_DOMAIN: &str = "velox-profile:v1:";
/// Hex characters kept from the HMAC (48 bits) — unique across any real
/// installation, still unguessable without the key.
const PSEUDONYM_HEX: usize = 12;
/// A "now" rate measured against a recorded sample older than this is no
/// longer "now" and is reported as unknown.
const NOW_RATE_MAX_AGE_MS: i64 = 15 * 60 * 1000;
/// Most stall episodes one deployment reports (newest kept).
const MAX_STALLS: usize = 50;

// ───────────────────────────── the document ─────────────────────────────

/// The whole export. Keys serialize in declaration order and every array is
/// sorted, so two profiles of the same installation diff field by field.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClusterProfile {
    pub schema_version: u32,
    /// RFC 3339, seconds, UTC.
    pub generated_at: String,
    /// `installation` (the admin) or `tenant`.
    pub scope: &'static str,
    /// Whether `?names=true` added real deployment / index-family names.
    pub names_included: bool,
    pub build: BuildFacts,
    /// Installation sections: present only for the admin scope — absent, not
    /// `null`, in a tenant's document (ADR-060 decision 5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubernetes: Option<KubernetesFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<HostNode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit: Option<Vec<Fit>>,
    /// A tenant's headroom: its ADR-041 quota row. Tenant documents only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<Quota>,
    pub deployments: Vec<DeploymentProfile>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BuildFacts {
    pub version: String,
    pub commit: String,
    /// `sha256:<64 hex>` of the running image; admin only, else `null`.
    pub image_digest: Option<String>,
    /// The operator image's tag — never the registry or repository path.
    pub operator_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KubernetesFacts {
    pub version: Option<String>,
    /// `k3s` | `rke2` | `eks` | `gke` | `unknown`.
    pub distribution: &'static str,
}

/// One host node. Always pseudonymous: node names are hostnames, an excluded
/// class even on opt-in.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HostNode {
    pub id: String,
    /// Closed set: `control-plane` | `etcd` | `worker` | `other`.
    pub roles: Vec<&'static str>,
    pub ready: bool,
    /// Closed set: `MemoryPressure` | `DiskPressure` | `PIDPressure`.
    pub pressures: Vec<&'static str>,
    pub kernel: Option<String>,
    pub cpu_millis: Allocation,
    pub memory_bytes: Allocation,
    pub host_disk_bytes: Option<Usage>,
    pub storage_bytes: Option<Usage>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Allocation {
    pub allocatable: u64,
    pub requested: Option<u64>,
    pub used: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Usage {
    pub total: u64,
    pub used: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StorageFacts {
    /// `longhorn` | `foreign_default` | `node_local` | `absent` | `unknown` —
    /// the ADR-061 classification, never the StorageClass name.
    pub class: &'static str,
    pub longhorn_replicas: Option<u32>,
    pub pool_bytes: Option<Pool>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pool {
    pub total: u64,
    pub used: u64,
    pub available: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Fit {
    pub size: &'static str,
    pub count: u64,
    /// `cpu` | `mem` | `disk`.
    pub limited_by: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Quota {
    pub max_deployments: i64,
    pub max_total_disk_bytes: u64,
    pub max_nodes: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeploymentProfile {
    pub id: String,
    /// The real deployment name — only with `?names=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Owner tenant's pseudonym; `null` for an admin-namespace deployment.
    pub tenant: Option<String>,
    /// The tenant slug — only for the admin, and only with `?names=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_slug: Option<String>,
    /// `small` | `medium` | `large` | `custom`.
    pub size: &'static str,
    /// `preset` when memory, disk and node count equal the size's preset.
    pub sizing: &'static str,
    /// `observability` | `security` | `search` | `other`.
    pub purpose: &'static str,
    pub opensearch_version: Option<String>,
    pub target_version: Option<String>,
    pub nodes: i64,
    pub memory_limit_bytes: Option<u64>,
    pub heap_declared_bytes: Option<u64>,
    pub heap_max_bytes_observed: Option<u64>,
    pub disk_per_node_bytes: Option<u64>,
    /// `green` | `yellow` | `red` | `unknown`.
    pub health: &'static str,
    pub settled: bool,
    pub indices: Option<Indices>,
    pub shards: Option<Shards>,
    pub store_bytes: Option<u64>,
    pub docs: Option<u64>,
    pub disk_watermark_headroom_bytes: Option<Watermarks>,
    pub restarts: Option<Restarts>,
    pub now: Option<Now>,
    pub window: Window,
    pub stalls: Vec<Stall>,
    pub integrations: Vec<Integration>,
    pub warnings: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Indices {
    pub total: u64,
    /// Distinct names once rollover and date suffixes are stripped.
    pub families: u64,
    pub by_class: IndexClasses,
    /// The family names themselves — only with `?names=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family_names: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IndexClasses {
    pub system: u64,
    pub managed: u64,
    pub user: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Shards {
    pub primaries: u64,
    pub total: u64,
    pub unassigned: u64,
    pub largest_primary_bytes: u64,
}

/// Free bytes above each disk watermark on the tightest node. Negative means
/// that watermark is already crossed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Watermarks {
    pub low: i64,
    pub high: i64,
    pub flood_stage: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Restarts {
    pub node_containers: i64,
    pub dashboards: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Now {
    pub cpu_percent: f64,
    pub heap_percent: f64,
    pub indexing_per_sec: Option<f64>,
    pub query_per_sec: Option<f64>,
    pub gc_old_millis_per_min: Option<f64>,
    pub gc_young_millis_per_min: Option<f64>,
}

/// Workload over what `velox-metrics-<deployment>` actually retains. The
/// coverage fields say how much that was; nothing is extrapolated.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Window {
    pub start: Option<String>,
    pub end: Option<String>,
    pub samples: u64,
    pub bucket_secs: i64,
    pub cpu_percent: Option<Percentiles>,
    pub heap_percent: Option<Percentiles>,
    pub indexing_per_sec: Option<Percentiles>,
    pub query_per_sec: Option<Percentiles>,
    pub gc_old_millis_per_min: Option<Percentiles>,
    pub gc_young_millis_per_min: Option<Percentiles>,
    pub disk_used_bytes: Option<FirstLast>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FirstLast {
    pub first: u64,
    pub last: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stall {
    pub at: String,
    /// The ADR-050 ladder rung.
    pub stage: &'static str,
    pub component: Option<String>,
    pub component_status: Option<String>,
    pub recovery_stage: Option<String>,
    /// `system` | `managed` | `user` — never the index name.
    pub recovery_index_class: Option<&'static str>,
    pub secs: i64,
    /// `node_bounce` | `dashboards_reset`.
    pub remediation: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Integration {
    pub id: String,
    pub version: String,
}

// ───────────────────────────── the allowlist ─────────────────────────────

/// One exportable leaf and why it is safe (the `api::ROUTES` pattern).
pub struct Field {
    pub path: &'static str,
    pub note: &'static str,
}

const fn f(path: &'static str, note: &'static str) -> Field {
    Field { path, note }
}

/// Every JSON leaf a profile can carry. Adding a fact is a one-line,
/// reviewable diff here; `exact_set` fails on anything emitted and unlisted,
/// and on anything listed and never emitted.
pub const FIELDS: &[Field] = &[
    f("/schema_version", "format version, a constant"),
    f("/generated_at", "when the export was made"),
    f("/scope", "closed set: installation | tenant"),
    f("/names_included", "which redaction mode produced the file"),
    f("/build/version", "compiled-in crate version"),
    f(
        "/build/commit",
        "compiled-in git sha of a public repository",
    ),
    f(
        "/build/image_digest",
        "content hash of a public image; admin only",
    ),
    f(
        "/build/operator_version",
        "operator image tag only, registry path dropped; admin only",
    ),
    f(
        "/kubernetes/version",
        "API server gitVersion, version charset only",
    ),
    f(
        "/kubernetes/distribution",
        "closed set parsed from the version",
    ),
    f("/nodes/[]/id", "keyed pseudonym; node names are hostnames"),
    f("/nodes/[]/roles/[]", "closed set of role codes"),
    f("/nodes/[]/ready", "node condition"),
    f("/nodes/[]/pressures/[]", "closed set of condition types"),
    f("/nodes/[]/kernel", "kernel release, version charset only"),
    f("/nodes/[]/cpu_millis/allocatable", "quantity"),
    f("/nodes/[]/cpu_millis/requested", "quantity"),
    f("/nodes/[]/cpu_millis/used", "quantity"),
    f("/nodes/[]/memory_bytes/allocatable", "quantity"),
    f("/nodes/[]/memory_bytes/requested", "quantity"),
    f("/nodes/[]/memory_bytes/used", "quantity"),
    f("/nodes/[]/host_disk_bytes/total", "quantity"),
    f("/nodes/[]/host_disk_bytes/used", "quantity"),
    f("/nodes/[]/storage_bytes/total", "quantity"),
    f("/nodes/[]/storage_bytes/used", "quantity"),
    f(
        "/storage/class",
        "closed ADR-061 classification, never the class name",
    ),
    f("/storage/longhorn_replicas", "a count"),
    f("/storage/pool_bytes/total", "quantity"),
    f("/storage/pool_bytes/used", "quantity"),
    f("/storage/pool_bytes/available", "quantity"),
    f("/fit/[]/size", "closed preset set"),
    f("/fit/[]/count", "a count"),
    f("/fit/[]/limited_by", "closed set: cpu | mem | disk"),
    f("/quota/max_deployments", "the tenant's own quota"),
    f("/quota/max_total_disk_bytes", "the tenant's own quota"),
    f("/quota/max_nodes", "the tenant's own quota"),
    f("/deployments/[]/id", "keyed pseudonym of namespace/name"),
    f("/deployments/[]/name", "opt-in only (?names=true)"),
    f("/deployments/[]/tenant", "keyed pseudonym of the tenant id"),
    f("/deployments/[]/tenant_slug", "opt-in AND admin only"),
    f("/deployments/[]/size", "closed preset set"),
    f("/deployments/[]/sizing", "closed set: preset | custom"),
    f("/deployments/[]/purpose", "closed purpose set"),
    f("/deployments/[]/opensearch_version", "version charset only"),
    f("/deployments/[]/target_version", "version charset only"),
    f("/deployments/[]/nodes", "a count"),
    f("/deployments/[]/memory_limit_bytes", "quantity"),
    f("/deployments/[]/heap_declared_bytes", "quantity"),
    f("/deployments/[]/heap_max_bytes_observed", "quantity"),
    f("/deployments/[]/disk_per_node_bytes", "quantity"),
    f("/deployments/[]/health", "closed colour set"),
    f("/deployments/[]/settled", "ADR-050 predicate"),
    f(
        "/deployments/[]/indices/total",
        "a count; names reduced away",
    ),
    f(
        "/deployments/[]/indices/families",
        "a count; names reduced away",
    ),
    f("/deployments/[]/indices/by_class/system", "a count"),
    f("/deployments/[]/indices/by_class/managed", "a count"),
    f("/deployments/[]/indices/by_class/user", "a count"),
    f(
        "/deployments/[]/indices/family_names/[]",
        "opt-in only (?names=true)",
    ),
    f("/deployments/[]/shards/primaries", "a count"),
    f("/deployments/[]/shards/total", "a count"),
    f("/deployments/[]/shards/unassigned", "a count"),
    f("/deployments/[]/shards/largest_primary_bytes", "quantity"),
    f("/deployments/[]/store_bytes", "quantity"),
    f("/deployments/[]/docs", "a count, never a document"),
    f(
        "/deployments/[]/disk_watermark_headroom_bytes/low",
        "quantity",
    ),
    f(
        "/deployments/[]/disk_watermark_headroom_bytes/high",
        "quantity",
    ),
    f(
        "/deployments/[]/disk_watermark_headroom_bytes/flood_stage",
        "quantity",
    ),
    f("/deployments/[]/restarts/node_containers", "a count"),
    f("/deployments/[]/restarts/dashboards", "a count"),
    f("/deployments/[]/now/cpu_percent", "utilisation"),
    f("/deployments/[]/now/heap_percent", "utilisation"),
    f("/deployments/[]/now/indexing_per_sec", "rate"),
    f("/deployments/[]/now/query_per_sec", "rate"),
    f("/deployments/[]/now/gc_old_millis_per_min", "rate"),
    f("/deployments/[]/now/gc_young_millis_per_min", "rate"),
    f("/deployments/[]/window/start", "coverage timestamp"),
    f("/deployments/[]/window/end", "coverage timestamp"),
    f("/deployments/[]/window/samples", "coverage count"),
    f("/deployments/[]/window/bucket_secs", "constant"),
    f(
        "/deployments/[]/window/cpu_percent/p50",
        "utilisation percentile",
    ),
    f(
        "/deployments/[]/window/cpu_percent/p95",
        "utilisation percentile",
    ),
    f(
        "/deployments/[]/window/heap_percent/p50",
        "utilisation percentile",
    ),
    f(
        "/deployments/[]/window/heap_percent/p95",
        "utilisation percentile",
    ),
    f(
        "/deployments/[]/window/indexing_per_sec/p50",
        "rate percentile",
    ),
    f(
        "/deployments/[]/window/indexing_per_sec/p95",
        "rate percentile",
    ),
    f(
        "/deployments/[]/window/query_per_sec/p50",
        "rate percentile",
    ),
    f(
        "/deployments/[]/window/query_per_sec/p95",
        "rate percentile",
    ),
    f(
        "/deployments/[]/window/gc_old_millis_per_min/p50",
        "rate percentile",
    ),
    f(
        "/deployments/[]/window/gc_old_millis_per_min/p95",
        "rate percentile",
    ),
    f(
        "/deployments/[]/window/gc_young_millis_per_min/p50",
        "rate percentile",
    ),
    f(
        "/deployments/[]/window/gc_young_millis_per_min/p95",
        "rate percentile",
    ),
    f("/deployments/[]/window/disk_used_bytes/first", "quantity"),
    f("/deployments/[]/window/disk_used_bytes/last", "quantity"),
    f("/deployments/[]/stalls/[]/at", "episode timestamp"),
    f(
        "/deployments/[]/stalls/[]/stage",
        "closed ADR-050 ladder rung",
    ),
    f(
        "/deployments/[]/stalls/[]/component",
        "operator vocabulary, word charset only",
    ),
    f(
        "/deployments/[]/stalls/[]/component_status",
        "operator vocabulary, word charset only",
    ),
    f(
        "/deployments/[]/stalls/[]/recovery_stage",
        "OpenSearch vocabulary, word charset only",
    ),
    f(
        "/deployments/[]/stalls/[]/recovery_index_class",
        "the index reduced to its class",
    ),
    f("/deployments/[]/stalls/[]/secs", "a duration"),
    f(
        "/deployments/[]/stalls/[]/remediation",
        "closed set; the pod name reduced away",
    ),
    f(
        "/deployments/[]/integrations/[]/id",
        "catalog id, id charset only",
    ),
    f(
        "/deployments/[]/integrations/[]/version",
        "version charset only",
    ),
    f("/deployments/[]/warnings/[]", "closed set of codes"),
];

// ───────────────────────────── redaction ─────────────────────────────

/// How identities leave: keyed pseudonyms always, real names only on opt-in.
pub struct Redaction {
    key: Vec<u8>,
    names: bool,
}

impl Redaction {
    pub fn new(key: impl AsRef<[u8]>, names: bool) -> Self {
        Self {
            key: key.as_ref().to_vec(),
            names,
        }
    }

    /// `<prefix>-` + the first [`PSEUDONYM_HEX`] hex chars of
    /// `HMAC-SHA256(key, "velox-profile:v1:" + identity)`: stable across
    /// profiles of one installation, not reversible without the key — a plain
    /// hash of a low-entropy hostname would be (ADR-060 alternatives).
    fn pseudonym(&self, prefix: &str, identity: &str) -> String {
        let mut mac =
            <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key).expect("HMAC takes any key");
        mac.update(PSEUDONYM_DOMAIN.as_bytes());
        mac.update(identity.as_bytes());
        let digest = hex::encode(mac.finalize().into_bytes());
        format!("{prefix}-{}", &digest[..PSEUDONYM_HEX])
    }
}

// ───────────────────────────── inputs ─────────────────────────────

/// Everything [`build`] reads. Raw on purpose — names, raw OpenSearch
/// responses and all — so the reduction is the pure half's job and the canary
/// test can seed the real shapes with sentinels.
#[derive(Clone, Debug, Default)]
pub struct ProfileInputs {
    pub generated_at_ms: i64,
    /// `Scope::Admin`. Installation sections and tenant slugs are dropped for
    /// anyone else even if a caller supplied them (fail closed).
    pub admin: bool,
    pub build: BuildInputs,
    pub installation: Option<InstallationInputs>,
    pub quota: Option<crate::k8s::TenantQuota>,
    /// `tenants.id` → slug; consulted only for the admin with names on.
    pub tenant_slugs: BTreeMap<String, String>,
    pub deployments: Vec<DeploymentInputs>,
}

#[derive(Clone, Debug, Default)]
pub struct BuildInputs {
    pub version: String,
    pub commit: String,
    pub image_digest: Option<String>,
    pub operator_image: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct InstallationInputs {
    pub apiserver_version: Option<String>,
    pub capacity: Option<ClusterCapacity>,
    pub storage: Option<DeploymentStorage>,
    pub longhorn_replicas: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct DeploymentInputs {
    pub name: String,
    pub namespace: String,
    /// The owner label (`tenants.id`), `None` for an admin-namespace CR.
    pub tenant_id: Option<String>,
    pub size: String,
    pub purpose: String,
    pub version: String,
    pub target_version: String,
    pub replicas: i64,
    pub memory: String,
    pub heap: String,
    pub disk: String,
    pub health: String,
    pub settled: bool,
    pub stalled: bool,
    pub provisioning_failed: bool,
    /// The stall happening now (ADR-050 `blocked`), when `stalled`.
    pub current_stall: Option<StallInputs>,
    /// Raw `_nodes/stats/os,jvm,fs,indices`.
    pub nodes_stats: Option<Value>,
    /// Raw `_source` of the newest recorded sample.
    pub newest_sample: Option<Value>,
    /// Raw `_cat/indices?format=json`.
    pub cat_indices: Option<Value>,
    /// Raw `_cat/shards?format=json&bytes=b`.
    pub cat_shards: Option<Value>,
    /// Raw `_cluster/settings?include_defaults=true&flat_settings=true`.
    pub cluster_settings: Option<Value>,
    /// Raw window aggregation response (see [`window_query`]).
    pub window: Option<Value>,
    /// Raw `_source`s of the recorded stall episodes.
    pub stall_docs: Vec<Value>,
    pub restarts: Option<(i64, i64)>,
    pub integrations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct StallInputs {
    pub at_ms: i64,
    pub stage: String,
    pub component: String,
    pub component_status: String,
    pub recovery_stage: String,
    /// Raw index name; reduced to its class before it leaves.
    pub recovery_index: String,
    pub secs: i64,
    pub remediation: Option<&'static str>,
}

// ───────────────────────────── the builder ─────────────────────────────

/// Build the document. Pure: no clock, no I/O, no global state.
pub fn build(inputs: ProfileInputs, redaction: &Redaction) -> ClusterProfile {
    let admin = inputs.admin;
    let installation = inputs.installation.filter(|_| admin);
    let capacity = installation.as_ref().and_then(|i| i.capacity.as_ref());
    let host_nodes = capacity.map(|c| c.nodes.as_slice()).unwrap_or_default();
    let host = host_warnings(
        host_nodes,
        installation.as_ref().and_then(|i| i.storage.as_ref()),
    );

    let mut deployments: Vec<DeploymentProfile> = inputs
        .deployments
        .iter()
        .map(|d| {
            let slug = d
                .tenant_id
                .as_ref()
                .and_then(|t| inputs.tenant_slugs.get(t))
                .filter(|_| admin && redaction.names)
                .cloned();
            deployment_profile(
                d,
                redaction,
                slug,
                &host,
                host_nodes,
                inputs.generated_at_ms,
            )
        })
        .collect();
    deployments.sort_by(|a, b| a.id.cmp(&b.id));

    let (kubernetes, nodes, storage, fit) = match &installation {
        Some(i) => {
            let mut nodes: Vec<HostNode> =
                host_nodes.iter().map(|n| host_node(n, redaction)).collect();
            nodes.sort_by(|a, b| a.id.cmp(&b.id));
            let fit = capacity
                .map(|c| {
                    c.fit
                        .iter()
                        .filter_map(|x| {
                            Some(Fit {
                                size: preset_size(&x.size)?,
                                count: x.count,
                                limited_by: match x.limited_by.as_str() {
                                    "cpu" => "cpu",
                                    "disk" => "disk",
                                    _ => "mem",
                                },
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let version = i.apiserver_version.as_deref().and_then(version_token);
            (
                Some(KubernetesFacts {
                    distribution: distribution(version.as_deref().unwrap_or("")),
                    version,
                }),
                Some(nodes),
                Some(StorageFacts {
                    class: storage_class(i.storage.as_ref()),
                    longhorn_replicas: i.longhorn_replicas,
                    pool_bytes: capacity.and_then(|c| c.storage.as_ref()).map(|p| Pool {
                        total: p.total,
                        used: p.used,
                        available: p.available,
                    }),
                }),
                Some(fit),
            )
        }
        None => (None, None, None, None),
    };

    ClusterProfile {
        schema_version: SCHEMA_VERSION,
        generated_at: rfc3339(inputs.generated_at_ms).unwrap_or_default(),
        scope: if admin { "installation" } else { "tenant" },
        names_included: redaction.names,
        build: BuildFacts {
            version: version_token(&inputs.build.version).unwrap_or_default(),
            commit: commit_token(&inputs.build.commit),
            image_digest: inputs
                .build
                .image_digest
                .filter(|_| admin)
                .filter(|d| is_sha256_digest(d)),
            operator_version: inputs
                .build
                .operator_image
                .filter(|_| admin)
                .as_deref()
                .and_then(image_tag),
        },
        kubernetes,
        nodes,
        storage,
        fit,
        quota: inputs.quota.filter(|_| !admin).map(|q| Quota {
            max_deployments: i64::from(q.max_deployments),
            max_total_disk_bytes: u64::try_from(q.max_total_disk_gb).unwrap_or(0) << 30,
            max_nodes: i64::from(q.max_nodes),
        }),
        deployments,
    }
}

fn host_node(n: &crate::api::NodeCapacity, r: &Redaction) -> HostNode {
    let mut roles: Vec<&'static str> = n.roles.iter().map(|x| node_role(x)).collect();
    roles.sort_unstable();
    roles.dedup();
    let mut pressures: Vec<&'static str> = n
        .pressures
        .iter()
        .filter_map(|p| match p.as_str() {
            "MemoryPressure" => Some("MemoryPressure"),
            "DiskPressure" => Some("DiskPressure"),
            "PIDPressure" => Some("PIDPressure"),
            _ => None,
        })
        .collect();
    pressures.sort_unstable();
    let usage = |u: &crate::api::ResUse| Usage {
        total: u.total,
        used: u.used,
    };
    HostNode {
        id: r.pseudonym("n", &n.name),
        roles,
        ready: n.ready,
        pressures,
        kernel: version_token(&n.kernel_version),
        cpu_millis: Allocation {
            allocatable: n.cpu.total,
            requested: n.cpu.requested,
            used: n.cpu.used,
        },
        memory_bytes: Allocation {
            allocatable: n.mem.total,
            requested: n.mem.requested,
            used: n.mem.used,
        },
        host_disk_bytes: n.host_disk.as_ref().map(usage),
        storage_bytes: n.storage.as_ref().map(usage),
    }
}

fn deployment_profile(
    d: &DeploymentInputs,
    r: &Redaction,
    tenant_slug: Option<String>,
    host: &[&'static str],
    host_nodes: &[crate::api::NodeCapacity],
    now_ms: i64,
) -> DeploymentProfile {
    let live = d.nodes_stats.as_ref().filter(|v| v.get("nodes").is_some());
    let live_sample = live.map(|v| sample_from_nodes_stats(v, now_ms));
    let per_node = live.map(node_facts).unwrap_or_default();

    let mut warnings: BTreeSet<&'static str> = host.iter().copied().collect();
    if d.stalled {
        warnings.insert("stalled");
    }
    if d.provisioning_failed {
        warnings.insert("provisioning_failed");
    }
    if live.is_none() {
        warnings.insert("metrics_unavailable");
    }
    if kernel_incompatible(host_nodes, &d.version) {
        warnings.insert("kernel_incompatible");
    }

    let mut integrations: Vec<Integration> = d
        .integrations
        .iter()
        .filter_map(|(id, v)| {
            Some(Integration {
                id: id_token(id)?,
                version: version_token(v)?,
            })
        })
        .collect();
    integrations.sort_by(|a, b| a.id.cmp(&b.id));

    DeploymentProfile {
        id: r.pseudonym("d", &format!("{}/{}", d.namespace, d.name)),
        name: r.names.then(|| d.name.clone()),
        tenant: d.tenant_id.as_ref().map(|t| r.pseudonym("t", t)),
        tenant_slug,
        size: preset_size(&d.size).unwrap_or("custom"),
        sizing: sizing_of(d),
        purpose: match d.purpose.as_str() {
            "observability" | "" => "observability",
            "security" => "security",
            "search" => "search",
            _ => "other",
        },
        opensearch_version: version_token(&d.version),
        target_version: version_token(&d.target_version),
        nodes: d.replicas,
        memory_limit_bytes: nonzero(crate::capacity::parse_qty_bytes(&d.memory)),
        heap_declared_bytes: jvm_bytes(&d.heap),
        heap_max_bytes_observed: per_node.iter().map(|n| n.heap_max).max().and_then(nonzero),
        disk_per_node_bytes: nonzero(crate::capacity::parse_qty_bytes(&d.disk)),
        health: match d.health.as_str() {
            "green" => "green",
            "yellow" => "yellow",
            "red" => "red",
            _ => "unknown",
        },
        settled: d.settled,
        indices: d.cat_indices.as_ref().and_then(|v| indices_of(v, r.names)),
        shards: d.cat_shards.as_ref().and_then(shards_of),
        store_bytes: live.map(|_| per_node.iter().map(|n| n.store).sum()),
        docs: live_sample.map(|s| s.docs),
        disk_watermark_headroom_bytes: d
            .cluster_settings
            .as_ref()
            .and_then(|s| watermark_headroom(s, &per_node)),
        restarts: d.restarts.map(|(node_containers, dashboards)| Restarts {
            node_containers,
            dashboards,
        }),
        now: live_sample.map(|s| now_of(&s, d.newest_sample.as_ref())),
        window: d
            .window
            .as_ref()
            .map(window_of)
            .unwrap_or_else(empty_window),
        stalls: stalls_of(d, now_ms),
        integrations,
        warnings: warnings.into_iter().collect(),
    }
}

// ───────────────────────────── reductions ─────────────────────────────

/// The #26 and #32 rules the wizard computes in the browser, as codes: a pure
/// backend function over the capacity payload (ADR-060 decision 3's warning
/// list). Silent when nothing is known — a guess is worse than nothing.
pub fn host_warnings(
    nodes: &[crate::api::NodeCapacity],
    storage: Option<&DeploymentStorage>,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    // #26: fewer than three host nodes cannot hold three copies of a volume.
    if !nodes.is_empty() && nodes.len() < 3 {
        out.push("storage_single_copy");
    }
    if nodes.iter().any(|n| !n.pressures.is_empty()) {
        out.push("node_pressure");
    }
    // ADR-061: a node-local default loses data on reschedule.
    if matches!(storage, Some(DeploymentStorage::NodeLocal(_))) {
        out.push("storage_node_local");
    }
    out
}

/// #32: OpenSearch 3.8's bundled JDK dies on Debian kernel 6.1.0-52 — the
/// wizard's rule, per deployment version.
pub fn kernel_incompatible(nodes: &[crate::api::NodeCapacity], version: &str) -> bool {
    version.starts_with("3.8")
        && nodes
            .iter()
            .any(|n| n.kernel_version.starts_with("6.1.0-52"))
}

/// Per-OpenSearch-node numbers the aggregate sample does not keep.
#[derive(Clone, Copy, Debug, Default)]
struct NodeFacts {
    heap_max: u64,
    store: u64,
    data_total: u64,
    data_available: u64,
}

fn node_facts(body: &Value) -> Vec<NodeFacts> {
    let Some(map) = body.get("nodes").and_then(Value::as_object) else {
        return Vec::new();
    };
    map.values()
        .map(|n| {
            let u = |p: &str| n.pointer(p).and_then(Value::as_u64).unwrap_or(0);
            let (mut data_total, mut data_available) = (0, 0);
            for d in n
                .pointer("/fs/data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                data_total += d.get("total_in_bytes").and_then(Value::as_u64).unwrap_or(0);
                data_available += d
                    .get("available_in_bytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            NodeFacts {
                heap_max: u("/jvm/mem/heap_max_in_bytes"),
                store: u("/indices/store/size_in_bytes"),
                data_total,
                data_available,
            }
        })
        .collect()
}

/// Counters of one recorded sample. Fields a pre-ADR-060 sample lacks stay
/// `None` rather than reading as zero — a zero would fake a huge rate.
fn recorded_counters(src: &Value) -> Option<(i64, [Option<u64>; 4])> {
    let ts = src.get("@timestamp").and_then(Value::as_i64)?;
    let u = |k: &str| src.get(k).and_then(Value::as_u64);
    Some((
        ts,
        [
            u("index_total"),
            u("query_total"),
            u("gc_old_millis"),
            u("gc_young_millis"),
        ],
    ))
}

/// `now`: utilisation from the live `_nodes/stats` call; rates from that same
/// call's counters against the newest recorded sample.
fn now_of(live: &Sample, newest: Option<&Value>) -> Now {
    let recorded = newest
        .and_then(recorded_counters)
        .filter(|(ts, _)| live.ts > *ts && live.ts - ts <= NOW_RATE_MAX_AGE_MS);
    let rate = |i: usize, cur: u64, per: f64| {
        let (ts, c) = recorded?;
        Some(round2(counter_rate((ts, c[i]?), (live.ts, cur)) * per))
    };
    Now {
        cpu_percent: round2(live.cpu_percent),
        heap_percent: round2(live.heap_percent),
        indexing_per_sec: rate(0, live.index_total, 1.0),
        query_per_sec: rate(1, live.query_total, 1.0),
        gc_old_millis_per_min: rate(2, live.gc_old_millis, 60.0),
        gc_young_millis_per_min: rate(3, live.gc_young_millis, 60.0),
    }
}

/// The window read: 5-minute `date_histogram` buckets over what the metrics
/// alias retains — never raw hits, so the 10,000-hit cap of `series` does not
/// apply. Stall episodes share the alias and are excluded.
pub fn window_query() -> Value {
    let max = |f: &str| serde_json::json!({ "max": { "field": f } });
    let avg = |f: &str| serde_json::json!({ "avg": { "field": f } });
    serde_json::json!({
        "size": 0,
        "query": { "bool": {
            "filter": [{ "range": { "@timestamp": { "gte": format!("now-{WINDOW_DAYS}d") } } }],
            "must_not": [{ "term": { "kind": STALL_KIND } }],
        }},
        "aggs": { "b": {
            "date_histogram": {
                "field": "@timestamp",
                "fixed_interval": format!("{}m", WINDOW_BUCKET_SECS / 60),
                "min_doc_count": 1,
            },
            "aggs": {
                "last_ts": max("@timestamp"),
                "cpu": avg("cpu_percent"),
                "heap": avg("heap_percent"),
                "disk": max("disk_used_bytes"),
                "idx": max("index_total"),
                "qry": max("query_total"),
                "gc_old": max("gc_old_millis"),
                "gc_young": max("gc_young_millis"),
            },
        }},
    })
}

/// The newest recorded sample (for `now` rates).
pub fn newest_sample_query() -> Value {
    serde_json::json!({
        "size": 1,
        "sort": [{ "@timestamp": "desc" }],
        "query": { "bool": { "must_not": [{ "term": { "kind": STALL_KIND } }] } },
    })
}

/// The recorded stall episodes inside the window.
pub fn stalls_query() -> Value {
    serde_json::json!({
        "size": MAX_STALLS,
        "sort": [{ "@timestamp": "desc" }],
        "query": { "bool": { "filter": [
            { "term": { "kind": STALL_KIND } },
            { "range": { "@timestamp": { "gte": format!("now-{WINDOW_DAYS}d") } } },
        ]}},
    })
}

/// One histogram bucket, reduced.
struct Bucket {
    key: i64,
    last_ts: i64,
    docs: u64,
    cpu: Option<f64>,
    heap: Option<f64>,
    disk: Option<u64>,
    counters: [Option<u64>; 4],
}

fn buckets_of(v: &Value) -> Vec<Bucket> {
    let arr = v
        .pointer("/aggregations/b/buckets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out: Vec<Bucket> = arr
        .iter()
        .filter_map(|b| {
            let val = |k: &str| {
                b.get(k)
                    .and_then(|x| x.get("value"))
                    .and_then(Value::as_f64)
            };
            let cnt = |k: &str| val(k).filter(|x| *x >= 0.0).map(|x| x as u64);
            let key = b.get("key").and_then(Value::as_i64)?;
            Some(Bucket {
                key,
                last_ts: val("last_ts").map(|x| x as i64).unwrap_or(key),
                docs: b.get("doc_count").and_then(Value::as_u64).unwrap_or(0),
                cpu: val("cpu"),
                heap: val("heap"),
                disk: cnt("disk"),
                counters: [cnt("idx"), cnt("qry"), cnt("gc_old"), cnt("gc_young")],
            })
        })
        .filter(|b| b.docs > 0)
        .collect();
    out.sort_by_key(|b| b.key);
    out
}

/// Rates between consecutive buckets that both carry counter `i`, through
/// the shared reset clamp.
fn bucket_rates(buckets: &[Bucket], i: usize, per: f64) -> Vec<f64> {
    buckets
        .windows(2)
        .filter_map(|w| {
            let (a, b) = (w[0].counters[i]?, w[1].counters[i]?);
            Some(counter_rate((w[0].last_ts, a), (w[1].last_ts, b)) * per)
        })
        .collect()
}

fn window_of(v: &Value) -> Window {
    let buckets = buckets_of(v);
    if buckets.is_empty() {
        return empty_window();
    }
    // Indexing rate through the existing `downsample` fold: one sample per
    // bucket, each bucket its own slot. Its first point has no predecessor
    // and is dropped rather than counted as a zero-rate bucket.
    let as_samples: Vec<Sample> = buckets
        .iter()
        .filter_map(|b| {
            Some(Sample {
                ts: b.last_ts,
                index_total: b.counters[0]?,
                ..Sample::default()
            })
        })
        .collect();
    let indexing: Vec<f64> = downsample(&as_samples, WINDOW_BUCKET_SECS * 1000)
        .iter()
        .skip(1)
        .map(|p| p.indexing_rate)
        .collect();
    let first_disk = buckets.iter().find_map(|b| b.disk);
    let last_disk = buckets.iter().rev().find_map(|b| b.disk);
    Window {
        start: rfc3339(buckets[0].key),
        end: rfc3339(buckets[buckets.len() - 1].last_ts),
        samples: buckets.iter().map(|b| b.docs).sum(),
        bucket_secs: WINDOW_BUCKET_SECS,
        cpu_percent: percentiles(buckets.iter().filter_map(|b| b.cpu).collect()),
        heap_percent: percentiles(buckets.iter().filter_map(|b| b.heap).collect()),
        indexing_per_sec: percentiles(indexing),
        query_per_sec: percentiles(bucket_rates(&buckets, 1, 1.0)),
        gc_old_millis_per_min: percentiles(bucket_rates(&buckets, 2, 60.0)),
        gc_young_millis_per_min: percentiles(bucket_rates(&buckets, 3, 60.0)),
        disk_used_bytes: first_disk
            .zip(last_disk)
            .map(|(first, last)| FirstLast { first, last }),
    }
}

fn empty_window() -> Window {
    Window {
        start: None,
        end: None,
        samples: 0,
        bucket_secs: WINDOW_BUCKET_SECS,
        cpu_percent: None,
        heap_percent: None,
        indexing_per_sec: None,
        query_per_sec: None,
        gc_old_millis_per_min: None,
        gc_young_millis_per_min: None,
        disk_used_bytes: None,
    }
}

/// Nearest-rank p50/p95; `None` over no values.
fn percentiles(mut v: Vec<f64>) -> Option<Percentiles> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let rank = |p: f64| v[((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1];
    Some(Percentiles {
        p50: round2(rank(0.50)),
        p95: round2(rank(0.95)),
    })
}

/// Index names → counts by class and family. Names leave only on opt-in.
fn indices_of(v: &Value, names: bool) -> Option<Indices> {
    let arr = v.as_array()?;
    let mut classes = IndexClasses {
        system: 0,
        managed: 0,
        user: 0,
    };
    let mut families = BTreeSet::new();
    for name in arr
        .iter()
        .filter_map(|i| i.get("index").and_then(Value::as_str))
    {
        match index_class(name) {
            "system" => classes.system += 1,
            "managed" => classes.managed += 1,
            _ => classes.user += 1,
        }
        families.insert(index_family(name).to_string());
    }
    Some(Indices {
        total: arr.len() as u64,
        families: families.len() as u64,
        by_class: classes,
        family_names: names.then(|| families.into_iter().collect()),
    })
}

/// `system` = dot-prefixed; `managed` = written by VeloxSearch itself (the
/// metrics series, the recipe indices, the OTel stack); `user` = the rest.
pub fn index_class(name: &str) -> &'static str {
    if name.starts_with('.') {
        return "system";
    }
    let family = index_family(name);
    let managed = name.starts_with("velox-metrics-")
        || crate::recipes::RECIPES
            .iter()
            .any(|r| family == crate::recipes::recipe_index(r))
        || [
            crate::otel_stack::SPAN_PATTERN,
            crate::otel_stack::SERVICE_MAP_PATTERN,
            crate::otel_stack::SERVICE_MAP_INDEX,
            crate::otel_stack::LOGS_PATTERN,
        ]
        .iter()
        .any(|p| name.starts_with(p.trim_end_matches('*')));
    if managed {
        "managed"
    } else {
        "user"
    }
}

/// An index name with its rollover counter (`-000001`) and a trailing date
/// (`-2026.09.14`, `-2026-09-14`, `-2026.09`) stripped.
pub fn index_family(name: &str) -> &str {
    let mut s = name;
    if let Some((head, tail)) = s.rsplit_once('-') {
        if tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_digit()) {
            s = head;
        }
    }
    for len in [10, 7] {
        let cut = s.len().saturating_sub(len);
        if s.len() > len + 1 && s.is_char_boundary(cut) && s.is_char_boundary(cut - 1) {
            let (head, tail) = s.split_at(cut);
            let sep = head.as_bytes()[head.len() - 1];
            if matches!(sep, b'-' | b'.' | b'_') && is_date(tail) {
                return &head[..head.len() - 1];
            }
        }
    }
    s
}

/// `YYYY.MM.DD` / `YYYY-MM-DD` / `YYYY.MM` / `YYYY-MM`.
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    match b.len() {
        10 => {
            digits(0..4)
                && digits(5..7)
                && digits(8..10)
                && b[4] == b[7]
                && matches!(b[4], b'.' | b'-')
        }
        7 => digits(0..4) && digits(5..7) && matches!(b[4], b'.' | b'-'),
        _ => false,
    }
}

fn shards_of(v: &Value) -> Option<Shards> {
    let arr = v.as_array()?;
    let mut out = Shards {
        primaries: 0,
        total: arr.len() as u64,
        unassigned: 0,
        largest_primary_bytes: 0,
    };
    for s in arr {
        let text = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("");
        if text("state") == "UNASSIGNED" {
            out.unassigned += 1;
        }
        if text("prirep") == "p" {
            out.primaries += 1;
            let store = s
                .get("store")
                .and_then(|x| x.as_u64().or_else(|| x.as_str()?.parse().ok()))
                .unwrap_or(0);
            out.largest_primary_bytes = out.largest_primary_bytes.max(store);
        }
    }
    Some(out)
}

/// Free bytes above each disk watermark on the tightest node.
fn watermark_headroom(settings: &Value, nodes: &[NodeFacts]) -> Option<Watermarks> {
    let nodes: Vec<&NodeFacts> = nodes.iter().filter(|n| n.data_total > 0).collect();
    if nodes.is_empty() {
        return None;
    }
    let headroom = |level: &str, default: &str| -> i64 {
        let key = format!("cluster.routing.allocation.disk.watermark.{level}");
        let raw = ["transient", "persistent", "defaults"]
            .iter()
            .find_map(|scope| settings.get(scope)?.get(&key)?.as_str())
            .unwrap_or(default);
        nodes
            .iter()
            .map(|n| {
                let must_stay_free = match watermark(raw) {
                    Watermark::UsedFraction(f) => (n.data_total as f64 * (1.0 - f)).round() as i64,
                    Watermark::FreeBytes(b) => b as i64,
                };
                n.data_available as i64 - must_stay_free
            })
            .min()
            .unwrap_or(0)
    };
    Some(Watermarks {
        low: headroom("low", "85%"),
        high: headroom("high", "90%"),
        flood_stage: headroom("flood_stage", "95%"),
    })
}

enum Watermark {
    UsedFraction(f64),
    FreeBytes(u64),
}

/// A watermark as OpenSearch accepts it: `85%`, a ratio `0.85`, or an
/// absolute amount of free space (`500mb`, `10gb`).
fn watermark(raw: &str) -> Watermark {
    let s = raw.trim().to_ascii_lowercase();
    if let Some(p) = s.strip_suffix('%') {
        return Watermark::UsedFraction(p.trim().parse::<f64>().unwrap_or(85.0) / 100.0);
    }
    if let Ok(r) = s.parse::<f64>() {
        return Watermark::UsedFraction(r);
    }
    for (suf, mult) in [
        ("pb", 1u64 << 50),
        ("tb", 1 << 40),
        ("gb", 1 << 30),
        ("mb", 1 << 20),
        ("kb", 1 << 10),
        ("b", 1),
    ] {
        if let Some(n) = s.strip_suffix(suf) {
            if let Ok(n) = n.trim().parse::<f64>() {
                return Watermark::FreeBytes((n * mult as f64) as u64);
            }
        }
    }
    Watermark::UsedFraction(0.85)
}

/// Recorded episodes plus the one happening now, oldest first.
fn stalls_of(d: &DeploymentInputs, now_ms: i64) -> Vec<Stall> {
    let mut out: Vec<(i64, Stall)> = d
        .stall_docs
        .iter()
        .filter_map(|doc| {
            let text = |k: &str| doc.get(k).and_then(Value::as_str).unwrap_or("");
            let at = doc.get("@timestamp").and_then(Value::as_i64)?;
            Some((
                at,
                Stall {
                    at: rfc3339(at)?,
                    stage: stage_code(text("stage")),
                    component: word_token(text("component")),
                    component_status: word_token(text("component_status")),
                    recovery_stage: word_token(text("recovery_stage")),
                    recovery_index_class: match text("recovery_index_class") {
                        "system" => Some("system"),
                        "managed" => Some("managed"),
                        "user" => Some("user"),
                        _ => None,
                    },
                    secs: doc
                        .get("recovery_secs")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    remediation: remediation_code(text("remediation")),
                },
            ))
        })
        .collect();
    if let Some(c) = d.current_stall.as_ref().filter(|_| d.stalled) {
        let stall = stall_now(c);
        // Already recorded as an episode → the record stands for it.
        let recorded = out
            .iter()
            .any(|(_, s)| s.stage == stall.stage && s.component == stall.component);
        if !recorded {
            out.push((c.at_ms.min(now_ms), stall));
        }
    }
    out.sort_by_key(|(at, _)| *at);
    let skip = out.len().saturating_sub(MAX_STALLS);
    out.into_iter().skip(skip).map(|(_, s)| s).collect()
}

fn stall_now(c: &StallInputs) -> Stall {
    Stall {
        at: rfc3339(c.at_ms).unwrap_or_default(),
        stage: stage_code(&c.stage),
        component: word_token(&c.component),
        component_status: word_token(&c.component_status),
        recovery_stage: word_token(&c.recovery_stage),
        recovery_index_class: (!c.recovery_index.is_empty())
            .then(|| index_class(&c.recovery_index)),
        secs: c.secs,
        remediation: c.remediation,
    }
}

// ───────────────────────────── tokens ─────────────────────────────

fn nonzero(v: u64) -> Option<u64> {
    (v > 0).then_some(v)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn rfc3339(ms: i64) -> Option<String> {
    use k8s_openapi::chrono::{DateTime, SecondsFormat};
    DateTime::from_timestamp_millis(ms).map(|t| t.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// A version-shaped string (`3.8.0`, `v1.36.3+k3s1`, `6.1.0-40-amd64`):
/// starts with a digit (after an optional `v`), version charset, ≤ 64 chars.
/// Anything else is not a version and does not leave.
fn version_token(s: &str) -> Option<String> {
    let s = s.trim();
    let body = s.strip_prefix('v').unwrap_or(s);
    let ok = !body.is_empty()
        && s.len() <= 64
        && body.as_bytes()[0].is_ascii_digit()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
    ok.then(|| s.to_string())
}

/// A single word of cluster vocabulary (`RollingRestart`, `Running`, `init`):
/// letters, digits and `_` only — no dots or dashes, so it cannot carry a
/// hostname, an address or an index name.
fn word_token(s: &str) -> Option<String> {
    let ok = !s.is_empty()
        && s.len() <= 40
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then(|| s.to_string())
}

/// A catalog id: lowercase, digits and `-`.
fn id_token(s: &str) -> Option<String> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    ok.then(|| s.to_string())
}

/// A git sha, or the literal `unknown` a build without one reports.
fn commit_token(s: &str) -> String {
    let sha = (7..=40).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if sha || s == crate::build_info::COMMIT_UNKNOWN {
        s.to_string()
    } else {
        crate::build_info::COMMIT_UNKNOWN.to_string()
    }
}

fn is_sha256_digest(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The tag of an image reference, never its registry or repository.
fn image_tag(image: &str) -> Option<String> {
    let last = image.rsplit('/').next()?;
    let (_, tag) = last.split_once(':')?;
    version_token(tag.split('@').next()?)
}

fn preset_size(s: &str) -> Option<&'static str> {
    crate::k8s::PRESET_SIZES.iter().copied().find(|p| *p == s)
}

fn sizing_of(d: &DeploymentInputs) -> &'static str {
    let Some(size) = preset_size(&d.size) else {
        return "custom";
    };
    let p = crate::k8s::sizing_profile(size);
    let q = crate::capacity::parse_qty_bytes;
    if q(&d.memory) == q(&p.mem) && q(&d.disk) == q(&p.disk) && d.replicas == i64::from(p.nodes) {
        "preset"
    } else {
        "custom"
    }
}

/// A JVM size (`1536m`, `2g`, `-Xmx1g`) in bytes; the last size in the string
/// wins, which is `-Xmx` on a legacy `-Xms… -Xmx…` pair.
fn jvm_bytes(s: &str) -> Option<u64> {
    s.split_whitespace().rev().find_map(|tok| {
        let t = tok.trim_start_matches("-Xmx").trim_start_matches("-Xms");
        let (num, mult) = match t.chars().last()?.to_ascii_lowercase() {
            'g' => (&t[..t.len() - 1], 1u64 << 30),
            'm' => (&t[..t.len() - 1], 1 << 20),
            'k' => (&t[..t.len() - 1], 1 << 10),
            _ => (t, 1),
        };
        num.parse::<u64>().ok().and_then(|n| nonzero(n * mult))
    })
}

fn node_role(r: &str) -> &'static str {
    match r {
        "control-plane" | "master" => "control-plane",
        "etcd" => "etcd",
        "worker" => "worker",
        _ => "other",
    }
}

fn distribution(version: &str) -> &'static str {
    if version.contains("+k3s") {
        "k3s"
    } else if version.contains("+rke2") {
        "rke2"
    } else if version.contains("-eks-") {
        "eks"
    } else if version.contains("-gke.") {
        "gke"
    } else {
        "unknown"
    }
}

fn storage_class(s: Option<&DeploymentStorage>) -> &'static str {
    match s {
        Some(DeploymentStorage::Longhorn { .. }) => "longhorn",
        Some(DeploymentStorage::ForeignDefault(_)) => "foreign_default",
        Some(DeploymentStorage::NodeLocal(_)) => "node_local",
        Some(DeploymentStorage::Absent) => "absent",
        None => "unknown",
    }
}

fn stage_code(s: &str) -> &'static str {
    [
        "storage",
        "accepted",
        "volumes",
        "nodes",
        "security",
        "dashboards",
        "settling",
        "ready",
    ]
    .into_iter()
    .find(|x| *x == s)
    .unwrap_or("unknown")
}

fn remediation_code(s: &str) -> Option<&'static str> {
    match s {
        "node_bounce" => Some("node_bounce"),
        "dashboards_reset" => Some("dashboards_reset"),
        _ => None,
    }
}

// ───────────────────────────── gathering ─────────────────────────────

use crate::scope::{Deployment, Scope};

/// How many deployments are profiled at once.
const GATHER_CONCURRENCY: usize = 4;

/// Gather every input for `scope` and build its profile. Talks to the
/// Kubernetes API and each deployment's own OpenSearch only. Every source is
/// best-effort: what cannot be read is absent or `null`, never invented.
pub async fn gather(scope: &Scope, names: bool) -> anyhow::Result<ClusterProfile> {
    use futures::stream::{self, StreamExt};
    let admin = scope.is_admin();
    let redaction = Redaction::new(crate::auth::pseudonym_key(), names);

    let deps = crate::k8s::scoped_deployments(scope).await?;
    let deployments: Vec<DeploymentInputs> = stream::iter(deps)
        .map(|dep| async move { gather_deployment(&dep).await })
        .buffer_unordered(GATHER_CONCURRENCY)
        .filter_map(|d| async move { d })
        .collect()
        .await;

    let mut inputs = ProfileInputs {
        generated_at_ms: now_ms(),
        admin,
        build: BuildInputs {
            version: crate::build_info::VERSION.to_string(),
            commit: crate::build_info::COMMIT.to_string(),
            ..BuildInputs::default()
        },
        deployments,
        ..ProfileInputs::default()
    };

    if admin {
        let info = crate::build_info::collect().await;
        inputs.build.image_digest = Some(info.image_digest);
        inputs.build.operator_image = Some(info.operator_image);
        let client = crate::k8s::client().await?;
        inputs.installation = Some(InstallationInputs {
            apiserver_version: client.apiserver_version().await.ok().map(|v| v.git_version),
            capacity: crate::capacity::cluster_capacity().await.ok(),
            storage: crate::bootstrap::classify_storage(&client).await.ok(),
            longhorn_replicas: crate::bootstrap::longhorn_replicas(&client).await,
        });
        if names && crate::tenants::enabled() {
            inputs.tenant_slugs = crate::tenants::slugs().await.unwrap_or_default();
        }
    } else if let Some(id) = scope.tenant_id() {
        inputs.quota = crate::tenants::quota_of(id).await.ok();
    }
    Ok(build(inputs, &redaction))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One deployment's inputs. `None` only when its CR vanished mid-read.
async fn gather_deployment(dep: &Deployment) -> Option<DeploymentInputs> {
    let status = crate::k8s::get_deployment(dep).await.ok().flatten()?;
    let os = OpenSearch::new(dep).await;
    let alias = format!("velox-metrics-{dep}");
    let a = &status.activity;
    let b = &a.blocked;
    let current_stall = a.stalled.then(|| StallInputs {
        at_ms: now_ms() - a.since_secs.max(0) * 1000,
        stage: a.stage.to_string(),
        component: b.component.clone(),
        component_status: b.component_status.clone(),
        recovery_stage: b.recovery_stage.clone(),
        recovery_index: b.recovery_index.clone(),
        secs: a.since_secs,
        remediation: remediation_of(b),
    });
    let hits = |v: Option<Value>| -> Vec<Value> {
        v.and_then(|v| v.pointer("/hits/hits").cloned())
            .and_then(|h| h.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|h| h.get("_source").cloned())
            .collect()
    };
    Some(DeploymentInputs {
        name: dep.name().to_string(),
        namespace: dep.namespace().to_string(),
        tenant_id: dep.tenant().map(str::to_string),
        size: status.size.clone(),
        purpose: status.purpose.clone(),
        version: status.version.clone(),
        target_version: status.target_version.clone(),
        replicas: status.replicas,
        memory: status.memory.clone(),
        heap: status.heap.clone(),
        disk: status.disk.clone(),
        health: status.health.clone(),
        settled: a.settled,
        stalled: a.stalled,
        provisioning_failed: status.provisioning.state == "failed",
        current_stall,
        nodes_stats: os.get("_nodes/stats/os,jvm,fs,indices").await,
        newest_sample: hits(os.search(&alias, &newest_sample_query()).await)
            .into_iter()
            .next(),
        cat_indices: os.get("_cat/indices?format=json&h=index").await,
        cat_shards: os
            .get("_cat/shards?format=json&bytes=b&h=prirep,state,store")
            .await,
        cluster_settings: os
            .get("_cluster/settings?include_defaults=true&flat_settings=true")
            .await,
        window: os.search(&alias, &window_query()).await,
        stall_docs: hits(os.search(&alias, &stalls_query()).await),
        restarts: crate::k8s::pod_restarts(dep).await,
        integrations: crate::k8s::integration_versions(dep)
            .await
            .unwrap_or_default(),
    })
}

/// The kind of remediation a stall already received — never the pod's name.
pub(crate) fn remediation_of(b: &crate::activity::Blocked) -> Option<&'static str> {
    if b.remediated_node.is_some() {
        Some("node_bounce")
    } else if b.dashboards_remediated {
        Some("dashboards_reset")
    } else {
        None
    }
}

/// Read-only access to one deployment's own OpenSearch.
struct OpenSearch {
    base: String,
    user: String,
    pass: String,
    http: Option<reqwest::Client>,
}

impl OpenSearch {
    async fn new(dep: &Deployment) -> Self {
        let (user, pass) = crate::k8s::admin_creds(dep).await;
        Self {
            base: crate::recipes::os_base(dep),
            user,
            pass,
            http: crate::recipes::http().ok(),
        }
    }

    async fn get(&self, path: &str) -> Option<Value> {
        let c = self.http.as_ref()?;
        let req = c.get(format!("{}/{path}", self.base));
        Self::json(req.basic_auth(&self.user, Some(&self.pass))).await
    }

    async fn search(&self, index: &str, body: &Value) -> Option<Value> {
        let c = self.http.as_ref()?;
        let req = c.post(format!("{}/{index}/_search", self.base)).json(body);
        Self::json(req.basic_auth(&self.user, Some(&self.pass))).await
    }

    async fn json(req: reqwest::RequestBuilder) -> Option<Value> {
        let resp = req.send().await.ok()?.error_for_status().ok()?;
        resp.json().await.ok()
    }
}

#[cfg(test)]
mod tests;
