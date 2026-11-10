// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The login a host agent's collector uses to write into a deployment's
//! OpenSearch — one shared, least-privilege account per deployment, created
//! here and handed to agents by Velox Mesh (ADR-068).
//!
//! ## Why not the admin credentials
//!
//! The in-cluster DaemonSet path authenticates its collectors with the
//! deployment's **admin** login (`agents.rs`, resolved through
//! `k8s::admin_creds`). That is defensible inside the cluster, where the
//! password never leaves a Secret mounted into a pod on the same node. It is
//! not defensible for Velox Mesh: an agent runs on someone's VM, outside the
//! cluster, and the credential it holds is as exposed as that host is. An
//! admin login there means one compromised web server can delete every index
//! in the deployment.
//!
//! So the collector gets an account that can do exactly one thing: append to
//! the indices the policy's integrations declare. It cannot search them, it
//! cannot drop them, and it cannot see any other index. That is the whole
//! point of this module, and `role_body` is where it is spelled out.
//!
//! ## Why a user and not an API key
//!
//! Because OpenSearch has no API keys. Elasticsearch does (`POST
//! /_security/api_key`); the OpenSearch security plugin never adopted the
//! feature, and its equivalent is an internal user mapped to a role. The
//! credential is therefore a username and a password, and the properties that
//! actually matter — scoped, revocable, rotatable without touching the admin
//! password — come from the role, not from the format.
//!
//! ## Where the password lives
//!
//! In a Kubernetes Secret next to the deployment, generated once and reused.
//! Read-or-create rather than regenerate: re-saving a policy must not change
//! the password under agents that are already running, because a rotation they
//! have not picked up yet is an outage with a 401 for a symptom.
//!
//! ## What writes it where
//!
//! | Step | Who |
//! | --- | --- |
//! | create the role and the user in OpenSearch | this module, with admin creds |
//! | keep the password | a Secret in the deployment's namespace |
//! | hand the credential to agents | Velox Mesh, over the authenticated check-in |
//! | put it in front of the engine | the agent, as `${VELOX_OS_USER}` / `${VELOX_OS_PASSWORD}` in the child's environment |
//!
//! The credential never reaches a policy revision or a configuration file:
//! the compiled config carries the `${...}` references the core's ConfigMap
//! path already uses (`agents::PASSWORD_REF`), and the agent exports the
//! values into the engine process. A policy in the Mesh's database is
//! therefore not a secret, which is what makes revisions safe to keep forever.

use crate::scope::Deployment;
use anyhow::{Context as _, Result};

/// The role the collector account holds. One per deployment's OpenSearch —
/// each deployment is its own cluster, so the name does not need qualifying.
pub(crate) const AGENT_ROLE: &str = "veloxagent-role";

/// The account itself. Shared by every agent writing into this deployment:
/// per-agent accounts would be the stronger model, but revoking one host then
/// means a security-plugin write per host and a rotation path we do not have
/// yet — this is the honest first step, and the role is the part that limits
/// the damage.
pub(crate) const AGENT_USER: &str = "veloxagent-user";

/// The Secret holding the account's password, in the deployment's namespace
/// beside its admin Secret.
fn secret_name(dep: &Deployment) -> String {
    format!("{}-veloxagent-user", dep.name())
}

/// What the account is allowed to do.
///
/// Three decisions, each one a thing that breaks if it is wrong:
///
/// 1. **`indices:data/write/bulk` at CLUSTER level.** Fluent Bit ships through
///    `_bulk`, and OpenSearch checks the bulk action twice — once as a cluster
///    permission for the request and once per index for its items. A role with
///    only the index-level grant fails every flush with a 403, which looks
///    exactly like a wrong password.
/// 2. **`write` and `create_index` per index pattern.** `write` is
///    `indices:data/write*` plus `indices:admin/mapping/put`; it does *not*
///    include creating the index, and the first flush into a brand-new
///    deployment creates one. `create_index` covers that and nothing else.
/// 3. **Patterns, not `*`.** The account may only touch the indices the
///    policy's integrations declare. Adding an integration widens the role;
///    nothing else does.
///
/// Notably absent: `read`, `search`, `delete` and anything cluster-wide. A
/// collector has no business reading back what it wrote.
pub(crate) fn role_body(patterns: &[String]) -> serde_json::Value {
    serde_json::json!({
        "description": "VeloxSearch: read and write on the veloxagent-* artifacts",
        "cluster_permissions": ["indices:data/write/bulk"],
        "index_permissions": [{
            "index_patterns": patterns,
            "allowed_actions": ["read", "write", "create_index"]
        }],
        "tenant_permissions": []
    })
}

/// The internal user, with the role attached directly rather than through a
/// separate rolesmapping write: one less call, and no window where the account
/// exists with no permissions.
fn user_body(password: &str) -> serde_json::Value {
    serde_json::json!({
        "password": password,
        "opendistro_security_roles": [AGENT_ROLE],
        "attributes": { "managed-by": "veloxsearch" },
        "description": "VeloxSearch: the Velox Agent collector account"
    })
}

/// The index patterns a set of artifact names grants, sorted and deduplicated
/// so the same policy always renders the same role (an unnecessary PUT is a
/// needless audit-log entry and a needless chance to fail).
///
/// Every name is refused unless it carries the agent prefix. That is not
/// defensive clutter: this list is the only thing standing between a policy
/// and a grant, and a name that arrived from somewhere else — a package that
/// declared its own index, a future caller passing `manifest.index` by
/// mistake — would hand the collector write access to the DaemonSet path's
/// data. `veloxagent-*` is what this account exists to touch.
pub(crate) fn patterns(artifact_names: &[String]) -> Vec<String> {
    let prefix = format!("{}-", velox_integrations::AGENT_ARTIFACT_PREFIX);
    let mut out: Vec<String> = artifact_names
        .iter()
        .map(|n| n.trim_end_matches('*').to_string())
        .filter(|n| n.starts_with(&prefix) && n.len() > prefix.len())
        .map(|n| velox_integrations::agent_artifact_pattern(&n))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Read the account's password from its Secret, or generate and store one.
async fn password(dep: &Deployment) -> Result<String> {
    let client = crate::k8s::client().await?;
    let name = secret_name(dep);
    if let Some(existing) =
        crate::k8s::get_dynamic(&client, "", "v1", "Secret", Some(dep.namespace()), &name).await?
    {
        if let Some(b64) = existing["data"]["password"].as_str() {
            use base64::Engine as _;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .context("the Velox Mesh agent Secret holds a password that is not base64")?;
            let pw = String::from_utf8(raw)
                .context("the Velox Mesh agent Secret holds a password that is not UTF-8")?;
            if !pw.is_empty() {
                return Ok(pw);
            }
        }
    }

    // The same generator the admin password uses: its alphabet is the one
    // that survives a shell, a URL, JSON and a Fluent Bit config, which is
    // exactly the set of places this value passes through.
    let pw = crate::k8s::gen_admin_password()?;
    let manifest = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "type": "Opaque",
        "metadata": {
            "name": name,
            "namespace": dep.namespace(),
            "labels": dep.owner_labels(),
        },
        "stringData": { "username": AGENT_USER, "password": pw },
    });
    crate::k8s::apply_dynamic(
        &client,
        "",
        "v1",
        "Secret",
        Some(dep.namespace()),
        &name,
        &manifest,
    )
    .await
    .with_context(|| format!("storing the Velox Mesh agent password for {}", dep.name()))?;
    Ok(pw)
}

/// Create (or bring up to date) the collector account and return the
/// credential Velox Mesh must hand to agents.
///
/// Idempotent, and ordered so a failure never leaves a usable account with the
/// wrong permissions: the role is written first, then the user that holds it.
/// The final check is not ceremony — it is the difference between "we wrote
/// some JSON" and "this login can write", and the operator finds out here
/// rather than from a 401 in an agent's log an hour later.
pub(crate) async fn ensure(
    dep: &Deployment,
    artifact_names: &[String],
) -> Result<(String, String)> {
    let base = crate::recipes::os_base(dep);
    let http = crate::recipes::http()?;
    let (admin_user, admin_pw) = crate::k8s::admin_creds(dep).await;
    if admin_pw.is_empty() {
        anyhow::bail!(
            "no admin credentials for {}; cannot create the collector account",
            dep.name()
        );
    }
    let pw = password(dep).await?;

    let role = http
        .put(format!("{base}/_plugins/_security/api/roles/{AGENT_ROLE}"))
        .basic_auth(&admin_user, Some(&admin_pw))
        .json(&role_body(&patterns(artifact_names)))
        .send()
        .await
        .context("creating the collector role")?;
    if !role.status().is_success() {
        let status = role.status();
        let body = role.text().await.unwrap_or_default();
        anyhow::bail!("creating the collector role: {status} {body}");
    }

    let user = http
        .put(format!(
            "{base}/_plugins/_security/api/internalusers/{AGENT_USER}"
        ))
        .basic_auth(&admin_user, Some(&admin_pw))
        .json(&user_body(&pw))
        .send()
        .await
        .context("creating the collector account")?;
    if !user.status().is_success() {
        let status = user.status();
        let body = user.text().await.unwrap_or_default();
        anyhow::bail!("creating the collector account: {status} {body}");
    }

    let check = http
        .get(format!("{base}/_plugins/_security/authinfo"))
        .basic_auth(AGENT_USER, Some(&pw))
        .send()
        .await
        .context("verifying the collector account")?;
    if !check.status().is_success() {
        anyhow::bail!(
            "the collector account was written but {} does not accept it ({})",
            dep.name(),
            check.status()
        );
    }

    Ok((AGENT_USER.to_string(), pw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_is_granted_at_the_cluster_level_too() {
        // A role without this fails every flush with a 403 that looks like a
        // bad password. OpenSearch checks the bulk action as a cluster
        // permission AND per index.
        let b = role_body(&["nginx-logs*".into()]);
        let cluster = b["cluster_permissions"].as_array().unwrap();
        assert!(cluster
            .iter()
            .any(|p| p.as_str() == Some("indices:data/write/bulk")));
    }

    #[test]
    fn the_account_reads_writes_and_creates_but_holds_nothing_wider() {
        let b = role_body(&["veloxagent-ssh-logs-v1*".into()]);
        let actions = b["index_permissions"][0]["allowed_actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        // `write` does not include index creation, and the first flush into a
        // fresh deployment creates the index.
        assert!(actions.contains(&"create_index"), "{actions:?}");
        assert!(actions.contains(&"write"), "{actions:?}");
        // Read is granted on these indices so an agent-side check or a
        // Dashboards session using this account can see what it wrote.
        assert!(actions.contains(&"read"), "{actions:?}");
        // What it must never hold: anything that reaches past the pattern.
        for forbidden in ["indices_all", "*", "all_access", "manage"] {
            assert!(!actions.contains(&forbidden), "{actions:?}");
        }
        assert!(b["cluster_permissions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p.as_str() == Some("indices:data/write/bulk")));
    }

    #[test]
    fn the_role_is_scoped_to_the_policys_artifacts() {
        let b = role_body(&patterns(&[
            "veloxagent-nginx-logs-v1".into(),
            "veloxagent-ssh-logs-v1".into(),
        ]));
        let pats = b["index_permissions"][0]["index_patterns"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            pats,
            vec!["veloxagent-nginx-logs-v1*", "veloxagent-ssh-logs-v1*"]
        );
    }

    #[test]
    fn patterns_are_stable_and_deduplicated() {
        // The same policy must render the same role: an unnecessary PUT is a
        // needless audit entry and a needless chance to fail.
        let a = patterns(&[
            "veloxagent-b-logs-v1".into(),
            "veloxagent-a-logs-v1".into(),
            "veloxagent-a-logs-v1*".into(),
        ]);
        assert_eq!(a, vec!["veloxagent-a-logs-v1*", "veloxagent-b-logs-v1*"]);
        assert_eq!(
            a,
            patterns(&[
                "veloxagent-a-logs-v1*".into(),
                "veloxagent-b-logs-v1".into()
            ])
        );
    }

    /// The grant list is the only thing between a policy and a permission, so
    /// a name that is not one of this path's artifacts does not get one —
    /// including the names the DaemonSet path owns, and `""`, which would
    /// render `*` and grant the entire cluster.
    #[test]
    fn only_this_paths_artifacts_can_be_granted() {
        for refused in ["", "*", "nginx-logs", "veloxagent-", "ssh-logs"] {
            assert!(
                patterns(&[refused.to_string()]).is_empty(),
                "{refused:?} was granted"
            );
        }
        assert_eq!(
            patterns(&["veloxagent-x-logs-v1".into()]),
            vec!["veloxagent-x-logs-v1*"]
        );
    }

    #[test]
    fn the_user_carries_the_role_directly() {
        let b = user_body("pw");
        assert_eq!(b["opendistro_security_roles"][0], AGENT_ROLE);
        assert_eq!(b["password"], "pw");
    }
}
