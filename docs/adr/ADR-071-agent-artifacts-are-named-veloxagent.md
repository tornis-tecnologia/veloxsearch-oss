# ADR-071 — Every artifact the agent path creates is named `veloxagent-<integration>-<signal>-v<major>`

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

5. **The DaemonSet path is not renamed.** It has data, dashboards and retention
   under the package's names. The prefix marks the agent's artifacts; it does
   not rename anyone else's. The registry packages keep declaring what they
   declare, so nothing in the external registry has to change and no package
   has to be re-signed.

6. **One derivation, three consumers.** The compiler puts the name in the
   collector config's `Index`, carries it on `BackendRequirement.artifact_name`
   for the core to install under, and it is what `os_agent_user::patterns`
   grants. A name computed three times is a name that will differ in one of
   them.

7. **The console shows these ids, not the package's.** `integration_artifacts`
   is read by the policy composer, and listing ids a policy never creates would
   send an operator looking for objects that do not exist.

## Consequences

- An operator reading an index list can tell which side wrote the data, and
  `veloxagent-*` describes everything the collector account may touch
  (ADR-070).
- A DaemonSet install and a policy for the same integration coexist on one
  deployment, with separate indices, separate templates and separate
  dashboards. They are *also* separate data: an operator who had nginx on the
  DaemonSet path and moves to the agent path starts a new index rather than
  appending to the old one. That is deliberate — the mapping and the pipeline
  are the same but their provenance is not — and it means a migration is a
  retention decision, not an in-place switch.
- Names get longer. `veloxagent-k8s-events-events-v1` is the ugly case, from an
  integration whose id already ends in its signal; it is still well under
  OpenSearch's 255-byte index-name limit and it is unambiguous.
- Dashboards shipped by a package are installed twice on a deployment that uses
  both paths. They are small, and the alternative is sharing objects between
  two installs that point at different indices.
- An integration that one day declares its signal in the manifest (schema 1.2)
  can replace the closed list with the declaration; the function is pure and
  the tests pin the current behaviour.
