# Architecture

*[Leia em português](ARCHITECTURE.pt-BR.md)*

VeloxSearch is a Rust control plane (axum) with a React SPA. It runs as a single
Pod in `veloxsearch-system`, talks to the Kubernetes API with a service account,
and manages OpenSearch deployments through the OpenSearch Kubernetes operator.

This document is the map. The design rationale for each module lives in the
doc-comment at the top of that module — those comments are dense on purpose and
are the authoritative source.

## The shape of a request

```
browser ──► /api/*  ──► auth_guard ──► handler (api.rs) ──► Scope ──► Deployment
                        (session         │                              │
                         cookie)         │                              ▼
                                         │                     k8s.rs (all writes)
                                         │                              │
                                         └──► pure module ──────────────┘
                                              (snapshot, upgrade,
                                               otel_stack, …)

browser ──► /*      ──► ServeDir (the SPA build) ──► index.html fallback
```

`main.rs` wires it in that order: the `/api` router, then the SPA `ServeDir`
fallback, then a global `auth::auth_guard` middleware layer over everything. Two
background tasks run alongside: `metrics::run_sampler` and
`version_feed::run_poller`. Postgres bring-up happens **before** serving and
exits the process on failure — fail closed, so the app never serves with a
half-migrated store.

## The `ssr` feature

Everything except `api` (the bare DTOs) sits behind the `ssr` feature, which
gates the entire Kubernetes/OpenSearch layer. `#[cfg(feature = "ssr")]` on a new
module is the norm, not the exception. The DTOs stay compilable alone so the
wire format can be reasoned about without dragging in kube-rs.

`default = ["ssr"]`, so a plain `cargo build` gives you the server.

## Module map

| Module | Role |
| --- | --- |
| `api.rs` | Every JSON handler, the route table (`routes()`), and the shared DTOs |
| `k8s.rs` | The Kubernetes/OpenSearch layer — the largest module, and the owner of **all** cluster writes |
| `scope.rs` | Ownership enforcement: `Scope` → `Deployment` capability token |
| `auth.rs` | Session cookie, token format, `auth_guard` |
| `tenants.rs`, `db.rs`, `mail.rs`, `email_denylist.rs` | Control-plane accounts (Postgres-backed, flag-gated) |
| `auth_provider.rs`, `auth_probe.rs` | Per-deployment LDAP/OIDC provider generation and the pre-save reachability probe |
| `bootstrap.rs` | First-run conformity gate; cert-manager / operator self-install |
| `recipes.rs`, `agents.rs`, `integrations.rs`, `catalog.rs` | Log integrations: built-in recipes, the Fluent Bit agents, the data-only apply engine, the signed-registry client |
| `otel_stack.rs` | The OpenTelemetry collection stack (an additive second option alongside recipes) |
| `capacity.rs`, `profiles.rs`, `metrics.rs`, `telemetry.rs`, `discovery.rs` | Sizing, cluster capacity, health sampling, discovery of existing telemetry |
| `snapshot.rs`, `upgrade.rs`, `activity.rs`, `provisioning.rs`, `version_feed.rs` | Day-2: snapshots, version upgrades, "is it settled?", deferred provisioning work |
| `access.rs` | How users reach dashboards (port-forward vs. ingress), read from a ConfigMap |
| `bin/velox.rs` | The `velox` operator CLI (`velox init`) |

## The two conventions that are load-bearing

### 1. Ownership is a type, not a check

`scope.rs` is the reason a forgotten authorization check is a compile error
rather than a security incident.

A handler receives an attacker-controlled `String`. To do anything with it, it
must turn that string into a `Deployment` — and the only way to mint one is
through a `Scope` derived from the signed session cookie. `Deployment` has
private fields, no public constructor and no `From<&str>`. The Kubernetes layer
takes `&Deployment`:

```rust
k8s::delete_cluster(&req.name)                        // does not compile
k8s::delete_cluster(&scope.require(&req.name).await?) // the only way in
```

Two rules follow, and both are absolute: **never** add a `&str`-taking entry
point to the Kubernetes layer, and **never** widen `Deployment`'s constructors.
Either one silently converts the guarantee back into a convention.

`Scope::resolve` returns `None` identically for "does not exist" and "belongs to
someone else" — same namespace, same label selector, same code path. The
anti-enumeration property is by construction rather than by care, and lookups
should be changed in a way that keeps it that way.

### 2. Pure module, and `k8s.rs` does the writing

`snapshot`, `upgrade`, `auth_provider`, `provisioning`, `activity` and
`otel_stack` make no cluster calls. They render artifacts (manifests, configs,
plans) and decide rules; `k8s.rs` applies the result. That split is what lets
most of the interesting logic be unit-tested with no cluster, no mocks and no
fixtures beyond plain data.

New logic belongs on the pure side.

## Off-cluster safety

Running the binary on a developer machine with a kubeconfig pointing at a real
cluster must never drive that cluster. Two mechanisms enforce it:

- `k8s.rs` falls back to the namespace `veloxsearch-dev`, which is deliberately
  chosen because it does not exist anywhere.
- `ensure_namespace_exists` turns every write against a missing namespace into a
  loud refusal rather than an error swallowed at some layer above.

Do not "fix" this by defaulting to a namespace that exists.

## API conventions

- No-argument reads are `GET`; everything else is `POST` with a JSON body whose
  field names match the DTO.
- Success returns the DTO as JSON (200), or an empty 200 for unit results.
- Errors return `{"error": "<message>"}` with 400 (validation), 401
  (credentials) or 500 (Kubernetes / OpenSearch layer).
- `login`, `logout` and `setup_admin` set the session cookie on the response.
- The deployment list streams over SSE at `GET /api/events`, every 3 seconds.

Adding an endpoint means four things, and the HTTP method must match on both
sides: a DTO in `api.rs`, a handler in `api.rs`, a line in `routes()`, and a
wrapper in `frontend/api.jsx`.

## Frontend

Plain React 18 + Vite. **No router and no state library.** Flat `.jsx` files at
the top level, not a `src/` tree.

`app.jsx` is the root. It boots off `GET /api/auth_state` and swaps screens by
state:

```
first_run                 → setup
!authenticated            → login
authenticated && !ready   → bootstrap
otherwise                 → the main app
```

Deployments arrive over the SSE stream. `localStorage` holds **only** UI
preferences (theme, language) — never server state, never mock data.

| File | Contents |
| --- | --- |
| `api.jsx` | Every REST wrapper plus the DTO adapters (`adaptDeployment`, …) |
| `i18n.jsx` | All UI strings, pt + en. UI text goes here, never inline in a view |
| `ui.jsx` | Shared primitives (Logo, Icon, Toast) |
| `views_*.jsx` | One file per screen; `views_deployment.jsx` is the big one |
| `styles.css` | The design system (CSS custom properties, IBM Plex, green accent) |
| `tweaks-panel.jsx` | The live design-tweak panel |

Because the end-to-end tests drive rendered widgets rather than URLs, renaming a
form field or reshaping a `nav.tabs` structure can break `tests/*_check.py`.
Check them when you reshape a screen.

## Integration packages are data

An integration is a manifest plus assets — ingest pipeline, index template,
saved objects, Fluent Bit config. It is never code, and two properties keep it
that way:

- **Interpolation is a closed set** of exactly eight tokens
  (`integrations::CLOSED_TOKENS`, [integrations/interpolation.md](integrations/interpolation.md)).
- **Signature verification is the entire security boundary**
  (`catalog::verify_package`, [integrations/signing.md](integrations/signing.md)).
  Unsigned, unknown-key and tampered are three distinct hard rejects, and the
  trusted keyring is compiled into the binary so verification needs no network.

A degraded registry is a state, not a crash: an unreachable or unauthorized
registry yields the last cached catalog marked stale, or the embedded bootstrap
catalog, always with a 200 and an error string the UI can show.

## Persistence

`migrations/*.sql` are plain SQL applied by the hand-rolled runner in `db.rs` —
deliberately the bare `tokio-postgres` driver, no ORM. Migrations must reach head
before the app serves. The query layer (sqlx vs. diesel) is a later, separate
decision that has not been made; see [ROADMAP.md](ROADMAP.md).

## Deploy artifacts

| Path | What it is |
| --- | --- |
| `deploy/install.yaml` | The generic one-file install manifest (ADR-027) |
| `deploy/Dockerfile` | Runtime image only; expects `./veloxsearch` and `./dist/` prebuilt |
| `deploy/bootstrap/` | Vendored cert-manager, OpenSearch operator and Longhorn manifests the first-run bootstrap applies. Large generated files — do not hand-edit |
| `deploy/tenant-templates/` | The per-tenant Namespace / ResourceQuota / LimitRange / NetworkPolicy set |

## Configuration

Every knob is an environment variable prefixed `VELOX_`. Feature flags default
to **off** and preserve prior behaviour when unset. The notable ones:

`VELOX_SITE_ADDR`, `VELOX_STATIC_DIR`, `VELOX_CONTROL_PLANE_NS`,
`VELOX_PG_ENABLED`, `VELOX_MULTITENANT_AUTH`, `VELOX_REGISTRY_URL`,
`VELOX_REGISTRY_TOKEN`, `VELOX_SESSION_SECRET`, `VELOX_COOKIE_SECURE`,
`VELOX_SMTP_*`. Secrets among them are inventoried in [SECRETS.md](SECRETS.md).

## Tests

Rust tests are inline `#[cfg(test)] mod tests` at the bottom of each module —
not a `tests/` Rust target, because `tests/` holds the Python end-to-end scripts.
`src/scope/tests.rs` is the one split-out exception.

See [DEVELOPMENT.md](DEVELOPMENT.md) for how to run the ones that need a live
Postgres or a registry checkout.
