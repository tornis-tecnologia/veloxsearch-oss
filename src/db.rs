// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Control-plane datastore bring-up (ADR-041, #92).
//!
//! A small in-cluster Postgres holds multi-user control-plane state — users,
//! tenants, membership/ownership, quotas, audit — while deployment state stays
//! in the `OpenSearchCluster` CRs (ADR-041 boundary). This module owns the
//! *bring-up* only:
//!
//!  - the credentials Secret (`veloxsearch-postgres-credentials`): app-managed
//!    per ADR-023/034 — generated in-cluster on first boot, never a manifest
//!    literal. The bundled StatefulSet (deploy/install.yaml) references it, so
//!    its pod waits in CreateContainerConfigError until this app first runs.
//!  - the connection config (`VELOX_PG_*` env, all with in-cluster defaults;
//!    the password is read from the Secret via the K8s API, never from env);
//!  - ordered plain-SQL migrations (`migrations/NNN_name.sql`, embedded at
//!    compile time) applied by the small idempotent runner below, versioned in
//!    a `schema_migrations` ledger — no migration framework (ADR-041).
//!
//! Everything is gated by `VELOX_PG_ENABLED` (default OFF): the flag exists so
//! the datastore can ship before anything depends on it. When ON, migrations
//! must reach head before the app serves — a DB outage at startup fails closed
//! (ADR-041) instead of silently degrading. When OFF the app behaves exactly
//! as before; there is deliberately NO default connection target beyond the
//! in-cluster Service name, so an off-cluster dev run can never reach a real
//! deployment by accident (the #67 `ns()` lesson).
//!
//! The query layer (sqlx vs. diesel vs. raw) and the auth/ownership wiring are
//! #93/#94's build — nothing here reads or writes the domain tables yet.

use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Api, PostParams};
use kube::Client;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// App-managed credentials Secret (ADR-023 pattern; inventoried in
/// docs/SECRETS.md). Two CSPRNG passwords, generated once at first app boot:
///  - `superuser-password` → the postgres image's own superuser (used only by
///    the DB pod itself: entrypoint + initdb),
///  - `app-password`       → the scoped NON-superuser `velox` role the app
///    connects as (created by the initdb script in deploy/install.yaml).
pub const PG_SECRET: &str = "veloxsearch-postgres-credentials";
const KEY_SUPERUSER_PW: &str = "superuser-password";
const KEY_APP_PW: &str = "app-password";

/// Conventional names, NOT secrets (mirrors `k8s::ADMIN_USER`): the scoped
/// application role and its database, created by the initdb script.
const APP_USER: &str = "velox";
const APP_DB: &str = "velox";

/// In-cluster Service name of the bundled StatefulSet (deploy/install.yaml).
/// Resolves ONLY inside the cluster namespace — off-cluster it is inert, which
/// is exactly the safe default (never a real host).
const DEFAULT_HOST: &str = "veloxsearch-postgres";

/// Ordered, embedded migrations. Append-only: new schema changes are NEW
/// files (`migrations/00N_name.sql` + an entry here) — never edits to applied
/// ones, since the ledger tracks versions, not content.
const MIGRATIONS: &[(&str, &str)] = &[
    ("001_init", include_str!("../migrations/001_init.sql")),
    (
        "002_auth_tokens",
        include_str!("../migrations/002_auth_tokens.sql"),
    ),
];

/// The bring-up feature flag (issue #92): default OFF. Flip via the
/// `veloxsearch-env` ConfigMap once the cluster has Longhorn (the Postgres PVC
/// pins `storageClass: longhorn` per ADR-043, so before first-run bootstrap
/// the pod is Pending by design).
pub fn pg_enabled() -> bool {
    flag_on(std::env::var("VELOX_PG_ENABLED").ok())
}

/// Pure flag parse, split out for tests (the `resolve_ns` idiom, #67).
fn flag_on(v: Option<String>) -> bool {
    matches!(
        v.as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Connection parameters (everything except the password, which only ever
/// lives in the Secret). Defaults target the bundled in-cluster StatefulSet.
#[derive(Debug, PartialEq, Eq)]
pub struct PgConfig {
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub user: String,
}

impl PgConfig {
    pub fn from_env() -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok();
        resolve_config(
            var("VELOX_PG_HOST"),
            var("VELOX_PG_PORT"),
            var("VELOX_PG_DB"),
            var("VELOX_PG_USER"),
        )
    }
}

/// Pure config resolution, unit-testable without touching process env.
fn resolve_config(
    host: Option<String>,
    port: Option<String>,
    dbname: Option<String>,
    user: Option<String>,
) -> Result<PgConfig> {
    fn or_default(v: Option<String>, default: &str) -> String {
        v.map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    }
    let port = match port.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(p) => p
            .parse::<u16>()
            .with_context(|| format!("VELOX_PG_PORT is not a valid port: {p:?}"))?,
        None => 5432,
    };
    Ok(PgConfig {
        host: or_default(host, DEFAULT_HOST),
        port,
        dbname: or_default(dbname, APP_DB),
        user: or_default(user, APP_USER),
    })
}

/// Idempotently create the credentials Secret. Called on every boot (the
/// bundled StatefulSet cannot start without it); a no-op once it exists, so
/// passwords are generated exactly once and never rotated implicitly
/// (rotation is a manual ALTER ROLE + Secret update — docs/SECRETS.md).
pub async fn ensure_pg_secret(client: &Client) -> Result<()> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), crate::k8s::ns());
    if secrets.get_opt(PG_SECRET).await?.is_some() {
        return Ok(());
    }
    // Alphanumeric only: safe in a psql `:'var'` literal, a URL and a shell.
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut string_data = BTreeMap::new();
    string_data.insert(
        KEY_SUPERUSER_PW.to_string(),
        crate::k8s::random_chars(ALPHABET, 32)?,
    );
    string_data.insert(
        KEY_APP_PW.to_string(),
        crate::k8s::random_chars(ALPHABET, 32)?,
    );
    let secret = Secret {
        metadata: kube::api::ObjectMeta {
            name: Some(PG_SECRET.to_string()),
            namespace: Some(crate::k8s::ns().to_string()),
            ..Default::default()
        },
        string_data: Some(string_data),
        ..Default::default()
    };
    secrets
        .create(&PostParams::default(), &secret)
        .await
        .context("creating postgres credentials Secret")?;
    tracing::info!(secret = PG_SECRET, "generated postgres credentials Secret");
    Ok(())
}

/// Best-effort variant for the flag-OFF path: keeps the bundled StatefulSet
/// startable without making an off-cluster dev run (no kube client) fatal.
pub async fn ensure_pg_secret_best_effort() -> Result<()> {
    let client = crate::k8s::client().await?;
    ensure_pg_secret(&client).await
}

/// The app role's password, read from the Secret via the K8s API. No env and
/// no default: Secret unreadable → hard error (fail closed), never a literal.
async fn app_password(client: &Client) -> Result<String> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), crate::k8s::ns());
    let secret = secrets
        .get_opt(PG_SECRET)
        .await?
        .with_context(|| format!("Secret {PG_SECRET} not found in {}", crate::k8s::ns()))?;
    secret
        .data
        .as_ref()
        .and_then(|d| d.get(KEY_APP_PW))
        .map(|b| String::from_utf8_lossy(&b.0).into_owned())
        .filter(|p| !p.is_empty())
        .with_context(|| format!("Secret {PG_SECRET} has no {KEY_APP_PW} key"))
}

async fn connect(cfg: &PgConfig, password: &str) -> Result<tokio_postgres::Client> {
    let mut config = tokio_postgres::Config::new();
    config
        .host(&cfg.host)
        .port(cfg.port)
        .dbname(&cfg.dbname)
        .user(&cfg.user)
        .password(password)
        .connect_timeout(Duration::from_secs(10));
    // NoTls: the ClusterIP Service on the pod network, same trust model as
    // every other in-cluster hop the app already makes.
    let (client, conn) = config
        .connect(tokio_postgres::NoTls)
        .await
        .with_context(|| format!("connecting to postgres at {}:{}", cfg.host, cfg.port))?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::warn!("postgres connection task ended: {e}");
        }
    });
    Ok(client)
}

/// Open a fresh application connection for a control-plane query (#79).
///
/// Deliberately connection-per-operation rather than a pool: every caller is a
/// low-frequency identity action (signup, verify, reset, login). Session
/// checking is HMAC-only and never touches the database, so there is no
/// per-request read path to pool for. When #80 adds per-request ownership
/// lookups, that is the moment a pool earns its keep — not before.
///
/// Fails closed the same way `bring_up` does: no kube client or no Secret
/// means no connection, never a fallback credential.
pub async fn connect_app() -> Result<tokio_postgres::Client> {
    let cfg = PgConfig::from_env()?;
    let kube = crate::k8s::client().await?;
    let password = app_password(&kube).await?;
    connect(&cfg, &password).await
}

/// The ~40-line idempotent runner (ADR-041): ensure the ledger, apply each
/// not-yet-applied migration in one transaction (DDL + ledger insert commit
/// together), skip the rest. Re-running against a current DB is a no-op.
/// Returns how many migrations were applied.
async fn run_migrations(pg: &mut tokio_postgres::Client) -> Result<usize> {
    pg.batch_execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    text PRIMARY KEY,
             applied_at timestamptz NOT NULL DEFAULT now()
         )",
    )
    .await
    .context("creating schema_migrations ledger")?;
    let applied: std::collections::HashSet<String> = pg
        .query("SELECT version FROM schema_migrations", &[])
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    let mut count = 0;
    for &(version, sql) in MIGRATIONS {
        if applied.contains(version) {
            continue;
        }
        let tx = pg.transaction().await?;
        tx.batch_execute(sql)
            .await
            .with_context(|| format!("applying migration {version}"))?;
        tx.execute(
            "INSERT INTO schema_migrations (version) VALUES ($1)",
            &[&version],
        )
        .await?;
        tx.commit()
            .await
            .with_context(|| format!("committing migration {version}"))?;
        tracing::info!(migration = version, "applied");
        count += 1;
    }
    Ok(count)
}

/// One full bring-up attempt: secret → password → connect → migrate.
async fn try_bring_up(cfg: &PgConfig) -> Result<usize> {
    let kube = crate::k8s::client().await?;
    ensure_pg_secret(&kube).await?;
    let password = app_password(&kube).await?;
    let mut pg = connect(cfg, &password).await?;
    run_migrations(&mut pg).await
}

/// Flag-ON startup path: retry the bring-up until it succeeds or the deadline
/// (`VELOX_PG_WAIT_SECS`, default 300) passes, then give up with an error —
/// the caller exits non-zero, K8s restarts the pod, and the loop converges
/// once the DB pod is up (on a very first boot the Secret this call creates
/// is what un-wedges the DB pod, so patience here is structural, not polish).
pub async fn bring_up() -> Result<()> {
    let cfg = PgConfig::from_env()?;
    let wait_secs = std::env::var("VELOX_PG_WAIT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(300);
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        match try_bring_up(&cfg).await {
            Ok(applied) => {
                tracing::info!(
                    applied,
                    host = %cfg.host,
                    db = %cfg.dbname,
                    "control-plane postgres ready (migrations at head)"
                );
                return Ok(());
            }
            Err(e) if Instant::now() < deadline => {
                tracing::warn!("postgres bring-up not ready yet, retrying in 5s: {e:#}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                return Err(e.context(format!(
                    "postgres bring-up did not succeed within {wait_secs}s (VELOX_PG_WAIT_SECS)"
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn flag_defaults_off_and_parses_truthy_values() {
        assert!(!flag_on(None));
        assert!(!flag_on(s("")));
        assert!(!flag_on(s("0")));
        assert!(!flag_on(s("false")));
        assert!(flag_on(s("1")));
        assert!(flag_on(s("true")));
        assert!(flag_on(s(" TRUE ")));
        assert!(flag_on(s("yes")));
        assert!(flag_on(s("on")));
    }

    #[test]
    fn config_defaults_target_the_bundled_service() {
        let cfg = resolve_config(None, None, None, None).unwrap();
        assert_eq!(
            cfg,
            PgConfig {
                host: DEFAULT_HOST.to_string(),
                port: 5432,
                dbname: APP_DB.to_string(),
                user: APP_USER.to_string(),
            }
        );
    }

    #[test]
    fn config_env_overrides_and_rejects_bad_port() {
        let cfg = resolve_config(s(" pg.example "), s("5433"), s("other"), s("app")).unwrap();
        assert_eq!(cfg.host, "pg.example");
        assert_eq!(cfg.port, 5433);
        assert_eq!(cfg.dbname, "other");
        assert_eq!(cfg.user, "app");
        // Blank falls back to the default instead of an empty host.
        assert_eq!(
            resolve_config(s("  "), None, None, None).unwrap().host,
            DEFAULT_HOST
        );
        assert!(resolve_config(None, s("not-a-port"), None, None).is_err());
    }

    #[test]
    fn migrations_are_ordered_unique_and_nonempty() {
        let mut seen = std::collections::HashSet::new();
        let mut last = "";
        for &(version, sql) in MIGRATIONS {
            assert!(
                seen.insert(version),
                "duplicate migration version {version}"
            );
            assert!(version > last, "migrations out of order at {version}");
            assert!(!sql.trim().is_empty(), "migration {version} is empty");
            // The ledger belongs to the runner; a migration re-creating it
            // would fight the runner's own bootstrap.
            assert!(
                !sql.contains("schema_migrations"),
                "migration {version} must not touch the runner-owned ledger"
            );
            last = version;
        }
    }

    #[test]
    fn init_migration_creates_the_five_control_plane_tables() {
        let sql = MIGRATIONS[0].1;
        for table in ["users", "tenants", "tenant_users", "quotas", "audit"] {
            assert!(
                sql.contains(&format!("CREATE TABLE {table}")),
                "001_init missing table {table}"
            );
        }
    }

    /// Live-DB proof (apply-from-empty + idempotency, the ADR-041 test shape).
    /// Ignored by default: needs a reachable Postgres, e.g.
    ///   podman run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=t postgres:16-alpine
    ///   VELOX_PG_TEST_URL=postgres://postgres:t@127.0.0.1:5433/postgres \
    ///     cargo test -- --ignored migrations_apply
    #[test]
    #[ignore = "needs a live Postgres (set VELOX_PG_TEST_URL)"]
    fn migrations_apply_from_empty_and_are_idempotent() {
        let url = std::env::var("VELOX_PG_TEST_URL").expect("VELOX_PG_TEST_URL not set");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (mut pg, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
                .await
                .expect("connect");
            tokio::spawn(conn);
            let first = run_migrations(&mut pg).await.expect("first run");
            assert_eq!(first, MIGRATIONS.len(), "fresh DB applies every migration");
            let second = run_migrations(&mut pg).await.expect("second run");
            assert_eq!(second, 0, "re-run must be a no-op");
            for table in ["users", "tenants", "tenant_users", "quotas", "audit"] {
                let row = pg
                    .query_one(
                        "SELECT to_regclass($1)::text",
                        &[&format!("public.{table}")],
                    )
                    .await
                    .unwrap();
                assert!(
                    row.get::<_, Option<String>>(0).is_some(),
                    "table {table} missing after migration"
                );
            }
        });
    }
}
