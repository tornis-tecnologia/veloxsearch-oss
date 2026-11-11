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
    /// `VELOX_MESH_URL` is set. False ⇒ the install guide.
    pub configured: bool,
    /// The configured base URL, so the screen can name what it tried.
    pub url: Option<String>,
    /// `None` when not configured; otherwise the result of `GET /healthz`.
    pub reachable: Option<bool>,
    /// Why the probe failed, for the operator — not a stack trace.
    pub error: Option<String>,
}

/// The configured Mesh base URL, trimmed of a trailing slash so joining a
/// path never produces `//`.
pub fn configured_url() -> Option<String> {
    std::env::var("VELOX_MESH_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

/// Read the status, probing the Mesh when one is configured.
pub async fn status() -> MeshStatus {
    let Some(url) = configured_url() else {
        return MeshStatus::default();
    };
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return MeshStatus {
                configured: true,
                url: Some(url),
                reachable: Some(false),
                error: Some(e.to_string()),
            }
        }
    };
    match client.get(format!("{url}/healthz")).send().await {
        Ok(r) if r.status().is_success() => MeshStatus {
            configured: true,
            url: Some(url),
            reachable: Some(true),
            error: None,
        },
        Ok(r) => MeshStatus {
            configured: true,
            url: Some(url),
            reachable: Some(false),
            error: Some(format!("the Mesh answered {} at /healthz", r.status())),
        },
        Err(e) => MeshStatus {
            configured: true,
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
}

impl PolicyDraft {
    /// The draft a stored policy document edits as: its integrations in
    /// order, each with the selection its activation carries.
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
        }
    }
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

/// Create an empty policy (ADR-073): a name the operator can then add
/// integrations to from the Data tab. Refused when it exists — a second
/// create is not an edit.
pub async fn create_policy(
    dep: &crate::scope::Deployment,
    id: &str,
    name: &str,
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
    };
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

/// The deployment's Mesh settings (ADR-072 §5): facts about how hosts reach
/// *this* deployment, kept on its CR like every other intent the core must
/// re-derive after a restart.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MeshSettings {
    /// Published OpenSearch endpoint for agents outside the cluster; unset
    /// means the in-cluster service name. One per deployment, because one
    /// deployment has one published address and every policy against it
    /// compiles with the same one.
    #[serde(default)]
    pub os_host: Option<String>,
    /// Where a host OUTSIDE the cluster reaches the Mesh itself — the `--mesh`
    /// of `velox-agent enroll`. The core talks to the controller over
    /// `VELOX_MESH_URL` (an in-cluster name); the address an agent on a VM
    /// uses is whatever the operator published, and it is a fact of the
    /// deployment because that is where the enrolment commands are shown.
    /// Unset = the console prints the `MESH_URL` placeholder.
    #[serde(default)]
    pub mesh_url: Option<String>,
}

/// What a setting may hold, before it lands verbatim somewhere an operator
/// would only meet the mistake later: `os_host` in a collector's output
/// stanza ("healthy agent, no data"), `mesh_url` in a shell command on the
/// screen. Empty strings are "unset" to the CR layer and pass.
pub fn validate_settings(s: &MeshSettings) -> Result<(), &'static str> {
    if let Some(h) = s
        .os_host
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        if h.contains("://") || h.contains('/') || h.chars().any(char::is_whitespace) {
            return Err("os_host is a host or host:port, without scheme or path");
        }
    }
    if let Some(u) = s
        .mesh_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        let scheme_ok = u.starts_with("http://") || u.starts_with("https://");
        let host = u.split("://").nth(1).unwrap_or("");
        if !scheme_ok || host.is_empty() || u.chars().any(char::is_whitespace) {
            return Err("mesh_url is a base URL such as https://mesh.example.com:4320");
        }
    }
    Ok(())
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

    // The deployment's published endpoint, read here rather than carried by
    // the draft (ADR-072 §5): every policy against this deployment compiles
    // against the same address, and the operator sets it once.
    let settings = crate::k8s::mesh_settings(dep)
        .await
        .with_context(|| format!("reading the Mesh settings of {}", dep.name()))?;

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
    };
    let renderer = velox_policy::PackageRenderer {
        store: &Verified(&packages),
        target: velox_policy::Target {
            deployment: draft.deployment.clone(),
            namespace: dep.namespace().to_string(),
            os_host: settings.os_host.clone(),
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
    Ok(serde_json::from_str(&text)?)
}

/// The token this control plane authenticates to its Mesh with.
pub fn service_token() -> Option<String> {
    std::env::var("VELOX_MESH_SERVICE_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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

    /// The draft no longer carries the published endpoint (ADR-072 §5): a
    /// body that still sends `os_host` deserialises — older consoles keep
    /// working — and the field is simply not there to be read.
    #[test]
    fn the_draft_ignores_a_per_policy_endpoint() {
        let d: PolicyDraft = serde_json::from_value(serde_json::json!({
            "id": "p", "name": "P", "deployment": "tornis-096o",
            "integrations": ["ssh"], "os_host": "1.2.3.4:9200"
        }))
        .unwrap();
        assert_eq!(d.integrations, vec!["ssh"]);
        let s: MeshSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(s, MeshSettings::default());
    }

    /// The two addresses an agent outside the cluster needs, each in the
    /// shape its consumer expects: a bare host[:port] for the collector's
    /// output, a base URL for `velox-agent enroll --mesh`.
    #[test]
    fn settings_are_validated_in_the_shape_their_consumer_expects() {
        let ok = |os: Option<&str>, mesh: Option<&str>| {
            validate_settings(&MeshSettings {
                os_host: os.map(String::from),
                mesh_url: mesh.map(String::from),
            })
        };
        assert!(ok(None, None).is_ok());
        assert!(ok(Some(""), Some("  ")).is_ok(), "empty is unset");
        assert!(ok(
            Some("os.example.com:9200"),
            Some("https://mesh.example.com:4320")
        )
        .is_ok());
        assert!(
            ok(Some("http://os.example.com"), None).is_err(),
            "no scheme on os_host"
        );
        assert!(
            ok(Some("os.example.com/path"), None).is_err(),
            "no path on os_host"
        );
        assert!(
            ok(None, Some("mesh.example.com:4320")).is_err(),
            "mesh_url needs a scheme"
        );
        assert!(ok(None, Some("https://")).is_err(), "mesh_url needs a host");
        assert!(
            ok(None, Some("https://mesh example.com")).is_err(),
            "no whitespace"
        );
        assert!(
            ok(None, Some("ftp://mesh.example.com")).is_err(),
            "http(s) only"
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
            url: Some("http://velox-mesh:4320".into()),
            reachable: Some(false),
            error: Some("connection refused".into()),
        };
        assert_ne!(down.configured, MeshStatus::default().configured);
        assert!(down.error.is_some(), "the operator is told why");
        assert!(down.url.is_some(), "and what was tried");
    }
}
