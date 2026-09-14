# ADR-059 — Collector configuration for sources outside the cluster

**Status:** proposed
**Date:** 2026-09-14

## Context

Installing an integration today PUTs its ingest pipeline and index template,
bulk-creates its saved objects, and deploys a Fluent Bit agent in
`velox-agents` (`integrations::apply_package`, `src/integrations.rs` L420;
`src/agents.rs`). That agent tails `/var/log` on the nodes of the cluster
VeloxSearch runs in. A VM, a Windows host or a database running somewhere
else gets nothing: the dashboards stay empty, and the user has to work out
which agent to run, where to send data, with which credential, and which
field the pipeline parses.

The package already knows most of the answer. Its index template sets
`index.default_pipeline` (`src/recipes.rs` L1724), so any document written to
`{index}` is parsed by the package's own pipeline, and that pipeline reads a
known field (`message`, L1773). What is missing is a config for an agent
outside the cluster, and somewhere safe for that agent to send data. Five
facts in the code shape the decision:

1. **The format has no place for it.** `manifest.schema.json` is
   `additionalProperties: false` with `schema_version` pinned to `"1.0"`, and
   `assets.agent_config` is required and means "the in-cluster Fluent Bit
   ConfigMap".
2. **Every existing credential is the admin credential.** The in-cluster
   agent authenticates with `admin_creds` (`src/agents.rs` L227-228), and
   `{os_user}`/`{os_password}` in `interpolation.md` resolve to the admin
   login. That doc's security note says the core "does not distinguish" where
   a token is placed. That is acceptable while every rendered asset stays
   inside the cluster. It stops being acceptable once a rendered config is
   pasted onto a host nobody here controls.
3. **The network already has a published path to 9200.** Tenant NetworkPolicy
   flow (4) admits only `velox-agents` to 9200
   (`deploy/tenant-templates/networkpolicy.yaml` L101, asserted at
   `src/k8s.rs` L5992). Flow (3) admits the ingress controller to 5601 *and*
   9200. In ingress mode the full OpenSearch API is already published as
   `<name>-opensearch.<domain>` (`src/access.rs` L74,
   `opensearch_ingress_manifest` at `src/k8s.rs` L1195), with IP allow-list
   annotations and authentication by the security plugin.
4. **The OTel stack's OTLP endpoint is published, but it is the wrong shape
   for this job.** `src/otel_stack.rs` (ADR-053 in its citations) publishes one
   host per signal (L701-760, `ingress_obj` L1683), authenticated by the
   collector's `basicauth/otlp` extension. It has one shared machine user
   (`OTLP_USER = "velox"`) per deployment. The collector forwards to Data
   Prepper, which writes as the **admin** user (`data_prepper_pipelines` L473,
   creds from L1819) into Data Prepper's own index families (`logs-otel-v1*`,
   L126). The credential Secret lives in the shared `velox-agents` namespace
   (L3216). The stack is opt-in and brings Data Prepper, Cortex and
   Alertmanager with it.
5. **No ingest-only principal exists, and the security index gets rewritten.**
   The app creates no OpenSearch roles or internal users today. When it owns
   the securityconfig, `internal_users_yaml` emits exactly two accounts,
   admin and the Dashboards service account (`src/auth_provider.rs` L777).
   The operator's update job runs `securityadmin -f internal_users.yml -t
   internalusers` (`src/k8s.rs` L4650, `src/auth_provider.rs` L453). A user
   created only through the REST API is not in those files. One property
   helps: `basic_internal_auth_domain` is emitted for every auth provider
   (`src/auth_provider.rs` L20), so HTTP basic auth against internal users
   keeps working whichever IdP is configured.

## Decision

### 1. Manifest: an optional `collectors` section, schema 1.1

```jsonc
// additions to manifest.schema.json
"schema_version": { "enum": ["1.0", "1.1"] },
"collectors": {
  "description": "Configs for agents that run OUTSIDE the cluster and ship to the deployment's ingest route. Requires schema_version 1.1.",
  "type": "array", "minItems": 1,
  "items": {
    "type": "object", "additionalProperties": false,
    "required": ["platform", "agent", "config", "install"],
    "properties": {
      "platform":      { "enum": ["linux", "windows", "macos", "container"] },
      "agent":         { "enum": ["fluent-bit", "otel-collector"] },
      "agent_version": { "description": "Lowest agent version the template was tested with; shown in the panel.", "type": "string" },
      "config":        { "$ref": "#/$defs/asset_file" },
      "install":       { "$ref": "#/$defs/i18n" }
    }
  }
}
// and in assets: agent_config is required iff collectors is absent
```

- `(platform, agent)` pairs are unique. JSON Schema cannot express that, so the
  engine checks it after schema validation.
- In 1.1, `assets.agent_config` becomes optional when `collectors` is present.
  A package for a source that never runs in the cluster (say, a Windows event
  log) does not deploy a DaemonSet that would find nothing. `apply_package`
  deploys the in-cluster agent only when the asset exists.
- `install` notes are package data, like `summary`. They are rendered as plain
  text, never as HTML or Markdown.
- Because the schema is `additionalProperties: false`, a 1.0 core refuses any
  manifest carrying `collectors`. That is the job `schema_version` exists to
  do. A package that uses the section declares `"1.1"` and a
  `min_core_version` that understands it. The catalog's existing `compatible`
  flag (`src/catalog.rs` L790) already keeps an older core from offering it.
  1.0 packages stay valid unchanged.

### 2. Ingest path: a published, write-only `_bulk` route per deployment

- In ingress mode every deployment gets a host `<deployment>-ingest.<domain>`.
  It is a standard Ingress **in the deployment's namespace** with exactly one
  path, `/_bulk` (`pathType: Exact`), whose backend is the deployment's 9200
  Service. It uses the same HTTPS-backend annotations, the same Traefik
  `IngressRoute`/`ServersTransport` variant and the same IP allow-list as the
  `-opensearch` route. It is created, re-applied on an access-mode switch, and
  deleted with the deployment's other routes. Integrations do not create it.
- **Authentication and authorization happen in OpenSearch**, with the
  integration's ingest principal (§3). No ingress annotation does that work.
  This is the same reasoning the OTel stack gives for putting basic auth in the
  collector: the check holds whichever controller sits in front.
- **No NetworkPolicy change.** Flow (3) already admits the ingress controller
  to 9200. Nothing new is opened to `velox-agents` or anyone else.
- Collector templates address `/_bulk` at the host root, with `{index}` as the
  target index. Collectors never receive the `-opensearch` host, never an
  in-cluster address, and never the admin credential.
- In port-forward mode there is no route, and the panel reports
  `ingress_required` (a code, ADR-019). No NodePort or LoadBalancer is created
  as a substitute.

### 3. Credentials: one ingest principal per integration per deployment

- **The Secret is the source of truth.** It is named
  `{deployment}-ingest-{recipe_id}`, lives in the deployment's namespace, is
  labelled with the deployment and the integration, and holds
  `username`/`password`. The password comes from the ADR-023 generator
  (`gen_admin_password`, `src/k8s.rs` L388). The plaintext exists there and
  nowhere else: not in a ConfigMap, the CR, a log line or a stored render. It
  is held to the rule `password_only_in_secret` enforces.
- **OpenSearch objects are derived from the Secret.** Each deployment gets an
  internal user and a role, both named `velox-ingest-{recipe_id}`, plus the
  mapping between them. They are written through `_plugins/_security/api` with
  the admin authority the engine already uses. The role grants index
  permissions on `{index}*` for writing documents and creating the index, plus
  the cluster-level bulk action. It grants no read or search action, no
  Dashboards tenant permission, and nothing on any other pattern. The exact
  action list is pinned by the live refusal test in the implementation (§8,
  step 2), not by this text.
- **Re-assertion.** The security index is rewritten from files that do not
  carry these principals (Context 5). An idempotent
  `ensure_ingest_principal` therefore runs:
  - at integration install;
  - after every path that runs securityadmin, which today means auth-provider
    apply/revert and admin password reset;
  - whenever the panel reveals the credential.

  Because the principal lives in the deployment's own security index, it
  cannot authenticate against any other deployment, including another
  deployment of the same tenant.
- **Rotation** is a TenantScoped route. It writes a new password to the
  Secret and then to the user, and the old password stops working
  immediately. There is one credential and no overlap window, so the panel
  says before confirming that every external collector must be updated.
- **Revocation** deletes the mapping, role and user, then the Secret.
  Issuing a new credential goes through the install path's `ensure`.
- **Uninstall** includes revocation (§7).

### 4. Interpolation and signing

- Collector templates are package files, so the canonical digest already
  covers them (`signing.md` §1, step 1: "every file in the package
  directory"). The only change to `signing.md` is listing them.
- **The closed variable set becomes per asset role.** A collector template may
  use `{ingest_host}`, `{ingest_url}`, `{ingest_user}`, `{ingest_password}`,
  `{index}`, `{recipe_id}` and `{deployment}`. In a collector template,
  `{os_host}`, `{os_user}`, `{os_password}`, `{ns}` and `{tenant}` are
  **foreign tokens, and the package is rejected at load** (fail closed, as
  today). The `ingest_*` tokens are foreign in in-cluster assets. This makes
  the engine, not registry review, the guard that keeps admin credentials
  inside the cluster. That matters more if ADR-058 (proposed) admits packages
  from more than one catalog source.
- **Secrets are rendered at request time only.** Signed bytes are the template
  with holes, never a value. The rendered config is produced on request by a
  TenantScoped route, returned with `Cache-Control: no-store`, and never
  persisted, cached or logged. A render is not signed content, and it is never
  stored anywhere signed content lives.
- Agent-native environment references pass through `render` untouched: in
  `${VAR}` the `{` is followed by an uppercase name, and in `${env:VAR}` the
  name is not closed by `}` (`src/integrations.rs` L244). A golden test
  asserts both.

### 5. UI: "Connect a source"

- After installing a package that declares `collectors`, the integration card
  offers a panel with:
  - a platform picker, and an agent picker when a platform has more than one
    agent;
  - the endpoint and username in clear, with the password behind a reveal
    call, as `/otel_stack_credentials` does;
  - the rendered config, with copy and download (a client-side Blob, named
    `{recipe_id}-{platform}.{conf|yaml}`);
  - the install notes;
  - rotate and revoke.
- **"Waiting for first data"** reuses `doc_count_of` (`src/recipes.rs`
  L1802), whose rule is that a non-success is an error, never "0 docs". When
  the panel opens it takes a baseline count over `{index}*`, then flips to
  `receiving` once the count rises past it, polling at the cadence
  `/monitoring_status` already uses. The claim is "documents are arriving in
  this integration's index", not "this collector connected" (§Consequences).
- States are codes (ADR-019): `ingress_required`, `tls_default_certificate`,
  `waiting`, `receiving`, `credential_missing`. The new routes (render,
  rotate, revoke) are TenantScoped entries in the `RoutePolicy` table, so an
  unowned deployment gets a 404 and never a credential.

### 6. Tenant boundary, exposure and TLS

- Everything per-deployment lives in the deployment's namespace: the ingest
  Secret and the ingest route. The OTel stack instead keeps its credential in
  the shared `velox-agents` namespace. Every route mints its `Deployment`
  through `Scope` (`src/scope.rs`), per ADR-044.
- **Exposure.** The new host publishes one path of a backend that ingress mode
  already publishes in full at `-opensearch`, so the added surface is a strict
  subset of the existing one, under the same allow-list. Failed basic-auth
  attempts do reach the security plugin, and this ADR adds no rate limiting
  for them (open question).
- **TLS from collector to ingress is mandatory and verified.** Every rendered
  template uses `https`/`tls On` with verification on. No template may render
  `tls.verify Off` or `insecure_skip_verify`, and the loader rejects templates
  that contain them. When `access.tls_secret` is empty (the controller's
  default certificate), the panel shows `tls_default_certificate` and states
  that the collector will refuse the connection. It does not hand out a
  skip-verify config to make the connection work.
- The hop from ingress to 9200 stays in-cluster, and the operator's internal
  CA stays unverified there, exactly as for the `-opensearch` route today
  (`src/k8s.rs` L1188-1194). This ADR does not change that, and it does not
  add mTLS for collectors.
- The ADR-051 caveat stands unchanged. This design relies on no NetworkPolicy
  flow that does not already exist.

### 7. Clean install ⇒ clean uninstall (ADR-039)

- When `collectors` is present, `created_resources` (`src/integrations.rs`
  L317) and `teardown_resources` gain `IngestUser`, `IngestRole`,
  `IngestRoleMapping` and `IngestSecret`. Both sets are derived from the same
  manifest, so the existing set-equality test covers them.
  `uninstall_package` revokes before `teardown_os`.
- Deleting a deployment sweeps ingest Secrets by label, next to
  `owned_secret_names` (`src/k8s.rs` L1549). Its "every Secret the app creates
  is also deleted" regression test is extended to cover them. The OpenSearch
  objects go with the cluster.
- The ingest route belongs to the deployment's inventory, not the
  integration's. After the last collector-bearing integration is uninstalled
  it authorizes nothing, and it is removed with the deployment.
- Indexed data stays after uninstall, the same contract `recipes::disable`
  documents. External collectors still running get 401.

### 8. Implementation outline

Each step is one PR, in this order:

1. **Format.** Schema 1.1, the manifest parser, per-role closed token sets,
   and the loader rule against skip-verify. Update `interpolation.md` and the
   `signing.md` file list. Golden tests: `{os_password}` in a collector
   template is rejected; `${env:X}` passes. No runtime change.
2. **Ingest principal.** `ensure`/`rotate`/`revoke`, the Secret in the
   deployment namespace, re-assert hooks in the auth-provider and
   admin-reset paths, and the label sweep on deployment delete with its test.
   A live test against a real deployment: `_bulk` to `{index}` is accepted;
   writing to another index is refused; `_search` on `{index}` is refused.
3. **Ingest route.** A pure manifest function plus the Traefik variant, wired
   into create, access-switch and delete, with tests mirroring the
   `-opensearch` route's.
4. **Engine and API.** `apply_package`/`uninstall_package` handle
   `collectors`, `agent_config` becomes optional, and resource accounting is
   extended. Add the render/rotate/revoke routes, their `RoutePolicy` rows
   and `frontend/api.jsx`.
5. **UI.** The "Connect a source" panel, the first-data state, i18n codes and
   browser checks (ADR-024).
6. **Reference integration.** `nginx` 1.1.0 in `veloxsearch-registry` with
   Fluent Bit templates for `linux` (`/var/log/nginx/access.log`) and
   `windows` (`C:\nginx\logs\access.log`). Both rename `log` to `message` so
   the existing pipeline's grok applies unchanged. Verified end to end from a
   fresh Linux VM and a fresh Windows host with no hand edits. Ships with a
   `CHANGELOG.md` entry and migration note (GOVERNANCE: package format
   change).

## Consequences

- An integration can finally bring a source outside the cluster to its
  dashboards. Packages stay data (ADR-039): a template is a file with holes,
  and no new token can express a command or a URL of the package's choosing.
- Enforcement lives in OpenSearch security. "Writes elsewhere are refused and
  it cannot read" can be tested directly against the cluster, not inferred
  from our own config.
- No new component, image or NetworkPolicy flow, so nothing changes for
  air-gapped installs (ADR-025).
- **Cost: the app now authors OpenSearch security objects and must re-assert
  them** after every security-index rewrite. A future path that runs
  securityadmin without calling `ensure_ingest_principal` leaves external
  collectors failing with 401 until the next re-assert. That is a new
  invariant to maintain, and a hook list that can be forgotten.
- Rotation is disruptive by design: one credential, no overlap, and every
  external collector must be edited.
- In ingress mode every deployment has an `-ingest` host, even with no
  collectors. It authorizes nothing without a principal, but it is one more
  name on the public side.
- Packages that use `collectors` need a 1.1-capable core.
- Port-forward installations cannot use external collectors at all.
- The first-data signal is index-level. With an in-cluster agent writing to
  the same index it flips without the external collector's help.
- This path carries documents to `_bulk`, which suits logs and events. OTLP
  traces and metrics still belong to the OTel stack's endpoint, outside the
  package format; nothing here forecloses a later `otlp` ingest kind.
- **Foreclosed:** managing the external agent itself. There is no remote
  config push, no fleet view and no upgrade of collectors. The product hands
  over a tested config and a credential; the agent's lifecycle belongs to its
  host.

## Alternatives considered

- **OTLP through the OTel stack's collector.** Lost on four facts
  (Context 4):
  - the stack is opt-in and heavy;
  - its data lands in Data Prepper's index families with Data Prepper's
    document shape, not in `{index}` behind the package's default pipeline;
  - it writes to OpenSearch as admin, so per-integration restriction would be
    enforced in collector and Data Prepper config rather than by OpenSearch;
  - one shared user per deployment makes per-integration revocation
    impossible.

  Making it fit means per-integration Data Prepper pipelines and sinks with
  per-integration users, plus collector routing by authenticated identity:
  effectively a second apply engine. It remains the right home for
  OTLP-native telemetry.
- **Point collectors at the existing `-opensearch` host with the ingest
  credential.** Lost: collectors would depend on the full admin API host, so
  that host could never be unpublished or allow-listed more tightly without
  breaking them, and a leaked credential could exercise every endpoint the
  security plugin serves.
- **A dedicated ingest gateway** (a proxy in `velox-agents` that checks
  collector auth and forwards with its own credential). Lost for now:
  - a new pinned image and air-gap payload;
  - a shared hop across tenants in `velox-agents`;
  - enforcement moves out of OpenSearch into our code.

  What it would buy (body-size limits, per-credential rate limits) is worth
  revisiting if the published route sees abuse.
- **Per-integration hosts** (`<deployment>-<recipe_id>-ingest`). Lost: a
  30-character deployment plus a 63-character id overflows a 63-character
  DNS label, and a per-integration object adds no authorization over a
  per-integration principal.
- **The admin credential, or one ingest credential per deployment.** Lost:
  the first takes admin out of the cluster. The second can write every
  index, so the refusal property cannot hold, and revoking one integration
  revokes them all.
- **Carrying ingest users in the generated `internal_users.yml`.** Lost: it
  covers only clusters whose securityconfig VeloxSearch owns (ADR-045).
  Clusters on the operator's default would still need the REST path, leaving
  two mechanisms for one object.
- **Rendering `${ENV}` references instead of the password.** Not the
  default, because it breaks "follow the panel with no hand edits". It stays
  open for service-style installs.

## Open questions

- Should the ADR-056 metrics sampler also detect a missing principal (a 401
  as the ingest user) and re-assert it, instead of relying on hooks alone?
- Should the `-ingest` route exist only while a collector-bearing
  integration is installed? That trades a smaller public surface for
  lifecycle state.
- Rate-limiting failed authentication on the published route
  (`auth_failure_listeners` in the security config) — here, or separately?
- Is `otel-collector` admitted in 1.1, or reserved until a package proves its
  `opensearch` exporter's document shape against a package pipeline?
- Should the panel offer an environment-variable form of the config
  alongside the inline one?
