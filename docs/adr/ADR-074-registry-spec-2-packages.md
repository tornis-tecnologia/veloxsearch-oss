# ADR-074 — Registry Spec 2.0: configurable packages, per-stream artifacts, a YAML compiler

**Status:** proposed
**Date:** 2026-11-12
**Amends:** ADR-039 (the registry), ADR-071 (artifact names), ADR-073 (inputs)
**Spec:** `docs/spec/README.md` and `schemas/` in veloxsearch-registry (Spec 2.0);
`docs/mesh-integration-guide.md` there is the consumer's guide. Where this ADR
and the spec disagree, the spec wins and this ADR is corrected.

## Context

A 1.x package is a **fixed template**: one `agent.conf.tmpl` per integration,
eight closed `{tokens}` plus scalar `${variables}`, interpolated as text. ADR-073
(schema 1.2) let a package declare *inputs* with variables, but the shape of
the collection — which plugin, which parser, which filters, what the index
template and pipeline say — stayed frozen in the package, and the operator could
only fill a handful of strings. Fleet's model, which operators know, is the
other way round: the integration declares **what can be configured**, the
console generates the form, the policy keeps the choices, and the platform
compiles. The registry team specified exactly that as Spec 2.0 and shipped it on
`feat/packages-spec-2.0`: 35 packages, a catalogue of 84 allow-listed Fluent Bit
plugins with typed parameters, JSON Schemas, a validator (R1–R24) and a
**reference compiler** (`scripts/render.py`) whose output the core must match.

Three facts of the current code shape the decision:

- `catalog::verify_package` already computes the 1.x canonical digest (JCS of
  the manifest without `signature`, `[relpath, sha256]` entries sorted bytewise)
  and Spec 2.0 §12 keeps **that algorithm**, only enumerating files from the
  manifest's `files:` inventory. Trust does not move; what moves is where the
  file list comes from and that each file has a sha256 to check before the
  digest.
- Fluent Bit processors exist only in the **YAML** configuration format, and the
  compiler (`velox-policy`) emits the classic `.conf` today. The agent
  (`velox-agent`) stages, dry-runs and promotes `.conf` files.
- The `veloxagent-<integration>-<signal>-v<major>` name of ADR-071 collides the
  moment one integration has two streams of the same signal (nginx `access` and
  `error` are both `logs`).

## Decision

1. **The Mesh path reads Spec 2.0; the cluster path keeps 1.x for now.** The
   core reads `packages/index.json` and `packages/<id>/<version>/` beside
   `catalog.json` and `integrations/<id>/`. The DaemonSet (`src/integrations.rs`,
   `recipes.rs`) and the golden gates stay on 1.x until the registry's phase F6
   moves them; nothing in this ADR changes a 1.x package. Schema 1.2 (ADR-073
   `inputs`) is **superseded on the Mesh path** by Spec 2.0 activations and is
   removed from it once no policy revision carries one; it never shipped beyond
   this branch.

2. **Trust (Spec 2.0 §3, §12).** Download `manifest.yaml`, verify the ed25519
   signature with the compiled-in keyring over the canonical digest built from
   the manifest and the `files:` inventory, then download exactly the inventory
   and check every sha256. *Unsigned*, *unknown key* and *tampered* stay three
   distinct refusals; an inventory file that is missing or whose hash differs
   is *tampered* for the whole package. A file the inventory does not name is
   never fetched. A verified `(id, version, manifest_sha256)` is immutable and
   may be cached indefinitely; `index.json` is a showcase and its
   `manifest_sha256` is checked against the manifest actually fetched. The
   package is parsed with **strict typed deserialisation** (unknown keys refused)
   plus the R-rules the core can evaluate, not with a JSON Schema engine: the
   schemas are the registry CI's contract, the core's contract is the Rust types
   that the conformance example must parse and the invalid variants must not.
   `velox sign` / `velox verify` accept both formats, detected by `spec_version`.

3. **Names (supersedes ADR-071 for 2.0 packages).** A stream's artifact is
   `N = veloxagent-<stream>.<integration>-v<major>`. The same `N` names the data
   stream, the component template `N@package`, the operator's `N@custom`
   (component template and ingest pipeline, created empty if absent, never
   overwritten), the ingest pipeline `N`, the ISM policy `N`, the index template
   `N` (`composed_of: [N@package, N@custom]`, `data_stream: {}`,
   `index.default_pipeline: N`) and the index pattern `N` (created by the core;
   packages ship none). Saved objects are `veloxagent-<integration>-v<major>-<id>`
   and their `stream:<id>` references are rewritten to `N` as parsed JSON, as
   ADR-071 does today. Readers key on the family `veloxagent-<stream>.<integration>-v*`.
   The `veloxagent-` prefix keeps ADR-070's single grant. 1.x packages keep the
   ADR-071 name.

4. **Backend objects are the deployment's, shared across policies by major.**
   Two policies on the same major share `N`; the core installs the backend of
   the highest active version of that major (a minor may only add mappings,
   R20). Removal is by use count (ADR-073 item 4): when no activation of the
   deployment uses that major, the package's objects go; `N@custom` and the data
   stay unless the operator asks. The backend is written **before** the revision
   that makes agents write to it.

5. **The activation (Spec 2.0 §13) is what a policy revision stores**:
   `{package_id, package_version, spec_version: "2.0", streams: {<id>: {enabled,
   input, parser, processors, backend}}}`. A `password` field is stored as
   `{"$secret": "<id>"}`; the value lives in the Mesh's per-policy secret store
   beside the collector credential (ADR-070), the compiler writes
   `${VELOX_SECRET_<ID>}`, the Mesh hands secrets only to an agent authenticated
   on that policy, and the agent exports them as environment variables to the
   engine without writing them to disk or logs.

6. **The compiler emits Fluent Bit YAML built as an object and serialised**,
   never concatenated text (Spec 2.0 §7): one `service:` from the agent
   settings; the packages' parsers under `parsers:` / `multiline_parsers:`
   named `<integration>.<stream>.<name>`; one input per enabled stream with
   `tag: velox.<integration>.<stream>`, its `fixed` keys with the closed context
   references resolved, its field bindings, `parser:` when chosen, and its
   processor chain attached to the input (filters as processors where the
   catalogue allows, else a global filter matched on the tag) ending in the
   identity metadata `velox.integration`, `velox.stream`, `velox.package_version`;
   outputs generated by the core from the policy's endpoints and routed by the
   stream's `event_type`: `logs`/`traces` → one `opensearch` output per stream
   (`index: N`, `write_operation: create`), `metrics` → one `opentelemetry`
   output to the policy's OTLP endpoint. TLS, verification and credentials come
   from the Mesh's endpoint settings only. Equivalence with `render.py` is a
   test, not a hope: the same package, activation and context must yield the
   same document.

7. **The closed set of context references is** `{stream_tag}`, `{artifact}`,
   `{state_dir}`, `{integration}`, `{stream}`, `{package_version}`; any other
   `{…}` or any `${…}` in a package is a hard refusal (R7), as the eight 1.x
   tokens are today. `{state_dir}` is the one reference the core cannot know
   (it is the agent's, root and unprivileged installs differ), so it compiles
   to `${VELOX_STATE_DIR}` and the agent exports that variable pointing at
   `<state>/fluent-bit/state`. The reference compiler takes the same string as
   its `state_dir` context, so equivalence holds.

8. **Metrics ride the OTel stack (ADR-049)** as Spec 2.0 §9 describes: a third
   endpoint kind, *OTLP*, in the deployment's Mesh settings (same
   `host | host:port | URL` parser as the OpenSearch endpoint, bound onto the
   `opentelemetry` output's `host/port/tls/metrics_uri`), an OTLP credential per
   policy provisioned by the core in the Collector's `basicauth/otlp` and carried
   to agents as `VELOX_OTLP_USER` / `VELOX_OTLP_PASSWORD`, the receiver exposed
   outside the cluster when a policy has external agents, and a **clear refusal**
   — never a silent drop — when a policy activates a `metrics` stream without an
   OTLP endpoint or without the OTel stack. Metric streams create nothing in
   OpenSearch; retention is Cortex's; dashboards query the Prometheus datasource
   filtered by `velox_integration` / `velox_stream`.

9. **Version gates.** A package version is offered when the core satisfies
   `min_mesh_version` (compared with the core's own build version — the Mesh
   service has no release of its own yet) and every agent on the policy
   satisfies `min_agent_version` and `min_fluent_bit_version`; fields and streams
   with `since` above the oldest agent's Fluent Bit are hidden by the console and
   omitted by the compiler. The agent reports `velox.fluent_bit.version` beside
   the existing `velox.engine.version`. Non-GA releases are hidden by default.

10. **The agent applies YAML** (`fluent-bit.yaml` with the same staging /
    dry-run / promote / rollback plan), keeps the `.conf` path for the
    transition, and chooses by the configuration map key it receives.

## Consequences

- The pure/impure split holds: `velox-integrations::spec2` parses and validates
  packages and names artifacts; `velox-policy` compiles; `catalog.rs` verifies
  and `integrations.rs` writes. No module gains a cluster call it did not have.
- `catalog.rs` grows a second transport surface (`packages/`) and the Mesh's
  save path switches to it; the 1.x surface is untouched until F6.
- Existing 1.2 activations on this branch are migrated or recreated; the two
  development agents enrolled against them are retired before the switch.
- Open until measured: `write_operation: create` on the `opensearch` output is
  confirmed on the minimum Fluent Bit by the registry's dry-run lane and by a
  real write in the backend phase; the external exposure of the OTLP receiver
  (Ingress or LoadBalancer, TLS, auth) is designed in the metrics phase.
