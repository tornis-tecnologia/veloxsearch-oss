# Architecture Decision Records

The source is dense with citations like `ADR-039` and `ADR-044`. This index says
what each number decided, so a citation in a comment is readable without leaving
the file.

## How to read a citation

**The rationale is almost always restated where it is cited.** The codebase's
convention is that a comment explains *why* and names the decision it came from
— the ADR number is a stable identifier for the decision, not a pointer you have
to follow to understand the code. If a comment cites an ADR and does not tell
you enough, that is a documentation bug worth reporting.

The full decision log predates the open-source release and is not published: it
carries infrastructure and client detail that has to come out first. The table
below is the part that is safe and useful — what each number settled — and
individual ADRs will be migrated into this directory as they are redacted.

New decisions are written here from the start, so this gap closes rather than
grows.

## Proposing a new decision

Anything that changes a boundary — the ownership model in `src/scope.rs`, the
signing path in `src/catalog.rs`, the platform contract in
[REQUIREMENTS.md](../REQUIREMENTS.md), the API shape, the integration package
format — gets an ADR **before** the implementation.

Open a pull request adding `docs/adr/ADR-0NN-short-title.md`:

```markdown
# ADR-0NN — Short title

**Status:** proposed | accepted | superseded by ADR-0MM
**Date:** YYYY-MM-DD

## Context
What forces are at play? What is true today that makes this a decision rather
than an obvious step?

## Decision
What we are doing. Present tense, active voice.

## Consequences
What becomes easier, what becomes harder, and what is now foreclosed. Be honest
about the cost — an ADR with no downside listed is an ADR that has not been
thought through.

## Alternatives considered
Each with the reason it lost. This section is the one future readers use most.
```

Discussion happens on that PR. See [GOVERNANCE.md](../../GOVERNANCE.md).

## Index

| ADR | Decides |
| --- | --- |
| ADR-001 | The operator CLI: `velox` as a separate binary rather than a subcommand or a script |
| ADR-002 | Two-phase RBAC — `veloxsearch-runtime` for day-to-day work, `veloxsearch-bootstrap` with cluster-admin for the one-time self-install |
| ADR-003 | Kubernetes as the control-plane datastore: managed credentials live in a Secret |
| ADR-005 | Deployment status is fundamentally poll-based; the SSE stream is a push view over a polling sampler |
| ADR-014 | Cluster conformity: which prerequisites must be present and serving before anything is promised |
| ADR-015 | Snapshots: basics first — one repository, scheduled, before any restore UI |
| ADR-016 | Sizing tiers: a deployment is always 3 nodes; presets plus a "custom size" profile |
| ADR-017 | Purpose profiles: what applying a purpose actually changes |
| ADR-018 | Baseline monitors, applied to every deployment via the deferred provisioning path |
| ADR-019 | The backend never ships user-facing prose; the UI translates codes (the i18n rule) |
| ADR-020 | Deployment naming: DNS-safe, `<name>-<suffix>`, capped at 30 characters |
| ADR-022 | VeloxSearch installs and owns cert-manager and the OpenSearch operator, from vendored manifests |
| ADR-023 | App-managed credentials: CSPRNG-generated, stored in Kubernetes Secrets, never defaulted |
| ADR-024 | The UI must stay free of hydration and panic errors — asserted by the browser checks |
| ADR-025 | Image distribution: side-load supported as a first-class path, pull Secret optional |
| ADR-026 | Requirements-first: keep the supported envelope narrow, make everything inside it bulletproof, refuse clearly outside it |
| ADR-027 | The generic one-file install manifest, the access modes (port-forward / ingress), and self-revocation of the bootstrap binding |
| ADR-028 | Purpose profiles applied at provisioning time, including retention per index class |
| ADR-030 | Control-plane credentials and per-deployment credentials are separate credential domains |
| ADR-031 | PVC-backed deployments; node-local provisioners are never acceptable |
| ADR-032 | The React SPA (superseding the Leptos-era UI) |
| ADR-034 | Non-secret runtime configuration lives in the `veloxsearch-env` ConfigMap; secrets never do |
| ADR-035 | Memory bounds: the operator derives heap as memory/2, so heap is never set directly |
| ADR-036 | The bundled Postgres StatefulSet pattern and how its passwords reach it |
| ADR-038 | The corporate-email gate for self-serve signup |
| ADR-039 | Integration packages are data, not code: the manifest format, the apply engine, and signing as the security boundary (registry hosting and key custody superseded by ADR-054) |
| ADR-040 | Version literals are derived, not hand-copied across files |
| ADR-041 | The Postgres-backed control-plane account store, and the tenant model on top of it |
| ADR-042 | The snapshot MinIO platform namespace |
| ADR-043 | Longhorn is the only supported deployment storage (amends ADR-031) |
| ADR-044 | The per-tenant namespace bundle: ResourceQuota, LimitRange, and default-deny NetworkPolicy |
| ADR-045 | The auth-provider axis: pure LDAP/OIDC generators plus a pre-save reachability probe |
| ADR-046 | Longhorn disk-headroom settings: the one deliberate divergence from the vendored upstream bundle |
| ADR-047 | A degraded registry is a state, not a crash: stale cache, then the embedded bootstrap catalog |
| ADR-048 | Version feed and upgrade pre-flight: the pinned catalog plus the hourly upstream check |
| ADR-049 | The snapshot/backup configuration surface: bucket, endpoint, schedule |
| ADR-050 | The activity view: a stalled deployment must say why, in place |
| ADR-051 | What the tenant NetworkPolicy set does *not* enforce, stated explicitly |
| ADR-052 | Deferred provisioning re-runs: profile and monitors re-applied to an existing CR |
| ADR-053 | The four-step create wizard (purpose → size → backup → review) and the opt-in next-generation Dashboards UI |
| ADR-054 | Registry and signing live on GitHub and Docker Hub — the public `veloxsearch-registry` repo, cosign-keyless Docker Hub image, signing key held outside the repo (supersedes the internal-registry plan) |

Numbers absent from this table (ADR-004, 006–013, 021, 029, 033, 037) were
either withdrawn before implementation or superseded by a later decision, and
are not cited anywhere in the source.
