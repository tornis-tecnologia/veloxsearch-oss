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
| ADR-043 | Longhorn is the only supported deployment storage (amends ADR-031; amended by ADR-061) |
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
| ADR-055 | *(decision 1 superseded by ADR-063)* VeloxSearch may surgically patch the operator's Dashboards Deployment — survivability fields under a dedicated field manager, one-shot `.kibana_1` remediation, container logs stay unread |
| ADR-056 | A healed dependency re-arms one exhausted provisioning wave — the metrics sampler triggers it, the CR counter still bounds it (amends ADR-052 rule 4) |
| ADR-057 | The upgrade contract: an upgrade changes the VeloxSearch control plane (and ADR-055's fields), never the operator, its CRDs or `OpenSearchCluster` specs — bootstrap installs only what is absent, operator drift is reported (R9), upgrades apply `upgrade.yaml` (no bootstrap binding, no `veloxsearch-env`: the operator's settings survive, #128) and a re-applied binding is revoked again |
| ADR-058 | Multiple integration catalog sources — sources as Secret-backed config, per-source key pinning with admin-approved keys, `source/id` provenance, install records so uninstall outlives a source, collector configs confined (proposed) |
| ADR-059 | *Proposed.* Collector configuration for sources outside the cluster — optional `collectors` manifest section (schema 1.1), a write-only `_bulk` ingest route per deployment, one ingest-only OpenSearch principal per integration, never admin credentials |
| ADR-060 | A bounded cluster profile (`GET /api/cluster_profile`): derived facts only, one field allowlist enforced by test, tenant-scoped, workload percentiles over the sampler's retained window, exported only by download — never pushed (accepted 2026-09-26, #59) |
| ADR-061 | Flexible default storage — Longhorn when present; otherwise the cluster's default StorageClass (foreign CSI durable, node-local warned); Longhorn auto-bootstrap only when no default exists (amends ADR-043, #88) |
| ADR-062 | Default retention per purpose — installation default in `veloxsearch-config` (admin), stamped per deployment on the CR at create with its source (default/override), apply-once: a `velox-retention` policy the user edited in OpenSearch is never rewritten except by an explicit reset; existing deployments that inherit the default move only through an explicit admin action |
| ADR-063 | The Dashboards first-boot fix goes through the CR: `spec.dashboards.replicas` held at 0 until the cluster settles, remediation holds the same way, no write on the operator's Deployment (supersedes ADR-055 decision 1, #46) |
| ADR-064 | *(proposed)* Admin-password reset safety — refused (409) unless the deployment is settled and no reset is pending; a failed operator nudge rolls the Secret back (or says the password changed); a failed securityconfig Job restores the previous password (#115) |
| ADR-065 | *(proposed)* Longhorn by default, with an opt-out — an explicit, recorded storage choice at first run with Longhorn pre-selected on a node-local default; node prerequisites pre-flighted before the question; the bootstrap binding held open until the recorded choice is satisfied (amends ADR-061) |
| ADR-066 | *(proposed)* Operator upgrade from Settings — a read-only impact report (CRD fields/versions removed, a removed field a live cluster sets blocks; per-pool restarts, single-node pools down; not-green, mid-upgrade, not-ready block) and a button armed by a one-time `veloxsearch-operator-upgrade` cluster-admin grant the app deletes after the upgrade or after 30 min unused (amends ADR-057, #176) |
| ADR-067 | *(proposed)* The console as a product shell — every screen has a URL (`/deployments/:id/:tab`, History API, no library; the login redirect keeps `?next=`), a sidebar of places with actions as buttons, deployment tabs grouped by task (Overview · Data · Backup · Access · Configuration · Activity) with delete only under Configuration and the ⋯ menu, one source per Overview fact, a truthful toast and one accessible Dialog, one vocabulary (deployment / backup / health words), sans for the interface and mono for data, stable `data-testid` hooks for the e2e scripts |
| ADR-068 | *(proposed)* Velox Mesh and Velox Agent live in `veloxagent/`, a separate Cargo workspace with separate binaries and images (the agent stays a small static binary), under the same `AGPL-3.0-only` licence and DCO as the core; the integration renderer is extracted into a shared crate both sides use; integrations stay signed data; the ten-agent free tier was a commercial signal, not a lock (withdrawn 2026-11-12: no agent limit). [ADR-068](ADR-068-veloxagent-workspace.md) |

| ADR-069 | *(proposed)* The collection engines are the operator's to install — VeloxSearch distributes no Fluent Bit binary and downloads none (upstream publishes no standalone static Linux build); the console carries the per-distribution procedure, the agent enforces a minimum of Fluent Bit 3.2 and names the version it found, generated configs use absolute paths (hot reload only keeps the config directory from 5.1.2), and `--dry-run` is probed rather than assumed. *Amendment 1:* `install.sh` runs that procedure (upstream repository, never our build), the agent's own `velox-agent-v*` release (musl tarballs, SHA256SUMS, cosign), one install root `/opt/velox-agent` with a `velox-agent` user holding `CAP_DAC_READ_SEARCH`, and a container image `FROM` upstream's Fluent Bit image, unmodified. [ADR-069](ADR-069-engines-are-the-operators-to-install.md) |

| ADR-070 | *(proposed)* Every Fluent Bit collector — the in-cluster DaemonSet and every host agent — writes with `veloxagent-user` holding `veloxagent-role` (`read` + `write` + `create_index` on `veloxagent-*`, plus bulk at cluster level; nothing wider); only the core holds the admin login, to create the account. OpenSearch has no API keys, so the credential is a user and a password: read-or-created in a Secret beside the deployment, carried to host agents by the Mesh as the declared extension `velox_output_credential` and to in-cluster collectors through their Secret, never into a ConfigMap or a policy revision. [ADR-070](ADR-070-the-collector-writes-with-a-scoped-account.md) |

| ADR-071 | *(proposed)* Every collection artifact on both paths is named `veloxagent-<integration>-<signal>-v<major>` — one string for the index, the index template, the ingest pipeline and the index pattern (saved objects suffixed off it); the signal from a closed list, the major in the name so a mapping change ships as `-v2`; readers key on the version-agnostic `-v*` family. The rename is a parsed-JSON transformation at install (the registry and its signatures are untouched), the built-in recipes install through the same function, and existing deployments migrate themselves on the next sampler tick, keeping their old indices as history. [ADR-071](ADR-071-agent-artifacts-are-named-veloxagent.md) |

| ADR-072 | *(proposed)* Velox Mesh is a deployment's, not the installation's: one controller, data partitioned by `(namespace, deployment)` on every policy and agent row; every console call names a deployment the core minted through `Scope::require`, agents belong to the deployment they enrolled under (moving means re-enrolling), deleting a deployment forgets its policies, tokens and agents, uninstalling an integration a policy activates is refused, tenants operate the Mesh of their own deployments, and the screen moves to `/deployments/:id/mesh`. Amendment 2: the deployment's settings are **named endpoint lists** (OpenSearch `host[:port]`, Mesh URL, one CR annotation), every policy is bound to one of each and cannot be saved without both (no in-cluster default), rebinding is a new revision, and every agent runs a policy. [ADR-072](ADR-072-the-mesh-is-scoped-to-a-deployment.md) |

| ADR-073 | *(proposed)* One onboarding for an integration, modelled on Fleet's: a package declares **inputs** (schema 1.2 — a signal, a Fluent Bit template per target, the variables they reference, optional backend assets of their own; a 1.0 package is one implicit `logs` input), an activation is the same `{inputs: {id: {vars}}}` on a Mesh policy and on the cluster's CR, the Data tab's *Add* dialog asks where (this cluster, a policy, a new policy) and what (inputs, variables), and the backend half is torn down only when the last target gives the integration up. [ADR-073](ADR-073-one-onboarding-for-an-integration.md) |

| ADR-074 | *(proposed)* Registry Spec 2.0 on the Mesh path — a package declares what can be configured (streams: a catalogued Fluent Bit input, typed fields with bindings, parsers, a processor chain, backend template/pipeline/lifecycle) and the core verifies (same digest algorithm, files enumerated from the signed `files:` inventory, each sha256 checked), validates with strict typed parsing plus the R-rules, compiles Fluent Bit **YAML** as an object (equivalent to the registry's `render.py`), installs per-stream artifacts named `veloxagent-<stream>.<integration>-v<major>` (data stream, `N@package`/`N@custom` component templates, pipeline, ISM, index pattern; supersedes ADR-071 for 2.0 packages) shared across policies by major, stores a Spec 2.0 activation on the revision with secrets outside it, routes outputs by `event_type` (metrics → OTLP endpoint and the OTel stack), and keeps the cluster path on 1.x until F6. `{state_dir}` compiles to `${VELOX_STATE_DIR}`. [ADR-074](ADR-074-registry-spec-2-packages.md) |
| ADR-075 | *(proposed)* A full installer, `deploy/install-veloxsearch-full.sh` (a release asset): three bare servers become a k3s HA cluster on embedded etcd (pinned version, secrets encryption, etcd snapshots, **`local-storage` disabled** so a node-local default never wins over replicated storage), every node prepared (open-iscsi/NFS/cryptsetup, modules, `vm.max_map_count`, swap off, multipathd blacklist, firewall) behind a hard preflight, Longhorn applied from the release's vendored bundle with the app's field manager **before** VeloxSearch, then the checksummed `install.yaml`. Driven from node 1 over SSH (`install --peers`) or node by node (`init`/`join`/`platform`); the admin is still created at `/setup`. [ADR-075](ADR-075-full-installer-ha-k3s.md) |

Numbers absent from this table (ADR-004, 006–013, 021, 029, 033, 037) were
either withdrawn before implementation or superseded by a later decision, and
are not cited anywhere in the source.
