// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Velox Mesh — the fleet controller.
//!
//! Two faces, deliberately unalike:
//!
//! * **Agent-facing** (`/v1/*`): enrollment and OpAMP check-ins. The agent
//!   always initiates — nothing is pushed into a host, which is what lets an
//!   agent live behind NAT and a firewall that allows only outbound HTTPS.
//!   Authentication is the per-agent credential minted at enrollment.
//! * **Console-facing** (`/api/mesh/*`): what an operator does. The core owns
//!   the login, so this side trusts a service token the operator configures
//!   on both ends (`VELOX_MESH_SERVICE_TOKEN`); the Mesh never sees a user
//!   password and never issues a session. Every call names a **deployment**
//!   (ADR-072): the Mesh is one service, but its policies, agents and tokens
//!   are a deployment's, and the core has already checked the caller owns the
//!   one it names before the request gets here.
//!
//! Stateless: Postgres holds everything (`store.rs`), so replicas are
//! interchangeable behind a Service. Same licence as the core
//! (AGPL-3.0-only) but a separate workspace, binary and image (ADR-068): the
//! Mesh scales and releases on its own cadence, and a core upgrade never
//! restarts the fleet's controller.
//!
//! **The Mesh does not compile policies and does not talk to the registry.**
//! The core does both, because it is the only side that holds the signing
//! keyring, and hands the compiled bodies here to be stored and served
//! (ADR-039: signature verification is the whole security boundary, and one
//! implementation of it is one attack surface).

mod fleet;
mod store;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use fleet::{AgentFacts, ConfigStatus, Status};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_postgres::Client;
use velox_license::{entitlement, Tier};

const ADDR_VAR: &str = "VELOX_MESH_ADDR";
const DEFAULT_ADDR: &str = "127.0.0.1:4320";
const DB_VAR: &str = "VELOX_MESH_PG_URL";
const SERVICE_TOKEN_VAR: &str = "VELOX_MESH_SERVICE_TOKEN";

struct AppState {
    pg: Client,
    /// Shared with the core's console. Unset ⇒ the console face is refused
    /// outright rather than left open: a controller that manages a fleet is
    /// not something to leave unauthenticated by default.
    service_token: Option<String>,
}

type App = State<Arc<AppState>>;

/// `{"error": "..."}`, the core's convention, so one client handles both.
fn err(code: StatusCode, msg: impl std::fmt::Display) -> (StatusCode, Json<serde_json::Value>) {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}
type ApiResult<T> = Result<T, (StatusCode, Json<serde_json::Value>)>;

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// Guard the console face. Refusing when the token is unset is the point: a
/// controller that manages a fleet must not be open because someone forgot a
/// variable.
fn console_auth(st: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let Some(expected) = st.service_token.as_deref() else {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{SERVICE_TOKEN_VAR} is not set; the console API is disabled"),
        ));
    };
    match bearer(headers) {
        Some(t) if t == expected => Ok(()),
        _ => Err(err(StatusCode::UNAUTHORIZED, "service token required")),
    }
}

// ---------------------------------------------------------------------------
// Agent face
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct EnrolReq {
    token: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    attributes: serde_json::Value,
}

/// Exchange an enrollment token for this agent's own identity and credential.
async fn enrol(State(st): App, Json(req): Json<EnrolReq>) -> ApiResult<Json<store::Enrolled>> {
    let attrs = if req.attributes.is_null() {
        serde_json::json!({})
    } else {
        req.attributes
    };
    store::enrol(&st.pg, &req.token, &req.hostname, &attrs)
        .await
        .map(Json)
        .map_err(|e| match e {
            store::Error::Refused(m) => err(StatusCode::UNAUTHORIZED, m),
            other => err(StatusCode::INTERNAL_SERVER_ERROR, other),
        })
}

/// The check-in. The agent reports what it is running and how it is; the
/// answer carries a new configuration **only when the hash differs**, so a
/// steady fleet costs one small request per agent per poll and no bodies.
async fn opamp(
    State(st): App,
    headers: HeaderMap,
    Json(msg): Json<velox_opamp::AgentToServer>,
) -> ApiResult<Json<velox_opamp::ServerToAgent>> {
    let credential =
        bearer(&headers).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "credential required"))?;
    let agent = store::authenticate(&st.pg, credential)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "credential required"))?;

    // What the agent told us about the config it last applied.
    let (status, error, applied_hash) = match &msg.remote_config_status {
        Some(r) => (
            match r.status {
                velox_opamp::RemoteConfigStatuses::Applied => ConfigStatus::Applied,
                velox_opamp::RemoteConfigStatuses::Applying => ConfigStatus::Applying,
                velox_opamp::RemoteConfigStatuses::Failed => ConfigStatus::Failed,
                velox_opamp::RemoteConfigStatuses::Unset => ConfigStatus::Unset,
            },
            r.error_message.clone(),
            Some(r.last_remote_config_hash.clone()),
        ),
        None => (ConfigStatus::Unset, String::new(), None),
    };

    // Which revision that hash belongs to, so the rollout view is counted
    // from the agent's own echo rather than from what we hoped it got.
    let current = match &agent.policy_id {
        Some(p) => store::current_config(&st.pg, p).await.ok(),
        None => None,
    };
    let applied_revision = match (&applied_hash, &current) {
        (Some(h), Some((rev, _, want))) if h == want => Some(*rev),
        _ => None,
    };

    let health = msg
        .health
        .as_ref()
        .and_then(|h| serde_json::to_value(h).ok());
    // The description's two maps fold into the agent's attributes: the
    // identifying ones repeat what enrolment recorded, the non-identifying
    // ones (engine version, CPU, memory — velox-agent's usage.rs) are the
    // facts the fleet view shows per row.
    let attributes = msg.agent_description.as_ref().map(|d| {
        let mut m = serde_json::Map::new();
        for (k, v) in d
            .identifying_attributes
            .iter()
            .chain(d.non_identifying_attributes.iter())
        {
            m.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        serde_json::Value::Object(m)
    });
    store::checkin(
        &st.pg,
        &agent.instance_uid,
        &store::Checkin {
            applied_revision,
            applied_hash: applied_hash.as_deref(),
            config_status: status.as_str(),
            config_error: &error,
            health: health.as_ref(),
            attributes: attributes.as_ref(),
        },
    )
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Hand back the configuration only when it is not what the agent has.
    let remote_config = match current {
        Some((_, compiled, hash)) if applied_hash.as_deref() != Some(hash.as_str()) => {
            let map: std::collections::BTreeMap<String, String> =
                serde_json::from_value(compiled).unwrap_or_default();
            Some(velox_opamp::AgentRemoteConfig {
                config: velox_opamp::AgentConfigMap {
                    config_map: map
                        .into_iter()
                        .map(|(engine, body)| {
                            (
                                engine,
                                velox_opamp::AgentConfigObject {
                                    // Fluent Bit's classic format, which is
                                    // what the packages ship and the compiler
                                    // assembles — it has no registered media
                                    // type, and claiming YAML would be a lie
                                    // the agent could act on.
                                    body,
                                    content_type: "text/plain".into(),
                                },
                            )
                        })
                        .collect(),
                },
                config_hash: hash,
            })
        }
        _ => None,
    };

    // The data-path credential, on every check-in rather than only alongside
    // a new configuration: a credential can be rotated without the policy
    // changing, and an agent that only learned about it with a new revision
    // would keep flushing with the old password until someone edited
    // something. The agent compares a fingerprint and restarts its engine
    // only when the value actually changed.
    let velox_output_credential = match &agent.policy_id {
        Some(p) => store::policy_credential(&st.pg, p)
            .await
            .ok()
            .flatten()
            .map(|(username, password)| velox_opamp::OutputCredential { username, password }),
        None => None,
    };

    Ok(Json(velox_opamp::ServerToAgent {
        instance_uid: agent.instance_uid,
        remote_config,
        flags: vec![],
        error_response: None,
        velox_output_credential,
    }))
}

// ---------------------------------------------------------------------------
// Console face
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AgentView {
    instance_uid: String,
    hostname: String,
    policy_id: Option<String>,
    attributes: serde_json::Value,
    enrolled_at: u64,
    last_seen: u64,
    applied_revision: Option<u64>,
    applied_hash: Option<String>,
    current_revision: Option<u64>,
    config_status: String,
    config_error: String,
    status: Status,
    up: bool,
}

/// The deployment a console request is about. Flattened into every request
/// body as `deployment_ns` + `deployment`, so a call that forgets it fails to
/// deserialise rather than silently answering for everything.
#[derive(Deserialize)]
struct DepReq {
    #[serde(flatten)]
    dep: store::DeploymentRef,
}

/// `store::Error` → the console's status codes, in one place.
fn store_err(e: store::Error) -> (StatusCode, Json<serde_json::Value>) {
    match e {
        store::Error::Refused(m) => err(StatusCode::BAD_REQUEST, m),
        store::Error::NotFound => err(StatusCode::NOT_FOUND, "not found"),
        other => err(StatusCode::INTERNAL_SERVER_ERROR, other),
    }
}

async fn agent_views(pg: &Client, dep: &store::DeploymentRef) -> store::Result<Vec<AgentView>> {
    let rows = store::list_agents(pg, dep).await?;
    let revisions = store::current_revisions(pg, dep).await?;
    let now = store::now();
    Ok(rows
        .into_iter()
        .map(|a| {
            let current_revision = a.policy_id.as_ref().and_then(|p| revisions.get(p)).copied();
            // Health that was never reported is not a claim of health: a fresh
            // agent reads as updating, never as healthy.
            let engines_healthy = a
                .health
                .as_ref()
                .and_then(|h| h.get("healthy").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let status = fleet::status(
                &AgentFacts {
                    unenrolled: a.unenrolled,
                    last_seen: a.last_seen,
                    applied_revision: a.applied_revision,
                    current_revision,
                    config_status: ConfigStatus::parse(&a.config_status),
                    engines_healthy,
                },
                now,
            );
            AgentView {
                instance_uid: a.instance_uid,
                hostname: a.hostname,
                policy_id: a.policy_id,
                attributes: a.attributes,
                enrolled_at: a.enrolled_at,
                last_seen: a.last_seen,
                applied_revision: a.applied_revision,
                applied_hash: a.applied_hash,
                current_revision,
                config_status: a.config_status,
                config_error: a.config_error,
                status,
                up: status.is_up(),
            }
        })
        .collect())
}

async fn agents(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<DepReq>,
) -> ApiResult<Json<Vec<AgentView>>> {
    console_auth(&st, &headers)?;
    agent_views(&st.pg, &req.dep)
        .await
        .map(Json)
        .map_err(store_err)
}

#[derive(Serialize)]
struct Overview {
    onboarding: fleet::OnboardingStep,
    policies: Vec<store::PolicyRow>,
    agents: Vec<AgentView>,
    rollout: std::collections::BTreeMap<String, fleet::Rollout>,
    /// The installation's seats, and how many of them the whole fleet uses —
    /// across every deployment, because the free tier is per installation.
    seats: u32,
    enrolled_total: u64,
    tier: &'static str,
}

/// Everything the deployment's Mesh tab needs on first paint, in one call:
/// where the operator is in the guided onboarding, the deployment's policies,
/// its fleet and each policy's rollout.
async fn overview(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<DepReq>,
) -> ApiResult<Json<Overview>> {
    console_auth(&st, &headers)?;
    let dep = &req.dep;
    // Development databases from before ADR-072 hold rows with the name and
    // no namespace; the first time the core asks about this deployment it
    // tells us the namespace, and from then on this is a no-op UPDATE.
    store::adopt_legacy(&st.pg, dep).await.map_err(store_err)?;
    let policies = store::list_policies(&st.pg, dep).await.map_err(store_err)?;
    let agents = agent_views(&st.pg, dep).await.map_err(store_err)?;
    let enrolled_total = store::enrolled_total(&st.pg).await.map_err(store_err)?;

    let mut rollout = std::collections::BTreeMap::new();
    for p in &policies {
        let statuses: Vec<Status> = agents
            .iter()
            .filter(|a| a.policy_id.as_deref() == Some(p.id.as_str()))
            .map(|a| a.status)
            .collect();
        rollout.insert(p.id.clone(), fleet::rollout(&statuses));
    }

    let ent = entitlement(None, store::now());
    Ok(Json(Overview {
        // Reachable by definition: this request arrived.
        onboarding: fleet::onboarding(
            true,
            policies.len(),
            &agents.iter().map(|a| a.status).collect::<Vec<_>>(),
        ),
        rollout,
        policies,
        agents,
        seats: ent.seats,
        enrolled_total,
        tier: match ent.tier {
            Tier::Free => "free",
            Tier::Licensed => "licensed",
            Tier::Grace { .. } => "grace",
            Tier::ReadOnly { .. } => "read-only",
        },
    }))
}

#[derive(Deserialize)]
struct PutPolicyReq {
    #[serde(flatten)]
    dep: store::DeploymentRef,
    /// The core's tenant for this deployment; the admin's deployments carry
    /// the schema default.
    #[serde(default)]
    tenant_id: Option<String>,
    id: String,
    name: String,
    /// The policy document, as `velox_policy::Policy` serialises it.
    body: serde_json::Value,
    /// Engine name → configuration body, compiled by the core.
    compiled: serde_json::Value,
    compiled_hash: String,
}

/// Create a policy or add a revision to it. **An update is a new revision**,
/// never a rewrite: agents on the old one show as updating until they echo
/// the new hash back, which is how a rollout is observed rather than assumed.
async fn put_policy(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<PutPolicyReq>,
) -> ApiResult<Json<serde_json::Value>> {
    console_auth(&st, &headers)?;
    let revision = store::put_policy(
        &st.pg,
        &req.dep,
        req.tenant_id.as_deref().unwrap_or("default"),
        &req.id,
        &req.name,
        &req.body,
        &req.compiled,
        &req.compiled_hash,
    )
    .await
    .map_err(store_err)?;
    Ok(Json(serde_json::json!({ "revision": revision })))
}

#[derive(Deserialize)]
struct PutCredentialReq {
    #[serde(flatten)]
    dep: store::DeploymentRef,
    policy_id: String,
    username: String,
    password: String,
}

/// Store the OpenSearch login the agents on a policy write with.
///
/// Written by the core, which is the side that holds the deployment's admin
/// credentials and creates the account. The Mesh is a courier here: it does
/// not mint this credential, cannot verify it, and serves it only to an agent
/// that authenticated on this policy.
async fn put_policy_credential(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<PutCredentialReq>,
) -> ApiResult<Json<serde_json::Value>> {
    console_auth(&st, &headers)?;
    store::put_policy_credential(
        &st.pg,
        &req.dep,
        &req.policy_id,
        &req.username,
        &req.password,
    )
    .await
    .map_err(store_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// The deployment is gone: drop its policies and tokens, unenrol its agents
/// (ADR-072 §4). Called by the core from `delete_cluster`, best-effort there,
/// so this must be idempotent — a second call finds nothing and says so.
async fn forget_deployment(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<DepReq>,
) -> ApiResult<Json<store::Forgotten>> {
    console_auth(&st, &headers)?;
    store::forget_deployment(&st.pg, &req.dep)
        .await
        .map(Json)
        .map_err(store_err)
}

/// The policy document of the current revision, for the editor. The console
/// edits what is stored, never what it happens to have in a tab.
async fn policy(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<PolicyIdReq>,
) -> ApiResult<Json<serde_json::Value>> {
    console_auth(&st, &headers)?;
    let (revision, body) = store::policy_body(&st.pg, &req.dep, &req.policy_id)
        .await
        .map_err(store_err)?;
    Ok(Json(
        serde_json::json!({ "revision": revision, "body": body }),
    ))
}

async fn policies(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<DepReq>,
) -> ApiResult<Json<Vec<store::PolicyRow>>> {
    console_auth(&st, &headers)?;
    store::list_policies(&st.pg, &req.dep)
        .await
        .map(Json)
        .map_err(store_err)
}

#[derive(Deserialize)]
struct PolicyIdReq {
    #[serde(flatten)]
    dep: store::DeploymentRef,
    policy_id: String,
    #[serde(default)]
    expires_at: Option<u64>,
}

async fn mint_token(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<PolicyIdReq>,
) -> ApiResult<Json<serde_json::Value>> {
    console_auth(&st, &headers)?;
    let token = store::mint_enrollment_token(&st.pg, &req.dep, &req.policy_id, req.expires_at)
        .await
        .map_err(store_err)?;
    // Shown once. Only the hash is stored, so a lost token is reissued.
    Ok(Json(serde_json::json!({ "token": token })))
}

#[derive(Deserialize)]
struct AssignReq {
    #[serde(flatten)]
    dep: store::DeploymentRef,
    instance_uid: String,
    policy_id: String,
}

/// Move an agent to another policy of its deployment. There is no push: the
/// next check-in sees a different hash and applies it. Another deployment's
/// policy is a 400 that says to re-enrol (ADR-072 §3).
async fn assign(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<AssignReq>,
) -> ApiResult<StatusCode> {
    console_auth(&st, &headers)?;
    store::assign_policy(&st.pg, &req.dep, &req.instance_uid, &req.policy_id)
        .await
        .map(|_| StatusCode::OK)
        .map_err(store_err)
}

/// Delete one of the deployment's policies; refused (400, with the count)
/// while a live agent runs it.
async fn delete_policy(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<PolicyIdReq>,
) -> ApiResult<StatusCode> {
    console_auth(&st, &headers)?;
    store::delete_policy(&st.pg, &req.dep, &req.policy_id)
        .await
        .map(|_| StatusCode::OK)
        .map_err(store_err)
}

#[derive(Deserialize)]
struct AgentIdReq {
    #[serde(flatten)]
    dep: store::DeploymentRef,
    instance_uid: String,
}

async fn unenrol(
    State(st): App,
    headers: HeaderMap,
    Json(req): Json<AgentIdReq>,
) -> ApiResult<StatusCode> {
    console_auth(&st, &headers)?;
    store::unenrol(&st.pg, &req.dep, &req.instance_uid)
        .await
        .map(|_| StatusCode::OK)
        .map_err(store_err)
}

#[tokio::main]
async fn main() {
    let addr = std::env::var(ADDR_VAR).unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let Ok(db) = std::env::var(DB_VAR) else {
        eprintln!("velox-mesh: {DB_VAR} is required (Postgres is the Mesh's only state)");
        std::process::exit(1);
    };

    // Schema to head BEFORE serving, and a failure exits — the same
    // fail-closed bring-up the core does for its own Postgres.
    let pg = match store::connect(&db).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("velox-mesh: cannot bring up the database: {e}");
            std::process::exit(1);
        }
    };

    let service_token = std::env::var(SERVICE_TOKEN_VAR)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if service_token.is_none() {
        eprintln!(
            "velox-mesh: {SERVICE_TOKEN_VAR} is unset — the console API will refuse every call"
        );
    }

    let ent = entitlement(None, store::now());
    eprintln!(
        "velox-mesh {} — tier {}, {} seats",
        env!("CARGO_PKG_VERSION"),
        match ent.tier {
            Tier::Free => "free",
            Tier::Licensed => "licensed",
            Tier::Grace { .. } => "grace",
            Tier::ReadOnly { .. } => "read-only",
        },
        ent.seats
    );

    let state = Arc::new(AppState { pg, service_token });
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // agent face
        .route("/v1/enroll", post(enrol))
        .route(velox_opamp::DEFAULT_PATH, post(opamp))
        // console face
        // Every console route is a POST with the deployment pair in the body
        // (ADR-072) — there is no "all deployments" read on this face.
        .route("/api/mesh/overview", post(overview))
        .route("/api/mesh/agents", post(agents))
        .route("/api/mesh/policies", post(policies))
        .route("/api/mesh/forget_deployment", post(forget_deployment))
        .route("/api/mesh/policy", post(policy))
        .route("/api/mesh/put_policy", post(put_policy))
        .route(
            "/api/mesh/put_policy_credential",
            post(put_policy_credential),
        )
        .route("/api/mesh/enrollment_token", post(mint_token))
        .route("/api/mesh/assign_policy", post(assign))
        .route("/api/mesh/unenrol", post(unenrol))
        .route("/api/mesh/delete_policy", post(delete_policy))
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("velox-mesh: cannot listen on {addr}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("velox-mesh: listening on http://{addr}");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("velox-mesh: server error: {e}");
        std::process::exit(1);
    }
}
