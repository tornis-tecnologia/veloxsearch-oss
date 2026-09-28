# Tenant isolation templates (ADR-044, issue #81)

Vendored per-tenant isolation primitives — the cluster-level floor under the
app-layer ownership walls (#80). Provisioned at tenant signup, one set per
tenant namespace: `namespace.yaml`, `resourcequota.yaml`, `limitrange.yaml`,
`networkpolicy.yaml`.

**These are templates, not live manifests.** `VELOX_*` tokens are replaced at
provision time by the control plane, the same vendored-bundle + token-replace
mechanism `src/bootstrap.rs::operator_bundle()` already uses (ADR-022).
`src/k8s.rs::provision_tenant()` `include_str!`s these four files, renders
them, and server-side-applies them in kind order (#81) — a token left
unreplaced is a test failure, so **editing a token name here without editing
`TENANT_TEMPLATES` in `src/k8s.rs` breaks the build's tests, not production**.

**RBAC.** The `veloxsearch-runtime` ClusterRole in `deploy/install.yaml` grants
`get`, `create` and `patch` on `resourcequotas` and `limitranges` (core) and on
`networkpolicies` (`networking.k8s.io`), cluster-wide, plus `create`/`patch`
(and read) on `namespaces`. That is exactly what a server-side apply needs:
a PATCH, which also needs `create` when the object is new. There is no
`update` and no `delete` — nothing replaces these objects and there is no
tenant teardown. Until #122 this grant was documented here but not shipped,
so every tenant was left without walls; the test
`runtime_cluster_role_can_apply_every_tenant_template_kind` in `src/k8s.rs`
now fails the build if a template kind is added without its grant.

A provisioning failure does not fail the signup. It is recorded in the audit
log (`tenant.provision_failed`), listed to the admin
(`bootstrap_status.unisolated_tenants`, shown as a notice), and retried by
`tenants::run_isolation_reconcile` at startup and on a slowing schedule.

The *other* half of ADR-044 wiring item 3 — per-tenant Secrets/Ingress
permissions, needed only once deployments themselves move into the tenant
namespace — is still owed (ADR-051).

| Token | Meaning |
| --- | --- |
| `VELOX_TENANT_NS` | `velox-t-<slug>` — the tenant namespace (`tenants.namespace`, ADR-041) |
| `VELOX_TENANT_SLUG` | tenant slug (URL/label-safe, ADR-041) |
| `VELOX_TENANT_ID` | `tenants.id` — the `veloxsearch.ai/tenant` owner-label value (ADR-044 amendment, #80) |
| `VELOX_CONTROL_PLANE_NS` | app namespace (`ns()`) |
| `VELOX_INGRESS_NS` | ingress-controller (Traefik) namespace (`VELOX_INGRESS_NAMESPACE`, default `traefik`) |
| `VELOX_AGENTS_NS` | collection-agent namespace (`src/agents.rs` `AGENT_NS`) |
| `VELOX_MINIO_NS` | snapshot MinIO platform namespace (ADR-042; `VELOX_MINIO_NAMESPACE`, default `minio`) |
| `VELOX_QUOTA_*` | rendered from the tenant's ADR-041 `quotas` row |

Design, rationale, worked quota defaults, enforcement caveats (CNIs without
NetworkPolicy support silently no-op), and the legacy-namespace migration path:
**ADR-044** (see [`docs/adr/README.md`](../../docs/adr/README.md)).
