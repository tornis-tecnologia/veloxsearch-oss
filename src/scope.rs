// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Per-request ownership enforcement — who is asking, and what they may touch
//! (#80; layer 1 of the ADR-044 isolation model).
//!
//! ## The shape of the guarantee
//!
//! Every `/api` handler that names a deployment must first turn that *name*
//! (attacker-controlled, straight out of a JSON body) into a [`Deployment`]
//! — and the only way to mint one is through a [`Scope`], which comes from the
//! signed session cookie and nothing else. `Deployment` has private fields, no
//! public constructor and no `From<&str>`, so the K8s and OpenSearch layers can
//! take `&Deployment` instead of `&str` and **an unscoped call stops being a
//! forgotten `if`, and becomes a compile error**:
//!
//! ```text
//!   k8s::delete_cluster(&req.name)      // does not compile: &String is not &Deployment
//!   k8s::delete_cluster(&scope.require(&req.name).await?)   // the only way in
//! ```
//!
//! That is the whole design. The route table in `api.rs` says which policy each
//! route carries; this module makes the scoped path the only reachable one.
//!
//! ## Anti-enumeration
//!
//! [`Scope::resolve`] answers `None` for a deployment that does not exist AND
//! for one that exists but belongs to someone else — not by convention but *by
//! construction*: a tenant's lookup is issued against the tenant's own
//! namespace with the tenant's own label selector, so a foreign name and a
//! nonexistent name go down literally the same code path and produce the same
//! bytes. A caller therefore cannot use the API to learn that a deployment
//! exists. Handlers turn that `None` into a 404 (`deployment not found`) for
//! mutations, or into the route's existing "nothing here" shape for reads.
//!
//! ## The admin session (v1) is the super-tenant
//!
//! A v1 token (`user:exp:sig`, minted only by the installation admin
//! credential in `auth.rs`) carries no tenant and resolves to [`Scope::Admin`]:
//! it sees and mutates everything, in every namespace. That is a deliberate,
//! explicitly-gated decision, not an oversight — the installation operator has
//! `kubectl` on the cluster anyway, so scoping them at the app layer would buy
//! nothing and would break every ops path (bootstrap, access settings, support
//! access to a customer deployment). It is also what keeps
//! `VELOX_MULTITENANT_AUTH=off` a no-op: with the flag off, every session is a
//! v1 session, every scope is `Admin`, the namespace is `k8s::ns()` and no
//! label selector is applied — byte-identical to the single-admin app.
//!
//! `Scope::Admin` is unforgeable for the same reason the tenant id is: the
//! whole token is HMAC-signed and `parse_token` is the only reader.

use anyhow::{Context, Result};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use std::collections::BTreeMap;

/// Owner label stamped on every `OpenSearchCluster` CR the control plane
/// creates for a tenant, and the selector every tenant read filters by.
///
/// The value is the `tenants.id` uuid, not the slug: the slug is a display
/// handle that a future rename could move, the id is the primary key ADR-041
/// makes authoritative. Postgres remains the source of truth — this label is
/// the denormalized cache ADR-041 describes, and when the two disagree the
/// namespace boundary (which is derived from Postgres on every request) wins,
/// because a CR in the wrong namespace is never even listed.
///
/// The `veloxsearch.ai/` prefix matches the label vocabulary already on the CRs
/// (`veloxsearch.ai/size`, `/purpose`, `/auth-kind`). ADR-041/044 write it as
/// `veloxsearch.io/tenant`; that is an erratum in the ADR text — this
/// installation has never had a `veloxsearch.io` label and one product must not
/// carry two label domains. Noted in DECISIONS.md.
pub const LABEL_TENANT: &str = "veloxsearch.ai/tenant";

/// Marks objects this control plane owns, so cluster-wide sweeps (and the
/// ADR-044 NetworkPolicy peers) can select them without knowing the tenant.
pub const LABEL_MANAGED_BY: &str = "veloxsearch.ai/managed-by";
pub const MANAGED_BY: &str = "veloxsearch";

// ───────────────────────────── the scope ─────────────────────────────

/// Who is acting, and inside which walls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The installation admin (a v1 session). Super-tenant — see the module
    /// docs for why this is deliberate.
    Admin,
    /// A control-plane account acting for exactly one tenant (a v2 session).
    Tenant {
        /// `tenants.id` — the value carried in the signed cookie.
        id: String,
        /// `tenants.namespace` — resolved from Postgres, never derived from
        /// the cookie, so a tenant cannot name its own namespace.
        namespace: String,
    },
}

impl Scope {
    /// A tenant scope. `pub(crate)` on purpose: outside this crate the only
    /// producer is the extractor, which reads a signed session.
    pub(crate) fn tenant(id: impl Into<String>, namespace: impl Into<String>) -> Self {
        Scope::Tenant {
            id: id.into(),
            namespace: namespace.into(),
        }
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Scope::Admin)
    }

    /// The tenant id, or `None` for the admin. `None` means "not tenant-
    /// scoped" — never "any tenant" (the same rule `auth::Session` states).
    pub fn tenant_id(&self) -> Option<&str> {
        match self {
            Scope::Admin => None,
            Scope::Tenant { id, .. } => Some(id),
        }
    }

    /// The namespace this scope WRITES into. Admin writes into the app
    /// namespace — the ADR-044 `legacy` namespace that today's deployments
    /// already live in, grandfathered rather than migrated.
    pub fn write_namespace(&self) -> String {
        match self {
            Scope::Admin => crate::k8s::ns().to_string(),
            Scope::Tenant { namespace, .. } => namespace.clone(),
        }
    }

    /// Label selector for tenant-scoped reads, or `None` for the admin.
    /// Belt to the namespace's braces: even if a CR were somehow created in the
    /// wrong namespace, it is not returned to a tenant without the right label.
    pub fn label_selector(&self) -> Option<String> {
        self.tenant_id().map(|id| format!("{LABEL_TENANT}={id}"))
    }

    /// Owner labels to stamp on objects this scope creates.
    pub fn owner_labels(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        out.insert(LABEL_MANAGED_BY.to_string(), MANAGED_BY.to_string());
        if let Some(id) = self.tenant_id() {
            out.insert(LABEL_TENANT.to_string(), id.to_string());
        }
        out
    }

    /// Does this scope own an object carrying these labels?
    ///
    /// Fails closed for a tenant: the label must be present AND equal. An
    /// unlabeled CR is a legacy (pre-multi-tenant) deployment and belongs to
    /// the admin's namespace — a tenant never inherits one by silence.
    pub fn owns(&self, labels: &BTreeMap<String, String>) -> bool {
        match self.tenant_id() {
            None => true,
            Some(id) => labels.get(LABEL_TENANT).map(String::as_str) == Some(id),
        }
    }

    /// Resolve a caller-supplied name into a [`Deployment`] this scope owns.
    ///
    /// `Ok(None)` for "no such deployment **in your scope**" — nonexistent and
    /// not-yours are the same answer through the same query (see the module
    /// docs). `Err` is reserved for a broken cluster connection, which is our
    /// fault and not an ownership signal.
    pub async fn resolve(&self, name: &str) -> Result<Option<Deployment>> {
        // Reject syntactically impossible names before they reach the API
        // server: `../` style values and label-invalid junk are not lookups.
        if crate::k8s::validate_name(name).is_err() {
            return Ok(None);
        }
        let Some(found) = crate::k8s::locate_cluster(self, name)
            .await
            .context("looking up deployment")?
        else {
            return Ok(None);
        };
        Ok(self.adopt(name, found))
    }

    /// Turn a located CR into a [`Deployment`], or refuse it. Split out of
    /// [`Scope::resolve`] so the *decision* — the security-relevant half — is
    /// testable without a cluster, which is what the `#[test]` table below
    /// exercises for both identities across every route's resolution path.
    pub(crate) fn adopt(&self, name: &str, found: LocatedCluster) -> Option<Deployment> {
        if !self.owns(&found.labels) {
            // For a tenant this is only reachable if a CR landed in the tenant
            // namespace without the owner label: refuse rather than adopt.
            tracing::warn!(
                deployment = %name, namespace = %found.namespace,
                "CR is not owned by the caller's scope; answering not-found",
            );
            return None;
        }
        Some(Deployment {
            name: name.to_string(),
            namespace: found.namespace,
            tenant: found.labels.get(LABEL_TENANT).cloned(),
        })
    }

    /// [`Scope::resolve`], with `None` already turned into the one refusal
    /// every deployment route gives: **404, `deployment not found`** — the same
    /// answer whether the name is unknown, misspelled, or someone else's.
    pub async fn require(&self, name: &str) -> Result<Deployment, ScopeError> {
        match self.resolve(name).await {
            Ok(Some(d)) => Ok(d),
            Ok(None) => Err(ScopeError::not_found()),
            Err(e) => Err(ScopeError::internal(e)),
        }
    }

    /// A handle for a deployment that is about to be CREATED. No existence
    /// check (there is nothing to check yet) — the name must merely be legal,
    /// and it is placed in this scope's namespace with this scope's owner
    /// labels, which is what makes the created CR resolvable later.
    pub fn claim(&self, name: &str) -> Result<Deployment, ScopeError> {
        crate::k8s::validate_name(name)
            .map_err(|e| ScopeError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
        Ok(Deployment {
            name: name.to_string(),
            namespace: self.write_namespace(),
            tenant: self.tenant_id().map(str::to_string),
        })
    }

    /// Refuse anything but the installation admin, with the anti-enumeration
    /// answer: a tenant is told the route does not exist, not that it is
    /// forbidden (which would confirm the feature).
    pub fn require_admin(&self) -> Result<(), ScopeError> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(ScopeError::not_found())
        }
    }
}

/// What a CR lookup found: where it lives and who it says it belongs to.
/// Just enough of the object for an ownership decision, so that decision can
/// be unit-tested without a cluster.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocatedCluster {
    pub namespace: String,
    pub labels: BTreeMap<String, String>,
}

// ────────────────────────── the capability token ─────────────────────────

/// A deployment name that has been resolved inside a caller's [`Scope`], plus
/// the namespace it actually lives in.
///
/// Construction is private to this module, so holding one is *proof* that an
/// ownership check ran. Every K8s/OpenSearch entry point that acts on a
/// deployment takes `&Deployment` for exactly that reason.
///
/// There is **no escape hatch** in a shipped binary — no `From<&str>`, no
/// crate-private constructor; the one bypass, `for_test`, is `#[cfg(test)]`.
/// The background paths that used to take a bare name (the deferred
/// profile/recipe task, the metrics sampler) now carry a handle they were
/// *given*: the task clones the one `create_cluster` produced, and the sampler
/// iterates `k8s::scoped_deployments(&Scope::Admin)`. So auditing this layer is
/// one grep — `Scope::(resolve|require|claim|adopt)` — and all four live in
/// this file.
///
/// `Display` yields the bare name, so the many `format!("...{deployment}...")`
/// call sites downstream (index names, service DNS, agent names) are unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    name: String,
    namespace: String,
    tenant: Option<String>,
}

impl Deployment {
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The namespace the CR, its Secrets and its Ingress live in — and the
    /// `.svc` middle segment for the OpenSearch/Dashboards service DNS.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn tenant(&self) -> Option<&str> {
        self.tenant.as_deref()
    }

    /// Test-only constructor, so unit tests elsewhere in the crate can build a
    /// fixture. `#[cfg(test)]`: the escape hatch does not exist in a shipped
    /// binary, so the "only a Scope can mint one" guarantee above is
    /// unconditional outside the test profile.
    #[cfg(test)]
    pub(crate) fn for_test(name: &str, namespace: &str, tenant: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            namespace: namespace.to_string(),
            tenant: tenant.map(str::to_string),
        }
    }

    /// Owner labels for objects created alongside this deployment.
    pub fn owner_labels(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        out.insert(LABEL_MANAGED_BY.to_string(), MANAGED_BY.to_string());
        if let Some(t) = &self.tenant {
            out.insert(LABEL_TENANT.to_string(), t.clone());
        }
        out
    }
}

impl std::fmt::Display for Deployment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

// ───────────────────────────── rejections ────────────────────────────

/// A refusal from the ownership layer, shaped like `api::ApiError` so handlers
/// can `?` it straight out (`{ "error": "..." }` + a status).
#[derive(Debug)]
pub struct ScopeError {
    status: StatusCode,
    message: String,
}

impl ScopeError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    /// The one answer for "not yours or not there" (see module docs).
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "deployment not found")
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }
    fn internal(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for ScopeError {
    fn into_response(self) -> Response {
        (
            self.status,
            axum::Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

// ─────────────────────────── the extractor ───────────────────────────

/// `Scope` as an Axum extractor: a handler that declares `scope: Scope` in its
/// signature cannot run without one.
///
/// The session cookie is re-verified here rather than trusted from the
/// middleware — the guard in `auth.rs` decides *whether* a request proceeds,
/// this decides *as whom*, and one of them re-reading the cookie is cheaper
/// than a shared mutable request extension that a future middleware could set.
impl<S> FromRequestParts<S> for Scope
where
    S: Send + Sync,
{
    type Rejection = ScopeError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let session = parts
            .headers
            .get(header::COOKIE)
            .and_then(|c| c.to_str().ok())
            .and_then(crate::auth::session_from_cookies)
            .ok_or_else(|| ScopeError::unauthorized("authentication required"))?;
        Scope::from_session(&session).await
    }
}

impl Scope {
    /// Session → scope. The tenant's namespace is read from Postgres
    /// (`tenants.namespace`, ADR-041 authoritative) and never derived from the
    /// cookie. A tenant whose row cannot be read is refused — a session with a
    /// tenant id must never silently widen into the admin scope.
    pub async fn from_session(session: &crate::auth::Session) -> Result<Self, ScopeError> {
        let Some(tenant_id) = session.tenant.as_deref() else {
            return Ok(Scope::Admin);
        };
        match tenant_namespace(tenant_id).await {
            Ok(Some(namespace)) => Ok(Scope::tenant(tenant_id, namespace)),
            Ok(None) => {
                // A signed cookie for a tenant that no longer exists (deleted
                // account). Fail closed as unauthenticated.
                tracing::warn!(tenant = %tenant_id, "session names a tenant with no row");
                Err(ScopeError::unauthorized("session is no longer valid"))
            }
            Err(e) => {
                tracing::error!(tenant = %tenant_id, "resolving tenant namespace: {e:#}");
                Err(ScopeError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the control plane is temporarily unavailable",
                ))
            }
        }
    }
}

/// `tenants.namespace` for a tenant id, memoized.
///
/// The mapping is immutable for the life of a tenant (`tenants.namespace` is
/// `UNIQUE NOT NULL` and nothing updates it — ADR-041/044 treat a namespace
/// move as a recreate), so caching it is not a staleness risk, and it keeps the
/// ownership check off the database on every single API call. A miss is NOT
/// cached: a tenant created after this process started must become resolvable
/// without a restart.
async fn tenant_namespace(tenant_id: &str) -> Result<Option<String>> {
    use std::collections::HashMap;
    use std::sync::RwLock;
    static CACHE: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

    if let Some(map) = CACHE.read().unwrap().as_ref() {
        if let Some(ns) = map.get(tenant_id) {
            return Ok(Some(ns.clone()));
        }
    }
    let pg = crate::db::connect_app().await?;
    let rows = pg
        .query(
            "SELECT namespace FROM tenants WHERE id = $1::text::uuid",
            &[&tenant_id],
        )
        .await
        .context("reading tenants.namespace")?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let namespace: String = row.get(0);
    CACHE
        .write()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(tenant_id.to_string(), namespace.clone());
    Ok(Some(namespace))
}

#[cfg(test)]
mod tests;
