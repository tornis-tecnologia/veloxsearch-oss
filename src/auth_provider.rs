// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Auth provider axis (ADR-045 rev. 2, issue #56) — **pure generators only**.
//!
//! A deployment optionally authenticates its OpenSearch / Dashboards users
//! against an external identity provider. This module turns a validated
//! `AuthProviderSpec` into the two artifacts the operator needs, and nothing
//! else — no cluster calls, no I/O. MR2 wires them onto the CR:
//!
//!   1. **Cluster side** → `spec.security.config.securityConfigSecret`: the full
//!      securityconfig file set (`config.yml` with the `authc`/`authz` domains,
//!      `roles_mapping.yml`, plus the four files the plugin still requires).
//!   2. **Dashboards side** → `spec.dashboards.additionalConfig`: the
//!      `opensearch_dashboards.yml` keys that switch the login page to the
//!      IdP's redirect flow. Empty for every kind that rides basic auth.
//!
//! Two invariants from the ADR are enforced here, not by the caller, because
//! getting them wrong bricks a cluster:
//!
//!   - **Break-glass.** `basic_internal_auth_domain` is emitted for *every*
//!     kind, the `adminCredentialsSecret` admin keeps `all_access`, and the
//!     Dashboards block keeps `basicauth` in `auth.type`. There is no way to
//!     ask this module for a config whose only login path is the IdP.
//!   - **Completeness.** Supplying `securityConfigSecret` REPLACES the
//!     operator's default securityconfig, so `security_config_files` always
//!     returns the whole set — including `internal_users.yml` carrying the
//!     admin *and* the `kibanaserver` account, without which Dashboards cannot
//!     reach the cluster at all.
//!
//! The generators are pure so every kind's rendered YAML is asserted in unit
//! tests without a live cluster (same pattern as `retention_policy()` /
//! `dashboards_ingress_manifest()`).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Placeholder the API echoes back in place of a stored credential, and which
/// the client may send back unchanged to mean "keep what is saved". Credentials
/// are write-only: they are never returned in clear text (mirrors the one-shot
/// TLS PEM handling in the access settings).
pub const SECRET_KEPT: &str = "__velox_secret_kept__";

/// securityconfig `config_version` the OpenSearch security plugin expects.
const CONFIG_VERSION: u8 = 2;

/// Roles referenced by the group→role mapping UI. `all_access`, `kibana_user`
/// and `readall` are *static* roles shipped inside the security plugin, so an
/// empty `roles.yml` still resolves them — that is why we can hand the operator
/// a securityconfig with no custom roles and not lose the admin path.
pub const BUILTIN_ROLES: &[&str] = &["all_access", "kibana_user", "readall"];

// ---------------------------------------------------------------------------
// Spec
// ---------------------------------------------------------------------------

/// One deployment's auth provider. Serializes flat with a `kind` discriminator:
/// `{"kind":"oidc","connect_url":"…","role_mappings":[…]}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthProvider {
    /// Operator default: internal users / basic auth. No securityconfig of ours.
    #[default]
    Internal,
    Ldap(LdapConfig),
    Oidc(OidcConfig),
    Saml(SamlConfig),
    Jwt(JwtConfig),
    Proxy(ProxyConfig),
}

/// The provider plus the mapping table that is common to every external kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuthProviderSpec {
    #[serde(flatten)]
    pub provider: AuthProvider,
    /// Directory group / IdP claim value → OpenSearch role. Order is preserved
    /// as the user typed it; duplicates on the same backend role are merged.
    #[serde(default)]
    pub role_mappings: Vec<RoleMapping>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleMapping {
    /// LDAP group DN/name, or the value found in the IdP's `roles_key` claim.
    pub backend_role: String,
    /// OpenSearch role it grants (see `BUILTIN_ROLES`, or a custom one).
    pub role: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LdapConfig {
    /// `host:port` entries, e.g. `dc1.corp.local:636`.
    pub hosts: Vec<String>,
    pub bind_dn: String,
    pub bind_password: String,
    pub userbase: String,
    /// Must contain the `{0}` username placeholder — `(sAMAccountName={0})` for
    /// Active Directory, `(uid={0})` for OpenLDAP.
    pub usersearch: String,
    pub username_attribute: String,
    pub rolebase: String,
    pub rolesearch: String,
    /// Attribute on the *user* entry listing its groups (AD: `memberOf`).
    pub userrolename: String,
    /// Attribute on the *role* entry holding its name (usually `cn`).
    pub rolename: String,
    pub resolve_nested_roles: bool,
    pub enable_ssl: bool,
    pub enable_start_tls: bool,
    /// PEM of the CA that signed the directory's certificate (private CAs).
    pub pemtrustedcas_content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OidcConfig {
    /// The `.well-known/openid-configuration` discovery document.
    pub connect_url: String,
    pub client_id: String,
    pub client_secret: String,
    /// Claim carrying the username (default `preferred_username`).
    pub subject_key: String,
    /// Claim carrying the group list (default `roles`).
    pub roles_key: String,
    /// Extra scopes appended to `openid profile email`.
    pub scope: String,
    pub logout_url: String,
    pub pemtrustedcas_content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SamlConfig {
    pub metadata_url: String,
    pub idp_entity_id: String,
    pub sp_entity_id: String,
    pub roles_key: String,
    pub subject_key: String,
    /// Shared HMAC secret between the security plugin and Dashboards.
    pub exchange_key: String,
    /// PEM of the CA that signed the IdP's certificate (private CAs).
    ///
    /// SAML needs this for exactly the reason OIDC does — the node fetches
    /// `metadata_url` over https — and it was missing here while `OidcConfig`
    /// had it, which made every SAML IdP behind a corporate CA (the normal
    /// ADFS deployment) impossible to configure: the pre-save probe could not
    /// reach the metadata either (MR4, #56).
    pub pemtrustedcas_content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JwtConfig {
    pub signing_key: String,
    pub jwt_header: String,
    pub jwt_url_parameter: String,
    pub subject_key: String,
    pub roles_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub user_header: String,
    pub roles_header: String,
    pub roles_separator: String,
    /// Regex of proxies allowed to set the headers above (`xff.internalProxies`).
    pub internal_proxies: String,
}

/// Accounts that must survive our securityconfig replacing the operator's.
/// `admin_*` comes from the deployment's `adminCredentialsSecret`;
/// `dashboards_*` from its `opensearchCredentialsSecret`.
#[derive(Debug, Clone, Copy)]
pub struct Accounts<'a> {
    pub admin_user: &'a str,
    pub admin_hash: &'a str,
    pub dashboards_user: &'a str,
    pub dashboards_hash: &'a str,
}

impl AuthProvider {
    /// Stable wire/label value (`spec.kind`, CR label, i18n key).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Ldap(_) => "ldap",
            Self::Oidc(_) => "oidc",
            Self::Saml(_) => "saml",
            Self::Jwt(_) => "jwt",
            Self::Proxy(_) => "proxy",
        }
    }

    /// Browser redirect flows: they own the Dashboards login page and therefore
    /// need a stable public HTTPS origin (ADR-045 invariant 3).
    pub fn needs_public_origin(&self) -> bool {
        matches!(self, Self::Oidc(_) | Self::Saml(_))
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Reject a spec that cannot work BEFORE anything is written — same day-2 guard
/// discipline as the sizing checks: never leave a half-applied auth change.
///
/// `access_mode` is the ADR-027 dashboard access mode (`ingress` /
/// `portforward`); `public_url` is the deployment's public origin when one
/// exists (see [`public_url`]).
pub fn validate(
    spec: &AuthProviderSpec,
    access_mode: &str,
    public_url: Option<&str>,
) -> Result<()> {
    for m in &spec.role_mappings {
        if m.backend_role.trim().is_empty() {
            bail!("role mapping with an empty directory group / claim value");
        }
        if m.role.trim().is_empty() {
            bail!(
                "role mapping for '{}' has no OpenSearch role",
                m.backend_role.trim()
            );
        }
    }

    if spec.provider.needs_public_origin() {
        if access_mode != "ingress" {
            bail!(
                "{} sign-in redirects the browser back to the deployment, which needs a public address — switch dashboard access to 'ingress' first",
                spec.provider.kind()
            );
        }
        match public_url {
            Some(u) if u.starts_with("https://") => {}
            _ => bail!(
                "{} sign-in requires the deployment to be reachable over HTTPS — set a base domain with TLS in the access settings",
                spec.provider.kind()
            ),
        }
    }

    // An unresolved sentinel means `merge_secrets` had nothing to fold it onto
    // (the kind changed, or there was no stored spec). Writing it through would
    // silently make the literal placeholder the credential.
    if let Some(field) = unresolved_secret(spec) {
        bail!("{field} must be re-entered after changing the authentication method");
    }

    match &spec.provider {
        AuthProvider::Internal => Ok(()),
        AuthProvider::Ldap(c) => validate_ldap(c),
        AuthProvider::Oidc(c) => validate_oidc(c),
        AuthProvider::Saml(c) => validate_saml(c),
        AuthProvider::Jwt(c) => validate_jwt(c),
        AuthProvider::Proxy(c) => validate_proxy(c),
    }
}

/// Name of the credential still holding [`SECRET_KEPT`], if any.
fn unresolved_secret(spec: &AuthProviderSpec) -> Option<&'static str> {
    let kept = |v: &str| v == SECRET_KEPT;
    match &spec.provider {
        AuthProvider::Ldap(c) if kept(&c.bind_password) => Some("bind password"),
        AuthProvider::Oidc(c) if kept(&c.client_secret) => Some("client secret"),
        AuthProvider::Saml(c) if kept(&c.exchange_key) => Some("exchange key"),
        AuthProvider::Jwt(c) if kept(&c.signing_key) => Some("signing key"),
        _ => None,
    }
}

fn validate_ldap(c: &LdapConfig) -> Result<()> {
    let hosts: Vec<&String> = c.hosts.iter().filter(|h| !h.trim().is_empty()).collect();
    if hosts.is_empty() {
        bail!("at least one directory server (host:port) is required");
    }
    for h in hosts {
        if !h.contains(':') {
            bail!("directory server '{h}' must include a port, e.g. '{h}:636'");
        }
    }
    if c.userbase.trim().is_empty() {
        bail!("user search base (userbase) is required");
    }
    if !c.usersearch.contains("{0}") {
        bail!("user search filter must contain the {{0}} username placeholder, e.g. (sAMAccountName={{0}})");
    }
    if c.bind_dn.trim().is_empty() != c.bind_password.is_empty() {
        bail!("bind DN and bind password must be provided together (leave both empty for an anonymous bind)");
    }
    if c.enable_ssl && c.enable_start_tls {
        bail!("choose either LDAPS (enable_ssl) or StartTLS, not both");
    }
    if !c.rolebase.trim().is_empty()
        && !c.rolesearch.contains("{0}")
        && !c.rolesearch.contains("{1}")
    {
        bail!("role search filter must reference {{0}} (user DN) or {{1}} (username), e.g. (member={{0}})");
    }
    Ok(())
}

fn validate_oidc(c: &OidcConfig) -> Result<()> {
    if !c.connect_url.starts_with("https://") {
        bail!("the OpenID discovery URL must be an https:// address");
    }
    if !c.connect_url.contains("/.well-known/") {
        bail!("expected the discovery document URL, e.g. https://idp/realms/velox/.well-known/openid-configuration");
    }
    if c.client_id.trim().is_empty() {
        bail!("client ID is required");
    }
    if c.client_secret.is_empty() {
        bail!("client secret is required");
    }
    Ok(())
}

fn validate_saml(c: &SamlConfig) -> Result<()> {
    if !c.metadata_url.starts_with("https://") && !c.metadata_url.starts_with("http://") {
        bail!("the identity provider metadata URL is required");
    }
    if c.idp_entity_id.trim().is_empty() {
        bail!("identity provider entity ID is required");
    }
    // The plugin signs the Dashboards session with this key; a short one is a
    // forgeable session, so refuse it here rather than warn in the docs.
    if c.exchange_key.chars().count() < 32 {
        bail!("exchange key must be at least 32 characters — it signs the Dashboards session");
    }
    Ok(())
}

fn validate_jwt(c: &JwtConfig) -> Result<()> {
    if c.signing_key.trim().is_empty() {
        bail!("a signing key (or JWKS URL) is required");
    }
    Ok(())
}

fn validate_proxy(c: &ProxyConfig) -> Result<()> {
    if c.user_header.trim().is_empty() {
        bail!("the header carrying the username is required");
    }
    if c.internal_proxies.trim().is_empty() {
        bail!("the trusted proxy pattern is required — without it any client can forge the username header");
    }
    Ok(())
}

/// The deployment's public origin, or `None` in port-forward mode. Mirrors the
/// host `dashboards_ingress_manifest()` builds.
pub fn public_url(name: &str, access_mode: &str, base_domain: &str) -> Option<String> {
    let d = base_domain.trim().trim_matches('.');
    if access_mode != "ingress" || d.is_empty() {
        return None;
    }
    Some(format!("https://{name}.{d}"))
}

// ---------------------------------------------------------------------------
// Write-only credential handling
// ---------------------------------------------------------------------------

/// Replace every stored credential with [`SECRET_KEPT`] so a `GET` never
/// discloses one. An unset credential stays empty, so the UI can tell
/// "defined" from "not defined".
pub fn redacted(spec: &AuthProviderSpec) -> AuthProviderSpec {
    let mut out = spec.clone();
    let keep = |v: &mut String| {
        if !v.is_empty() {
            *v = SECRET_KEPT.to_string();
        }
    };
    match &mut out.provider {
        AuthProvider::Ldap(c) => keep(&mut c.bind_password),
        AuthProvider::Oidc(c) => keep(&mut c.client_secret),
        AuthProvider::Saml(c) => keep(&mut c.exchange_key),
        AuthProvider::Jwt(c) => keep(&mut c.signing_key),
        AuthProvider::Internal | AuthProvider::Proxy(_) => {}
    }
    out
}

/// Fold an incoming spec onto the stored one: a credential the client sent back
/// as [`SECRET_KEPT`] means "unchanged", anything else replaces it. Kinds that
/// differ carry no secret across.
pub fn merge_secrets(incoming: &AuthProviderSpec, stored: &AuthProviderSpec) -> AuthProviderSpec {
    let mut out = incoming.clone();
    let restore = |new: &mut String, old: &str| {
        if new == SECRET_KEPT {
            *new = old.to_string();
        }
    };
    match (&mut out.provider, &stored.provider) {
        (AuthProvider::Ldap(n), AuthProvider::Ldap(o)) => {
            restore(&mut n.bind_password, &o.bind_password)
        }
        (AuthProvider::Oidc(n), AuthProvider::Oidc(o)) => {
            restore(&mut n.client_secret, &o.client_secret)
        }
        (AuthProvider::Saml(n), AuthProvider::Saml(o)) => {
            restore(&mut n.exchange_key, &o.exchange_key)
        }
        (AuthProvider::Jwt(n), AuthProvider::Jwt(o)) => restore(&mut n.signing_key, &o.signing_key),
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------
// Cluster-side artifact: the securityconfig file set
// ---------------------------------------------------------------------------

/// Every file the replaced securityconfig must carry. `None` for
/// `AuthProvider::Internal` — that kind means "leave the operator's own
/// securityconfig alone", which is also how a provider is removed.
pub fn security_config_files(
    spec: &AuthProviderSpec,
    accounts: &Accounts,
    public_url: Option<&str>,
) -> Result<Option<BTreeMap<String, String>>> {
    if matches!(spec.provider, AuthProvider::Internal) {
        return Ok(None);
    }
    let mut files = BTreeMap::new();
    files.insert("config.yml".into(), security_config_yaml(spec, public_url)?);
    files.insert(
        "roles_mapping.yml".into(),
        roles_mapping_yaml(spec, accounts)?,
    );
    files.insert("internal_users.yml".into(), internal_users_yaml(accounts)?);
    // No custom roles / action groups / tenants: the plugin's static ones cover
    // every role the mapping UI offers. The files must still exist, or
    // `securityadmin` leaves the previous (default) content in place for them.
    files.insert("roles.yml".into(), empty_config("roles")?);
    files.insert("action_groups.yml".into(), empty_config("actiongroups")?);
    files.insert("tenants.yml".into(), empty_config("tenants")?);
    Ok(Some(files))
}

/// The file set that takes a cluster BACK to internal-only authentication.
///
/// Removing a provider CANNOT be done by deleting our Secret and clearing the
/// CR field, which is what the ADR originally assumed ("removing it restores
/// the operator default securityconfig"). Verified against a live cluster in
/// MR4: the operator's update job only ever runs
///
///   securityadmin.sh -f .../internal_users.yml -t internalusers
///
/// so it rewrites the internal users and *nothing else*. `config.yml` keeps
/// whatever was last pushed — meaning a "removed" directory provider stays live
/// in the security index while the app reports `internal`, and the directory
/// can still authenticate into the cluster.
///
/// Once we have replaced a cluster's securityconfig we own it: reverting means
/// pushing an internal-only securityconfig, not dropping ours.
pub fn internal_only_security_config_files(
    accounts: &Accounts,
) -> Result<BTreeMap<String, String>> {
    let spec = AuthProviderSpec::default();
    let mut files = BTreeMap::new();
    files.insert("config.yml".into(), security_config_yaml(&spec, None)?);
    files.insert(
        "roles_mapping.yml".into(),
        roles_mapping_yaml(&spec, accounts)?,
    );
    files.insert("internal_users.yml".into(), internal_users_yaml(accounts)?);
    files.insert("roles.yml".into(), empty_config("roles")?);
    files.insert("action_groups.yml".into(), empty_config("actiongroups")?);
    files.insert("tenants.yml".into(), empty_config("tenants")?);
    Ok(files)
}

/// `config.yml` — the authc/authz domains. Pure.
pub fn security_config_yaml(spec: &AuthProviderSpec, public_url: Option<&str>) -> Result<String> {
    let mut authc = serde_json::Map::new();
    // INVARIANT (break-glass): the internal domain is emitted for every kind,
    // first in the chain. `challenge: false` hands the 401 challenge to the
    // external domain (or to Dashboards' redirect) while still letting the
    // local admin authenticate with basic credentials — including when the IdP
    // is unreachable.
    authc.insert(
        "basic_internal_auth_domain".into(),
        serde_json::json!({
            "description": "Local accounts — always available (break-glass)",
            "http_enabled": true,
            "transport_enabled": true,
            "order": 0,
            "http_authenticator": { "type": "basic", "challenge": false },
            "authentication_backend": { "type": "intern" }
        }),
    );

    let mut authz = serde_json::Map::new();
    let mut http = serde_json::json!({ "anonymous_auth_enabled": false });

    match &spec.provider {
        AuthProvider::Internal => {
            // The internal domain must issue the challenge when it is alone.
            authc["basic_internal_auth_domain"]["http_authenticator"]["challenge"] =
                serde_json::Value::Bool(true);
        }
        AuthProvider::Ldap(c) => {
            authc.insert(
                "ldap_auth_domain".into(),
                serde_json::json!({
                    "description": "Directory accounts (LDAP / Active Directory)",
                    "http_enabled": true,
                    "transport_enabled": true,
                    "order": 1,
                    "http_authenticator": { "type": "basic", "challenge": true },
                    "authentication_backend": { "type": "ldap", "config": ldap_backend(c, false) }
                }),
            );
            if !c.rolebase.trim().is_empty() {
                authz.insert(
                    "roles_from_ldap".into(),
                    serde_json::json!({
                        "description": "Directory groups as backend roles",
                        "http_enabled": true,
                        "transport_enabled": true,
                        "authorization_backend": { "type": "ldap", "config": ldap_backend(c, true) }
                    }),
                );
            }
        }
        AuthProvider::Oidc(c) => {
            let mut cfg = serde_json::json!({
                "openid_connect_url": c.connect_url,
                "subject_key": or_default(&c.subject_key, "preferred_username"),
                "roles_key": or_default(&c.roles_key, "roles"),
            });
            if !c.pemtrustedcas_content.trim().is_empty() {
                // `enable_ssl` is NOT optional decoration here. The security
                // plugin builds the IdP HTTP client through its generic
                // SettingsBasedSSLConfigurator, which only assembles a custom
                // trust store when this flag is on. Ship the PEM without it and
                // the setting is silently ignored: the node falls back to the
                // JVM's cacerts, fetching the JWKS fails with
                // `unable to find valid certification path to requested target`,
                // and every login is rejected with "Authentication finally
                // failed" — the config looking perfectly correct all the while
                // (found on a live cluster, MR4 #56).
                cfg["openid_connect_idp"] = serde_json::json!({
                    "enable_ssl": true,
                    "verify_hostnames": true,
                    "pemtrustedcas_content": c.pemtrustedcas_content,
                });
            }
            authc.insert(
                "openid_auth_domain".into(),
                serde_json::json!({
                    "description": "OpenID Connect single sign-on",
                    "http_enabled": true,
                    "transport_enabled": false,
                    "order": 1,
                    "http_authenticator": { "type": "openid", "challenge": false, "config": cfg },
                    "authentication_backend": { "type": "noop" }
                }),
            );
        }
        AuthProvider::Saml(c) => {
            // `kibana_url` is where the IdP posts the assertion back to; it is
            // the deployment's public origin, which `validate` already required.
            let kibana_url = public_url.unwrap_or_default();
            let mut idp = serde_json::json!({
                "metadata_url": c.metadata_url,
                "entity_id": c.idp_entity_id,
            });
            // Same trap as the OIDC branch: the plugin builds this client
            // through SettingsBasedSSLConfigurator, which ignores the PEM
            // unless `enable_ssl` is on. See the openid domain above.
            if !c.pemtrustedcas_content.trim().is_empty() {
                idp["enable_ssl"] = serde_json::Value::Bool(true);
                idp["verify_hostnames"] = serde_json::Value::Bool(true);
                idp["pemtrustedcas_content"] =
                    serde_json::Value::String(c.pemtrustedcas_content.clone());
            }
            let mut cfg = serde_json::json!({
                "idp": idp,
                "sp": { "entity_id": or_default(&c.sp_entity_id, "opensearch-dashboards") },
                "kibana_url": kibana_url,
                "roles_key": or_default(&c.roles_key, "roles"),
                "exchange_key": c.exchange_key,
            });
            if !c.subject_key.trim().is_empty() {
                cfg["subject_key"] = serde_json::Value::String(c.subject_key.clone());
            }
            authc.insert(
                "saml_auth_domain".into(),
                serde_json::json!({
                    "description": "SAML 2.0 single sign-on",
                    "http_enabled": true,
                    "transport_enabled": false,
                    "order": 1,
                    "http_authenticator": { "type": "saml", "challenge": true, "config": cfg },
                    "authentication_backend": { "type": "noop" }
                }),
            );
        }
        AuthProvider::Jwt(c) => {
            let mut cfg = serde_json::json!({
                "signing_key": c.signing_key,
                "subject_key": or_default(&c.subject_key, "sub"),
                "roles_key": or_default(&c.roles_key, "roles"),
            });
            if !c.jwt_header.trim().is_empty() {
                cfg["jwt_header"] = serde_json::Value::String(c.jwt_header.clone());
            }
            if !c.jwt_url_parameter.trim().is_empty() {
                cfg["jwt_url_parameter"] = serde_json::Value::String(c.jwt_url_parameter.clone());
            }
            authc.insert(
                "jwt_auth_domain".into(),
                serde_json::json!({
                    "description": "Bearer tokens (machine-to-machine)",
                    "http_enabled": true,
                    "transport_enabled": false,
                    "order": 1,
                    "http_authenticator": { "type": "jwt", "challenge": false, "config": cfg },
                    "authentication_backend": { "type": "noop" }
                }),
            );
        }
        AuthProvider::Proxy(c) => {
            // Header auth is only safe behind a trusted proxy — `validate`
            // requires the pattern, and it is wired into `xff` here so the
            // plugin drops forged headers from anywhere else.
            http["xff"] = serde_json::json!({
                "enabled": true,
                "internalProxies": c.internal_proxies,
            });
            authc.insert(
                "proxy_auth_domain".into(),
                serde_json::json!({
                    "description": "Identity asserted by a trusted reverse proxy",
                    "http_enabled": true,
                    "transport_enabled": false,
                    "order": 1,
                    "http_authenticator": {
                        "type": "proxy",
                        "challenge": false,
                        "config": {
                            "user_header": c.user_header,
                            "roles_header": or_default(&c.roles_header, "x-proxy-roles"),
                            "roles_separator": or_default(&c.roles_separator, ","),
                        }
                    },
                    "authentication_backend": { "type": "noop" }
                }),
            );
        }
    }

    let doc = serde_json::json!({
        "_meta": { "type": "config", "config_version": CONFIG_VERSION },
        "config": { "dynamic": {
            "http": http,
            "authc": serde_json::Value::Object(authc),
            "authz": serde_json::Value::Object(authz),
        }}
    });
    to_yaml(&doc)
}

/// The LDAP connection block, shared by the authc backend and the authz
/// (role lookup) backend — `for_authz` adds the group-resolution keys.
fn ldap_backend(c: &LdapConfig, for_authz: bool) -> serde_json::Value {
    let hosts: Vec<String> = c
        .hosts
        .iter()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect();
    let mut cfg = serde_json::json!({
        "hosts": hosts,
        "userbase": c.userbase.trim(),
        "usersearch": c.usersearch.trim(),
        "enable_ssl": c.enable_ssl,
        "enable_start_tls": c.enable_start_tls,
        "verify_hostnames": true,
    });
    if !c.bind_dn.trim().is_empty() {
        cfg["bind_dn"] = serde_json::Value::String(c.bind_dn.trim().to_string());
        cfg["password"] = serde_json::Value::String(c.bind_password.clone());
    }
    if !c.username_attribute.trim().is_empty() {
        cfg["username_attribute"] =
            serde_json::Value::String(c.username_attribute.trim().to_string());
    }
    if !c.pemtrustedcas_content.trim().is_empty() {
        cfg["pemtrustedcas_content"] = serde_json::Value::String(c.pemtrustedcas_content.clone());
    }
    if for_authz {
        cfg["rolebase"] = serde_json::Value::String(c.rolebase.trim().to_string());
        cfg["rolesearch"] = serde_json::Value::String(c.rolesearch.trim().to_string());
        cfg["rolename"] = serde_json::Value::String(or_default(&c.rolename, "cn"));
        cfg["resolve_nested_roles"] = serde_json::Value::Bool(c.resolve_nested_roles);
        if c.userrolename.trim().is_empty() {
            // Explicit "no attribute on the user entry" — otherwise the plugin
            // falls back to `memberOf` and silently double-counts groups.
            cfg["userroleattribute"] = serde_json::Value::Null;
        } else {
            cfg["userrolename"] = serde_json::Value::String(c.userrolename.trim().to_string());
        }
    }
    cfg
}

/// `roles_mapping.yml` — backend role (directory group / claim value) → role.
pub fn roles_mapping_yaml(spec: &AuthProviderSpec, a: &Accounts) -> Result<String> {
    // INVARIANT (ADR-045 #1, the authorization half). Our securityconfig
    // REPLACES the operator's, and `roles_mapping.yml` is what turns an account
    // into privileges. Emitting only the customer's group rows authenticates
    // the admin and then grants it nothing: `cluster:monitor/main` is denied,
    // the readiness probe fails, and the rolling restart never finishes — the
    // cluster hangs with `RolesChecked []` in its log. Likewise the Dashboards
    // service account without `kibana_server` cannot query the cluster at all.
    // Both mappings are therefore emitted for EVERY kind, before the customer's
    // rows, and are asserted in unit tests.
    let mut by_role: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut users_by_role: BTreeMap<String, Vec<String>> = BTreeMap::new();
    by_role.insert("all_access".into(), vec!["admin".into()]);
    users_by_role.insert("all_access".into(), vec![a.admin_user.trim().to_string()]);
    users_by_role.insert(
        "kibana_server".into(),
        vec![a.dashboards_user.trim().to_string()],
    );

    for m in &spec.role_mappings {
        let role = m.role.trim();
        let backend = m.backend_role.trim();
        if role.is_empty() || backend.is_empty() {
            continue;
        }
        let e = by_role.entry(role.to_string()).or_default();
        if !e.iter().any(|b| b == backend) {
            e.push(backend.to_string());
        }
    }

    let mut doc = serde_json::Map::new();
    doc.insert(
        "_meta".into(),
        serde_json::json!({ "type": "rolesmapping", "config_version": CONFIG_VERSION }),
    );
    for role in by_role
        .keys()
        .chain(users_by_role.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
    {
        let mut entry = serde_json::Map::new();
        entry.insert("reserved".into(), serde_json::Value::Bool(false));
        if let Some(b) = by_role.get(&role) {
            entry.insert("backend_roles".into(), serde_json::json!(b));
        }
        if let Some(u) = users_by_role.get(&role) {
            let users: Vec<&String> = u.iter().filter(|s| !s.is_empty()).collect();
            if !users.is_empty() {
                entry.insert("users".into(), serde_json::json!(users));
            }
        }
        doc.insert(role, serde_json::Value::Object(entry));
    }
    to_yaml(&serde_json::Value::Object(doc))
}

/// `internal_users.yml` — INVARIANT: the local admin and the Dashboards service
/// account survive the replacement. Without the latter, Dashboards cannot query
/// the cluster and every login fails regardless of the IdP.
fn internal_users_yaml(a: &Accounts) -> Result<String> {
    if a.admin_user.trim().is_empty() || a.admin_hash.trim().is_empty() {
        bail!("refusing to generate a securityconfig without the local admin account");
    }
    if a.dashboards_user.trim().is_empty() || a.dashboards_hash.trim().is_empty() {
        bail!("refusing to generate a securityconfig without the Dashboards service account");
    }
    let mut doc = serde_json::Map::new();
    doc.insert(
        "_meta".into(),
        serde_json::json!({ "type": "internalusers", "config_version": CONFIG_VERSION }),
    );
    doc.insert(
        a.admin_user.trim().into(),
        serde_json::json!({
            "hash": a.admin_hash,
            "reserved": true,
            "backend_roles": ["admin"],
            "description": "VeloxSearch break-glass administrator"
        }),
    );
    doc.insert(
        a.dashboards_user.trim().into(),
        serde_json::json!({
            "hash": a.dashboards_hash,
            "reserved": true,
            "description": "OpenSearch Dashboards service account"
        }),
    );
    to_yaml(&serde_json::Value::Object(doc))
}

fn empty_config(kind: &str) -> Result<String> {
    to_yaml(&serde_json::json!({
        "_meta": { "type": kind, "config_version": CONFIG_VERSION }
    }))
}

// ---------------------------------------------------------------------------
// Dashboards-side artifact
// ---------------------------------------------------------------------------

/// Environment variable the Dashboards pod resolves the OIDC client secret
/// from. `opensearch_dashboards.yml` carries `${VELOX_OIDC_CLIENT_SECRET}`;
/// the value is mounted from a Secret through `spec.dashboards.env`, so the
/// credential never enters the operator's ConfigMap.
pub const OIDC_SECRET_ENV: &str = "VELOX_OIDC_CLIENT_SECRET";

/// Dashboards paths the IdP POSTs a SAML assertion (or a logout) to, which must
/// therefore be exempt from XSRF checking.
///
/// Both families on purpose. `_opendistro` is the one the flow actually uses —
/// the security plugin advertises the SP's AssertionConsumerService under it —
/// and `_plugins` is the modern alias Dashboards also registers. Listing only
/// the latter is a login that dies at the last hop (MR4, #56).
pub const SAML_XSRF_PATHS: [&str; 6] = [
    "/_opendistro/_security/saml/acs",
    "/_opendistro/_security/saml/acs/idpinitiated",
    "/_opendistro/_security/saml/logout",
    "/_plugins/_security/saml/acs",
    "/_plugins/_security/saml/acs/idpinitiated",
    "/_plugins/_security/saml/logout",
];

/// Key under which the IdP's CA PEM is stored in the deployment's auth Secret,
/// and the file name it is mounted as inside the Dashboards container.
pub const IDP_CA_KEY: &str = "idp-ca.pem";
/// Directory the auth Secret is mounted at in the Dashboards pod.
pub const IDP_CA_DIR: &str = "/usr/share/opensearch-dashboards/config/velox-auth";

/// Absolute path of the mounted CA, for `opensearch_security.openid.root_ca`.
pub fn idp_ca_path() -> String {
    format!("{IDP_CA_DIR}/{IDP_CA_KEY}")
}

/// The IdP CA PEM a deployment needs mounted into its **Dashboards** pod, if any.
///
/// The securityconfig's `pemtrustedcas_content` covers the OpenSearch nodes,
/// which validate the JWT against the IdP's JWKS. It does NOT cover Dashboards:
/// the security-dashboards plugin is a Node process that fetches the discovery
/// document itself at startup, over its own TLS stack, with only the system
/// roots. Behind a private CA it dies before serving:
///
///   UNABLE_TO_VERIFY_LEAF_SIGNATURE  →  GET …/.well-known/openid-configuration
///   FATAL Error: Failed when trying to obtain the endpoints from your IdP
///
/// So the same PEM has to reach the Dashboards container as a *file* and be
/// named in `opensearch_security.openid.root_ca` (found on a live cluster,
/// MR4 #56 — the ADR's design covered only the cluster side).
pub fn dashboards_ca_pem(spec: &AuthProviderSpec) -> Option<String> {
    match &spec.provider {
        AuthProvider::Oidc(c) if !c.pemtrustedcas_content.trim().is_empty() => {
            Some(c.pemtrustedcas_content.clone())
        }
        _ => None,
    }
}

/// Secret-sourced environment for the Dashboards pod: env var name → value.
/// Pairs with the `${…}` references left in [`dashboards_additional_config`].
pub fn dashboards_env(spec: &AuthProviderSpec) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let AuthProvider::Oidc(c) = &spec.provider {
        m.insert(OIDC_SECRET_ENV.to_string(), c.client_secret.clone());
    }
    m
}

/// `spec.dashboards.additionalConfig` — the `opensearch_dashboards.yml` keys
/// that switch the login page to the IdP's redirect flow. Empty for the kinds
/// that ride the basic-auth challenge (internal, ldap) or that never involve a
/// browser (jwt, proxy).
///
/// The operator renders this map into a **ConfigMap**, so no credential may
/// appear in it (ADR-045 spike question #1). The OIDC client secret is emitted
/// as a `${…}` reference to [`OIDC_SECRET_ENV`], whose value the caller mounts
/// from a Secret via `spec.dashboards.env` — see [`dashboards_env`].
pub fn dashboards_additional_config(
    spec: &AuthProviderSpec,
    public_url: Option<&str>,
) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    match &spec.provider {
        AuthProvider::Oidc(c) => {
            // INVARIANT (break-glass): `basicauth` stays in the list, so the
            // local admin can always sign in from the same page.
            m.insert(
                "opensearch_security.auth.type".into(),
                "[\"basicauth\",\"openid\"]".into(),
            );
            m.insert(
                "opensearch_security.auth.multiple_auth_enabled".into(),
                "true".into(),
            );
            m.insert(
                "opensearch_security.openid.connect_url".into(),
                c.connect_url.clone(),
            );
            m.insert(
                "opensearch_security.openid.client_id".into(),
                c.client_id.clone(),
            );
            // NEVER the literal secret: this map becomes a ConfigMap. The
            // value is resolved at container start from the env var below.
            m.insert(
                "opensearch_security.openid.client_secret".into(),
                format!("${{{OIDC_SECRET_ENV}}}"),
            );
            if let Some(u) = public_url {
                // `opensearch_security.openid.base_redirect_url` is the key the
                // security plugin actually redirects on. Do NOT also emit
                // `server.publicBaseUrl`: that is a Kibana 7.11+ setting, and
                // OpenSearch Dashboards forked at 7.10 — its legacy config
                // schema rejects unknown `server.*` keys outright, so the
                // container dies at boot with
                // `child "server" fails because ["publicBaseUrl" is not allowed]`
                // (found on a live cluster, MR4 #56).
                m.insert(
                    "opensearch_security.openid.base_redirect_url".into(),
                    u.to_string(),
                );
            }
            // A private CA has to reach Dashboards too, as a file — see
            // `dashboards_ca_pem`. Without this the plugin cannot even fetch
            // the discovery document and the container never starts.
            if !c.pemtrustedcas_content.trim().is_empty() {
                m.insert("opensearch_security.openid.root_ca".into(), idp_ca_path());
            }
            if !c.scope.trim().is_empty() {
                m.insert(
                    "opensearch_security.openid.scope".into(),
                    c.scope.trim().to_string(),
                );
            }
            if !c.logout_url.trim().is_empty() {
                m.insert(
                    "opensearch_security.openid.logout_url".into(),
                    c.logout_url.trim().to_string(),
                );
            }
            m.insert("opensearch_security.cookie.secure".into(), "true".into());
        }
        AuthProvider::Saml(_) => {
            m.insert(
                "opensearch_security.auth.type".into(),
                "[\"basicauth\",\"saml\"]".into(),
            );
            m.insert(
                "opensearch_security.auth.multiple_auth_enabled".into(),
                "true".into(),
            );
            // The IdP POSTs the assertion to these paths; without the allowlist
            // Dashboards rejects them as XSRF and login dead-ends.
            //
            // BOTH path families, and the `_opendistro` one is the load-bearing
            // half (MR4, #56): the security plugin still advertises the SP's
            // AssertionConsumerService at `<kibana_url>/_opendistro/_security/
            // saml/acs`, so that is where the IdP actually posts. A browser POST
            // carries no `osd-xsrf` header — that is the entire reason this
            // allowlist exists — so listing only `_plugins` left Dashboards
            // answering the assertion with
            // `400 Request must contain the osd-xsrf header`, *after* the user
            // had already authenticated at the IdP. `_plugins` is kept because
            // Dashboards registers both and a later plugin release may switch.
            m.insert(
                "server.xsrf.allowlist".into(),
                serde_json::to_string(&SAML_XSRF_PATHS).unwrap_or_default(),
            );
            // The session lifetime is the IdP's to decide, not ours to extend.
            m.insert(
                "opensearch_security.session.keepalive".into(),
                "false".into(),
            );
            m.insert("opensearch_security.cookie.secure".into(), "true".into());
            // No `server.publicBaseUrl` here either — see the OIDC branch. The
            // SP side of SAML takes its URL from the provider's `kibana_url`,
            // which goes into the securityconfig, not into this block.
        }
        AuthProvider::Internal
        | AuthProvider::Ldap(_)
        | AuthProvider::Jwt(_)
        | AuthProvider::Proxy(_) => {}
    }
    m
}

// ---------------------------------------------------------------------------

fn or_default(v: &str, fallback: &str) -> String {
    let t = v.trim();
    if t.is_empty() {
        fallback.to_string()
    } else {
        t.to_string()
    }
}

fn to_yaml(v: &serde_json::Value) -> Result<String> {
    Ok(serde_yaml::to_string(v)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accounts() -> Accounts<'static> {
        Accounts {
            admin_user: "admin",
            admin_hash: "$2y$12$adminhash",
            dashboards_user: "kibanaserver",
            dashboards_hash: "$2y$12$osdhash",
        }
    }

    fn ldap() -> AuthProviderSpec {
        AuthProviderSpec {
            provider: AuthProvider::Ldap(LdapConfig {
                hosts: vec!["dc1.corp.local:636".into()],
                bind_dn: "CN=svc,DC=corp,DC=local".into(),
                bind_password: "s3cret".into(),
                userbase: "OU=Users,DC=corp,DC=local".into(),
                usersearch: "(sAMAccountName={0})".into(),
                rolebase: "OU=Groups,DC=corp,DC=local".into(),
                rolesearch: "(member={0})".into(),
                rolename: "cn".into(),
                resolve_nested_roles: true,
                enable_ssl: true,
                ..Default::default()
            }),
            role_mappings: vec![RoleMapping {
                backend_role: "CN=os-admins,OU=Groups,DC=corp,DC=local".into(),
                role: "all_access".into(),
            }],
        }
    }

    fn oidc() -> AuthProviderSpec {
        AuthProviderSpec {
            provider: AuthProvider::Oidc(OidcConfig {
                connect_url: "https://kc.corp/realms/velox/.well-known/openid-configuration".into(),
                client_id: "veloxsearch".into(),
                client_secret: "topsecret".into(),
                ..Default::default()
            }),
            role_mappings: vec![RoleMapping {
                backend_role: "os-admins".into(),
                role: "all_access".into(),
            }],
        }
    }

    fn saml() -> AuthProviderSpec {
        AuthProviderSpec {
            provider: AuthProvider::Saml(SamlConfig {
                metadata_url: "https://kc.corp/realms/velox/protocol/saml/descriptor".into(),
                idp_entity_id: "https://kc.corp/realms/velox".into(),
                exchange_key: "0123456789abcdef0123456789abcdef".into(),
                ..Default::default()
            }),
            role_mappings: vec![],
        }
    }

    fn yaml_of(spec: &AuthProviderSpec, url: Option<&str>) -> serde_json::Value {
        let s = security_config_yaml(spec, url).expect("render");
        serde_yaml::from_str(&s).expect("valid yaml")
    }

    // --- break-glass invariant (the one that bricks clusters) ---------------

    #[test]
    fn every_kind_keeps_the_internal_auth_domain() {
        let url = Some("https://prod.example.com");
        for spec in [
            AuthProviderSpec::default(),
            ldap(),
            oidc(),
            saml(),
            AuthProviderSpec {
                provider: AuthProvider::Jwt(JwtConfig {
                    signing_key: "k".into(),
                    ..Default::default()
                }),
                role_mappings: vec![],
            },
            AuthProviderSpec {
                provider: AuthProvider::Proxy(ProxyConfig {
                    user_header: "x-proxy-user".into(),
                    internal_proxies: "10\\.10\\..*".into(),
                    ..Default::default()
                }),
                role_mappings: vec![],
            },
        ] {
            let d = yaml_of(&spec, url);
            let basic = &d["config"]["dynamic"]["authc"]["basic_internal_auth_domain"];
            assert_eq!(
                basic["authentication_backend"]["type"],
                "intern",
                "{} lost the local login path",
                spec.provider.kind()
            );
            assert_eq!(basic["http_enabled"], true);
            assert_eq!(basic["order"], 0, "local domain must be tried first");
        }
    }

    #[test]
    fn internal_domain_challenges_only_when_it_is_alone() {
        let alone = yaml_of(&AuthProviderSpec::default(), None);
        assert_eq!(
            alone["config"]["dynamic"]["authc"]["basic_internal_auth_domain"]["http_authenticator"]
                ["challenge"],
            true
        );
        let with_ldap = yaml_of(&ldap(), None);
        assert_eq!(
            with_ldap["config"]["dynamic"]["authc"]["basic_internal_auth_domain"]
                ["http_authenticator"]["challenge"],
            false,
            "the external domain owns the challenge"
        );
    }

    #[test]
    fn internal_users_carries_admin_and_dashboards_account() {
        let files = security_config_files(&ldap(), &accounts(), None)
            .unwrap()
            .unwrap();
        let u: serde_json::Value =
            serde_yaml::from_str(&files["internal_users.yml"]).expect("valid yaml");
        assert_eq!(u["admin"]["hash"], "$2y$12$adminhash");
        assert_eq!(u["admin"]["backend_roles"][0], "admin");
        assert_eq!(
            u["kibanaserver"]["hash"], "$2y$12$osdhash",
            "dropping kibanaserver breaks every Dashboards login"
        );
    }

    #[test]
    fn securityconfig_is_complete() {
        let files = security_config_files(&oidc(), &accounts(), Some("https://p.example.com"))
            .unwrap()
            .unwrap();
        for f in [
            "config.yml",
            "internal_users.yml",
            "roles.yml",
            "roles_mapping.yml",
            "action_groups.yml",
            "tenants.yml",
        ] {
            assert!(
                files.contains_key(f),
                "missing {f} — cluster would hang at 'Security not initialized'"
            );
        }
    }

    #[test]
    fn missing_admin_account_is_refused() {
        let a = Accounts {
            admin_user: "",
            ..accounts()
        };
        assert!(security_config_files(&ldap(), &a, None).is_err());
    }

    #[test]
    fn internal_kind_leaves_the_operator_default_alone() {
        let files = security_config_files(&AuthProviderSpec::default(), &accounts(), None).unwrap();
        assert!(
            files.is_none(),
            "removing a provider must not push a securityconfig"
        );
    }

    // --- LDAP ---------------------------------------------------------------

    #[test]
    fn ldap_renders_authc_and_authz_domains() {
        let d = yaml_of(&ldap(), None);
        let dom = &d["config"]["dynamic"]["authc"]["ldap_auth_domain"];
        assert_eq!(dom["http_authenticator"]["type"], "basic");
        assert_eq!(dom["http_authenticator"]["challenge"], true);
        assert_eq!(dom["authentication_backend"]["type"], "ldap");
        let cfg = &dom["authentication_backend"]["config"];
        assert_eq!(cfg["hosts"][0], "dc1.corp.local:636");
        assert_eq!(cfg["usersearch"], "(sAMAccountName={0})");
        assert_eq!(cfg["enable_ssl"], true);
        assert_eq!(cfg["password"], "s3cret");
        // authz side resolves the groups
        let az =
            &d["config"]["dynamic"]["authz"]["roles_from_ldap"]["authorization_backend"]["config"];
        assert_eq!(az["rolebase"], "OU=Groups,DC=corp,DC=local");
        assert_eq!(az["rolesearch"], "(member={0})");
        assert_eq!(az["resolve_nested_roles"], true);
        assert!(
            az.get("userrolename").is_none() && az["userroleattribute"].is_null(),
            "no user attribute configured ⇒ pin userroleattribute to null"
        );
    }

    #[test]
    fn ldap_without_rolebase_emits_no_authz() {
        let mut s = ldap();
        if let AuthProvider::Ldap(c) = &mut s.provider {
            c.rolebase = String::new();
        }
        let d = yaml_of(&s, None);
        assert!(d["config"]["dynamic"]["authz"]
            .as_object()
            .is_none_or(|m| m.is_empty()));
    }

    #[test]
    fn ldap_validation_catches_the_common_mistakes() {
        let base = |f: fn(&mut LdapConfig)| {
            let mut s = ldap();
            if let AuthProvider::Ldap(c) = &mut s.provider {
                f(c);
            }
            validate(&s, "portforward", None)
        };
        assert!(base(|_| {}).is_ok());
        assert!(base(|c| c.hosts.clear()).is_err(), "no server");
        assert!(
            base(|c| c.hosts = vec!["dc1.corp.local".into()]).is_err(),
            "no port"
        );
        assert!(
            base(|c| c.usersearch = "(sAMAccountName=user)".into()).is_err(),
            "filter without {{0}}"
        );
        assert!(
            base(|c| c.bind_password = String::new()).is_err(),
            "half a bind"
        );
        assert!(
            base(|c| c.enable_start_tls = true).is_err(),
            "LDAPS and StartTLS are mutually exclusive"
        );
        assert!(base(|c| c.userbase = String::new()).is_err());
    }

    #[test]
    fn ldap_needs_no_dashboards_config() {
        assert!(dashboards_additional_config(&ldap(), Some("https://p.example.com")).is_empty());
        assert!(dashboards_env(&ldap()).is_empty());
    }

    #[test]
    fn no_kind_puts_a_credential_in_the_dashboards_config() {
        // Everything in this map is rendered into a ConfigMap by the operator.
        let url = Some("https://prod.example.com");
        for (spec, secret) in [
            (ldap(), "s3cret"),
            (oidc(), "topsecret"),
            (saml(), "0123456789abcdef0123456789abcdef"),
        ] {
            let m = dashboards_additional_config(&spec, url);
            assert!(
                !m.values().any(|v| v.contains(secret)),
                "{} leaked a credential into the ConfigMap",
                spec.provider.kind()
            );
        }
    }

    // --- OIDC ---------------------------------------------------------------

    #[test]
    fn oidc_renders_both_sides() {
        let url = Some("https://prod.example.com");
        let d = yaml_of(&oidc(), url);
        let dom = &d["config"]["dynamic"]["authc"]["openid_auth_domain"];
        assert_eq!(dom["http_authenticator"]["type"], "openid");
        assert_eq!(
            dom["http_authenticator"]["config"]["openid_connect_url"],
            "https://kc.corp/realms/velox/.well-known/openid-configuration"
        );
        assert_eq!(
            dom["http_authenticator"]["config"]["subject_key"], "preferred_username",
            "default claim"
        );
        assert_eq!(dom["authentication_backend"]["type"], "noop");

        let m = dashboards_additional_config(&oidc(), url);
        assert_eq!(
            m["opensearch_security.auth.type"],
            "[\"basicauth\",\"openid\"]"
        );
        assert_eq!(m["opensearch_security.auth.multiple_auth_enabled"], "true");
        assert_eq!(m["opensearch_security.openid.client_id"], "veloxsearch");
        // The ConfigMap must carry a reference, never the credential itself.
        assert_eq!(
            m["opensearch_security.openid.client_secret"],
            "${VELOX_OIDC_CLIENT_SECRET}"
        );
        assert!(
            !m.values().any(|v| v.contains("topsecret")),
            "the client secret leaked into the ConfigMap-bound config"
        );
        assert!(
            !m.contains_key("server.publicBaseUrl"),
            "OpenSearch Dashboards forked Kibana at 7.10 and its legacy config \
             schema rejects unknown server.* keys — emitting publicBaseUrl kills \
             the container at boot"
        );
        assert_eq!(dashboards_env(&oidc())[OIDC_SECRET_ENV], "topsecret");
        assert_eq!(
            m["opensearch_security.openid.base_redirect_url"],
            "https://prod.example.com"
        );
        assert_eq!(m["opensearch_security.cookie.secure"], "true");
    }

    #[test]
    fn a_private_idp_ca_is_actually_switched_on() {
        // Regression (MR4, #56): `pemtrustedcas_content` alone is inert. The
        // security plugin only builds a custom trust store when `enable_ssl` is
        // set, so without it the node quietly used the JVM cacerts, the JWKS
        // fetch died with `unable to find valid certification path to requested
        // target`, and every OIDC login was refused by a config that read as
        // correct.
        let mut s = oidc();
        if let AuthProvider::Oidc(c) = &mut s.provider {
            c.pemtrustedcas_content = "-----BEGIN CERTIFICATE-----\nxx\n".into();
        }
        let idp = &yaml_of(&s, Some("https://p.example.com"))["config"]["dynamic"]["authc"]
            ["openid_auth_domain"]["http_authenticator"]["config"]["openid_connect_idp"];
        assert_eq!(
            idp["enable_ssl"], true,
            "a trusted CA without enable_ssl is silently ignored"
        );
        assert!(idp["pemtrustedcas_content"]
            .as_str()
            .unwrap()
            .contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn oidc_validation() {
        let bad = |f: fn(&mut OidcConfig)| {
            let mut s = oidc();
            if let AuthProvider::Oidc(c) = &mut s.provider {
                f(c);
            }
            validate(&s, "ingress", Some("https://p.example.com"))
        };
        assert!(bad(|_| {}).is_ok());
        assert!(
            bad(|c| c.connect_url = "http://kc/.well-known/x".into()).is_err(),
            "plain http"
        );
        assert!(
            bad(|c| c.connect_url = "https://kc.corp/realms/velox".into()).is_err(),
            "not the discovery doc"
        );
        assert!(bad(|c| c.client_secret = String::new()).is_err());
        assert!(bad(|c| c.client_id = String::new()).is_err());
    }

    // --- SAML ---------------------------------------------------------------

    #[test]
    fn saml_renders_both_sides() {
        let url = Some("https://prod.example.com");
        let d = yaml_of(&saml(), url);
        let cfg =
            &d["config"]["dynamic"]["authc"]["saml_auth_domain"]["http_authenticator"]["config"];
        assert_eq!(cfg["idp"]["entity_id"], "https://kc.corp/realms/velox");
        assert_eq!(cfg["sp"]["entity_id"], "opensearch-dashboards");
        assert_eq!(cfg["kibana_url"], "https://prod.example.com");
        assert_eq!(cfg["exchange_key"], "0123456789abcdef0123456789abcdef");

        let m = dashboards_additional_config(&saml(), url);
        assert!(m["server.xsrf.allowlist"].contains("/_plugins/_security/saml/acs"));
        assert!(m["server.xsrf.allowlist"].contains("/_plugins/_security/saml/logout"));
        assert_eq!(m["opensearch_security.session.keepalive"], "false");
        assert_eq!(
            m["opensearch_security.auth.type"],
            "[\"basicauth\",\"saml\"]"
        );
        assert!(
            !m.contains_key("server.publicBaseUrl"),
            "same Kibana-7.11-only key that kills the OIDC container at boot"
        );
    }

    #[test]
    fn saml_xsrf_allowlist_covers_the_legacy_acs_path() {
        // Regression (MR4, #56), found only on a live cluster: the allowlist
        // named the `_plugins` paths alone, but the security plugin advertises
        // the SP's AssertionConsumerService under `_opendistro`, so that is
        // where the IdP posts the assertion. The browser's POST carries no
        // `osd-xsrf` header, so Dashboards answered
        //   400 Request must contain the osd-xsrf header
        // *after* the user had authenticated at the IdP — a login that cannot
        // succeed, from a config whose YAML is perfectly well formed.
        let allow = &dashboards_additional_config(&saml(), Some("https://prod.example.com"))
            ["server.xsrf.allowlist"];
        for p in [
            "/_opendistro/_security/saml/acs",
            "/_opendistro/_security/saml/acs/idpinitiated",
            "/_opendistro/_security/saml/logout",
            "/_plugins/_security/saml/acs",
        ] {
            assert!(allow.contains(p), "{p} missing from {allow}");
        }
        // It has to stay parseable as the JSON array OSD expects, not a string
        // that merely looks like one.
        let parsed: Vec<String> = serde_json::from_str(allow).expect("not a JSON array");
        assert_eq!(parsed.len(), SAML_XSRF_PATHS.len());
    }

    #[test]
    fn saml_carries_a_private_idp_ca_the_same_way_oidc_does() {
        // Regression (MR4, #56): SamlConfig had no CA field at all while
        // OidcConfig did, though both fetch IdP metadata over https. Every SAML
        // IdP behind a corporate PKI — the normal ADFS deployment — was
        // therefore impossible: the node could not fetch `metadata_url`, and
        // the pre-save probe refused a configuration that would have worked.
        let mut s = saml();
        if let AuthProvider::Saml(c) = &mut s.provider {
            c.pemtrustedcas_content = "-----BEGIN CERTIFICATE-----\nxx\n".into();
        }
        let idp = &yaml_of(&s, Some("https://p.example.com"))["config"]["dynamic"]["authc"]
            ["saml_auth_domain"]["http_authenticator"]["config"]["idp"];
        assert_eq!(
            idp["enable_ssl"], true,
            "a trusted CA without enable_ssl is silently ignored"
        );
        assert!(idp["pemtrustedcas_content"]
            .as_str()
            .unwrap()
            .contains("BEGIN CERTIFICATE"));

        // No CA configured → the keys stay absent, so a public IdP keeps using
        // the default trust store.
        let plain = &yaml_of(&saml(), Some("https://p.example.com"))["config"]["dynamic"]["authc"]
            ["saml_auth_domain"]["http_authenticator"]["config"]["idp"];
        assert!(plain["enable_ssl"].is_null());
        assert!(plain["pemtrustedcas_content"].is_null());
    }

    #[test]
    fn saml_short_exchange_key_is_refused() {
        let mut s = saml();
        if let AuthProvider::Saml(c) = &mut s.provider {
            c.exchange_key = "short".into();
        }
        assert!(validate(&s, "ingress", Some("https://p.example.com")).is_err());
    }

    // --- origin gate --------------------------------------------------------

    #[test]
    fn redirect_kinds_require_an_https_origin() {
        for s in [oidc(), saml()] {
            assert!(
                validate(&s, "portforward", None).is_err(),
                "{} must be refused without a public address",
                s.provider.kind()
            );
            assert!(
                validate(&s, "ingress", Some("http://prod.example.com")).is_err(),
                "plain http is not an origin for a redirect flow"
            );
            assert!(validate(&s, "ingress", Some("https://prod.example.com")).is_ok());
        }
        // LDAP is unaffected — it never redirects the browser.
        assert!(validate(&ldap(), "portforward", None).is_ok());
    }

    #[test]
    fn public_url_follows_the_access_mode() {
        assert_eq!(
            public_url("prod", "ingress", "example.com").as_deref(),
            Some("https://prod.example.com")
        );
        assert_eq!(
            public_url("prod", "ingress", "  .example.com. ").as_deref(),
            Some("https://prod.example.com")
        );
        assert_eq!(public_url("prod", "portforward", "example.com"), None);
        assert_eq!(public_url("prod", "ingress", "   "), None);
    }

    // --- role mapping -------------------------------------------------------

    #[test]
    fn role_mapping_groups_backends_under_each_role() {
        let spec = AuthProviderSpec {
            provider: oidc().provider,
            role_mappings: vec![
                RoleMapping {
                    backend_role: "os-admins".into(),
                    role: "all_access".into(),
                },
                RoleMapping {
                    backend_role: " os-admins ".into(),
                    role: "all_access".into(),
                },
                RoleMapping {
                    backend_role: "devs".into(),
                    role: "kibana_user".into(),
                },
                RoleMapping {
                    backend_role: "ops".into(),
                    role: "kibana_user".into(),
                },
            ],
        };
        let d: serde_json::Value =
            serde_yaml::from_str(&roles_mapping_yaml(&spec, &accounts()).unwrap()).unwrap();
        assert_eq!(d["_meta"]["type"], "rolesmapping");
        let aa: Vec<&str> = d["all_access"]["backend_roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(aa.contains(&"os-admins"));
        assert_eq!(
            aa.iter().filter(|b| **b == "os-admins").count(),
            1,
            "duplicate rows are merged, not repeated"
        );
        let ku = d["kibana_user"]["backend_roles"].as_array().unwrap();
        assert_eq!(ku.len(), 2);
    }

    #[test]
    fn the_admin_and_the_dashboards_account_keep_their_roles_for_every_kind() {
        // Regression: the generated roles_mapping.yml once carried ONLY the
        // customer's rows. Applied, it authenticated the admin and granted it
        // nothing — `cluster:monitor/main` denied, readiness probe failing,
        // rolling restart stuck — and left Dashboards unable to query at all
        // (found on a live cluster, ADR-045 MR4).
        for spec in [ldap(), oidc(), saml()] {
            let d: serde_json::Value =
                serde_yaml::from_str(&roles_mapping_yaml(&spec, &accounts()).unwrap()).unwrap();
            let aa = &d["all_access"];
            assert!(
                aa["backend_roles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "admin"),
                "all_access lost the 'admin' backend role for {}",
                spec.provider.kind()
            );
            assert!(
                aa["users"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == accounts().admin_user),
                "all_access lost the local admin user for {}",
                spec.provider.kind()
            );
            assert!(
                d["kibana_server"]["users"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == accounts().dashboards_user),
                "kibana_server lost the Dashboards account for {} — Dashboards \
                 cannot reach the cluster without it",
                spec.provider.kind()
            );
        }
    }

    #[test]
    fn incomplete_role_mapping_rows_are_refused() {
        let mut s = oidc();
        s.role_mappings = vec![RoleMapping {
            backend_role: "devs".into(),
            role: "  ".into(),
        }];
        assert!(validate(&s, "ingress", Some("https://p.example.com")).is_err());
        s.role_mappings = vec![RoleMapping {
            backend_role: "".into(),
            role: "readall".into(),
        }];
        assert!(validate(&s, "ingress", Some("https://p.example.com")).is_err());
    }

    // --- write-only credentials --------------------------------------------

    #[test]
    fn credentials_are_never_echoed_back() {
        let r = redacted(&ldap());
        match &r.provider {
            AuthProvider::Ldap(c) => {
                assert_eq!(c.bind_password, SECRET_KEPT);
                assert_eq!(c.bind_dn, "CN=svc,DC=corp,DC=local", "non-secrets survive");
            }
            _ => panic!("kind changed"),
        }
        match &redacted(&oidc()).provider {
            AuthProvider::Oidc(c) => assert_eq!(c.client_secret, SECRET_KEPT),
            _ => panic!("kind changed"),
        }
    }

    #[test]
    fn unset_credentials_stay_visibly_empty() {
        let mut s = ldap();
        if let AuthProvider::Ldap(c) = &mut s.provider {
            c.bind_dn = String::new();
            c.bind_password = String::new();
        }
        match &redacted(&s).provider {
            AuthProvider::Ldap(c) => assert!(c.bind_password.is_empty()),
            _ => panic!("kind changed"),
        }
    }

    #[test]
    fn kept_sentinel_restores_the_stored_credential() {
        let stored = ldap();
        let incoming = redacted(&stored); // what the UI round-trips untouched
        let merged = merge_secrets(&incoming, &stored);
        match &merged.provider {
            AuthProvider::Ldap(c) => assert_eq!(c.bind_password, "s3cret"),
            _ => panic!("kind changed"),
        }
        assert_eq!(merged, stored, "an untouched round-trip must be a no-op");
    }

    #[test]
    fn a_new_credential_replaces_the_stored_one() {
        let stored = ldap();
        let mut incoming = redacted(&stored);
        if let AuthProvider::Ldap(c) = &mut incoming.provider {
            c.bind_password = "rotated".into();
        }
        match &merge_secrets(&incoming, &stored).provider {
            AuthProvider::Ldap(c) => assert_eq!(c.bind_password, "rotated"),
            _ => panic!("kind changed"),
        }
    }

    #[test]
    fn switching_kind_carries_no_credential_across() {
        let merged = merge_secrets(&redacted(&oidc()), &ldap());
        match &merged.provider {
            // The sentinel is left as-is rather than resolved from an unrelated
            // kind's credential…
            AuthProvider::Oidc(c) => assert_eq!(c.client_secret, SECRET_KEPT),
            _ => panic!("kind changed"),
        }
        // …and validation refuses it, so the placeholder can never be stored as
        // the credential itself.
        let err = validate(&merged, "ingress", Some("https://p.example.com"))
            .expect_err("unresolved sentinel must not pass validation");
        assert!(err.to_string().contains("client secret"), "{err}");
    }

    #[test]
    fn a_first_time_spec_cannot_smuggle_the_sentinel() {
        let mut s = ldap();
        if let AuthProvider::Ldap(c) = &mut s.provider {
            c.bind_password = SECRET_KEPT.into();
        }
        assert!(validate(&s, "portforward", None).is_err());
    }

    // --- proxy / jwt --------------------------------------------------------

    #[test]
    fn proxy_wires_the_trusted_proxy_pattern_into_xff() {
        let s = AuthProviderSpec {
            provider: AuthProvider::Proxy(ProxyConfig {
                user_header: "x-proxy-user".into(),
                internal_proxies: "10\\.10\\.0\\..*".into(),
                ..Default::default()
            }),
            role_mappings: vec![],
        };
        assert!(validate(&s, "portforward", None).is_ok());
        let d = yaml_of(&s, None);
        assert_eq!(d["config"]["dynamic"]["http"]["xff"]["enabled"], true);
        assert_eq!(
            d["config"]["dynamic"]["http"]["xff"]["internalProxies"],
            "10\\.10\\.0\\..*"
        );
        assert_eq!(
            d["config"]["dynamic"]["authc"]["proxy_auth_domain"]["http_authenticator"]["config"]
                ["roles_separator"],
            ","
        );
    }

    #[test]
    fn proxy_without_a_trusted_pattern_is_refused() {
        let s = AuthProviderSpec {
            provider: AuthProvider::Proxy(ProxyConfig {
                user_header: "x-proxy-user".into(),
                ..Default::default()
            }),
            role_mappings: vec![],
        };
        assert!(
            validate(&s, "portforward", None).is_err(),
            "forgeable identity header"
        );
    }

    #[test]
    fn jwt_defaults_the_claim_keys() {
        let s = AuthProviderSpec {
            provider: AuthProvider::Jwt(JwtConfig {
                signing_key: "base64key".into(),
                ..Default::default()
            }),
            role_mappings: vec![],
        };
        let d = yaml_of(&s, None);
        let cfg =
            &d["config"]["dynamic"]["authc"]["jwt_auth_domain"]["http_authenticator"]["config"];
        assert_eq!(cfg["subject_key"], "sub");
        assert_eq!(cfg["roles_key"], "roles");
        assert!(cfg.get("jwt_header").is_none(), "unset optionals stay out");
    }

    // --- wire format --------------------------------------------------------

    #[test]
    fn spec_round_trips_through_json_with_a_kind_tag() {
        let json = serde_json::to_value(oidc()).unwrap();
        assert_eq!(json["kind"], "oidc");
        assert_eq!(json["client_id"], "veloxsearch");
        let back: AuthProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, oidc());
    }

    #[test]
    fn an_absent_provider_deserializes_as_internal() {
        let s: AuthProviderSpec = serde_json::from_str(r#"{"kind":"internal"}"#).unwrap();
        assert_eq!(s.provider.kind(), "internal");
        assert!(s.role_mappings.is_empty());
    }
}
