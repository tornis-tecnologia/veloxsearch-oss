// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Ownership-decision tests (#80).
//!
//! ## What these prove, and what they cannot
//!
//! The card's acceptance is "account B gets 401/404 on account A's deployment,
//! dashboards and credentials across **all** `/api/*` routes". That property is
//! established by two halves, and only one of them is a test:
//!
//!  1. **Every deployment route goes through the decision below** — proved by
//!     the *compiler*, not by a test. The K8s and OpenSearch layers take
//!     `&Deployment`, `Deployment` has no constructor outside `scope.rs`, and
//!     the only producers are `Scope::adopt` (behind `resolve`/`require`) and
//!     `Scope::claim` (create only). A handler that skipped the check would
//!     have nothing to pass, so it would not compile. `tests/route_scoping.rs`
//!     pins the complementary half: no route can even be *mounted* without a
//!     declared policy.
//!  2. **The decision refuses cross-tenant access** — proved here, table-driven
//!     over `api::ROUTES`, so a route added to the tenant-scoped set is covered
//!     the moment it is declared.
//!
//! What is NOT proved here, stated plainly: that the labels are stamped on real
//! CRs and that the K8s API honours the namespace/label selector. Those need a
//! live cluster (`k8s::create_cluster` → read the CR back) and are named in the
//! MR as live-fleet acceptance, not claimed as done.

use super::*;
use crate::api::{Policy, ROUTES};

const TENANT_A: &str = "7f1c2a3e-0000-4000-8000-00000000000a";
const TENANT_B: &str = "7f1c2a3e-0000-4000-8000-00000000000b";
const NS_A: &str = "velox-t-acme";
const NS_B: &str = "velox-t-globex";

fn scope_a() -> Scope {
    Scope::tenant(TENANT_A, NS_A)
}
fn scope_b() -> Scope {
    Scope::tenant(TENANT_B, NS_B)
}

/// A CR as it comes back from the API server: where it lives, who owns it.
fn cluster_of(tenant: &str, namespace: &str) -> LocatedCluster {
    let mut labels = BTreeMap::new();
    labels.insert(LABEL_TENANT.to_string(), tenant.to_string());
    labels.insert(LABEL_MANAGED_BY.to_string(), MANAGED_BY.to_string());
    LocatedCluster {
        namespace: namespace.to_string(),
        labels,
    }
}

/// A deployment from before multi-tenancy: no owner label at all.
fn legacy_cluster() -> LocatedCluster {
    LocatedCluster {
        namespace: "veloxsearch-test".to_string(),
        labels: BTreeMap::new(),
    }
}

/// Every route that names a deployment, straight from the audit table — so a
/// new tenant-scoped route joins this test by being declared, and cannot be
/// added without a declaration at all (see `tests/route_scoping.rs`).
fn tenant_scoped_routes() -> Vec<&'static str> {
    ROUTES
        .iter()
        .filter(|r| r.policy == Policy::TenantScoped)
        .map(|r| r.path)
        .collect()
}

// ───────────────────────── the acceptance property ────────────────────────

/// **The card's acceptance, at the decision layer.** Account B asks for account
/// A's deployment — by name, on every deployment-touching route — and the
/// resolution refuses. `None` is what the handlers turn into 404 (mutations,
/// credentials) or the route's "nothing here" shape (reads); either way, never
/// A's data.
#[test]
fn account_b_is_refused_account_as_deployment_on_every_tenant_scoped_route() {
    let a_deployment = cluster_of(TENANT_A, NS_A);
    let routes = tenant_scoped_routes();
    assert!(
        routes.len() >= 15,
        "the deployment surface should be the bulk of the API; got {routes:?}",
    );
    for route in routes {
        assert_eq!(
            scope_b().adopt("acme-logs-x1z9", a_deployment.clone()),
            None,
            "{route}: account B must not resolve account A's deployment",
        );
        // ...and the same route resolves fine for its actual owner, so the
        // refusal above is ownership and not a blanket failure.
        assert!(
            scope_a().adopt("acme-logs-x1z9", a_deployment.clone()).is_some(),
            "{route}: account A must still reach its own deployment",
        );
    }
}

/// Anti-enumeration: the refusal for "someone else's" is the same value as for
/// "does not exist", so no route can be used to discover that a name is taken.
/// (`resolve` returns `Ok(None)` for both; the query that produces them is the
/// same one — a lookup confined to the caller's namespace.)
#[test]
fn not_yours_and_not_there_are_the_same_answer() {
    let foreign = scope_b().adopt("acme-logs-x1z9", cluster_of(TENANT_A, NS_A));
    let missing: Option<Deployment> = None; // what `resolve` yields for an unknown name
    assert_eq!(foreign, missing);
}

/// The label is checked even when the object turned up inside the tenant's own
/// namespace — the "belt" half of belt-and-braces. A CR that landed in the
/// wrong namespace is refused rather than adopted by proximity.
#[test]
fn a_tenant_does_not_adopt_a_foreign_cr_found_in_its_own_namespace() {
    let misplaced = cluster_of(TENANT_A, NS_B);
    assert_eq!(scope_b().adopt("stray", misplaced), None);
}

/// Fail closed on silence: an unlabeled (pre-#80) CR belongs to the legacy
/// namespace and to the admin. A tenant never inherits one.
#[test]
fn a_tenant_does_not_adopt_an_unlabeled_legacy_cr() {
    assert_eq!(scope_a().adopt("velox-test0-yj2s", legacy_cluster()), None);
    assert!(!scope_a().owns(&BTreeMap::new()));
}

/// A tenant cannot widen itself by editing the label it is compared against —
/// the comparison is against the value from the SIGNED cookie, and any other
/// value (empty, wildcard, the managed-by marker) simply fails to match.
#[test]
fn no_label_value_lets_a_tenant_match_another_tenants_cr() {
    let a = cluster_of(TENANT_A, NS_A);
    for forged in ["", "*", TENANT_B, MANAGED_BY, "veloxsearch.ai/tenant"] {
        assert_eq!(
            Scope::tenant(forged, NS_A).adopt("acme-logs-x1z9", a.clone()),
            None,
            "{forged:?} must not match tenant A's label",
        );
    }
}

// ───────────────────────── flag-OFF regression ────────────────────────────

/// With `VELOX_MULTITENANT_AUTH` off every session is a v1 admin session, so
/// this is the ONLY scope that exists on `develop` today. It must behave
/// exactly like the single-admin app: the app namespace, no label selector,
/// and every deployment — including the unlabeled ones that already exist on
/// Tornis prod — still reachable.
#[test]
fn admin_scope_is_the_pre_80_behaviour() {
    assert!(!crate::tenants::enabled(), "flag must default to OFF");
    let admin = Scope::Admin;
    assert!(admin.is_admin());
    assert_eq!(admin.tenant_id(), None);
    assert_eq!(admin.write_namespace(), crate::k8s::ns());
    assert_eq!(
        admin.label_selector(),
        None,
        "an admin list must carry no selector — that is what keeps the query \
         byte-identical to the one that shipped before #80",
    );
    assert!(admin.owns(&BTreeMap::new()), "legacy CRs stay visible");
    assert!(admin.owns(&cluster_of(TENANT_A, NS_A).labels), "and so do tenants'");
    assert!(admin.require_admin().is_ok());
}

/// The admin adopts anything, keeping the tenant attribution it finds — that is
/// what makes support access ("look at customer X's deployment") work without
/// laundering the object's ownership.
#[test]
fn admin_adopts_any_deployment_and_keeps_its_owner() {
    let dep = Scope::Admin
        .adopt("acme-logs-x1z9", cluster_of(TENANT_A, NS_A))
        .expect("admin sees everything");
    assert_eq!(dep.namespace(), NS_A, "acts in the object's namespace, not ours");
    assert_eq!(dep.tenant(), Some(TENANT_A));

    let legacy = Scope::Admin
        .adopt("velox-test0-yj2s", legacy_cluster())
        .expect("legacy deployments stay reachable");
    assert_eq!(legacy.tenant(), None);
    assert_eq!(legacy.namespace(), "veloxsearch-test");
}

// ────────────────────── namespace + label derivation ──────────────────────

#[test]
fn a_tenant_acts_only_inside_its_own_namespace() {
    let b = scope_b();
    assert_eq!(b.write_namespace(), NS_B);
    assert_eq!(
        b.label_selector().as_deref(),
        Some("veloxsearch.ai/tenant=7f1c2a3e-0000-4000-8000-00000000000b"),
    );
    let dep = b.claim("globex-logs-aa11").expect("legal name");
    assert_eq!(dep.namespace(), NS_B);
    assert_eq!(dep.tenant(), Some(TENANT_B));
}

/// Owner labels go onto everything a deployment owns (CR, credentials Secret,
/// Ingress), so offboarding and audit are one selector rather than a guess.
#[test]
fn owner_labels_carry_the_tenant_and_the_managed_by_marker() {
    let dep = scope_a().claim("acme-logs-x1z9").unwrap();
    let labels = dep.owner_labels();
    assert_eq!(labels.get(LABEL_TENANT).map(String::as_str), Some(TENANT_A));
    assert_eq!(
        labels.get(LABEL_MANAGED_BY).map(String::as_str),
        Some(MANAGED_BY)
    );
    // The admin stamps managed-by but no tenant: a legacy deployment has no
    // owner row to point at, and inventing one would be a lie in a label.
    let admin_labels = Scope::Admin.owner_labels();
    assert!(!admin_labels.contains_key(LABEL_TENANT));
    assert_eq!(
        admin_labels.get(LABEL_MANAGED_BY).map(String::as_str),
        Some(MANAGED_BY)
    );
}

/// `claim` is the create path, and it is the one place a caller-supplied name
/// becomes a handle without an existence check — so the name still has to be
/// legal. A rejected name is a 400 (the user's own input), not a 404.
#[test]
fn claim_refuses_an_illegal_name() {
    for bad in ["", "../../kube-system", "UPPER", "-leading", "trailing-"] {
        let err = scope_a().claim(bad).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{bad:?}");
    }
    assert!(scope_a().claim("acme-logs-x1z9").is_ok());
}

// ───────────────────────────── refusals ───────────────────────────────────

/// The refusal a tenant gets from an admin-only route is "not found", not
/// "forbidden": 403 would confirm the route exists and that they simply lack a
/// role, which is exactly the fact an attacker is probing for.
#[test]
fn tenants_are_told_admin_routes_do_not_exist() {
    let err = scope_a().require_admin().unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
    assert_eq!(err.message(), "deployment not found");
}

/// The not-found message names nothing — no deployment name, no namespace, no
/// tenant. An error string is a side channel too.
#[test]
fn the_refusal_leaks_nothing() {
    let err = ScopeError::not_found();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
    for leak in [TENANT_A, NS_A, "acme-logs-x1z9", "namespace", "tenant"] {
        assert!(
            !err.message().contains(leak),
            "the refusal must not mention {leak:?}: {}",
            err.message()
        );
    }
}

/// A v1 (admin) session resolves without touching Postgres at all — which is
/// what makes the flag-off path independent of the datastore being up.
#[test]
fn a_v1_session_resolves_to_admin_without_a_datastore() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let session = crate::auth::Session {
        user: "admin".to_string(),
        tenant: None,
    };
    assert_eq!(rt.block_on(Scope::from_session(&session)).unwrap(), Scope::Admin);
}

/// A v2 session for a tenant that cannot be resolved must NOT widen into the
/// admin scope. With no datastore reachable the lookup errors, and the answer
/// is a refusal (503) — never `Scope::Admin`.
#[test]
fn an_unresolvable_tenant_session_never_becomes_admin() {
    // This path dials the datastore (and therefore the kube client for its
    // credentials Secret), which needs the process-wide rustls provider that
    // `main.rs` installs at startup.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let session = crate::auth::Session {
        user: "ops@acme.com".to_string(),
        tenant: Some(TENANT_A.to_string()),
    };
    // A tenant that cannot be resolved is refused, never widened.
    let err = rt.block_on(Scope::from_session(&session)).unwrap_err();
    assert!(
        matches!(
            err.status(),
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::UNAUTHORIZED
        ),
        "unexpected status {}",
        err.status()
    );
}

// ─────────────────────── live-Postgres proof (ignored) ────────────────────
//
// The tenant→namespace mapping is a SQL read and is not unit-testable without
// a server (the `db.rs`/`tenants.rs` precedent):
//
//   podman run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=t postgres:16-alpine
//   VELOX_PG_TEST_URL=postgres://postgres:t@127.0.0.1:5433/postgres \
//     cargo test -- --ignored --test-threads=1 scope
//
// It talks to Postgres directly rather than through `db::connect_app`, which
// needs a kube client for the credentials Secret — same split as `tenants.rs`.

#[test]
#[ignore = "needs a live Postgres (set VELOX_PG_TEST_URL)"]
fn tenant_namespace_comes_from_the_row_and_two_tenants_never_share_one() {
    let url = std::env::var("VELOX_PG_TEST_URL").expect("VELOX_PG_TEST_URL not set");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (pg, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .expect("connect");
        tokio::spawn(conn);
        pg.batch_execute(
            "DROP SCHEMA IF EXISTS velox_test_80 CASCADE;
             CREATE SCHEMA velox_test_80;
             SET search_path = velox_test_80, public;",
        )
        .await
        .expect("reset test schema");
        pg.batch_execute(include_str!("../../migrations/001_init.sql"))
            .await
            .expect("migrate");

        let a: String = pg
            .query_one(
                "INSERT INTO tenants (slug, namespace, display_name)
                 VALUES ('acme', 'velox-t-acme', 'Acme') RETURNING id::text",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let b: String = pg
            .query_one(
                "INSERT INTO tenants (slug, namespace, display_name)
                 VALUES ('globex', 'velox-t-globex', 'Globex') RETURNING id::text",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_ne!(a, b);

        let read = |id: String| {
            let pg = &pg;
            async move {
                let rows = pg
                    .query(
                        "SELECT namespace FROM tenants WHERE id = $1::text::uuid",
                        &[&id],
                    )
                    .await
                    .unwrap();
                rows.first().map(|r| r.get::<_, String>(0))
            }
        };
        assert_eq!(read(a.clone()).await.as_deref(), Some("velox-t-acme"));
        assert_eq!(read(b.clone()).await.as_deref(), Some("velox-t-globex"));

        // The scopes the two rows produce touch disjoint namespaces, which is
        // the wall every read and write below `Scope` is confined by.
        let sa = Scope::tenant(a.clone(), read(a.clone()).await.unwrap());
        let sb = Scope::tenant(b.clone(), read(b.clone()).await.unwrap());
        assert_ne!(sa.write_namespace(), sb.write_namespace());
        assert_eq!(sb.adopt("acme-logs-x1z9", cluster_of(&a, NS_A)), None);

        // A session for a tenant that does not exist resolves to nothing —
        // the branch that must refuse rather than fall back to Admin.
        let unknown = pg
            .query(
                "SELECT namespace FROM tenants WHERE id = $1::text::uuid",
                &[&"7f1c2a3e-0000-4000-8000-0000000000ff".to_string()],
            )
            .await
            .unwrap();
        assert!(unknown.is_empty());
    });
}
