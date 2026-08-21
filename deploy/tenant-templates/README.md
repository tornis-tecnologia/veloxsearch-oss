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
The `veloxsearch-runtime` ClusterRole gained `resourcequotas`/`limitranges`/
`networkpolicies` create/update so the control plane can apply them (#81); the
*other* half of ADR-044 wiring item 3 — per-tenant Secrets/Ingress permissions,
needed only once deployments themselves move into the tenant namespace — is
still owed (ADR-051).

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
