# ADR-071 — Every collection artifact is named `veloxagent-<integration>-<signal>-v<major>`

**Status:** proposed
**Date:** 2026-11-09

## Context

Turning on an integration creates more than an index: an ingest pipeline, an
index template, an index pattern and a set of dashboards (ADR-039, and the
"both halves" promise of the Fleet journey). Until now a policy reused the
names the package declares — index `nginx-logs`, pipeline `nginx`, template
`nginx-logs`, saved objects `velox-nginx-overview` and friends. Those names
are fine while there is one way to collect. There are two:

- the **in-cluster DaemonSet** path (`recipes.rs` / `agents.rs`), which has
  been writing `nginx-logs` for as long as the product has existed, with
  dashboards and retention built on it;
- the **agent** path (Velox Mesh, ADR-068), which collects from hosts outside
  the cluster.

Both can be active on the same deployment, and the packages' saved-object ids
are fixed strings. Installing an integration through a policy under those ids
therefore *overwrites the DaemonSet install's panels* and silently repoints
them at a different index — a failure nobody attributes to the right cause.
The same collision decides the security question next door: a grant for
`nginx-logs*` is a grant over the data the in-cluster path owns, handed to a
credential that lives on someone's VM (ADR-070).

There is a second problem the old names cannot express. An index template is
not editable in place for a mapping change that conflicts with what is already
indexed. With one name per integration, shipping `nginx` 2.0 with a different
mapping means a deployment where half the documents disagree with their
template.

## Decision

**One name per integration, signal and major version, shared by all of its
artifacts on the agent path:**

```
veloxagent-<integration>-<signal>-v<major>
veloxagent-nginx-logs-v1      veloxagent-nginx-metrics-v1
veloxagent-ssh-logs-v1        veloxagent-k8s-events-events-v1
```

1. **The same string names the index, the index template, the ingest pipeline
   and the index pattern.** They live in different namespaces in OpenSearch, so
   one string cannot collide, and "which template covers this index" and "which
   pipeline fed it" become answerable by looking rather than by
   cross-referencing a table. Saved objects hang off it with their own suffix
   (`veloxagent-nginx-logs-v1-overview`); the package's own `velox-nginx-`
   prefix is dropped, because the artifact name already says which integration
   this is.

2. **`signal` comes from a closed list** (`logs`, `metrics`, `traces`,
   `events`, `audit`), read off the last segment of the package's declared
   index and defaulting to `logs`. The alternative — stripping the package id
   — leaves an empty segment in the middle of a name the first time an id and
   its index are the same word (`k8s-events`).

3. **The major version is in the name**, so a mapping change ships as
   `...-v2` and leaves v1 to retention instead of being attempted in place.

4. **The renaming is a transformation of the package, not a change to it.**
   `velox-integrations` builds the id map and rewrites it through parsed JSON:
   every object id, every reference between objects, the index pattern's title,
   the template's `index_patterns` and `index.default_pipeline`, and the index
   reference buried in `kibanaSavedObjectMeta.searchSourceJSON` — which is a
   JSON document stored as a string, and which a rename that stopped at the
   outer object would leave pointing at the old pattern. Matching is by string
   **equality** (plus the `<id>*` form the template uses), never substring: a
   pipeline description reads "VeloxSearch nginx access-log pipeline", and a
   substring rule would rewrite prose.

5. **Both collection paths use these names.** The in-cluster DaemonSet and
   Velox Agent on a host install through one function
   (`integrations::apply_package`, and `apply_package_for_agent` for the
   backend half of a policy), and the egress-less fallback goes through it too:
   a built-in recipe is turned into the package the registry would serve
   (`recipes::bootstrap_package`, gated against the real registry by
   `registry_golden::the_bootstrap_package_is_the_registry_package`). Two
   installers would have been two sets of names. The registry packages keep
   declaring what they declare — the rename happens at install — so nothing in
   the external registry changes and no package is re-signed.

6. **One derivation, three consumers.** The compiler puts the name in the
   collector config's `Index`, carries it on `BackendRequirement.artifact_name`
   for the core to install under, and every reader — retention, the security
   detectors, "is data arriving" — keys on the version-agnostic family
   `veloxagent-<integration>-<signal>-v*`, so a `-v1` index keeps ageing out
   after `-v2` ships and an upgrade never reads as silence. A name computed in
   several places is a name that will differ in one of them.

7. **Existing deployments migrate themselves.** The sampler's credential
   reconcile (`agents::reconcile_creds`) finds any collector whose ConfigMap
   still logs in as something other than `veloxagent-user` — everything
   installed before ADR-070 — and reinstalls that integration through the
   catalog at the version the deployment recorded. One step moves it to the
   collector account *and* to the new names. The old indices and their old
   dashboards stay: they are data, and retention owns them.

8. **The console shows these ids, not the package's.** `integration_artifacts`
   is read by the policy composer, and listing ids a policy never creates would
   send an operator looking for objects that do not exist.

## Consequences

- An operator reading an index list can tell what VeloxSearch collected, and
  `veloxagent-*` describes everything the collector account may touch
  (ADR-070) — one static grant for both paths.
- A deployment collected before this change has two generations of artifacts
  for a while: the old index with its old dashboards (history, no longer
  written), and the `veloxagent-` index with the new ones. Uninstalling an
  integration removes both sets' pipeline, template and saved objects; neither
  path deletes an index.
- The move is not an in-place switch of data: new documents go to the new
  index from the moment the collector is reinstalled. That is deliberate — an
  in-place rename of an index with live writers is a reindex, and the mapping
  and pipeline are unchanged, so nothing is gained by moving history.
- Names get longer. `veloxagent-k8s-events-events-v1` is the ugly case, from an
  integration whose id already ends in its signal; it is still well under
  OpenSearch's 255-byte index-name limit and it is unambiguous.
- An integration that one day declares its signal in the manifest (schema 1.2)
  can replace the closed list with the declaration; the function is pure and
  the tests pin the current behaviour.

Verified on a live deployment by installing `k8s-events` with the previous core
(collector on `HTTP_User admin`, index `k8s-events`) and starting the new one:
within one sampler tick the collector was reinstalled on `veloxagent-user` and
`veloxagent-k8s-events-events-v1`, the pre-ADR-071 account, role and Secret
were deleted, the core's "receiving" check read the new index, and the
collector logged no 401, 403 or output error. A host agent under Velox Mesh
kept writing into `veloxagent-ssh-logs-v1` with the same account throughout.
