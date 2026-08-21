// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Control-plane accounts: self-serve signup, email verification, password
//! reset, and the login lookup that gives a session its tenant (#79, ADR-041).
//!
//! This is the multi-user half of authentication. `auth.rs` keeps owning the
//! single admin credential (the K8s Secret, ADR-023) and the HMAC cookie; this
//! module owns *users and tenants*, which live in the ADR-041 Postgres.
//! Nothing here runs unless [`enabled`] is true.
//!
//! ## The flag
//!
//! `VELOX_MULTITENANT_AUTH` (default OFF, same parse as `VELOX_PG_ENABLED`).
//! OFF, every entry point returns immediately and the app behaves exactly as
//! it did before this card: the admin logs in with the admin credential and
//! there is no signup surface at all. It also implies the datastore — with
//! `VELOX_PG_ENABLED` off there is nothing to write to, so both must be on.
//!
//! ## Two disciplines the code is shaped around
//!
//! **User enumeration.** A public signup form and a public reset form are both
//! oracles for "does this address have an account" unless every branch answers
//! identically. So they do: signup on a taken address, reset for an unknown
//! address, and their happy paths all return the same `202`. The cost is that
//! "you already have an account" has to be said in the *email* rather than in
//! the HTTP response, which is where a stranger cannot read it.
//!
//! **Tokens are secrets, rows are not.** Only the SHA-256 digest of a token is
//! stored (see migrations/002). Lookup is by digest — an index probe against a
//! 190-bit random value, so there is no timing signal worth defending and no
//! redeemable secret in a database backup.
//!
//! ## Signup also builds the tenant's cluster-level walls (#81)
//!
//! Signup records `tenants.namespace` AND provisions it: the namespace, a
//! ResourceQuota rendered from the `quotas` row this module seeds, a LimitRange
//! and a default-deny NetworkPolicy set (`k8s::provision_tenant`, ADR-044/051).
//! That call is best-effort in the same sense the verification mail is — the
//! account row is already committed, provisioning is idempotent, and a tenant
//! whose namespace is missing is refused at their first deployment with an
//! actionable message rather than silently landing in someone else's namespace.
//! Losing the account because a cluster call failed would be the worse trade.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// Verification links outlive a working day; reset links deliberately do not.
const VERIFY_TTL_HOURS: i64 = 24;
const RESET_TTL_HOURS: i64 = 2;

/// 32 chars of the CSPRNG alphabet ≈ 190 bits — a value nobody guesses and
/// nothing enumerates, which is what lets the digest lookup stay a plain
/// index probe.
const TOKEN_LEN: usize = 32;
const TOKEN_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Matches `auth::complete_setup` — one password policy for the whole product.
const MIN_PASSWORD_LEN: usize = 8;

/// One tenant, one `velox-t-<slug>` namespace (ADR-044). The prefix is what
/// makes a tenant slug incapable of naming `kube-system`. `pub(crate)` so
/// `k8s::render_tenant_bundle` can refuse to provision a `tenants.namespace`
/// that is not this mapping of the slug (#81).
pub(crate) const NAMESPACE_PREFIX: &str = "velox-t-";
/// `velox-t-` + slug must stay inside a 63-char DNS label.
const MAX_SLUG_LEN: usize = 40;

/// A bcrypt hash of a value nobody knows, verified against when the account
/// does not exist so that "unknown user" costs the same wall-clock time as
/// "wrong password". Generated once with `DEFAULT_COST`.
const DUMMY_HASH: &str = "$2b$12$YqcB8tL84CqTcx1JrbChUO3e5J48oQ3HTpn0EspOg7mVgjFVTr1/i";

/// Who is signed in: the identity a session carries and #80 scopes by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub email: String,
    pub user_id: String,
    pub tenant_id: String,
}

/// The multi-tenant auth feature flag (#79). Default OFF so `develop` stays
/// shippable while the surface is incomplete (no frontend yet).
pub fn enabled() -> bool {
    flag_on(std::env::var("VELOX_MULTITENANT_AUTH").ok())
}

/// Pure flag parse, split out for tests (the `db::flag_on` idiom, #67).
fn flag_on(v: Option<String>) -> bool {
    matches!(
        v.as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

// ─────────────────────────── pure helpers ───────────────────────────

/// SHA-256 hex of a token. The only form that reaches the database.
fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn new_token() -> Result<String> {
    crate::k8s::random_chars(TOKEN_ALPHABET, TOKEN_LEN)
}

/// One password rule, stated once. Length only: complexity classes push people
/// toward `Passw0rd!` and this account is not the last line of defence — the
/// deployment credentials are separate (ADR-030).
fn validate_password(password: &str) -> Result<()> {
    if password.len() < MIN_PASSWORD_LEN {
        bail!("password must be at least {MIN_PASSWORD_LEN} characters");
    }
    // bcrypt silently truncates at 72 bytes; refusing is honest, truncating is
    // a password that is not the password the user typed.
    if password.len() > 72 {
        bail!("password must be at most 72 characters");
    }
    Ok(())
}

/// URL/label-safe tenant handle: lower-case alphanumerics and single hyphens,
/// starting and ending alphanumeric. `None` when nothing usable survives, so
/// the caller falls back rather than emitting an invalid namespace name.
fn slugify(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let slug = out.trim_matches('-');
    // Truncation can re-expose a trailing hyphen, so trim again after it.
    let slug: String = slug.chars().take(MAX_SLUG_LEN).collect();
    let slug = slug.trim_matches('-');
    (slug.len() >= 2).then(|| slug.to_string())
}

/// Where a tenant's slug comes from: the name they typed, else the local part
/// of their address, else a neutral constant. Never the domain — two people
/// from the same company get two tenants unless they explicitly join one, and
/// `tenants.slug` is UNIQUE.
fn slug_seed(email: &str, display_name: Option<&str>) -> String {
    display_name
        .and_then(slugify)
        .or_else(|| slugify(email.split('@').next().unwrap_or("")))
        .unwrap_or_else(|| "tenant".to_string())
}

/// The ADR-044 tenant→namespace mapping. Recorded here, provisioned by #81
/// (ADR-051).
fn namespace_for(slug: &str) -> String {
    format!("{NAMESPACE_PREFIX}{slug}")
}

/// Human-facing tenant name: what they typed, else the address they signed up
/// with (never blank — `tenants.display_name` is NOT NULL).
fn display_name_for(email: &str, requested: Option<&str>) -> String {
    requested
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(120).collect())
        .unwrap_or_else(|| email.to_string())
}

/// Was this failure a unique-constraint violation on `constraint`?
fn violates(e: &tokio_postgres::Error, constraint: &str) -> bool {
    e.as_db_error()
        .is_some_and(|d| d.constraint() == Some(constraint))
}

// ─────────────────────────── signup ───────────────────────────

/// What a caller may tell the user. Every variant is safe to render publicly —
/// notably there is no "that address is taken", by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignupOutcome {
    /// Accepted. Says nothing about whether an account was created: a taken
    /// address lands here too (see the enumeration discipline in the module
    /// docs).
    Accepted,
}

/// Self-serve signup: create a user, their tenant, their owner membership and
/// a default quota row, then mail a verification link.
///
/// Rejects (visibly, because these are the user's own input to fix):
///  - a malformed address,
///  - a free/consumer/disposable mailbox (ADR-038, [`crate::email_denylist`]),
///  - a password that fails [`validate_password`].
///
/// Does NOT reject an address that already has an account — that answers
/// `Accepted` without touching the row, and the existing owner gets nothing.
pub async fn signup(
    raw_email: &str,
    password: &str,
    display_name: Option<&str>,
) -> Result<SignupOutcome> {
    if !enabled() {
        bail!("self-serve signup is not enabled on this installation");
    }
    let mut pg = crate::db::connect_app().await?;
    let Some((email, token, tenant)) =
        signup_on(&mut pg, raw_email, password, display_name).await?
    else {
        // Address already registered. Same answer, no mail, no row touched.
        tracing::info!("signup for an address that already exists; answering as accepted");
        return Ok(SignupOutcome::Accepted);
    };
    // The cluster-level walls (#81). After the commit, never inside it: a K8s
    // round-trip must not hold a Postgres transaction open.
    if let Err(e) = provision_isolation(&mut pg, &tenant).await {
        tracing::error!(tenant = %tenant.slug, "tenant isolation was NOT provisioned: {e:#}");
    }
    // Best-effort: a relay outage must not undo an account. The user can ask
    // for a new link; a rolled-back signup would be far worse.
    if let Err(e) = crate::mail::send(
        &email,
        crate::mail::verification_subject(),
        &crate::mail::verification_body(&token, VERIFY_TTL_HOURS),
    )
    .await
    {
        tracing::error!("could not deliver the verification mail: {e:#}");
    }
    Ok(SignupOutcome::Accepted)
}

/// Signup minus the connection and the mail: validate, persist, mint.
///
/// Returns the address and the RAW verification token so the caller — the one
/// place that talks to a relay — can deliver it. Minting and delivering are
/// separated because they fail for unrelated reasons: a failed insert must
/// undo the account, a failed send must not.
///
/// `None` means the address is already registered.
async fn signup_on(
    pg: &mut tokio_postgres::Client,
    raw_email: &str,
    password: &str,
    display_name: Option<&str>,
) -> Result<Option<(String, String, crate::k8s::TenantIdentity)>> {
    let email = crate::email_denylist::validate_email(raw_email)
        .context("that does not look like an email address")?;
    if crate::email_denylist::is_free_email(&email) {
        bail!("please sign up with your work email address");
    }
    validate_password(password)?;
    let hash = bcrypt::hash(password, bcrypt::DEFAULT_COST).context("hashing password")?;
    let Some((user_id, tenant)) = create_user_and_tenant(pg, &email, &hash, display_name).await?
    else {
        return Ok(None);
    };
    let token = issue_token(pg, "email_verification_tokens", &user_id, VERIFY_TTL_HOURS).await?;
    Ok(Some((email, token, tenant)))
}

/// The transactional core of signup. `Ok(None)` means the address is already
/// registered; every other failure is an error.
///
/// The retry loop exists for exactly one reason: `tenants.slug` is UNIQUE and
/// two people can want `acme`. On a slug collision the transaction is rolled
/// back and retried with a random suffix, which is also the only thing that
/// closes the check-then-insert race a pre-flight `SELECT` would leave open.
async fn create_user_and_tenant(
    pg: &mut tokio_postgres::Client,
    email: &str,
    password_hash: &str,
    display_name: Option<&str>,
) -> Result<Option<(String, crate::k8s::TenantIdentity)>> {
    let seed = slug_seed(email, display_name);
    let display = display_name_for(email, display_name);
    const ATTEMPTS: usize = 5;
    for attempt in 0..ATTEMPTS {
        let slug = if attempt == 0 {
            seed.clone()
        } else {
            let suffix = crate::k8s::random_chars(b"abcdefghijklmnopqrstuvwxyz0123456789", 6)?;
            let stem: String = seed.chars().take(MAX_SLUG_LEN - 7).collect();
            format!("{}-{suffix}", stem.trim_end_matches('-'))
        };
        let tx = pg.transaction().await?;
        let user = match tx
            .query_one(
                "INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id::text",
                &[&email, &password_hash],
            )
            .await
        {
            Ok(row) => row,
            Err(e) if violates(&e, "users_email_key") => return Ok(None),
            Err(e) => return Err(e).context("creating user"),
        };
        let user_id: String = user.get(0);

        let tenant = tx
            .query_one(
                "INSERT INTO tenants (slug, namespace, display_name) VALUES ($1, $2, $3)
                 RETURNING id::text",
                &[&slug, &namespace_for(&slug), &display],
            )
            .await;
        let tenant = match tenant {
            Ok(row) => row,
            Err(e) if violates(&e, "tenants_slug_key") || violates(&e, "tenants_namespace_key") => {
                // Rolls back the user insert too; the next attempt redoes it.
                tx.rollback()
                    .await
                    .context("rolling back a slug collision")?;
                continue;
            }
            Err(e) => return Err(e).context("creating tenant"),
        };
        let tenant_id: String = tenant.get(0);

        // `$n::text::uuid`, never `$n::uuid`: a bare cast makes Postgres infer
        // the PARAMETER as uuid, and the driver has no uuid feature enabled —
        // it would refuse to serialize a String. Casting from text keeps the
        // wire type text and lets the server do the conversion, which is also
        // what lets ids stay plain `String` throughout this module.
        tx.execute(
            "INSERT INTO tenant_users (tenant_id, user_id, role)
             VALUES ($1::text::uuid, $2::text::uuid, 'owner')",
            &[&tenant_id, &user_id],
        )
        .await
        .context("recording tenant ownership")?;
        // Explicit row rather than relying on absence: #84's admission gate
        // reads a quota, and "no row" and "the default quota" must not be the
        // same question.
        tx.execute(
            "INSERT INTO quotas (tenant_id) VALUES ($1::text::uuid)",
            &[&tenant_id],
        )
        .await
        .context("seeding tenant quota")?;
        audit(
            &tx,
            Some(&user_id),
            Some(&tenant_id),
            "user.signup",
            Some(&slug),
        )
        .await?;
        tx.commit().await.context("committing signup")?;
        tracing::info!(tenant = %slug, "self-serve signup created a tenant");
        return Ok(Some((
            user_id,
            crate::k8s::TenantIdentity {
                id: tenant_id,
                namespace: namespace_for(&slug),
                slug,
            },
        )));
    }
    bail!("could not allocate a unique tenant slug after {ATTEMPTS} attempts")
}

// ─────────────────── cluster-level isolation (#81) ───────────────────

/// The tenant's ADR-041 `quotas` row — the numbers the ResourceQuota is
/// rendered from. Signup seeds it explicitly, so a missing row is a broken
/// invariant to surface, not a default to invent.
async fn read_quota(
    pg: &tokio_postgres::Client,
    tenant_id: &str,
) -> Result<crate::k8s::TenantQuota> {
    let row = pg
        .query_opt(
            "SELECT max_deployments, max_total_disk_gb, max_nodes FROM quotas
             WHERE tenant_id = $1::text::uuid",
            &[&tenant_id],
        )
        .await
        .context("reading the tenant quota")?
        .context("tenant has no quotas row")?;
    Ok(crate::k8s::TenantQuota {
        max_deployments: row.get(0),
        max_total_disk_gb: row.get(1),
        max_nodes: row.get(2),
    })
}

/// Provision the tenant's namespace, ResourceQuota, LimitRange and default-deny
/// NetworkPolicy set (ADR-044/049), and record the outcome in the audit log.
///
/// The audit row is written on **both** paths on purpose: after a bad signup an
/// operator's first question is "which tenants are running without walls", and
/// silence is not an answer to it.
async fn provision_isolation(
    pg: &mut tokio_postgres::Client,
    tenant: &crate::k8s::TenantIdentity,
) -> Result<()> {
    let outcome = match read_quota(pg, &tenant.id).await {
        Ok(quota) => crate::k8s::provision_tenant(tenant, &quota).await,
        Err(e) => Err(e),
    };
    let action = if outcome.is_ok() {
        "tenant.provisioned"
    } else {
        "tenant.provision_failed"
    };
    let tx = pg
        .transaction()
        .await
        .context("opening the provisioning audit transaction")?;
    audit(&tx, None, Some(&tenant.id), action, Some(&tenant.namespace)).await?;
    tx.commit()
        .await
        .context("committing the provisioning audit")?;
    outcome
}

// ─────────────────────────── tokens ───────────────────────────

/// Mint a single-use token for `user_id`, returning the RAW value (the only
/// place it exists outside the mail we are about to send).
///
/// Also prunes that user's spent and expired rows: the tokens tables are the
/// only ones that accumulate garbage, and the issuing path is the one moment
/// we already know which user to sweep — so this costs one statement and no
/// scheduler.
async fn issue_token(
    pg: &tokio_postgres::Client,
    table: &str,
    user_id: &str,
    ttl_hours: i64,
) -> Result<String> {
    // `table` is never caller-controlled — it is one of two literals below.
    debug_assert!(matches!(
        table,
        "email_verification_tokens" | "password_reset_tokens"
    ));
    pg.execute(
        &format!(
            "DELETE FROM {table}
             WHERE user_id = $1::text::uuid AND (consumed_at IS NOT NULL OR expires_at < now())"
        ),
        &[&user_id],
    )
    .await
    .context("pruning spent tokens")?;
    let token = new_token()?;
    pg.execute(
        &format!(
            "INSERT INTO {table} (token_hash, user_id, expires_at)
             VALUES ($1, $2::text::uuid, now() + make_interval(hours => $3::int))"
        ),
        &[&token_hash(&token), &user_id, &(ttl_hours as i32)],
    )
    .await
    .context("storing token")?;
    Ok(token)
}

/// Consume a token, returning the user it belonged to. Unknown, expired and
/// already-used tokens are all `None` — one answer, so a probe learns nothing
/// beyond "this link does not work".
async fn consume_token(
    tx: &tokio_postgres::Transaction<'_>,
    table: &str,
    token: &str,
) -> Result<Option<String>> {
    debug_assert!(matches!(
        table,
        "email_verification_tokens" | "password_reset_tokens"
    ));
    let row = tx
        .query_opt(
            &format!(
                "UPDATE {table} SET consumed_at = now()
                 WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > now()
                 RETURNING user_id::text"
            ),
            &[&token_hash(token)],
        )
        .await
        .context("consuming token")?;
    Ok(row.map(|r| r.get(0)))
}

// ─────────────────────────── verification ───────────────────────────

/// Redeem a verification link. `false` = the link is unknown, expired or
/// already used; the caller must not distinguish those for the user.
pub async fn verify_email(token: &str) -> Result<bool> {
    if !enabled() {
        return Ok(false);
    }
    let mut pg = crate::db::connect_app().await?;
    verify_email_on(&mut pg, token).await
}

async fn verify_email_on(pg: &mut tokio_postgres::Client, token: &str) -> Result<bool> {
    let tx = pg.transaction().await?;
    let Some(user_id) = consume_token(&tx, "email_verification_tokens", token).await? else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE users SET email_verified = true, updated_at = now() WHERE id = $1::text::uuid",
        &[&user_id],
    )
    .await
    .context("marking email verified")?;
    audit(&tx, Some(&user_id), None, "user.email_verified", None).await?;
    tx.commit().await.context("committing verification")?;
    Ok(true)
}

// ─────────────────────────── password reset ───────────────────────────

/// Request a reset link. Always `Ok(())`, whatever the address turns out to
/// be: an unknown address, a malformed one and a real one are indistinguishable
/// from outside. Only a real one gets mail.
pub async fn request_password_reset(raw_email: &str) -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    let pg = crate::db::connect_app().await?;
    let Some((email, token)) = request_password_reset_on(&pg, raw_email).await? else {
        return Ok(());
    };
    if let Err(e) = crate::mail::send(
        &email,
        crate::mail::reset_subject(),
        &crate::mail::reset_body(&token, RESET_TTL_HOURS),
    )
    .await
    {
        tracing::error!("could not deliver the password-reset mail: {e:#}");
    }
    Ok(())
}

/// The reset request minus the connection and the mail. `None` — no mail to
/// send — covers both a malformed address and an unknown one, which is what
/// makes the two indistinguishable from outside.
async fn request_password_reset_on(
    pg: &tokio_postgres::Client,
    raw_email: &str,
) -> Result<Option<(String, String)>> {
    let Some(email) = crate::email_denylist::validate_email(raw_email) else {
        return Ok(None);
    };
    let Some(row) = pg
        .query_opt("SELECT id::text FROM users WHERE email = $1", &[&email])
        .await
        .context("looking up account for reset")?
    else {
        tracing::info!("password reset requested for an unknown address; answering as accepted");
        return Ok(None);
    };
    let user_id: String = row.get(0);
    let token = issue_token(pg, "password_reset_tokens", &user_id, RESET_TTL_HOURS).await?;
    Ok(Some((email, token)))
}

/// Redeem a reset link and set a new password. `false` = the link does not
/// work, same three reasons collapsed into one answer as verification.
///
/// A successful reset also marks the address verified: redeeming a link sent
/// to that mailbox IS proof of control over it, so leaving an account stuck
/// unverified after a reset would be theatre.
///
/// Every other outstanding reset token for the user is consumed in the same
/// transaction — after a reset, older links in older mails are dead.
pub async fn reset_password(token: &str, new_password: &str) -> Result<bool> {
    if !enabled() {
        return Ok(false);
    }
    let mut pg = crate::db::connect_app().await?;
    reset_password_on(&mut pg, token, new_password).await
}

async fn reset_password_on(
    pg: &mut tokio_postgres::Client,
    token: &str,
    new_password: &str,
) -> Result<bool> {
    validate_password(new_password)?;
    let hash = bcrypt::hash(new_password, bcrypt::DEFAULT_COST).context("hashing password")?;
    let tx = pg.transaction().await?;
    let Some(user_id) = consume_token(&tx, "password_reset_tokens", token).await? else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE users SET password_hash = $2, email_verified = true, updated_at = now()
         WHERE id = $1::text::uuid",
        &[&user_id, &hash],
    )
    .await
    .context("storing new password")?;
    tx.execute(
        "UPDATE password_reset_tokens SET consumed_at = now()
         WHERE user_id = $1::text::uuid AND consumed_at IS NULL",
        &[&user_id],
    )
    .await
    .context("invalidating outstanding reset tokens")?;
    audit(&tx, Some(&user_id), None, "user.password_reset", None).await?;
    tx.commit().await.context("committing password reset")?;
    Ok(true)
}

// ─────────────────────────── login ───────────────────────────

/// Resolve an email + password to the principal a session is minted for.
///
/// `None` for every failure — flag off, unknown address, wrong password,
/// unverified address, suspended account — because the login endpoint answers
/// "invalid username or password" to all of them. When the address is unknown
/// the password is still verified against [`DUMMY_HASH`], so the response time
/// does not distinguish "no such user" from "wrong password".
///
/// Errors (as opposed to `None`) are database failures, which the caller logs
/// and turns into the same rejection: a broken DB must not become a bypass.
pub async fn authenticate(raw_email: &str, password: &str) -> Result<Option<Principal>> {
    if !enabled() {
        return Ok(None);
    }
    let pg = crate::db::connect_app().await?;
    authenticate_on(&pg, raw_email, password).await
}

async fn authenticate_on(
    pg: &tokio_postgres::Client,
    raw_email: &str,
    password: &str,
) -> Result<Option<Principal>> {
    let Some(email) = crate::email_denylist::validate_email(raw_email) else {
        return Ok(None);
    };
    // The tenant a session lands in: the one this user owns, oldest first, so
    // the answer is stable once membership grows past one row (#80).
    let row = pg
        .query_opt(
            "SELECT u.id::text, u.password_hash, u.email_verified, u.status, tu.tenant_id::text
             FROM users u
             JOIN tenant_users tu ON tu.user_id = u.id
             WHERE u.email = $1
             ORDER BY (tu.role = 'owner') DESC, tu.created_at ASC
             LIMIT 1",
            &[&email],
        )
        .await
        .context("looking up account for login")?;

    let Some(row) = row else {
        // Same work as the found path, so the timing matches.
        let _ = bcrypt::verify(password, DUMMY_HASH);
        return Ok(None);
    };
    let (user_id, hash): (String, String) = (row.get(0), row.get(1));
    let (verified, status): (bool, String) = (row.get(2), row.get(3));
    let tenant_id: String = row.get(4);

    if !bcrypt::verify(password, &hash).unwrap_or(false) {
        return Ok(None);
    }
    if !verified {
        tracing::info!("login refused: email not verified");
        return Ok(None);
    }
    if status != "active" {
        tracing::info!(status = %status, "login refused: account not active");
        return Ok(None);
    }
    Ok(Some(Principal {
        email,
        user_id,
        tenant_id,
    }))
}

// ─────────────────────────── audit ───────────────────────────

/// Append one control-plane audit row (ADR-041). Inside the caller's
/// transaction so the event and the change it describes commit together —
/// an audit trail that can disagree with the data is worse than none.
async fn audit(
    tx: &tokio_postgres::Transaction<'_>,
    actor: Option<&str>,
    tenant: Option<&str>,
    action: &str,
    target: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO audit (actor_user_id, tenant_id, action, target)
         VALUES ($1::text::uuid, $2::text::uuid, $3, $4)",
        &[&actor, &tenant, &action, &target],
    )
    .await
    .with_context(|| format!("recording audit event {action}"))?;
    Ok(())
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
        assert!(flag_on(s(" TRUE ")));
        assert!(flag_on(s("yes")));
        assert!(flag_on(s("on")));
    }

    #[test]
    fn password_policy_matches_the_admin_rule_and_bcrypts_ceiling() {
        assert!(validate_password("shorty7").is_err());
        assert!(validate_password("longenough").is_ok());
        // bcrypt truncates past 72 bytes; refuse rather than silently shorten.
        assert!(validate_password(&"a".repeat(72)).is_ok());
        assert!(validate_password(&"a".repeat(73)).is_err());
    }

    #[test]
    fn slugify_produces_label_safe_handles() {
        assert_eq!(slugify("Acme Corp"), Some("acme-corp".to_string()));
        assert_eq!(slugify("  ACME  "), Some("acme".to_string()));
        assert_eq!(slugify("a.b_c"), Some("a-b-c".to_string()));
        assert_eq!(slugify("--Acme--"), Some("acme".to_string()));
        // Nothing usable survives → the caller must fall back.
        assert_eq!(slugify("!!"), None);
        assert_eq!(slugify("a"), None);
        assert_eq!(slugify(""), None);
    }

    /// `velox-t-<slug>` has to fit a 63-char DNS label, and truncation must not
    /// leave a trailing hyphen (an invalid label).
    #[test]
    fn slugs_stay_inside_a_dns_label() {
        let long = slugify(&"x".repeat(120)).unwrap();
        assert_eq!(long.len(), MAX_SLUG_LEN);
        assert!(namespace_for(&long).len() <= 63);
        let truncated_at_a_hyphen = slugify(&format!("{}-tail", "y".repeat(MAX_SLUG_LEN - 1)));
        let slug = truncated_at_a_hyphen.unwrap();
        assert!(
            !slug.ends_with('-'),
            "truncation left an invalid label: {slug}"
        );
    }

    #[test]
    fn slug_seed_prefers_the_typed_name_then_the_local_part() {
        assert_eq!(slug_seed("ops@acme.com", Some("Acme Corp")), "acme-corp");
        assert_eq!(slug_seed("ops@acme.com", None), "ops");
        assert_eq!(slug_seed("ops@acme.com", Some("  ")), "ops");
        // Unusable on both counts → a neutral handle, never an empty one.
        assert_eq!(slug_seed("!@acme.com", Some("!!")), "tenant");
    }

    #[test]
    fn namespace_mapping_is_the_adr046_prefix() {
        assert_eq!(namespace_for("acme"), "velox-t-acme");
        // The prefix is what makes a tenant slug unable to name a system ns.
        assert!(namespace_for("kube-system").starts_with(NAMESPACE_PREFIX));
    }

    #[test]
    fn display_name_is_never_blank() {
        assert_eq!(display_name_for("ops@acme.com", Some("Acme")), "Acme");
        assert_eq!(
            display_name_for("ops@acme.com", Some("   ")),
            "ops@acme.com"
        );
        assert_eq!(display_name_for("ops@acme.com", None), "ops@acme.com");
        assert_eq!(
            display_name_for("ops@acme.com", Some(&"z".repeat(400))).len(),
            120
        );
    }

    #[test]
    fn tokens_are_high_entropy_and_never_stored_raw() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), TOKEN_LEN);
        assert_ne!(a, b, "tokens must not repeat");
        assert!(
            a.chars().all(|c| c.is_ascii_alphanumeric()),
            "URL-safe only"
        );
        let h = token_hash(&a);
        assert_eq!(h.len(), 64, "sha256 hex");
        assert_ne!(h, a, "the raw token must never be what we store");
        assert_eq!(h, token_hash(&a), "hashing is deterministic");
        assert_ne!(h, token_hash(&b));
    }

    /// The timing-equalising hash has to actually verify, or the "unknown
    /// user" branch costs a string compare instead of a bcrypt round.
    #[test]
    fn dummy_hash_is_a_real_bcrypt_hash() {
        assert!(bcrypt::verify("anything", DUMMY_HASH).is_ok());
        assert!(!bcrypt::verify("anything", DUMMY_HASH).unwrap());
    }

    /// The flag is the whole safety story for shipping this on `develop`: with
    /// it off, no entry point may reach the datastore or create anything.
    #[test]
    fn every_entry_point_is_inert_while_the_flag_is_off() {
        // `enabled()` reads the process env; this test asserts the OFF branch,
        // which is the default in every environment that has not opted in.
        assert!(!enabled(), "VELOX_MULTITENANT_AUTH must default to OFF");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // No kube client, no Postgres: these would all error if the flag
            // check did not short-circuit before `db::connect_app`.
            assert!(signup("ops@acme.com", "longenough", None).await.is_err());
            assert!(!verify_email("whatever").await.unwrap());
            assert!(request_password_reset("ops@acme.com").await.is_ok());
            assert!(!reset_password("whatever", "longenough").await.unwrap());
            assert!(authenticate("ops@acme.com", "longenough")
                .await
                .unwrap()
                .is_none());
        });
    }

    // ── live-Postgres proof ────────────────────────────────────────────
    //
    // The flows above are mostly SQL, and SQL is not unit-testable without a
    // server. These are the real thing, ignored by default (the `db.rs`
    // precedent):
    //
    //   podman run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=t postgres:16-alpine
    //   VELOX_PG_TEST_URL=postgres://postgres:t@127.0.0.1:5433/postgres \
    //     cargo test -- --ignored --test-threads=1 tenants
    //
    // They talk to Postgres directly rather than through `db::connect_app`,
    // which needs a kube client for the credentials Secret. That means they
    // prove the SQL and the flow logic; the Secret-sourced connection is
    // covered by `db.rs` and by the live-fleet run.

    async fn fresh_db(url: &str) -> tokio_postgres::Client {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .expect("connect");
        tokio::spawn(conn);
        // A dedicated schema, not `public`: `db.rs`'s migration test needs a
        // virgin database, and these tests share whatever server the developer
        // pointed VELOX_PG_TEST_URL at. Dropping first also makes a re-run
        // clean without a fresh container.
        client
            .batch_execute(
                "DROP SCHEMA IF EXISTS velox_test_79 CASCADE;
                 CREATE SCHEMA velox_test_79;
                 SET search_path = velox_test_79, public;",
            )
            .await
            .expect("reset test schema");
        for sql in [
            include_str!("../migrations/001_init.sql"),
            include_str!("../migrations/002_auth_tokens.sql"),
        ] {
            client.batch_execute(sql).await.expect("migrate");
        }
        client
    }

    fn live_url() -> Option<String> {
        std::env::var("VELOX_PG_TEST_URL").ok()
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// Signup happy path: one user, one tenant, owner membership, a quota row,
    /// an audit row — and a verification token that is redeemable exactly once.
    #[test]
    #[ignore = "needs a live Postgres (set VELOX_PG_TEST_URL)"]
    fn tenants_signup_creates_the_whole_account_and_verification_is_single_use() {
        let url = live_url().expect("VELOX_PG_TEST_URL not set");
        rt().block_on(async {
            let mut pg = fresh_db(&url).await;
            let hash = bcrypt::hash("longenough", 4).unwrap();
            let (user_id, tenant) =
                create_user_and_tenant(&mut pg, "ops@acme.com", &hash, Some("Acme Corp"))
                    .await
                    .unwrap()
                    .expect("first signup creates a user");
            assert_eq!(tenant.slug, "acme-corp");
            assert_eq!(tenant.namespace, "velox-t-acme-corp");

            let t = pg
                .query_one(
                    "SELECT t.slug, t.namespace, tu.role, q.max_deployments
                     FROM tenants t
                     JOIN tenant_users tu ON tu.tenant_id = t.id
                     JOIN quotas q ON q.tenant_id = t.id",
                    &[],
                )
                .await
                .unwrap();
            assert_eq!(t.get::<_, String>(0), "acme-corp");
            assert_eq!(t.get::<_, String>(1), "velox-t-acme-corp");
            assert_eq!(t.get::<_, String>(2), "owner");
            assert_eq!(t.get::<_, i32>(3), 1);
            let audited: i64 = pg
                .query_one(
                    "SELECT count(*) FROM audit WHERE action = 'user.signup'",
                    &[],
                )
                .await
                .unwrap()
                .get(0);
            assert_eq!(audited, 1);

            // The same address again: no second user, no error.
            assert!(
                create_user_and_tenant(&mut pg, "ops@acme.com", &hash, None)
                    .await
                    .unwrap()
                    .is_none(),
                "a duplicate address must be a no-op, not a second account"
            );
            let users: i64 = pg
                .query_one("SELECT count(*) FROM users", &[])
                .await
                .unwrap()
                .get(0);
            assert_eq!(users, 1);

            // Verification token: redeemable once, then dead.
            let token = issue_token(&pg, "email_verification_tokens", &user_id, 24)
                .await
                .unwrap();
            let tx = pg.transaction().await.unwrap();
            assert_eq!(
                consume_token(&tx, "email_verification_tokens", &token)
                    .await
                    .unwrap(),
                Some(user_id.clone())
            );
            tx.commit().await.unwrap();
            let tx = pg.transaction().await.unwrap();
            assert_eq!(
                consume_token(&tx, "email_verification_tokens", &token)
                    .await
                    .unwrap(),
                None,
                "replaying a consumed link must not work"
            );
            assert_eq!(
                consume_token(&tx, "email_verification_tokens", "not-a-token")
                    .await
                    .unwrap(),
                None,
                "an unknown token is indistinguishable from a spent one"
            );
        });
    }

    /// The card's acceptance criterion, end to end: **a stranger signs up,
    /// verifies by email, logs in, resets their password, and logs in with the
    /// new one — all persisted.** Every step goes through the same code the
    /// HTTP handlers call, minus the Secret-sourced connection (see above).
    ///
    /// It also pins the two properties the rest of M1 depends on: the login
    /// carries a tenant id, and that id survives into a session cookie.
    #[test]
    #[ignore = "needs a live Postgres (set VELOX_PG_TEST_URL)"]
    fn tenants_a_stranger_can_sign_up_verify_log_in_and_reset() {
        let url = live_url().expect("VELOX_PG_TEST_URL not set");
        rt().block_on(async {
            let mut pg = fresh_db(&url).await;

            // 1. Sign up. The raw verification token comes back for delivery.
            let (email, verify_token, _tenant) = signup_on(
                &mut pg,
                " Ops@Acme.com ",
                "correct horse",
                Some("Acme Corp"),
            )
            .await
            .unwrap()
            .expect("a fresh address must create an account");
            assert_eq!(email, "ops@acme.com", "the stored identity is normalised");

            // 2. Unverified accounts cannot log in yet.
            assert!(
                authenticate_on(&pg, "ops@acme.com", "correct horse")
                    .await
                    .unwrap()
                    .is_none(),
                "an unverified address must not be able to log in"
            );

            // 3. Verify by email.
            assert!(verify_email_on(&mut pg, &verify_token).await.unwrap());

            // 4. Log in — and the principal carries the tenant.
            let principal = authenticate_on(&pg, "ops@acme.com", "correct horse")
                .await
                .unwrap()
                .expect("a verified account must log in");
            assert_eq!(principal.email, "ops@acme.com");
            let tenant_id = principal.tenant_id.clone();
            assert!(!tenant_id.is_empty());
            // The seam #80 builds on: the tenant survives into the cookie.
            let cookie = crate::auth::make_tenant_token(&principal.email, &tenant_id);
            assert_eq!(
                crate::auth::parse_token(&cookie).and_then(|s| s.tenant),
                Some(tenant_id),
                "the session must carry the tenant identity"
            );
            // Wrong password is still refused for a real, verified account.
            assert!(authenticate_on(&pg, "ops@acme.com", "wrong horse")
                .await
                .unwrap()
                .is_none());

            // 5. Reset the password.
            let (_, reset_token) = request_password_reset_on(&pg, "ops@acme.com")
                .await
                .unwrap()
                .expect("a known address gets a link");
            // An unknown address gets nothing — and says nothing.
            assert!(request_password_reset_on(&pg, "nobody@acme.com")
                .await
                .unwrap()
                .is_none());
            assert!(reset_password_on(&mut pg, &reset_token, "battery staple")
                .await
                .unwrap());
            // The link is spent.
            assert!(!reset_password_on(&mut pg, &reset_token, "third attempt")
                .await
                .unwrap());

            // 6. The new password works and the old one does not.
            assert!(authenticate_on(&pg, "ops@acme.com", "battery staple")
                .await
                .unwrap()
                .is_some());
            assert!(authenticate_on(&pg, "ops@acme.com", "correct horse")
                .await
                .unwrap()
                .is_none());
        });
    }

    /// The ADR-038 gate, on the path signup actually takes: a free mailbox is
    /// refused before anything is written.
    #[test]
    #[ignore = "needs a live Postgres (set VELOX_PG_TEST_URL)"]
    fn tenants_signup_refuses_free_mailboxes_and_weak_passwords() {
        let url = live_url().expect("VELOX_PG_TEST_URL not set");
        rt().block_on(async {
            let mut pg = fresh_db(&url).await;
            for (email, password, why) in [
                ("someone@gmail.com", "correct horse", "free mailbox"),
                ("someone@yahoo.com.br", "correct horse", "brand + suffix"),
                ("not-an-address", "correct horse", "malformed"),
                ("ops@acme.com", "short", "weak password"),
            ] {
                let err = signup_on(&mut pg, email, password, None)
                    .await
                    .expect_err(&format!("{why} must be refused: {email}"));
                assert!(!format!("{err:#}").is_empty());
            }
            let users: i64 = pg
                .query_one("SELECT count(*) FROM users", &[])
                .await
                .unwrap()
                .get(0);
            assert_eq!(users, 0, "a refused signup must write nothing");
        });
    }

    /// A slug collision does not fail the signup — it retries with a suffix,
    /// and both tenants end up with distinct, valid namespaces.
    #[test]
    #[ignore = "needs a live Postgres (set VELOX_PG_TEST_URL)"]
    fn tenants_slug_collision_falls_back_to_a_suffix() {
        let url = live_url().expect("VELOX_PG_TEST_URL not set");
        rt().block_on(async {
            let mut pg = fresh_db(&url).await;
            let hash = bcrypt::hash("longenough", 4).unwrap();
            for email in ["a@acme.com", "b@other.com"] {
                create_user_and_tenant(&mut pg, email, &hash, Some("Acme"))
                    .await
                    .unwrap()
                    .expect("both signups must succeed");
            }
            let rows = pg
                .query("SELECT slug, namespace FROM tenants ORDER BY slug", &[])
                .await
                .unwrap();
            assert_eq!(rows.len(), 2);
            let slugs: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
            assert_eq!(slugs[0], "acme");
            assert!(
                slugs[1].starts_with("acme-"),
                "expected a suffixed slug, got {slugs:?}"
            );
            for r in &rows {
                let ns: String = r.get(1);
                assert!(ns.starts_with("velox-t-") && ns.len() <= 63);
            }
        });
    }

    /// Expiry is enforced in SQL, so it needs a server to be proven at all.
    #[test]
    #[ignore = "needs a live Postgres (set VELOX_PG_TEST_URL)"]
    fn tenants_expired_tokens_are_refused_and_pruned() {
        let url = live_url().expect("VELOX_PG_TEST_URL not set");
        rt().block_on(async {
            let mut pg = fresh_db(&url).await;
            let hash = bcrypt::hash("longenough", 4).unwrap();
            let (user_id, _tenant) = create_user_and_tenant(&mut pg, "ops@acme.com", &hash, None)
                .await
                .unwrap()
                .unwrap();
            let token = issue_token(&pg, "password_reset_tokens", &user_id, 2)
                .await
                .unwrap();
            pg.execute(
                "UPDATE password_reset_tokens SET expires_at = now() - interval '1 minute'",
                &[],
            )
            .await
            .unwrap();
            let tx = pg.transaction().await.unwrap();
            assert_eq!(
                consume_token(&tx, "password_reset_tokens", &token)
                    .await
                    .unwrap(),
                None,
                "an expired link must not work"
            );
            tx.commit().await.unwrap();
            // Issuing the next one sweeps the expired row — no scheduler.
            issue_token(&pg, "password_reset_tokens", &user_id, 2)
                .await
                .unwrap();
            let live: i64 = pg
                .query_one("SELECT count(*) FROM password_reset_tokens", &[])
                .await
                .unwrap()
                .get(0);
            assert_eq!(live, 1, "spent/expired rows are pruned at issue time");
        });
    }
}
