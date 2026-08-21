// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Pre-save reachability probe for an auth provider (ADR-045 MR2, #56).
//!
//! Auth misconfiguration is otherwise discovered as "the cluster restarted and
//! nobody can log in": the securityconfig only fails at `securityadmin` time,
//! minutes later, on a cluster that has already rolled. So the screen tests
//! first and this module answers what it could actually reach.
//!
//! It NEVER writes anything, and a failure never persists a spec. What it can
//! confirm today, per kind:
//!   * `oidc`  — fetch the discovery document, check the endpoints the plugin
//!     will use (`issuer`, `authorization_endpoint`, `jwks_uri`).
//!   * `saml`  — fetch the IdP metadata and confirm it is an `EntityDescriptor`
//!     for the entity ID that was configured.
//!   * `ldap`  — TCP reachability of every configured server, then, against the
//!     first: the TLS handshake (LDAPS or StartTLS, trusting
//!     `pemtrustedcas_content` when given), a bind as `bind_dn`, a sample user
//!     search through `usersearch` under `userbase`, and the group resolution
//!     that decides whether anyone gets a role at all (MR4, #56).
//!   * `jwt` / `proxy` / `internal` — nothing to reach; reported as such.

use crate::auth_provider::{AuthProvider, AuthProviderSpec};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct ProbeResult {
    pub ok: bool,
    /// What was actually resolved, one line per check — shown inline next to
    /// the form, not as a toast.
    pub checks: Vec<String>,
    /// The upstream failure, verbatim. Never a rewritten "something went wrong".
    pub error: Option<String>,
}

impl ProbeResult {
    fn ok(checks: Vec<String>) -> Self {
        Self {
            ok: true,
            checks,
            error: None,
        }
    }
    fn fail(checks: Vec<String>, error: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            checks,
            error: Some(error.to_string()),
        }
    }
}

pub async fn probe(spec: &AuthProviderSpec) -> ProbeResult {
    match &spec.provider {
        AuthProvider::Internal => ProbeResult::ok(vec![
            "Local accounts only — nothing external to reach.".into(),
        ]),
        AuthProvider::Ldap(c) => probe_ldap(c).await,
        AuthProvider::Oidc(c) => probe_oidc(c).await,
        AuthProvider::Saml(c) => probe_saml(c).await,
        AuthProvider::Jwt(_) => ProbeResult::ok(vec![
            "Tokens are validated locally against the signing key — nothing to reach.".into(),
        ]),
        AuthProvider::Proxy(_) => ProbeResult::ok(vec![
            "Identity is asserted by the proxy in front of the cluster — nothing to reach.".into(),
        ]),
    }
}

/// The upstream failure with its causes appended, not just its category.
///
/// `reqwest::Error` renders as `error sending request for url (…)` and hides
/// everything that names the actual mistake — `invalid peer certificate:
/// UnknownIssuer`, `Connection refused`, a DNS failure — one or two links down
/// the `source()` chain. The screen promises the exact upstream error (ADR-045,
/// UI rule 5); without this it shows a category and the operator has no idea
/// which field is wrong. Found while probing a SAML IdP behind a private CA
/// (MR4, #56).
fn cause_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(c) = src {
        let text = c.to_string();
        // Some layers restate their cause; do not print it twice.
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        src = c.source();
    }
    out
}

/// A client that trusts the platform roots plus, when given, the customer's own
/// CA — the common case for an internal IdP.
fn http_client(extra_ca: &str) -> anyhow::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder().timeout(TIMEOUT);
    let pem = extra_ca.trim();
    if !pem.is_empty() {
        // `Certificate::from_pem` defers parsing, so a pasted private key or a
        // truncated block would surface much later as an opaque TLS error.
        // Check the marker here, where we can name the actual mistake.
        if !pem.contains("-----BEGIN CERTIFICATE-----") {
            anyhow::bail!(
                "the pasted CA is not valid PEM — expected a block starting with \
                 -----BEGIN CERTIFICATE-----"
            );
        }
        b = b.add_root_certificate(
            reqwest::Certificate::from_pem(pem.as_bytes())
                .map_err(|e| anyhow::anyhow!("the pasted CA certificate is not valid PEM: {e}"))?,
        );
    }
    Ok(b.build()?)
}

async fn probe_oidc(c: &crate::auth_provider::OidcConfig) -> ProbeResult {
    let mut checks = Vec::new();
    let client = match http_client(&c.pemtrustedcas_content) {
        Ok(c) => c,
        Err(e) => return ProbeResult::fail(checks, e),
    };
    let resp = match client.get(c.connect_url.trim()).send().await {
        Ok(r) => r,
        Err(e) => return ProbeResult::fail(checks, cause_chain(&e)),
    };
    if !resp.status().is_success() {
        return ProbeResult::fail(
            checks,
            format!("the discovery URL answered {}", resp.status()),
        );
    }
    checks.push(format!("discovery document reachable ({})", resp.status()));
    let doc: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return ProbeResult::fail(checks, format!("not a JSON discovery document: {e}")),
    };
    // The three fields the security plugin and Dashboards actually consume.
    for key in ["issuer", "authorization_endpoint", "jwks_uri"] {
        match doc.get(key).and_then(|v| v.as_str()) {
            Some(v) => checks.push(format!("{key} = {v}")),
            None => {
                return ProbeResult::fail(
                    checks,
                    format!("the discovery document has no '{key}' — the sign-in flow needs it"),
                )
            }
        }
    }
    if let Some(t) = doc.get("token_endpoint").and_then(|v| v.as_str()) {
        checks.push(format!("token_endpoint = {t}"));
    }
    ProbeResult::ok(checks)
}

async fn probe_saml(c: &crate::auth_provider::SamlConfig) -> ProbeResult {
    let mut checks = Vec::new();
    // The IdP's own CA, when it has a private one — without this the probe
    // could never reach an ADFS behind a corporate PKI, and refused to save a
    // configuration that would in fact have worked (MR4, #56).
    let client = match http_client(&c.pemtrustedcas_content) {
        Ok(c) => c,
        Err(e) => return ProbeResult::fail(checks, e),
    };
    let resp = match client.get(c.metadata_url.trim()).send().await {
        Ok(r) => r,
        Err(e) => return ProbeResult::fail(checks, cause_chain(&e)),
    };
    if !resp.status().is_success() {
        return ProbeResult::fail(
            checks,
            format!("the metadata URL answered {}", resp.status()),
        );
    }
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => return ProbeResult::fail(checks, cause_chain(&e)),
    };
    if !body.contains("EntityDescriptor") {
        return ProbeResult::fail(
            checks,
            "the URL answered, but the body is not SAML metadata (no EntityDescriptor)",
        );
    }
    checks.push("metadata reachable and parseable as SAML".into());
    let entity = c.idp_entity_id.trim();
    if !entity.is_empty() && !body.contains(entity) {
        return ProbeResult::fail(
            checks,
            format!("the metadata does not mention the entity ID '{entity}'"),
        );
    }
    if !entity.is_empty() {
        checks.push(format!("entity ID '{entity}' present in the metadata"));
    }
    if body.contains("SingleSignOnService") {
        checks.push("SingleSignOnService endpoint advertised".into());
    }
    ProbeResult::ok(checks)
}

/// Escape the characters that would otherwise change the meaning of an LDAP
/// filter (RFC 4515). The user DN is interpolated into `rolesearch`, and a DN
/// legitimately contains parentheses — unescaped, a search for
/// `(member=cn=A (B),…)` silently matches nothing and the probe would report
/// "0 groups" for a directory that is in fact fine.
fn escape_filter_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for ch in v.chars() {
        match ch {
            '\\' => out.push_str("\\5c"),
            '*' => out.push_str("\\2a"),
            '(' => out.push_str("\\28"),
            ')' => out.push_str("\\29"),
            '\0' => out.push_str("\\00"),
            _ => out.push(ch),
        }
    }
    out
}

/// Root store for an LDAPS/StartTLS dial: the platform roots plus the
/// customer's own CA when one was pasted (same posture as `http_client`).
#[cfg(feature = "ssr")]
fn ldap_tls_config(extra_ca: &str) -> anyhow::Result<std::sync::Arc<rustls::ClientConfig>> {
    use anyhow::Context;
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(c);
    }
    let pem = extra_ca.trim();
    if !pem.is_empty() {
        let mut added = 0usize;
        // `PemObject` is the maintained home of the parser `rustls-pemfile`
        // used to wrap (RUSTSEC-2025-0134); same bytes, same iteration.
        use rustls_pki_types::pem::PemObject;
        for cert in rustls_pki_types::CertificateDer::pem_slice_iter(pem.as_bytes()) {
            roots
                .add(cert.context("the CA certificate is not valid PEM")?)
                .context("the CA certificate could not be added to the trust store")?;
            added += 1;
        }
        if added == 0 {
            anyhow::bail!("the CA certificate is not valid PEM");
        }
    }
    Ok(std::sync::Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// TCP-reach every configured server, then bind and search against the first —
/// the whole point of a pre-save probe is that "reachable" is not the question
/// the user is asking (ADR-045: bind as `bind_dn`, sample user search returning
/// the matched DN and its groups).
async fn probe_ldap(c: &crate::auth_provider::LdapConfig) -> ProbeResult {
    use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry};

    let mut checks = Vec::new();
    let hosts: Vec<&str> = c
        .hosts
        .iter()
        .map(|h| h.trim())
        .filter(|h| !h.is_empty())
        .collect();
    if hosts.is_empty() {
        return ProbeResult::fail(checks, "no directory server to test");
    }
    // Every server, not just the first: the security plugin fails over across
    // the whole list, so one dead member is a latent outage worth naming now.
    for host in &hosts {
        match tokio::time::timeout(TIMEOUT, tokio::net::TcpStream::connect(host)).await {
            Ok(Ok(_)) => checks.push(format!("{host} reachable")),
            Ok(Err(e)) => return ProbeResult::fail(checks, format!("{host}: {e}")),
            Err(_) => {
                return ProbeResult::fail(checks, format!("{host}: timed out after {TIMEOUT:?}"))
            }
        }
    }

    let host = hosts[0];
    let scheme = if c.enable_ssl { "ldaps" } else { "ldap" };
    let url = format!("{scheme}://{host}");
    let mut settings = LdapConnSettings::new()
        .set_conn_timeout(TIMEOUT)
        .set_starttls(c.enable_start_tls);
    if c.enable_ssl || c.enable_start_tls {
        match ldap_tls_config(&c.pemtrustedcas_content) {
            Ok(cfg) => settings = settings.set_config(cfg),
            Err(e) => return ProbeResult::fail(checks, format!("{e:#}")),
        }
    }

    let (conn, mut ldap) = match LdapConnAsync::with_settings(settings, &url).await {
        Ok(v) => v,
        Err(e) => return ProbeResult::fail(checks, format!("{url}: {e}")),
    };
    ldap3::drive!(conn);
    if c.enable_ssl || c.enable_start_tls {
        checks.push(format!(
            "TLS established ({})",
            if c.enable_ssl { "LDAPS" } else { "StartTLS" }
        ));
    }

    // Bind. An empty bind DN is an explicit anonymous bind, which validate()
    // already forced to pair with an empty password.
    let bind = if c.bind_dn.trim().is_empty() {
        ldap.simple_bind("", "").await
    } else {
        ldap.simple_bind(c.bind_dn.trim(), &c.bind_password).await
    };
    match bind.and_then(|r| r.success()) {
        Ok(_) if c.bind_dn.trim().is_empty() => checks.push("anonymous bind accepted".into()),
        Ok(_) => checks.push(format!("bind OK as {}", c.bind_dn.trim())),
        Err(e) => {
            let _ = ldap.unbind().await;
            return ProbeResult::fail(
                checks,
                format!("bind as '{}' failed: {e}", c.bind_dn.trim()),
            );
        }
    }

    // Sample user search: the configured filter with `{0}` widened to `*`, so
    // the probe proves the base and the filter resolve without needing the
    // operator to name a test account.
    let user_filter = c.usersearch.replace("{0}", "*");
    let user_attr = if c.username_attribute.trim().is_empty() {
        "uid"
    } else {
        c.username_attribute.trim()
    };
    let found = ldap
        .search(
            c.userbase.trim(),
            Scope::Subtree,
            &user_filter,
            vec![user_attr],
        )
        .await
        .and_then(|r| r.success());
    let entries = match found {
        Ok((rs, _)) => rs,
        Err(e) => {
            let _ = ldap.unbind().await;
            return ProbeResult::fail(
                checks,
                format!(
                    "user search {user_filter} under '{}' failed: {e}",
                    c.userbase.trim()
                ),
            );
        }
    };
    if entries.is_empty() {
        let _ = ldap.unbind().await;
        return ProbeResult::fail(
            checks,
            format!(
                "no user matched {user_filter} under '{}' — check the user base and the search filter",
                c.userbase.trim()
            ),
        );
    }
    let sample = SearchEntry::construct(entries[0].clone());
    let sample_name = sample
        .attrs
        .get(user_attr)
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or_default();
    checks.push(format!(
        "{} users matched — sample: {}",
        entries.len(),
        sample.dn
    ));

    // Role resolution, the half that decides whether anyone gets a role at all.
    if !c.rolebase.trim().is_empty() {
        let role_filter = c
            .rolesearch
            .replace("{0}", &escape_filter_value(&sample.dn))
            .replace("{1}", &escape_filter_value(&sample_name))
            .replace("{2}", &escape_filter_value(&sample_name));
        let rolename = if c.rolename.trim().is_empty() {
            "cn"
        } else {
            c.rolename.trim()
        };
        match ldap
            .search(
                c.rolebase.trim(),
                Scope::Subtree,
                &role_filter,
                vec![rolename],
            )
            .await
            .and_then(|r| r.success())
        {
            Ok((rs, _)) => {
                let names: Vec<String> = rs
                    .into_iter()
                    .map(|e| {
                        let e = SearchEntry::construct(e);
                        e.attrs
                            .get(rolename)
                            .and_then(|v| v.first())
                            .cloned()
                            .unwrap_or(e.dn)
                    })
                    .collect();
                if names.is_empty() {
                    // Not a failure: a directory may legitimately carry groups
                    // this sample user is not in. Say it plainly instead of
                    // reporting a green that hides an empty mapping.
                    checks.push(format!(
                        "no group matched for {} — users will authenticate with no role until the mapping resolves",
                        sample.dn
                    ));
                } else {
                    checks.push(format!(
                        "{} groups found for the sample user: {}",
                        names.len(),
                        names.join(", ")
                    ));
                }
            }
            Err(e) => {
                let _ = ldap.unbind().await;
                return ProbeResult::fail(
                    checks,
                    format!(
                        "group search {role_filter} under '{}' failed: {e}",
                        c.rolebase.trim()
                    ),
                );
            }
        }
    } else if !c.userrolename.trim().is_empty() {
        // Active-Directory shape: groups come off the user entry itself.
        match ldap
            .search(
                c.userbase.trim(),
                Scope::Subtree,
                &user_filter,
                vec![c.userrolename.trim()],
            )
            .await
            .and_then(|r| r.success())
        {
            Ok((rs, _)) if !rs.is_empty() => {
                let e = SearchEntry::construct(rs[0].clone());
                let groups = e
                    .attrs
                    .get(c.userrolename.trim())
                    .cloned()
                    .unwrap_or_default();
                checks.push(format!(
                    "{} groups on the sample user's '{}' attribute",
                    groups.len(),
                    c.userrolename.trim()
                ));
            }
            Ok(_) => checks.push(format!(
                "no '{}' attribute on the sample user",
                c.userrolename.trim()
            )),
            Err(e) => {
                let _ = ldap.unbind().await;
                return ProbeResult::fail(checks, format!("group attribute lookup failed: {e}"));
            }
        }
    }

    let _ = ldap.unbind().await;
    ProbeResult::ok(checks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_provider::{JwtConfig, LdapConfig, ProxyConfig};

    #[tokio::test]
    async fn kinds_with_nothing_to_reach_pass_without_network() {
        for p in [
            AuthProvider::Internal,
            AuthProvider::Jwt(JwtConfig {
                signing_key: "k".into(),
                ..Default::default()
            }),
            AuthProvider::Proxy(ProxyConfig {
                user_header: "x-user".into(),
                internal_proxies: ".*".into(),
                ..Default::default()
            }),
        ] {
            let r = probe(&AuthProviderSpec {
                provider: p,
                role_mappings: vec![],
            })
            .await;
            assert!(r.ok);
            assert!(r.error.is_none());
            assert_eq!(r.checks.len(), 1);
        }
    }

    #[tokio::test]
    async fn ldap_without_servers_fails_instead_of_passing_empty() {
        let r = probe(&AuthProviderSpec {
            provider: AuthProvider::Ldap(LdapConfig::default()),
            role_mappings: vec![],
        })
        .await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("no directory server"));
    }

    #[tokio::test]
    async fn a_reachable_port_that_is_not_a_directory_fails_the_probe() {
        // A socket that accepts TCP but speaks no LDAP: the whole point of MR4
        // is that this used to pass as "reachable" and now must not — a green
        // probe has to mean the bind and the searches actually worked.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            // Accept and hold the connection open without ever answering.
            if let Ok((s, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(30)).await;
                drop(s);
            }
        });
        let r = probe(&AuthProviderSpec {
            provider: AuthProvider::Ldap(LdapConfig {
                hosts: vec![addr.clone()],
                bind_dn: "cn=admin,dc=x".into(),
                bind_password: "p".into(),
                userbase: "ou=people,dc=x".into(),
                usersearch: "(uid={0})".into(),
                ..Default::default()
            }),
            role_mappings: vec![],
        })
        .await;
        assert!(
            !r.ok,
            "a non-LDAP socket must not probe green: {:?}",
            r.checks
        );
        // Reachability is still reported — it is a real, useful check — but it
        // is no longer the verdict.
        assert!(r.checks.iter().any(|c| c.contains(&addr)));
    }

    #[test]
    fn filter_values_are_escaped_so_a_dn_with_parens_still_matches() {
        assert_eq!(
            escape_filter_value("cn=Foo (Bar),ou=x"),
            "cn=Foo \\28Bar\\29,ou=x"
        );
        assert_eq!(escape_filter_value("a*b\\c"), "a\\2ab\\5cc");
    }

    #[test]
    fn a_bad_ldap_ca_paste_is_a_clear_error_not_a_panic() {
        let e = ldap_tls_config("not a certificate")
            .unwrap_err()
            .to_string();
        assert!(e.contains("not valid PEM"), "{e}");
    }

    #[tokio::test]
    async fn a_closed_port_is_reported_with_the_upstream_error() {
        // Take a port, then drop the listener so nothing is accepting on it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let r = probe(&AuthProviderSpec {
            provider: AuthProvider::Ldap(LdapConfig {
                hosts: vec![addr.clone()],
                ..Default::default()
            }),
            role_mappings: vec![],
        })
        .await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains(&addr));
    }

    #[test]
    fn a_bad_ca_paste_is_a_clear_error_not_a_panic() {
        let e = http_client("not a certificate").unwrap_err().to_string();
        assert!(e.contains("not valid PEM"), "{e}");
    }
}
