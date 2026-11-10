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
// Policies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct PolicyRow {
    pub id: String,
    pub name: String,
    pub current_revision: u64,
    pub updated_at: u64,
}

/// Create a policy at revision 1, or add revision N+1 to an existing one.
/// **Revisions are immutable**: an edit never rewrites a row, it inserts the
/// next one and moves the pointer. That is what makes a rollout observable
/// and a rollback a pointer move.
///
/// `compiled` is engine name → configuration body, compiled by the core.
pub async fn put_policy(
    pg: &Client,
    id: &str,
    name: &str,
    body: &serde_json::Value,
    compiled: &serde_json::Value,
    compiled_hash: &str,
) -> Result<u64> {
    let t = now() as i64;
    let existing: Option<i64> = pg
        .query_opt(
            "SELECT current_revision FROM mesh_policies WHERE id = $1",
            &[&id],
        )
        .await?
        .map(|r| r.get(0));
    let revision = match existing {
        None => {
            pg.execute(
                "INSERT INTO mesh_policies (id, name, current_revision, created_at, updated_at)
                 VALUES ($1, $2, 1, $3, $3)",
                &[&id, &name, &t],
            )
            .await?;
            1i64
        }
        Some(current) => {
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
             (policy_id, revision, body, compiled, compiled_hash, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        &[&id, &revision, body, compiled, &compiled_hash, &t],
    )
    .await?;
    Ok(revision as u64)
}

pub async fn list_policies(pg: &Client) -> Result<Vec<PolicyRow>> {
    Ok(pg
        .query(
            "SELECT id, name, current_revision, updated_at FROM mesh_policies ORDER BY name",
            &[],
        )
        .await?
        .into_iter()
        .map(|r| PolicyRow {
            id: r.get(0),
            name: r.get(1),
            current_revision: r.get::<_, i64>(2) as u64,
            updated_at: r.get::<_, i64>(3) as u64,
        })
        .collect())
}

/// The policy document of the current revision, for the editor.
pub async fn policy_body(pg: &Client, id: &str) -> Result<(u64, serde_json::Value)> {
    let row = pg
        .query_opt(
            "SELECT r.revision, r.body
               FROM mesh_policy_revisions r
               JOIN mesh_policies p ON p.id = r.policy_id AND p.current_revision = r.revision
              WHERE p.id = $1",
            &[&id],
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
    policy_id: &str,
    username: &str,
    password: &str,
) -> Result<()> {
    if username.is_empty() || password.is_empty() {
        return Err(Error::Refused(
            "a collector credential needs both a username and a password".into(),
        ));
    }
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
    policy_id: &str,
    expires_at: Option<u64>,
) -> Result<String> {
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
    let row = pg
        .query_opt(
            "SELECT policy_id, expires_at, revoked FROM mesh_enrollment_tokens WHERE token_hash = $1",
            &[&hash(token)],
        )
        .await?
        .ok_or_else(refused)?;
    let policy_id: String = row.get(0);
    let expires_at: Option<i64> = row.get(1);
    if row.get::<_, bool>(2) || expires_at.is_some_and(|e| (e as u64) < now()) {
        return Err(refused());
    }

    let instance_uid = mint_secret();
    let credential = mint_secret();
    let t = now() as i64;
    pg.execute(
        "INSERT INTO mesh_agents
            (instance_uid, policy_id, credential_hash, hostname, attributes, enrolled_at, last_seen)
         VALUES ($1, $2, $3, $4, $5, $6, $6)",
        &[
            &instance_uid,
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

pub async fn list_agents(pg: &Client) -> Result<Vec<AgentRow>> {
    Ok(pg
        .query(
            &format!("SELECT {AGENT_COLS} FROM mesh_agents ORDER BY last_seen DESC"),
            &[],
        )
        .await?
        .iter()
        .map(agent_from_row)
        .collect())
}

/// The current revision of every policy, for deriving each agent's status
/// without a query per row.
pub async fn current_revisions(pg: &Client) -> Result<BTreeMap<String, u64>> {
    Ok(pg
        .query("SELECT id, current_revision FROM mesh_policies", &[])
        .await?
        .into_iter()
        .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1) as u64))
        .collect())
}

/// Move an agent to another policy. The next check-in sees a different hash
/// and applies it — there is no push, and no re-enrollment.
pub async fn assign_policy(pg: &Client, instance_uid: &str, policy_id: &str) -> Result<()> {
    let n = pg
        .execute(
            "UPDATE mesh_agents SET policy_id = $2 WHERE instance_uid = $1 AND NOT unenrolled",
            &[&instance_uid, &policy_id],
        )
        .await?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

pub async fn unenrol(pg: &Client, instance_uid: &str) -> Result<()> {
    let n = pg
        .execute(
            "UPDATE mesh_agents SET unenrolled = true WHERE instance_uid = $1",
            &[&instance_uid],
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
        assert_eq!(versions, vec!["001_mesh", "002_policy_credentials"]);
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
}
