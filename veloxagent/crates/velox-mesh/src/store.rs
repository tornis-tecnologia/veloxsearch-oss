// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Postgres: the Mesh's only state (ARCHITECTURE.md §4.1).
//!
//! Replicas are interchangeable behind a Service, so nothing that matters
//! lives in memory. The driver is bare `tokio-postgres` and the migration
//! runner is hand-rolled, both deliberately and both mirroring the core's
//! `db.rs`: the query layer is a later, separate decision, and the ledger
//! tracks migration *versions*, not content, so applied files are never
//! edited — a change is a new file.
//!
//! ## Secrets are hashed, never stored
//!
//! Two secrets exist here and neither is kept:
//!
//! * an **enrollment token**, minted per policy, reusable and revocable — the
//!   operator pastes it into the install command;
//! * a **per-agent credential**, minted at enrollment, which the agent sends
//!   on every check-in.
//!
//! Only `sha256` of each is stored, the same way the core keeps session and
//! password-reset tokens. They are separate on purpose: Fleet separates the
//! communication key from the output key, and here revoking one host must not
//! revoke the fleet.

use aws_lc_rs::rand::{SecureRandom as _, SystemRandom};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use tokio_postgres::{Client, NoTls};

/// Ordered, embedded migrations. Append-only, exactly like the core's.
const MIGRATIONS: &[(&str, &str)] = &[
    ("001_mesh", include_str!("../../../migrations/001_mesh.sql")),
    (
        "002_policy_credentials",
        include_str!("../../../migrations/002_policy_credentials.sql"),
    ),
    (
        "003_deployment_scope",
        include_str!("../../../migrations/003_deployment_scope.sql"),
    ),
];

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Db(#[from] tokio_postgres::Error),
    #[error("{0}")]
    Refused(String),
    #[error("not found")]
    NotFound,
}

/// Connect and bring the schema to head. Returns the client; the connection
/// task is spawned and owns the socket.
///
/// Migrations run **before** anything is served and a failure is fatal to the
/// caller — the same fail-closed bring-up the core does. A Mesh serving an
/// old schema would hand agents configuration it cannot record the result of.
pub async fn connect(url: &str) -> Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("velox-mesh: postgres connection closed: {e}");
        }
    });
    migrate(&client).await?;
    Ok(client)
}

async fn migrate(pg: &Client) -> Result<usize> {
    pg.batch_execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    text PRIMARY KEY,
             applied_at timestamptz NOT NULL DEFAULT now()
         )",
    )
    .await?;
    let applied: std::collections::HashSet<String> = pg
        .query("SELECT version FROM schema_migrations", &[])
        .await?
        .into_iter()
        .map(|r| r.get::<_, String>(0))
        .collect();
    let mut n = 0;
    for (version, sql) in MIGRATIONS {
        if applied.contains(*version) {
            continue;
        }
        pg.batch_execute(sql).await?;
        pg.execute(
            "INSERT INTO schema_migrations (version) VALUES ($1)",
            &[version],
        )
        .await?;
        eprintln!("velox-mesh: applied migration {version}");
        n += 1;
    }
    Ok(n)
}

/// Unix seconds. One representation for every time in this product.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A URL-safe secret from the OS CSPRNG, through the provider this product
/// already links. 32 bytes: longer than anything that needs guessing twice.
pub fn mint_secret() -> String {
    let mut buf = [0u8; 32];
    SystemRandom::new()
        .fill(&mut buf)
        .expect("the OS CSPRNG is not optional");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// What is stored in place of a secret.
pub fn hash(secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// The deployment every row belongs to (ADR-072)
// ---------------------------------------------------------------------------

/// The pair the core's `Deployment` capability is built from. Every console
/// request carries one, and every query here filters on it: the Mesh is one
/// service, but what it stores is a deployment's.
///
/// The Mesh does not — cannot — check that the caller owns this pair. The
/// core does, with `Scope::require`, before the request leaves it, and the
/// service token is what says the caller *is* the core. Two layers, one
/// decision each.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct DeploymentRef {
    pub deployment_ns: String,
    pub deployment: String,
}

impl DeploymentRef {
    fn check(&self) -> Result<()> {
        if self.deployment_ns.is_empty() || self.deployment.is_empty() {
            return Err(Error::Refused(
                "a deployment is a namespace and a name; both are required".into(),
            ));
        }
        Ok(())
    }
}

/// Does this policy belong to this deployment? `NotFound` otherwise — the
/// same answer as "no such policy", on purpose: a caller asking about another
/// deployment's policy learns nothing from the shape of the refusal, which is
/// the anti-enumeration property the core's `Scope::resolve` has.
async fn policy_belongs(pg: &Client, dep: &DeploymentRef, policy_id: &str) -> Result<()> {
    pg.query_opt(
        "SELECT 1 FROM mesh_policies WHERE id = $1 AND deployment_ns = $2 AND deployment = $3",
        &[&policy_id, &dep.deployment_ns, &dep.deployment],
    )
    .await?
    .map(|_| ())
    .ok_or(Error::NotFound)
}

/// Adopt rows the back-fill could not finish (`003_deployment_scope.sql`):
/// policies and agents of this deployment NAME whose namespace is still
/// unknown. Only the core knows the namespace, and it tells us the first time
/// it asks about the deployment. Idempotent and a no-op forever after the
/// first call; a development-database convenience that a released Mesh never
/// exercises.
pub async fn adopt_legacy(pg: &Client, dep: &DeploymentRef) -> Result<u64> {
    dep.check()?;
    let a = pg
        .execute(
            "UPDATE mesh_policies SET deployment_ns = $1 WHERE deployment_ns = '' AND deployment = $2",
            &[&dep.deployment_ns, &dep.deployment],
        )
        .await?;
    let b = pg
        .execute(
            "UPDATE mesh_agents SET deployment_ns = $1 WHERE deployment_ns = '' AND deployment = $2",
            &[&dep.deployment_ns, &dep.deployment],
        )
        .await?;
    Ok(a + b)
}

/// What forgetting a deployment touched, for the core's log line.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Forgotten {
    pub policies: u64,
    pub agents: u64,
}

/// The deployment is being deleted: drop its policies (revisions, tokens and
/// credentials cascade) and mark its agents unenrolled. The agents stop
/// receiving configuration on their next check-in; Fluent Bit stays on the
/// host with its last config, which is the operator's to remove (ADR-072 §4).
///
/// Agents first, so an agent checking in between the two statements is
/// refused rather than handed a configuration for a policy about to vanish.
pub async fn forget_deployment(pg: &Client, dep: &DeploymentRef) -> Result<Forgotten> {
    dep.check()?;
    let agents = pg
        .execute(
            "UPDATE mesh_agents SET unenrolled = true, policy_id = NULL
              WHERE deployment_ns = $1 AND deployment = $2 AND NOT unenrolled",
            &[&dep.deployment_ns, &dep.deployment],
        )
        .await?;
    let policies = pg
        .execute(
            "DELETE FROM mesh_policies WHERE deployment_ns = $1 AND deployment = $2",
            &[&dep.deployment_ns, &dep.deployment],
        )
        .await?;
    Ok(Forgotten { policies, agents })
}

// ---------------------------------------------------------------------------
// Policies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct PolicyRow {
    pub id: String,
    pub name: String,
    pub current_revision: u64,
    pub updated_at: u64,
    /// The integration ids the current revision activates, in policy order.
    /// Read off the revision body so the Data tab can say "hosts collect this
    /// too, under policy X" and the core can refuse to uninstall an
    /// integration a policy still writes through (ADR-072 §6) — without
    /// either side fetching every policy document.
    pub integrations: Vec<String>,
}

/// Create a policy at revision 1, or add revision N+1 to an existing one.
/// **Revisions are immutable**: an edit never rewrites a row, it inserts the
/// next one and moves the pointer. That is what makes a rollout observable
/// and a rollback a pointer move.
///
/// `compiled` is engine name → configuration body, compiled by the core.
///
/// A policy belongs to one deployment for life: a second save naming another
/// deployment is refused, not moved. Policy ids are global (they are what an
/// enrollment token points at), so the refusal also closes the door on one
/// deployment taking over another's policy by reusing its id.
pub async fn put_policy(
    pg: &Client,
    dep: &DeploymentRef,
    tenant_id: &str,
    id: &str,
    name: &str,
    body: &serde_json::Value,
    compiled: &serde_json::Value,
    compiled_hash: &str,
) -> Result<u64> {
    dep.check()?;
    let t = now() as i64;
    let existing = pg
        .query_opt(
            "SELECT current_revision, deployment_ns, deployment FROM mesh_policies WHERE id = $1",
            &[&id],
        )
        .await?
        .map(|r| {
            (
                r.get::<_, i64>(0),
                DeploymentRef {
                    deployment_ns: r.get(1),
                    deployment: r.get(2),
                },
            )
        });
    let revision = match existing {
        None => {
            pg.execute(
                "INSERT INTO mesh_policies
                     (id, tenant_id, deployment_ns, deployment, name, current_revision, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, 1, $6, $6)",
                &[&id, &tenant_id, &dep.deployment_ns, &dep.deployment, &name, &t],
            )
            .await?;
            1i64
        }
        Some((_, owner)) if owner != *dep => {
            // Not `NotFound`: the caller is about to CREATE, and "created
            // nothing, here is why" is the honest answer. The owner is not
            // named — that would be enumeration.
            return Err(Error::Refused(format!(
                "policy {id} already exists on another deployment; pick another id"
            )));
        }
        Some((current, _)) => {
            let next = current + 1;
            pg.execute(
                "UPDATE mesh_policies SET current_revision = $2, name = $3, updated_at = $4 WHERE id = $1",
                &[&id, &next, &name, &t],
            )
            .await?;
            next
        }
    };
    pg.execute(
        "INSERT INTO mesh_policy_revisions
             (policy_id, revision, tenant_id, body, compiled, compiled_hash, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        &[
            &id,
            &revision,
            &tenant_id,
            body,
            compiled,
            &compiled_hash,
            &t,
        ],
    )
    .await?;
    Ok(revision as u64)
}

/// The deployment's policies, by name.
pub async fn list_policies(pg: &Client, dep: &DeploymentRef) -> Result<Vec<PolicyRow>> {
    dep.check()?;
    Ok(pg
        .query(
            "SELECT p.id, p.name, p.current_revision, p.updated_at,
                    COALESCE((SELECT array_agg(a->>'package_id' ORDER BY ord)
                                FROM mesh_policy_revisions r,
                                     jsonb_array_elements(r.body->'activations')
                                       WITH ORDINALITY AS t(a, ord)
                               WHERE r.policy_id = p.id AND r.revision = p.current_revision),
                             '{}'::text[])
               FROM mesh_policies p
              WHERE p.deployment_ns = $1 AND p.deployment = $2 ORDER BY p.name",
            &[&dep.deployment_ns, &dep.deployment],
        )
        .await?
        .into_iter()
        .map(|r| PolicyRow {
            id: r.get(0),
            name: r.get(1),
            current_revision: r.get::<_, i64>(2) as u64,
            updated_at: r.get::<_, i64>(3) as u64,
            integrations: r.get(4),
        })
        .collect())
}

/// The policy document of the current revision, for the editor.
pub async fn policy_body(
    pg: &Client,
    dep: &DeploymentRef,
    id: &str,
) -> Result<(u64, serde_json::Value)> {
    dep.check()?;
    let row = pg
        .query_opt(
            "SELECT r.revision, r.body
               FROM mesh_policy_revisions r
               JOIN mesh_policies p ON p.id = r.policy_id AND p.current_revision = r.revision
              WHERE p.id = $1 AND p.deployment_ns = $2 AND p.deployment = $3",
            &[&id, &dep.deployment_ns, &dep.deployment],
        )
        .await?
        .ok_or(Error::NotFound)?;
    Ok((row.get::<_, i64>(0) as u64, row.get(1)))
}

/// What an agent on this policy should be running right now.
pub async fn current_config(
    pg: &Client,
    policy_id: &str,
) -> Result<(u64, serde_json::Value, String)> {
    let row = pg
        .query_opt(
            "SELECT r.revision, r.compiled, r.compiled_hash
               FROM mesh_policy_revisions r
               JOIN mesh_policies p ON p.id = r.policy_id AND p.current_revision = r.revision
              WHERE p.id = $1",
            &[&policy_id],
        )
        .await?
        .ok_or(Error::NotFound)?;
    Ok((row.get::<_, i64>(0) as u64, row.get(1), row.get(2)))
}

/// The OpenSearch login the agents on this policy write with. Replaced
/// whole on every save: there is one current credential, and keeping older
/// ones would mean keeping passwords that nothing can use.
pub async fn put_policy_credential(
    pg: &Client,
    dep: &DeploymentRef,
    policy_id: &str,
    username: &str,
    password: &str,
) -> Result<()> {
    if username.is_empty() || password.is_empty() {
        return Err(Error::Refused(
            "a collector credential needs both a username and a password".into(),
        ));
    }
    dep.check()?;
    policy_belongs(pg, dep, policy_id).await?;
    pg.execute(
        "INSERT INTO mesh_policy_credentials (policy_id, username, password, updated_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (policy_id) DO UPDATE
            SET username = EXCLUDED.username,
                password = EXCLUDED.password,
                updated_at = EXCLUDED.updated_at",
        &[&policy_id, &username, &password, &(now() as i64)],
    )
    .await?;
    Ok(())
}

/// What an agent on this policy is given on check-in. `None` when no
/// credential has been stored yet — an older policy, or a core that could not
/// reach its deployment — and the agent then reports a configuration it
/// cannot authenticate rather than guessing.
pub async fn policy_credential(pg: &Client, policy_id: &str) -> Result<Option<(String, String)>> {
    Ok(pg
        .query_opt(
            "SELECT username, password FROM mesh_policy_credentials WHERE policy_id = $1",
            &[&policy_id],
        )
        .await?
        .map(|r| (r.get(0), r.get(1))))
}

// ---------------------------------------------------------------------------
// Enrollment
// ---------------------------------------------------------------------------

/// Mint a token for a policy. The secret is returned **once**; only its hash
/// is kept, so a lost token is reissued rather than recovered.
pub async fn mint_enrollment_token(
    pg: &Client,
    dep: &DeploymentRef,
    policy_id: &str,
    expires_at: Option<u64>,
) -> Result<String> {
    dep.check()?;
    policy_belongs(pg, dep, policy_id).await?;
    let secret = mint_secret();
    let id = mint_secret();
    pg.execute(
        "INSERT INTO mesh_enrollment_tokens (id, token_hash, policy_id, created_at, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
        &[
            &&id[..22],
            &hash(&secret),
            &policy_id,
            &(now() as i64),
            &expires_at.map(|e| e as i64),
        ],
    )
    .await?;
    Ok(secret)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Enrolled {
    pub instance_uid: String,
    pub credential: String,
    pub policy_id: String,
}

/// Exchange an enrollment token for an identity and a credential of this
/// agent's own. An expired or revoked token is refused with the same words as
/// an unknown one: a caller holding a wrong token learns nothing about which
/// kind of wrong it is.
pub async fn enrol(
    pg: &Client,
    token: &str,
    hostname: &str,
    attributes: &serde_json::Value,
) -> Result<Enrolled> {
    let refused = || Error::Refused("enrollment token is not valid".into());
    // The agent inherits the policy's deployment HERE and keeps it for life
    // (ADR-072 §3): the pair is copied, not derived, so a deleted policy does
    // not orphan the agent out of its deployment's fleet.
    let row = pg
        .query_opt(
            "SELECT t.policy_id, t.expires_at, t.revoked,
                    p.tenant_id, p.deployment_ns, p.deployment
               FROM mesh_enrollment_tokens t
               JOIN mesh_policies p ON p.id = t.policy_id
              WHERE t.token_hash = $1",
            &[&hash(token)],
        )
        .await?
        .ok_or_else(refused)?;
    let policy_id: String = row.get(0);
    let expires_at: Option<i64> = row.get(1);
    if row.get::<_, bool>(2) || expires_at.is_some_and(|e| (e as u64) < now()) {
        return Err(refused());
    }
    let tenant_id: String = row.get(3);
    let deployment_ns: String = row.get(4);
    let deployment: String = row.get(5);

    let instance_uid = mint_secret();
    let credential = mint_secret();
    let t = now() as i64;
    pg.execute(
        "INSERT INTO mesh_agents
            (instance_uid, tenant_id, deployment_ns, deployment, policy_id, credential_hash,
             hostname, attributes, enrolled_at, last_seen)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)",
        &[
            &instance_uid,
            &tenant_id,
            &deployment_ns,
            &deployment,
            &policy_id,
            &hash(&credential),
            &hostname,
            attributes,
            &t,
        ],
    )
    .await?;
    Ok(Enrolled {
        instance_uid,
        credential,
        policy_id,
    })
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

/// One agent as stored. The *status* is not here: it is derived at read time
/// from these facts (`fleet::status`).
#[derive(Debug, Clone)]
pub struct AgentRow {
    pub instance_uid: String,
    pub hostname: String,
    pub policy_id: Option<String>,
    pub attributes: serde_json::Value,
    pub enrolled_at: u64,
    pub last_seen: u64,
    pub applied_revision: Option<u64>,
    pub applied_hash: Option<String>,
    pub config_status: String,
    pub config_error: String,
    pub health: Option<serde_json::Value>,
    pub unenrolled: bool,
}

fn agent_from_row(r: &tokio_postgres::Row) -> AgentRow {
    AgentRow {
        instance_uid: r.get(0),
        hostname: r.get(1),
        policy_id: r.get(2),
        attributes: r.get(3),
        enrolled_at: r.get::<_, i64>(4) as u64,
        last_seen: r.get::<_, i64>(5) as u64,
        applied_revision: r.get::<_, Option<i64>>(6).map(|v| v as u64),
        applied_hash: r.get(7),
        config_status: r.get(8),
        config_error: r.get(9),
        health: r.get(10),
        unenrolled: r.get(11),
    }
}

const AGENT_COLS: &str = "instance_uid, hostname, policy_id, attributes, enrolled_at, last_seen, \
                          applied_revision, applied_hash, config_status, config_error, health, unenrolled";

/// Authenticate a check-in by its credential. Returns the agent, or `None` —
/// a wrong credential and an unenrolled agent are the same answer.
pub async fn authenticate(pg: &Client, credential: &str) -> Result<Option<AgentRow>> {
    Ok(pg
        .query_opt(
            &format!("SELECT {AGENT_COLS} FROM mesh_agents WHERE credential_hash = $1 AND NOT unenrolled"),
            &[&hash(credential)],
        )
        .await?
        .as_ref()
        .map(agent_from_row))
}

/// Record a check-in: what the agent says it is running, and how it is.
pub async fn checkin(
    pg: &Client,
    instance_uid: &str,
    applied_revision: Option<u64>,
    applied_hash: Option<&str>,
    config_status: &str,
    config_error: &str,
    health: Option<&serde_json::Value>,
) -> Result<()> {
    pg.execute(
        "UPDATE mesh_agents SET
             last_seen = $2,
             applied_revision = COALESCE($3, applied_revision),
             applied_hash = COALESCE($4, applied_hash),
             config_status = $5,
             config_error = $6,
             health = COALESCE($7, health)
         WHERE instance_uid = $1",
        &[
            &instance_uid,
            &(now() as i64),
            &applied_revision.map(|v| v as i64),
            &applied_hash,
            &config_status,
            &config_error,
            &health,
        ],
    )
    .await?;
    Ok(())
}

/// The deployment's fleet, most recent check-in first.
pub async fn list_agents(pg: &Client, dep: &DeploymentRef) -> Result<Vec<AgentRow>> {
    dep.check()?;
    Ok(pg
        .query(
            &format!(
                "SELECT {AGENT_COLS} FROM mesh_agents
                  WHERE deployment_ns = $1 AND deployment = $2 ORDER BY last_seen DESC"
            ),
            &[&dep.deployment_ns, &dep.deployment],
        )
        .await?
        .iter()
        .map(agent_from_row)
        .collect())
}

/// The current revision of each of the deployment's policies, for deriving
/// each agent's status without a query per row.
pub async fn current_revisions(pg: &Client, dep: &DeploymentRef) -> Result<BTreeMap<String, u64>> {
    dep.check()?;
    Ok(pg
        .query(
            "SELECT id, current_revision FROM mesh_policies WHERE deployment_ns = $1 AND deployment = $2",
            &[&dep.deployment_ns, &dep.deployment],
        )
        .await?
        .into_iter()
        .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1) as u64))
        .collect())
}

/// How many agents the installation manages, across every deployment: the
/// seat count is the installation's (ADR-068 §5), not a deployment's.
pub async fn enrolled_total(pg: &Client) -> Result<u64> {
    let n: i64 = pg
        .query_one("SELECT count(*) FROM mesh_agents WHERE NOT unenrolled", &[])
        .await?
        .get(0);
    Ok(n as u64)
}

/// Move an agent to another policy **of the same deployment**. The next
/// check-in sees a different hash and applies it — there is no push, and no
/// re-enrollment.
///
/// A policy of another deployment is refused, by name: a credential issued
/// for one deployment's data path must never start writing into another's,
/// and the way to put a host on another deployment is to enrol it there
/// (ADR-072 §3). The agent is looked up first so "no such agent" and "wrong
/// deployment" stay distinct errors the operator can act on.
pub async fn assign_policy(
    pg: &Client,
    dep: &DeploymentRef,
    instance_uid: &str,
    policy_id: &str,
) -> Result<()> {
    dep.check()?;
    pg.query_opt(
        "SELECT 1 FROM mesh_agents
          WHERE instance_uid = $1 AND deployment_ns = $2 AND deployment = $3 AND NOT unenrolled",
        &[&instance_uid, &dep.deployment_ns, &dep.deployment],
    )
    .await?
    .ok_or(Error::NotFound)?;
    if policy_belongs(pg, dep, policy_id).await.is_err() {
        return Err(Error::Refused(format!(
            "policy {policy_id} is not a policy of deployment {}; to move a host to another \
             deployment, enrol it there",
            dep.deployment
        )));
    }
    pg.execute(
        "UPDATE mesh_agents SET policy_id = $2 WHERE instance_uid = $1",
        &[&instance_uid, &policy_id],
    )
    .await?;
    Ok(())
}

pub async fn unenrol(pg: &Client, dep: &DeploymentRef, instance_uid: &str) -> Result<()> {
    dep.check()?;
    let n = pg
        .execute(
            "UPDATE mesh_agents SET unenrolled = true
              WHERE instance_uid = $1 AND deployment_ns = $2 AND deployment = $3",
            &[&instance_uid, &dep.deployment_ns, &dep.deployment],
        )
        .await?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A secret is URL-safe (it travels in a command line and a header) and
    /// long enough not to be guessed; two mints never collide.
    #[test]
    fn secrets_are_url_safe_and_unique() {
        let a = mint_secret();
        let b = mint_secret();
        assert_ne!(a, b);
        assert!(a.len() >= 43, "32 bytes of base64url");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "not URL-safe: {a}"
        );
    }

    /// What is stored is the digest, and it is stable, fixed-width hex — so a
    /// unique index on it is meaningful and the secret is unrecoverable.
    #[test]
    fn the_hash_is_stable_hex_and_not_the_secret() {
        let s = mint_secret();
        let h = hash(&s);
        assert_eq!(h, hash(&s));
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(h, s);
        assert_ne!(hash("a"), hash("b"));
    }

    /// The migration is embedded, so a binary with no files beside it still
    /// brings its schema to head.
    #[test]
    fn the_schema_is_embedded_in_the_binary() {
        let versions: Vec<&str> = MIGRATIONS.iter().map(|(v, _)| *v).collect();
        assert_eq!(
            versions,
            vec!["001_mesh", "002_policy_credentials", "003_deployment_scope"]
        );
        let sql: String = MIGRATIONS.iter().map(|(_, s)| *s).collect();
        for table in [
            "mesh_policies",
            "mesh_policy_revisions",
            "mesh_enrollment_tokens",
            "mesh_agents",
            "mesh_policy_credentials",
        ] {
            assert!(sql.contains(table), "{table} is not in the migration");
        }
        // Secrets the Mesh only has to VERIFY are columns of hashes.
        assert!(sql.contains("token_hash"));
        assert!(sql.contains("credential_hash"));
        assert!(
            !sql.contains("token text NOT NULL"),
            "a raw token column would be a leak"
        );
    }

    /// The one secret kept in the clear, and the reason, in the schema where
    /// the next person will read it. The Mesh must *hand this one over* — an
    /// agent cannot authenticate to OpenSearch with a hash — so the limit on
    /// the damage is the account's permissions, not the storage. If this test
    /// ever has to be relaxed, that argument has changed and someone should
    /// have to say so out loud.
    #[test]
    fn the_only_plaintext_secret_is_the_output_credential_and_it_is_explained() {
        let (_, sql) = MIGRATIONS[1];
        assert!(sql.contains("password   text NOT NULL"));
        assert!(
            sql.contains("hand") && sql.contains("hash"),
            "the migration must say WHY this one is not hashed"
        );
    }

    /// Every row the console can see carries the deployment pair, on BOTH
    /// tables (ADR-072): an agent derives nothing through its policy, because
    /// the policy can be deleted under it. The back-fill reads the name out of
    /// the revision body — the only place it was stored before.
    #[test]
    fn policies_and_agents_both_carry_the_deployment_pair() {
        let (_, sql) = MIGRATIONS[2];
        for table in ["mesh_policies", "mesh_agents"] {
            let alter = format!("ALTER TABLE {table}");
            let i = sql
                .find(&alter)
                .unwrap_or_else(|| panic!("{table} is not altered"));
            let block = &sql[i..sql[i..].find(';').map(|j| i + j).unwrap()];
            assert!(block.contains("deployment_ns"), "{table}: no deployment_ns");
            assert!(
                block.contains("deployment    text"),
                "{table}: no deployment"
            );
        }
        assert!(
            sql.contains("body->'outputs'->0->>'deployment'"),
            "the back-fill must recover the name from the revision body"
        );
    }

    /// An empty half of the pair is refused before any SQL runs: a query
    /// filtered on `deployment_ns = ''` would match exactly the rows the
    /// back-fill left unadopted, which is how a scope check turns into a leak.
    #[test]
    fn an_incomplete_pair_is_refused() {
        for (ns, name) in [("", "a"), ("ns", ""), ("", "")] {
            let d = DeploymentRef {
                deployment_ns: ns.into(),
                deployment: name.into(),
            };
            assert!(matches!(d.check(), Err(Error::Refused(_))), "{d:?} passed");
        }
        let ok = DeploymentRef {
            deployment_ns: "legacy".into(),
            deployment: "tornis-096o".into(),
        };
        assert!(ok.check().is_ok());
    }
}
