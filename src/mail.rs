// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Outbound transactional mail for the control-plane account flows (#79).
//!
//! Two jobs and no more: build the verification / password-reset messages, and
//! hand them to an SMTP relay the operator configures with env
//! (`veloxsearch-env` ConfigMap for the host/port/TLS, a Secret for the
//! password — see docs/auth/accounts.md).
//!
//! **The dev-mode fallback is the important part.** With no `VELOX_SMTP_HOST`
//! there is no relay, and the mailer LOGS the link at `warn!` instead of
//! failing. That makes the whole flow exercisable on a laptop or in a
//! port-forwarded cluster without standing up a mail server — the link is in
//! `kubectl logs`. It is loud on purpose: a production deployment that forgot
//! to configure SMTP prints one warning per signup, and account links in a log
//! sink is a fact an operator must be able to see.
//!
//! Delivery is never allowed to decide whether an account exists: callers send
//! best-effort and report the same answer either way (see the enumeration
//! discipline in `tenants`). A relay outage costs a re-request, not a signup.

use anyhow::{Context, Result};

/// Where the links point. The API endpoints accept the raw token directly, so
/// this only has to be right for humans clicking mail; the SPA routes it names
/// land with the frontend card.
const DEFAULT_PUBLIC_URL: &str = "http://localhost:3000";
const DEFAULT_FROM: &str = "VeloxSearch <no-reply@veloxsearch.ai>";

/// How to speak to the relay. Mirrors the three shapes a real relay offers;
/// `None` is plaintext, for an in-cluster relay on the pod network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// Connect in the clear, then `STARTTLS` (submission, :587). The default.
    StartTls,
    /// TLS from the first byte (implicit TLS / submissions, :465).
    Wrapper,
    /// No TLS at all (:25 to an in-cluster relay).
    None,
}

impl TlsMode {
    fn parse(v: &str) -> Result<Self> {
        match v.trim().to_ascii_lowercase().as_str() {
            "starttls" => Ok(Self::StartTls),
            "tls" | "wrapper" | "implicit" => Ok(Self::Wrapper),
            "none" | "plain" | "off" => Ok(Self::None),
            other => anyhow::bail!("VELOX_SMTP_TLS must be starttls|tls|none, got {other:?}"),
        }
    }
    fn default_port(self) -> u16 {
        match self {
            Self::StartTls => 587,
            Self::Wrapper => 465,
            Self::None => 25,
        }
    }
}

/// Resolved relay settings. Absent (`None` from [`smtp_config`]) means
/// dev-mode: log the message instead of sending it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub user: Option<String>,
    pub password: Option<String>,
}

/// Read the relay config from the process env. `Ok(None)` = no
/// `VELOX_SMTP_HOST` = dev-mode. `Err` = a host WAS configured but something
/// alongside it is unusable — that is an operator mistake and must not
/// silently degrade into logging account links.
pub fn smtp_config() -> Result<Option<SmtpConfig>> {
    let var = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    resolve_smtp(
        var("VELOX_SMTP_HOST"),
        var("VELOX_SMTP_PORT"),
        var("VELOX_SMTP_TLS"),
        var("VELOX_SMTP_USER"),
        var("VELOX_SMTP_PASSWORD"),
    )
}

/// Pure resolution, unit-testable without touching process env (the
/// `db::resolve_config` idiom).
fn resolve_smtp(
    host: Option<String>,
    port: Option<String>,
    tls: Option<String>,
    user: Option<String>,
    password: Option<String>,
) -> Result<Option<SmtpConfig>> {
    let Some(host) = host else {
        return Ok(None);
    };
    let tls = match tls {
        Some(v) => TlsMode::parse(&v)?,
        None => TlsMode::StartTls,
    };
    let port = match port {
        Some(p) => p
            .parse::<u16>()
            .with_context(|| format!("VELOX_SMTP_PORT is not a valid port: {p:?}"))?,
        None => tls.default_port(),
    };
    // A username with no password would fail the AUTH exchange at send time,
    // deep inside a best-effort path where the error is only a log line.
    // Catch it at config time instead.
    if user.is_some() && password.is_none() {
        anyhow::bail!("VELOX_SMTP_USER is set but VELOX_SMTP_PASSWORD is not");
    }
    Ok(Some(SmtpConfig {
        host,
        port,
        tls,
        user,
        password,
    }))
}

/// Base URL the emailed links are built from, without a trailing slash.
pub fn public_url() -> String {
    std::env::var("VELOX_PUBLIC_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PUBLIC_URL.to_string())
}

fn from_address() -> String {
    std::env::var("VELOX_MAIL_FROM")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_FROM.to_string())
}

/// Send a plain-text message, or log it when no relay is configured.
///
/// Errors describe a delivery failure only. Callers treat them as
/// non-fatal — see the module docs.
pub async fn send(to: &str, subject: &str, body: &str) -> Result<()> {
    let Some(cfg) = smtp_config()? else {
        // Dev-mode. The body carries the link, so this is the whole flow.
        tracing::warn!(
            to,
            subject,
            "VELOX_SMTP_HOST unset — logging this mail instead of sending it:\n{body}"
        );
        return Ok(());
    };
    send_via(&cfg, to, subject, body).await
}

async fn send_via(cfg: &SmtpConfig, to: &str, subject: &str, body: &str) -> Result<()> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    let message = Message::builder()
        .from(from_address().parse().context("VELOX_MAIL_FROM is not a valid address")?)
        .to(to.parse().with_context(|| format!("invalid recipient {to:?}"))?)
        .subject(subject)
        .header(lettre::message::header::ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .context("building message")?;

    let mut builder = match cfg.tls {
        TlsMode::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)
            .context("building STARTTLS relay")?,
        TlsMode::Wrapper => {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host).context("building TLS relay")?
        }
        TlsMode::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.host),
    }
    .port(cfg.port);
    if let (Some(user), Some(password)) = (&cfg.user, &cfg.password) {
        builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
    }
    builder
        .build()
        .send(message)
        .await
        .with_context(|| format!("sending mail via {}:{}", cfg.host, cfg.port))?;
    Ok(())
}

// ─────────────────────────── message bodies ───────────────────────────
//
// Plain text, no HTML: these are two links and a sentence, and a text/plain
// body renders identically in every client and never trips a spam filter for
// its markup. English only for now — the SPA's i18n is a frontend concern and
// the locale is not part of a signup yet.

pub fn verification_subject() -> &'static str {
    "Confirm your VeloxSearch email address"
}

pub fn verification_body(token: &str, ttl_hours: i64) -> String {
    let base = public_url();
    format!(
        "Welcome to VeloxSearch.\n\n\
         Confirm this address to activate your account:\n\n\
         {base}/verify-email?token={token}\n\n\
         The link is valid for {ttl_hours} hours and can be used once.\n\
         If you did not create a VeloxSearch account, ignore this message —\n\
         nothing was activated.\n"
    )
}

pub fn reset_subject() -> &'static str {
    "Reset your VeloxSearch password"
}

pub fn reset_body(token: &str, ttl_hours: i64) -> String {
    let base = public_url();
    format!(
        "A password reset was requested for this VeloxSearch account.\n\n\
         Choose a new password:\n\n\
         {base}/reset-password?token={token}\n\n\
         The link is valid for {ttl_hours} hours and can be used once.\n\
         If you did not request this, ignore this message — your current\n\
         password still works and nothing has changed.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// No host = dev-mode, which is what makes the flow runnable with no relay.
    #[test]
    fn no_host_means_dev_mode() {
        assert_eq!(resolve_smtp(None, s("587"), s("tls"), None, None).unwrap(), None);
    }

    #[test]
    fn port_defaults_follow_the_tls_mode() {
        let port = |tls: Option<String>| {
            resolve_smtp(s("relay.example"), None, tls, None, None)
                .unwrap()
                .unwrap()
                .port
        };
        assert_eq!(port(None), 587, "STARTTLS submission is the default");
        assert_eq!(port(s("starttls")), 587);
        assert_eq!(port(s("tls")), 465);
        assert_eq!(port(s("none")), 25);
    }

    #[test]
    fn explicit_port_wins_and_bad_input_is_an_error() {
        let cfg = resolve_smtp(s("relay.example"), s("2525"), s("none"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(cfg.port, 2525);
        assert_eq!(cfg.tls, TlsMode::None);
        assert!(resolve_smtp(s("relay.example"), s("nope"), None, None, None).is_err());
        assert!(resolve_smtp(s("relay.example"), None, s("ssl-maybe"), None, None).is_err());
    }

    /// A half-configured credential fails at config time, not inside the
    /// best-effort send where the error is only a log line.
    #[test]
    fn user_without_password_is_rejected() {
        assert!(resolve_smtp(s("relay.example"), None, None, s("bot"), None).is_err());
        let cfg = resolve_smtp(s("relay.example"), None, None, s("bot"), s("pw"))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.user.as_deref(), Some("bot"));
        // Anonymous relays are legitimate (in-cluster, IP-allowlisted).
        assert!(resolve_smtp(s("relay.example"), None, None, None, None)
            .unwrap()
            .unwrap()
            .user
            .is_none());
    }

    /// The body IS the dev-mode delivery mechanism — if the token is not in it,
    /// the fallback logs a link nobody can use.
    #[test]
    fn bodies_carry_the_token_and_the_ttl() {
        for body in [verification_body("tok123", 24), reset_body("tok123", 2)] {
            assert!(body.contains("tok123"), "token missing from mail body");
        }
        assert!(verification_body("t", 24).contains("valid for 24 hours"));
        assert!(reset_body("t", 2).contains("valid for 2 hours"));
        assert!(verification_body("t", 24).contains("/verify-email?token=t"));
        assert!(reset_body("t", 2).contains("/reset-password?token=t"));
    }
}
