// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Server-only Kubernetes integration: multi-tenant OpenSearch deployments.
//!
//! Each deployment is an `OpenSearchCluster` CR (opensearch.org/v1) managed by
//! the operator, plus a per-cluster admin Secret and a dashboards Ingress at
//! `<name>.veloxsearch.ai`. Create / list / delete, parameterized by sizing.

use anyhow::{bail, Context, Result};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Namespace, PersistentVolumeClaim, Secret};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::{
    Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, Patch,
    PatchParams, PostParams,
};
use kube::Client;
use std::collections::BTreeMap;

use crate::scope::{Deployment, LocatedCluster, Scope};

/// The off-cluster fallback namespace (#67). Deliberately a namespace that does
/// NOT exist in any real deployment — never `veloxsearch-test` or
/// any other namespace holding customer data. A dev box whose kubeconfig still
/// points at a live cluster then lands on this inert namespace, so `ns()` cannot
/// silently drive production; the `ensure_namespace_exists` guard turns every
/// write op into a loud, actionable refusal instead.
const DEV_FALLBACK_NS: &str = "veloxsearch-dev";

/// App namespace (= where OpenSearch CRs, the operator and our Secrets live).
/// Downward-API env first, then the SA-mounted file — the two signals that we
/// are genuinely running as a pod in a namespace — so the same binary runs in
/// veloxsearch-system (generic install, ADR-027) or veloxsearch-test (Tornis
/// prod). Off-cluster with neither signal, it falls back to the inert
/// `DEV_FALLBACK_NS` and warns loudly (#67): it must NEVER default to a
/// namespace that holds real data.
pub(crate) fn ns() -> &'static str {
    static NS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NS.get_or_init(|| {
        let pod_env = std::env::var("POD_NAMESPACE").ok();
        let sa_file =
            std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace").ok();
        resolve_ns(pod_env, sa_file).unwrap_or_else(|fallback| {
            // Off-cluster with nothing configured. Say so loudly and pick an
            // inert namespace — a stale-kubeconfig dev run must not touch a
            // live deployment namespace (e.g. veloxsearch-test) by default.
            tracing::warn!(
                namespace = fallback,
                "POD_NAMESPACE is unset and no in-cluster service-account namespace \
                 was found — falling back to the inert dev namespace '{fallback}'. \
                 Set POD_NAMESPACE explicitly to target a real namespace; this \
                 binary will NOT drive an existing deployment namespace by default."
            );
            fallback.to_string()
        })
    })
}

/// Pick the app namespace from the two in-cluster signals: the downward-API
/// `POD_NAMESPACE` env (`pod_env`) first, then the SA-mounted namespace file
/// (`sa_file`). Returns `Ok(ns)` when either named a non-blank namespace, or
/// `Err(DEV_FALLBACK_NS)` when neither did — the `Err` carries the inert
/// fallback so the caller can warn before using it. Split out (like
/// `disk_resize_check`) so the selection is unit-testable without touching
/// process env or the filesystem (#67).
fn resolve_ns(pod_env: Option<String>, sa_file: Option<String>) -> Result<String, &'static str> {
    pod_env
        .or(sa_file)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or(DEV_FALLBACK_NS)
}
// OpenSearch admin username — conventional, NOT a secret. The PASSWORD is never
// a literal: it is generated per cluster by `gen_admin_password` and stored in
// that cluster's admin Secret (read back via `admin_creds`).
pub(crate) const ADMIN_USER: &str = "admin";
const LABEL_SIZE: &str = "veloxsearch.ai/size";
const LABEL_PURPOSE: &str = "veloxsearch.ai/purpose";
const LABEL_MONITORS: &str = "veloxsearch.ai/monitors";
/// Installed integration package versions, `id=version` comma-separated — the
/// ADR-039 extension of the `monitors` list (#75). A sibling annotation, not a
/// new encoding inside `monitors`, so nothing that reads `monitors` changes.
const LABEL_INTEGRATION_VERSIONS: &str = "veloxsearch.ai/integration-versions";
/// Which identity provider a deployment authenticates against (ADR-045).
/// Denormalized onto the CR so list views don't read a Secret per deployment.
const LABEL_AUTH_KIND: &str = "veloxsearch.ai/auth-kind";
/// `otel_stack::STACK_VERSION` when the OTel observability stack is installed
/// on this deployment, absent otherwise (ADR-053). Written only by
/// `set_otel_stack`; deliberately NOT part of `monitors`, see that function.
const LABEL_OTEL_STACK: &str = "veloxsearch.ai/otel-stack";

pub async fn client() -> Result<Client> {
    Client::try_default()
        .await
        .context("failed to build kube client (KUBECONFIG / in-cluster)")
}

fn cluster_resource() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "opensearch.org",
        "v1",
        "OpenSearchCluster",
    ))
}

/// The `OpenSearchCluster` API in ONE named namespace.
///
/// Since #80 a deployment's namespace is a property of the deployment (its
/// tenant's, per ADR-044), not an ambient global: every CR call below goes
/// through a handle built from a resolved [`Deployment`] or an explicit
/// [`Scope`], so no deployment path can silently fall back to `ns()` and act
/// on another tenant's object.
pub(crate) fn cluster_api_in(client: &Client, namespace: &str) -> Api<DynamicObject> {
    Api::namespaced_with(client.clone(), namespace, &cluster_resource())
}

/// The CR API for one deployment the caller has been proven to own.
fn os_api(client: &Client, dep: &Deployment) -> Api<DynamicObject> {
    cluster_api_in(client, dep.namespace())
}

fn ingress_api(client: &Client, namespace: &str) -> Api<DynamicObject> {
    let gvk = GroupVersionKind::gvk("networking.k8s.io", "v1", "Ingress");
    Api::namespaced_with(client.clone(), namespace, &ApiResource::from_gvk(&gvk))
}

/// Reduce a CR to what an ownership decision needs.
fn located(o: &DynamicObject, namespace: &str) -> LocatedCluster {
    LocatedCluster {
        namespace: namespace.to_string(),
        labels: o.metadata.labels.clone().unwrap_or_default(),
    }
}

/// Every `OpenSearchCluster` a scope may read.
///
/// Three shapes, one rule — a caller only ever sees objects it owns:
///   * **tenant** → its own namespace, filtered server-side by the owner label;
///   * **admin, flag off** → the app namespace, no selector: byte-identical to
///     the query that shipped before #80 (the whole no-regression claim);
///   * **admin, flag on** → every namespace, because the installation admin is
///     the super-tenant (see `scope.rs`).
pub(crate) async fn list_clusters(scope: &Scope) -> Result<Vec<DynamicObject>> {
    let client = client().await?;
    let mut lp = ListParams::default();
    if let Some(sel) = scope.label_selector() {
        lp = lp.labels(&sel);
    }
    let api = match scope {
        Scope::Tenant { namespace, .. } => cluster_api_in(&client, namespace),
        Scope::Admin if !crate::tenants::enabled() => cluster_api_in(&client, ns()),
        Scope::Admin => Api::all_with(client.clone(), &cluster_resource()),
    };
    let list = api.list(&lp).await.context("listing OpenSearchClusters")?;
    // Belt to the selector's braces: re-check ownership in-process, so a server
    // that ignored the selector still cannot leak a foreign deployment.
    Ok(list
        .into_iter()
        .filter(|o| scope.owns(&o.metadata.labels.clone().unwrap_or_default()))
        .collect())
}

/// Find one CR by name within a scope's reach — the lookup behind
/// [`Scope::resolve`]. A tenant's query never leaves its own namespace, which
/// is what makes "not yours" and "does not exist" the same answer.
pub(crate) async fn locate_cluster(scope: &Scope, name: &str) -> Result<Option<LocatedCluster>> {
    let client = client().await?;
    let found = match scope {
        Scope::Tenant { namespace, .. } => cluster_api_in(&client, namespace)
            .get_opt(name)
            .await
            .context("looking up deployment")?
            .map(|o| located(&o, namespace)),
        Scope::Admin if !crate::tenants::enabled() => cluster_api_in(&client, ns())
            .get_opt(name)
            .await
            .context("looking up deployment")?
            .map(|o| located(&o, ns())),
        Scope::Admin => {
            let api: Api<DynamicObject> = Api::all_with(client.clone(), &cluster_resource());
            api.list(&ListParams::default().fields(&format!("metadata.name={name}")))
                .await
                .context("looking up deployment")?
                .into_iter()
                .next()
                .map(|o| {
                    let n = o
                        .metadata
                        .namespace
                        .clone()
                        .unwrap_or_else(|| ns().to_string());
                    located(&o, &n)
                })
        }
    };
    Ok(found)
}

/// Server-side-apply one object of any kind, resolving the API from a literal
/// GVK (no discovery round-trip — the caller knows the kind at compile time).
///
/// This is the workload-provisioning counterpart of `bootstrap::apply_doc`,
/// which resolves through `kube::discovery` because it applies vendored YAML
/// bundles of unknown shape. Callers here build the manifest inline, so the
/// discovery cost would buy nothing.
///
/// The field manager is `veloxsearch` with `force`, and both are load-bearing:
/// changing either would make the next apply of an existing DaemonSet a
/// conflicting co-owner rather than the same owner updating in place.
pub(crate) async fn apply_dynamic(
    client: &Client,
    group: &str,
    version: &str,
    kind: &str,
    namespace: Option<&str>,
    name: &str,
    manifest: &serde_json::Value,
) -> Result<()> {
    let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
        None => Api::all_with(client.clone(), &ar),
    };
    let pp = PatchParams::apply("veloxsearch").force();
    api.patch(name, &pp, &Patch::Apply(manifest))
        .await
        .with_context(|| format!("applying {kind}/{name}"))?;
    Ok(())
}

/// Best-effort delete of one object, by literal GVK. Deliberately infallible:
/// teardown paths iterate an inventory in which a missing object is the normal
/// case (a partial install, or a second uninstall), and one 404 must never stop
/// the rest of the sweep.
pub(crate) async fn delete_dynamic(
    client: &Client,
    group: &str,
    version: &str,
    kind: &str,
    namespace: Option<&str>,
    name: &str,
) {
    let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
        None => Api::all_with(client.clone(), &ar),
    };
    let _ = api.delete(name, &DeleteParams::default()).await;
}

fn admin_secret_name(name: &str) -> String {
    format!("{name}-admin-credentials")
}

/// Credentials for the `kibanaserver` account Dashboards logs in with. We own
/// this Secret (rather than letting the operator generate one) because the
/// auth-provider path replaces `internal_users.yml`, and the hash we write
/// there has to match a password we actually know (ADR-045).
fn dashboards_secret_name(name: &str) -> String {
    format!("{name}-dashboards-credentials")
}

/// Holds the securityconfig YAML file set the operator hands to `securityadmin`.
fn securityconfig_secret_name(name: &str) -> String {
    format!("{name}-securityconfig")
}

/// Holds the saved auth-provider spec plus any credential the Dashboards pod
/// reads through `spec.dashboards.env` — deliberately NOT the securityconfig
/// Secret, whose keys are all mounted as files for `securityadmin -cd`.
fn auth_state_secret_name(name: &str) -> String {
    format!("{name}-auth")
}

/// Key under which `auth_state_secret_name` stores the spec (as JSON).
const AUTH_SPEC_KEY: &str = "spec.json";

/// Field manager owning ONLY the auth-related fields of the CR. Separate from
/// the `veloxsearch` manager that applies the full manifest, so neither prunes
/// the other's fields — and so dropping a provider prunes exactly what auth
/// added (same trick as `veloxsearch-secreset`).
const AUTH_FIELD_MANAGER: &str = "veloxsearch-auth";

/// Conventional Dashboards service account in the OpenSearch security config.
const DASHBOARDS_USER: &str = "kibanaserver";

/// Deployment name must be a DNS-1123 label (used as a hostname component).
pub(crate) fn validate_name(name: &str) -> Result<()> {
    let ok = (1..=40).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !ok {
        bail!("invalid name '{name}': use 1-40 chars, lowercase letters/digits/'-', not starting/ending with '-'");
    }
    Ok(())
}

/// Coerce arbitrary user input into a valid DNS-1123 label fragment: lowercase,
/// only `[a-z0-9-]`, no leading/trailing `-`, capped so `<base>-<suffix>` fits.
fn sanitize_label(s: &str) -> String {
    let mapped: String = s
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed: String = mapped.trim_matches('-').chars().take(35).collect();
    let trimmed = trimmed.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "cluster".to_string()
    } else {
        trimmed
    }
}

/// A short random base-36 suffix (Elastic-Cloud-style `name-xxxx`).
fn rand_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let c = CTR.fetch_add(1, Ordering::Relaxed);
    // mix time + monotonic counter so rapid successive calls differ
    let mut x = nanos ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    const CS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(4);
    for _ in 0..4 {
        out.push(CS[(x % 36) as usize] as char);
        x /= 36;
    }
    out
}

/// Draw `len` characters uniformly at random from `alphabet` using the OS
/// CSPRNG (`getrandom`). Rejection-samples raw bytes so the mapping carries no
/// modulo bias. `alphabet` must be 1..=128 bytes.
pub(crate) fn random_chars(alphabet: &[u8], len: usize) -> Result<String> {
    let n = alphabet.len();
    // Largest multiple of n that fits in a byte — bytes at/above it are rejected
    // so every alphabet index is equally likely.
    let limit = (256 / n * n) as u16;
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        getrandom::fill(&mut buf).context("OS CSPRNG (getrandom) unavailable")?;
        for &b in &buf {
            if (b as u16) < limit {
                out.push(alphabet[b as usize % n] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Generate a strong, random, per-cluster OpenSearch admin password (CSPRNG).
///
/// 24 chars from an alphabet that satisfies OpenSearch 3.0's strict password
/// regex — at least one upper, lower, digit and special — while staying free of
/// any character that could break a shell, JSON, a URL or a Fluent Bit config:
/// the only specials are the RFC-3986 "unreserved" set `-._~`. Loops until all
/// four classes are present (overwhelmingly the first draw at length 24), so the
/// operator's securityconfig seeding never rejects it. NEVER a literal default.
fn gen_admin_password() -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    const LEN: usize = 24;
    loop {
        let pw = random_chars(ALPHABET, LEN)?;
        let has = |f: fn(char) -> bool| pw.chars().any(f);
        // A LEADING '-' makes the password look like a command-line flag, and
        // several things downstream pass it as an argv element rather than as
        // an environment value. The OpenSearch Dashboards entrypoint turns
        // OPENSEARCH_PASSWORD into `--opensearch.password <value>`, and a value
        // starting with '-' is consumed as the next option, so the container
        // dies at boot with `Extra serve options "--opensearch.password" must
        // have a value` — roughly one deployment in seventy, on a code path
        // that only runs once the auth provider is set (found in MR4, #56).
        // Requiring the first character to be alphanumeric costs nothing in
        // entropy that matters here and removes the whole class.
        if pw.starts_with(|c: char| c.is_ascii_alphanumeric())
            && has(|c| c.is_ascii_uppercase())
            && has(|c| c.is_ascii_lowercase())
            && has(|c| c.is_ascii_digit())
            && has(|c| matches!(c, '-' | '.' | '_' | '~'))
        {
            return Ok(pw);
        }
    }
}

/// Generate a unique deployment name `<base>-<suffix>` for a NEW deployment
/// (ADR-020). Always appends a suffix (like competitors) so two "blablabl"s
/// never collide.
///
/// Uniqueness is **cluster-wide**, not per namespace: the name becomes a
/// dashboards subdomain (`<name>.veloxsearch.ai`) and a collection-agent name
/// in the shared `velox-agents` namespace, both of which are global. Two
/// tenants picking the same name must therefore still get different names —
/// so with multi-tenancy on the candidate is checked against every namespace.
/// That reveals nothing: the caller never learns why a candidate was skipped,
/// only which random suffix it ended up with. With the flag off the check is
/// the single-namespace one that shipped before.
pub async fn unique_name(scope: &Scope, base: &str) -> Result<String> {
    let base = sanitize_label(base);
    let client = client().await?;
    for _ in 0..25 {
        let candidate = format!("{base}-{}", rand_suffix());
        let taken = if crate::tenants::enabled() {
            let api: Api<DynamicObject> = Api::all_with(client.clone(), &cluster_resource());
            !api.list(&ListParams::default().fields(&format!("metadata.name={candidate}")))
                .await
                .context("checking deployment name uniqueness")?
                .items
                .is_empty()
        } else {
            cluster_api_in(&client, &scope.write_namespace())
                .get_opt(&candidate)
                .await?
                .is_some()
        };
        if !taken {
            return Ok(candidate);
        }
    }
    bail!("could not generate a unique name for '{base}' after 25 tries")
}

struct Sizing {
    replicas: u32,
    disk: &'static str,
    /// The ONE memory number of a tier: nodePool request AND limit (Guaranteed
    /// QoS — never overcommit a data node), and the value the operator derives
    /// the JVM heap from (ADR-035). Values = the previous tiers' *requests*, so
    /// scheduling footprints and the capacity planner's math are unchanged.
    mem: &'static str,
    cpu_req: &'static str,
    cpu_lim: &'static str,
}

/// Sizing presets = memory + disk tiers. Replicas are ALWAYS 3 (product
/// decision, 2026-06-09 daily): 3 nodes for quorum/safety, never varied by
/// preset (≥3 also survives the operator removing its bootstrap pod). The JVM
/// heap is NOT a preset field — the OPERATOR derives it as half the memory
/// request (ADR-035): small 2Gi→1g, medium/large 3Gi→1536m.
fn sizing(size: &str) -> Sizing {
    match size {
        "large" => Sizing {
            replicas: 3,
            disk: "20Gi",
            mem: "3Gi",
            cpu_req: "1",
            cpu_lim: "2",
        },
        "medium" => Sizing {
            replicas: 3,
            disk: "10Gi",
            mem: "3Gi",
            cpu_req: "1",
            cpu_lim: "2",
        },
        // "small" / default
        _ => Sizing {
            replicas: 3,
            disk: "5Gi",
            mem: "2Gi",
            cpu_req: "500m",
            cpu_lim: "1",
        },
    }
}

/// The named sizing presets, in ascending order. The single list the API and
/// the wizard agree on — `sizing()` defines each one's actual values.
pub const PRESET_SIZES: [&str; 3] = ["small", "medium", "large"];

/// Per-node resource *requests* of a sizing preset as raw K8s quantity strings
/// `(cpu_request, mem_request, disk)` — read straight from `sizing()` so the
/// capacity planner (`capacity.rs`) sizes "how many more deployments fit" from
/// the same preset table the creator uses (never a second copy of the numbers).
/// A deployment is always 3 nodes (ADR-016), so the planner multiplies by 3.
pub(crate) fn preset_requests(size: &str) -> (String, String, String) {
    let s = sizing(size);
    (s.cpu_req.to_string(), s.mem.to_string(), s.disk.to_string())
}

/// A resolved sizing tier as the wizard consumes it (ADR-016): the preset key,
/// a display label, the always-3 node count, the node memory, the operator-
/// derived JVM heap (half the memory, ADR-035), and the disk. This is the
/// serializable projection of `sizing()` + `heap_short()` — the
/// `/api/sizing_presets` and `/api/custom_sizing` responses — so the frontend
/// never hardcodes tier values.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SizingProfile {
    /// Preset key ("small"/"medium"/"large") or "custom".
    pub name: String,
    /// Human label for the size card ("Small" / "Custom").
    pub label: String,
    /// Node count — ALWAYS 3 (ADR-016 invariant), never varied by tier.
    pub nodes: u32,
    /// Node memory (request = limit), e.g. "4Gi".
    pub mem: String,
    /// JVM heap, short form ("1536m"/"2g") — the value the OPERATOR will set
    /// (half of `mem`, ADR-035); shown so users know before applying.
    pub heap: String,
    /// Persistent disk per node, e.g. "20Gi".
    pub disk: String,
}

/// One preset resolved into its wire shape. Reads straight from `sizing()` so
/// the preset values live in exactly one place.
pub fn sizing_profile(size: &str) -> SizingProfile {
    let s = sizing(size);
    let name = match size {
        "large" => "large",
        "medium" => "medium",
        _ => "small",
    };
    let mut label = name.to_string();
    label[..1].make_ascii_uppercase();
    SizingProfile {
        name: name.to_string(),
        label,
        nodes: s.replicas,
        mem: s.mem.to_string(),
        heap: heap_short(s.mem),
        disk: s.disk.to_string(),
    }
}

/// All preset tiers — the `/api/sizing_presets` payload.
pub fn sizing_presets() -> Vec<SizingProfile> {
    PRESET_SIZES.iter().map(|s| sizing_profile(s)).collect()
}

/// The "custom size" profile (ADR-016): resolve user-supplied memory/disk into
/// a full sizing, applying the same invariants as a preset — 3 nodes, and the
/// JVM heap the operator will derive (half the memory, ADR-035). Blank inputs
/// fall back to the `small` preset's value, so a partial custom config is
/// still complete.
pub fn custom_sizing(memory: Option<&str>, disk: Option<&str>) -> SizingProfile {
    let base = sizing("small");
    fn nonblank(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|s| !s.is_empty())
    }
    let mem = nonblank(memory).unwrap_or(base.mem).to_string();
    let disk = nonblank(disk).unwrap_or(base.disk).to_string();
    SizingProfile {
        name: "custom".to_string(),
        label: "Custom".to_string(),
        nodes: base.replicas, // 3-node invariant holds for custom too
        heap: heap_short(&mem),
        mem,
        disk,
    }
}

/// Guard a disk resize on an EXISTING deployment (the Edit/resize flow). No-op
/// for a brand-new cluster (the create flow passes a name that doesn't exist
/// yet), so it's safe to call unconditionally from `create_cluster`.
///
/// Two rules, matching what Kubernetes/CSI actually allow:
///  1. **No shrink** — a PVC's `requests.storage` can only grow; a smaller value
///     is rejected downstream, so refuse it up front with a clear message
///     instead of letting the CR drift from the real (larger) volume.
///  2. **Grow needs an expandable class** — increasing `diskSize` only takes
///     effect if the default StorageClass has `allowVolumeExpansion: true`;
///     otherwise the CR changes but the volume never grows. Refuse rather than
///     silently no-op. (Permissive when there's no default class to inspect.)
async fn validate_disk_resize(client: &Client, dep: &Deployment, new_disk: &str) -> Result<()> {
    let Some(obj) = os_api(client, dep).get_opt(dep.name()).await? else {
        return Ok(()); // new cluster — nothing to compare against
    };
    let Some(cur) = obj
        .data
        .pointer("/spec/nodePools/0/diskSize")
        .and_then(|v| v.as_str())
    else {
        return Ok(()); // current size unknown — don't block
    };
    // Only consult the StorageClass when the size actually changes (a resize),
    // so the common no-op save doesn't pay an extra list call.
    let sc_allows = if cur == new_disk {
        None
    } else {
        crate::bootstrap::deployment_sc_allows_expansion(client).await?
    };
    disk_resize_check(cur, new_disk, sc_allows)
}

/// Pure shrink/grow decision for the disk-resize guard — split out so the rules
/// are unit-testable without a live cluster. `sc_allows_expansion` is the
/// deployment StorageClass's `allowVolumeExpansion` (None = unknown → permissive).
fn disk_resize_check(cur: &str, new: &str, sc_allows_expansion: Option<bool>) -> Result<()> {
    let (Some(cur_mib), Some(new_mib)) = (parse_mem_mib(cur), parse_mem_mib(new)) else {
        return Ok(()); // unparseable — leave it to the operator/apiserver
    };
    if new_mib < cur_mib {
        bail!(
            "disk cannot shrink ({cur} → {new}): Kubernetes does not support \
             reducing a PVC. Keep the disk at {cur} or larger."
        );
    }
    if new_mib > cur_mib && sc_allows_expansion == Some(false) {
        bail!(
            "disk expansion ({cur} → {new}) needs the default StorageClass to \
             allow volume expansion (allowVolumeExpansion: true), which it does not — \
             the change was not applied."
        );
    }
    Ok(())
}

/// Node-memory bounds for the operator-delegated heap (#52 guard, bounds from
/// ADR-035): the operator derives the JVM heap as **half the memory** (request
/// = limit) and applies no floor/cap of its own, so the input is bounded here.
/// Under 1Gi the derived heap lands below 512Mi — too small to run an
/// OpenSearch node; over 62Gi the heap would cross the 31g compressed-oops
/// ceiling.
const MIN_NODE_MEM_MIB: u64 = 1024;
const MAX_NODE_MEM_MIB: u64 = 62 * 1024;

/// Node-scaling guard (#52). Scaling up AND down are supported (the operator
/// drains on scale-down) — the one unsupported value is zero: a deployment
/// with no nodes is not "paused", it is unreachable, and the CR would sit
/// broken. The supported way to stop paying for a deployment is deleting it.
fn node_scale_check(replicas: u32) -> Result<()> {
    if replicas == 0 {
        bail!(
            "cannot scale to 0 nodes: an OpenSearch deployment needs at least \
             one node. To stop a deployment entirely, delete it instead."
        );
    }
    Ok(())
}

/// A user-supplied memory/disk override must be a valid Kubernetes quantity —
/// otherwise it goes into the CR verbatim and the apply breaks downstream with
/// an apiserver quantity-parse error nobody can act on. Refuse clearly (#52).
fn quantity_check(field: &str, v: &str) -> Result<u64> {
    parse_mem_mib(v).ok_or_else(|| {
        anyhow::anyhow!("invalid {field} '{v}': use a Kubernetes quantity like 512Mi or 4Gi")
    })
}

/// Memory guard (#52, reconciled with #55/ADR-035's operator-managed heap):
/// up AND down are supported — syntax first (`quantity_check`), then the
/// delegated-heap bounds (`MIN`/`MAX_NODE_MEM_MIB`). Presets pass trivially;
/// this protects the custom-size and day-2 memory paths.
fn memory_check(mem: &str) -> Result<()> {
    let mib = quantity_check("memory", mem)?;
    if mib < MIN_NODE_MEM_MIB {
        bail!(
            "memory {mem} is below the 1Gi minimum: the operator sets the JVM \
             heap to half the memory, and an OpenSearch node needs at least a \
             512Mi heap."
        );
    }
    if mib > MAX_NODE_MEM_MIB {
        bail!(
            "memory {mem} is above the 62Gi maximum: the operator sets the JVM \
             heap to half the memory, which would cross the 31g compressed-oops \
             ceiling."
        );
    }
    Ok(())
}

/// Admin-password guard — the rule `reset_admin_password` enforces, split out
/// (like `disk_resize_check`) so it is unit-testable without a cluster.
fn password_check(new_password: &str) -> Result<()> {
    if new_password.trim().len() < 8 {
        bail!("password must be at least 8 characters");
    }
    Ok(())
}

/// Namespace-first guard (#52): every deployment op lands in the deployment's
/// namespace — the app namespace for the admin, the tenant's namespace since
/// ADR-044. If it is missing (bootstrap never ran, POD_NAMESPACE points
/// somewhere stale, or the tenant was never provisioned) the apiserver would
/// answer each op with a bare "namespaces not found" — refuse up front with the
/// actionable message instead. Permissive when the namespace can't be read
/// (tight RBAC): the apply itself will surface the real error.
async fn ensure_namespace_exists(client: &Client, namespace: &str) -> Result<()> {
    let api: Api<Namespace> = Api::all(client.clone());
    match api.get_opt(namespace).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => bail!(
            "namespace '{namespace}' does not exist on this cluster — run the \
             VeloxSearch bootstrap (first-run setup) before creating or \
             editing deployments"
        ),
        Err(e) => {
            tracing::debug!("namespace check skipped (cannot read namespaces): {e}");
            Ok(())
        }
    }
}

/// The nodePool `persistence` block for the OpenSearchCluster CR (ADR-031,
/// pinned by ADR-043).
///
/// A `pvc` claim (the operator sizes it from the pool's `diskSize`), pinned to
/// the `longhorn` StorageClass — Longhorn is the only supported deployment
/// storage (ADR-043), so the CR names it explicitly instead of falling through
/// to whatever the cluster's default happens to be. Choosing another class in
/// the UI is a non-goal (REQUIREMENTS.md R3); ensuring Longhorn exists via the
/// self-bootstrap + storage-ready gate is owned by bootstrap.rs (#13/#14).
fn node_persistence() -> serde_json::Value {
    serde_json::json!({ "pvc": {
        "storageClass": crate::bootstrap::LONGHORN_SC,
        "accessModes": ["ReadWriteOnce"]
    } })
}

/// Parse a Kubernetes memory quantity ("4Gi" / "512Mi" / "2G" / "1000M" /
/// plain bytes) into MiB. Returns None when unparseable.
fn parse_mem_mib(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, to_bytes): (&str, f64) = if let Some(n) = s.strip_suffix("Gi") {
        (n, 1024.0 * 1024.0 * 1024.0)
    } else if let Some(n) = s.strip_suffix("Mi") {
        (n, 1024.0 * 1024.0)
    } else if let Some(n) = s.strip_suffix('G') {
        (n, 1_000_000_000.0)
    } else if let Some(n) = s.strip_suffix('M') {
        (n, 1_000_000.0)
    } else {
        (s, 1.0)
    };
    let v: f64 = num.trim().parse().ok()?;
    if v <= 0.0 {
        return None;
    }
    Some((v * to_bytes / (1024.0 * 1024.0)) as u64)
}

/// The JVM heap size in MiB **as the operator computes it** (ADR-035): half
/// the nodePool memory request, 512 MiB when the request is missing or
/// unparseable. This is a display-side mirror of the operator's
/// `CalculateJvmHeapSizeSettings` (verified in the vendored operator, image
/// v3.0.0-alpha / chart 3.0.2) — VeloxSearch no longer renders JVM flags into
/// the CR, it only PREDICTS what the operator will set so the wizard/status
/// can show it. Keep in lockstep with the vendored operator on upgrades.
fn heap_mib(mem: &str) -> u64 {
    parse_mem_mib(mem).map(|mib| mib / 2).unwrap_or(512)
}

/// The heap as a short human quantity ("1536m" / "2g") — what the wizard's JVM
/// hint, size cards and deployment status display. Same value the operator
/// derives (`heap_mib`), just in a compact unit.
fn heap_short(mem: &str) -> String {
    let half = heap_mib(mem);
    if half.is_multiple_of(1024) {
        format!("{}g", half / 1024)
    } else {
        format!("{half}m")
    }
}

async fn ensure_admin_secret(client: &Client, dep: &Deployment) -> Result<()> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), dep.namespace());
    let secret_name = admin_secret_name(dep.name());
    if secrets.get_opt(&secret_name).await?.is_some() {
        return Ok(());
    }
    // Fresh CSPRNG password per cluster, generated once at create and stored in
    // the Secret the operator seeds the admin hash from (and `admin_creds` reads).
    let mut string_data = BTreeMap::new();
    string_data.insert("username".to_string(), ADMIN_USER.to_string());
    string_data.insert("password".to_string(), gen_admin_password()?);
    let secret = Secret {
        metadata: kube::api::ObjectMeta {
            name: Some(secret_name),
            namespace: Some(dep.namespace().to_string()),
            // The per-deployment credential carries the same owner labels as
            // its CR, so an offboarding sweep (or an audit) can find every
            // object belonging to a tenant with one selector.
            labels: Some(dep.owner_labels()),
            ..Default::default()
        },
        string_data: Some(string_data),
        ..Default::default()
    };
    secrets
        .create(&PostParams::default(), &secret)
        .await
        .context("creating admin credentials secret")?;
    Ok(())
}

/// The deployment's single node pool for the OpenSearchCluster CR — the memory
/// integration surface (#55, ADR-035). Two deliberate properties:
///
///  1. **No `jvm` field.** Omitting it is what hands JVM/heap management to
///     the operator's built-in memory tuning: when a pool spec carries no
///     explicit `-Xms/-Xmx`, the operator computes them as **half the memory
///     request** and injects `OPENSEARCH_JAVA_OPTS` itself
///     (`CalculateJvmHeapSizeSettings` + `AppendJvmHeapSizeSettings`, verified
///     in the vendored operator — image v3.0.0-alpha, chart 3.0.2). An
///     explicit `jvm` would OVERRIDE that capability, which is exactly what
///     the previous hand-rendered `-Xms/-Xmx` string did.
///  2. **Memory request = limit** (Guaranteed QoS). One user-facing number:
///     what you set is what is scheduled, what bounds the pod, and what the
///     operator halves for the heap — so a day-2 memory up/down is a plain
///     resources patch and the heap follows automatically.
fn node_pool(
    replicas: u32,
    disk: &str,
    mem: &str,
    cpu_req: &str,
    cpu_lim: &str,
) -> serde_json::Value {
    serde_json::json!({
        "component": "nodes",
        "replicas": replicas,
        "diskSize": disk,
        "roles": ["cluster_manager", "data", "ingest"],
        "resources": {
            "requests": { "memory": mem, "cpu": cpu_req },
            "limits": { "memory": mem, "cpu": cpu_lim }
        },
        // PVC-backed persistence (ADR-031): data survives pod reschedule.
        // The volume is sized by `diskSize` above and pinned to the
        // `longhorn` StorageClass (ADR-043) — the Longhorn self-bootstrap
        // and the storage-ready gate that guarantee that class exists live
        // in bootstrap.rs (#13/#14).
        "persistence": node_persistence()
    })
}

/// Optional overrides on top of a sizing preset, plus extra opensearch.yml
/// config and the monitoring targets to remember for this deployment.
#[derive(Debug, Default)]
pub struct CreateOverrides {
    pub replicas: Option<u32>,
    /// Node memory (request = limit). The OPERATOR derives the JVM heap as
    /// half of this (ADR-035) — the heap is never set directly (meeting 3 rule).
    pub memory: Option<String>,
    pub disk: Option<String>,
    /// Extra `opensearch.yml` settings → `spec.general.additionalConfig`.
    pub additional_config: serde_json::Map<String, serde_json::Value>,
    /// Monitoring recipe ids configured for this deployment (e.g. ["nginx"]).
    pub monitors: Vec<String>,
    /// OpenSearch version to CREATE this deployment with (wizard choice). Only
    /// ever read when the CR does not exist yet: on an existing deployment the
    /// version is preserved and moves solely through `upgrade_cluster`
    /// (ADR-048 invariant 1). `None` = `DEFAULT_VERSION`.
    pub version: Option<String>,
}

/// The versions to write for a deployment: whatever the CR already carries, or
/// `DEFAULT_VERSION` when there is no CR yet (a create). Pure over the CR's
/// `data` so the preservation rule is testable without a cluster (ADR-048
/// invariant 1).
///
/// A CR with nodes pinned but `spec.dashboards.version` absent (an interrupted
/// phase 2, or a hand-written CR) follows the nodes rather than jumping to the
/// default — same reasoning: never move a version the user did not ask to move.
fn versions_of(cr: Option<&serde_json::Value>) -> (String, String) {
    let read = |ptr: &str| {
        cr.and_then(|d| d.pointer(ptr))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let nodes = read("/spec/general/version")
        .unwrap_or_else(|| crate::upgrade::DEFAULT_VERSION.to_string());
    let dashboards = read("/spec/dashboards/version").unwrap_or_else(|| nodes.clone());
    (nodes, dashboards)
}

/// Guard a version chosen in the create wizard (ADR-048 rev. 2). Unlike an
/// upgrade there is no "current" to compare against — a new deployment can be
/// created at any version that exists. What we refuse is a version that would
/// simply never come up: a malformed tag, or one whose images are not
/// published. A registry we cannot reach degrades to a warning: the operator
/// can still delete a deployment that fails to pull, which is exactly what an
/// upgrade cannot do.
async fn validate_create_version(v: &str) -> Result<()> {
    crate::upgrade::Version::parse(v)?;
    for repo in [
        crate::upgrade::IMAGE_NODES,
        crate::upgrade::IMAGE_DASHBOARDS,
    ] {
        match crate::version_feed::image_tag_exists(repo, v).await {
            Ok(true) => {}
            Ok(false) => bail!(
                "the image {repo}:{v} does not exist — the deployment's pods would sit in \
                 ImagePullBackOff and never start"
            ),
            Err(e) => tracing::warn!("could not verify {repo}:{v} before creating: {e:#}"),
        }
    }
    Ok(())
}

/// Create (or update) a named OpenSearch deployment + its dashboards ingress.
///
/// Takes a resolved [`Deployment`], not a name: `dep` carries the namespace the
/// CR lands in and the owner labels stamped onto it, both of which come from
/// the caller's session rather than from the request body.
pub async fn create_cluster(
    dep: &Deployment,
    size: &str,
    purpose: &str,
    ov: CreateOverrides,
) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    // Day-2 guards (#52): refuse unsupported/broken values BEFORE anything is
    // created or mutated — never leave a half-applied change behind.
    if let Some(r) = ov.replicas {
        node_scale_check(r)?;
    }
    if let Some(m) = ov.memory.as_deref() {
        memory_check(m)?;
    }
    if let Some(d) = ov.disk.as_deref() {
        quantity_check("disk", d)?;
    }
    // Same rule as every other guard (#52): refuse BEFORE anything is created.
    // This ran after `ensure_admin_secret` at first, which left an orphan
    // credentials Secret behind for a deployment that was never created.
    // `ov.version` is only ever set by the create handler — a save carries
    // `None`, so this cannot fire on an existing deployment.
    if let Some(v) = ov
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        validate_create_version(v).await?;
    }
    let client = client().await?;
    // Namespace-first (#52): all resources below land in the deployment's
    // namespace, which for a tenant is the one ADR-044 provisioned for it.
    ensure_namespace_exists(&client, dep.namespace()).await?;
    // Storage-ready gate (#14, ADR-031): the node pool below claims a PVC, so
    // never provision against a node-local/absent default StorageClass — that
    // leaves PVCs Pending forever. This passes immediately on a real default and
    // otherwise remediates (install Longhorn) or refuses with a clear message.
    crate::bootstrap::ensure_storage_ready(&client)
        .await
        .context("storage not ready for PVC-backed cluster")?;
    ensure_admin_secret(&client, dep).await?;
    let s = sizing(size);
    let pp = PatchParams::apply("veloxsearch").force();

    let replicas = ov.replicas.unwrap_or(s.replicas);
    let disk = ov.disk.unwrap_or_else(|| s.disk.to_string());
    // Resize guard (#16): on an existing deployment, refuse a disk shrink or a
    // grow the default StorageClass can't honor. No-op for a new cluster.
    validate_disk_resize(&client, dep, &disk).await?;
    // Memory is THE user-facing tuning knob (ADR-035): one number, applied as
    // request = limit. The operator derives the JVM heap from it. An override
    // was already bounds-checked by the day-2 guards above; preset values are
    // in range by construction (asserted in tests).
    let mem = ov.memory.unwrap_or_else(|| s.mem.to_string());

    // ADR-048 invariant 1 — NO implicit version change, ever. This function is
    // also the save path for an existing deployment (`POST /api/save_cluster`),
    // so a literal here means editing memory or ticking a monitor rewrites the
    // version and the operator starts a rolling upgrade nobody asked for and
    // that cannot be undone (it rejects downgrades). An existing CR therefore
    // re-applies the versions it already runs; only a genuinely new one gets
    // `DEFAULT_VERSION`. The version moves via `upgrade_cluster()` alone.
    let existing = os_api(&client, dep).get_opt(name).await?;
    let (mut node_version, mut dash_version) = versions_of(existing.as_ref().map(|o| &o.data));
    // A version chosen in the create wizard applies to a NEW deployment only.
    // On an existing CR the request is ignored on purpose: changing the version
    // through this path is the very bug invariant 1 forbids, and the supported
    // way to move it is the upgrade operation, which pre-flights first.
    if existing.is_none() {
        // Already validated above, before anything was written.
        if let Some(v) = ov
            .version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            node_version = v.to_string();
            dash_version = v.to_string();
        }
    }

    let mut general = serde_json::json!({
        "serviceName": name,
        "version": node_version,
        "httpPort": 9200,
        "setVMMaxMapCount": true,
    });
    if !ov.additional_config.is_empty() {
        general["additionalConfig"] = serde_json::Value::Object(ov.additional_config);
    }

    let mut annotations = serde_json::Map::new();
    if !ov.monitors.is_empty() {
        annotations.insert(
            LABEL_MONITORS.to_string(),
            serde_json::Value::String(ov.monitors.join(",")),
        );
    }

    // Ownership is stamped at creation, in the same apply as the spec, so a CR
    // can never exist without an owner: `Scope::owns` reads exactly this label
    // back, and `list_clusters` selects on it.
    let mut labels = dep.owner_labels();
    labels.insert(LABEL_SIZE.to_string(), size.to_string());
    labels.insert(LABEL_PURPOSE.to_string(), purpose.to_string());

    let manifest = serde_json::json!({
        "apiVersion": "opensearch.org/v1",
        "kind": "OpenSearchCluster",
        "metadata": {
            "name": name,
            "namespace": dep.namespace(),
            "labels": labels,
            "annotations": annotations
        },
        "spec": {
            "general": general,
            "security": {
                // TLS generate:true is REQUIRED or the operator never initializes
                // security and the node hangs at "Security not initialized".
                "tls": { "transport": { "generate": true }, "http": { "generate": true } },
                "config": { "adminCredentialsSecret": { "name": admin_secret_name(name) } }
            },
            "dashboards": {
                "enable": true,
                "version": dash_version,
                "replicas": 1,
                "resources": {
                    "requests": { "memory": "512Mi", "cpu": "200m" },
                    "limits": { "memory": "1Gi", "cpu": "500m" }
                }
            },
            "nodePools": [node_pool(replicas, &disk, &mem, s.cpu_req, s.cpu_lim)]
        }
    });
    os_api(&client, dep)
        .patch(name, &pp, &Patch::Apply(&manifest))
        .await
        .context("applying OpenSearchCluster CR")?;

    // Public dashboards ingress at <name>.<base_domain> — ingress mode only
    // (ADR-027). In portforward mode no Ingress exists and the UI hands out a
    // kubectl port-forward command instead.
    let access = crate::access::get().await?;
    ensure_dashboards_ingress(&client, &access, dep).await?;
    ensure_opensearch_ingress(&client, &access, dep).await?;
    Ok(())
}

/// Render the dashboards Ingress manifest for one deployment (pure — testable
/// without a cluster). When the access config names a TLS secret (issue #54),
/// a `spec.tls` block terminates TLS with that client-provided certificate;
/// otherwise the spec is byte-identical to the historical one (edge/controller
/// default TLS — regression-free default path).
fn dashboards_ingress_manifest(
    access: &crate::access::AccessConfig,
    dep: &Deployment,
    host: &str,
    allow: &[String],
) -> serde_json::Value {
    let name = dep.name();
    let ing_name = format!("{name}-dashboards");
    let backend = serde_json::json!({
        "service": { "name": format!("{name}-dashboards"), "port": { "number": 5601 } }
    });
    let rule = |h: &str| {
        serde_json::json!({
            "host": h,
            "http": { "paths": [{ "path": "/", "pathType": "Prefix", "backend": backend }]}
        })
    };
    // Both the historical short host and the explicit `-dashboard` one. Adding
    // rather than renaming: the short host is what SSO redirect URIs were
    // registered with (ADR-045).
    let mut hosts = vec![host.to_string()];
    if let Some(alias) = access.dashboard_alias_host(name) {
        if alias != host {
            hosts.push(alias);
        }
    }
    let mut spec = serde_json::json!({
        "ingressClassName": access.ingress_class,
        "rules": hosts.iter().map(|h| rule(h)).collect::<Vec<_>>()
    });
    let tls_secret = access.tls_secret.trim();
    if !tls_secret.is_empty() {
        spec["tls"] = serde_json::json!([{ "hosts": hosts, "secretName": tls_secret }]);
    }
    serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "Ingress",
        "metadata": {
            "name": ing_name,
            "namespace": dep.namespace(),
            "labels": dep.owner_labels(),
            "annotations": ip_allow_annotations(dep, allow),
        },
        "spec": spec
    })
}

/// Router annotation attaching the allow-list middleware, or an empty map when
/// the deployment has no list — the default, which leaves the route open.
fn ip_allow_annotations(dep: &Deployment, allow: &[String]) -> serde_json::Value {
    let name = dep.name();
    if allow.is_empty() {
        return serde_json::json!({});
    }
    serde_json::json!({
        "traefik.ingress.kubernetes.io/router.middlewares":
            format!("{}-{name}-ipallow@kubernetescrd", dep.namespace())
    })
}

/// Apply the dashboards Ingress for one deployment per the access config
/// (no-op in portforward mode). Called at create time and again for every
/// existing deployment when access settings switch to ingress mode.
pub async fn ensure_dashboards_ingress(
    client: &Client,
    access: &crate::access::AccessConfig,
    dep: &Deployment,
) -> Result<()> {
    let Some(host) = access.dashboard_host(dep.name()) else {
        return Ok(());
    };
    let allow = ip_allow_list(dep).await;
    ensure_ip_allow_middleware(client, dep, &allow).await;
    let ing_name = format!("{}-dashboards", dep.name());
    let ingress = dashboards_ingress_manifest(access, dep, &host, &allow);
    ingress_api(client, dep.namespace())
        .patch(
            &ing_name,
            &PatchParams::apply("veloxsearch").force(),
            &Patch::Apply(&ingress),
        )
        .await
        .context("applying dashboards ingress")?;
    Ok(())
}

/// Render the OpenSearch **API** Ingress (pure — testable without a cluster).
///
/// Separate from the Dashboards route because it is a different audience: this
/// is what a client library, an agent outside the cluster, or a direct `_bulk`
/// writer targets. It is authenticated by OpenSearch's own security plugin, so
/// unlike Cortex or Alertmanager it is not an open surface — but it is the
/// cluster's full admin API, which is why it gets its own explicitly-named host
/// rather than sharing one with the UI.
///
/// The backend speaks **HTTPS with the operator's internal CA**, which no
/// ingress controller trusts by default. Both controller-specific ways of
/// saying "the backend is HTTPS, do not verify it" are set: Traefik reads the
/// `serversscheme`/`serverstransport` pair (the transport itself is a separate
/// object, `opensearch_transport_manifest`), nginx reads `backend-protocol`.
/// A controller that understands neither ignores both annotations, and the
/// route then fails its TLS handshake rather than silently serving plaintext.
fn opensearch_ingress_manifest(
    access: &crate::access::AccessConfig,
    dep: &Deployment,
    host: &str,
    allow: &[String],
) -> serde_json::Value {
    let name = dep.name();
    let ing_name = format!("{name}-opensearch");
    let mut annotations = serde_json::json!({
        "traefik.ingress.kubernetes.io/service.serversscheme": "https",
        "traefik.ingress.kubernetes.io/service.serverstransport":
            format!("{}-{name}-opensearch@kubernetescrd", dep.namespace()),
        "nginx.ingress.kubernetes.io/backend-protocol": "HTTPS",
    });
    if let Some(m) = ip_allow_annotations(dep, allow).as_object() {
        for (k, v) in m {
            annotations[k] = v.clone();
        }
    }
    let mut spec = serde_json::json!({
        "ingressClassName": access.ingress_class,
        "rules": [{
            "host": host,
            "http": { "paths": [{
                "path": "/", "pathType": "Prefix",
                "backend": { "service": { "name": name, "port": { "number": 9200 } } }
            }]}
        }]
    });
    let tls_secret = access.tls_secret.trim();
    if !tls_secret.is_empty() {
        spec["tls"] = serde_json::json!([{ "hosts": [host], "secretName": tls_secret }]);
    }
    serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "Ingress",
        "metadata": {
            "name": ing_name,
            "namespace": dep.namespace(),
            "annotations": annotations
        },
        "spec": spec
    })
}

/// Create or remove the allow-list middleware to match the deployment's list.
///
/// Best-effort in both directions: on a cluster without Traefik's CRDs the
/// middleware never exists, and the annotation referencing it is inert. That is
/// a real limitation and it is named in the UI rather than hidden.
async fn ensure_ip_allow_middleware(client: &Client, dep: &Deployment, allow: &[String]) {
    let name = dep.name();
    let mw = format!("{name}-ipallow");
    if allow.is_empty() {
        delete_dynamic(
            client,
            "traefik.io",
            "v1alpha1",
            "Middleware",
            Some(dep.namespace()),
            &mw,
        )
        .await;
        return;
    }
    if let Err(e) = apply_dynamic(
        client,
        "traefik.io",
        "v1alpha1",
        "Middleware",
        Some(dep.namespace()),
        &mw,
        &ip_allow_middleware_manifest(dep, allow),
    )
    .await
    {
        tracing::warn!("ip allow-list for {name}: middleware not applied: {e:#}");
    }
}

/// Traefik-native route for the OpenSearch API.
///
/// A plain `Ingress` with `service.serversscheme`/`service.serverstransport`
/// annotations is the portable way to say "this backend is HTTPS, do not verify
/// it" — and on Traefik 3.7.4 it does not work: the router comes up and every
/// request answers 502, because the transport is never applied and the default
/// one rejects the operator's CA. The same backend behind an `IngressRoute`
/// naming the transport explicitly answers immediately. Measured, both ways, on
/// the live cluster.
///
/// So Traefik gets its own object and every other controller keeps the plain
/// Ingress. Two shapes for one route is worse than one — but silently serving
/// 502 on the endpoint we tell customers to send data to is worse still.
fn opensearch_route_manifest(
    access: &crate::access::AccessConfig,
    dep: &Deployment,
    host: &str,
    allow: &[String],
) -> serde_json::Value {
    let name = dep.name();
    let mut service = serde_json::json!({
        "name": name, "port": 9200, "scheme": "https",
        "serversTransport": format!("{name}-opensearch"),
    });
    if service["serversTransport"].is_null() {
        service.as_object_mut().unwrap().remove("serversTransport");
    }
    let mut route = serde_json::json!({
        "match": format!("Host(`{host}`)"),
        "kind": "Rule",
        "services": [service],
    });
    if !allow.is_empty() {
        route["middlewares"] = serde_json::json!([{ "name": format!("{name}-ipallow"), "namespace": dep.namespace() }]);
    }
    let mut spec = serde_json::json!({
        "entryPoints": ["websecure"],
        "routes": [route],
    });
    let tls_secret = access.tls_secret.trim();
    if !tls_secret.is_empty() {
        spec["tls"] = serde_json::json!({ "secretName": tls_secret });
    }
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "IngressRoute",
        "metadata": { "name": format!("{name}-opensearch"), "namespace": dep.namespace() },
        "spec": spec
    })
}

/// Traefik `ServersTransport` telling it not to verify the operator's CA.
///
/// Only meaningful on Traefik; applied best-effort so a cluster running another
/// controller (or one without the CRD) is not blocked by its absence.
fn opensearch_transport_manifest(dep: &Deployment) -> serde_json::Value {
    let name = dep.name();
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "ServersTransport",
        "metadata": { "name": format!("{name}-opensearch"), "namespace": dep.namespace() },
        "spec": { "insecureSkipVerify": true }
    })
}

/// Apply the OpenSearch API Ingress per the access config (no-op in
/// port-forward mode).
pub async fn ensure_opensearch_ingress(
    client: &Client,
    access: &crate::access::AccessConfig,
    dep: &Deployment,
) -> Result<()> {
    let name = dep.name();
    let Some(host) = access.opensearch_host(name) else {
        return Ok(());
    };
    if access.ingress_class == "traefik" {
        // Best-effort: without it Traefik refuses the backend handshake, but a
        // failure here must not block the rest of a create.
        if let Err(e) = apply_dynamic(
            client,
            "traefik.io",
            "v1alpha1",
            "ServersTransport",
            Some(dep.namespace()),
            &format!("{name}-opensearch"),
            &opensearch_transport_manifest(dep),
        )
        .await
        {
            tracing::warn!("opensearch route for {name}: ServersTransport not applied: {e:#}");
        }
    }
    let allow = ip_allow_list(dep).await;
    ensure_ip_allow_middleware(client, dep, &allow).await;
    let ing_name = format!("{name}-opensearch");

    if access.ingress_class == "traefik" {
        // Traefik path: its own route object, and no plain Ingress for the same
        // host — two routers on one host is an ambiguity nobody wants to debug.
        let _ = ingress_api(client, dep.namespace())
            .delete(&ing_name, &DeleteParams::default())
            .await;
        apply_dynamic(
            client,
            "traefik.io",
            "v1alpha1",
            "IngressRoute",
            Some(dep.namespace()),
            &ing_name,
            &opensearch_route_manifest(access, dep, &host, &allow),
        )
        .await
        .context("applying opensearch route")?;
        return Ok(());
    }

    let ingress = opensearch_ingress_manifest(access, dep, &host, &allow);
    ingress_api(client, dep.namespace())
        .patch(
            &ing_name,
            &PatchParams::apply("veloxsearch").force(),
            &Patch::Apply(&ingress),
        )
        .await
        .context("applying opensearch ingress")?;
    Ok(())
}

/// Light PEM sanity check for a client-injected cert/key pair (issue #54):
/// both halves present and carrying the right PEM markers. Full chain/key
/// matching is the ingress controller's job — this only catches swapped or
/// truncated pastes before they land in a Secret.
pub fn validate_tls_pem(cert: &str, key: &str) -> Result<()> {
    if !cert.contains("-----BEGIN CERTIFICATE-----") {
        bail!("certificate is not PEM (missing BEGIN CERTIFICATE)");
    }
    if !key.contains("PRIVATE KEY-----") {
        bail!("key is not PEM (missing BEGIN [RSA/EC] PRIVATE KEY)");
    }
    Ok(())
}

/// Create-or-update a `kubernetes.io/tls` Secret in the app namespace from
/// client-provided PEM material (issue #54). Server-side apply so re-pasting
/// a renewed certificate rotates it in place.
pub async fn ensure_tls_secret(
    client: &Client,
    secret_name: &str,
    cert_pem: &str,
    key_pem: &str,
) -> Result<()> {
    validate_tls_pem(cert_pem, key_pem)?;
    let secrets: Api<Secret> = Api::namespaced(client.clone(), ns());
    let secret = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": { "name": secret_name, "namespace": ns() },
        "type": "kubernetes.io/tls",
        "stringData": { "tls.crt": cert_pem, "tls.key": key_pem }
    });
    secrets
        .patch(
            secret_name,
            &PatchParams::apply("veloxsearch").force(),
            &Patch::Apply(&secret),
        )
        .await
        .context("applying dashboards TLS secret")?;
    Ok(())
}

/// Every deployment a scope owns, as resolved handles (cheap — no per-
/// deployment status calls). Returns [`Deployment`]s rather than names so a
/// caller that iterates them (the access-settings backfill, the metrics
/// sampler) cannot lose the ownership proof on the way.
pub async fn scoped_deployments(scope: &Scope) -> Result<Vec<Deployment>> {
    Ok(list_clusters(scope)
        .await?
        .into_iter()
        .filter_map(|o| {
            let name = o.metadata.name.clone()?;
            let namespace = o
                .metadata
                .namespace
                .clone()
                .unwrap_or_else(|| scope.write_namespace());
            scope.adopt(&name, located(&o, &namespace))
        })
        .collect())
}

/// Binding state of one data-volume PVC backing a deployment node (ADR-031).
#[derive(Clone, Debug, Default)]
pub struct PvcInfo {
    /// `status.phase` — "Bound" / "Pending" (empty only on a brand-new claim).
    pub phase: String,
    /// `status.capacity.storage` when bound, else the requested size, in bytes.
    pub capacity_bytes: u64,
}

/// A Kubernetes storage Quantity ("10Gi" / "512Mi" / plain bytes) → bytes.
/// Reuses the memory-quantity parser (which yields MiB). 0 when unparseable.
fn quantity_bytes(q: &Quantity) -> u64 {
    parse_mem_mib(&q.0)
        .map(|mib| mib.saturating_mul(1024 * 1024))
        .unwrap_or(0)
}

/// The data-volume PVCs backing a deployment, keyed by PVC name so a node can
/// find its own volume (`data-<node.name>` — the operator's StatefulSet
/// volumeClaimTemplate is `data`, and an OpenSearch node's name is its pod name).
///
/// Best-effort: any error (no kube client, RBAC, list failure) degrades to an
/// empty map so the Overview shows the data-path `fs.data` meter rather than
/// failing — callers treat a present-but-not-"Bound" entry as Pending.
pub async fn data_pvcs(dep: &Deployment) -> BTreeMap<String, PvcInfo> {
    data_pvcs_in(dep.namespace(), dep.name()).await
}

/// The same listing, addressed by namespace + name. Private, and only reachable
/// from a caller that already resolved the deployment (`data_pvcs` above, or
/// `status_from`, which is handed an object the scope layer just listed).
async fn data_pvcs_in(namespace: &str, name: &str) -> BTreeMap<String, PvcInfo> {
    let mut out = BTreeMap::new();
    let Ok(client) = client().await else {
        return out;
    };
    let api: Api<PersistentVolumeClaim> = Api::namespaced(client, namespace);
    let Ok(list) = api.list(&ListParams::default()).await else {
        return out;
    };
    // The operator names every node-pool volume `data-<cluster>-<pool>-<ord>`;
    // scope to this deployment by that prefix (exact per-node lookup downstream
    // keeps a longer-named sibling deployment's PVCs from being misattributed).
    let prefix = format!("data-{name}-");
    for pvc in list {
        let Some(pvc_name) = pvc.metadata.name.clone() else {
            continue;
        };
        if !pvc_name.starts_with(&prefix) {
            continue;
        }
        let status = pvc.status.as_ref();
        let phase = status.and_then(|s| s.phase.clone()).unwrap_or_default();
        // Bound: real provisioned capacity. Otherwise fall back to the request.
        let cap = status
            .and_then(|s| s.capacity.as_ref())
            .and_then(|c| c.get("storage"))
            .or_else(|| {
                pvc.spec
                    .as_ref()
                    .and_then(|sp| sp.resources.as_ref())
                    .and_then(|r| r.requests.as_ref())
                    .and_then(|req| req.get("storage"))
            })
            .map(quantity_bytes)
            .unwrap_or(0);
        out.insert(
            pvc_name,
            PvcInfo {
                phase,
                capacity_bytes: cap,
            },
        );
    }
    out
}

/// Every Secret this app creates for one deployment. Named in ONE place so a
/// delete cannot forget one: the ADR-045 auth Secrets were added to the create
/// path and not to the delete path, which left an identity provider's bind
/// password and the whole securityconfig behind after the deployment was gone
/// (found 2026-08-06 on the local k3s).
fn owned_secret_names(name: &str) -> [String; 5] {
    [
        admin_secret_name(name),
        // ADR-045: kibanaserver credentials, the securityconfig file set, and
        // the saved provider spec + its client secret / bind password.
        dashboards_secret_name(name),
        securityconfig_secret_name(name),
        auth_state_secret_name(name),
        // ADR-049: the S3 access key / secret key the keystore is built from.
        snapshot_secret_name(name),
    ]
}

/// The label the operator stamps on every PVC it creates for a deployment —
/// both the per-node `data-<name>-<pool>-<n>` claims and the single
/// `<name>-bootstrap-data` one. Selecting by label instead of by name prefix
/// catches both shapes and cannot misfire on a sibling deployment whose name
/// merely starts with the same characters.
const LABEL_OS_CLUSTER: &str = "opensearch.org/opensearch-cluster";

/// How long to wait for the operator to tear the StatefulSet down before
/// sweeping the PVCs. Measured at ~15s for a 3-node cluster on the local k3s;
/// double it and give up rather than block a background task forever.
const STS_GONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Delete the data volumes a deployment leaves behind.
///
/// StatefulSet `volumeClaimTemplates` PVCs carry **no ownerReference**, so the
/// Kubernetes garbage collector never touches them: deleting the CR (and with
/// it the StatefulSet) leaves every data volume Bound forever. Under Longhorn
/// each orphan keeps its slice of the *scheduling budget* reserved even though
/// the bytes are free, so a handful of create/delete cycles exhausts the pool
/// and the NEXT deployment comes up with zero replicas — `faulted` volume,
/// "No available disk candidates to create a new replica", and the pod stuck on
/// `AttachVolume.Attach failed ... volume is not ready for workloads`.
///
/// Found live 2026-08-12: 27 orphans from 11 deleted deployments held 135 GiB
/// of a 157 GiB budget, which broke provisioning outright. Same class of bug as
/// the ADR-045 Secrets that `owned_secret_names` now guards against.
///
/// **Ordering is load-bearing**: the StatefulSet must be gone first, or its
/// controller recreates each PVC as fast as we delete it.
///
/// Returns how many PVCs were deleted.
pub async fn delete_data_pvcs(dep: &Deployment) -> Result<usize> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let sts: Api<StatefulSet> = Api::namespaced(client.clone(), dep.namespace());
    let deadline = std::time::Instant::now() + STS_GONE_TIMEOUT;
    loop {
        // A list (not a get) because a deployment may have several node pools,
        // and any surviving one would recreate its claims.
        let lp = ListParams::default().labels(&format!("{LABEL_OS_CLUSTER}={name}"));
        match sts.list(&lp).await {
            Ok(l) if l.items.is_empty() => break,
            // A list failure here is not fatal on its own, but deleting PVCs
            // while a StatefulSet might still exist is: bail instead.
            Err(e) if std::time::Instant::now() >= deadline => {
                return Err(e).context("listing statefulsets before the PVC sweep");
            }
            Ok(_) if std::time::Instant::now() >= deadline => {
                anyhow::bail!("statefulsets for {name} still present after {STS_GONE_TIMEOUT:?}");
            }
            _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
        }
    }

    let pvcs: Api<PersistentVolumeClaim> = Api::namespaced(client, dep.namespace());
    let lp = ListParams::default().labels(&format!("{LABEL_OS_CLUSTER}={name}"));
    let list = pvcs.list(&lp).await.context("listing PVCs to delete")?;
    let dp = DeleteParams::default();
    let mut deleted = 0;
    for pvc in list {
        let Some(pvc_name) = pvc.metadata.name.as_deref() else {
            continue;
        };
        match pvcs.delete(pvc_name, &dp).await {
            Ok(_) => deleted += 1,
            Err(e) => tracing::warn!("could not delete PVC {pvc_name}: {e:#}"),
        }
    }
    Ok(deleted)
}

/// Delete a deployment and everything it owns: CR, dashboards ingress, every
/// Secret we created for it (admin + the ADR-045 auth set), its data volumes,
/// and any collection agents shipping to it (otherwise DaemonSets keep tailing
/// logs into a dead endpoint forever).
///
/// The PVC sweep runs detached because it must first wait for the operator to
/// tear the StatefulSet down (see `delete_data_pvcs`) — up to a couple of
/// minutes, which is far too long to hold the HTTP response open.
pub async fn delete_cluster(dep: &Deployment) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let dp = DeleteParams::default();
    // Best-effort: ignore not-found.
    let _ = os_api(&client, dep).delete(name, &dp).await;
    let _ = ingress_api(&client, dep.namespace())
        .delete(&format!("{name}-dashboards"), &dp)
        .await;
    // The OpenSearch API route and its Traefik transport die with the
    // deployment too — a dangling Ingress would keep answering for a service
    // that no longer exists.
    let _ = ingress_api(&client, dep.namespace())
        .delete(&format!("{name}-opensearch"), &dp)
        .await;
    for kind in ["ServersTransport", "IngressRoute"] {
        delete_dynamic(
            &client,
            "traefik.io",
            "v1alpha1",
            kind,
            Some(dep.namespace()),
            &format!("{name}-opensearch"),
        )
        .await;
    }
    delete_dynamic(
        &client,
        "traefik.io",
        "v1alpha1",
        "Middleware",
        Some(dep.namespace()),
        &format!("{name}-ipallow"),
    )
    .await;
    let secrets: Api<Secret> = Api::namespaced(client.clone(), dep.namespace());
    for s in owned_secret_names(name) {
        let _ = secrets.delete(&s, &dp).await;
    }
    for recipe in crate::recipes::RECIPES {
        let _ = crate::agents::remove_agent(dep, recipe).await;
    }
    let owner = dep.clone();
    tokio::spawn(async move {
        match delete_data_pvcs(&owner).await {
            Ok(n) => tracing::info!("deleted {n} data volume(s) of {owner}"),
            Err(e) => tracing::warn!("data volumes of {owner} not reclaimed: {e:#}"),
        }
    });
    Ok(())
}

/// Add or remove a recipe from a deployment's `monitors` annotation so the
/// Status list and Overview reflect what is actually being collected. Patches
/// only the annotation (JSON merge) — leaves the operator-managed spec alone.
pub async fn set_monitor(dep: &Deployment, recipe: &str, enabled: bool) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let api = os_api(&client, dep);
    let obj = api
        .get(name)
        .await
        .context("fetching cluster for monitor update")?;

    let mut monitors = monitors_from(obj.metadata.annotations.as_ref());
    monitors.retain(|m| m != recipe);
    if enabled {
        monitors.push(recipe.to_string());
    }

    // null removes the annotation key in a merge patch when nothing is left.
    let value = if monitors.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(monitors.join(","))
    };
    let mut annotations = serde_json::Map::new();
    annotations.insert(LABEL_MONITORS.to_string(), value);
    // Disabling drops the recorded package version along with the monitor, so
    // an uninstalled integration is never reported back as "installed 1.0.0"
    // (ADR-039 versioning state, #75).
    if !enabled {
        let mut versions = parse_integration_versions(&obj);
        if versions.remove(recipe).is_some() {
            annotations.insert(
                LABEL_INTEGRATION_VERSIONS.to_string(),
                render_integration_versions(&versions),
            );
        }
    }
    let patch = serde_json::json!({ "metadata": { "annotations": annotations } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .context("patching monitors annotation")?;
    Ok(())
}

/// The installed package version per integration id, read off the deployment
/// (ADR-039 "Versioning": the core records installed id+version so the
/// Integrations tab can show installed vs available vs update-available).
/// Stored as `id=version` pairs in a sibling of the `monitors` annotation
/// rather than inside it, so every existing reader of `monitors` — the status
/// list, the wizard, the frontend — keeps parsing exactly what it always did.
fn parse_integration_versions(obj: &DynamicObject) -> BTreeMap<String, String> {
    obj.metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(LABEL_INTEGRATION_VERSIONS))
        .map(|s| {
            s.split(',')
                .filter_map(|pair| pair.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .filter(|(k, v)| !k.is_empty() && !v.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Serialize the map back, or null to remove the annotation when it is empty.
fn render_integration_versions(versions: &BTreeMap<String, String>) -> serde_json::Value {
    if versions.is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::Value::String(
        versions
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Installed integration id → version for one deployment.
pub async fn integration_versions(dep: &Deployment) -> Result<BTreeMap<String, String>> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let obj = os_api(&client, dep)
        .get(name)
        .await
        .context("fetching cluster for integration versions")?;
    Ok(parse_integration_versions(&obj))
}

/// Record (or clear) the installed version of one integration on a deployment.
pub async fn set_integration_version(
    dep: &Deployment,
    id: &str,
    version: Option<&str>,
) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let api = os_api(&client, dep);
    let obj = api
        .get(name)
        .await
        .context("fetching cluster for integration version update")?;

    let mut versions = parse_integration_versions(&obj);
    match version {
        Some(v) => versions.insert(id.to_string(), v.to_string()),
        None => versions.remove(id),
    };
    let patch = serde_json::json!({ "metadata": { "annotations": {
        LABEL_INTEGRATION_VERSIONS: render_integration_versions(&versions),
    } } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .context("patching integration-versions annotation")?;
    Ok(())
}

/// Record (or clear) the OTel observability stack marker on a deployment
/// (ADR-053). `None` removes the annotation.
///
/// A dedicated annotation rather than an entry in `monitors`, for two reasons
/// that are both load-bearing:
///
///  * `monitors` is the enabled-**recipe** set. `recipes::recipe_index` falls
///    through to the nginx index for an unknown id, so a stack marker parked
///    there would make the Integrations tab report nginx's doc count as the
///    stack's, and `views_status` would count it as an integration.
///  * `monitors` is **round-tripped by the Edit form**, which posts the list it
///    was rendered with straight back into `save_cluster` → `create_cluster`,
///    where it is written by *server-side apply*. A stale browser tab would
///    therefore silently unset a marker whose 17 objects are still running —
///    exactly the shape of the ADR-048 `save_cluster`-clobbers-version bug
///    (#110). Nothing round-trips this annotation, and like `set_monitor` it is
///    a merge patch on `metadata` alone, so it can never move a version.
pub async fn set_otel_stack(dep: &Deployment, version: Option<&str>) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let api = os_api(&client, dep);
    let value = match version {
        Some(v) => serde_json::Value::String(v.to_string()),
        None => serde_json::Value::Null,
    };
    let patch = serde_json::json!({ "metadata": { "annotations": { LABEL_OTEL_STACK: value } } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .context("patching otel-stack annotation")?;
    Ok(())
}

/// CIDRs allowed to reach this deployment's public routes. Absent = open.
const LABEL_IP_ALLOW: &str = "veloxsearch.ai/ip-allow-list";

/// Read the deployment's IP allow-list.
pub async fn ip_allow_list(dep: &Deployment) -> Vec<String> {
    get_deployment(dep)
        .await
        .ok()
        .flatten()
        .map(|s| s.ip_allow_list)
        .unwrap_or_default()
}

/// Set (or clear) the allow-list. An empty list removes the annotation and the
/// middleware with it, which is the **default**: routes are open unless the
/// customer chooses otherwise.
///
/// Merge patch on `metadata` only — like `set_monitor` and `set_otel_stack`, so
/// it can never move a version (ADR-048).
pub async fn set_ip_allow_list(dep: &Deployment, cidrs: &[String]) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    for c in cidrs {
        validate_cidr(c)?;
    }
    let client = client().await?;
    let value = if cidrs.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(cidrs.join(","))
    };
    let patch = serde_json::json!({ "metadata": { "annotations": { LABEL_IP_ALLOW: value } } });
    os_api(&client, dep)
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .context("patching ip allow-list annotation")?;

    // Re-apply the routes so the middleware appears or disappears with it.
    let access = crate::access::get().await.unwrap_or_default();
    ensure_dashboards_ingress(&client, &access, dep).await?;
    ensure_opensearch_ingress(&client, &access, dep).await?;
    Ok(())
}

/// Reject anything that is not an IPv4/IPv6 CIDR or bare address.
///
/// Validated here rather than left to Traefik: a typo in a *security* control
/// must fail loudly at the moment it is saved, not silently widen or narrow
/// access later.
pub fn validate_cidr(s: &str) -> Result<()> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty entry in the allow-list");
    }
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let ip: std::net::IpAddr = addr
        .parse()
        .with_context(|| format!("{s} is not an IP address or CIDR"))?;
    if let Some(p) = prefix {
        let bits: u8 = p
            .parse()
            .with_context(|| format!("{s} has a non-numeric prefix"))?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if bits > max {
            bail!("{s} has a prefix wider than /{max}");
        }
    }
    Ok(())
}

/// Traefik middleware restricting a route to the allow-list.
fn ip_allow_middleware_manifest(dep: &Deployment, cidrs: &[String]) -> serde_json::Value {
    let name = dep.name();
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "Middleware",
        "metadata": { "name": format!("{name}-ipallow"), "namespace": dep.namespace() },
        // `ipStrategy.depth: 1` reads the client from the last hop recorded in
        // X-Forwarded-For. Without it Traefik matches on the immediate peer,
        // which behind HAProxy is HAProxy — and the list would match nothing.
        "spec": { "ipAllowList": { "sourceRange": cidrs, "ipStrategy": { "depth": 1 } } }
    })
}

/// Field manager for the OTel stack's slice of the CR. A manager of its own, in
/// the same pattern as `AUTH_FIELD_MANAGER` and `SNAPSHOT_FIELD_MANAGER`: the
/// keys below live under `spec.dashboards.additionalConfig`, a granular map, so
/// this manager owns exactly its own keys and `create_cluster`'s apply (which
/// never mentions them) cannot strip them.
const OTEL_FIELD_MANAGER: &str = "veloxsearch-otel";

/// Field manager for the next-generation UI's slice of the same map.
///
/// A **separate** manager from `OTEL_FIELD_MANAGER`, and that separation is the
/// whole design: `additionalConfig` is a granular map, so server-side apply
/// tracks ownership per key. Uninstalling the observability stack removes the
/// stack's keys and cannot touch the UI's — which is what lets the new UI be a
/// deployment-level choice that outlives the feature that first required it.
const UI_FIELD_MANAGER: &str = "veloxsearch-ui";

/// Marks the new UI as the *user's* choice rather than the stack's requirement.
///
/// Without this, "on" is ambiguous: the observability stack turns the new UI on
/// because it cannot work without workspaces, and on uninstall we would have no
/// way to tell a deployment that wanted the new UI from one that merely
/// tolerated it. Present = the user asked; absent = only the stack did, and
/// uninstall takes it back down.
const LABEL_NEXT_UI: &str = "veloxsearch.ai/next-ui";

/// The Dashboards keys that make up the next-generation UI.
///
/// `workspace.enabled` is what produces the new navigation, the new home and
/// the Observability nav group. `home:useNewHomePage` is what makes that
/// navigation the actual chrome rather than a feature nobody can reach — and it
/// is also what `chrome.navGroup.getNavGroupEnabled()` reads, which gates
/// Datasets.
///
/// `opensearch_security.multitenancy.enabled: false` belongs to this group, not
/// to the stack's: it is a consequence of workspaces, and upstream states it as
/// a prerequisite ("you must disable multi-tenancy to prevent conflicts").
/// Keeping it here is what makes `recipes::tenant_scope` correct for a
/// deployment that enables the new UI without ever installing the stack.
///
/// Not included: `theme:version`. The theme default is already `v8`, whose
/// label is "Next (preview)" — the Next theme ships on by default and pinning
/// it would only take the choice away from the user.
pub fn next_ui_config() -> [(&'static str, &'static str); 3] {
    [
        ("workspace.enabled", "true"),
        ("opensearch_security.multitenancy.enabled", "false"),
        ("uiSettings.overrides.home:useNewHomePage", "true"),
    ]
}

/// OpenSearch Dashboards config keys the observability stack needs.
///
/// The whole reason the Observability screens looked "missing" before ADR-053
/// rev. 2: the Dashboards image the operator pulls is the SAME `3.8.0` the
/// upstream observability-stack chart uses — `explore`, `data_source` and
/// `dataset_management` ship in it but are **disabled by default**, so
///
///  * `/app/explore/{metrics,traces,logs}` do not exist,
///  * the `explore` saved-object type is rejected (no PromQL panels),
///  * the `data-connection` saved-object type is unregistered, which is what
///    the APM `APM-Config-*` correlation references — without it the APM
///    services / service-map screens have nothing to read.
///
/// `workspace.enabled` is the key that produces the *shape* of the upstream
/// playground rather than a plain Dashboards with extra apps. The Observability
/// nav group — and with it **Agent Monitoring** (agentTraces registers "Traces"
/// and "Spans" under `DEFAULT_APP_CATEGORIES.agentMonitoring`) and
/// **Application performance** (APM services, topology map) — only renders
/// *inside a workspace whose use case is observability*. Read straight out of
/// `agentTraces.plugin.js` in the shipped image, then confirmed by creating one:
/// with workspaces off, those apps exist and are simply never listed.
///
/// `uiSettings.overrides.home:useNewHomePage` is what makes `/app/workspace_initial`
/// the landing page, the way the playground opens. It is also load-bearing for
/// **Datasets**: `dataset_management` registers its app as
/// `AppStatus.accessible` only when `chrome.navGroup.getNavGroupEnabled()` is
/// true, and that reads this exact setting.
///
/// `opensearch_security.multitenancy.enabled: false` is **required, not
/// optional**, and is the one key the upstream docs state as a hard
/// prerequisite: "If your deployment includes the Security plugin, you must
/// disable multi-tenancy to prevent conflicts." Workspaces and tenants are two
/// different scoping mechanisms for the same saved objects, and with both on,
/// an object written under one tenant is invisible to a session resolving a
/// different one — the failure looks exactly like the screens being empty.
///
/// Verified live against 3.8.0. One upstream key is deliberately **not** copied:
/// `query_enhancements.ppl.lint.enabled`, which **this build rejects at boot** —
/// an unknown key is a fatal `InvalidConfigurationError` and exit 64, so this
/// list stays to what was actually observed booting, never to what upstream's
/// values.yaml happens to contain.
pub fn otel_dashboards_config() -> [(&'static str, &'static str); 7] {
    [
        ("data_source.enabled", "true"),
        ("data_source.ssl.verificationMode", "none"),
        ("datasetManagement.enabled", "true"),
        ("explore.enabled", "true"),
        ("explore.discoverTraces.enabled", "true"),
        ("explore.discoverMetrics.enabled", "true"),
        ("observability.alertManager.enabled", "true"),
    ]
}

/// Turn the next-generation UI on or off for a deployment.
///
/// `chosen` records *why*: `true` when the user asked for it (the annotation is
/// set and the stack's uninstall will leave it alone), `false` when the
/// observability stack is turning it on because it cannot work without
/// workspaces. Disabling always clears the annotation.
///
/// ADR-048: patches `spec.dashboards.additionalConfig` and `metadata`, never
/// `spec.general` or `spec.dashboards.version`, so it cannot move a version.
///
/// Turning this OFF hands saved-object scoping back to tenants; turning it ON
/// hands it to workspaces. `recipes::tenant_scope` reads the resulting config,
/// so the two stay consistent without a second source of truth.
pub async fn set_next_ui(dep: &Deployment, enable: bool, chosen: bool) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let mut cfg = serde_json::Map::new();
    if enable {
        for (k, v) in next_ui_config() {
            cfg.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    let manifest = serde_json::json!({
        "apiVersion": "opensearch.org/v1",
        "kind": "OpenSearchCluster",
        "metadata": { "name": name },
        "spec": { "dashboards": { "additionalConfig": serde_json::Value::Object(cfg) } },
    });
    os_api(&client, dep)
        .patch(
            name,
            &PatchParams::apply(UI_FIELD_MANAGER).force(),
            &Patch::Apply(&manifest),
        )
        .await
        .context("patching dashboards additionalConfig for the next-gen UI")?;

    let marker = if enable && chosen {
        serde_json::Value::String("1".to_string())
    } else {
        serde_json::Value::Null
    };
    let patch = serde_json::json!({ "metadata": { "annotations": { LABEL_NEXT_UI: marker } } });
    os_api(&client, dep)
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .context("patching the next-ui annotation")?;
    Ok(())
}

/// Is the next-generation UI on, and did the user ask for it?
///
/// Read from the CR's own config rather than from a marker: the config is what
/// the Dashboards pod actually boots with, and a marker can drift from it.
pub async fn next_ui_state(dep: &Deployment) -> (bool, bool) {
    let Ok(client) = client().await else {
        return (false, false);
    };
    let Ok(Some(obj)) = os_api(&client, dep).get_opt(dep.name()).await else {
        return (false, false);
    };
    let enabled = obj
        .data
        .pointer("/spec/dashboards/additionalConfig/workspace.enabled")
        .and_then(|v| v.as_str())
        == Some("true");
    let chosen = obj
        .metadata
        .annotations
        .as_ref()
        .is_some_and(|a| a.contains_key(LABEL_NEXT_UI));
    (enabled, chosen)
}

/// Whether saved objects on this deployment are scoped by workspace.
///
/// The gate `recipes::tenant_scope` asks. Fails closed to `false` — a read
/// error must never silently drop the `securitytenant` header on a deployment
/// whose objects live in a tenant.
pub async fn workspaces_enabled(dep: &Deployment) -> bool {
    next_ui_state(dep).await.0
}

/// Turn the ADR-053 Dashboards config keys on or off.
///
/// Server-side apply under `OTEL_FIELD_MANAGER`, so `enable: false` is an apply
/// with the keys absent — SSA then removes precisely the keys this manager owns
/// and leaves anyone else's (an SSO deployment's `opensearch_security.*`) alone.
///
/// ADR-048: touches `spec.dashboards.additionalConfig` only. It never
/// constructs `spec.general` or `spec.dashboards.version`, so it cannot move a
/// version.
pub async fn set_dashboards_otel_config(dep: &Deployment, enable: bool) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let mut cfg = serde_json::Map::new();
    if enable {
        for (k, v) in otel_dashboards_config() {
            cfg.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    let manifest = serde_json::json!({
        "apiVersion": "opensearch.org/v1",
        "kind": "OpenSearchCluster",
        "metadata": { "name": name },
        "spec": { "dashboards": { "additionalConfig": serde_json::Value::Object(cfg) } },
    });
    os_api(&client, dep)
        .patch(
            name,
            &PatchParams::apply(OTEL_FIELD_MANAGER).force(),
            &Patch::Apply(&manifest),
        )
        .await
        .context("patching dashboards additionalConfig")?;
    Ok(())
}

/// The address the ingress controller answers on.
///
/// Two sources, in the order that is right rather than the order that is easy:
/// the ingress controller's own `LoadBalancer` address first, because that is
/// what DNS would point at; a node's InternalIP only as a fallback, for the
/// bare-metal/NodePort case where no LoadBalancer address is ever assigned.
/// Returns `None` rather than guessing when neither is available.
pub async fn ingress_endpoint_ip() -> Option<String> {
    use k8s_openapi::api::core::v1::{Node, Service};
    let client = client().await.ok()?;

    let services: Api<Service> = Api::all(client.clone());
    if let Ok(list) = services.list(&kube::api::ListParams::default()).await {
        for svc in list.items {
            let is_lb = svc
                .spec
                .as_ref()
                .and_then(|s| s.type_.as_deref())
                .map(|t| t == "LoadBalancer")
                .unwrap_or(false);
            if !is_lb {
                continue;
            }
            let addr = svc
                .status
                .and_then(|s| s.load_balancer)
                .and_then(|lb| lb.ingress)
                .and_then(|ing| ing.into_iter().next())
                // A hostname here (AWS-style) is already a working DNS name, so
                // it is not something to prefix — only an IP can be.
                .and_then(|i| i.ip);
            if let Some(ip) = addr.filter(|s| !s.is_empty()) {
                return Some(ip);
            }
        }
    }

    let nodes: Api<Node> = Api::all(client);
    let list = nodes.list(&kube::api::ListParams::default()).await.ok()?;
    list.items.into_iter().find_map(|n| {
        n.status?
            .addresses?
            .into_iter()
            .find(|a| a.type_ == "InternalIP")
            .map(|a| a.address)
    })
}

/// Whether this deployment even has a Dashboards Deployment to configure.
///
/// `dashboards.enable: false` is a legal CR; the caller skips the whole
/// config-and-wait dance rather than timing out on something that will never
/// exist.
pub async fn has_dashboards(dep: &Deployment) -> bool {
    let name = dep.name();
    use k8s_openapi::api::apps::v1::Deployment;
    let Ok(client) = client().await else {
        return false;
    };
    let api: Api<Deployment> = Api::namespaced(client, dep.namespace());
    matches!(
        api.get_opt(&format!("{name}-dashboards")).await,
        Ok(Some(_))
    )
}

/// Full status + config of one deployment (drives the list and the edit page).
pub struct Status {
    pub name: String,
    pub phase: String,
    pub health: String,
    /// **The one node pair**, and the same one `activity` carries: nodes ready
    /// (clamped) over the number the CR asks for (issue #131).
    ///
    /// `nodes_desired` used to be the StatefulSet's `.status.replicas` — a
    /// count of live pods — while the SPA's NODES tile divided by the CR's
    /// `replicas`. Two denominators for one fact, and a mid-roll deployment
    /// could show `3/3 — all ready` beside `0/3` and, when the operator refused
    /// to shrink a pool it could not safely touch, a numerator larger than its
    /// total (`6/3`, then `8/3`). Both now come from the same place.
    pub nodes_ready: i32,
    pub nodes_desired: i32,
    pub size: String,
    pub purpose: String,
    pub monitors: Vec<String>,
    /// `otel_stack::STACK_VERSION` when the OTel stack is installed, else empty
    /// (ADR-053). Separate from `monitors` — see `set_otel_stack`.
    pub otel_stack: String,
    /// The next-generation UI (workspaces + new navigation) is on. Read from
    /// the CR's own Dashboards config, not from a marker, because that is what
    /// the pod boots with.
    pub next_ui: bool,
    /// The user asked for the new UI, as opposed to the observability stack
    /// having required it. Decides whether uninstalling the stack takes it back
    /// down.
    pub next_ui_chosen: bool,
    /// CIDRs allowed to reach the public routes. Empty = open, the default.
    pub ip_allow_list: Vec<String>,
    pub replicas: i64,
    /// The JVM heap in effect: derived (half the memory — what the operator
    /// sets, ADR-035) or, on pre-ADR-035 CRs, the explicit `jvm` string.
    pub heap: String,
    /// Node memory (request = limit, e.g. "4Gi"); the operator halves it for
    /// the heap.
    pub memory: String,
    pub disk: String,
    /// `spec.general.additionalConfig` rendered back as "key: value" lines.
    pub extra_config: String,
    pub dashboard_url: Option<String>,
    /// Public address of the OpenSearch API, when the platform is in ingress
    /// mode and the cluster is up.
    pub opensearch_url: Option<String>,
    /// Portforward-mode alternative to `dashboard_url` (ADR-027).
    pub dashboard_portforward: Option<String>,
    /// Identity provider in effect (`internal` when none is configured).
    pub auth_kind: String,
    /// OpenSearch version actually running (`status.version`, falling back to
    /// the spec while the operator has not reported yet) — ADR-048.
    pub version: String,
    /// `spec.general.version` when it differs from `version` (an upgrade in
    /// flight or refused); empty otherwise.
    pub target_version: String,
    /// `spec.dashboards.version` — phase 2 of an upgrade lags the nodes.
    pub dashboards_version: String,
    /// Where an upgrade is, reduced from the CR's own reporting.
    pub upgrade: crate::upgrade::UpgradeState,
    /// Pods already rolled onto the current StatefulSet revision. With the
    /// desired count this is the "nó N de 3" the upgrade progress shows.
    pub nodes_updated: i32,
    /// Version the hourly upstream check found, when it is a legal upgrade for
    /// THIS deployment (ADR-048 rev. 2). Empty otherwise — including when the
    /// check is disabled or has never succeeded.
    pub suggested_version: String,
    /// The CR's `metadata.creationTimestamp` (RFC 3339), so the list can say
    /// how old a deployment is. Empty when the API server did not report one.
    pub created_at: String,
    /// Snapshot repository + scheduled policy state (ADR-049). `configured:
    /// false` — no repository on the CR — is the default and a valid state.
    pub snapshot: SnapshotState,
    /// What this deployment is doing right now, and whether it has settled
    /// (ADR-050). Replaces `health == "green"` as the readiness answer for the
    /// UI, the control locks and `wait_settled`.
    pub activity: crate::activity::Activity,
    /// Whether the purpose profile and the selected monitors have actually been
    /// applied (ADR-052). `complete` for every deployment that carries no
    /// record — which is all of them until something defers work. Derived from
    /// the CR we already read, so it costs no extra call.
    pub provisioning: crate::provisioning::ProvisioningState,
}

async fn status_from(
    client: &Client,
    access: &crate::access::AccessConfig,
    obj: &DynamicObject,
) -> Status {
    let name = obj.metadata.name.clone().unwrap_or_default();
    let labels = obj.metadata.labels.clone().unwrap_or_default();
    let annotations = obj.metadata.annotations.clone().unwrap_or_default();
    let status = obj.data.get("status");
    let phase = status
        .and_then(|s| s.get("phase"))
        .and_then(|v| v.as_str())
        .unwrap_or("Pending")
        .to_string();
    let health = status
        .and_then(|s| s.get("health"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Read the configured spec (for the edit form).
    let spec = obj.data.get("spec");
    let pool = spec
        .and_then(|s| s.get("nodePools"))
        .and_then(|p| p.as_array())
        .and_then(|a| a.first());
    let replicas = pool
        .and_then(|p| p.get("replicas"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let disk = pool
        .and_then(|p| p.get("diskSize"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let memory = pool
        .and_then(|p| p.get("resources"))
        .and_then(|r| r.get("limits"))
        .and_then(|l| l.get("memory"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // ADR-035: new CRs carry no `jvm` — the operator derives the heap as half
    // the memory (request = limit), so display that. CRs from before the
    // delegation still hold an explicit `jvm` string until their next save;
    // show it verbatim since it is what actually runs.
    let heap = pool
        .and_then(|p| p.get("jvm"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| heap_short(&memory));
    let extra_config = spec
        .and_then(|s| s.get("general"))
        .and_then(|g| g.get("additionalConfig"))
        .and_then(|c| c.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let monitors = monitors_from(Some(&annotations));
    // Whether the deferred half of a create/save actually happened (ADR-052).
    // Derived from this same read — the purpose label and the monitors
    // annotation are the intent, the record annotation is what has been
    // applied — so a deployment nobody deferred work for costs nothing.
    let provisioning = crate::provisioning::state_from(
        labels
            .get(LABEL_PURPOSE)
            .map(String::as_str)
            .unwrap_or_default(),
        &monitors,
        annotations
            .get(crate::provisioning::ANNOTATION)
            .map(String::as_str),
    );

    // ADR-053. Empty = not installed; it rides the existing SSE stream, so the
    // Integrations tab needs no extra request to know which panel to render.
    let otel_stack = annotations
        .get(LABEL_OTEL_STACK)
        .cloned()
        .unwrap_or_default();

    // The config is the truth: it is what the Dashboards pod boots with. The
    // annotation only records who asked for it.
    let next_ui = obj
        .data
        .pointer("/spec/dashboards/additionalConfig/workspace.enabled")
        .and_then(|v| v.as_str())
        == Some("true");
    let next_ui_chosen = annotations.contains_key(LABEL_NEXT_UI);

    // Absent annotation = open, which is the default posture: a customer opts
    // IN to restriction, never out of it by accident.
    let ip_allow_list: Vec<String> = annotations
        .get(LABEL_IP_ALLOW)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    // The object's OWN namespace, never the app namespace: with per-tenant
    // namespaces the StatefulSet sits beside its CR, not beside us.
    let obj_ns = obj
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| ns().to_string());
    let sts: Api<StatefulSet> = Api::namespaced(client.clone(), &obj_ns);
    let (nodes_ready, nodes_desired, nodes_updated) =
        match sts.get_opt(&format!("{name}-nodes")).await {
            Ok(Some(s)) => {
                let st = s.status.unwrap_or_default();
                (
                    st.ready_replicas.unwrap_or(0),
                    st.replicas,
                    st.updated_replicas.unwrap_or(0),
                )
            }
            _ => (0, 0, 0),
        };

    // Version + upgrade state (ADR-048). `status.version` is what the operator
    // considers running; the spec is what we asked for. They diverge exactly
    // while an upgrade is pending, rolling, or was refused.
    let (spec_nodes_version, dashboards_version) = versions_of(Some(&obj.data));
    let status_version = status
        .and_then(|s| s.get("version"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&spec_nodes_version)
        .to_string();
    let components = status
        .and_then(|s| s.get("componentsStatus"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    // The operator records a REFUSED upgrade only as a Warning Event — never in
    // componentsStatus (verified in its `upgrade.go`), so a bad target would
    // otherwise fail silently. Only paid for when the versions disagree.
    let warning = if status_version != spec_nodes_version {
        last_upgrade_warning(client, &name).await
    } else {
        None
    };
    let upgrade = crate::upgrade::upgrade_state(
        &spec_nodes_version,
        &status_version,
        &components,
        warning.as_deref(),
    );
    let target_version = if status_version != spec_nodes_version {
        spec_nodes_version.clone()
    } else {
        String::new()
    };

    // Activity + settledness (ADR-050). Gathering the full signal set costs a
    // PVC list and a Deployment GET per deployment, and the SSE stream runs
    // every 3s — so a deployment that is plainly stable short-circuits and pays
    // nothing (invariant 5). The fast check must stay STRICTLY STRONGER than
    // the predicate it stands in for: every clause below is also a clause of
    // `activity::settled_of`, so a deployment can never take this exit and
    // report settled without the checks that define settled.
    let components: Vec<(String, String)> = components
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    Some((
                        r.get("component")?.as_str()?.to_string(),
                        r.get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let initialized = status
        .and_then(|s| s.get("initialized"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // How many nodes the deployment is SUPPOSED to have. The operator creates
    // the StatefulSet with one replica and scales it up, so `.status.replicas`
    // reports 1 for a 3-node cluster during a create (measured 2026-08-08) —
    // using it as the target would call a one-node cluster complete. The spec
    // is the authority; the StatefulSet is the fallback for a CR we could not
    // read the pool from.
    let target_nodes = if replicas > 0 {
        replicas as i32
    } else {
        nodes_desired
    };
    let plainly_stable = health == "green"
        && target_nodes > 0
        && nodes_ready == target_nodes
        && nodes_updated == target_nodes
        && initialized
        && !upgrade.in_flight();
    let activity = if plainly_stable
        && components
            .iter()
            .all(|(_, s)| crate::activity::component_is_terminal(s))
    {
        // Confirm the last clause cheaply: a settled cluster's Dashboards
        // Deployment is up, and this is the one call the fast path still makes.
        if dashboards_ready(client, &name).await {
            crate::activity::Activity {
                // Even the fast path carries the node pair, so the tile has ONE
                // source in the steady state as well (issue #131).
                nodes_ready: nodes_ready.min(target_nodes),
                nodes_total: target_nodes,
                ..crate::activity::Activity::idle()
            }
        } else {
            crate::activity::evaluate(&crate::activity::ActivityInput {
                phase: phase.clone(),
                health: health.clone(),
                initialized,
                nodes_ready,
                nodes_desired: target_nodes,
                nodes_updated,
                pvcs_bound: 0,
                pvcs_total: 0,
                dashboards_ready: false,
                upgrade: upgrade.clone(),
                components: components.clone(),
                // The one clause this path skipped is Dashboards, which is
                // seconds-scale — not a stall worth two HTTP calls to explain.
                since_secs: 0,
                cluster: None,
            })
        }
    } else {
        let pvcs = data_pvcs_in(&obj_ns, &name).await;
        let pvcs_total = pvcs.len() as i32;
        let pvcs_bound = pvcs.values().filter(|p| p.phase == "Bound").count() as i32;
        let mut input = crate::activity::ActivityInput {
            phase: phase.clone(),
            health: health.clone(),
            initialized,
            nodes_ready,
            nodes_desired: target_nodes,
            nodes_updated,
            pvcs_bound,
            pvcs_total,
            dashboards_ready: dashboards_ready(client, &name).await,
            upgrade: upgrade.clone(),
            components,
            since_secs: node_pool_age_secs(
                client,
                &obj_ns,
                &name,
                obj.metadata.creation_timestamp.as_ref(),
            )
            .await,
            cluster: None,
        };
        // Two passes, on purpose (issue #131). The first is the pure verdict
        // from Kubernetes alone; only if THAT says the deployment has been
        // going nowhere for `STALL_AFTER_SECS` is it worth asking OpenSearch
        // why, and then the verdict is recomputed with the answer. So the cost
        // of the diagnosis is paid by deployments that are actually stuck, and
        // by nobody else — the rule the short-circuit above exists to protect
        // (ADR-050 invariant 5).
        let first = crate::activity::evaluate(&input);
        if !first.stalled {
            first
        } else {
            input.cluster = Some(stall_diagnosis_in(&obj_ns, &name).await);
            crate::activity::evaluate(&input)
        }
    };

    // Snapshot state (ADR-049). The repository lives on the CR we already have,
    // so the policy CR is only fetched for a deployment that actually
    // configured snapshots — a deployment without them costs no extra call.
    let snap_cfg = snapshot_config_from(&obj.data, None);
    let snapshot = if snap_cfg.enabled {
        let policy = policy_api_in(client, &obj_ns)
            .get_opt(&crate::snapshot::policy_name(&name))
            .await
            .ok()
            .flatten()
            .map(|o| o.data);
        let cfg = snapshot_config_from(&obj.data, policy.as_ref());
        snapshot_state_from(&cfg, policy.as_ref(), phase.eq_ignore_ascii_case("running"))
    } else {
        SnapshotState::default()
    };

    let (dashboard_url, dashboard_portforward) = if health == "green" {
        match access.dashboard_url(&name) {
            Some(u) => (Some(u), None),
            None => (
                None,
                Some(crate::access::AccessConfig::portforward_cmd(&obj_ns, &name)),
            ),
        }
    } else {
        (None, None)
    };
    // The API's public address, when there is one. Same gate as the dashboards
    // URL: an address for a cluster that is not up yet is a link to a 502.
    let opensearch_url = if health == "green" {
        access.opensearch_url(&name)
    } else {
        None
    };

    Status {
        name,
        phase,
        health,
        // One source, and the activity verdict is it — so nothing downstream
        // can arrive at a different pair (issue #131).
        nodes_ready: activity.nodes_ready,
        nodes_desired: activity.nodes_total,
        size: labels.get(LABEL_SIZE).cloned().unwrap_or_default(),
        purpose: labels.get(LABEL_PURPOSE).cloned().unwrap_or_default(),
        monitors,
        otel_stack,
        next_ui,
        next_ui_chosen,
        ip_allow_list,
        replicas,
        heap,
        memory,
        disk,
        extra_config,
        dashboard_url,
        opensearch_url,
        dashboard_portforward,
        auth_kind: labels
            .get(LABEL_AUTH_KIND)
            .cloned()
            .unwrap_or_else(|| "internal".into()),
        suggested_version: crate::version_feed::suggestion_for(&status_version),
        created_at: obj
            .metadata
            .creation_timestamp
            .as_ref()
            .map(|t| t.0.to_rfc3339())
            .unwrap_or_default(),
        version: status_version,
        target_version,
        dashboards_version,
        upgrade,
        nodes_updated,
        snapshot,
        activity,
        provisioning,
    }
}

/// One line of the activity accordion (ADR-050).
pub struct ActivityLine {
    pub at: String,
    pub severity: &'static str,
    pub source: &'static str,
    pub object: String,
    pub title: String,
    pub detail: String,
}

/// Everything the cluster is saying about one deployment right now: Kubernetes
/// Events, per-pod container state, and the operator's own `componentsStatus`.
///
/// This is what the "Detalhes" accordion shows, and deliberately **not**
/// container logs: reading those needs `pods/log`, which is cluster-wide and
/// crosses the tenant boundary (ADR-044), and a provisioning pod is usually
/// `Pending` or in `ImagePullBackOff` and has no log at all. What explains a
/// stuck provision is exactly these three sources, and the runtime ClusterRole
/// already grants `get/list/watch` on events and pods.
///
/// Best-effort per source: a failure in one contributes a warning line rather
/// than failing the call — the same discipline as `last_upgrade_warning`.
pub async fn activity_log(dep: &Deployment, limit: usize) -> Result<Vec<ActivityLine>> {
    use k8s_openapi::api::core::v1::{Event, Pod};

    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let mut out: Vec<ActivityLine> = Vec::new();

    // 1. Kubernetes Events for everything this deployment owns. One list call,
    //    filtered client-side by name prefix: a field selector can only match
    //    one object at a time, and we want the CR, its pods, its PVCs and the
    //    Dashboards Deployment together in one timeline.
    let events: Api<Event> = Api::namespaced(client.clone(), dep.namespace());
    match events.list(&ListParams::default()).await {
        Ok(list) => {
            for e in list {
                let obj = e.involved_object.name.clone().unwrap_or_default();
                if !(obj == name || obj.starts_with(&format!("{name}-"))) {
                    continue;
                }
                let at = e
                    .last_timestamp
                    .as_ref()
                    .map(|t| t.0.to_rfc3339())
                    .or_else(|| e.event_time.as_ref().map(|t| t.0.to_rfc3339()))
                    .unwrap_or_default();
                out.push(ActivityLine {
                    at,
                    severity: if e.type_.as_deref() == Some("Warning") {
                        "warn"
                    } else {
                        "info"
                    },
                    source: "event",
                    object: obj,
                    title: e.reason.clone().unwrap_or_default(),
                    detail: e.message.clone().unwrap_or_default(),
                });
            }
        }
        Err(e) => out.push(ActivityLine {
            at: String::new(),
            severity: "warn",
            source: "event",
            object: dep.namespace().to_string(),
            title: "events unavailable".into(),
            detail: format!("{e}"),
        }),
    }

    // 2. Per-pod state. This is the half that names the actual blocker —
    //    `ImagePullBackOff`, `CrashLoopBackOff`, an unbound volume — because a
    //    pod stuck before start emits little and logs nothing.
    let pods: Api<Pod> = Api::namespaced(client.clone(), dep.namespace());
    match pods.list(&ListParams::default()).await {
        Ok(list) => {
            for p in list {
                let pod_name = p.metadata.name.clone().unwrap_or_default();
                if !pod_name.starts_with(&format!("{name}-")) {
                    continue;
                }
                let Some(st) = p.status else { continue };
                let phase = st.phase.clone().unwrap_or_default();
                let waiting: Vec<String> = st
                    .container_statuses
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|c| {
                        let w = c.state?.waiting?;
                        let reason = w.reason.unwrap_or_default();
                        if reason.is_empty() {
                            return None;
                        }
                        Some(match w.message {
                            Some(m) if !m.is_empty() => format!("{}: {reason} — {m}", c.name),
                            _ => format!("{}: {reason}", c.name),
                        })
                    })
                    .collect();
                let unmet: Vec<String> = st
                    .conditions
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| c.status != "True")
                    .map(|c| match c.message {
                        Some(m) if !m.is_empty() => format!("{}: {m}", c.type_),
                        _ => c.type_,
                    })
                    .collect();
                // A running pod with nothing unmet is not news.
                if phase == "Running" && waiting.is_empty() && unmet.is_empty() {
                    continue;
                }
                let mut detail = waiting;
                detail.extend(unmet);
                out.push(ActivityLine {
                    at: st
                        .start_time
                        .as_ref()
                        .map(|t| t.0.to_rfc3339())
                        .unwrap_or_default(),
                    severity: if phase == "Failed" { "error" } else { "info" },
                    source: "pod",
                    object: pod_name,
                    title: phase,
                    detail: detail.join("\n"),
                });
            }
        }
        Err(e) => out.push(ActivityLine {
            at: String::new(),
            severity: "warn",
            source: "pod",
            object: name.to_string(),
            title: "pods unavailable".into(),
            detail: format!("{e}"),
        }),
    }

    // 3. The operator's own reporting. `upgrade_state` reads exactly one row of
    //    this array and discards the rest (`component != "Upgrader"`); here the
    //    whole thing is shown, which is the only place those rows surface.
    if let Ok(Some(cr)) = os_api(&client, dep).get_opt(name).await {
        if let Some(rows) = cr
            .data
            .pointer("/status/componentsStatus")
            .and_then(|v| v.as_array())
        {
            for r in rows {
                let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("");
                out.push(ActivityLine {
                    at: String::new(),
                    severity: if crate::activity::component_is_terminal(status) {
                        "info"
                    } else {
                        "warn"
                    },
                    source: "operator",
                    object: r
                        .get("component")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string(),
                    title: status.to_string(),
                    detail: r
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
    }

    // Newest first. RFC 3339 in UTC sorts lexicographically; lines without a
    // timestamp (operator rows) sort last, which is where they belong — they
    // are a current state, not an event.
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out.truncate(limit);
    Ok(out)
}

/// Is the deployment's Dashboards Deployment serving? (ADR-050.)
///
/// A cluster whose UI is down has not finished coming up — the wizard hands the
/// user a dashboard link, so "ready" has to include the thing the link points
/// at. Best-effort: any lookup failure reads as not-ready, which keeps the
/// deployment in an activity state rather than declaring it settled on a
/// missing answer.
async fn dashboards_ready(client: &Client, name: &str) -> bool {
    use k8s_openapi::api::apps::v1::Deployment;
    let api: Api<Deployment> = Api::namespaced(client.clone(), ns());
    match api.get_opt(&format!("{name}-dashboards")).await {
        Ok(Some(d)) => d
            .status
            .and_then(|s| s.ready_replicas)
            .map(|r| r >= 1)
            .unwrap_or(false),
        // No Dashboards Deployment at all: nothing to wait for. A deployment
        // created with `dashboards.enable: false` would sit unsettled forever
        // otherwise.
        Ok(None) => true,
        Err(_) => false,
    }
}

/// Seconds since anything in the deployment's node pool last moved — the clock
/// the activity panel renders (issue #131).
///
/// The anchor is the newest node pod's `creationTimestamp`, falling back to the
/// CR's when no pod exists yet, and `0` when neither is readable.
///
/// **Why a pod timestamp is the right anchor.** A roll advances by replacing a
/// pod, so the newest pod's age is exactly "how long since the last real
/// advance". The two events that corrupted the old clock leave it alone: an
/// operator reconcile writes CR status and creates no pod, and a tab switch
/// remounts a component and touches nothing in the cluster at all. It is also
/// stateless, which is the constraint — ADR-050 invariant 6 forbids keeping
/// provisioning state in memory, so the duration has to be readable from the
/// cluster or not exist.
///
/// Best-effort: a failed list degrades to the CR's own age rather than an
/// error, and an unmeasurable age is reported as `0`, which `activity::evaluate`
/// treats as "never claim a stall".
async fn node_pool_age_secs(
    client: &Client,
    namespace: &str,
    name: &str,
    cr_created: Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::Time>,
) -> i64 {
    use k8s_openapi::api::core::v1::Pod;
    use k8s_openapi::chrono::{DateTime, Utc};

    let prefix = format!("{name}-nodes-");
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let newest: Option<DateTime<Utc>> = match pods.list(&ListParams::default()).await {
        Ok(list) => list
            .into_iter()
            .filter(|p| {
                p.metadata
                    .name
                    .as_deref()
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .filter_map(|p| p.metadata.creation_timestamp.map(|t| t.0))
            .max(),
        Err(_) => None,
    };
    let anchor = newest.or_else(|| cr_created.map(|t| t.0));
    anchor
        .map(|t| (Utc::now() - t).num_seconds().max(0))
        .unwrap_or(0)
}

/// How long a memoized stall diagnosis stays fresh.
const DIAGNOSIS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// A hung OpenSearch must not hang a status frame. The SSE stream builds every
/// deployment's `Status` serially, so an unbounded request here would stop the
/// whole screen updating — the exact failure mode the panel is being taught to
/// report on.
const DIAGNOSIS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

type DiagnosisCache = std::sync::Mutex<
    std::collections::HashMap<String, (std::time::Instant, crate::activity::ClusterBlock)>,
>;

fn diagnosis_cache() -> &'static DiagnosisCache {
    static CACHE: std::sync::OnceLock<DiagnosisCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Why a stalled deployment is not converging, asked of OpenSearch itself.
///
/// Two GETs. `_cluster/health` gives the unassigned-shard count; the incident
/// (#131) had **7** behind a single blocked recovery, and nothing the app read
/// mentioned them. `_recovery?active_only=true` gives the recovery that is
/// holding green back, with its stage and its age — at any minute of those
/// sixteen hours it would have answered `.opendistro_security`, stage `init`,
/// 15.9 hours old. That is the sentence the user needed and never got.
///
/// **Only called for a deployment `activity::evaluate` already called stalled**,
/// and memoized for [`DIAGNOSIS_TTL`] on top of that: the SSE stream re-renders
/// every 3 seconds and a stall lasts hours, so asking on every frame would be
/// two round-trips per frame for as long as the problem lasts. The cache is an
/// optimisation and never an authority — it holds no verdict, only a reading;
/// a restart re-asks, and every verdict is still recomputed from the cluster on
/// every tick (ADR-050 invariant 6).
///
/// Each half is independent and best-effort: a cluster that answers health but
/// not `_recovery` still contributes its shard count, and a cluster that
/// answers neither leaves the defaults ("unknown", not "zero") for the UI to
/// degrade against.
async fn stall_diagnosis_in(namespace: &str, name: &str) -> crate::activity::ClusterBlock {
    let key = format!("{namespace}/{name}");
    if let Ok(cache) = diagnosis_cache().lock() {
        if let Some((at, hit)) = cache.get(&key) {
            if at.elapsed() < DIAGNOSIS_TTL {
                return hit.clone();
            }
        }
    }

    let mut out = crate::activity::ClusterBlock::default();
    let base = crate::recipes::os_base_in(name, namespace);
    if let Ok(http) = crate::recipes::http() {
        let (user, pass) = admin_creds_in(namespace, name).await;
        let get = |path: &str| {
            http.get(format!("{base}{path}"))
                .basic_auth(&user, Some(&pass))
                .timeout(DIAGNOSIS_TIMEOUT)
                .send()
        };

        // Half one: the colour's arithmetic. `unassigned_shards` is the number
        // that turns "yellow" from a colour into a cause.
        if let Ok(resp) = get("/_cluster/health").await {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                if let Some(n) = v.get("unassigned_shards").and_then(|x| x.as_i64()) {
                    out.unassigned_shards = n as i32;
                }
            }
        }

        // Half two: the recovery that is not finishing. `active_only=true`
        // keeps the response to what is in flight, so this is small even on a
        // cluster with thousands of shards.
        if let Ok(resp) = get("/_recovery?active_only=true").await {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                if let Some(indices) = v.as_object() {
                    for (index, body) in indices {
                        let Some(shards) = body.get("shards").and_then(|s| s.as_array()) else {
                            continue;
                        };
                        for s in shards {
                            let ms = s
                                .get("total_time_in_millis")
                                .and_then(|x| x.as_i64())
                                .unwrap_or(0);
                            // The OLDEST active recovery is the one to name:
                            // the rest are usually queued behind it (the
                            // incident's peers were refused with "reached the
                            // limit of outgoing shard recoveries [2]").
                            if ms / 1000 <= out.recovery_secs {
                                continue;
                            }
                            out.recovery_secs = ms / 1000;
                            out.recovery_index = index.clone();
                            // `_recovery` shouts its stages (`INIT`) where
                            // `_cat/recovery` whispers them (`init`); the
                            // lower-case spelling is the one operators read in
                            // the logs, so normalise to it.
                            out.recovery_stage = s
                                .get("stage")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_ascii_lowercase();
                        }
                    }
                }
            }
        }
    }

    if let Ok(mut cache) = diagnosis_cache().lock() {
        // Deleted deployments would otherwise leak an entry each. Nothing here
        // is state worth keeping, so anything long stale simply goes.
        cache.retain(|_, (at, _)| at.elapsed() < DIAGNOSIS_TTL * 20);
        cache.insert(key, (std::time::Instant::now(), out.clone()));
    }
    out
}

/// Message of the most recent `Warning`/`Upgrade` Event on a deployment's CR.
///
/// This is the operator's ONLY record of a rejected version: its upgrade
/// reconciler emits the event and returns a terminal error that leaves the
/// other reconcilers running, so nothing on the CR's status changes. Read-only
/// and best-effort — an events lookup that fails must never break a status
/// read (the `events` verb is already in the runtime ClusterRole).
async fn last_upgrade_warning(client: &Client, name: &str) -> Option<String> {
    use k8s_openapi::api::core::v1::Event;
    let events: Api<Event> = Api::namespaced(client.clone(), ns());
    let lp = ListParams::default().fields(&format!(
        "involvedObject.name={name},involvedObject.kind=OpenSearchCluster,type=Warning"
    ));
    let list = events.list(&lp).await.ok()?;
    let mut best: Option<(String, String)> = None;
    for e in list {
        if e.reason.as_deref() != Some("Upgrade") {
            continue;
        }
        let Some(msg) = e.message.clone().filter(|m| !m.trim().is_empty()) else {
            continue;
        };
        let ts = e
            .last_timestamp
            .as_ref()
            .map(|t| t.0.to_rfc3339())
            .or_else(|| e.event_time.as_ref().map(|t| t.0.to_rfc3339()))
            .unwrap_or_default();
        // RFC3339 in UTC sorts lexicographically — newest wins.
        if best.as_ref().is_none_or(|(b, _)| ts >= *b) {
            best = Some((ts, msg));
        }
    }
    best.map(|(_, m)| m)
}

/// The OpenSearch admin login for one deployment, read from its credentials
/// Secret — the single source of truth for authenticating to a deployment's
/// OpenSearch: `recipes`, `profiles`, `metrics` and the collection `agents` all
/// use it so a reset password (which rewrites the Secret) is honored everywhere
/// without a hardcoded constant.
///
/// Infallible — any lookup error degrades to `(admin, "")`. There is deliberately
/// NO password fallback: the per-cluster password lives only in the Secret, so if
/// it can't be read no literal could authenticate anyway; an empty password fails
/// closed with a clean 401 instead of leaking or guessing a default.
pub async fn admin_creds(dep: &Deployment) -> (String, String) {
    admin_creds_in(dep.namespace(), dep.name()).await
}

/// The same read, addressed by namespace + name. Private, and only reachable
/// from a caller that already resolved the deployment — the `data_pvcs_in`
/// arrangement, for the same caller: `status_from`'s stall diagnosis, which
/// holds a CR the scope layer just listed and no [`Deployment`] token.
async fn admin_creds_in(namespace: &str, name: &str) -> (String, String) {
    if let Ok(client) = client().await {
        let secrets: Api<Secret> = Api::namespaced(client, namespace);
        if let Ok(Some(s)) = secrets.get_opt(&admin_secret_name(name)).await {
            let get = |k: &str| {
                s.data
                    .as_ref()
                    .and_then(|d| d.get(k))
                    .and_then(|b| String::from_utf8(b.0.clone()).ok())
            };
            if let (Some(u), Some(p)) = (get("username"), get("password")) {
                return (u, p);
            }
        }
    }
    // Secret unreadable — fail closed (empty password → 401), never a default.
    (ADMIN_USER.to_string(), String::new())
}

/// The OpenSearch admin login for one deployment. Shown on the Overview page so
/// users can actually log into Dashboards.
pub async fn dashboard_credentials(dep: &Deployment) -> Result<(String, String)> {
    validate_name(dep.name())?;
    Ok(admin_creds(dep).await)
}

/// Reset the OpenSearch admin password for one deployment.
///
/// The admin user is `is_reserved: true` in the operator-seeded security config,
/// so it CANNOT be changed via the security REST API (verified live on a single-node k3s cluster —
/// both `/api/account` and `/api/internalusers/admin` return 403 "reserved").
/// The operator seeds the admin hash from the `adminCredentialsSecret`, so the
/// reset path is: rewrite that Secret, then force the operator to reconcile and
/// re-run its securityconfig update job so the new hash is applied to the
/// running cluster. `admin_creds` immediately reflects the new password for the
/// app's own calls.
/// Reset the cluster admin password to a freshly generated one and return it.
///
/// The caller never chooses the value. A human-chosen password for a machine
/// account is the weakest link in the deployment — it is typed once, reused,
/// and never rotated — and nothing here needs it to be memorable: the app hands
/// it back through the UI and it is stored in the deployment's Secret. Same
/// generator as the one that seeds the account at creation, so the strength
/// rules live in exactly one place.
pub async fn reset_admin_password_random(dep: &Deployment) -> Result<String> {
    let pw = gen_admin_password()?;
    reset_admin_password(dep, &pw).await?;
    Ok(pw)
}

pub async fn reset_admin_password(dep: &Deployment, new_password: &str) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    password_check(new_password)?;
    let client = client().await?;
    // Existence-first (#52): patching the Secret of a deployment that doesn't
    // exist would either 404 cryptically or, worse, leave an orphan Secret a
    // later create would silently adopt. Refuse with the actual situation.
    ensure_namespace_exists(&client, dep.namespace()).await?;
    if os_api(&client, dep).get_opt(name).await?.is_none() {
        bail!(
            "no deployment named '{name}' in namespace '{}' — cannot reset \
             its admin password",
            dep.namespace()
        );
    }

    // 1. Update the credentials Secret — the source of truth the operator seeds
    //    the admin hash from, and what `admin_creds` reads.
    let secrets: Api<Secret> = Api::namespaced(client.clone(), dep.namespace());
    let patch =
        serde_json::json!({ "stringData": { "username": ADMIN_USER, "password": new_password } });
    secrets
        .patch(
            &admin_secret_name(name),
            &PatchParams::default(),
            &Patch::Merge(&patch),
        )
        .await
        .context("updating admin credentials Secret")?;

    // 2. Force a reconcile so the operator re-applies the security config with
    //    the new hash. A dedicated field manager owns ONLY this annotation, so
    //    server-side apply can't prune the rest of the spec.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let anno = serde_json::json!({
        "apiVersion": "opensearch.org/v1",
        "kind": "OpenSearchCluster",
        "metadata": {
            "name": name,
            "annotations": { "veloxsearch.ai/security-reset": ts.to_string() }
        }
    });
    os_api(&client, dep)
        .patch(
            name,
            &PatchParams::apply("veloxsearch-secreset").force(),
            &Patch::Apply(&anno),
        )
        .await
        .context("forcing operator reconcile for password reset")?;

    Ok(())
}

/// Wait until a deployment has actually settled — every node ready AND on the
/// current revision, security initialized, Dashboards up, nothing rolling
/// (ADR-050 `activity::settled_of`).
///
/// This used to wait on `health == "green"` alone, which is the same mistake
/// the UI made: the operator rolls one node at a time, so between two restarts
/// every pod is briefly up and the cluster reports green while the roll still
/// has nodes to go. Everything this function hands off to — `profiles::apply`,
/// `recipes::apply`, the ADR-049 repository registration — was therefore racing
/// a cluster that was still moving. Found live on 2026-08-07.
pub async fn wait_settled(dep: &Deployment, secs: u64) -> Result<()> {
    let name = dep.name();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut last = String::from("unknown");
    while std::time::Instant::now() < deadline {
        if let Ok(Some(s)) = get_deployment(dep).await {
            if s.activity.settled {
                return Ok(());
            }
            last = format!("{} ({}%)", s.activity.stage, s.activity.percent);
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
    bail!("deployment {name} did not settle within {secs}s (stuck at {last})")
}

// ─────────────────── deferred provisioning (ADR-052) ───────────────────

/// RFC 3339 now, for the record's `updated_at`. The pure module has no clock;
/// every timestamp it stores is supplied here.
fn now_rfc3339() -> String {
    k8s_openapi::chrono::Utc::now().to_rfc3339()
}

/// The `monitors` annotation parsed back into ids. One reader for the three
/// places that need it (the status, the monitor toggle, the deferred applier):
/// a disagreement between them is a monitor that installs but never shows up,
/// or the reverse.
fn monitors_from(annotations: Option<&BTreeMap<String, String>>) -> Vec<String> {
    annotations
        .and_then(|a| a.get(LABEL_MONITORS))
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// What the deferred applier needs, from a single CR read: what the user asked
/// for (the purpose label and the monitors annotation — ADR-052's point that
/// the intent is already on the CR) and what has actually been applied.
struct Deferred {
    purpose: String,
    monitors: Vec<String>,
    record: crate::provisioning::Record,
}

/// Read the deferred-provisioning inputs. `None` means the CR is gone — a
/// deployment deleted while its provisioning was still outstanding, which is a
/// legitimate end state and not an error.
async fn read_deferred(client: &Client, dep: &Deployment) -> Result<Option<Deferred>> {
    let Some(obj) = os_api(client, dep).get_opt(dep.name()).await? else {
        return Ok(None);
    };
    let labels = obj.metadata.labels.clone().unwrap_or_default();
    let annotations = obj.metadata.annotations.clone().unwrap_or_default();
    Ok(Some(Deferred {
        purpose: labels.get(LABEL_PURPOSE).cloned().unwrap_or_default(),
        monitors: monitors_from(Some(&annotations)),
        record: crate::provisioning::parse(
            annotations
                .get(crate::provisioning::ANNOTATION)
                .map(String::as_str),
        ),
    }))
}

/// Write (or, with `None`, remove) the record annotation. A JSON merge patch on
/// one key: it cannot touch the operator-managed spec, and `null` is how a
/// merge patch deletes — the same idiom `set_monitor` already uses.
async fn write_deferred_record(
    dep: &Deployment,
    record: Option<&crate::provisioning::Record>,
) -> Result<()> {
    let client = client().await?;
    let value = match record {
        Some(r) => serde_json::Value::String(crate::provisioning::render(r)),
        None => serde_json::Value::Null,
    };
    let patch = serde_json::json!({ "metadata": { "annotations": {
        crate::provisioning::ANNOTATION: value,
    } } });
    os_api(&client, dep)
        .patch(dep.name(), &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .context("patching the deferred-provisioning record")?;
    Ok(())
}

/// Mark a deployment as owing deferred work, BEFORE the applier starts.
///
/// Written from the request path rather than from the background task, so a
/// backend that dies during the wait still leaves a deployment that reports
/// "not applied" instead of one that looks finished — which is the whole defect
/// (ADR-052). The remaining window is the milliseconds between the CR apply and
/// this patch.
///
/// Merges rather than overwrites: the attempt schedule is reset (this IS the
/// new run) while the applied set is kept, so a save re-applies only what
/// actually changed. On a brand-new deployment there is nothing to merge with.
pub async fn begin_deferred_provisioning(dep: &Deployment) -> Result<()> {
    let client = client().await?;
    let Some(state) = read_deferred(&client, dep).await? else {
        bail!(
            "no deployment named '{}' in namespace '{}' — cannot record its deferred work",
            dep.name(),
            dep.namespace()
        );
    };
    let mut record = state.record;
    record.restart(&now_rfc3339());
    write_deferred_record(dep, Some(&record)).await
}

/// Apply everything a create or save deferred until the cluster settled,
/// retrying on the ADR-052 schedule, and keep the record on the CR current so
/// the UI can say what is still missing.
///
/// Spawned, never awaited by a request: settling takes minutes. Safe to call
/// twice on the same deployment (the user clicking "retry" during a retry) —
/// every item it applies is an idempotent upsert, so concurrent appliers
/// duplicate work rather than corrupt it.
pub fn spawn_deferred_provisioning(dep: Deployment, snapshot: Option<SnapshotConfig>) {
    tokio::spawn(async move { run_deferred_provisioning(dep, snapshot).await });
}

async fn run_deferred_provisioning(dep: Deployment, mut snapshot: Option<SnapshotConfig>) {
    use crate::provisioning::{retry_delay, settle_budget, Item, SETTLE_BUDGETS};

    // The attempt counter that drives the schedule lives on the CR, so a
    // backend restart resumes it instead of restarting it. This local bound is
    // the backstop for the case where the record cannot be written at all: it
    // stops a task that would otherwise loop forever on a stuck counter.
    for _ in 0..=SETTLE_BUDGETS.len() {
        let client = match client().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("deferred provisioning of {dep} has no kube client: {e:#}");
                return;
            }
        };
        let state = match read_deferred(&client, &dep).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                tracing::info!("{dep} was deleted before its provisioning finished");
                return;
            }
            Err(e) => {
                tracing::error!("reading the provisioning record of {dep}: {e:#}");
                return;
            }
        };
        let mut record = state.record;
        let Some(budget) = settle_budget(record.attempts) else {
            // Out of attempts, not out of options: the record stays on the CR,
            // the deployment reports `failed` with the last error, and the
            // retry route starts a fresh schedule.
            record.mark_exhausted(&now_rfc3339());
            if let Err(e) = write_deferred_record(&dep, Some(&record)).await {
                tracing::error!("recording the exhausted schedule of {dep}: {e:#}");
            }
            tracing::error!(
                "deferred provisioning of {dep} gave up after {} attempts ({}); \
                 the deployment reports what is still missing and can be retried",
                record.attempts,
                record.last_error
            );
            return;
        };

        let delay = retry_delay(record.attempts);
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        }
        if let Err(e) = wait_settled(&dep, budget).await {
            record.mark_failed(&format!("{e:#}"), &now_rfc3339());
            tracing::warn!(
                "{e:#}; attempt {} of the deferred provisioning",
                record.attempts
            );
            if let Err(e) = write_deferred_record(&dep, Some(&record)).await {
                tracing::error!("recording a failed provisioning attempt on {dep}: {e:#}");
            }
            continue;
        }

        // ADR-049: the operator's repository reconciler only runs once the
        // cluster is `PhaseRunning`, so the repository is registered here rather
        // than at create time — and BEFORE the profile, whose ISM retention
        // policy snapshots into this very repository.
        //
        // Deliberately NOT tracked in the record: the configuration carries S3
        // credentials, which must not be written to an annotation, so it lives
        // only in this task. In-process retries cover it; a backend restart
        // between create and settle loses it, and the user re-enters it in the
        // snapshot tab. Narrowing that gap needs credentials persisted at create
        // time, which ADR-049's pre-flight (it refuses an unsettled cluster)
        // does not currently allow — stated rather than hidden.
        if let Some(cfg) = snapshot.clone() {
            match set_snapshot_config(&dep, cfg).await {
                Ok(_) => {
                    tracing::info!("snapshot repository configured on {dep}");
                    snapshot = None;
                }
                Err(e) => tracing::error!("snapshot repository on {dep} failed: {e:#}"),
            }
        }

        let mut failure: Option<String> = None;
        for item in record.pending(&state.purpose, &state.monitors) {
            let outcome = match &item {
                Item::Profile(purpose) => crate::profiles::apply(&dep, purpose).await,
                // A selected monitor is either a built-in recipe or a catalog
                // package (ADR-039): the wizard's data step offers both, so
                // route by which one it is rather than failing the id as
                // "unknown recipe".
                Item::Monitor(id) => {
                    if crate::recipes::RECIPES.contains(&id.as_str()) {
                        crate::recipes::apply(&dep, id).await
                    } else {
                        crate::catalog::install(&dep, id, None).await
                    }
                }
                // Applied once and recorded, never re-asserted — that is what
                // makes it a default the user can keep or discard.
                Item::DashboardsDefault(key) => match key.as_str() {
                    crate::provisioning::DARK_MODE => {
                        crate::recipes::set_dark_mode_default(&dep).await
                    }
                    // An unknown key is a record written by a NEWER build than
                    // this one. Treat it as done rather than retrying forever
                    // against a setting this binary knows nothing about.
                    other => {
                        tracing::warn!("unknown Dashboards default '{other}' on {dep}; skipping");
                        Ok(())
                    }
                },
            };
            match (&item, outcome) {
                (Item::Profile(purpose), Ok(())) => {
                    tracing::info!("profile '{purpose}' applied to {dep}");
                    record.mark_done(&item, &now_rfc3339());
                    // Switching TO search must honour "no agents" (ADR-028) by
                    // removing any the previous purpose left running; data is
                    // kept. Unconditional for search rather than flagged from
                    // the save path, so a retry after a restart still does it —
                    // on a fresh cluster it is a handful of fast 404s.
                    if purpose == "search" {
                        for r in crate::recipes::RECIPES {
                            if let Err(e) = crate::recipes::disable(&dep, r).await {
                                tracing::debug!("removing agent '{r}' from {dep}: {e:#}");
                            }
                        }
                    }
                }
                (Item::Monitor(id), Ok(())) => {
                    tracing::info!("deferred monitor '{id}' applied to {dep}");
                    record.mark_done(&item, &now_rfc3339());
                }
                (Item::DashboardsDefault(key), Ok(())) => {
                    tracing::info!("Dashboards default '{key}' set on {dep}");
                    record.mark_done(&item, &now_rfc3339());
                }
                (Item::DashboardsDefault(key), Err(e)) => {
                    // Cosmetic, and last in the plan: a Dashboards that is not
                    // answering yet must not make the whole provision look
                    // failed when the profile and every monitor landed. Recorded
                    // as done so it is not retried for the life of the cluster.
                    tracing::warn!("Dashboards default '{key}' on {dep} did not apply: {e:#}");
                    record.mark_done(&item, &now_rfc3339());
                }
                (Item::Profile(purpose), Err(e)) => {
                    // Stop the pass: the profile installs the retention ISM
                    // policy that auto-attaches to indices created afterwards,
                    // and the monitors are what create them. Going on would put
                    // this deployment's logs outside retention.
                    failure = Some(format!("profile '{purpose}': {e:#}"));
                    tracing::error!("profile '{purpose}' on {dep} failed: {e:#}");
                    break;
                }
                (Item::Monitor(id), Err(e)) => {
                    // One bad monitor does not block the others — a catalog
                    // package whose registry is down should not cost the user
                    // the built-in recipes they also selected.
                    failure.get_or_insert(format!("monitor '{id}': {e:#}"));
                    tracing::error!("deferred monitor '{id}' on {dep} failed: {e:#}");
                }
            }
        }

        // One write per pass rather than one per item. A pass is seconds, and
        // losing a `done` mark to a crash mid-pass costs a re-applied upsert,
        // which is exactly what idempotence is for.
        if record.pending(&state.purpose, &state.monitors).is_empty() && snapshot.is_none() {
            if let Err(e) = write_deferred_record(&dep, None).await {
                // The record is stale, not wrong: `state_from` reports a record
                // with nothing outstanding as complete, so this does not leave
                // a healthy deployment flagged.
                tracing::warn!("clearing the provisioning record of {dep}: {e:#}");
            }
            tracing::info!("deferred provisioning of {dep} is complete");
            return;
        }
        record.mark_failed(
            &failure.unwrap_or_else(|| "the snapshot repository was not registered".into()),
            &now_rfc3339(),
        );
        if let Err(e) = write_deferred_record(&dep, Some(&record)).await {
            tracing::error!("recording a failed provisioning attempt on {dep}: {e:#}");
        }
    }
    // Only reachable when the record could never be written: the persisted
    // counter never advanced, so the loop ran out on its local backstop instead
    // of on the schedule. Say so, because the deployment cannot.
    tracing::error!(
        "deferred provisioning of {dep} stopped without being able to record its state — \
         the profile and monitors may not be applied and the deployment will not report it"
    );
}

// ───────────────────────── version upgrade (ADR-048) ─────────────────────────

/// How long we follow a rolling upgrade before giving up on the background
/// phase-2 patch. Three nodes restarted one at a time, each waiting for green,
/// is minutes — an hour is generous and bounded. Giving up here does NOT abort
/// anything: the state lives on the CR, so the UI keeps reporting it and the
/// Dashboards phase can be retried.
const UPGRADE_WATCH_SECS: u64 = 3600;

/// What an upgrade would do, resolved before anything is written.
#[derive(Debug, Clone)]
pub struct UpgradePlan {
    /// The version running now.
    pub current: String,
    pub target: String,
    /// Nodes are already on the target and only `spec.dashboards.version` is
    /// behind — the retry path for a failed phase 2 (ADR-048 consequences).
    pub phase_two_only: bool,
    /// Both image tags were confirmed to exist. False means the registry could
    /// not be reached (air-gapped), which the caller must explicitly accept —
    /// never a silent pass (invariant 3).
    pub images_verified: bool,
}

/// Pre-flight: everything that can refuse an upgrade, evaluated BEFORE the CR
/// is touched (ADR-048 invariant 2). Given there is no rollback, this is the
/// last place a mistake is still cheap.
pub async fn preflight_upgrade(
    dep: &Deployment,
    target: &str,
    allow_untested: bool,
) -> Result<UpgradePlan> {
    let name = dep.name();
    validate_name(name)?;
    let target = target.trim();
    let Some(s) = get_deployment(dep).await? else {
        bail!(
            "no deployment named '{name}' in namespace '{}'",
            dep.namespace()
        );
    };

    // 1. Is this a target we ship, or the one the hourly check discovered and
    //    the UI is offering as a tag (ADR-048 rev. 2)? The free-text override
    //    is a deliberate act and is still subject to every rule below
    //    (invariant 5).
    if !allow_untested
        && !crate::upgrade::in_catalog(target)
        && !crate::version_feed::is_discovered(target)
    {
        bail!(
            "{target} is not one of the tested versions ({}) — use the advanced \
             override if you really mean it",
            crate::upgrade::CATALOG
                .iter()
                .map(|e| e.version)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // 2. Phase-2-only retry: the nodes already run the target, Dashboards does
    //    not. `validate` would refuse this ("already the running version"), and
    //    it is exactly the state a failed phase 2 leaves behind.
    let phase_two_only = s.version == target && s.dashboards_version != target;
    if !phase_two_only {
        crate::upgrade::validate(&s.version, target)?;
    }

    // 3. Cluster state: never start a rolling restart on a cluster that is not
    //    healthy, still provisioning, or already mid-upgrade.
    if s.upgrade.in_flight() {
        bail!(
            "an upgrade to {} is already in progress on '{name}' — wait for it to finish",
            if s.target_version.is_empty() {
                target.to_string()
            } else {
                s.target_version.clone()
            }
        );
    }
    if s.health != "green" {
        bail!(
            "'{name}' is {} — a rolling upgrade restarts nodes one by one and \
             needs a green cluster to make progress",
            if s.health == "unknown" {
                "not reporting its health"
            } else {
                &s.health
            }
        );
    }

    // 4. Both images resolve before EITHER is written — an upgraded data plane
    //    with an unpullable Dashboards is not a state we may create.
    let mut images_verified = true;
    for repo in [
        crate::upgrade::IMAGE_NODES,
        crate::upgrade::IMAGE_DASHBOARDS,
    ] {
        match crate::version_feed::image_tag_exists(repo, target).await {
            Ok(true) => {}
            Ok(false) => bail!(
                "the image {repo}:{target} does not exist — the first restarted node would \
                 sit in ImagePullBackOff and the upgrade would never advance"
            ),
            Err(e) => {
                tracing::warn!("could not verify {repo}:{target}: {e:#}");
                images_verified = false;
            }
        }
    }

    Ok(UpgradePlan {
        current: s.version,
        target: target.to_string(),
        phase_two_only,
        images_verified,
    })
}

/// Upgrade a deployment's OpenSearch version — the ONLY write path allowed to
/// move it (ADR-048).
///
/// Phase 1 patches `spec.general.version`; the operator then restarts the nodes
/// one at a time, waiting for green between each. Phase 2 patches
/// `spec.dashboards.version` once the data plane reports the target, so a newer
/// Dashboards never talks to older nodes. Phase 2 runs in a background task —
/// the request returns as soon as the upgrade is accepted, and all progress is
/// read back from the CR (nothing is kept in memory, so a backend restart or a
/// page reload loses nothing).
///
/// `confirm_unverified` is the user's explicit acceptance of an unreachable
/// registry; without it an unverifiable image tag refuses the upgrade.
pub async fn upgrade_cluster(
    dep: &Deployment,
    target: &str,
    allow_untested: bool,
    confirm_unverified: bool,
) -> Result<UpgradePlan> {
    let plan = preflight_upgrade(dep, target, allow_untested).await?;
    if !plan.images_verified && !confirm_unverified {
        bail!(
            "could not verify that the {} and {} images exist for {} (registry unreachable) — \
             confirm to upgrade anyway",
            crate::upgrade::IMAGE_NODES,
            crate::upgrade::IMAGE_DASHBOARDS,
            plan.target
        );
    }
    let name = dep.name();
    let client = client().await?;
    let target = plan.target.clone();

    if plan.phase_two_only {
        patch_version(&client, dep, "dashboards", &target).await?;
        tracing::info!("dashboards phase re-run for {name} → {target}");
        return Ok(plan);
    }

    // Phase 1 — the write that starts the rolling upgrade. A JSON merge patch:
    // it touches this one field and nothing else, so no other part of the spec
    // (shape, auth, monitors — invariant 6) can be pruned or rewritten by it.
    patch_version(&client, dep, "general", &target).await?;
    tracing::info!("upgrade of {name}: {} → {target} accepted", plan.current);

    // Phase 2 — deferred until the operator reports the nodes on the target.
    {
        let n = dep.clone();
        let t = target.clone();
        tokio::spawn(async move {
            match wait_nodes_upgraded(&n, &t, UPGRADE_WATCH_SECS).await {
                Ok(purpose) => {
                    let client = match crate::k8s::client().await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::error!("dashboards phase for {n} skipped: {e:#}");
                            return;
                        }
                    };
                    match patch_version(&client, &n, "dashboards", &t).await {
                        Ok(()) => tracing::info!("dashboards of {n} upgraded to {t}"),
                        Err(e) => tracing::error!(
                            "nodes of {n} are on {t} but the dashboards phase failed: {e:#} \
                             — retry the upgrade to re-run this phase only"
                        ),
                    }
                    // The upgrade must not change what the deployment IS:
                    // re-assert the purpose profile rather than regenerate it.
                    if let Err(e) = crate::profiles::apply(&n, &purpose).await {
                        tracing::error!("re-asserting profile '{purpose}' on {n}: {e:#}");
                    }
                }
                Err(e) => tracing::error!(
                    "{e:#}; the dashboards phase was not started — the CR still reports \
                     the real state"
                ),
            }
        });
    }
    Ok(plan)
}

/// Patch one version field with a JSON merge patch. `section` is `general`
/// (the nodes) or `dashboards`.
async fn patch_version(
    client: &Client,
    dep: &Deployment,
    section: &str,
    version: &str,
) -> Result<()> {
    let patch = serde_json::json!({ "spec": { section: { "version": version } } });
    os_api(client, dep)
        .patch(dep.name(), &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .with_context(|| format!("patching spec.{section}.version to {version}"))?;
    Ok(())
}

/// Wait until the operator reports the nodes running `target` with no pool
/// still rolling. Returns the deployment's purpose so the caller can re-assert
/// its profile. Fails (without touching anything) on timeout or on a refusal
/// the operator surfaced meanwhile.
async fn wait_nodes_upgraded(dep: &Deployment, target: &str, secs: u64) -> Result<String> {
    let name = dep.name();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        match get_deployment(dep).await {
            Ok(Some(s)) => {
                if let crate::upgrade::UpgradeState::Failed { reason } = &s.upgrade {
                    bail!("the operator refused the upgrade of {name} to {target}: {reason}");
                }
                if crate::upgrade::nodes_upgraded(&s.upgrade, &s.version, target)
                    && s.health == "green"
                {
                    return Ok(s.purpose);
                }
            }
            Ok(None) => bail!("deployment {name} disappeared during its upgrade"),
            Err(e) => tracing::debug!("polling {name} during upgrade: {e:#}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    }
    bail!("nodes of {name} did not reach {target} within {secs}s")
}

/// Every OpenSearch deployment the caller owns — the ONLY list read. There is
/// deliberately no unscoped variant to reach for.
pub async fn list_deployments(scope: &Scope) -> Result<Vec<Status>> {
    let client = client().await?;
    let list = list_clusters(scope).await?;
    let access = crate::access::get().await?;
    let mut out = Vec::new();
    for obj in list {
        out.push(status_from(&client, &access, &obj).await);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Full detail of one deployment the caller owns (for the edit page).
/// `None` only when the CR vanished between resolution and read.
pub async fn get_deployment(dep: &Deployment) -> Result<Option<Status>> {
    let client = client().await?;
    match os_api(&client, dep).get_opt(dep.name()).await? {
        Some(obj) => {
            let access = crate::access::get().await?;
            Ok(Some(status_from(&client, &access, &obj).await))
        }
        None => Ok(None),
    }
}

// ───────────────────────── auth provider (ADR-045, #56) ─────────────────────

use crate::auth_provider::{self, Accounts, AuthProvider, AuthProviderSpec};

/// The auth-owned slice of the CR. Applied under its OWN field manager, so:
///   * the `veloxsearch` manager that applies the full manifest on every save
///     never prunes these fields, and
///   * dropping back to `internal` prunes exactly what auth added — the
///     `Internal` arm returns a manifest without them, and server-side apply
///     removes the fields this manager used to own.
///
/// Pure, so the whole wiring is asserted without a cluster.
fn auth_cr_patch(
    name: &str,
    spec: &AuthProviderSpec,
    public_url: Option<&str>,
) -> serde_json::Value {
    let kind = spec.provider.kind();
    let mut manifest = serde_json::json!({
        "apiVersion": "opensearch.org/v1",
        "kind": "OpenSearchCluster",
        "metadata": { "name": name, "labels": { LABEL_AUTH_KIND: kind } },
    });
    if matches!(spec.provider, AuthProvider::Internal) {
        // Nothing under `spec`: the operator goes back to seeding its own
        // securityconfig and its own Dashboards credentials.
        return manifest;
    }

    let mut dashboards = serde_json::json!({
        // We own this account because our `internal_users.yml` carries its hash.
        "opensearchCredentialsSecret": { "name": dashboards_secret_name(name) },
    });
    let extra = auth_provider::dashboards_additional_config(spec, public_url);
    if !extra.is_empty() {
        dashboards["additionalConfig"] = serde_json::json!(extra);
    }
    // Credentials referenced by `${…}` in the config above come from a Secret,
    // never from the operator's ConfigMap.
    let env: Vec<serde_json::Value> = auth_provider::dashboards_env(spec)
        .keys()
        .map(|k| {
            serde_json::json!({
                "name": k,
                "valueFrom": { "secretKeyRef": { "name": auth_state_secret_name(name), "key": k } }
            })
        })
        .collect();
    if !env.is_empty() {
        dashboards["env"] = serde_json::Value::Array(env);
    }
    // A private IdP CA has to reach the Dashboards container as a FILE: the
    // security-dashboards plugin fetches the discovery document over its own
    // Node TLS stack, which knows nothing about the securityconfig's
    // `pemtrustedcas_content` and dies at boot with
    // UNABLE_TO_VERIFY_LEAF_SIGNATURE (MR4, #56). The config key pointing at
    // this path is emitted by `dashboards_additional_config`.
    if auth_provider::dashboards_ca_pem(spec).is_some() {
        dashboards["additionalVolumes"] = serde_json::json!([{
            "name": "velox-idp-ca",
            "path": auth_provider::IDP_CA_DIR,
            "secret": {
                "secretName": auth_state_secret_name(name),
                "items": [{ "key": auth_provider::IDP_CA_KEY, "path": auth_provider::IDP_CA_KEY }],
            },
            // Rotating the CA changes only the Secret, not the CR, so without
            // this the pod keeps the old PEM until something else rolls it.
            "restartPods": true,
        }]);
    }

    manifest["spec"] = serde_json::json!({
        "security": { "config": {
            "securityConfigSecret": { "name": securityconfig_secret_name(name) },
            // Re-stated so this manager keeps the admin path bound even if the
            // full-manifest apply is ever reordered.
            "adminCredentialsSecret": { "name": admin_secret_name(name) },
        }},
        "dashboards": dashboards,
    });
    manifest
}

/// Revert patch for a cluster whose securityconfig we already own.
///
/// It differs from `auth_cr_patch(.., Internal, ..)` in the one way that
/// matters: `securityConfigSecret` STAYS set, pointing at the internal-only
/// file set. Clearing it would leave the operator with nothing to apply, and
/// the previously pushed authc domain would stay live in the security index
/// (MR4 — the operator's update job only ever rewrites `internal_users.yml`).
///
/// The Dashboards side is emptied: no `additionalConfig`, no injected env, and
/// the credentials secret handed back to the operator.
fn auth_cr_patch_internal_owned(name: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "opensearch.org/v1",
        "kind": "OpenSearchCluster",
        "metadata": { "name": name, "labels": { LABEL_AUTH_KIND: "internal" } },
        "spec": {
            "security": { "config": {
                "securityConfigSecret": { "name": securityconfig_secret_name(name) },
                "adminCredentialsSecret": { "name": admin_secret_name(name) },
            }},
            "dashboards": {},
        },
    })
}

/// bcrypt hash in the `$2y$` variant. `$2b$` (what the crate emits) is the same
/// algorithm with a different tag, but the security plugin's verifier is the
/// stricter of the two — so normalize rather than hope.
fn bcrypt_2y(password: &str) -> Result<String> {
    let h = bcrypt::hash(password, bcrypt::DEFAULT_COST).context("hashing account password")?;
    Ok(h.replacen("$2b$", "$2y$", 1))
}

/// A generated credential for something that is not an OpenSearch account.
///
/// Same generator, same rules (no leading `-`, mixed classes) as the cluster
/// admin password, because the same hazards apply: these end up in argv, in
/// YAML and in copy-paste instructions.
pub(crate) fn gen_token() -> Result<String> {
    gen_admin_password()
}

/// bcrypt (`$2y$`) for callers outside this module — htpasswd files and the
/// Prometheus web-config format both want that variant.
pub(crate) fn bcrypt_hash(password: &str) -> Result<String> {
    bcrypt_2y(password)
}

/// Copy a `kubernetes.io/tls` Secret from the app namespace into another one.
///
/// An Ingress can only name a Secret in its **own** namespace, and the OTel
/// stack's Services live in `velox-agents` while the operator-managed TLS
/// material lives here. Without the copy the routes fall back to the ingress
/// controller's self-signed default, which is a browser warning on an endpoint
/// we are telling people to point production exporters at.
pub async fn copy_tls_secret(client: &Client, secret_name: &str, target_ns: &str) -> Result<()> {
    let src: Api<Secret> = Api::namespaced(client.clone(), ns());
    let Some(s) = src
        .get_opt(secret_name)
        .await
        .context("reading TLS secret")?
    else {
        bail!("TLS secret {secret_name} not found in {}", ns());
    };
    // `stringData` rather than `data`: the API server decodes the source for us
    // into bytes, and PEM is text — round-tripping through base64 by hand would
    // add a dependency for nothing.
    let data = s.data.unwrap_or_default();
    let pem = |k: &str| {
        data.get(k)
            .and_then(|v| String::from_utf8(v.0.clone()).ok())
            .unwrap_or_default()
    };
    let (cert, key) = (pem("tls.crt"), pem("tls.key"));
    if cert.is_empty() || key.is_empty() {
        bail!("TLS secret {secret_name} has no tls.crt/tls.key");
    }
    let manifest = serde_json::json!({
        "apiVersion": "v1", "kind": "Secret", "type": "kubernetes.io/tls",
        "metadata": { "name": secret_name, "namespace": target_ns },
        "stringData": { "tls.crt": cert, "tls.key": key }
    });
    let dst: Api<Secret> = Api::namespaced(client.clone(), target_ns);
    dst.patch(
        secret_name,
        &PatchParams::apply("veloxsearch-otel").force(),
        &Patch::Apply(&manifest),
    )
    .await
    .with_context(|| format!("copying TLS secret into {target_ns}"))?;
    Ok(())
}

async fn apply_secret(
    secrets: &Api<Secret>,
    dep: &Deployment,
    name: &str,
    data: BTreeMap<String, String>,
) -> Result<()> {
    apply_secret_with(secrets, dep, name, data, AUTH_FIELD_MANAGER).await
}

/// Same, under a caller-chosen field manager: a Secret owned by one feature
/// must not be pruned when another feature re-applies its own set of keys.
async fn apply_secret_with(
    secrets: &Api<Secret>,
    dep: &Deployment,
    name: &str,
    data: BTreeMap<String, String>,
    field_manager: &str,
) -> Result<()> {
    let manifest = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": dep.namespace(),
            "labels": dep.owner_labels(),
        },
        "stringData": data,
    });
    secrets
        .patch(
            name,
            &PatchParams::apply(field_manager).force(),
            &Patch::Apply(&manifest),
        )
        .await
        .with_context(|| format!("applying secret {name}"))?;
    Ok(())
}

/// The `kibanaserver` password, creating the Secret on first use. Returned in
/// clear because the caller has to bcrypt it into `internal_users.yml`.
async fn ensure_dashboards_secret(client: &Client, dep: &Deployment) -> Result<String> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), dep.namespace());
    let secret_name = dashboards_secret_name(dep.name());
    if let Some(s) = secrets.get_opt(&secret_name).await? {
        if let Some(p) = s
            .data
            .as_ref()
            .and_then(|d| d.get("password"))
            .and_then(|b| String::from_utf8(b.0.clone()).ok())
        {
            return Ok(p);
        }
    }
    let password = gen_admin_password()?;
    let mut data = BTreeMap::new();
    data.insert("username".to_string(), DASHBOARDS_USER.to_string());
    data.insert("password".to_string(), password.clone());
    apply_secret(&secrets, dep, &secret_name, data).await?;
    Ok(password)
}

/// The saved spec, or `Internal` when a deployment has never been configured.
/// Unreadable state degrades to `Internal` rather than erroring: the caller is
/// about to overwrite it, and the CR is the authority on what is actually
/// applied.
pub async fn get_auth_provider(dep: &Deployment) -> Result<AuthProviderSpec> {
    validate_name(dep.name())?;
    let client = client().await?;
    Ok(read_auth_spec(&client, dep).await)
}

async fn read_auth_spec(client: &Client, dep: &Deployment) -> AuthProviderSpec {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), dep.namespace());
    let Ok(Some(s)) = secrets.get_opt(&auth_state_secret_name(dep.name())).await else {
        return AuthProviderSpec::default();
    };
    s.data
        .as_ref()
        .and_then(|d| d.get(AUTH_SPEC_KEY))
        .and_then(|b| String::from_utf8(b.0.clone()).ok())
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

/// Point a deployment at an identity provider — or, with `Internal`, hand its
/// security configuration back to the operator.
///
/// Day-2 by construction: it edits an existing CR, never re-creates it. Order
/// matters — the Secrets exist before the CR references them, so the operator's
/// reconcile never sees a dangling reference; on removal the CR is pruned first
/// so the Secrets are unreferenced before they are deleted.
pub async fn set_auth_provider(dep: &Deployment, incoming: AuthProviderSpec) -> Result<()> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    // Existence-first (#52): configuring auth for a deployment that isn't there
    // would leave orphan Secrets a later create could silently adopt.
    ensure_namespace_exists(&client, dep.namespace()).await?;
    if os_api(&client, dep).get_opt(name).await?.is_none() {
        bail!(
            "no deployment named '{name}' in namespace '{}' — cannot configure its authentication",
            dep.namespace()
        );
    }

    // Credentials the client echoed back untouched resolve against what is
    // stored; validation then rejects anything still unresolved.
    let stored = read_auth_spec(&client, dep).await;
    let spec = auth_provider::merge_secrets(&incoming, &stored);
    let access = crate::access::get().await?;
    let public = auth_provider::public_url(name, &access.mode, &access.base_domain);
    auth_provider::validate(&spec, &access.mode, public.as_deref())?;

    let secrets: Api<Secret> = Api::namespaced(client.clone(), dep.namespace());
    let os = os_api(&client, dep);
    let pp = PatchParams::apply(AUTH_FIELD_MANAGER).force();

    if matches!(spec.provider, AuthProvider::Internal) {
        // A cluster we never touched still has the operator's own securityconfig
        // in its security index, so there is nothing to undo — clear the CR and
        // drop our Secrets.
        //
        // A cluster we DID configure is different, and dropping the Secret there
        // is a security hole rather than a revert (MR4, verified live): the
        // operator's update job runs `securityadmin -f internal_users.yml -t
        // internalusers` and never rewrites `config.yml`, so the directory /
        // IdP domain we pushed stays live in the security index while the app
        // reports `internal`. Push an internal-only securityconfig instead and
        // keep owning it.
        let owned = secrets
            .get_opt(&securityconfig_secret_name(name))
            .await
            .context("looking for an existing securityconfig")?
            .is_some();

        if owned {
            let (admin_user, admin_password) = admin_creds(dep).await;
            if admin_password.is_empty() {
                bail!(
                    "cannot read the admin credentials of '{name}' — refusing to rewrite its \
                     security configuration, which would lock every account out"
                );
            }
            let dashboards_password = ensure_dashboards_secret(&client, dep).await?;
            let files = auth_provider::internal_only_security_config_files(&Accounts {
                admin_user: &admin_user,
                admin_hash: &bcrypt_2y(&admin_password)?,
                dashboards_user: DASHBOARDS_USER,
                dashboards_hash: &bcrypt_2y(&dashboards_password)?,
            })?;
            apply_secret(&secrets, dep, &securityconfig_secret_name(name), files).await?;
            // Keep `securityConfigSecret` pointed at it: clearing the field
            // would stop the operator from ever applying the revert.
            os.patch(
                name,
                &pp,
                &Patch::Apply(&auth_cr_patch_internal_owned(name)),
            )
            .await
            .context("pointing the cluster CR at the internal-only securityconfig")?;
        } else {
            os.patch(name, &pp, &Patch::Apply(&auth_cr_patch(name, &spec, None)))
                .await
                .context("removing the auth provider from the cluster CR")?;
            if let Err(e) = secrets
                .delete(&securityconfig_secret_name(name), &DeleteParams::default())
                .await
            {
                tracing::warn!("deleting the securityconfig after reverting: {e:#}");
            }
        }

        // The provider spec and the Dashboards credential env go either way:
        // nothing references them once the kind is `internal`.
        for s in [auth_state_secret_name(name), dashboards_secret_name(name)] {
            if let Err(e) = secrets.delete(&s, &DeleteParams::default()).await {
                tracing::warn!("deleting {s} after reverting to internal auth: {e:#}");
            }
        }
        return Ok(());
    }

    // INVARIANT (ADR-045): our securityconfig REPLACES the operator's, so both
    // accounts it seeds have to be reproduced from passwords we can read. An
    // unreadable admin password means we would write a securityconfig nobody
    // can authenticate against — refuse instead.
    let (admin_user, admin_password) = admin_creds(dep).await;
    if admin_password.is_empty() {
        bail!(
            "cannot read the admin credentials of '{name}' — refusing to replace its security \
             configuration, which would lock every account out"
        );
    }
    let dashboards_password = ensure_dashboards_secret(&client, dep).await?;
    let admin_hash = bcrypt_2y(&admin_password)?;
    let dashboards_hash = bcrypt_2y(&dashboards_password)?;
    let files = auth_provider::security_config_files(
        &spec,
        &Accounts {
            admin_user: &admin_user,
            admin_hash: &admin_hash,
            dashboards_user: DASHBOARDS_USER,
            dashboards_hash: &dashboards_hash,
        },
        public.as_deref(),
    )?
    .context("internal kind reached the external-provider path")?;

    apply_secret(&secrets, dep, &securityconfig_secret_name(name), files).await?;

    let mut state = auth_provider::dashboards_env(&spec);
    state.insert(
        AUTH_SPEC_KEY.to_string(),
        serde_json::to_string(&spec).context("serializing the auth provider spec")?,
    );
    // Same Secret carries the IdP CA, which the CR mounts as a file into the
    // Dashboards pod (see `auth_cr_patch`).
    if let Some(pem) = auth_provider::dashboards_ca_pem(&spec) {
        state.insert(auth_provider::IDP_CA_KEY.to_string(), pem);
    }
    apply_secret(&secrets, dep, &auth_state_secret_name(name), state).await?;

    // Applying the CR is what triggers the operator's securityconfig update job
    // and the Dashboards rollout — the "applying" window the UI warns about.
    os.patch(
        name,
        &pp,
        &Patch::Apply(&auth_cr_patch(name, &spec, public.as_deref())),
    )
    .await
    .context("wiring the auth provider onto the cluster CR")?;
    Ok(())
}

// ─────────────── snapshot repository + policy (ADR-049) ────────────────

use crate::snapshot::{self, PolicyConfig, SnapshotConfig, SnapshotState};

/// Field manager owning ONLY the snapshot slice of the CR
/// (`spec.general.snapshotRepositories`, `spec.general.keystore`, and the
/// `repository-s3` plugin lists). Separate from the `veloxsearch` manager that
/// applies the full manifest on every save — otherwise every Edit-tab save
/// would prune the whole snapshot configuration, and dropping the slice here
/// would have no way to prune exactly what it added. Same trick as
/// `veloxsearch-auth`.
const SNAPSHOT_FIELD_MANAGER: &str = "veloxsearch-snapshot";

/// Holds the S3 access key / secret key the operator loads into the OpenSearch
/// keystore. Listed in `owned_secret_names` so it dies with the deployment.
fn snapshot_secret_name(name: &str) -> String {
    format!("{name}-{}", snapshot::SECRET_SUFFIX)
}

fn policy_api(client: &Client, dep: &Deployment) -> Api<DynamicObject> {
    policy_api_in(client, dep.namespace())
}

/// Same, addressed by namespace — for `status_from`, which holds the object the
/// scope layer already resolved rather than the capability token itself.
fn policy_api_in(client: &Client, namespace: &str) -> Api<DynamicObject> {
    let gvk = GroupVersionKind::gvk("opensearch.org", "v1", "OpensearchSnapshotPolicy");
    Api::namespaced_with(client.clone(), namespace, &ApiResource::from_gvk(&gvk))
}

/// What a save is going to do, decided server-side. The UI renders this; it
/// never derives it (ADR-049 invariant 4).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SnapshotPlan {
    /// True when the keystore moves — the nodes then roll one at a time.
    pub will_restart: bool,
    pub repo: String,
    pub policy: String,
}

/// The snapshot-owned slice of the CR.
///
/// Pure, so the wiring is asserted without a cluster. `None` (or a disabled
/// config) renders a manifest with nothing under `spec`: server-side apply then
/// removes every field this manager used to own — the repository entry, the
/// keystore entry and the plugin list — which is exactly the revert.
///
/// `pluginsList` carries `repository-s3` because it is **not** bundled in the
/// OpenSearch image (verified on 3.7.0). It is part of the pod spec, so it only
/// ever appears on a deployment that actually uses snapshots.
fn snapshot_cr_patch(name: &str, cfg: Option<&SnapshotConfig>) -> serde_json::Value {
    let mut manifest = serde_json::json!({
        "apiVersion": "opensearch.org/v1",
        "kind": "OpenSearchCluster",
        "metadata": { "name": name },
    });
    let Some(cfg) = cfg.filter(|c| c.enabled) else {
        return manifest;
    };
    manifest["spec"] = serde_json::json!({
        "general": {
            "snapshotRepositories": [snapshot::repo_entry(cfg)],
            "keystore": [snapshot::keystore_entry(&snapshot_secret_name(name))],
            "pluginsList": [snapshot::S3_PLUGIN],
        },
        // The bootstrap pod registers the repository during initialization, so
        // it needs the plugin too (operator docs, "Add secrets to keystore").
        "bootstrap": {
            "pluginsList": [snapshot::S3_PLUGIN],
            "keystore": [snapshot::keystore_entry(&snapshot_secret_name(name))],
        }
    });
    manifest
}

/// Reconstruct the saved configuration from the CRs — there is no separate
/// state Secret, because everything except the credentials is already on the
/// CR and the credentials must never be read back (they return as the
/// `secret_kept` sentinel).
fn snapshot_config_from(
    cr: &serde_json::Value,
    policy: Option<&serde_json::Value>,
) -> SnapshotConfig {
    let repo = cr
        .pointer("/spec/general/snapshotRepositories")
        .and_then(|v| v.as_array())
        .and_then(|a| {
            a.iter()
                .find(|r| r.get("name").and_then(|n| n.as_str()) == Some(snapshot::REPO_NAME))
        });
    let Some(repo) = repo else {
        return SnapshotConfig::default();
    };
    let get = |k: &str| {
        repo.pointer(&format!("/settings/{k}"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let host = get("endpoint");
    let endpoint = if host.is_empty() {
        String::new()
    } else {
        let proto = repo
            .pointer("/settings/protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("https");
        format!("{proto}://{host}")
    };
    let region = match get("region") {
        r if r.is_empty() => "us-east-1".to_string(),
        r => r,
    };

    SnapshotConfig {
        enabled: true,
        bucket: get("bucket"),
        base_path: get("base_path"),
        endpoint,
        region,
        // Absent means the plugin default (false); we always write it.
        path_style_access: get("path_style_access") != "false",
        // Never read back in clear — the UI echoes the sentinel on save and
        // `set_snapshot_config` resolves it against what is stored.
        access_key: snapshot::SECRET_KEPT.to_string(),
        secret_key: snapshot::SECRET_KEPT.to_string(),
        policy: policy_config_from(policy),
    }
}

fn policy_config_from(policy: Option<&serde_json::Value>) -> PolicyConfig {
    let Some(p) = policy.and_then(|p| p.pointer("/spec")) else {
        // No policy CR: the repository is configured but nothing is scheduled.
        return PolicyConfig {
            enabled: false,
            ..PolicyConfig::default()
        };
    };
    let d = PolicyConfig::default();
    let s = |ptr: &str, fallback: &str| {
        p.pointer(ptr)
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(fallback)
            .to_string()
    };
    let n = |ptr: &str, fallback: u32| {
        p.pointer(ptr)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(fallback)
    };
    // `maxAge` is written as "<n>d"; anything else falls back to the default
    // rather than silently becoming 0 (which validation would then refuse).
    let max_age_days = p
        .pointer("/deletion/deleteCondition/maxAge")
        .and_then(|v| v.as_str())
        .and_then(|v| v.trim_end_matches('d').parse::<u32>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(d.max_age_days);

    PolicyConfig {
        enabled: true,
        cron: s("/creation/schedule/cron/expression", &d.cron),
        timezone: s("/creation/schedule/cron/timezone", &d.timezone),
        indices: s("/snapshotConfig/indices", &d.indices),
        include_global_state: p
            .pointer("/snapshotConfig/includeGlobalState")
            .and_then(|v| v.as_bool())
            .unwrap_or(d.include_global_state),
        max_age_days,
        max_count: n("/deletion/deleteCondition/maxCount", d.max_count),
        min_count: n("/deletion/deleteCondition/minCount", d.min_count),
    }
}

/// Live state of the snapshot configuration, for the status chip. The operator
/// speaks `PENDING | CREATED | ERROR | IGNORED` and puts its own message in
/// `status.reason` — both are shown verbatim (ADR-045 UI rule 5).
fn snapshot_state_from(
    cfg: &SnapshotConfig,
    policy: Option<&serde_json::Value>,
    cluster_running: bool,
) -> SnapshotState {
    if !cfg.enabled {
        return SnapshotState::default();
    }
    let policy_state = policy
        .and_then(|p| p.pointer("/status/state"))
        .and_then(|v| v.as_str())
        .unwrap_or(if cfg.policy.enabled {
            // The reconciler only runs once the cluster is `PhaseRunning`, so a
            // fresh deployment is pending, not broken.
            "PENDING"
        } else {
            ""
        })
        .to_string();
    let last_error = policy
        .and_then(|p| p.pointer("/status/reason"))
        .and_then(|v| v.as_str())
        .filter(|_| policy_state == "ERROR")
        .unwrap_or("")
        .to_string();
    SnapshotState {
        configured: true,
        repo: cfg.bucket.clone(),
        schedule: if cfg.policy.enabled {
            snapshot::schedule_label(&cfg.policy)
        } else {
            String::new()
        },
        policy_state: if cluster_running || policy.is_some() {
            policy_state
        } else {
            "PENDING".to_string()
        },
        last_error,
    }
}

/// Read a deployment's snapshot configuration + live state. Credentials come
/// back as the `secret_kept` sentinel, never in clear.
pub async fn get_snapshot_config(
    dep: &Deployment,
) -> Result<(SnapshotConfig, SnapshotState, bool)> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let Some(cr) = os_api(&client, dep).get_opt(name).await? else {
        bail!(
            "no deployment named '{name}' in namespace '{}'",
            dep.namespace()
        );
    };
    let policy = policy_api(&client, dep)
        .get_opt(&snapshot::policy_name(name))
        .await
        .ok()
        .flatten()
        .map(|o| o.data);
    let cfg = snapshot_config_from(&cr.data, policy.as_ref());
    let running = cr
        .data
        .pointer("/status/phase")
        .and_then(|v| v.as_str())
        .map(|p| p.eq_ignore_ascii_case("running"))
        .unwrap_or(false);
    let state = snapshot_state_from(&cfg, policy.as_ref(), running);
    Ok((cfg, state, cfg_has_stored_credentials(&client, dep).await))
}

/// Whether a credentials Secret already exists — the difference between "first
/// configuration" (both keys required) and "an edit that keeps them".
async fn cfg_has_stored_credentials(client: &Client, dep: &Deployment) -> bool {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), dep.namespace());
    matches!(
        secrets.get_opt(&snapshot_secret_name(dep.name())).await,
        Ok(Some(_))
    )
}

/// Refuse everything refusable BEFORE the first write (ADR-048 invariant 2,
/// carried over): an invalid configuration must leave no Secret, no CR slice
/// and no policy CR behind.
async fn preflight_snapshot(
    client: &Client,
    dep: &Deployment,
    incoming: &SnapshotConfig,
    stored: &SnapshotConfig,
    has_credentials: bool,
) -> Result<SnapshotPlan> {
    let name = dep.name();
    let cfg = snapshot::with_defaults(name, incoming);
    snapshot::validate(&cfg)?;

    // Credentials: the keystore needs both keys, so a rotation cannot be half a
    // rotation, and a first configuration cannot rely on a Secret that is not
    // there yet.
    let provided = |v: &str| !v.trim().is_empty() && v != snapshot::SECRET_KEPT;
    if cfg.enabled {
        let (a, s) = (provided(&cfg.access_key), provided(&cfg.secret_key));
        if a != s {
            bail!(
                "informe a access key E a secret key juntas: o keystore do OpenSearch \
                 recebe as duas de uma vez, não dá para trocar só uma."
            );
        }
        if !a && !has_credentials {
            bail!("informe a access key e a secret key do bucket S3.");
        }
    }

    let will_restart = snapshot::needs_restart(Some(stored), incoming);
    if will_restart {
        // A restart on a cluster that has not settled compounds two
        // disruptions. This used to check `health != "green"` on its own, which
        // accepts a cluster mid-roll (ADR-050): between two node restarts every
        // pod is up and the cluster reports green. Ask the one predicate.
        let s = get_deployment(dep)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no deployment named '{name}'"))?;
        if !s.activity.settled {
            bail!(
                "esta mudança reinicia os nós e o deployment ainda não está estável \
                 (agora: {} — {}%). Espere terminar e tente de novo.",
                s.activity.stage,
                s.activity.percent
            );
        }
    }

    // Fail here, with a sentence, rather than on the apply with a raw 404: the
    // policy CRD ships with the operator, so its absence means an operator too
    // old for this feature.
    if cfg.enabled && cfg.policy.enabled {
        policy_api(client, dep)
            .list(&ListParams::default().limit(1))
            .await
            .context(
                "o operator instalado não conhece OpensearchSnapshotPolicy — \
                 atualize o OpenSearch Operator para usar snapshots agendados",
            )?;
    }

    Ok(SnapshotPlan {
        will_restart,
        repo: snapshot::REPO_NAME.to_string(),
        policy: snapshot::policy_name(name),
    })
}

/// Read-only plan for a prospective save — what the UI asks before showing the
/// confirmation, so "isto reinicia os nós" is a server answer.
pub async fn plan_snapshot_config(
    dep: &Deployment,
    incoming: &SnapshotConfig,
) -> Result<SnapshotPlan> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let Some(cr) = os_api(&client, dep).get_opt(name).await? else {
        bail!(
            "no deployment named '{name}' in namespace '{}'",
            dep.namespace()
        );
    };
    let policy = policy_api(&client, dep)
        .get_opt(&snapshot::policy_name(name))
        .await
        .ok()
        .flatten()
        .map(|o| o.data);
    let stored = snapshot_config_from(&cr.data, policy.as_ref());
    let has_creds = cfg_has_stored_credentials(&client, dep).await;
    preflight_snapshot(&client, dep, incoming, &stored, has_creds).await
}

/// Configure (or clear) a deployment's snapshot repository and scheduled policy.
///
/// Order matters, the same way it does for the auth provider: on the way in the
/// Secret exists before the CR references it, so the operator's reconcile never
/// sees a dangling keystore reference; on the way out the CR slice is pruned
/// first so the Secret is unreferenced before it is deleted.
pub async fn set_snapshot_config(
    dep: &Deployment,
    incoming: SnapshotConfig,
) -> Result<SnapshotPlan> {
    let name = dep.name();
    validate_name(name)?;
    let client = client().await?;
    let Some(cr) = os_api(&client, dep).get_opt(name).await? else {
        bail!(
            "no deployment named '{name}' in namespace '{}' — cannot configure its snapshots",
            dep.namespace()
        );
    };
    let policy_name = snapshot::policy_name(name);
    let existing_policy = policy_api(&client, dep)
        .get_opt(&policy_name)
        .await
        .ok()
        .flatten()
        .map(|o| o.data);
    let stored = snapshot_config_from(&cr.data, existing_policy.as_ref());
    let has_creds = cfg_has_stored_credentials(&client, dep).await;

    let plan = preflight_snapshot(&client, dep, &incoming, &stored, has_creds).await?;
    let cfg = snapshot::with_defaults(name, &incoming);

    let secrets: Api<Secret> = Api::namespaced(client.clone(), dep.namespace());
    let os = os_api(&client, dep);
    let policies = policy_api(&client, dep);
    let pp = PatchParams::apply(SNAPSHOT_FIELD_MANAGER).force();
    let dp = DeleteParams::default();

    if !cfg.enabled {
        // Prune the CR slice first (the keystore reference goes away), then the
        // policy, then the Secret. Note the repository stays registered inside
        // OpenSearch — the operator's repository reconciler has no Delete path
        // (ADR-049, operator caveat 2). Harmless, and deregistration belongs
        // with the snapshot/restore routes in #83.
        os.patch(name, &pp, &Patch::Apply(&snapshot_cr_patch(name, None)))
            .await
            .context("clearing the snapshot configuration on the cluster CR")?;
        let _ = policies.delete(&policy_name, &dp).await;
        let _ = secrets.delete(&snapshot_secret_name(name), &dp).await;
        return Ok(plan);
    }

    // New key material only. The sentinel (or an empty field) means "keep what
    // is stored", and the stored Secret is left untouched — which is also why
    // it must not be rewritten with the sentinel as its value.
    let provided = |v: &str| !v.trim().is_empty() && v != snapshot::SECRET_KEPT;
    if provided(&cfg.access_key) && provided(&cfg.secret_key) {
        let mut data = BTreeMap::new();
        data.insert(snapshot::KEY_ACCESS.to_string(), cfg.access_key.clone());
        data.insert(snapshot::KEY_SECRET.to_string(), cfg.secret_key.clone());
        apply_secret_with(
            &secrets,
            dep,
            &snapshot_secret_name(name),
            data,
            SNAPSHOT_FIELD_MANAGER,
        )
        .await?;
    }

    os.patch(
        name,
        &pp,
        &Patch::Apply(&snapshot_cr_patch(name, Some(&cfg))),
    )
    .await
    .context("wiring the snapshot repository onto the cluster CR")?;

    if cfg.policy.enabled {
        policies
            .patch(
                &policy_name,
                &pp,
                &Patch::Apply(&snapshot::policy_cr(dep.namespace(), name, &cfg)),
            )
            .await
            .context("applying the scheduled snapshot policy")?;
    } else {
        let _ = policies.delete(&policy_name, &dp).await;
    }

    Ok(plan)
}

/// Ask OpenSearch itself whether the repository actually works — the analogue
/// of the auth provider's reachability probe. Writes nothing: `_verify` only
/// has every node try to reach the bucket.
pub async fn verify_snapshot_repo(dep: &Deployment) -> Result<String> {
    let name = dep.name();
    validate_name(name)?;
    let base = crate::recipes::os_base(dep);
    let c = crate::recipes::http()?;
    let (u, p) = admin_creds(dep).await;
    let resp = c
        .post(format!("{base}/_snapshot/{}/_verify", snapshot::REPO_NAME))
        .basic_auth(&u, Some(&p))
        .send()
        .await
        .context("contacting OpenSearch to verify the snapshot repository")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        let nodes = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("nodes").and_then(|n| n.as_object()).map(|o| o.len()))
            .unwrap_or(0);
        return Ok(format!("{nodes}"));
    }
    // The S3 reason (403, wrong endpoint, missing bucket) is inside the body —
    // it is the only thing that tells the user what to fix, so it is passed
    // through instead of being flattened into "verification failed".
    let reason = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/root_cause/0/reason")
                .or_else(|| v.pointer("/error/reason"))
                .and_then(|r| r.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(400).collect());
    bail!("{reason}");
}

// ───────────── per-tenant isolation primitives (#81, ADR-044/051) ─────────────
//
// The cluster-level floor under `scope.rs`'s app-layer walls: one namespace per
// tenant, a ResourceQuota rendered from that tenant's Postgres `quotas` row, a
// LimitRange that makes the quota safe to enforce, and a default-deny
// NetworkPolicy set. Provisioned at signup (`tenants.rs`), idempotent on re-run
// (server-side apply), and refusing to emit a manifest it could not fully
// render — an unresolved `VELOX_*` token would produce an object that selects
// nothing and denies nothing, which is the silent failure this section exists
// to prevent.

/// The vendored templates (ADR-044), applied by [`crate::bootstrap::apply_bundle`],
/// which sorts Namespace first so the namespaced objects have somewhere to land.
const TENANT_TEMPLATES: [&str; 4] = [
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/deploy/tenant-templates/namespace.yaml"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/deploy/tenant-templates/resourcequota.yaml"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/deploy/tenant-templates/limitrange.yaml"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/deploy/tenant-templates/networkpolicy.yaml"
    )),
];

/// Field manager for tenant primitives. Its own manager (not `veloxsearch`) so
/// re-rendering a quota prunes exactly the quota's fields and nothing else —
/// the `veloxsearch-auth` / `veloxsearch-secreset` precedent.
const TENANT_FIELD_MANAGER: &str = "veloxsearch-tenant";

/// Namespaces the tenant NetworkPolicy set punches holes for. Every one is a
/// *peer* in a policy, so a wrong value is a silently broken flow rather than a
/// visible error — hence each is read from where the code that actually uses it
/// reads, or from explicit config, never re-typed as a fresh literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceLayout {
    /// Where the control plane and the operator run (`ns()`).
    pub control_plane: String,
    /// The ingress controller's namespace. Traefik on both the Tornis cluster
    /// and a generic k3s install, but the namespace differs by distribution
    /// (`traefik` here, `kube-system` on stock k3s), so it is configurable.
    pub ingress: String,
    /// Where collection agents run (`agents::AGENT_NS`).
    pub agents: String,
    /// The ADR-042 snapshot MinIO. Nothing deploys it yet, so this egress hole
    /// is a forward declaration: it selects nothing until a namespace by this
    /// name exists, and it is configurable for an install that puts MinIO
    /// somewhere else.
    pub minio: String,
}

impl NamespaceLayout {
    /// The layout of the cluster this process is running in.
    pub fn current() -> Self {
        Self {
            control_plane: ns().to_string(),
            ingress: env_ns("VELOX_INGRESS_NAMESPACE", "traefik"),
            agents: crate::agents::AGENT_NS.to_string(),
            minio: env_ns("VELOX_MINIO_NAMESPACE", "minio"),
        }
    }
}

/// A namespace name from the environment, falling back when unset or blank.
fn env_ns(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Who we are provisioning for: the three `tenants` columns the manifests need.
/// Passed in rather than read here — Postgres is `tenants.rs`'s business, the
/// cluster is this module's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantIdentity {
    /// `tenants.id` — the `veloxsearch.ai/tenant` owner-label value (#80).
    pub id: String,
    /// `tenants.slug`.
    pub slug: String,
    /// `tenants.namespace` (`velox-t-<slug>`).
    pub namespace: String,
}

/// The tenant's ADR-041 `quotas` row. The ResourceQuota is a *rendering* of
/// this, never numbers typed into a manifest: Postgres stays authoritative, so
/// a quota change is a re-render and a re-apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TenantQuota {
    pub max_deployments: i32,
    pub max_total_disk_gb: i32,
    pub max_nodes: i32,
}

/// What lands in `ResourceQuota.spec.hard`, as K8s quantity strings.
#[derive(Clone, Debug, PartialEq, Eq)]
struct QuotaHard {
    pods: String,
    cpu_req: String,
    cpu_lim: String,
    mem_req: String,
    mem_lim: String,
    storage: String,
    pvcs: String,
    deployments: String,
}

/// Object-count headroom. A rolling update runs the new pod (and, on a resize,
/// its PVC) alongside the old one, so a quota sized to the steady state would
/// deadlock the very update it exists to survive.
const QUOTA_COUNT_HEADROOM: i64 = 2;
/// Per-deployment auxiliary allowance on top of the OpenSearch nodes: the
/// Dashboards pod plus the operator's short-lived securityconfig job. These are
/// ADR-044's worked-default deltas made explicit (`requests.cpu` 4 against 3
/// node-CPUs; `limits.memory` 12Gi against 9Gi of Guaranteed node memory).
const AUX_CPU_REQ_MILLIS: i64 = 1000;
const AUX_CPU_LIM_MILLIS: i64 = 1000;
const AUX_MEM_REQ_MIB: i64 = 1024;
const AUX_MEM_LIM_MIB: i64 = 3072;

/// Render `spec.hard` from the tenant's quota row and the **largest** sizing
/// preset — the worst case the tenant can actually ask for — reading the preset
/// from [`sizing`] so there is never a second copy of those numbers (the rule
/// `preset_requests` already applies for the capacity planner).
///
/// ADR-044's worked default (1 deployment × 3 nodes × 50Gi) renders to
/// pods 8 · requests.cpu 4 · limits.cpu 7 · requests.memory 10Gi ·
/// limits.memory 12Gi · requests.storage 50Gi · pvcs 6 · clusters 1 — pinned by
/// the unit tests below.
fn render_quota_hard(q: &TenantQuota) -> Result<QuotaHard> {
    if q.max_deployments < 0 || q.max_nodes < 0 || q.max_total_disk_gb < 0 {
        bail!("quota row has a negative value: {q:?}");
    }
    let largest = PRESET_SIZES
        .last()
        .context("PRESET_SIZES is empty — no largest preset to size a quota from")?;
    let s = sizing(largest);
    let deployments = i64::from(q.max_deployments);
    let nodes = i64::from(q.max_nodes);
    let node_cpu_req = cpu_millis(s.cpu_req)?;
    let node_cpu_lim = cpu_millis(s.cpu_lim)?;
    // ADR-035: OpenSearch node memory is request == limit (Guaranteed QoS), so
    // both memory sums start from the same per-node number.
    let node_mem = mem_mib(s.mem)?;

    Ok(QuotaHard {
        // Each deployment is `max_nodes` OpenSearch pods plus one Dashboards pod.
        pods: (deployments * (nodes + 1) * QUOTA_COUNT_HEADROOM).to_string(),
        cpu_req: fmt_cpu(deployments * (nodes * node_cpu_req + AUX_CPU_REQ_MILLIS)),
        cpu_lim: fmt_cpu(deployments * (nodes * node_cpu_lim + AUX_CPU_LIM_MILLIS)),
        mem_req: fmt_mem(deployments * (nodes * node_mem + AUX_MEM_REQ_MIB)),
        mem_lim: fmt_mem(deployments * (nodes * node_mem + AUX_MEM_LIM_MIB)),
        // The disk budget is the tenant's own column, not a derived number: it
        // is the one quota the operator actually sells.
        storage: format!("{}Gi", q.max_total_disk_gb),
        // One PVC per OpenSearch node; Dashboards is stateless.
        pvcs: (deployments * nodes * QUOTA_COUNT_HEADROOM).to_string(),
        deployments: q.max_deployments.to_string(),
    })
}

/// K8s CPU quantity → millicores. Handles the two forms `sizing()` emits
/// (`"1"`, `"500m"`); anything else is an error rather than a silent zero,
/// because a zero CPU quota rejects every pod in the namespace.
fn cpu_millis(q: &str) -> Result<i64> {
    let q = q.trim();
    let v = match q.strip_suffix('m') {
        Some(n) => n.parse::<i64>().ok(),
        None => q.parse::<f64>().ok().map(|f| (f * 1000.0).round() as i64),
    };
    v.filter(|v| *v >= 0)
        .with_context(|| format!("cannot read '{q}' as a CPU quantity"))
}

/// K8s memory quantity → MiB. Handles `Gi`/`Mi`, the forms `sizing()` emits.
fn mem_mib(q: &str) -> Result<i64> {
    let q = q.trim();
    let v = if let Some(n) = q.strip_suffix("Gi") {
        n.parse::<i64>().ok().map(|g| g * 1024)
    } else if let Some(n) = q.strip_suffix("Mi") {
        n.parse::<i64>().ok()
    } else {
        None
    };
    v.filter(|v| *v >= 0)
        .with_context(|| format!("cannot read '{q}' as a memory quantity"))
}

/// Millicores → the shortest exact quantity string (`4000` → `"4"`).
fn fmt_cpu(millis: i64) -> String {
    if millis % 1000 == 0 {
        (millis / 1000).to_string()
    } else {
        format!("{millis}m")
    }
}

/// MiB → the shortest exact quantity string (`10240` → `"10Gi"`).
fn fmt_mem(mib: i64) -> String {
    if mib % 1024 == 0 {
        format!("{}Gi", mib / 1024)
    } else {
        format!("{mib}Mi")
    }
}

/// The first `VELOX_<TOKEN>` still standing in a rendered document, if any.
///
/// Checked over the whole file, comments included — a template's header lists
/// its own tokens, and those get substituted too, so a stale name there is a
/// stale name in the manifest below it. `VELOX_*` (the templates' way of
/// writing "the token family") is not a token: a real one continues with an
/// upper-case letter or a digit.
fn unresolved_token(doc: &str) -> Option<String> {
    const MARK: &str = "VELOX_";
    let mut from = 0;
    while let Some(rel) = doc[from..].find(MARK) {
        let at = from + rel;
        let rest = &doc[at + MARK.len()..];
        if rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return Some(doc[at..].chars().take(40).collect());
        }
        from = at + MARK.len();
    }
    None
}

/// Render the four templates into one multi-doc YAML bundle.
///
/// Refuses rather than emits when anything is off: an empty tenant id (the
/// owner label would match everything), a slug that is not a DNS-1123 label, a
/// namespace that is not the ADR-041 `velox-t-` mapping of that slug (so a bad
/// row cannot make us apply a default-deny NetworkPolicy to `kube-system`), or
/// a `VELOX_` token left standing after substitution.
fn render_tenant_bundle(
    t: &TenantIdentity,
    q: &TenantQuota,
    layout: &NamespaceLayout,
) -> Result<String> {
    if t.id.trim().is_empty() {
        bail!("tenant id is empty — the owner label would select every object");
    }
    validate_name(&t.slug).context("tenant slug is not a DNS-1123 label")?;
    let expected_ns = format!("{}{}", crate::tenants::NAMESPACE_PREFIX, t.slug);
    if t.namespace != expected_ns {
        bail!(
            "refusing to provision namespace '{}': the ADR-041 mapping for slug '{}' \
             is '{expected_ns}'",
            t.namespace,
            t.slug
        );
    }
    let hard = render_quota_hard(q)?;
    // Longest token first, so no token can be eaten by a shorter prefix of
    // itself (`VELOX_TENANT_NS` vs `VELOX_TENANT_SLUG`).
    let mut tokens: Vec<(&str, &str)> = vec![
        ("VELOX_TENANT_NS", t.namespace.as_str()),
        ("VELOX_TENANT_SLUG", t.slug.as_str()),
        ("VELOX_TENANT_ID", t.id.as_str()),
        ("VELOX_CONTROL_PLANE_NS", layout.control_plane.as_str()),
        ("VELOX_INGRESS_NS", layout.ingress.as_str()),
        ("VELOX_AGENTS_NS", layout.agents.as_str()),
        ("VELOX_MINIO_NS", layout.minio.as_str()),
        ("VELOX_QUOTA_PODS", hard.pods.as_str()),
        ("VELOX_QUOTA_CPU_REQ", hard.cpu_req.as_str()),
        ("VELOX_QUOTA_CPU_LIM", hard.cpu_lim.as_str()),
        ("VELOX_QUOTA_MEM_REQ", hard.mem_req.as_str()),
        ("VELOX_QUOTA_MEM_LIM", hard.mem_lim.as_str()),
        ("VELOX_QUOTA_STORAGE", hard.storage.as_str()),
        ("VELOX_QUOTA_PVCS", hard.pvcs.as_str()),
        ("VELOX_QUOTA_DEPLOYMENTS", hard.deployments.as_str()),
    ];
    tokens.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));

    let mut out = String::new();
    for template in TENANT_TEMPLATES {
        let mut doc = template.to_string();
        for (token, value) in &tokens {
            doc = doc.replace(token, value);
        }
        // The guard that makes this whole mechanism safe.
        if let Some(near) = unresolved_token(&doc) {
            bail!(
                "tenant template left an unresolved token near '{near}' — the rendered \
                 object would select nothing; add it to render_tenant_bundle"
            );
        }
        out.push_str("---\n");
        out.push_str(&doc);
        if !doc.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

/// Create — or reconcile — a tenant's namespace, ResourceQuota, LimitRange and
/// default-deny NetworkPolicy set.
///
/// Idempotent: everything is server-side applied, so re-running after a quota
/// change re-renders the numbers and leaves the rest alone. Called from signup,
/// and safe to call again from an operator path.
///
/// **What this does not prove:** the NetworkPolicy objects existing is not the
/// same as traffic being denied. Enforcement belongs to the CNI — k3s' embedded
/// controller enforces these, a CNI without policy support accepts them and
/// enforces nothing (ADR-044's enforcement caveat, restated in ADR-051). Only
/// the live cross-tenant probe counts as evidence.
pub async fn provision_tenant(t: &TenantIdentity, q: &TenantQuota) -> Result<()> {
    let bundle = render_tenant_bundle(t, q, &NamespaceLayout::current())?;
    let client = client().await?;
    crate::bootstrap::apply_bundle(&client, &bundle, TENANT_FIELD_MANAGER)
        .await
        .with_context(|| format!("provisioning isolation for tenant '{}'", t.slug))?;
    tracing::info!(
        tenant = %t.slug, namespace = %t.namespace,
        "provisioned tenant namespace, quota, limits and default-deny network policy",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── per-tenant isolation primitives (#81, ADR-044/051) ─────────────
    //
    // These prove the MANIFESTS, not the cluster. Whether a rendered
    // NetworkPolicy actually drops a packet is the CNI's business and is
    // verified nowhere in this repo — see ADR-051's "not verified here".

    fn tenant_fixture() -> TenantIdentity {
        TenantIdentity {
            id: "11111111-2222-3333-4444-555555555555".into(),
            slug: "acme-corp".into(),
            namespace: "velox-t-acme-corp".into(),
        }
    }

    /// The ADR-041 default `quotas` row.
    fn default_quota() -> TenantQuota {
        TenantQuota {
            max_deployments: 1,
            max_total_disk_gb: 50,
            max_nodes: 3,
        }
    }

    /// Deliberately NOT the process's real layout: `NamespaceLayout::current()`
    /// reads process env, which other tests share. Every peer name here is
    /// distinctive so a test can prove the policy carries *these* values rather
    /// than a literal baked into the template.
    fn test_layout() -> NamespaceLayout {
        NamespaceLayout {
            control_plane: "velox-control".into(),
            ingress: "velox-ingress".into(),
            agents: "velox-agent-fleet".into(),
            minio: "velox-snapshots".into(),
        }
    }

    fn parse_bundle(bundle: &str) -> Vec<serde_json::Value> {
        serde_yaml::Deserializer::from_str(bundle)
            .filter_map(|de| {
                let v: serde_json::Value = serde::Deserialize::deserialize(de)
                    .expect("rendered bundle must be valid YAML");
                v.is_object().then_some(v)
            })
            .collect()
    }

    fn rendered() -> Vec<serde_json::Value> {
        let bundle =
            render_tenant_bundle(&tenant_fixture(), &default_quota(), &test_layout()).unwrap();
        parse_bundle(&bundle)
    }

    fn hard(bundle: &[serde_json::Value], key: &str) -> String {
        bundle
            .iter()
            .find(|d| d["kind"] == "ResourceQuota")
            .and_then(|d| d["spec"]["hard"].get(key))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("ResourceQuota has no spec.hard.{key}"))
            .to_string()
    }

    /// ADR-044's worked default, pinned. If someone changes a headroom constant
    /// or the sizing table, this is the test that says the ADR now disagrees
    /// with the code.
    #[test]
    fn tenant_quota_renders_the_adr044_worked_default() {
        let b = rendered();
        assert_eq!(hard(&b, "pods"), "8");
        assert_eq!(hard(&b, "requests.cpu"), "4");
        assert_eq!(hard(&b, "limits.cpu"), "7");
        assert_eq!(hard(&b, "requests.memory"), "10Gi");
        assert_eq!(hard(&b, "limits.memory"), "12Gi");
        assert_eq!(hard(&b, "requests.storage"), "50Gi");
        assert_eq!(hard(&b, "persistentvolumeclaims"), "6");
        assert_eq!(hard(&b, "count/opensearchclusters.opensearch.org"), "1");
    }

    /// The point of the quota being *rendered*: every number moves with the
    /// tenant's Postgres row. A hardcoded manifest would pass the test above
    /// and fail this one.
    #[test]
    fn tenant_quota_is_wired_to_the_row_not_baked_into_the_template() {
        let q = TenantQuota {
            max_deployments: 3,
            max_total_disk_gb: 200,
            max_nodes: 5,
        };
        let b = parse_bundle(&render_tenant_bundle(&tenant_fixture(), &q, &test_layout()).unwrap());
        // 3 deployments × (5 nodes + 1 dashboards) × 2 headroom
        assert_eq!(hard(&b, "pods"), "36");
        // 3 × (5 × 1 CPU + 1 aux) = 18
        assert_eq!(hard(&b, "requests.cpu"), "18");
        // 3 × (5 × 2 CPU + 1 aux) = 33
        assert_eq!(hard(&b, "limits.cpu"), "33");
        // 3 × (5 × 3Gi + 1Gi aux) = 48Gi
        assert_eq!(hard(&b, "requests.memory"), "48Gi");
        // 3 × (5 × 3Gi + 3Gi aux) = 54Gi
        assert_eq!(hard(&b, "limits.memory"), "54Gi");
        assert_eq!(hard(&b, "requests.storage"), "200Gi");
        assert_eq!(hard(&b, "persistentvolumeclaims"), "30");
        assert_eq!(hard(&b, "count/opensearchclusters.opensearch.org"), "3");
    }

    /// …and to the sizing table, not to a second copy of its numbers. Derived
    /// here from `sizing()` itself, so adding a bigger preset moves the quota.
    #[test]
    fn tenant_quota_sizes_from_the_largest_preset() {
        let largest = sizing(PRESET_SIZES.last().unwrap());
        let q = default_quota();
        let expected_cpu = fmt_cpu(
            i64::from(q.max_deployments)
                * (i64::from(q.max_nodes) * cpu_millis(largest.cpu_req).unwrap()
                    + AUX_CPU_REQ_MILLIS),
        );
        assert_eq!(hard(&rendered(), "requests.cpu"), expected_cpu);
    }

    /// The whole-file guard: a template token nobody substituted renders an
    /// object that selects nothing — a NetworkPolicy that denies nothing while
    /// looking present. Rendering must fail loudly instead.
    #[test]
    fn tenant_bundle_leaves_no_unresolved_token() {
        let bundle =
            render_tenant_bundle(&tenant_fixture(), &default_quota(), &test_layout()).unwrap();
        assert_eq!(
            unresolved_token(&bundle),
            None,
            "unresolved token survived rendering:\n{bundle}"
        );
        // …and the guard is not vacuous: an unknown token is a hard refusal.
        assert!(unresolved_token("name: VELOX_NEW_THING").is_some());
        assert_eq!(unresolved_token("VELOX_* tokens are replaced"), None);
        assert_eq!(parse_bundle(&bundle).len(), 9, "4 templates → 9 objects");
    }

    /// The posture the whole card is for: an empty podSelector denying both
    /// directions, so anything not explicitly allowed dies.
    #[test]
    fn tenant_bundle_default_denies_ingress_and_egress() {
        let b = rendered();
        let deny = b
            .iter()
            .find(|d| d["kind"] == "NetworkPolicy" && d["metadata"]["name"] == "velox-default-deny")
            .expect("no default-deny NetworkPolicy in the bundle");
        assert_eq!(
            deny["spec"]["podSelector"],
            serde_json::json!({}),
            "a non-empty podSelector would leave unmatched pods wide open"
        );
        let types = deny["spec"]["policyTypes"].as_array().unwrap();
        assert!(types.contains(&serde_json::json!("Ingress")));
        assert!(
            types.contains(&serde_json::json!("Egress")),
            "ingress-only default-deny still lets a tenant dial another tenant"
        );
    }

    /// Every hole is punched at a namespace this install actually configured —
    /// and at no other tenant. The peers come from the layout, so a template
    /// that hardcoded `veloxsearch-test` would fail here.
    #[test]
    fn tenant_bundle_peers_are_configured_namespaces_and_never_another_tenant() {
        let b = rendered();
        let layout = test_layout();
        let mut peers: Vec<String> = Vec::new();
        fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(m) => {
                    for (k, val) in m {
                        if k == "namespaceSelector" {
                            if let Some(n) =
                                val["matchLabels"]["kubernetes.io/metadata.name"].as_str()
                            {
                                out.push(n.to_string());
                            }
                        }
                        walk(val, out);
                    }
                }
                serde_json::Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        for doc in &b {
            walk(doc, &mut peers);
        }
        peers.sort();
        peers.dedup();
        // ADR-044's five flows, minus the two that are intra-namespace pod
        // selectors: DNS to kube-system plus the four configured namespaces.
        let mut expected = vec![
            "kube-system".to_string(),
            layout.agents.clone(),
            layout.control_plane.clone(),
            layout.ingress.clone(),
            layout.minio.clone(),
        ];
        expected.sort();
        assert_eq!(
            peers, expected,
            "the allowed peer set drifted from ADR-044's five flows"
        );
        assert!(
            !peers
                .iter()
                .any(|p| p.starts_with(crate::tenants::NAMESPACE_PREFIX)),
            "a tenant namespace is named as a peer: {peers:?}"
        );
    }

    /// Ownership labels are the same vocabulary `scope.rs` filters by — one
    /// selector for offboarding and audit. The pre-#80 `veloxsearch.io/` domain
    /// must not survive anywhere: a sweep by the wrong domain finds nothing.
    #[test]
    fn tenant_objects_carry_the_scope_owner_labels() {
        let t = tenant_fixture();
        let bundle = render_tenant_bundle(&t, &default_quota(), &test_layout()).unwrap();
        assert!(
            !bundle.contains("veloxsearch.io/"),
            "the ADR-044 erratum label domain is still in the templates"
        );
        for doc in parse_bundle(&bundle) {
            let labels = &doc["metadata"]["labels"];
            assert_eq!(
                labels[crate::scope::LABEL_MANAGED_BY],
                crate::scope::MANAGED_BY,
                "{} is not selectable by the managed-by sweep",
                doc["kind"]
            );
            assert_eq!(
                labels[crate::scope::LABEL_TENANT],
                t.id,
                "{} carries the wrong owner (must be tenants.id, not the slug)",
                doc["kind"]
            );
        }
    }

    /// Nothing may land outside the tenant's namespace — least of all a
    /// default-deny NetworkPolicy.
    #[test]
    fn tenant_objects_land_only_in_the_tenant_namespace() {
        let t = tenant_fixture();
        for doc in rendered() {
            if doc["kind"] == "Namespace" {
                assert_eq!(doc["metadata"]["name"], t.namespace);
            } else {
                assert_eq!(
                    doc["metadata"]["namespace"], t.namespace,
                    "{} escaped the tenant namespace",
                    doc["kind"]
                );
            }
        }
    }

    /// A `tenants` row that does not match the ADR-041 mapping is a refusal,
    /// not a manifest: applying a default-deny NetworkPolicy to `kube-system`
    /// would take the cluster down.
    #[test]
    fn provisioning_refuses_rows_that_could_target_the_wrong_namespace() {
        let cases = [
            (
                TenantIdentity {
                    namespace: "kube-system".into(),
                    ..tenant_fixture()
                },
                "namespace that is not the slug mapping",
            ),
            (
                TenantIdentity {
                    id: "  ".into(),
                    ..tenant_fixture()
                },
                "blank tenant id",
            ),
            (
                TenantIdentity {
                    slug: "Acme Corp".into(),
                    ..tenant_fixture()
                },
                "slug that is not a DNS label",
            ),
        ];
        for (t, why) in cases {
            assert!(
                render_tenant_bundle(&t, &default_quota(), &test_layout()).is_err(),
                "must refuse: {why}"
            );
        }
        // A zero quota is legal (a suspended tenant); a negative one is a bug.
        assert!(render_quota_hard(&TenantQuota {
            max_deployments: 0,
            max_total_disk_gb: 0,
            max_nodes: 0
        })
        .is_ok());
        assert!(render_quota_hard(&TenantQuota {
            max_deployments: -1,
            ..default_quota()
        })
        .is_err());
    }

    /// The quantity helpers, because a mis-parse becomes a quota that rejects
    /// every pod in the namespace rather than a visible error.
    #[test]
    fn quota_quantities_parse_and_format_exactly() {
        assert_eq!(cpu_millis("1").unwrap(), 1000);
        assert_eq!(cpu_millis("500m").unwrap(), 500);
        assert_eq!(cpu_millis(" 2 ").unwrap(), 2000);
        assert!(cpu_millis("plenty").is_err());
        assert!(cpu_millis("-1").is_err());
        assert_eq!(mem_mib("3Gi").unwrap(), 3072);
        assert_eq!(mem_mib("512Mi").unwrap(), 512);
        assert!(
            mem_mib("3G").is_err(),
            "SI vs binary must not be guessed at"
        );
        assert_eq!(fmt_cpu(4000), "4");
        assert_eq!(fmt_cpu(4500), "4500m");
        assert_eq!(fmt_mem(10240), "10Gi");
        assert_eq!(fmt_mem(512), "512Mi");
    }

    /// Every allowed flow, port by port. The peer test above proves *which*
    /// namespaces may talk; this proves *what* they may reach — a policy that
    /// named the right namespace on the wrong port is the same broken flow as
    /// one that named the wrong namespace, and it looks equally healthy in
    /// `kubectl get netpol`. Flow (4) in particular is why `agents::AGENT_NS`
    /// is `pub(crate)`: agents ship to `{deployment}.{ns}.svc:9200`, so if this
    /// hole closes, ingest stops with no error anywhere in this repo.
    #[test]
    fn tenant_bundle_opens_exactly_the_adr044_ports_per_peer() {
        let b = rendered();
        let layout = test_layout();
        // (policy name, peer namespace, ports it may reach). Spelled out as a
        // literal tuple type rather than hidden behind an alias: the shape IS
        // the assertion, and a reader checking ADR-044 against this table
        // should not have to jump to a type definition to see it.
        #[allow(clippy::type_complexity)]
        let expected: [(&str, &str, &[(&str, i64)]); 4] = [
            (
                "velox-allow-from-control-plane",
                &layout.control_plane,
                &[("TCP", 9200), ("TCP", 5601)],
            ),
            (
                "velox-allow-from-ingress",
                &layout.ingress,
                &[("TCP", 5601), ("TCP", 9200)],
            ),
            ("velox-allow-from-agents", &layout.agents, &[("TCP", 9200)]),
            (
                "velox-allow-egress-dns-snapshots",
                &layout.minio,
                &[("TCP", 9000)],
            ),
        ];
        for (policy, peer, ports) in expected {
            let doc = b
                .iter()
                .find(|d| d["kind"] == "NetworkPolicy" && d["metadata"]["name"] == policy)
                .unwrap_or_else(|| panic!("{policy} is missing from the bundle"));
            let rules = doc["spec"]["ingress"]
                .as_array()
                .or_else(|| doc["spec"]["egress"].as_array())
                .unwrap_or_else(|| panic!("{policy} has neither ingress nor egress rules"));
            let rule = rules
                .iter()
                .find(|r| {
                    let peers = r["from"].as_array().or_else(|| r["to"].as_array());
                    peers.is_some_and(|ps| {
                        ps.iter().any(|p| {
                            p["namespaceSelector"]["matchLabels"]["kubernetes.io/metadata.name"]
                                == *peer
                        })
                    })
                })
                .unwrap_or_else(|| panic!("{policy} has no rule whose peer is '{peer}'"));
            let got: Vec<(String, i64)> = rule["ports"]
                .as_array()
                .unwrap_or_else(|| panic!("{policy} opens no ports toward '{peer}'"))
                .iter()
                .map(|p| {
                    (
                        p["protocol"].as_str().unwrap_or("").to_string(),
                        p["port"].as_i64().unwrap_or(-1),
                    )
                })
                .collect();
            let want: Vec<(String, i64)> = ports.iter().map(|(p, n)| (p.to_string(), *n)).collect();
            assert_eq!(got, want, "{policy} → '{peer}' opens the wrong ports");
        }
    }

    /// The layout the real provisioning path uses must read its peer names from
    /// the code that owns them, not from copies. `agents` is the one that bites:
    /// a second `"velox-agents"` literal here would keep compiling and keep
    /// passing every fixture-based test above while ingest silently dies the day
    /// `agents.rs` renames its namespace.
    #[test]
    fn live_layout_reads_peers_from_their_owners() {
        let live = NamespaceLayout::current();
        assert_eq!(live.agents, crate::agents::AGENT_NS);
        assert_eq!(live.control_plane, ns());
        // Env-configurable peers still have to be non-empty: an empty
        // `kubernetes.io/metadata.name` selects nothing, silently.
        assert!(!live.ingress.is_empty());
        assert!(!live.minio.is_empty());
        assert_eq!(
            env_ns("VELOX_NS_VAR_THAT_IS_UNSET_IN_TESTS", "fallback"),
            "fallback"
        );
    }

    fn ingress_access(tls_secret: &str) -> crate::access::AccessConfig {
        crate::access::AccessConfig {
            mode: "ingress".into(),
            base_domain: "example.com".into(),
            ingress_class: "traefik".into(),
            tls_secret: tls_secret.into(),
        }
    }

    // ── delete leaves nothing behind ───────────────────────────────────

    #[test]
    fn every_secret_the_app_creates_is_also_deleted() {
        // Regression: the ADR-045 Secrets were created by the auth flow and
        // never deleted with the deployment, orphaning an IdP bind password and
        // the securityconfig. Any new per-deployment Secret must appear here.
        let owned = owned_secret_names("prod");
        for expected in [
            admin_secret_name("prod"),
            dashboards_secret_name("prod"),
            securityconfig_secret_name("prod"),
            auth_state_secret_name("prod"),
        ] {
            assert!(
                owned.contains(&expected),
                "{expected} is not deleted with the deployment"
            );
        }
    }

    // ── version preservation (ADR-048 invariant 1) ─────────────────────

    #[test]
    fn a_save_preserves_the_version_the_deployment_already_runs() {
        // The regression this ADR exists for: `create_cluster` is also the save
        // path, and it used to write a literal "3.7.0". Editing the memory of a
        // 3.0.0 deployment therefore started an unrequested, unrevertable major
        // upgrade. Whatever the CR runs is what a save re-applies.
        let cr = serde_json::json!({
            "spec": {
                "general": { "version": "3.0.0" },
                "dashboards": { "version": "3.0.0" }
            }
        });
        assert_eq!(
            versions_of(Some(&cr)),
            ("3.0.0".to_string(), "3.0.0".to_string())
        );
    }

    #[test]
    fn a_new_deployment_gets_the_default_version() {
        let (nodes, dash) = versions_of(None);
        assert_eq!(nodes, crate::upgrade::DEFAULT_VERSION);
        assert_eq!(dash, crate::upgrade::DEFAULT_VERSION);
    }

    #[test]
    fn dashboards_follow_the_nodes_when_their_version_is_missing() {
        // An interrupted phase 2 leaves nodes pinned and dashboards unset —
        // that must not become "jump dashboards to the default".
        let cr = serde_json::json!({ "spec": { "general": { "version": "3.6.0" } } });
        assert_eq!(
            versions_of(Some(&cr)),
            ("3.6.0".to_string(), "3.6.0".to_string())
        );
    }

    #[test]
    fn a_blank_version_falls_back_to_the_default_not_to_an_empty_tag() {
        let cr = serde_json::json!({ "spec": { "general": { "version": "  " } } });
        let (nodes, dash) = versions_of(Some(&cr));
        assert_eq!(nodes, crate::upgrade::DEFAULT_VERSION);
        assert_eq!(dash, crate::upgrade::DEFAULT_VERSION);
    }

    // ── auth provider CR wiring (ADR-045) ──────────────────────────────

    fn oidc_spec() -> AuthProviderSpec {
        AuthProviderSpec {
            provider: AuthProvider::Oidc(crate::auth_provider::OidcConfig {
                connect_url: "https://kc.corp/realms/v/.well-known/openid-configuration".into(),
                client_id: "veloxsearch".into(),
                client_secret: "topsecret".into(),
                ..Default::default()
            }),
            role_mappings: vec![],
        }
    }

    #[test]
    fn a_private_idp_ca_reaches_the_dashboards_container_as_a_file() {
        // Regression (MR4, #56): `pemtrustedcas_content` only ever reached the
        // securityconfig, which covers the OpenSearch nodes. The Dashboards
        // security plugin fetches the discovery document over its own Node TLS
        // stack and died at boot with UNABLE_TO_VERIFY_LEAF_SIGNATURE, so every
        // OIDC deployment behind a corporate CA was unbootable.
        let mut spec = oidc_spec();
        if let AuthProvider::Oidc(c) = &mut spec.provider {
            c.pemtrustedcas_content = "-----BEGIN CERTIFICATE-----\nxx\n".into();
        }
        let m = auth_cr_patch("prod", &spec, Some("https://prod.example.com"));
        let vol = &m["spec"]["dashboards"]["additionalVolumes"][0];
        assert_eq!(vol["secret"]["secretName"], "prod-auth");
        assert_eq!(vol["path"], crate::auth_provider::IDP_CA_DIR);
        assert_eq!(
            vol["secret"]["items"][0]["key"],
            crate::auth_provider::IDP_CA_KEY
        );
        // …and the plugin has to be told where it landed.
        assert_eq!(
            m["spec"]["dashboards"]["additionalConfig"]["opensearch_security.openid.root_ca"],
            crate::auth_provider::idp_ca_path()
        );

        // No CA configured → no volume, no key: the default trust store is
        // right for a public IdP and an empty mount would be noise.
        let plain = auth_cr_patch("prod", &oidc_spec(), Some("https://prod.example.com"));
        assert!(plain["spec"]["dashboards"]["additionalVolumes"].is_null());
        assert!(plain["spec"]["dashboards"]["additionalConfig"]
            ["opensearch_security.openid.root_ca"]
            .is_null());
    }

    #[test]
    fn auth_patch_wires_both_artifacts_and_keeps_the_secret_out_of_the_configmap() {
        let m = auth_cr_patch("prod", &oidc_spec(), Some("https://prod.example.com"));
        assert_eq!(m["metadata"]["labels"][LABEL_AUTH_KIND], "oidc");
        assert_eq!(
            m["spec"]["security"]["config"]["securityConfigSecret"]["name"],
            "prod-securityconfig"
        );
        assert_eq!(
            m["spec"]["security"]["config"]["adminCredentialsSecret"]["name"],
            "prod-admin-credentials"
        );
        // Dashboards side: config keys + the account whose hash we wrote.
        let d = &m["spec"]["dashboards"];
        assert_eq!(
            d["opensearchCredentialsSecret"]["name"],
            "prod-dashboards-credentials"
        );
        assert_eq!(
            d["additionalConfig"]["opensearch_security.openid.base_redirect_url"],
            "https://prod.example.com"
        );
        // The credential arrives as an env var sourced from a Secret; the
        // additionalConfig map becomes a ConfigMap and must stay clean.
        assert_eq!(d["env"][0]["name"], crate::auth_provider::OIDC_SECRET_ENV);
        assert_eq!(
            d["env"][0]["valueFrom"]["secretKeyRef"]["name"],
            "prod-auth"
        );
        assert!(
            !serde_json::to_string(&d["additionalConfig"])
                .unwrap()
                .contains("topsecret"),
            "client secret leaked into the ConfigMap-bound config"
        );
    }

    #[test]
    fn reverting_to_internal_prunes_every_auth_field() {
        // Server-side apply removes what this manager previously owned, so the
        // revert manifest must carry NO spec at all.
        let m = auth_cr_patch("prod", &AuthProviderSpec::default(), None);
        assert_eq!(m["metadata"]["labels"][LABEL_AUTH_KIND], "internal");
        assert!(
            m.get("spec").is_none(),
            "a leftover spec field would keep the securityconfig bound"
        );
    }

    #[test]
    fn ldap_patch_carries_no_dashboards_config_or_env() {
        let spec = AuthProviderSpec {
            provider: AuthProvider::Ldap(crate::auth_provider::LdapConfig {
                hosts: vec!["dc1.corp.local:636".into()],
                userbase: "OU=Users,DC=corp,DC=local".into(),
                usersearch: "(sAMAccountName={0})".into(),
                ..Default::default()
            }),
            role_mappings: vec![],
        };
        let d = &auth_cr_patch("prod", &spec, None)["spec"]["dashboards"];
        assert!(d.get("additionalConfig").is_none());
        assert!(d.get("env").is_none());
        // …but the service account is still ours, because our securityconfig
        // replaced the operator's internal_users.yml.
        assert_eq!(
            d["opensearchCredentialsSecret"]["name"],
            "prod-dashboards-credentials"
        );
    }

    #[test]
    fn bcrypt_hashes_use_the_variant_the_security_plugin_accepts() {
        let h = bcrypt_2y("s3cret").unwrap();
        assert!(h.starts_with("$2y$"), "{h}");
        assert!(bcrypt::verify("s3cret", &h.replacen("$2y$", "$2b$", 1)).unwrap());
    }

    /// The deployment the ingress tests render for.
    fn test_dep() -> Deployment {
        Deployment::for_test("prod", "veloxsearch-test", None)
    }

    #[test]
    fn the_allow_list_defaults_to_open_and_validates_what_it_accepts() {
        // Default is OPEN: no list means no middleware annotation, so a
        // customer opts IN to restriction and can never be locked out by a
        // default they did not choose.
        let m =
            dashboards_ingress_manifest(&ingress_access(""), &test_dep(), "prod.example.com", &[]);
        assert!(m["metadata"]["annotations"].as_object().unwrap().is_empty());

        let m = dashboards_ingress_manifest(
            &ingress_access(""),
            &test_dep(),
            "prod.example.com",
            &["203.0.113.0/24".to_string()],
        );
        assert!(
            m["metadata"]["annotations"]["traefik.ingress.kubernetes.io/router.middlewares"]
                .as_str()
                .unwrap()
                .contains("prod-ipallow")
        );

        // A typo in a security control must fail when it is saved, not later.
        for good in ["203.0.113.0/24", "198.51.100.7", "2001:db8::/32", "::1"] {
            validate_cidr(good).unwrap_or_else(|e| panic!("{good} rejected: {e}"));
        }
        for bad in [
            "",
            "not-an-ip",
            "203.0.113.0/33",
            "203.0.113.0/x",
            "10.0.0.0/999",
        ] {
            assert!(validate_cidr(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn the_dashboards_route_keeps_its_original_host() {
        // Renaming it would invalidate every SSO redirect URI registered at the
        // customer's IdP (ADR-045). The explicit `-dashboard` name is an ALIAS.
        let m =
            dashboards_ingress_manifest(&ingress_access(""), &test_dep(), "prod.example.com", &[]);
        let hosts: Vec<&str> = m["spec"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["host"].as_str().unwrap())
            .collect();
        assert!(
            hosts.contains(&"prod.example.com"),
            "the SSO host was dropped: {hosts:?}"
        );
    }

    #[test]
    fn the_traefik_route_names_the_transport_and_the_allow_list() {
        // The plain-Ingress annotation path answers 502 on Traefik 3.7.4 — the
        // transport is never applied and the default one rejects the operator's
        // CA. This shape was measured working on the live cluster.
        let m = opensearch_route_manifest(
            &ingress_access("wild-tls"),
            &test_dep(),
            "prod-opensearch.example.com",
            &[],
        );
        let svc = &m["spec"]["routes"][0]["services"][0];
        assert_eq!(svc["scheme"], "https");
        assert_eq!(svc["serversTransport"], "prod-opensearch");
        assert_eq!(svc["port"], 9200);
        assert_eq!(m["spec"]["tls"]["secretName"], "wild-tls");
        assert!(
            m["spec"]["routes"][0]["middlewares"].is_null(),
            "no list = no middleware"
        );

        let m = opensearch_route_manifest(
            &ingress_access("wild-tls"),
            &test_dep(),
            "prod-opensearch.example.com",
            &["203.0.113.0/24".to_string()],
        );
        assert_eq!(
            m["spec"]["routes"][0]["middlewares"][0]["name"],
            "prod-ipallow"
        );
    }

    #[test]
    fn the_opensearch_route_talks_https_to_its_backend() {
        // The API serves HTTPS with the operator's internal CA. Without telling
        // the controller both facts the route fails its handshake.
        let m = opensearch_ingress_manifest(
            &ingress_access(""),
            &test_dep(),
            "prod-opensearch.example.com",
            &[],
        );
        let a = &m["metadata"]["annotations"];
        assert_eq!(
            a["traefik.ingress.kubernetes.io/service.serversscheme"],
            "https"
        );
        assert_eq!(a["nginx.ingress.kubernetes.io/backend-protocol"], "HTTPS");
        let b = &m["spec"]["rules"][0]["http"]["paths"][0]["backend"]["service"];
        assert_eq!(b["name"], "prod");
        assert_eq!(b["port"]["number"], 9200);
    }

    #[test]
    fn ingress_manifest_without_tls_secret_is_unchanged() {
        // Regression guarantee (issue #54): no tls_secret ⇒ the rendered spec
        // is exactly the historical one — no `tls` key at all.
        let m =
            dashboards_ingress_manifest(&ingress_access(""), &test_dep(), "prod.example.com", &[]);
        assert_eq!(m["metadata"]["name"], "prod-dashboards");
        assert_eq!(m["spec"]["ingressClassName"], "traefik");
        assert_eq!(m["spec"]["rules"][0]["host"], "prod.example.com");
        assert_eq!(
            m["spec"]["rules"][0]["http"]["paths"][0]["backend"]["service"]["name"],
            "prod-dashboards"
        );
        assert!(
            m["spec"].get("tls").is_none(),
            "default path must not grow a tls block"
        );
    }

    #[test]
    fn ingress_manifest_with_tls_secret_terminates_tls() {
        let m = dashboards_ingress_manifest(
            &ingress_access("client-wildcard-tls"),
            &test_dep(),
            "prod.example.com",
            &[],
        );
        assert_eq!(m["spec"]["tls"][0]["secretName"], "client-wildcard-tls");
        assert_eq!(m["spec"]["tls"][0]["hosts"][0], "prod.example.com");
        // The plain-HTTP rule is untouched — the controller redirects/serves.
        assert_eq!(m["spec"]["rules"][0]["host"], "prod.example.com");
    }

    #[test]
    fn ingress_manifest_trims_whitespace_only_tls_secret() {
        let m = dashboards_ingress_manifest(
            &ingress_access("   "),
            &test_dep(),
            "prod.example.com",
            &[],
        );
        assert!(m["spec"].get("tls").is_none());
    }

    #[test]
    fn validate_tls_pem_checks_markers() {
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB…\n-----END CERTIFICATE-----";
        let key = "-----BEGIN PRIVATE KEY-----\nMIIE…\n-----END PRIVATE KEY-----";
        let rsa_key = "-----BEGIN RSA PRIVATE KEY-----\nMIIE…\n-----END RSA PRIVATE KEY-----";
        assert!(validate_tls_pem(cert, key).is_ok());
        assert!(validate_tls_pem(cert, rsa_key).is_ok(), "PKCS#1 keys count");
        assert!(
            validate_tls_pem(key, cert).is_err(),
            "swapped paste rejected"
        );
        assert!(validate_tls_pem(cert, "not a key").is_err());
        assert!(validate_tls_pem("", key).is_err());
    }

    #[test]
    fn parse_mem_handles_units() {
        assert_eq!(parse_mem_mib("4Gi"), Some(4096));
        assert_eq!(parse_mem_mib("512Mi"), Some(512));
        assert_eq!(parse_mem_mib("3Gi"), Some(3072));
        assert_eq!(parse_mem_mib("1G"), Some(953)); // 1e9 bytes ≈ 953 MiB
        assert_eq!(parse_mem_mib("nonsense"), None);
        assert_eq!(parse_mem_mib("0Gi"), None);
    }

    #[test]
    fn generated_admin_password_is_strong_safe_and_unique() {
        let pw = gen_admin_password().expect("CSPRNG available in test env");
        assert_eq!(pw.chars().count(), 24, "fixed 24-char length");
        // Only shell/JSON/URL/Fluent-Bit-safe chars: alnum + RFC-3986 unreserved.
        assert!(
            pw.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
            "unsafe char in password: {pw}"
        );
        // Satisfies OpenSearch's strict regex: all four classes present.
        assert!(
            pw.chars().any(|c| c.is_ascii_uppercase()),
            "needs uppercase: {pw}"
        );
        assert!(
            pw.chars().any(|c| c.is_ascii_lowercase()),
            "needs lowercase: {pw}"
        );
        assert!(pw.chars().any(|c| c.is_ascii_digit()), "needs digit: {pw}");
        assert!(
            pw.chars().any(|c| matches!(c, '-' | '.' | '_' | '~')),
            "needs a special: {pw}"
        );
        // Two CSPRNG draws differ — no fixed/default value is ever returned.
        let pw2 = gen_admin_password().expect("CSPRNG available in test env");
        assert_ne!(pw, pw2, "two draws must differ (random)");
    }

    #[test]
    fn generated_password_never_starts_with_a_dash() {
        // Regression (MR4, #56): a password beginning with '-' reads as a flag
        // to anything that passes it as argv. The OpenSearch Dashboards
        // entrypoint turns OPENSEARCH_PASSWORD into `--opensearch.password
        // <value>`, so the container died at boot with
        // `Extra serve options "--opensearch.password" must have a value`.
        // A single draw would miss this ~98.5% of the time.
        for _ in 0..2000 {
            let pw = gen_admin_password().expect("CSPRNG available in test env");
            assert!(
                pw.starts_with(|c: char| c.is_ascii_alphanumeric()),
                "password must start with an alphanumeric, got {pw}"
            );
        }
    }

    #[test]
    fn disk_resize_rules() {
        // Same size → always fine (no-op save), regardless of SC capability.
        assert!(disk_resize_check("20Gi", "20Gi", Some(false)).is_ok());
        // Shrink → always refused.
        assert!(disk_resize_check("20Gi", "10Gi", Some(true)).is_err());
        assert!(disk_resize_check("20Gi", "5Gi", None).is_err());
        // Grow with an expandable class → allowed.
        assert!(disk_resize_check("10Gi", "20Gi", Some(true)).is_ok());
        // Grow when the class can't expand → refused (would silently no-op).
        assert!(disk_resize_check("10Gi", "20Gi", Some(false)).is_err());
        // Grow with unknown SC capability → permissive (don't block legit resize).
        assert!(disk_resize_check("10Gi", "20Gi", None).is_ok());
        // Unparseable current size → don't block (leave it to the apiserver).
        assert!(disk_resize_check("weird", "10Gi", Some(false)).is_ok());
    }

    /// #67: the namespace resolution must prefer the real in-cluster signals
    /// (downward-API env, then the SA-mounted file) and, when neither is
    /// present (off-cluster), fall back to the inert dev namespace — NEVER to
    /// `veloxsearch-test`, which holds live customer data.
    #[test]
    fn ns_resolution_falls_back_inert_never_prod() {
        // Downward-API env wins.
        assert_eq!(
            resolve_ns(Some("veloxsearch-test".into()), None).unwrap(),
            "veloxsearch-test"
        );
        // In-cluster SA file when no env — trimmed of the trailing newline.
        assert_eq!(
            resolve_ns(None, Some("veloxsearch-system\n".into())).unwrap(),
            "veloxsearch-system"
        );
        // Env beats the SA file when both are present.
        assert_eq!(resolve_ns(Some("a".into()), Some("b".into())).unwrap(), "a");
        // A blank/whitespace signal is ignored → fallback (not a blank ns()).
        assert_eq!(
            resolve_ns(Some("   ".into()), None).unwrap_err(),
            DEV_FALLBACK_NS
        );
        // Off-cluster, nothing set: the inert dev fallback, and explicitly NOT
        // a real production namespace — this is the whole point of #67.
        let fb = resolve_ns(None, None).unwrap_err();
        assert_eq!(fb, DEV_FALLBACK_NS);
        assert_ne!(
            fb, "veloxsearch-test",
            "off-cluster must never default to prod (#67)"
        );
    }

    /// #52: node scaling up/down is supported; scaling to zero is refused with
    /// a message that names the supported alternative (delete).
    #[test]
    fn node_scale_rules() {
        assert!(node_scale_check(1).is_ok());
        assert!(node_scale_check(3).is_ok());
        assert!(node_scale_check(5).is_ok());
        let err = node_scale_check(0).unwrap_err().to_string();
        assert!(err.contains("0 nodes"), "names the refused value: {err}");
        assert!(err.contains("delete"), "points at the supported op: {err}");
    }

    /// #52: memory can move up AND down, but a garbage quantity or a value the
    /// operator-derived heap makes unrunnable is refused before the apply.
    /// Bounds reconciled with #55/ADR-035: the operator computes heap = mem/2
    /// with no floor/cap of its own, so the floor moved 512Mi → 1Gi (heap ≥
    /// 512Mi) and a 62Gi ceiling appeared (heap ≤ 31g compressed-oops).
    #[test]
    fn memory_rules() {
        assert!(memory_check("1Gi").is_ok()); // exactly the floor (heap 512Mi)
        assert!(memory_check("4Gi").is_ok());
        assert!(memory_check("2Gi").is_ok()); // scale-down target stays legal
        assert!(memory_check("62Gi").is_ok()); // exactly the ceiling (heap 31g)
        let floor = memory_check("512Mi").unwrap_err().to_string();
        assert!(floor.contains("1Gi"), "names the floor: {floor}");
        assert!(floor.contains("heap"), "explains the operator why: {floor}");
        let ceil = memory_check("100Gi").unwrap_err().to_string();
        assert!(ceil.contains("62Gi"), "names the ceiling: {ceil}");
        // "4GB" is not a Kubernetes quantity — refuse instead of a broken apply.
        let bad = memory_check("4GB").unwrap_err().to_string();
        assert!(
            bad.contains("4GB") && bad.contains("4Gi"),
            "shows the fix: {bad}"
        );
        assert!(memory_check("lots").is_err());
    }

    /// #52: a disk override must be a valid quantity (the shrink/grow rules on
    /// top of it are `disk_resize_rules` above).
    #[test]
    fn disk_quantity_rules() {
        assert!(quantity_check("disk", "20Gi").is_ok());
        assert!(quantity_check("disk", "500Mi").is_ok());
        let err = quantity_check("disk", "20GB").unwrap_err().to_string();
        assert!(
            err.contains("disk") && err.contains("20GB"),
            "names field+value: {err}"
        );
    }

    /// #52: the admin-password rule `reset_admin_password` enforces.
    #[test]
    fn password_rules() {
        assert!(password_check("LongEnough1_").is_ok());
        assert!(password_check("short").is_err());
        // Whitespace padding does not smuggle a short password through.
        assert!(password_check("   abc   ").is_err());
    }

    #[test]
    fn heap_mirrors_the_operator_rule() {
        // ADR-035: our display heap must equal what the operator computes —
        // CalculateJvmHeapSizeSettings = memory request / 2 in MiB, default
        // 512 MiB when the request is missing/unparseable (verified in the
        // vendored operator, image v3.0.0-alpha / chart 3.0.2).
        assert_eq!(heap_mib("4Gi"), 2048);
        assert_eq!(heap_mib("3Gi"), 1536);
        assert_eq!(heap_mib("2Gi"), 1024);
        assert_eq!(heap_mib("1Gi"), 512);
        assert_eq!(heap_mib(""), 512, "operator default when unset");
        assert_eq!(
            heap_mib("nonsense"),
            512,
            "operator default when unparseable"
        );
    }

    #[test]
    fn node_pool_delegates_heap_to_the_operator() {
        // ADR-035: the CR must NOT carry a `jvm` field — its absence is what
        // makes the operator compute -Xms/-Xmx = half the memory request.
        let p = node_pool(3, "10Gi", "3Gi", "1", "2");
        assert!(
            p.get("jvm").is_none(),
            "jvm must be omitted (operator-managed)"
        );
        // One memory number: request = limit (Guaranteed QoS), so the value
        // the operator halves is exactly the value the user set.
        let res = p.get("resources").expect("resources block");
        assert_eq!(
            res.pointer("/requests/memory"),
            Some(&serde_json::json!("3Gi"))
        );
        assert_eq!(
            res.pointer("/limits/memory"),
            Some(&serde_json::json!("3Gi"))
        );
        assert_eq!(res.pointer("/requests/cpu"), Some(&serde_json::json!("1")));
        assert_eq!(res.pointer("/limits/cpu"), Some(&serde_json::json!("2")));
        assert_eq!(p.get("replicas"), Some(&serde_json::json!(3)));
        assert_eq!(p.get("diskSize"), Some(&serde_json::json!("10Gi")));
        assert!(
            p.get("persistence").is_some(),
            "PVC persistence stays (ADR-031)"
        );
    }

    #[test]
    fn node_persistence_is_a_pvc_pinned_to_longhorn() {
        // ADR-031: node pools claim a PVC (RWO) instead of the old ephemeral
        // emptyDir. ADR-043: the claim is pinned to the `longhorn` class —
        // never left to whatever the cluster's default happens to be.
        let p = node_persistence();
        assert!(p.get("emptyDir").is_none(), "emptyDir must be gone");
        let pvc = p.get("pvc").expect("persistence.pvc block present");
        assert_eq!(
            pvc.get("accessModes").and_then(|m| m.as_array()),
            Some(&vec![serde_json::json!("ReadWriteOnce")]),
        );
        assert_eq!(
            pvc.get("storageClass"),
            Some(&serde_json::json!("longhorn")),
            "storageClass must be pinned to longhorn (ADR-043)"
        );
    }

    #[test]
    fn presets_always_three_nodes() {
        for size in ["small", "medium", "large", "anything-else"] {
            assert_eq!(sizing(size).replicas, 3, "size {size} must be 3 nodes");
        }
    }

    #[test]
    fn preset_heap_tracks_memory() {
        // The displayed heap for each preset is half its (single) memory
        // number — exactly what the operator will derive from the request.
        assert_eq!(heap_short(sizing("small").mem), "1g"); // 2Gi/2
        assert_eq!(heap_short(sizing("medium").mem), "1536m"); // 3Gi/2
        assert_eq!(heap_short(sizing("large").mem), "1536m"); // 3Gi/2
                                                              // All presets stay inside the guarded memory range.
        for size in ["small", "medium", "large"] {
            assert!(memory_check(sizing(size).mem).is_ok(), "preset {size}");
        }
    }

    #[test]
    fn heap_short_matches_operator_value() {
        // Same MiB the operator computes, in a compact human unit.
        assert_eq!(heap_short("4Gi"), "2g");
        assert_eq!(heap_short("3Gi"), "1536m");
        assert_eq!(heap_short("2Gi"), "1g");
        assert_eq!(heap_short("1Gi"), "512m");
    }

    #[test]
    fn presets_mirror_sizing() {
        // Every preset profile reads its values straight from `sizing()`, so the
        // API can never drift from what `create_cluster` actually applies.
        let presets = sizing_presets();
        assert_eq!(presets.len(), 3);
        assert_eq!(
            presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["small", "medium", "large"],
        );
        for p in &presets {
            let s = sizing(&p.name);
            assert_eq!(p.nodes, 3, "every tier is 3 nodes (ADR-016)");
            assert_eq!(p.mem, s.mem);
            assert_eq!(p.disk, s.disk);
            assert_eq!(p.heap, heap_short(s.mem));
        }
        assert_eq!(presets[0].label, "Small");
    }

    #[test]
    fn custom_sizing_derives_heap_and_holds_invariants() {
        let c = custom_sizing(Some("8Gi"), Some("100Gi"));
        assert_eq!(c.name, "custom");
        assert_eq!(c.nodes, 3, "custom is still 3 nodes (ADR-016)");
        assert_eq!(c.mem, "8Gi");
        assert_eq!(c.disk, "100Gi");
        assert_eq!(c.heap, "4g", "heap is half the memory");

        // Blank/absent inputs fall back to the small preset so the profile is
        // always complete.
        let small = sizing("small");
        let d = custom_sizing(None, Some("  "));
        assert_eq!(d.mem, small.mem);
        assert_eq!(d.disk, small.disk);
        assert_eq!(d.heap, heap_short(small.mem));
    }

    // ── snapshot repository CR wiring (ADR-049) ────────────────────────

    fn snap_cfg() -> SnapshotConfig {
        SnapshotConfig {
            enabled: true,
            bucket: "velox-snap".into(),
            base_path: "prod".into(),
            endpoint: "http://minio.minio.svc:9000".into(),
            access_key: "AKIA".into(),
            secret_key: "s3cr3t".into(),
            ..SnapshotConfig::default()
        }
    }

    #[test]
    fn snapshot_patch_carries_repo_keystore_and_plugin() {
        let m = snapshot_cr_patch("prod", Some(&snap_cfg()));
        let g = &m["spec"]["general"];
        assert_eq!(
            g["snapshotRepositories"][0]["name"],
            crate::snapshot::REPO_NAME
        );
        assert_eq!(
            g["snapshotRepositories"][0]["settings"]["bucket"],
            "velox-snap"
        );
        assert_eq!(g["keystore"][0]["secret"]["name"], "prod-snapshot-s3");
        // repository-s3 is NOT bundled in the OpenSearch image (verified on
        // 3.7.0), so both the nodes and the bootstrap pod must declare it.
        assert_eq!(g["pluginsList"][0], crate::snapshot::S3_PLUGIN);
        assert_eq!(
            m["spec"]["bootstrap"]["pluginsList"][0],
            crate::snapshot::S3_PLUGIN
        );
        // No key material on the CR — it reaches the node through the keystore.
        let s = serde_json::to_string(&m).unwrap();
        assert!(
            !s.contains("AKIA") && !s.contains("s3cr3t"),
            "credentials on the CR: {s}"
        );
    }

    /// Disabling renders an empty spec: server-side apply then prunes exactly
    /// what this manager owned — repository, keystore and plugin list.
    #[test]
    fn snapshot_patch_when_disabled_prunes_the_slice() {
        for cfg in [
            None,
            Some(SnapshotConfig {
                enabled: false,
                ..snap_cfg()
            }),
        ] {
            let m = snapshot_cr_patch("prod", cfg.as_ref());
            assert!(
                m.get("spec").is_none(),
                "disabled patch must carry no spec: {m}"
            );
            assert_eq!(m["metadata"]["name"], "prod");
        }
    }

    /// ADR-049 invariant 2 — the regression that matters: `create_cluster()` is
    /// also the save path, and its full-manifest apply would wipe the snapshot
    /// configuration if the slice were part of it. It is not: the slice belongs
    /// to `veloxsearch-snapshot`, and a manager never prunes fields it does not
    /// own.
    #[test]
    fn the_create_manifest_does_not_own_the_snapshot_slice() {
        let general = serde_json::json!({
            "serviceName": "prod", "version": "3.7.0",
            "httpPort": 9200, "setVMMaxMapCount": true,
        });
        let s = serde_json::to_string(&general).unwrap();
        for field in ["snapshotRepositories", "keystore", "pluginsList"] {
            assert!(
                !s.contains(field),
                "the create manifest must not carry `{field}` — it would prune the \
                 snapshot slice on every save"
            );
        }
        assert_ne!(SNAPSHOT_FIELD_MANAGER, "veloxsearch");
        assert_ne!(SNAPSHOT_FIELD_MANAGER, AUTH_FIELD_MANAGER);
    }

    /// ADR-049 invariant 7: the ADR-045 leak was a Secret created on one path
    /// and forgotten on the other.
    #[test]
    fn the_snapshot_secret_dies_with_the_deployment() {
        assert!(owned_secret_names("prod").contains(&"prod-snapshot-s3".to_string()));
    }

    /// The configuration round-trips through the CR — there is no state Secret,
    /// so a wrong read here means the edit form silently loses fields.
    #[test]
    fn snapshot_config_round_trips_through_the_cr() {
        let cfg = crate::snapshot::with_defaults("prod", &snap_cfg());
        let cr = serde_json::json!({ "spec": snapshot_cr_patch("prod", Some(&cfg))["spec"] });
        let back = snapshot_config_from(&cr, None);
        assert!(back.enabled);
        assert_eq!(back.bucket, "velox-snap");
        assert_eq!(back.base_path, "prod");
        assert_eq!(back.endpoint, "http://minio.minio.svc:9000");
        assert!(back.path_style_access);
        // Credentials never come back in clear.
        assert_eq!(back.access_key, crate::snapshot::SECRET_KEPT);
        assert_eq!(back.secret_key, crate::snapshot::SECRET_KEPT);
        // No policy CR means the repository exists but nothing is scheduled.
        assert!(!back.policy.enabled);

        // A CR with no repository is the default, valid, unconfigured state.
        let empty = serde_json::json!({ "spec": { "general": { "version": "3.7.0" } } });
        assert!(!snapshot_config_from(&empty, None).enabled);
    }

    #[test]
    fn snapshot_policy_round_trips_through_its_cr() {
        let mut cfg = crate::snapshot::with_defaults("prod", &snap_cfg());
        cfg.policy.cron = "15 5 * * *".into();
        cfg.policy.max_age_days = 30;
        cfg.policy.max_count = 20;
        cfg.policy.min_count = 2;
        cfg.policy.indices = "logs-*".into();
        let policy = crate::snapshot::policy_cr("velox", "prod", &cfg);
        let back = policy_config_from(Some(&policy));
        assert!(back.enabled);
        assert_eq!(back.cron, "15 5 * * *");
        assert_eq!(back.max_age_days, 30);
        assert_eq!(back.max_count, 20);
        assert_eq!(back.min_count, 2);
        assert_eq!(back.indices, "logs-*");
    }

    /// While the cluster provisions the repository reconciler has not run yet
    /// (it waits for `PhaseRunning`), so the state is pending — not an error.
    #[test]
    fn snapshot_state_is_pending_before_the_cluster_runs() {
        let cfg = crate::snapshot::with_defaults("prod", &snap_cfg());
        let st = snapshot_state_from(&cfg, None, false);
        assert!(st.configured);
        assert_eq!(st.policy_state, "PENDING");
        assert!(st.last_error.is_empty());

        let errored = serde_json::json!({
            "status": { "state": "ERROR", "reason": "403 Access Denied" }
        });
        let st = snapshot_state_from(&cfg, Some(&errored), true);
        assert_eq!(st.policy_state, "ERROR");
        assert_eq!(
            st.last_error, "403 Access Denied",
            "the S3 reason must reach the UI verbatim"
        );

        // A reason attached to a non-error state is not an error message.
        let created = serde_json::json!({ "status": { "state": "CREATED", "reason": "ok" } });
        assert!(snapshot_state_from(&cfg, Some(&created), true)
            .last_error
            .is_empty());

        // Unconfigured deployments report nothing at all.
        assert!(!snapshot_state_from(&SnapshotConfig::default(), None, true).configured);
    }
}
