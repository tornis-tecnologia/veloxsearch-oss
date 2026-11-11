# ADR-072 — Velox Mesh is a deployment's, not the installation's

**Status:** proposed
**Date:** 2026-11-10

## Context

ADR-068 shipped Velox Mesh as one controller behind one URL, and the console
gave it one screen: `/mesh`, beside *Deployments* in the sidebar. Every list
on that screen — policies, agents, enrollment tokens, the onboarding step,
each policy's rollout — was the installation's. The deployment a policy
delivered data to appeared only inside the policy document
(`outputs[].deployment`); no table in the Mesh carried it, and no query
filtered on it.

That shape has three faults a second deployment exposes at once:

- **Ownership stops at the save.** `mesh_save_policy` was the only Mesh
  handler that went through `Scope::require`. Minting a token, moving an
  agent and reading the fleet were `AdminOnly` and never asked which
  deployment a policy or an agent belonged to, so an agent could be assigned
  to a policy that writes into another deployment's indices — a cross-tenant
  write path, and one the type-level ownership rule (`scope.rs`) exists to
  make impossible.
- **The Data tab and the Mesh disagree about the same artifacts.** Both paths
  install `veloxagent-<id>-…` pipelines and templates (ADR-071). The Data tab
  could uninstall an integration while a policy's hosts were writing through
  it; nothing told the tab that a policy depended on them.
- **The screen has no deployment to be about.** Onboarding, the published
  OpenSearch endpoint for off-cluster agents (`os_host`), the "receiving
  data" check — each is a fact about one deployment, and a page with no
  deployment in its URL had to carry a target picker on every form instead.

The user's decision (2026-11-10): the Mesh is configured **per deployment**
and shown **under** the deployment, with its policies and agents attached to
it and tied to the integrations that deployment already has.

## Decision

1. **One Mesh service, data partitioned by deployment.** The controller stays
   one binary, one Postgres and one enrollment URL — like the Fleet Server,
   the service is infrastructure and the *data* is what is scoped. Every
   policy and every agent row carries `(deployment_ns, deployment)`, the same
   pair the core's `Deployment` capability is built from. A Mesh per
   deployment (one pod and one database each) was considered and rejected:
   N times the resources and the licence, and no isolation the scope does not
   already give.

2. **Every console call to the Mesh names a deployment and is filtered by
   it.** The core turns the caller's string into a `Deployment` with
   `Scope::require` before the call leaves the process, exactly as the
   OpenSearch and Kubernetes layers already demand; the Mesh then refuses to
   answer about rows outside that pair. `overview` becomes the deployment's
   overview — its onboarding step, its policies, its fleet, its rollouts.
   Only the licence and the seat count stay global: the free tier is ten
   agents per installation, not per deployment.

3. **An agent belongs to the deployment it enrolled under, for life.** The
   agent row copies the pair from the policy at enrollment rather than
   deriving it through `policy_id`, so deleting a policy (`policy_id` becomes
   NULL) does not orphan the agent. `assign_policy` refuses a policy of
   another deployment; moving a host to another deployment is a new
   enrollment, with that deployment's token. A credential issued for one
   deployment's data path must never start writing to another's.

4. **Deleting a deployment forgets its Mesh.** `delete_cluster` tells the
   Mesh to drop the deployment's policies, tokens and credentials and to mark
   its agents `unenrolled`. The agents stop receiving configuration; Fluent
   Bit stays installed on the host with its last config, which is the
   operator's to remove. Blocking the delete while agents exist was
   considered and rejected: a deployment is deleted precisely when its data
   path is gone, and a fleet that can block that is a fleet holding the
   platform hostage.

5. **The published endpoint is the deployment's setting, not the policy's.**
   `os_host` — where a host outside the cluster reaches this deployment's
   OpenSearch — moves from a per-policy field to an annotation on the
   deployment's CR, read at compile time. One deployment has one published
   address; every policy against it compiles against the same one.

6. **Integrations and policies know about each other.** Each policy row the
   Mesh serves names the integrations its current revision activates. The
   Data tab shows, per integration, whether hosts collect it too and under
   which policies, and hands an integration to the policy composer
   ("collect from hosts"); the core refuses to uninstall an integration a
   policy still activates (400, naming the policy). A policy pins its own
   package version in its revision; the in-cluster install keeps its own
   record — the two paths share the artifacts' *names* (ADR-071), not one
   version pointer.

7. **Tenants operate the Mesh of their own deployments.** The routes move
   from `AdminOnly` to `TenantScoped`; `Scope::resolve`'s anti-enumeration
   property (not found ≡ not yours) carries over unchanged because the Mesh
   is only ever asked about a deployment the scope already minted.
   `mesh_status` stays global and tells a tenant only *configured* and
   *reachable* — the Mesh URL is infrastructure.

8. **The console moves the screen under the deployment:**
   `/deployments/:id/mesh/:sub` with `overview | agents | policies`. The
   sidebar entry goes; `/mesh/*` redirects to `/deployments`. The install
   guide (no Mesh configured) and the reachability error move to *Settings*,
   admin-only, because they are about the installation, not a deployment.

## Amendment (2026-11-11)

- The deployment's Mesh settings (§5) are a **settings sub-tab** of the Mesh
  tab, not a card on its overview, and carry a second address beside
  `os_host`: `mesh_url` (annotation `veloxsearch.ai/mesh-agent-url`), where a
  host outside the cluster reaches the Mesh. It fills the `--mesh` of every
  enrolment command the console prints; unset prints the `MESH_URL`
  placeholder. Validated as an http(s) base URL. The controller's own
  installation stays the admin's, under Settings.
- The overview is the fleet at a glance — one count per status word
  (healthy, unhealthy, updating, offline, unenrolled), one stacked bar, the
  configured agents with their policy, read-only — plus the guided first
  run. An agent's policy is changed in the **agent's editor** on the Agents
  tab, which shows what the chosen policy brings (its integrations, inputs
  and values, read off the stored document) before *Deploy*; the row itself
  has no picker.

## Amendment 2 (2026-11-11): named endpoints, bound per policy, required

Supersedes the first amendment's two scalar addresses and §5's "one published
address per deployment".

- **The deployment's settings hold named endpoints**, as one JSON document
  on the CR (`veloxsearch.ai/mesh-settings`): `os_endpoints` (`id`, `name`,
  `host[:port]`) and `mesh_endpoints` (`id`, `name`, http(s) base URL). Ids
  are slugs, unique per list. The two pre-amendment annotations are read as
  one endpoint each under the id `published` and cleared by the next save.
  The in-cluster service name is **offered** on the screen as a one-click
  endpoint and never assumed: a host that cannot resolve it fails silently,
  and the symptom ("healthy agent, no data") is found on the host hours
  later. One deployment is reached by different addresses from different
  networks, so the list, not a scalar.
- **Every policy is bound to one endpoint of each kind**, by id, in its
  document (`endpoints.opensearch`, `endpoints.mesh`; the Mesh's policy rows
  carry them as `os_endpoint`/`mesh_endpoint`). The OpenSearch address is
  what the compiler puts in every collector output; the Mesh address is the
  `--mesh` of the policy's enrolment command.
- **An OpenSearch endpoint is spelled the way the operator has it, and the
  compiler writes it the way Fluent Bit reads it.** `host` (9200 over TLS,
  the package default), `host:port`, or an http(s) URL with an optional
  path — the scheme decides TLS and the default port (80/443), the path is
  the reverse-proxy prefix. The `opensearch` output has no "address" key:
  it takes `Host` (a name or IP, nothing else), `Port`, `tls` and `Path`
  separately, and every package template fixes `Port 9200` / `tls On` for
  the in-cluster service. So `velox_policy::OsAddress` parses the endpoint
  once (the settings save validates with the same function) and `compile`
  **binds** it onto every `opensearch` output of the assembled body,
  replacing those keys and leaving the rest of the stanza the package's
  (`tls.verify`, `Index`, the credential references). The closed token set
  is not widened for this (ADR-039): a `{os_port}` would have to reach
  every signed package and would still leave `tls` and `Path` to be typed
  by hand. An ingress on 443 typed as `host:443` had produced
  `Host host:443` + `Port 9200` — a config Fluent Bit cannot connect with —
  which is what forced the rule. OTel bodies are not rewritten; their
  `{os_host}` is the endpoint's host. **Both are required**:
  `save_policy` refuses a draft that names none or names one the settings no
  longer hold (400), so a policy cannot exist without somewhere to send.
  Rebinding (`mesh_policy_endpoints`) is a new revision. A settings save that
  would remove an endpoint a policy still names is refused with the policy's
  name; changing an address in place is allowed and takes effect on the
  policy's next save. Revisions written before this parse with both unset
  and show as *incomplete*; they cannot gain an integration or an agent until
  bound.
- **Every agent runs a policy.** The enrolment token is the policy's, so an
  agent is never without one; the console's *Add agent* dialog starts with
  the policy, mints its token and completes the enrol command from the
  policy's Mesh endpoint. The agent's editor shows the policy's endpoints
  with its integrations as the dependencies, and cannot deploy an unbound
  policy to a host.
- **The settings sub-tab is also where the Mesh's state lives**: the "Mesh
  ready" banner, the guided first run and a *daemons* table — the controller
  this console uses plus each configured Mesh address, each probed from the
  control plane (`mesh_endpoints_status`: `GET /healthz`, a coarse result,
  the response body never returned — a tenant's settings must not become a
  fetch-anything oracle). The overview is the fleet alone and raises only
  what blocks it (no Mesh, Mesh down, endpoints missing).
- Two console routes are added (`mesh_endpoints_status`,
  `mesh_policy_endpoints`), `mesh_create_policy` takes the two ids, and
  `mesh_settings` answers the lists plus `in_cluster_os_host`.

## Amendment 3 (2026-11-11): the catalogue, the fleet, the add-agent page

The user's review of the console (2026-11-11) found the Data tab "very
confusing" — one integration carried its targets, its policies and its
received-data facts on one card — and the Mesh's *Overview* and *Agents*
sub-tabs redundant. Three decisions:

- **The Data tab is the catalogue.** Every package, the installed ones in
  green (installed on the cluster or on any policy of the deployment, an
  *Installed* filter beside the categories). A card carries no received-
  data facts and no policy pills: it answers "is it on?" and offers add /
  configure on the cluster / remove from the cluster / *see in policies*.
  Per-policy configure and remove stay on the policy's card (amendment to
  ADR-073's "one dialog": the dialog is still the one onboarding; the
  per-target rows moved).
- ~~What the deployment receives is on the deployment's overview~~ — a
  *Data received* panel shipped with this amendment and was withdrawn the
  same day at the user's request; the deployment's overview keeps its
  tiles (documents, ingestion, storage, nodes) and the per-integration
  receiving view is off for now.
- **The Policies sub-tab is a table**: policy, agents, revision,
  integrations, and a menu per row — *Set endpoints* (rebind, a new
  revision), *Settings* (binding, rollout, integrations configured or
  removed in place through the one onboarding dialog), *Add integration*,
  *Add agent* (the add-agent page with the policy chosen; disabled while
  the policy is unbound) and *Remove*. *New policy* is a dialog asking for
  the name and both endpoints. **Removing a policy** is a new console route
  (`mesh_delete_policy` → the Mesh's `delete_policy`): refused, with the
  count, while a live agent runs it — every agent runs a policy — in one
  `DELETE … WHERE NOT EXISTS` so an enrolment cannot slip in between;
  revisions, tokens and the policy's credential cascade; nothing in
  OpenSearch is touched (the `veloxagent-*` artifacts are shared by name,
  ADR-071).
- **The Mesh overview is the fleet** (the Agents sub-tab is merged into it;
  `/mesh/agents` redirects). The table is Fleet-shaped: status, host (OS ·
  arch), policy and applied revision, **CPU, memory**, last report, agent
  and engine version, edit / unenrol. The agent reports its cost on every
  check-in in the OpAMP description's non-identifying attributes
  (`velox.agent.cpu_percent`, `velox.agent.rss_bytes` — the agent process
  plus the engine it supervises, from `/proc`; `velox.engine`,
  `velox.engine.version`, `velox.engine.up`), and the Mesh merges them into
  the agent row's `attributes` at check-in. No protocol extension: the
  slot exists in OpAMP for exactly this.
- **"Add agent" is a page, not a dialog**: `/mesh/add-agent[?policy=<id>]`,
  in three steps — the policy (with its endpoints; an unbound policy does
  not advance), the token (minted, shown once), install and enrol (the
  per-system guide with the enrol command completed, and a watch on the
  fleet that says when a new host reports). An operator does this with a
  terminal open on the host; a dialog is lost on the first mis-click.

## Consequences

- One schema migration (`003_deployment_scope.sql`) adds the pair to
  `mesh_policies` and `mesh_agents`, back-fills the deployment name from
  `body->outputs`, and leaves the namespace for the core to fill on its first
  reconcile. Rows that cannot be resolved are hidden, not deleted. The Mesh
  has not been released, so this touches development databases only.
- The Mesh grows one console route (`forget_deployment`) and every existing
  console route gains a required `deployment` pair. The agent protocol
  (OpAMP + the declared extensions) is untouched: the enrollment token points
  at a policy and the policy at a deployment, which is all the agent needs.
- The core's `api::ROUTES` table changes policy for the Mesh routes; the
  startup check that every route has an entry makes a forgotten one a panic.
- `tests/mesh_check.py` is rewritten for the new URL and asserts the
  redirect from the old one. The `data-testid` hooks of the three panels are
  kept; the deployment tab adds `dep-tab-mesh`.
- Supersedes the global `/mesh` screen described in ADR-068's consequences
  and the `os_host` field of `PolicyDraft` introduced with ADR-070.
