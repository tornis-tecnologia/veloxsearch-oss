# ADR-073 — One onboarding for an integration: inputs, a target, a policy

**Status:** proposed
**Date:** 2026-11-10

## Context

Two things could be done with an integration, and they were two different
things. *Install* on the Data tab put the backend half (pipeline, template,
dashboards) in OpenSearch and deployed a DaemonSet with the package's fixed
Fluent Bit template — no question asked. The Velox Mesh tab had a second
composer that picked integrations into a policy for hosts. Neither let the
operator say *what* to collect: the templates were written for container
logs (`/var/log/containers/*nginx*.log` plus the Kubernetes metadata
filter), so the same package on a host tailed nothing, and a metrics source
had no way to exist beside a log source in one package. The user's test
found the other half of the problem: installing the backend of an
integration without the collection that feeds it makes no sense.

Elastic's Fleet has one journey for this and operators know it: **Add
integration → configure its inputs (paths, ports, which signals) → pick the
agent policy (or create one) → save**, and the policy's agents pick the
change up. The user asked for that journey, with the integration's
particulars kept in the integration's own package in the registry repository
and coupled to the Mesh's agentic side.

## Decision

1. **A package declares inputs (manifest schema 1.2).** An input is the unit
   an operator turns on — "access logs", "status metrics" — with a `signal`
   from the closed list (which names its index `veloxagent-<id>-<signal>-
   v<major>`, ADR-071), a Fluent Bit template **per target** it can be
   collected on (`cluster`: the DaemonSet; `host`: Velox Agent), the
   variables those templates reference as `${name}` (each marked with the
   targets it applies to), and optionally its own ingest pipeline, index
   template and saved objects. A 1.0/1.1 package is read as one implicit
   `logs` input whose cluster and host templates are both its
   `agent.conf.tmpl`; the twelve shipped packages keep working unchanged.
   Every file an input names is an asset: fetched, digested and signed with
   the rest. The engine supplies the one `[SERVICE]` section a Fluent Bit
   configuration has and strips the templates' own, so two inputs — or two
   integrations in one policy — never hand the engine two.

2. **An activation is the same thing on both targets.** `{ inputs: { <id>:
   { vars } } }` — the inputs turned on and their values — is what a Mesh
   policy's activation carries and what the cluster records on the CR
   (`veloxsearch.ai/integration-inputs`, beside `integration-versions`). An
   absent input is off; an empty selection means the package's
   default-enabled inputs with default values, which is what every install
   from before this ADR means. Rendering is per input and per target: the
   cluster's collector is the enabled inputs' cluster templates, a host's
   configuration is their host templates, each writing to its own index.

3. **One onboarding, on the Data tab.** *Add* opens a dialog: where to
   collect (this cluster, one of the deployment's policies, or a new policy
   named on the spot), then the inputs available on that target with their
   variables, defaults from the signed manifest, required ones enforced —
   then *Add*. `POST /api/add_integration {deployment, id, policy, inputs}`
   installs the backend half for the enabled inputs and either deploys the
   cluster collector or stores the policy's next revision. The same dialog
   edits an activation (target fixed, recorded values filled in). The Mesh
   tab's Policies screen creates policies (a name), mints tokens and shows
   rollouts; its "add integration" sends the operator to the Data tab with
   the policy preselected. There is no second composer.

4. **The backend half belongs to whoever still uses it.** Removing an
   integration from one target (`POST /api/remove_integration`) takes it off
   that target; the pipeline, templates and dashboards are torn down only
   when neither the cluster nor any policy of the deployment activates it
   any more. ADR-072's refusal to uninstall while a policy used the
   integration is withdrawn: the operator removes per target, and the last
   removal cleans up.

5. **The cluster is a policy too, implicitly.** It appears as a target in the
   dialog and as a badge on the Data tab exactly like a policy does, and
   `catalog_install` / `catalog_uninstall` stay as the wizard's aliases for
   adding to and removing from it.

## Consequences

- `crates/velox-integrations` gains `InputDecl`, `Input`, `InputSelection`,
  `render_inputs`, `assemble_fluent_bit` and per-input renaming and
  accounting; `velox-policy`'s `Activation` gains `inputs`; the core's
  `apply_package` / `apply_package_for_agent` / `collector_config` take a
  selection. `docs/integrations/manifest.schema.json` is at 1.2 and the
  registry's `validate.py` checks the input rules.
- The registry packages `nginx` (access logs on cluster and host with an
  `access_log` path, status metrics on a host via `stub_status`, off by
  default) and `ssh` (`auth_log` path on a host) move to schema 1.2 as the
  worked examples, at 1.1.0. They carry the staging placeholder until the
  maintainer signs them with `velox sign`; a core refuses them as UNSIGNED
  until then, which is the design working. The other ten stay at 1.0.
- The golden gates compare a registry package's **first input** to the
  in-binary recipe (cluster template, pipeline, index template) and require
  the recipe's resources to be a subset of the package's, so a 1.2 package
  may add inputs and never lose the recipe.
- `tests/mesh_check.py` drives the dialog on both targets. The Mesh tab's
  composer test ids (`mesh-integration-*`, `mesh-policy-save`) are gone;
  `mesh-policy-create`, `policy-*`, `add-integration*` and `add-target-*`
  replace them.
- Not done here: saved objects per input are installed but the Data tab's
  "receiving" check still reads the first input's index family; a metrics
  input's data shows in Dashboards, not on the tab's row.
