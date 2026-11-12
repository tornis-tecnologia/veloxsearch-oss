// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Does this installation have a Velox Mesh, and can the console reach it?
//!
//! Velox Mesh is a separate binary, image and release (ADR-068): the core does
//! not embed it, start it or depend on its crates. What the core needs is one
//! fact — whether an operator has installed one and pointed us at it — so the
//! console can either open the fleet screens or explain how to get there.
//! That fact is `VELOX_MESH_URL`, the same env-var convention as
//! `VELOX_REGISTRY_URL`, and unset means **not installed**: a feature that
//! defaults to off and preserves prior behaviour, like every other flag here.
//!
//! The probe is deliberately shallow. `GET /healthz` with a short timeout
//! separates the three states an operator actually has to tell apart:
//!
//! | Configured | Reachable | What the console shows |
//! | --- | --- | --- |
//! | no | — | how to install Velox Mesh |
//! | yes | no | the URL and the error — a deploy that did not come up, or a NetworkPolicy |
//! | yes | yes | the fleet |
//!
//! Collapsing the middle row into "not installed" is the failure mode worth
//! avoiding: an operator who *did* install it would be told to install it
//! again.

use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// How long the console waits for the Mesh before calling it unreachable.
/// Short on purpose: this answer gates a screen's first paint.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// What the console needs to decide which Velox Mesh screen to render.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct MeshStatus {
    /// A Mesh is known to this control plane. False ⇒ the install guide.
    pub configured: bool,
    /// How: `env` (VELOX_MESH_URL + token) or `installed` (found in the
    /// `velox-mesh` namespace, ADR-072 amendment 5). `None` when not
    /// configured. The console offers "reapply" only for `installed`.
    pub source: Option<&'static str>,
    /// The configured base URL, so the screen can name what it tried.
    pub url: Option<String>,
    /// `None` when not configured; otherwise the result of `GET /healthz`.
    pub reachable: Option<bool>,
    /// Why the probe failed, for the operator — not a stack trace.
    pub error: Option<String>,
}

/// Where the console installs the Mesh, and where the core looks for one
/// (ADR-072 amendment 5): namespace, and the one name its Service, Secret
/// and Deployment share. Fixed, like `velox-agents`: the core's Role is
/// granted in this namespace and nowhere else.
pub const MESH_NAMESPACE: &str = "velox-mesh";
pub const MESH_NAME: &str = "velox-mesh";
pub const MESH_PORT: u16 = 4320;

/// The Mesh found in [`MESH_NAMESPACE`]: base URL and service token. Read on
/// start, after an install, and by a status read that finds none (so a Mesh
/// applied by hand appears without restarting the core).
static DISCOVERED: std::sync::RwLock<Option<(String, String)>> = std::sync::RwLock::new(None);

fn env_url() -> Option<String> {
    std::env::var("VELOX_MESH_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

/// The in-cluster URL of the Mesh the console installs.
pub fn installed_url() -> String {
    format!("http://{MESH_NAME}.{MESH_NAMESPACE}.svc:{MESH_PORT}")
}

/// The configured Mesh base URL, trimmed of a trailing slash so joining a
/// path never produces `//`: `VELOX_MESH_URL` when set (a Mesh that lives
/// elsewhere), else the one found installed.
pub fn configured_url() -> Option<String> {
    env_url().or_else(|| {
        DISCOVERED
            .read()
            .ok()
            .and_then(|d| d.as_ref().map(|(u, _)| u.clone()))
    })
}

/// Look for the installed Mesh — its Service and the token in its Secret —
/// and remember it. Both or nothing: a Service without the token is a Mesh
/// the core cannot authenticate to, so it is not "configured". An API error
/// keeps what was known (an outage must not make the fleet disappear).
pub async fn discover() -> bool {
    match crate::k8s::installed_mesh_token().await {
        Ok(found) => {
            let v = found.map(|token| (installed_url(), token));
            let hit = v.is_some();
            if let Ok(mut d) = DISCOVERED.write() {
                *d = v;
            }
            hit
        }
        Err(e) => {
            tracing::debug!("velox mesh discovery: {e:#}");
            configured_url().is_some()
        }
    }
}

/// Read the status, probing the Mesh when one is configured. With none
/// known, look once — a Mesh applied by hand since the start is found here.
pub async fn status() -> MeshStatus {
    if configured_url().is_none() {
        discover().await;
    }
    let Some(url) = configured_url() else {
        return MeshStatus::default();
    };
    let source = Some(if env_url().is_some() {
        "env"
    } else {
        "installed"
    });
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return MeshStatus {
                configured: true,
                source,
                url: Some(url),
                reachable: Some(false),
                error: Some(e.to_string()),
            }
        }
    };
    match client.get(format!("{url}/healthz")).send().await {
        Ok(r) if r.status().is_success() => MeshStatus {
            configured: true,
            source,
            url: Some(url),
            reachable: Some(true),
            error: None,
        },
        Ok(r) => MeshStatus {
            configured: true,
            source,
            url: Some(url),
            reachable: Some(false),
            error: Some(format!("the Mesh answered {} at /healthz", r.status())),
        },
        Err(e) => MeshStatus {
            configured: true,
            source,
            url: Some(url),
            reachable: Some(false),
            // `e.to_string()` on reqwest carries the URL already; the operator
            // needs the reason, not our formatting of it.
            error: Some(e.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// Talking to the Mesh: compile here, store there
// ---------------------------------------------------------------------------

/// The console's side of a policy. The operator picks integrations and a
/// destination; everything an agent needs is derived from that, here, because
/// this is the only process that holds **verified** packages (ADR-039: the
/// signing keyring is the whole security boundary, and one implementation of
/// it is one attack surface). The Mesh stores and serves what it is handed
/// and never calls the registry.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PolicyDraft {
    pub id: String,
    pub name: String,
    /// The OpenSearch deployment the data lands in.
    pub deployment: String,
    /// Integration ids, in the order the operator added them.
    pub integrations: Vec<String>,
    /// Per-integration variables (schema 1.1), keyed by integration id. An
    /// older console's field; `inputs` is what the onboarding sends.
    #[serde(default)]
    pub vars: BTreeMap<String, BTreeMap<String, String>>,
    /// Per-integration input selection (ADR-073), keyed by integration id.
    /// An integration listed without one gets the package's defaults.
    #[serde(default)]
    pub inputs: BTreeMap<String, velox_integrations::InputSelection>,
    /// The deployment's configured endpoints this policy is bound to (ADR-072
    /// amendment 2), by id. Both are required to save: `save_policy` refuses
    /// a draft that names none or names one the settings no longer hold.
    #[serde(default)]
    pub endpoints: velox_policy::PolicyEndpoints,
    /// Spec 2.0 activations (ADR-074), in the order the operator added them.
    /// A draft carries these or `integrations`, never both. An empty
    /// `package_version` means "the newest version this core can run".
    #[serde(default)]
    pub packages: Vec<velox_integrations::spec2::Activation>,
    /// Values for the `password` fields of `packages`, by secret id — only
    /// those typed in this edit. A referenced id absent here keeps the value
    /// the Mesh already holds; one the Mesh does not hold is refused.
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
}

impl PolicyDraft {
    /// The draft a stored policy document edits as: its integrations in
    /// order, each with the selection its activation carries, bound to the
    /// endpoints the document names.
    fn from_policy(deployment: &str, p: &velox_policy::Policy) -> PolicyDraft {
        PolicyDraft {
            id: p.id.clone(),
            name: p.name.clone(),
            deployment: deployment.to_string(),
            integrations: p.activations.iter().map(|a| a.package_id.clone()).collect(),
            vars: BTreeMap::new(),
            inputs: p
                .activations
                .iter()
                .map(|a| (a.package_id.clone(), a.selection()))
                .collect(),
            endpoints: p.endpoints.clone(),
            packages: p.packages.clone(),
            secrets: BTreeMap::new(),
        }
    }
}

/// Resolve the draft's endpoint ids against the deployment's settings: the
/// OpenSearch host the collectors ship to, and the Mesh URL the policy's
/// agents enrol against. A policy is refused — not compiled against a
/// fallback — when either is missing or names an endpoint the operator has
/// since removed. The in-cluster service name used to be the silent default
/// here; it no longer is, because the symptom of a wrong default is a healthy
/// agent that delivers nothing, found on the host hours later. The operator
/// names the address once, in Settings, and every policy says which one.
fn resolve_endpoints<'s>(
    settings: &'s MeshSettings,
    e: &velox_policy::PolicyEndpoints,
) -> anyhow::Result<(&'s OsEndpoint, &'s MeshEndpoint)> {
    let os_id = e
        .opensearch
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("the policy names no OpenSearch endpoint; pick one of the deployment's configured endpoints"))?;
    let mesh_id = e
        .mesh
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("the policy names no Velox Mesh endpoint; pick one of the deployment's configured endpoints"))?;
    let os = settings
        .os_endpoints
        .iter()
        .find(|x| x.id == os_id)
        .ok_or_else(|| {
            anyhow::anyhow!("OpenSearch endpoint `{os_id}` is not configured on this deployment")
        })?;
    let mesh = settings
        .mesh_endpoints
        .iter()
        .find(|x| x.id == mesh_id)
        .ok_or_else(|| {
            anyhow::anyhow!("Velox Mesh endpoint `{mesh_id}` is not configured on this deployment")
        })?;
    Ok((os, mesh))
}

/// The current document of one of the deployment's policies.
pub async fn policy_doc(
    dep: &crate::scope::Deployment,
    policy_id: &str,
) -> anyhow::Result<velox_policy::Policy> {
    use anyhow::Context as _;
    let out: serde_json::Value =
        call_for(dep, "policy", serde_json::json!({ "policy_id": policy_id })).await?;
    let body = out
        .get("body")
        .cloned()
        .context("the Mesh answered with no policy body")?;
    serde_json::from_value(body).context("the stored policy does not parse")
}

/// Create an empty policy (ADR-073): a name, bound to one OpenSearch and one
/// Mesh endpoint of the deployment (ADR-072 amendment 2), that the operator
/// then adds integrations to. Refused when it exists — a second create is not
/// an edit — and refused without both endpoints, by `save_policy`.
pub async fn create_policy(
    dep: &crate::scope::Deployment,
    id: &str,
    name: &str,
    endpoints: velox_policy::PolicyEndpoints,
    tenant_id: Option<&str>,
) -> anyhow::Result<u64> {
    if policy_doc(dep, id).await.is_ok() {
        anyhow::bail!("policy {id} already exists on {}", dep.name());
    }
    let draft = PolicyDraft {
        id: id.to_string(),
        name: name.to_string(),
        deployment: dep.name().to_string(),
        integrations: vec![],
        vars: BTreeMap::new(),
        inputs: BTreeMap::new(),
        endpoints,
        packages: vec![],
        secrets: BTreeMap::new(),
    };
    save_policy(&draft, dep, tenant_id).await
}

/// Rebind one of the deployment's policies to other endpoints: the stored
/// document with the new ids, recompiled and saved as the next revision —
/// the OpenSearch host is baked into every collector stanza, so a change of
/// endpoint is a change of configuration the agents must pick up, never a
/// pointer edit on the current revision.
pub async fn set_endpoints(
    dep: &crate::scope::Deployment,
    policy_id: &str,
    endpoints: velox_policy::PolicyEndpoints,
    tenant_id: Option<&str>,
) -> anyhow::Result<u64> {
    let doc = policy_doc(dep, policy_id).await?;
    let mut draft = PolicyDraft::from_policy(dep.name(), &doc);
    draft.endpoints = endpoints;
    save_policy(&draft, dep, tenant_id).await
}

/// Add or reconfigure one integration on one of the deployment's policies
/// (ADR-073): the stored document with this activation set, saved as the next
/// revision. The backend half is installed by `save_policy` as always.
pub async fn set_activation(
    dep: &crate::scope::Deployment,
    policy_id: &str,
    package_id: &str,
    selection: &velox_integrations::InputSelection,
    tenant_id: Option<&str>,
) -> anyhow::Result<u64> {
    let doc = policy_doc(dep, policy_id).await?;
    let mut draft = PolicyDraft::from_policy(dep.name(), &doc);
    if !draft.integrations.iter().any(|i| i == package_id) {
        draft.integrations.push(package_id.to_string());
    }
    draft
        .inputs
        .insert(package_id.to_string(), selection.clone());
    save_policy(&draft, dep, tenant_id).await
}

/// Take one integration off one of the deployment's policies: the next
/// revision without it. Agents stop collecting it on their next check-in.
/// Whether the backend half goes too is the caller's call
/// (`catalog::backend_still_used`).
pub async fn unset_activation(
    dep: &crate::scope::Deployment,
    policy_id: &str,
    package_id: &str,
    tenant_id: Option<&str>,
) -> anyhow::Result<u64> {
    let doc = policy_doc(dep, policy_id).await?;
    let mut draft = PolicyDraft::from_policy(dep.name(), &doc);
    if !draft.integrations.iter().any(|i| i == package_id) {
        anyhow::bail!("policy {policy_id} does not activate {package_id}");
    }
    draft.integrations.retain(|i| i != package_id);
    draft.inputs.remove(package_id);
    save_policy(&draft, dep, tenant_id).await
}

/// One address at which hosts reach this deployment's OpenSearch: a bare
/// `host[:port]`, because that is what the collector's output stanza takes
/// (`Host`, with the port fixed by the template). Named, because one
/// deployment is reached by different addresses from different networks —
/// the ClusterIP from a VM on the node network, a load balancer from
/// outside — and a policy says which of them its hosts use.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OsEndpoint {
    /// Stable, slug-shaped; what a policy references.
    pub id: String,
    pub name: String,
    pub host: String,
    /// Derived by the core from the access configuration, never stored
    /// (ADR-072 amendment 4): the deployment's public route. Read-only on
    /// the settings screen; a save drops it before writing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub managed: bool,
    /// The address answers with a certificate hosts cannot verify (an
    /// ingress controller's self-signed default): the compiler writes
    /// `tls.verify Off` on the output bound to it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub insecure: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One address at which hosts reach the Mesh: the `--mesh` of `velox-agent
/// enroll`, an http(s) base URL. The core itself talks to the controller over
/// `VELOX_MESH_URL`, an in-cluster name a VM cannot resolve; these are the
/// addresses the operator published, and they are the deployment's because
/// that is where the enrolment commands are shown.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MeshEndpoint {
    pub id: String,
    pub name: String,
    pub url: String,
    /// As on [`OsEndpoint`]: derived from the access configuration.
    #[serde(default, skip_serializing_if = "is_false")]
    pub managed: bool,
    /// The agent must be told `--insecure` to enrol against it; the console
    /// writes the flag into the enrol command and the probe skips
    /// verification for it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub insecure: bool,
}

/// The deployment's Mesh settings (ADR-072 §5, amendment 2): the endpoints
/// its policies bind to, kept on its CR like every other intent the core
/// must re-derive after a restart. Lists, not one value each: a policy
/// *chooses* an OpenSearch endpoint and a Mesh endpoint from here, and both
/// are required — the in-cluster service name is no longer an implicit
/// default, because a host that cannot resolve it fails silently.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MeshSettings {
    #[serde(default)]
    pub os_endpoints: Vec<OsEndpoint>,
    #[serde(default)]
    pub mesh_endpoints: Vec<MeshEndpoint>,
}

impl MeshSettings {
    /// The settings a deployment configured before the lists existed — one
    /// `os_host`, one `mesh_url` on their own annotations — as one endpoint
    /// each, under a fixed id. Read-side only: the next save writes the list
    /// form and clears the old annotations.
    pub fn from_legacy(os_host: Option<String>, mesh_url: Option<String>) -> MeshSettings {
        MeshSettings {
            os_endpoints: os_host
                .filter(|h| !h.trim().is_empty())
                .map(|h| OsEndpoint {
                    id: LEGACY_ENDPOINT_ID.into(),
                    name: "Published".into(),
                    host: h.trim().to_string(),
                    ..Default::default()
                })
                .into_iter()
                .collect(),
            mesh_endpoints: mesh_url
                .filter(|u| !u.trim().is_empty())
                .map(|u| MeshEndpoint {
                    id: LEGACY_ENDPOINT_ID.into(),
                    name: "Published".into(),
                    url: u.trim().trim_end_matches('/').to_string(),
                    ..Default::default()
                })
                .into_iter()
                .collect(),
        }
    }
}

/// The id the pre-list `os_host`/`mesh_url` values are read under.
pub const LEGACY_ENDPOINT_ID: &str = "published";

/// The id of the endpoints derived from the deployment's public routes
/// (ADR-072 amendment 4). Reserved: an operator's endpoint cannot take it.
pub const ROUTE_ENDPOINT_ID: &str = "ingress";

/// The in-cluster Service a `VELOX_MESH_URL` names — `http://<svc>.<ns>.svc
/// [.<cluster domain>][:port]` — as `(service, namespace, port)`: what an
/// Ingress backend for the Mesh's agent face needs. `None` for any other
/// shape (a loopback dev Mesh, an external URL, an https backend), and then
/// no route is made: an Ingress can only point at a Service in its own
/// namespace over plain HTTP, which is what the Mesh serves in-cluster.
pub fn in_cluster_service(url: &str) -> Option<(String, String, u16)> {
    let rest = url.trim().strip_prefix("http://")?;
    let authority = rest.split('/').next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok().filter(|p| *p != 0)?),
        None => (authority, 80),
    };
    let mut parts = host.split('.');
    let svc = parts.next()?;
    let ns = parts.next()?;
    if parts.next()? != "svc" || svc.is_empty() || ns.is_empty() {
        return None;
    }
    Some((svc.to_string(), ns.to_string(), port))
}

/// The settings as the deployment's policies and screens see them: what the
/// operator stored, plus — in ingress mode — the deployment's public routes
/// as one endpoint of each kind, first, under [`ROUTE_ENDPOINT_ID`]
/// (ADR-072 amendment 4). The OpenSearch one is `https://<name>-opensearch.
/// <domain>:443`; the Mesh one `https://<name>-mesh.<domain>:443`, only when
/// `mesh_routed` (the core can route to the Mesh at all). Derived on every
/// read, never stored, so a changed base domain moves every policy bound to
/// them at its next save, and leaving ingress mode takes them away (a policy
/// still bound to one is then refused at compile, by name).
pub fn with_route_endpoints(
    mut stored: MeshSettings,
    access: &crate::access::AccessConfig,
    name: &str,
    mesh_routed: bool,
) -> MeshSettings {
    stored.os_endpoints.retain(|e| e.id != ROUTE_ENDPOINT_ID);
    stored.mesh_endpoints.retain(|e| e.id != ROUTE_ENDPOINT_ID);
    let insecure = access.routes_self_signed();
    if let Some(host) = access.opensearch_endpoint(name) {
        stored.os_endpoints.insert(
            0,
            OsEndpoint {
                id: ROUTE_ENDPOINT_ID.into(),
                name: "Public URL".into(),
                host,
                managed: true,
                insecure,
            },
        );
    }
    if let Some(url) = access.mesh_endpoint(name).filter(|_| mesh_routed) {
        stored.mesh_endpoints.insert(
            0,
            MeshEndpoint {
                id: ROUTE_ENDPOINT_ID.into(),
                name: "Public URL".into(),
                url,
                managed: true,
                insecure,
            },
        );
    }
    stored
}

/// What a save writes: the operator's endpoints alone.
pub fn without_route_endpoints(mut s: MeshSettings) -> MeshSettings {
    s.os_endpoints.retain(|e| !e.managed);
    s.mesh_endpoints.retain(|e| !e.managed);
    s
}

/// An endpoint id is a slug: it travels in JSON on a CR annotation and in a
/// policy document, and is matched by equality. Same shape as a component id.
fn valid_endpoint_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// What a setting may hold, before it lands verbatim somewhere an operator
/// would only meet the mistake later: a host in a collector's output stanza
/// ("healthy agent, no data"), a URL in a shell command on the screen. Ids
/// must be slugs and unique within their list; names must not be blank.
pub fn validate_settings(s: &MeshSettings) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for e in &s.os_endpoints {
        if !valid_endpoint_id(&e.id) {
            return Err(format!("OpenSearch endpoint id {:?} is not a slug", e.id));
        }
        if !seen.insert(e.id.as_str()) {
            return Err(format!("OpenSearch endpoint id {:?} is repeated", e.id));
        }
        if e.id == ROUTE_ENDPOINT_ID && !e.managed {
            return Err(format!(
                "endpoint id {ROUTE_ENDPOINT_ID:?} is reserved for the deployment's public URL"
            ));
        }
        if e.name.trim().is_empty() {
            return Err(format!("OpenSearch endpoint {} needs a name", e.id));
        }
        // The three spellings Fluent Bit's output can be bound to —
        // `host`, `host:port`, an http(s) URL with an optional path — parsed
        // by the same function the compiler binds with (velox_policy::
        // os_address), so a saved endpoint is one the policy can compile.
        velox_policy::OsAddress::parse(&e.host)
            .map_err(|reason| format!("OpenSearch endpoint {}: {reason}", e.id))?;
    }
    seen.clear();
    for e in &s.mesh_endpoints {
        if !valid_endpoint_id(&e.id) {
            return Err(format!("Velox Mesh endpoint id {:?} is not a slug", e.id));
        }
        if !seen.insert(e.id.as_str()) {
            return Err(format!("Velox Mesh endpoint id {:?} is repeated", e.id));
        }
        if e.id == ROUTE_ENDPOINT_ID && !e.managed {
            return Err(format!(
                "endpoint id {ROUTE_ENDPOINT_ID:?} is reserved for the deployment's public URL"
            ));
        }
        if e.name.trim().is_empty() {
            return Err(format!("Velox Mesh endpoint {} needs a name", e.id));
        }
        let u = e.url.trim();
        let scheme_ok = u.starts_with("http://") || u.starts_with("https://");
        let host = u.split("://").nth(1).unwrap_or("");
        if !scheme_ok || host.is_empty() || u.chars().any(char::is_whitespace) {
            return Err(format!(
                "Velox Mesh endpoint {}: the address is a base URL such as https://mesh.example.com:4320",
                e.id
            ));
        }
    }
    Ok(())
}

/// The endpoints a new settings document would take away from policies that
/// still name them: `(policy id, endpoint id)` for each. The caller refuses
/// the save and names them — a policy pointing at a removed endpoint would
/// fail at its next compile with a message about settings, far from the
/// screen that caused it. Changing an endpoint's *address* in place is fine
/// and intended: it takes effect on the policy's next save.
pub fn removed_endpoints_in_use(
    current: &MeshSettings,
    next: &MeshSettings,
    policies: &[(String, Option<String>, Option<String>)],
) -> Vec<(String, String)> {
    let gone_os: Vec<&str> = current
        .os_endpoints
        .iter()
        .filter(|e| !next.os_endpoints.iter().any(|n| n.id == e.id))
        .map(|e| e.id.as_str())
        .collect();
    let gone_mesh: Vec<&str> = current
        .mesh_endpoints
        .iter()
        .filter(|e| !next.mesh_endpoints.iter().any(|n| n.id == e.id))
        .map(|e| e.id.as_str())
        .collect();
    let mut out = Vec::new();
    for (pid, os, mesh) in policies {
        if let Some(o) = os.as_deref().filter(|o| gone_os.contains(o)) {
            out.push((pid.clone(), o.to_string()));
        }
        if let Some(m) = mesh.as_deref().filter(|m| gone_mesh.contains(m)) {
            out.push((pid.clone(), m.to_string()));
        }
    }
    out
}

/// What the console shows per configured Mesh endpoint on the settings
/// screen: can the control plane reach it? A coarse answer on purpose. The
/// URL is operator data, so the probe is a `GET /healthz` with a short
/// timeout and the response body is never returned — a tenant must not get
/// a fetch-anything oracle out of a settings screen — and the error is the
/// category, not reqwest's text.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EndpointProbe {
    pub id: String,
    pub reachable: bool,
    /// `None` when reachable; otherwise one of `timeout`, `refused`,
    /// `status <code>`.
    pub error: Option<String>,
    pub latency_ms: u64,
}

/// Probe every configured Mesh endpoint, in parallel. The controller the
/// core itself uses is `status()`; this is about the addresses hosts use.
pub async fn probe_endpoints(s: &MeshSettings) -> Vec<EndpointProbe> {
    // Two clients: an endpoint marked `insecure` is probed the way its agents
    // will talk to it (`velox-agent enroll --insecure`), every other one with
    // verification — so "reachable" means what it will mean on the host.
    let build = |insecure: bool| {
        reqwest::Client::builder()
            .timeout(PROBE_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(insecure)
            .build()
    };
    let clients = build(false).and_then(|v| build(true).map(|i| (v, i)));
    let Ok((verifying, lenient)) = clients else {
        return s
            .mesh_endpoints
            .iter()
            .map(|e| EndpointProbe {
                id: e.id.clone(),
                reachable: false,
                error: Some("client".into()),
                latency_ms: 0,
            })
            .collect();
    };
    let probes = s.mesh_endpoints.iter().map(|e| {
        let client = if e.insecure { &lenient } else { &verifying }.clone();
        let id = e.id.clone();
        let url = format!("{}/healthz", e.url.trim_end_matches('/'));
        async move {
            let started = std::time::Instant::now();
            let res = client.get(url).send().await;
            let latency_ms = started.elapsed().as_millis() as u64;
            match res {
                Ok(r) if r.status().is_success() => EndpointProbe {
                    id,
                    reachable: true,
                    error: None,
                    latency_ms,
                },
                Ok(r) => EndpointProbe {
                    id,
                    reachable: false,
                    error: Some(format!("status {}", r.status().as_u16())),
                    latency_ms,
                },
                Err(e) => EndpointProbe {
                    id,
                    reachable: false,
                    error: Some(if e.is_timeout() { "timeout" } else { "refused" }.into()),
                    latency_ms,
                },
            }
        }
    });
    futures::future::join_all(probes).await
}

/// The body every Mesh call starts from: the deployment pair the Mesh
/// partitions on. Built from a `Deployment` — the capability the caller's
/// scope minted — and from nothing else, which is what makes "the Mesh only
/// answers about deployments the caller owns" true by construction.
fn dep_body(dep: &crate::scope::Deployment) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("deployment_ns".into(), dep.namespace().into());
    m.insert("deployment".into(), dep.name().into());
    m
}

/// The request body for a call about `dep`: the pair, then `extra`'s fields —
/// except the pair itself, which is the scope's to set, never the request's.
/// A console body that carried `deployment_ns` would otherwise reach the Mesh
/// as a different deployment's, with the scope check already behind it.
fn body_for(dep: &crate::scope::Deployment, extra: serde_json::Value) -> serde_json::Value {
    let mut body = dep_body(dep);
    if let serde_json::Value::Object(m) = extra {
        for (k, v) in m {
            if k != "deployment_ns" && k != "deployment" {
                body.insert(k, v);
            }
        }
    }
    serde_json::Value::Object(body)
}

/// POST `path` about this deployment, with `extra` fields merged into the
/// body. Every console-face call goes through here.
pub async fn call_for<T: serde::de::DeserializeOwned>(
    dep: &crate::scope::Deployment,
    path: &str,
    extra: serde_json::Value,
) -> anyhow::Result<T> {
    call_mesh(path, &body_for(dep, extra)).await
}

/// Which of the deployment's policies activate this integration. Empty when
/// no Mesh is configured: without a fleet nothing writes through the agent
/// path, so there is nothing to protect. A configured Mesh that cannot be
/// reached is an error, not an empty answer — the caller is about to delete
/// what hosts may be writing through, and must not do it blind.
pub async fn policies_using(
    dep: &crate::scope::Deployment,
    integration: &str,
) -> anyhow::Result<Vec<String>> {
    if configured_url().is_none() {
        return Ok(vec![]);
    }
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        #[serde(default)]
        integrations: Vec<String>,
    }
    let rows: Vec<Row> = call_for(dep, "policies", serde_json::json!({})).await?;
    Ok(rows
        .into_iter()
        .filter(|r| r.integrations.iter().any(|i| i == integration))
        .map(|r| r.id)
        .collect())
}

/// Integration id → the deployment's policies that activate it (1.x and
/// Spec 2.0 alike — the Mesh lists both). Empty without a Mesh.
pub async fn policies_by_integration(
    dep: &crate::scope::Deployment,
) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    if configured_url().is_none() {
        return Ok(BTreeMap::new());
    }
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        #[serde(default)]
        integrations: Vec<String>,
    }
    let rows: Vec<Row> = call_for(dep, "policies", serde_json::json!({})).await?;
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in rows {
        for i in r.integrations {
            out.entry(i).or_default().push(r.id.clone());
        }
    }
    Ok(out)
}

/// `(policy id, OpenSearch endpoint id, Mesh endpoint id)` for each of the
/// deployment's policies — what a settings save checks before removing an
/// endpoint. Empty when no Mesh is configured (no policy can exist); a
/// configured Mesh that does not answer is an error, for the same reason as
/// `policies_using`.
pub async fn policy_endpoint_rows(
    dep: &crate::scope::Deployment,
) -> anyhow::Result<Vec<(String, Option<String>, Option<String>)>> {
    if configured_url().is_none() {
        return Ok(vec![]);
    }
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        #[serde(default)]
        os_endpoint: Option<String>,
        #[serde(default)]
        mesh_endpoint: Option<String>,
    }
    let rows: Vec<Row> = call_for(dep, "policies", serde_json::json!({})).await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.id, r.os_endpoint, r.mesh_endpoint))
        .collect())
}

/// The deployment is being deleted (ADR-072 §4). `Ok(None)` when no Mesh is
/// configured; otherwise what the Mesh forgot, for the log.
pub async fn forget(dep: &crate::scope::Deployment) -> anyhow::Result<Option<serde_json::Value>> {
    if configured_url().is_none() {
        return Ok(None);
    }
    call_for(dep, "forget_deployment", serde_json::json!({}))
        .await
        .map(Some)
}

/// A `PackageStore` over packages this process fetched and verified. Borrows
/// rather than owns: the same set is needed again after compiling, to install
/// each integration's backend half.
struct Verified<'a>(&'a BTreeMap<(String, String), velox_integrations::Package>);

impl velox_policy::PackageStore for Verified<'_> {
    fn get(&self, id: &str, version: &str) -> Option<&velox_integrations::Package> {
        self.0.get(&(id.to_string(), version.to_string()))
    }
}

/// Fetch and verify every integration the draft names, compile the policy,
/// and store it in the Mesh as a new revision.
///
/// Compiling here is what makes "add an integration" mean something: the
/// agent gets the collection stanza, and `compiled.backend` names what the
/// deployment must carry — which the core then installs with the same
/// apply-engine the DaemonSet path uses, so both ends come from one package.
pub async fn save_policy(
    draft: &PolicyDraft,
    dep: &crate::scope::Deployment,
    tenant_id: Option<&str>,
) -> anyhow::Result<u64> {
    use anyhow::Context as _;

    // The endpoints the draft names, resolved against the deployment's
    // settings (ADR-072 §5, amendment 2): the operator maintains the
    // addresses once, every policy says which ones it uses, and a policy
    // that names none — or one that has been removed — is refused here,
    // before anything is compiled or installed.
    let settings = crate::k8s::mesh_settings(dep)
        .await
        .with_context(|| format!("reading the Mesh settings of {}", dep.name()))?;
    let (os_endpoint, _mesh_endpoint) = resolve_endpoints(&settings, &draft.endpoints)
        .with_context(|| format!("policy {} cannot be saved", draft.id))?;

    // Spec 2.0 (ADR-074): its own path, end to end. A policy is one engine
    // configuration, so it is all Spec 2.0 or all 1.x.
    if !draft.packages.is_empty() {
        if !draft.integrations.is_empty() {
            anyhow::bail!(
                "policy {} mixes Spec 2.0 packages with 1.x integrations ({}); \
                 a policy compiles to one configuration format",
                draft.id,
                draft.integrations.join(", ")
            );
        }
        return save_policy_v2(draft, dep, tenant_id, os_endpoint).await;
    }

    let mut packages = BTreeMap::new();
    let mut activations = Vec::new();
    for id in &draft.integrations {
        let artifacts = crate::catalog::artifacts(id)
            .await
            .with_context(|| format!("reading integration {id}"))?;
        let fetched = crate::catalog::Registry::from_env()
            .fetch_package(id)
            .await?;
        crate::catalog::verify_package(&fetched.manifest_yaml, &fetched.assets)
            .with_context(|| format!("refusing to compile unverified integration {id}"))?;
        let pkg = fetched.into_package()?;
        packages.insert((id.clone(), artifacts.version.clone()), pkg);
        activations.push(velox_policy::Activation {
            package_id: id.clone(),
            package_version: artifacts.version,
            vars: draft.vars.get(id).cloned().unwrap_or_default(),
            inputs: draft
                .inputs
                .get(id)
                .map(|s| s.inputs.clone())
                .unwrap_or_default(),
        });
    }

    let policy = velox_policy::Policy {
        id: draft.id.clone(),
        name: draft.name.clone(),
        // The Mesh owns revision numbering: it is the thing that knows what N
        // already is, and two writers choosing N is how revisions collide.
        revision: 0,
        activations,
        outputs: vec![velox_policy::Output::OpenSearch {
            deployment: draft.deployment.clone(),
        }],
        agent: velox_policy::AgentSettings::default(),
        endpoints: draft.endpoints.clone(),
        packages: vec![],
    };
    let renderer = velox_policy::PackageRenderer {
        store: &Verified(&packages),
        target: velox_policy::Target {
            deployment: draft.deployment.clone(),
            namespace: dep.namespace().to_string(),
            // Always the named endpoint's address: the renderer's in-cluster
            // fallback is for the DaemonSet path, never for a host. Parsed
            // here so host, port, TLS and path land in the output's own keys.
            os_endpoint: Some(
                velox_policy::OsAddress::parse(&os_endpoint.host)
                    .map_err(|e| anyhow::anyhow!("OpenSearch endpoint {}: {e}", os_endpoint.id))?,
            ),
        },
    };
    let compiled = velox_policy::compile(&policy, &renderer)
        .map_err(|e| anyhow::anyhow!("compiling policy {}: {e}", draft.id))?;

    let bodies: BTreeMap<String, String> = compiled
        .bodies
        .iter()
        .map(|(engine, body)| {
            let name = serde_json::to_value(engine)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{engine:?}"));
            (name, body.clone())
        })
        .collect();

    // The collector account, before the policy that needs it exists: an agent
    // picks up a new revision on its next check-in, and a flush the account
    // cannot make is a 403 an operator reads as a bad password. `ensure` is
    // idempotent and grants `veloxagent-*` — every name this compiler emits
    // carries the prefix (ADR-071), so a new integration needs no new grant.
    let (os_user, os_password) = crate::os_agent_user::ensure(dep).await.with_context(|| {
        format!(
            "preparing the collector account on {} for policy {}",
            dep.name(),
            draft.id
        )
    })?;
    // The backend half of every activation, under this path's names: ingest
    // pipeline, index template, index pattern and dashboards. Before the
    // policy is stored, because the agent starts writing on its next check-in
    // and documents that arrive before the template do get the mapping the
    // template was meant to prevent.
    //
    // No collector is deployed here — that is the policy's job, on the host.
    for ((id, version), pkg) in &packages {
        let selection = policy
            .activations
            .iter()
            .find(|a| &a.package_id == id)
            .map(|a| a.selection())
            .unwrap_or_default();
        crate::integrations::apply_package_for_agent(dep, pkg, version, &selection)
            .await
            .with_context(|| {
                format!(
                    "installing the backend artifacts of {id}@{version} on {}",
                    dep.name()
                )
            })?;
    }

    let out: serde_json::Value = call_for(
        dep,
        "put_policy",
        serde_json::json!({
            "tenant_id": tenant_id,
            "id": draft.id,
            "name": draft.name,
            "body": policy,
            "compiled": bodies,
            "compiled_hash": compiled.hash,
        }),
    )
    .await?;
    let revision = out
        .get("revision")
        .and_then(|v| v.as_u64())
        .context("the Mesh did not answer with a revision")?;

    // The credential row, after the policy it belongs to exists — it is keyed
    // by policy id, and a credential for a policy that does not exist yet is
    // not a thing the Mesh will store.
    //
    // Handed over separately from the policy and never inside it: revisions
    // are immutable, kept forever and readable by anyone who can read the
    // fleet, while this is a password. The Mesh serves it only to an agent
    // that authenticated on this policy.
    //
    // The window this ordering opens is empty in practice: the only moment
    // the row is missing is the first save of a brand-new policy, which by
    // definition has no agents yet. A later save reuses the same account, so
    // agents already running are unaffected.
    let _: serde_json::Value = call_for(
        dep,
        "put_policy_credential",
        serde_json::json!({
            "policy_id": draft.id,
            "username": os_user,
            "password": os_password,
        }),
    )
    .await
    .context("handing the collector credential to the Mesh")?;

    Ok(revision)
}

/// The Spec 2.0 save (ADR-074): resolve each activation's version in the
/// registry index, fetch and verify the package (signature, then inventory),
/// check it against the vendored plugin catalogue, compile the YAML and the
/// backend, settle the secrets, install the backend, and only then store the
/// revision — agents write on their next check-in, so the templates and data
/// streams must already exist.
async fn save_policy_v2(
    draft: &PolicyDraft,
    dep: &crate::scope::Deployment,
    tenant_id: Option<&str>,
    os_endpoint: &OsEndpoint,
) -> anyhow::Result<u64> {
    use anyhow::Context as _;
    use velox_integrations::spec2;
    use velox_policy::{backend2, compile2};

    let os = velox_policy::OsAddress::parse(&os_endpoint.host)
        .map_err(|e| anyhow::anyhow!("OpenSearch endpoint {}: {e}", os_endpoint.id))?;
    let catalog = spec2::builtin_catalog();

    let mut packages = Vec::new();
    let mut activations = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for a in &draft.packages {
        if !seen.insert(a.package_id.clone()) {
            anyhow::bail!(
                "package {} is activated twice in policy {}",
                a.package_id,
                draft.id
            );
        }
        let pkg = crate::catalog::resolve_package_v2(&a.package_id, Some(&a.package_version))
            .await
            .with_context(|| format!("policy {}: package {}", draft.id, a.package_id))?;
        let version = pkg.manifest.version.clone();
        let mut act = a.clone();
        act.package_version = version;
        act.spec_version = spec2::SPEC_VERSION.to_string();
        packages.push(pkg);
        activations.push(act);
    }

    let mut ctx = compile2::Context::new(&os);
    // An endpoint served under a certificate hosts cannot verify (the
    // deployment's public route on a lab domain, ADR-072 amendment 4).
    ctx.opensearch_tls_verify = !os_endpoint.insecure;
    let activated: Vec<compile2::Activated<'_>> = packages
        .iter()
        .zip(&activations)
        .map(|(p, a)| compile2::Activated {
            package: p,
            activation: a,
        })
        .collect();
    let compiled = compile2::compile(&activated, &ctx, catalog)
        .map_err(|e| anyhow::anyhow!("compiling policy {}: {e}", draft.id))?;
    let backends = packages
        .iter()
        .zip(&activations)
        .map(|(p, a)| backend2::compile(p, a, &ctx.checks()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("compiling the backend of policy {}: {e}", draft.id))?;

    // Secrets: every referenced id needs a value, typed now or already held.
    // `policy_secret_ids` fails for a policy the Mesh does not have yet —
    // which then holds nothing.
    let held: std::collections::BTreeSet<String> = call_for::<serde_json::Value>(
        dep,
        "policy_secret_ids",
        serde_json::json!({ "policy_id": draft.id }),
    )
    .await
    .ok()
    .and_then(|v| serde_json::from_value::<Vec<String>>(v["ids"].clone()).ok())
    .unwrap_or_default()
    .into_iter()
    .collect();
    let policy_exists = !held.is_empty()
        || call_for::<serde_json::Value>(
            dep,
            "policy",
            serde_json::json!({ "policy_id": draft.id }),
        )
        .await
        .is_ok();
    let missing: Vec<&String> = compiled
        .secrets
        .iter()
        .filter(|id| !draft.secrets.contains_key(*id) && !held.contains(*id))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "policy {} cannot be saved: secret(s) {} need a value",
            draft.id,
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let set: BTreeMap<String, String> = draft
        .secrets
        .iter()
        .filter(|(id, _)| compiled.secrets.contains(*id))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let keep: Vec<String> = compiled
        .secrets
        .iter()
        .filter(|id| !set.contains_key(*id))
        .cloned()
        .collect();
    let secrets_body = serde_json::json!({ "policy_id": draft.id, "set": set, "keep": keep });

    // The collector account (its grant now spans the data streams' backing
    // indices), then the backend, then — existing policy — the secrets, so a
    // revision never reaches an agent before what it references.
    let (os_user, os_password) = crate::os_agent_user::ensure(dep).await.with_context(|| {
        format!(
            "preparing the collector account on {} for policy {}",
            dep.name(),
            draft.id
        )
    })?;
    for b in &backends {
        crate::integrations::install_backend_v2(dep, b)
            .await
            .with_context(|| {
                format!(
                    "installing the backend of {}@{} on {}",
                    b.package_id,
                    b.package_version,
                    dep.name()
                )
            })?;
    }
    if policy_exists {
        let _: serde_json::Value = call_for(dep, "put_policy_secrets", secrets_body.clone())
            .await
            .context("handing the integration secrets to the Mesh")?;
    }

    let policy = velox_policy::Policy {
        id: draft.id.clone(),
        name: draft.name.clone(),
        revision: 0,
        activations: vec![],
        outputs: vec![velox_policy::Output::OpenSearch {
            deployment: draft.deployment.clone(),
        }],
        agent: velox_policy::AgentSettings::default(),
        endpoints: draft.endpoints.clone(),
        packages: activations,
    };
    let hash = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(b"fluent_bit_yaml\n");
        h.update(compiled.yaml.as_bytes());
        hex::encode(h.finalize())
    };
    let out: serde_json::Value = call_for(
        dep,
        "put_policy",
        serde_json::json!({
            "tenant_id": tenant_id,
            "id": draft.id,
            "name": draft.name,
            "body": policy,
            "compiled": { "fluent_bit_yaml": compiled.yaml },
            "compiled_hash": hash,
        }),
    )
    .await?;
    let revision = out
        .get("revision")
        .and_then(|v| v.as_u64())
        .context("the Mesh did not answer with a revision")?;
    let _: serde_json::Value = call_for(
        dep,
        "put_policy_credential",
        serde_json::json!({ "policy_id": draft.id, "username": os_user, "password": os_password }),
    )
    .await
    .context("handing the collector credential to the Mesh")?;
    if !policy_exists {
        let _: serde_json::Value = call_for(dep, "put_policy_secrets", secrets_body)
            .await
            .context("handing the integration secrets to the Mesh")?;
    }
    Ok(revision)
}

/// POST to the Mesh's console face with the shared service token. Every call
/// goes through here so the token is read in one place and a Mesh that is not
/// configured fails with one message.
pub async fn call_mesh<T: serde::de::DeserializeOwned>(
    path: &str,
    body: &serde_json::Value,
) -> anyhow::Result<T> {
    let url = configured_url()
        .ok_or_else(|| anyhow::anyhow!("no Velox Mesh is configured (VELOX_MESH_URL is unset)"))?;
    let token = service_token()
        .ok_or_else(|| anyhow::anyhow!("VELOX_MESH_SERVICE_TOKEN is unset on the control plane"))?;
    let res = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?
        .post(format!("{url}/api/mesh/{path}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await?;
    read_mesh(res).await
}

async fn read_mesh<T: serde::de::DeserializeOwned>(res: reqwest::Response) -> anyhow::Result<T> {
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        // The Mesh speaks the core's error shape, so its words reach the
        // operator rather than a generic "request failed".
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_string)))
            .unwrap_or(text);
        anyhow::bail!("the Mesh answered {status}: {msg}");
    }
    // A unit answer (`unenrol` is a bare 200) deserialises as `{}` so the
    // console gets its empty object, not a 500 for a body that is not there.
    let text = if text.trim().is_empty() {
        "{}"
    } else {
        text.as_str()
    };
    Ok(serde_json::from_str(text)?)
}

/// The token this control plane authenticates to its Mesh with: the env's
/// when `VELOX_MESH_URL` is set (the two travel together), else the
/// installed Mesh's Secret.
pub fn service_token() -> Option<String> {
    if env_url().is_some() {
        return std::env::var("VELOX_MESH_SERVICE_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    DISCOVERED
        .read()
        .ok()
        .and_then(|d| d.as_ref().map(|(_, t)| t.clone()))
}

// ---------------------------------------------------------------------------
// Installing the Mesh from the console (ADR-072 amendment 5) — the pure half;
// `k8s::install_mesh` writes it.
// ---------------------------------------------------------------------------

/// deploy/mesh/mesh.yaml, compiled in: the button and `kubectl apply` produce
/// the same objects, and the manifest stays the one place they are described.
pub const MESH_MANIFEST: &str = include_str!("../deploy/mesh/mesh.yaml");

/// The manifest's objects, with the Mesh container's image replaced when
/// the installation names another (`VELOX_MESH_IMAGE`: a mirror, a dev
/// build). Refuses a manifest that would place anything outside
/// [`MESH_NAMESPACE`] — the core's rights stop there, and so does the button.
pub fn mesh_objects(image: Option<&str>) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut docs = crate::bootstrap::bundle_docs(MESH_MANIFEST)?;
    for d in &mut docs {
        let kind = d["kind"].as_str().unwrap_or("").to_string();
        let ns = if kind == "Namespace" {
            d["metadata"]["name"].as_str()
        } else {
            d["metadata"]["namespace"].as_str()
        };
        if ns != Some(MESH_NAMESPACE) {
            anyhow::bail!("deploy/mesh/mesh.yaml places a {kind} outside {MESH_NAMESPACE}");
        }
        if let (Some(img), "Deployment") = (image, kind.as_str()) {
            if let Some(cs) = d["spec"]["template"]["spec"]["containers"].as_array_mut() {
                for c in cs.iter_mut().filter(|c| c["name"] == MESH_NAME) {
                    c["image"] = serde_json::Value::String(img.to_string());
                }
            }
        }
    }
    Ok(docs)
}

/// The Mesh's database URL on the installation's Postgres, the shape the
/// manifest's step 2 writes: own role, own database, NoTls in-cluster.
pub fn mesh_pg_url(app_namespace: &str, password: &str) -> String {
    format!(
        "postgres://velox_mesh:{password}@veloxsearch-postgres.{app_namespace}.svc:5432/velox_mesh?sslmode=disable"
    )
}

/// The password inside a `pg-url` this module wrote — so a reinstall keeps
/// the database credential the running Mesh already uses.
pub fn password_of_pg_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("postgres://velox_mesh:")?;
    let (pw, _) = rest.split_once('@')?;
    (!pw.is_empty()).then(|| pw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Mesh call is about the deployment the scope minted (ADR-072 §2):
    /// the pair comes off the `Deployment`, and a request body that tries to
    /// name another one — `deployment_ns` smuggled in beside `policy_id` — is
    /// overwritten, not forwarded. This is the one place the console's
    /// `deployment` string and the Mesh's partition key meet.
    #[test]
    fn the_pair_is_the_scopes_and_cannot_be_overridden_by_the_request() {
        let dep = crate::scope::Deployment::for_test("tornis-096o", "veloxsearch-system", None);
        let body = body_for(
            &dep,
            serde_json::json!({
                "deployment_ns": "someone-elses-ns",
                "deployment": "someone-elses-deployment",
                "policy_id": "host-ssh",
            }),
        );
        assert_eq!(body["deployment_ns"], "veloxsearch-system");
        assert_eq!(body["deployment"], "tornis-096o");
        assert_eq!(body["policy_id"], "host-ssh");
        assert_eq!(body.as_object().unwrap().len(), 3);
    }

    /// `null` for the tenant is what the admin sends; the Mesh defaults it.
    /// A non-object `extra` adds nothing rather than panicking.
    #[test]
    fn a_bare_call_carries_exactly_the_pair() {
        let dep = crate::scope::Deployment::for_test("a", "ns", Some("t1"));
        let body = body_for(&dep, serde_json::Value::Null);
        assert_eq!(
            body,
            serde_json::json!({ "deployment_ns": "ns", "deployment": "a" })
        );
    }

    /// The draft carries endpoint IDS (ADR-072 amendment 2), never an address:
    /// a body that still sends the pre-amendment `os_host` deserialises —
    /// older consoles keep working — and the field is simply not there to be
    /// read. A draft that names no endpoints parses too; `save_policy` is
    /// where it is refused.
    #[test]
    fn the_draft_names_endpoints_by_id_and_ignores_an_inline_address() {
        let d: PolicyDraft = serde_json::from_value(serde_json::json!({
            "id": "p", "name": "P", "deployment": "tornis-096o",
            "integrations": ["ssh"], "os_host": "1.2.3.4:9200",
            "endpoints": { "opensearch": "lan", "mesh": "lan" }
        }))
        .unwrap();
        assert_eq!(d.integrations, vec!["ssh"]);
        assert_eq!(d.endpoints.opensearch.as_deref(), Some("lan"));
        let bare: PolicyDraft = serde_json::from_value(serde_json::json!({
            "id": "p", "name": "P", "deployment": "d", "integrations": []
        }))
        .unwrap();
        assert!(!bare.endpoints.complete());
        let s: MeshSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(s, MeshSettings::default());
    }

    fn settings(os: &[(&str, &str)], mesh: &[(&str, &str)]) -> MeshSettings {
        MeshSettings {
            os_endpoints: os
                .iter()
                .map(|(id, host)| OsEndpoint {
                    id: id.to_string(),
                    name: id.to_uppercase(),
                    host: host.to_string(),
                    ..Default::default()
                })
                .collect(),
            mesh_endpoints: mesh
                .iter()
                .map(|(id, url)| MeshEndpoint {
                    id: id.to_string(),
                    name: id.to_uppercase(),
                    url: url.to_string(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    /// ADR-072 amendment 5: the button applies deploy/mesh/mesh.yaml as is —
    /// four objects, all in the Mesh's namespace, a `restricted` namespace,
    /// the image replaced only when the installation names one — and the
    /// URL the core then uses is the one an Ingress can route to.
    #[test]
    fn the_install_button_applies_the_manifest_in_its_namespace_only() {
        let docs = mesh_objects(None).unwrap();
        let kinds: Vec<&str> = docs.iter().map(|d| d["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            ["Namespace", "ServiceAccount", "Deployment", "Service"]
        );
        assert_eq!(
            docs[0]["metadata"]["labels"]["pod-security.kubernetes.io/enforce"],
            "restricted"
        );
        let dep = &docs[2];
        let c = &dep["spec"]["template"]["spec"]["containers"][0];
        let pinned = c["image"].as_str().unwrap().to_string();
        assert!(
            pinned.starts_with("docker.io/tornistecnologia/veloxsearch-mesh:"),
            "{pinned}"
        );
        assert_eq!(c["securityContext"]["runAsNonRoot"], true);
        assert_eq!(c["securityContext"]["allowPrivilegeEscalation"], false);
        assert_eq!(
            dep["spec"]["template"]["spec"]["automountServiceAccountToken"],
            false
        );
        assert_eq!(c["env"][1]["valueFrom"]["secretKeyRef"]["name"], MESH_NAME);
        assert_eq!(docs[3]["spec"]["ports"][0]["port"], MESH_PORT);

        let custom = mesh_objects(Some("localhost:5000/velox-mesh:dev")).unwrap();
        assert_eq!(
            custom[2]["spec"]["template"]["spec"]["containers"][0]["image"],
            "localhost:5000/velox-mesh:dev"
        );
        assert_eq!(
            in_cluster_service(&installed_url()),
            Some((MESH_NAME.into(), MESH_NAMESPACE.into(), MESH_PORT))
        );
    }

    /// A reinstall reads back the database password it wrote.
    #[test]
    fn the_mesh_pg_url_round_trips_its_password() {
        let u = mesh_pg_url("veloxsearch-system", "abc123");
        assert_eq!(
            u,
            "postgres://velox_mesh:abc123@veloxsearch-postgres.veloxsearch-system.svc:5432/velox_mesh?sslmode=disable"
        );
        assert_eq!(password_of_pg_url(&u).as_deref(), Some("abc123"));
        assert_eq!(password_of_pg_url("postgres://other:x@h/db"), None);
        assert_eq!(password_of_pg_url("postgres://velox_mesh:@h/db"), None);
    }

    /// ADR-072 amendment 4: only an in-cluster `http://<svc>.<ns>.svc` URL
    /// names a Service an Ingress can point at.
    #[test]
    fn the_mesh_service_is_read_off_an_in_cluster_url_only() {
        assert_eq!(
            in_cluster_service("http://velox-mesh.veloxsearch-system.svc:4320"),
            Some(("velox-mesh".into(), "veloxsearch-system".into(), 4320))
        );
        assert_eq!(
            in_cluster_service("http://velox-mesh.ops.svc.cluster.local:4320/"),
            Some(("velox-mesh".into(), "ops".into(), 4320))
        );
        assert_eq!(
            in_cluster_service("http://mesh.ops.svc"),
            Some(("mesh".into(), "ops".into(), 80))
        );
        for no in [
            "http://127.0.0.1:4320",
            "https://velox-mesh.veloxsearch-system.svc:4320",
            "http://mesh.example.com:4320",
            "http://velox-mesh.veloxsearch-system.svc:0",
            "",
        ] {
            assert_eq!(in_cluster_service(no), None, "{no}");
        }
    }

    /// In ingress mode the deployment's public routes come first, managed,
    /// marked insecure on a lab domain; an operator's endpoint under the
    /// reserved id is replaced, and a save writes the operator's alone.
    #[test]
    fn route_endpoints_are_derived_first_and_never_stored() {
        let access = crate::access::AccessConfig {
            mode: "ingress".into(),
            base_domain: "192.168.1.200.sslip.io".into(),
            ingress_class: "traefik".into(),
            tls_secret: String::new(),
        };
        let stored = settings(
            &[("lan", "10.0.0.5"), (ROUTE_ENDPOINT_ID, "stale")],
            &[("lan", "http://10.0.0.5:4320")],
        );
        let s = with_route_endpoints(stored.clone(), &access, "tornis-096o", true);
        assert_eq!(s.os_endpoints.len(), 2);
        let os = &s.os_endpoints[0];
        assert_eq!(os.id, ROUTE_ENDPOINT_ID);
        assert_eq!(
            os.host,
            "https://tornis-096o-opensearch.192.168.1.200.sslip.io:443"
        );
        assert!(os.managed && os.insecure);
        let mesh = &s.mesh_endpoints[0];
        assert_eq!(
            mesh.url,
            "https://tornis-096o-mesh.192.168.1.200.sslip.io:443"
        );
        assert!(mesh.managed && mesh.insecure);
        // The derived addresses parse as their consumers read them.
        let a = velox_policy::OsAddress::parse(&os.host).unwrap();
        assert_eq!((a.port, a.tls), (443, true));
        assert_eq!(validate_settings(&s), Ok(()));

        let saved = without_route_endpoints(s);
        assert_eq!(saved.os_endpoints.len(), 1);
        assert_eq!(saved.os_endpoints[0].id, "lan");
        assert_eq!(saved.mesh_endpoints.len(), 1);
        assert!(!serde_json::to_string(&saved).unwrap().contains("managed"));

        // No Mesh route without a routable Mesh; nothing at all on port-forward.
        let no_mesh = with_route_endpoints(stored.clone(), &access, "x", false);
        assert!(no_mesh.mesh_endpoints.iter().all(|e| !e.managed));
        let pf = with_route_endpoints(stored, &crate::access::AccessConfig::default(), "x", true);
        assert!(pf.os_endpoints.iter().all(|e| !e.managed));

        // A TLS Secret means a certificate hosts can verify.
        let mut tls = access.clone();
        tls.tls_secret = "wildcard".into();
        let v = with_route_endpoints(MeshSettings::default(), &tls, "x", true);
        assert!(!v.os_endpoints[0].insecure && !v.mesh_endpoints[0].insecure);
    }

    /// The reserved id is the core's: an operator's endpoint cannot take it.
    #[test]
    fn the_route_endpoint_id_is_reserved() {
        let s = settings(&[(ROUTE_ENDPOINT_ID, "10.0.0.5")], &[]);
        assert!(validate_settings(&s).unwrap_err().contains("reserved"));
        let s = settings(&[], &[(ROUTE_ENDPOINT_ID, "http://10.0.0.5:4320")]);
        assert!(validate_settings(&s).unwrap_err().contains("reserved"));
    }

    /// Each address in the shape its consumer expects — `host`,
    /// `host:port` or an http(s) URL for the collector's output (the port
    /// and TLS follow the spelling, bound at compile time), a base URL for
    /// `velox-agent enroll --mesh` — under a slug id unique in its list.
    #[test]
    fn settings_are_validated_in_the_shape_their_consumer_expects() {
        assert!(validate_settings(&MeshSettings::default()).is_ok());
        assert!(validate_settings(&settings(
            &[
                ("lan", "10.43.207.250"),
                ("lb", "os.example.com:9200"),
                ("ing", "os.192.168.1.200.sslip.io:443"),
                ("url", "https://os.example.com"),
                ("proxy", "http://proxy.example.com:8080/opensearch/"),
            ],
            &[
                ("lan", "http://192.168.1.200:4320"),
                ("pub", "https://mesh.example.com:4320")
            ]
        ))
        .is_ok());
        let err = |s: MeshSettings| validate_settings(&s).unwrap_err();
        assert!(err(settings(&[("lan", "ftp://os.example.com")], &[])).contains("http or https"));
        assert!(err(settings(&[("lan", "os.example.com:x")], &[])).contains("port"));
        assert!(
            err(settings(&[("lan", "https://u:p@os.example.com")], &[])).contains("credentials")
        );
        assert!(err(settings(&[("lan", "")], &[])).contains("empty"));
        assert!(err(settings(&[], &[("lan", "mesh.example.com:4320")])).contains("base URL"));
        assert!(err(settings(&[], &[("lan", "https://")])).contains("base URL"));
        assert!(err(settings(&[], &[("lan", "https://mesh example.com")])).contains("base URL"));
        assert!(err(settings(&[], &[("lan", "ftp://mesh.example.com")])).contains("base URL"));
        // Ids: slugs, unique per list (the same id in BOTH lists is fine —
        // they are different namespaces).
        assert!(err(settings(&[("LAN", "h")], &[])).contains("slug"));
        assert!(err(settings(&[("-lan", "h")], &[])).contains("slug"));
        assert!(err(settings(&[("", "h")], &[])).contains("slug"));
        assert!(err(settings(&[("a", "h"), ("a", "h2")], &[])).contains("repeated"));
        assert!(validate_settings(&settings(&[("a", "h")], &[("a", "http://x")])).is_ok());
        let mut blank = settings(&[("a", "h")], &[]);
        blank.os_endpoints[0].name = "  ".into();
        assert!(err(blank).contains("name"));
    }

    /// A policy compiles against the endpoint it NAMES, and only that: no
    /// endpoint, half an endpoint, or a removed one is a refusal that says
    /// which — never the in-cluster default the DaemonSet path uses.
    #[test]
    fn a_policy_resolves_both_endpoints_or_is_refused_by_name() {
        let s = settings(&[("lan", "10.0.0.1")], &[("lan", "http://10.0.0.1:4320")]);
        let ep = |os: Option<&str>, mesh: Option<&str>| velox_policy::PolicyEndpoints {
            opensearch: os.map(String::from),
            mesh: mesh.map(String::from),
        };
        let (o, m) = resolve_endpoints(&s, &ep(Some("lan"), Some("lan"))).unwrap();
        assert_eq!(o.host, "10.0.0.1");
        assert_eq!(m.url, "http://10.0.0.1:4320");
        let msg = |e: velox_policy::PolicyEndpoints| {
            format!("{:#}", resolve_endpoints(&s, &e).unwrap_err())
        };
        assert!(msg(ep(None, None)).contains("no OpenSearch endpoint"));
        assert!(msg(ep(Some(""), Some("lan"))).contains("no OpenSearch endpoint"));
        assert!(msg(ep(Some("lan"), None)).contains("no Velox Mesh endpoint"));
        assert!(msg(ep(Some("gone"), Some("lan"))).contains("`gone` is not configured"));
        assert!(msg(ep(Some("lan"), Some("gone"))).contains("`gone` is not configured"));
    }

    /// Taking an endpoint out of the settings while a policy still names it
    /// is refused with the policy's name; renaming or re-addressing one in
    /// place is not a removal.
    #[test]
    fn removing_an_endpoint_a_policy_uses_is_caught_before_the_save() {
        let cur = settings(
            &[("lan", "a"), ("lb", "b")],
            &[("lan", "http://a"), ("pub", "http://b")],
        );
        let policies = vec![
            (
                "web".to_string(),
                Some("lb".to_string()),
                Some("pub".to_string()),
            ),
            (
                "db".to_string(),
                Some("lan".to_string()),
                Some("lan".to_string()),
            ),
            ("old".to_string(), None, None),
        ];
        let next = settings(
            &[("lan", "a-moved")],
            &[("lan", "http://a"), ("pub", "http://b")],
        );
        assert_eq!(
            removed_endpoints_in_use(&cur, &next, &policies),
            vec![("web".to_string(), "lb".to_string())]
        );
        let next = settings(&[("lan", "a"), ("lb", "b")], &[("lan", "http://a")]);
        assert_eq!(
            removed_endpoints_in_use(&cur, &next, &policies),
            vec![("web".to_string(), "pub".to_string())]
        );
        assert!(removed_endpoints_in_use(&cur, &cur, &policies).is_empty());
    }

    /// The pre-list settings — one `os_host`, one `mesh_url` — read as one
    /// endpoint each under a fixed id, so a deployment configured before the
    /// amendment loses nothing; blanks read as no endpoint.
    #[test]
    fn legacy_scalar_settings_become_one_named_endpoint_each() {
        let s = MeshSettings::from_legacy(
            Some(" 10.43.207.250 ".into()),
            Some("http://192.168.1.200:4320/".into()),
        );
        assert_eq!(s.os_endpoints.len(), 1);
        assert_eq!(s.os_endpoints[0].id, LEGACY_ENDPOINT_ID);
        assert_eq!(s.os_endpoints[0].host, "10.43.207.250");
        assert_eq!(s.mesh_endpoints[0].url, "http://192.168.1.200:4320");
        assert!(validate_settings(&s).is_ok());
        assert_eq!(
            MeshSettings::from_legacy(Some("  ".into()), None),
            MeshSettings::default()
        );
    }

    /// Unset is "not installed", and that is the state a fresh install is in.
    /// The three fields must agree, because the console branches on them.
    #[test]
    fn unconfigured_is_the_default_and_says_nothing_else() {
        let s = MeshStatus::default();
        assert!(!s.configured);
        assert_eq!(s.url, None);
        assert_eq!(
            s.reachable, None,
            "an unconfigured Mesh is not 'unreachable'"
        );
        assert_eq!(s.error, None);
    }

    /// A trailing slash in the operator's env var must not become `//healthz`.
    #[test]
    fn the_url_is_normalised() {
        // `configured_url` reads the process environment, so drive the same
        // normalisation the function applies rather than mutating it here.
        let norm = |s: &str| s.trim().trim_end_matches('/').to_string();
        assert_eq!(norm("http://velox-mesh:4320/"), "http://velox-mesh:4320");
        assert_eq!(norm("  http://velox-mesh:4320  "), "http://velox-mesh:4320");
        assert_eq!(norm("http://velox-mesh:4320"), "http://velox-mesh:4320");
    }

    /// Configured-but-unreachable is its OWN state: an operator who installed
    /// the Mesh and hit a NetworkPolicy must not be told to install it again.
    #[test]
    fn configured_but_unreachable_is_distinct_from_not_installed() {
        let down = MeshStatus {
            configured: true,
            source: Some("installed"),
            url: Some("http://velox-mesh:4320".into()),
            reachable: Some(false),
            error: Some("connection refused".into()),
        };
        assert_ne!(down.configured, MeshStatus::default().configured);
        assert!(down.error.is_some(), "the operator is told why");
        assert!(down.url.is_some(), "and what was tried");
    }
}
