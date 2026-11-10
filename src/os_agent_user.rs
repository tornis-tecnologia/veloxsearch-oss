// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The login every Fluent Bit collector of a deployment writes with —
//! `veloxagent-user`, holding `veloxagent-role` — created here, by the core,
//! with the deployment's admin login (ADR-070).
//!
//! ## Who uses what
//!
//! | Process | Login |
//! | --- | --- |
//! | the core (creates accounts, templates, pipelines, dashboards) | the deployment's admin |
//! | the in-cluster Fluent Bit (DaemonSet / Deployment, `agents.rs`) | `veloxagent-user` |
//! | the host Fluent Bit under Velox Agent (Velox Mesh, `mesh.rs`) | `veloxagent-user` |
//!
//! The core keeps the admin login because creating an internal user and a
//! role is a privileged call — there is no other way to make the account. It
//! never leaves the core's process. Collectors never see it: a collector's
//! credential sits in a Secret mounted into a pod, or in the memory of an agent
//! on someone's VM, and either is as exposed as the place it runs. An admin
//! login there means one compromised host can delete every index in the
//! deployment.
//!
//! ## Why a user and not an API key
//!
//! Because OpenSearch has no API keys. Elasticsearch does (`POST
//! /_security/api_key`); the OpenSearch security plugin never adopted the
//! feature, and its equivalent is an internal user mapped to a role. The
//! credential is therefore a username and a password, and the properties that
//! actually matter — scoped, revocable, rotatable without touching the admin
//! password — come from the role.
//!
//! ## What the role reaches
//!
//! `veloxagent-*`, and nothing else (ADR-071). Every artifact either
//! collection path creates carries that prefix, so the prefix *is* the
//! boundary: one static pattern rather than a per-policy list, because both
//! paths share one account per deployment and a computed grant would have to
//! be the union of every policy and every in-cluster install — a list that is
//! wrong the moment either changes.
//!
//! ## Where the password lives
//!
//! In a Secret beside the deployment, generated once and reused. Read-or-
//! create rather than regenerate: re-installing an integration must not rotate
//! the password under collectors that are already running, because a rotation
//! they have not picked up yet is an outage with a 401 for a symptom. Deleting
//! the Secret is the rotation gesture; the next [`ensure`] mints a new one and
//! every collector converges on it (the in-cluster ones through
//! `agents::reconcile_creds`, the host ones through the Mesh's check-in).
//!
//! The credential never reaches a policy revision or a ConfigMap: configs
//! carry `${VELOX_OS_PASSWORD}`, and the value travels in a Secret
//! (in-cluster) or in the agent's memory (host).

use crate::scope::Deployment;
use anyhow::{Context as _, Result};

/// The role every collector account holds. One per deployment's OpenSearch —
/// each deployment is its own cluster, so the name does not need qualifying.
pub(crate) const AGENT_ROLE: &str = "veloxagent-role";

/// The account itself, shared by every collector writing into this
/// deployment. Per-collector accounts would be the stronger model, but
/// revoking one host then means a security-plugin write per host and a
/// rotation path that does not exist yet; what limits the damage today is the
/// role.
pub(crate) const AGENT_USER: &str = "veloxagent-user";

/// Names this module used before ADR-071 settled them. Removed on every
/// [`ensure`] — an account nothing uses is an account nobody rotates, and the
/// old role granted write on index names that now belong to nobody.
/// Idempotent: a 404 is the normal answer, and once no deployment carries
/// them this list can go.
const LEGACY_USERS: &[&str] = &["velox_mesh_agent"];
const LEGACY_ROLES: &[&str] = &["velox_mesh_writer"];

/// The Secret holding the account's password, in the deployment's namespace
/// beside its admin Secret.
fn secret_name(dep: &Deployment) -> String {
    format!("{}-veloxagent-user", dep.name())
}

/// The Secret the pre-ADR-071 account kept its password in. Deleted with the
/// account it belonged to.
fn legacy_secret_name(dep: &Deployment) -> String {
    format!("{}-velox-mesh-agent", dep.name())
}

/// What the account is allowed to do.
///
/// 1. **`indices:data/write/bulk` at CLUSTER level.** Fluent Bit ships through
///    `_bulk`, and OpenSearch checks the bulk action twice — once as a cluster
///    permission for the request and once per index for its items. A role with
///    only the index-level grant fails every flush with a 403, which looks
///    exactly like a wrong password.
/// 2. **`read`, `write` and `create_index` on `veloxagent-*`.** `write` is
///    `indices:data/write*` plus `indices:admin/mapping/put`; it does *not*
///    include creating the index, and the first flush into a fresh deployment
///    creates one. `read` lets the account see what it wrote — an agent-side
///    check, or a Dashboards session opened with it.
/// 3. **Nothing else.** No `indices_all`, no `*`, no cluster permission beyond
///    bulk. The DaemonSet path's pre-rename indices (`nginx-logs`, …) are
///    outside the pattern on purpose: they are history, written by an admin
///    login that no collector holds any more.
pub(crate) fn role_body() -> serde_json::Value {
    serde_json::json!({
        "description": "VeloxSearch: read and write on the veloxagent-* artifacts",
        "cluster_permissions": ["indices:data/write/bulk"],
        "index_permissions": [{
            "index_patterns": [velox_integrations::AGENT_GRANT_PATTERN],
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
        "description": "VeloxSearch: the collector account (in-cluster and Velox Agent)"
    })
}

/// The stored password, or `None` when no account has been made yet.
///
/// A Secret read and nothing more — this is what the hot paths call (the
/// sampler's reconcile runs every tick, per deployment), so it must not write
/// to the security plugin or every tick becomes two audit-log entries.
async fn stored_password(dep: &Deployment) -> Result<Option<String>> {
    let client = crate::k8s::client().await?;
    crate::k8s::secret_value(&client, dep.namespace(), &secret_name(dep), "password")
        .await
        .map(|v| v.filter(|p| !p.is_empty()))
}

/// Read the account's password from its Secret, or generate and store one.
async fn password(dep: &Deployment) -> Result<String> {
    if let Some(pw) = stored_password(dep).await? {
        return Ok(pw);
    }
    // The same generator the admin password uses: its alphabet is the one
    // that survives a shell, a URL, JSON and a Fluent Bit config, which is
    // exactly the set of places this value passes through.
    let pw = crate::k8s::gen_admin_password()?;
    let client = crate::k8s::client().await?;
    let name = secret_name(dep);
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
    .with_context(|| format!("storing the collector password for {}", dep.name()))?;
    Ok(pw)
}

/// The collector credential, creating the account if there is none yet.
///
/// The cheap path is one Secret read; only a deployment with no stored
/// password pays for [`ensure`]. What the in-cluster collectors and the Mesh
/// both call before handing the login on.
pub(crate) async fn credential(dep: &Deployment) -> Result<(String, String)> {
    match stored_password(dep).await? {
        Some(pw) => Ok((AGENT_USER.to_string(), pw)),
        None => ensure(dep).await,
    }
}

/// Create (or bring up to date) the collector account and return its login.
///
/// Idempotent, and ordered so a failure never leaves a usable account with the
/// wrong permissions: the role is written first, then the user that holds it.
/// The final check is not ceremony — it is the difference between "we wrote
/// some JSON" and "this login can write", and the operator finds out here
/// rather than from a 401 in a collector's log an hour later.
pub(crate) async fn ensure(dep: &Deployment) -> Result<(String, String)> {
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
        .json(&role_body())
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

    remove_legacy(dep, &http, &base, &admin_user, &admin_pw).await;
    Ok((AGENT_USER.to_string(), pw))
}

/// Delete the pre-ADR-071 account, role and Secret. Best-effort and after the
/// new account is verified: a cleanup failure must never stand between a
/// collector and a working login, and a 404 is the expected answer once a
/// deployment has been through this once.
async fn remove_legacy(
    dep: &Deployment,
    http: &reqwest::Client,
    base: &str,
    admin_user: &str,
    admin_pw: &str,
) {
    for (kind, names) in [("internalusers", LEGACY_USERS), ("roles", LEGACY_ROLES)] {
        for name in names {
            match http
                .delete(format!("{base}/_plugins/_security/api/{kind}/{name}"))
                .basic_auth(admin_user, Some(admin_pw))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    tracing::info!("removed legacy {kind}/{name} from {}", dep.name())
                }
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => {}
                Ok(r) => tracing::warn!(
                    "could not remove legacy {kind}/{name} from {}: {}",
                    dep.name(),
                    r.status()
                ),
                Err(e) => tracing::warn!(
                    "could not remove legacy {kind}/{name} from {}: {e}",
                    dep.name()
                ),
            }
        }
    }
    if let Ok(client) = crate::k8s::client().await {
        let secrets: kube::Api<k8s_openapi::api::core::v1::Secret> =
            kube::Api::namespaced(client, dep.namespace());
        match secrets
            .delete(&legacy_secret_name(dep), &Default::default())
            .await
        {
            Ok(_) => tracing::info!(
                "removed legacy Secret {} from {}",
                legacy_secret_name(dep),
                dep.namespace()
            ),
            Err(kube::Error::Api(e)) if e.code == 404 => {}
            Err(e) => tracing::warn!(
                "could not remove legacy Secret {}: {e}",
                legacy_secret_name(dep)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_is_granted_at_the_cluster_level_too() {
        // A role without this fails every flush with a 403 that looks like a
        // bad password. OpenSearch checks the bulk action as a cluster
        // permission AND per index.
        let b = role_body();
        let cluster = b["cluster_permissions"].as_array().unwrap();
        assert!(cluster
            .iter()
            .any(|p| p.as_str() == Some("indices:data/write/bulk")));
    }

    #[test]
    fn the_account_reads_writes_and_creates_but_holds_nothing_wider() {
        let b = role_body();
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
        assert!(actions.contains(&"read"), "{actions:?}");
        for forbidden in ["indices_all", "*", "all_access", "manage", "delete"] {
            assert!(!actions.contains(&forbidden), "{actions:?}");
        }
        assert!(b["cluster_permissions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p.as_str() == Some("indices:data/write/bulk")));
    }

    /// The grant is the prefix and only the prefix. Notably absent: the
    /// DaemonSet path's pre-rename names, which no collector writes any more.
    #[test]
    fn the_role_reaches_veloxagent_and_nothing_else() {
        let b = role_body();
        let pats = b["index_permissions"][0]["index_patterns"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert_eq!(pats, vec!["veloxagent-*"]);
        assert_eq!(b["index_permissions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_user_carries_the_role_directly() {
        let b = user_body("pw");
        assert_eq!(b["opendistro_security_roles"][0], AGENT_ROLE);
        assert_eq!(b["password"], "pw");
    }

    /// The legacy names are not the current ones — cleanup must never delete
    /// the account it just created.
    #[test]
    fn cleanup_cannot_touch_the_current_account() {
        assert!(!LEGACY_USERS.contains(&AGENT_USER));
        assert!(!LEGACY_ROLES.contains(&AGENT_ROLE));
        let d = Deployment::for_test("logs-a1b2", "ns", None);
        assert_ne!(secret_name(&d), legacy_secret_name(&d));
    }
}
