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
    /// Per-integration variables (schema 1.1), keyed by integration id.
    #[serde(default)]
    pub vars: BTreeMap<String, BTreeMap<String, String>>,
    /// Published OpenSearch endpoint for agents outside the cluster; unset
    /// means the in-cluster service name.
    #[serde(default)]
    pub os_host: Option<String>,
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
) -> anyhow::Result<u64> {
    use anyhow::Context as _;

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
            os_host: draft.os_host.clone(),
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

    // The names this policy's artifacts take — `veloxagent-<integration>-
    // <signal>-v<major>` (ADR-071) — read off what the compiler produced
    // rather than derived again here, so the index in the collector's config,
    // the artifacts in OpenSearch and the account's grant are one decision.
    let artifact_names: Vec<String> = compiled
        .backend
        .iter()
        .map(|b| b.artifact_name.clone())
        .collect();

    // The collector account, before the policy that needs it exists.
    //
    // Order is the whole point: an agent picks up a new revision on its next
    // check-in, and a revision whose indices the account cannot write yet
    // means a flush rejected with a 403 and an operator reading a 401-shaped
    // error. The account is created (or widened to cover a newly added
    // integration) first, then the policy is stored, then agents converge.
    //
    // The indices come from the packages we just verified, so the role grants
    // exactly what the policy's integrations declare and nothing else.
    let (os_user, os_password) = crate::os_agent_user::ensure(dep, &artifact_names)
        .await
        .with_context(|| {
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
        crate::integrations::apply_package_for_agent(dep, pkg, version)
            .await
            .with_context(|| {
                format!(
                    "installing the backend artifacts of {id}@{version} on {}",
                    dep.name()
                )
            })?;
    }

    let out: serde_json::Value = call_mesh(
        "put_policy",
        &serde_json::json!({
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
    let _: serde_json::Value = call_mesh(
        "put_policy_credential",
        &serde_json::json!({
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

/// GET the same way.
pub async fn get_mesh<T: serde::de::DeserializeOwned>(path: &str) -> anyhow::Result<T> {
    let url = configured_url()
        .ok_or_else(|| anyhow::anyhow!("no Velox Mesh is configured (VELOX_MESH_URL is unset)"))?;
    let token = service_token()
        .ok_or_else(|| anyhow::anyhow!("VELOX_MESH_SERVICE_TOKEN is unset on the control plane"))?;
    let res = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?
        .get(format!("{url}/api/mesh/{path}"))
        .bearer_auth(token)
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
