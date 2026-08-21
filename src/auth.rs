// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Server-only authentication: a single admin credential + signed-cookie session.
//!
//! Credential resolution (ADR-023, first-run journey):
//!   1. **Managed Secret** `veloxsearch-credentials` (created by the first-run
//!      `/setup` screen): bcrypt password hash + a generated session secret.
//!   2. **Env** `VELOX_ADMIN_USER`/`VELOX_ADMIN_PASSWORD` (legacy/break-glass).
//!   3. Neither → **first-run mode**: every route is gated to `/setup`.
//! The session is a stateless HMAC-signed cookie. The Axum middleware gates ALL
//! routes — including the dangerous `/api/create_cluster` — except the login
//! page, the login endpoint, and static assets.

use anyhow::{bail, Context, Result};
use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use hmac::{Hmac, Mac};
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Api, Patch, PatchParams};
use sha2::Sha256;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

pub const COOKIE_NAME: &str = "velox_session";
const SESSION_TTL_SECS: u64 = 60 * 60 * 24; // 24h
/// The managed credentials Secret (ADR-003: K8s as the datastore).
const CREDS_SECRET: &str = "veloxsearch-credentials";

type HmacSha256 = Hmac<Sha256>;

/// Where credentials come from, cached after first load.
#[derive(Clone)]
enum AuthState {
    /// Not yet probed (or probe failed — retried next request).
    Unknown,
    /// No managed Secret and no env password → onboarding must run.
    FirstRun,
    /// Managed Secret present: bcrypt hash + persisted session secret.
    Managed {
        user: String,
        hash: String,
        session_secret: String,
    },
    /// Env-provided credentials (legacy/break-glass).
    Env,
}

static STATE: RwLock<AuthState> = RwLock::new(AuthState::Unknown);

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
fn admin_user() -> String {
    env_or("VELOX_ADMIN_USER", "admin")
}
fn admin_password() -> Option<String> {
    std::env::var("VELOX_ADMIN_PASSWORD").ok()
}
/// Session-signing secret: managed (persisted in the Secret) > env > dev default.
fn secret() -> String {
    if let AuthState::Managed { session_secret, .. } = &*STATE.read().unwrap() {
        return session_secret.clone();
    }
    env_or("VELOX_SESSION_SECRET", "dev-insecure-change-me")
}
fn secure_cookie() -> bool {
    env_or("VELOX_COOKIE_SECURE", "0") == "1"
}

fn secret_str(s: &Secret, key: &str) -> Option<String> {
    s.data
        .as_ref()
        .and_then(|d| d.get(key))
        .and_then(|b| String::from_utf8(b.0.clone()).ok())
}

/// Probe the managed Secret / env and cache the result. Probe failures are NOT
/// cached (cluster might be briefly unreachable) — they fall back to Env rules
/// for that request only.
async fn load_state() -> AuthState {
    {
        let cur = STATE.read().unwrap().clone();
        if !matches!(cur, AuthState::Unknown) {
            return cur;
        }
    }
    let probed = match probe().await {
        Ok(st) => {
            *STATE.write().unwrap() = st.clone();
            st
        }
        Err(e) => {
            tracing::warn!("auth state probe failed (not cached): {e:#}");
            if admin_password().is_some() {
                AuthState::Env
            } else {
                AuthState::FirstRun
            }
        }
    };
    probed
}

async fn probe() -> Result<AuthState> {
    let client = crate::k8s::client().await?;
    let api: Api<Secret> = Api::namespaced(client, crate::k8s::ns());
    if let Some(s) = api.get_opt(CREDS_SECRET).await? {
        let (user, hash, session_secret) = (
            secret_str(&s, "username"),
            secret_str(&s, "password_hash"),
            secret_str(&s, "session_secret"),
        );
        if let (Some(user), Some(hash), Some(session_secret)) = (user, hash, session_secret) {
            return Ok(AuthState::Managed {
                user,
                hash,
                session_secret,
            });
        }
        tracing::warn!("managed credentials secret is malformed; ignoring it");
    }
    Ok(if admin_password().is_some() {
        AuthState::Env
    } else {
        AuthState::FirstRun
    })
}

/// Is the app waiting for the first-run admin account to be created?
pub async fn is_first_run() -> bool {
    matches!(load_state().await, AuthState::FirstRun)
}

fn random_hex(bytes: usize) -> Result<String> {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")
        .context("opening /dev/urandom")?
        .read_exact(&mut buf)
        .context("reading entropy")?;
    Ok(hex::encode(buf))
}

/// First-run completion: validate, bcrypt-hash, persist the managed Secret,
/// refresh the cache. Only callable while in FirstRun state.
pub async fn complete_setup(username: &str, password: &str) -> Result<()> {
    if !matches!(load_state().await, AuthState::FirstRun) {
        bail!("setup has already been completed");
    }
    let username = username.trim();
    if username.is_empty() || username.len() > 64 {
        bail!("username must be 1-64 characters");
    }
    if password.len() < 8 {
        bail!("password must be at least 8 characters");
    }
    let hash = bcrypt::hash(password, bcrypt::DEFAULT_COST).context("hashing password")?;
    let session_secret = random_hex(32)?;

    let client = crate::k8s::client().await?;
    let api: Api<Secret> = Api::namespaced(client, crate::k8s::ns());
    let manifest = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": { "name": CREDS_SECRET, "namespace": crate::k8s::ns() },
        "type": "Opaque",
        "stringData": {
            "username": username,
            "password_hash": hash,
            "session_secret": session_secret,
        }
    });
    api.patch(
        CREDS_SECRET,
        &PatchParams::apply("veloxsearch").force(),
        &Patch::Apply(&manifest),
    )
    .await
    .context("persisting credentials secret")?;

    *STATE.write().unwrap() = AuthState::Managed {
        user: username.to_string(),
        hash,
        session_secret,
    };
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn sign(data: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret().as_bytes()).expect("HMAC key");
    mac.update(data.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut r = 0u8;
    for (x, y) in a.iter().zip(b) {
        r |= x ^ y;
    }
    r == 0
}

pub async fn check_credentials(user: &str, pass: &str) -> bool {
    match load_state().await {
        AuthState::Managed { user: u, hash, .. } => {
            let user_ok = constant_eq(user.as_bytes(), u.as_bytes());
            // bcrypt::verify is constant-time on the hash comparison.
            let pass_ok = bcrypt::verify(pass, &hash).unwrap_or(false);
            user_ok && pass_ok
        }
        AuthState::Env => {
            let user_ok = constant_eq(user.as_bytes(), admin_user().as_bytes());
            let pass_ok = admin_password()
                .map(|p| constant_eq(pass.as_bytes(), p.as_bytes()))
                .unwrap_or(false);
            user_ok && pass_ok
        }
        // FirstRun/Unknown: nothing to log into yet.
        _ => false,
    }
}

/// Who a valid cookie says you are.
///
/// `tenant` is the seam #80 builds ownership enforcement on: a handler that
/// must scope by tenant reads it here rather than trusting anything in the
/// request body. The admin session has no tenant — it is the installation
/// operator, not a member of an org — so `None` means "not tenant-scoped",
/// never "any tenant".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Admin username, or the user's email for a control-plane account.
    pub user: String,
    /// `tenants.id`, present only on a multi-tenant session (#79).
    pub tenant: Option<String>,
}

/// Marks a tenant-carrying token. A v1 token has three colon-separated fields
/// and a v2 token has five, so the two are told apart by shape and no admin
/// username can be mistaken for a version tag.
const TOKEN_V2: &str = "v2";

/// Admin token (unchanged, v1): `user:exp:hexsig`, signed over `user:exp`.
///
/// This is deliberately byte-identical to what shipped before #79: sessions
/// issued by the previous build stay valid across the upgrade, and with the
/// multi-tenant flag off it is the only shape ever minted.
pub fn make_token(user: &str) -> String {
    let data = format!("{user}:{}", now() + SESSION_TTL_SECS);
    let sig = sign(&data);
    format!("{data}:{sig}")
}

/// Tenant token (v2): `v2:user:tenant:exp:hexsig`, signed over everything
/// before the signature.
///
/// The tenant id is INSIDE the signed payload — a cookie whose tenant field
/// was edited fails the HMAC check, so the identity a handler reads is as
/// trustworthy as the session itself. `user` is an email and `tenant` a uuid,
/// neither of which can contain a colon, so the encoding stays unambiguous.
pub fn make_tenant_token(user: &str, tenant: &str) -> String {
    let data = format!("{TOKEN_V2}:{user}:{tenant}:{}", now() + SESSION_TTL_SECS);
    let sig = sign(&data);
    format!("{data}:{sig}")
}

/// Parse and verify a token: signature first, then expiry. `None` for anything
/// malformed, forged or stale — one answer, so a caller cannot accidentally
/// treat "expired" as "almost valid".
pub fn parse_token(token: &str) -> Option<Session> {
    let parts: Vec<&str> = token.split(':').collect();
    let (session, exp, sig) = match parts.as_slice() {
        [user, exp, sig] => (
            Session {
                user: (*user).to_string(),
                tenant: None,
            },
            *exp,
            *sig,
        ),
        [tag, user, tenant, exp, sig] if *tag == TOKEN_V2 => (
            Session {
                user: (*user).to_string(),
                tenant: Some((*tenant).to_string()),
            },
            *exp,
            *sig,
        ),
        _ => return None,
    };
    // The signed payload is everything up to the last field, so re-signing
    // never has to re-derive the format it is checking.
    let signed = &token[..token.len() - sig.len() - 1];
    if !constant_eq(sig.as_bytes(), sign(signed).as_bytes()) {
        return None;
    }
    matches!(exp.parse::<u64>(), Ok(e) if e > now()).then_some(session)
}

pub fn verify_token(token: &str) -> bool {
    parse_token(token).is_some()
}

/// The session carried by a `Cookie` header, if any. The one place cookie
/// splitting is implemented, so the middleware and the handlers cannot drift.
pub fn session_from_cookies(cookie_header: &str) -> Option<Session> {
    let prefix = format!("{COOKIE_NAME}=");
    cookie_header
        .split(';')
        .filter_map(|c| c.trim().strip_prefix(&prefix))
        .find_map(parse_token)
}

pub fn session_cookie(token: &str) -> String {
    let mut c = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}"
    );
    if secure_cookie() {
        c.push_str("; Secure");
    }
    c
}

pub fn clear_cookie() -> String {
    let mut c = format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure_cookie() {
        c.push_str("; Secure");
    }
    c
}

/// Static files served from the SPA bundle (tower-http `ServeDir`). Vite emits
/// everything under `/assets/` plus a handful of root files (favicon, manifest);
/// they all carry an extension, so "the last path segment contains a dot" is a
/// robust catch-all that never matches the extensionless SPA routes (/, /login,
/// /setup, /d/:name).
fn is_asset(path: &str) -> bool {
    path == "/favicon.ico"
        || path.starts_with("/assets/")
        || path.rsplit('/').next().is_some_and(|seg| seg.contains('.'))
}

fn is_public(path: &str) -> bool {
    path == "/login"
        || path == "/api/login"
        // The SPA probes auth state before it can show anything.
        || path == "/api/auth_state"
        || is_tenant_auth_public(path)
        || is_asset(path)
}

/// The self-serve account endpoints (#79). Necessarily unauthenticated — a
/// stranger has no session — so they are opened ONLY while the multi-tenant
/// flag is on. With the flag off the guard does not know these paths exist and
/// they 401/redirect like any other unknown API route, which is what keeps
/// this card shippable on `develop` with no reachable new surface.
fn is_tenant_auth_public(path: &str) -> bool {
    matches!(
        path,
        "/api/signup" | "/api/verify_email" | "/api/request_password_reset" | "/api/reset_password"
    ) && crate::tenants::enabled()
}

/// Paths reachable in first-run mode — ONLY the setup flow (plus the auth probe
/// the SPA uses to discover it IS first-run). Everything else (especially
/// cluster-mutating APIs) stays sealed until an admin exists.
fn is_setup(path: &str) -> bool {
    path == "/setup" || path == "/api/setup_admin" || path == "/api/auth_state" || is_asset(path)
}

fn has_valid_session(req: &Request) -> bool {
    req.headers()
        .get(header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .and_then(session_from_cookies)
        .is_some()
}

/// Axum middleware. First-run mode funnels everything to /setup; afterwards,
/// allow public paths and valid sessions, redirect browser navigations to
/// /login, and reject API calls with 401.
pub async fn auth_guard(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if is_asset(&path) {
        return next.run(req).await;
    }
    if is_first_run().await {
        return if is_setup(&path) {
            next.run(req).await
        } else if path.starts_with("/api/") {
            StatusCode::UNAUTHORIZED.into_response()
        } else {
            Redirect::to("/setup").into_response()
        };
    }
    if path == "/setup" {
        // Setup is done — never show it again.
        return Redirect::to("/").into_response();
    }
    if is_public(&path) || has_valid_session(&req) {
        next.run(req).await
    } else if path.starts_with("/api/") {
        StatusCode::UNAUTHORIZED.into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The admin token is the one shape that existed before #79. Sessions
    /// minted by the previous build must still parse after the upgrade, so its
    /// encoding is pinned here rather than merely exercised.
    #[test]
    fn admin_token_shape_is_unchanged_and_carries_no_tenant() {
        let token = make_token("admin");
        assert_eq!(token.split(':').count(), 3, "v1 is user:exp:sig");
        assert!(token.starts_with("admin:"));
        assert_eq!(
            parse_token(&token),
            Some(Session {
                user: "admin".to_string(),
                tenant: None,
            })
        );
    }

    /// The seam #80 builds on: the tenant id comes back out of the cookie.
    #[test]
    fn tenant_token_round_trips_the_tenant_id() {
        let token = make_tenant_token("ops@acme.com", "7f1c2a3e-0000-4000-8000-000000000001");
        assert_eq!(
            parse_token(&token),
            Some(Session {
                user: "ops@acme.com".to_string(),
                tenant: Some("7f1c2a3e-0000-4000-8000-000000000001".to_string()),
            })
        );
    }

    /// The whole reason the tenant lives inside the signed payload: swapping it
    /// for someone else's must invalidate the cookie, not re-scope the session.
    #[test]
    fn editing_the_tenant_field_invalidates_the_token() {
        let token = make_tenant_token("ops@acme.com", "tenant-a");
        let forged = token.replace("tenant-a", "tenant-b");
        assert_ne!(forged, token);
        assert!(
            parse_token(&forged).is_none(),
            "a re-tenanted cookie must not verify"
        );
        // Same for the user field, and for the signature itself.
        assert!(parse_token(&token.replace("ops@", "root@")).is_none());
        assert!(parse_token(&format!("{token}0")).is_none());
    }

    #[test]
    fn expired_tokens_are_refused_in_both_versions() {
        // Signed correctly, but with an expiry in the past.
        let v1 = format!("admin:1:{}", sign("admin:1"));
        assert!(parse_token(&v1).is_none());
        let v2_data = format!("{TOKEN_V2}:ops@acme.com:tenant-a:1");
        let v2 = format!("{v2_data}:{}", sign(&v2_data));
        assert!(parse_token(&v2).is_none());
    }

    #[test]
    fn malformed_tokens_are_refused() {
        for bad in [
            "",
            "admin",
            "admin:123",
            "admin:notanumber:deadbeef",
            // Five fields but not tagged v2 — must not be read as a tenant token.
            "v1:ops@acme.com:tenant-a:99999999999:deadbeef",
        ] {
            assert!(parse_token(bad).is_none(), "{bad:?} must not parse");
        }
    }

    /// Cookie headers carry more than ours; the session must be found among
    /// them and unaffected by neighbours that happen to look token-shaped.
    #[test]
    fn session_is_picked_out_of_a_shared_cookie_header() {
        let token = make_tenant_token("ops@acme.com", "tenant-a");
        let header = format!("theme=dark; {COOKIE_NAME}={token}; other=a:b:c");
        assert_eq!(
            session_from_cookies(&header).map(|s| s.tenant),
            Some(Some("tenant-a".to_string()))
        );
        assert!(session_from_cookies("theme=dark; other=a:b:c").is_none());
        assert!(session_from_cookies(&format!("{COOKIE_NAME}=garbage")).is_none());
    }

    /// The flag is what keeps this card inert on `develop`: with it off the
    /// account endpoints are not public, so the guard treats them like any
    /// other unauthenticated API call.
    #[test]
    fn account_endpoints_are_not_public_while_the_flag_is_off() {
        assert!(!crate::tenants::enabled(), "flag must default to OFF");
        for path in [
            "/api/signup",
            "/api/verify_email",
            "/api/request_password_reset",
            "/api/reset_password",
        ] {
            assert!(
                !is_public(path),
                "{path} must stay sealed with the flag off"
            );
        }
        // The pre-existing public set is untouched either way.
        assert!(is_public("/api/login"));
        assert!(is_public("/api/auth_state"));
        assert!(is_public("/login"));
    }
}
