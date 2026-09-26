# ADR-060 — A bounded cluster profile, exported only by download

**Status:** proposed
**Date:** 2026-09-14

## Context

VeloxSearch already measures most of what it takes to say whether an
installation is too big, too small, or the wrong shape (disk-bound,
memory-starved, mostly idle). Each measurement answers one question for one
screen, and none of them can be taken away:

- `capacity::cluster_capacity()` — host nodes: allocatable, requested and
  live CPU/memory, kubelet root-fs, Longhorn pool, pressures, kernel, and the
  "how many more fit" estimate. `AdminOnly`: its route note says a tenant's
  headroom is its quota (#84), not the host.
- `metrics::node_stats()` — per OpenSearch node CPU, heap used/max, data-path
  disk, docs, store size. Point in time.
- `metrics::run_sampler()` — every `VELOX_METRICS_INTERVAL_SECS` (default
  60s, floor 5s) folds `_nodes/stats` into one `Sample` per deployment
  (`cpu_percent` and `heap_percent` meaned across nodes; `disk_used_bytes`,
  `docs`, `index_total` summed) and writes it to `velox-metrics-<deployment>`
  **inside that deployment's own OpenSearch**, zero replicas. The ISM policy
  `velox-metrics-retention` rolls the write index at 1d or 256mb and deletes a
  backing index at 7d age, so the retained window is ~7–8 days at best — less
  for a young deployment, one the sampler could not reach, or one that lost a
  node (no replica). `metrics::series()` clamps reads to 7 days.
- `k8s::list_deployments()` → `Status`: size label, OpenSearch version and
  upgrade target (ADR-048), declared memory limit and heap (ADR-035), disk,
  `activity` (ADR-050) and `provisioning` (ADR-052).
- `bootstrap`: `apiserver_version()` for R1, `classify_storage()` (ADR-043),
  and the Longhorn replica count `reconcile_longhorn_sizing()` writes (#26).
- `k8s::integration_versions()` — installed integration id → version.

Four facts make this a decision rather than a new screen:

1. **The shapes leak by design.** `Status` carries `dashboard_url`,
   `opensearch_url`, a port-forward command, `extra_config` and the IP
   allow-list; `NodeCapacity` carries node names; `Blocked` carries a raw
   `recovery_index`; `TelemetrySource.address` is a service DNS name. Right for
   the UI, wrong for a file that is meant to be handed to someone outside the
   installation.
2. **Some wanted facts are not collected anywhere.** Query rate, GC pressure,
   shard counts, disk-watermark headroom and node-pod restarts are not read.
   For two of them the data is already on the wire: the
   `_nodes/stats/os,jvm,fs,indices` call returns `indices.search` and
   `jvm.gc`, and both are ignored. "Recent stall diagnoses" do not exist either: ADR-050 recomputes
   `activity` from the cluster on every read and remembers nothing, so only a
   stall happening *now* is visible.
3. **Two warnings live only in the browser.** The single-copy-storage (#26)
   and kernel-incompatibility (#32) rules are computed in
   `frontend/views_create.jsx` from `cluster_capacity`; the backend has no
   warning list to export.
4. **The existing read path cannot serve a full window.** `series()` pulls
   raw hits sorted ascending with `size: 10_000`. A full 7-day window at the
   default cadence is 10,080 samples, so the *newest* ~80 minutes are cut; at
   the 5s floor the cap bites at ~14 hours.

## Decision

1. **One endpoint, one document.** `GET /api/cluster_profile` returns a
   `ClusterProfile` with an integer `schema_version` (starting at `1`).
   Additive fields keep the version and consumers ignore unknown keys; a
   removal, rename or unit change bumps it. Fixed units (bytes, millicores,
   per-second rates), codes instead of prose (ADR-019), keys in declaration
   order, arrays sorted by id — so two profiles of the same installation diff
   field by field, and `generated_at` plus the live values are the only
   expected churn.

2. **Derived facts only, through a dedicated projection.** A new
   `src/profile.rs` owns its own DTOs and a pure
   `build(ProfileInputs, &Redaction) -> ClusterProfile`. It never serializes
   `Status`, `ClusterCapacity` or `NodeStat` directly: a field added to a UI
   DTO must not silently appear in an export. The gathering half (async, the
   existing functions above) and the deciding half (pure, tested) split the
   way `activity::evaluate` and `Scope::adopt` already do.

3. **Excluded data classes — never in the document, not even on opt-in:**
   document contents, mappings and field names (data about contents);
   credentials of any kind (`admin_creds` is used to query and never reaches
   the builder's output); hostnames, IPs, URLs, service addresses and ingress
   domains; free text written by users or the cluster (`extra_config`, IP
   allow-lists, auth-provider config, Kubernetes Event messages,
   `provisioning.last_error`). The remediated pod name in `Blocked` reduces to
   a boolean.

   **Reduced unless the caller opts in** (`?names=true`): node, deployment and
   tenant identities become keyed pseudonyms — `HMAC(session_secret,
   "velox-profile:v1:" + name)`, truncated — stable across profiles of the
   same installation (so diffs line up) and not reversible without the key.
   Index names reduce to counts by class (`system` = dot-prefixed, `managed` =
   `velox-metrics-*` and monitor indices, `user`) and a count of index
   *families* (rollover/date suffix stripped); `recovery_index` reduces to its
   class. With opt-in, real deployment and index-family names are added;
   tenant slugs only for the installation admin. The document states which
   mode produced it (`names_included`).

4. **One allowlist, enforced in CI.** `profile::FIELDS` is a single
   `const` table of JSON pointer paths (`/deployments/[]/shards/total`), each
   with a `note` saying why it is safe — the `api::ROUTES` pattern. Two tests
   run under `cargo test`:
   - *Exact set:* build a maximal profile from a fixture (every `Option` set,
     every `Vec` non-empty), walk the serialized JSON, and require the leaf
     paths to equal `FIELDS` both ways — an unlisted field fails, and so does a
     listed field nothing emits.
   - *Canary,* in the shape of `otel_stack::password_only_in_secret`: seed
     every excluded source with sentinels (a password, `sentinel-node.example`,
     `203.0.113.7`, `sentinel-index-7`, a tenant slug, a document body, a
     dashboard URL) and assert none appear in the output with `names=false`,
     and that with `names=true` only the name sentinels do.

   Adding a field is therefore a one-line, reviewable diff to `FIELDS`.

5. **The tenant boundary (ADR-044).** The route is declared `TenantScoped`
   in `api::ROUTES`, not `AdminOnly` — `AdminOnly` answers a tenant 404, and a
   tenant must be able to profile its own deployments. The handler takes no
   deployment name; its only list is `k8s::scoped_deployments(&scope)`, so
   there is nothing to resolve and nothing foreign to reach. The installation
   sections (`kubernetes`, `nodes`, `storage`, `fit`) are built only for
   `Scope::Admin`; a tenant document omits those keys (absent, not `null`)
   and carries its ADR-041 quota row (`max_deployments`,
   `max_total_disk_gb`, `max_nodes`, read the way `tenants.rs` already reads
   it) as its headroom. `scope` in the document says which one it is.
   Left open: whether a tenant session should also need the `owner` role.
   No route checks `tenant_users.role` today, so requiring it would add a new
   authorization mechanism, and that belongs in its own decision.

6. **Workload, not an instant.** Every rate and utilisation appears twice:
   `now` (from the same `_nodes/stats` call `node_stats` makes) and `window`
   (p50/p95 over what `velox-metrics-<deployment>` actually retains). The
   window is read with a `date_histogram` aggregation at a fixed 5-minute
   bucket (≤ 2,016 buckets over 7 days) — never raw hits, so the 10,000-hit
   cap does not apply — and folded in Rust through the existing
   `downsample` logic, which already clamps `index_total` across a counter
   reset. The document states the coverage it really had: `start`, `end`,
   `samples`, `bucket_secs`. A percentile over a field older samples lack
   (see 7) is computed over the samples that carry it, and is `null` when none
   do. Nothing is extrapolated beyond retention.

7. **New collection, additive and bounded.** The sampler's `Sample` and index
   template gain `query_total` (`indices.search.query_total`),
   `gc_old_millis` and `gc_young_millis` (`jvm.gc.collectors.*.
   collection_time_in_millis`) — both already in the response it fetches. Templates apply at the next
   rollover (≤ 1 day), so old indices age out naturally. When a deployment's
   verdict is `stalled`, the sampler also writes one `stall` document per
   distinct `(stage, component)` episode — stage, component and status,
   recovery stage and age, recovery index *class*, remediation kind — into the
   same alias, inheriting its 7-day retention. That is the only history of
   stalls, and it is bounded by the same policy as everything else.

8. **No automatic upload, no phone-home.** The profile leaves the cluster
   only when a signed-in user downloads it. There is no export endpoint to
   configure, no scheduled push, no env var that enables one, and the
   `profile` module makes no outbound call other than to the Kubernetes API
   and the deployment's own OpenSearch. Adding any other path — push, beacon,
   or "send to support" — needs its own ADR.

9. **The UI.** The Capacity view gains **Download cluster profile**. It
   fetches the profile once, shows the exact JSON that will be saved
   (pretty-printed, with a names on/off toggle that refetches), and saves
   *those bytes* from a client-side `Blob` — no second request, so the preview
   cannot differ from the file. The button, the toggle and the preview
   heading are i18n keys; `tests/*_check.py` cover the dialog.

### Draft schema (version 1)

Where each field comes from is listed after the example; *new* marks facts
not collected today.

```json
{
  "schema_version": 1,
  "generated_at": "2026-09-14T12:00:00Z",
  "scope": "installation",
  "names_included": false,
  "build": { "version": "0.9.0", "commit": "9897468" },
  "kubernetes": { "version": "v1.36.3+k3s1", "distribution": "k3s" },
  "nodes": [
    {
      "id": "n-3f9a1c", "roles": ["control-plane"], "ready": true,
      "pressures": [], "kernel": "6.1.0-40-amd64",
      "cpu_millis":   { "allocatable": 4000, "requested": 2600, "used": 1210 },
      "memory_bytes": { "allocatable": 16654532608, "requested": 11811160064, "used": 9102190592 },
      "host_disk_bytes": { "total": 107374182400, "used": 38654705664 },
      "storage_bytes":   { "total": 85899345920, "used": 32212254720 }
    }
  ],
  "storage": {
    "class": "longhorn", "longhorn_replicas": 3,
    "pool_bytes": { "total": 257698037760, "used": 96636764160, "available": 161061273600 }
  },
  "fit": [ { "size": "small", "count": 2, "limited_by": "mem" } ],
  "deployments": [
    {
      "id": "d-81be07", "tenant": "t-5c20e4",
      "size": "medium", "sizing": "preset", "purpose": "logs",
      "opensearch_version": "3.8.0", "target_version": null,
      "nodes": 3, "memory_limit_bytes": 3221225472,
      "heap_declared_bytes": 1610612736, "heap_max_bytes_observed": 1610612736,
      "disk_per_node_bytes": 10737418240,
      "health": "green", "settled": true,
      "indices": { "total": 41, "families": 6, "by_class": { "system": 12, "managed": 9, "user": 20 } },
      "shards": { "primaries": 44, "total": 88, "unassigned": 0, "largest_primary_bytes": 2147483648 },
      "store_bytes": 19338473472, "docs": 12004311,
      "disk_watermark_headroom_bytes": { "low": 4294967296, "high": 3221225472, "flood_stage": 2147483648 },
      "restarts": { "node_containers": 2, "dashboards": 0 },
      "now": {
        "cpu_percent": 23.0, "heap_percent": 61.0,
        "indexing_per_sec": 410.2, "query_per_sec": 12.5, "gc_old_millis_per_min": 40.0
      },
      "window": {
        "start": "2026-09-07T12:05:00Z", "end": "2026-09-14T12:00:00Z",
        "samples": 9870, "bucket_secs": 300,
        "cpu_percent":           { "p50": 18.0,  "p95": 71.0 },
        "heap_percent":          { "p50": 55.0,  "p95": 78.0 },
        "indexing_per_sec":      { "p50": 380.0, "p95": 1210.0 },
        "query_per_sec":         { "p50": 9.1,   "p95": 44.0 },
        "gc_old_millis_per_min": { "p50": 12.0,  "p95": 310.0 },
        "disk_used_bytes":       { "first": 17179869184, "last": 19338473472 }
      },
      "stalls": [
        {
          "at": "2026-09-12T03:14:00Z", "stage": "nodes",
          "component": "RollingRestart", "component_status": "Running",
          "recovery_stage": "init", "recovery_index_class": "system",
          "secs": 57240, "remediation": "node_bounce"
        }
      ],
      "integrations": [ { "id": "nginx", "version": "1.2.0" } ],
      "warnings": ["storage_single_copy"]
    }
  ]
}
```

| Field | Source |
| --- | --- |
| `build` | the `GET /api/build_info` payload (#55), embedded verbatim; its field set is whatever #55 settles |
| `kubernetes.version` | `client.apiserver_version().git_version`, as the R1 probe in `bootstrap.rs` reads it |
| `kubernetes.distribution` | *new*: a pure parse of the `git_version` suffix (`+k3s`, `+rke2`, `-eks-`, `-gke.`) into a closed set, else `unknown` |
| `nodes[]` | `capacity::cluster_capacity()` → `NodeCapacity`, with `name` replaced by the pseudonym |
| `storage.class`, `pool_bytes` | `bootstrap::classify_storage()`; `ClusterCapacity.storage` |
| `storage.longhorn_replicas` | `numberOfReplicas` on the `longhorn` StorageClass — the read `reconcile_longhorn_sizing()` already performs while waiting for the rebuild, extracted into a function |
| `fit` | `ClusterCapacity.fit` |
| `size`, `purpose`, `opensearch_version`, `target_version`, `nodes`, `memory_limit_bytes`, `heap_declared_bytes`, `disk_per_node_bytes`, `health`, `settled` | `Status` from `k8s::list_deployments()` (quantity strings converted to bytes) |
| `sizing` | *new, pure*: `preset` when the CR's memory/disk/replicas equal `sizing(size)`, else `custom` — the size label alone does not record overrides |
| `heap_max_bytes_observed`, `store_bytes`, `docs`, `now.cpu_percent`, `now.heap_percent` | `metrics::node_stats()` |
| `indices`, `shards` | *new*: `_cluster/health` (today called only for a stalled deployment) and `_cat/indices?format=json&bytes=b`, reduced to counts before leaving `profile.rs` |
| `disk_watermark_headroom_bytes` | *new*: `_cluster/settings?include_defaults=true` watermarks (percentage or absolute) against each node's `fs.data`; the minimum across nodes |
| `restarts` | Dashboards: the container scan `k8s.rs` does for #46; node containers: *new*, the same scan over the node pods. Cumulative since each pod's creation — a roll resets it |
| `now.indexing_per_sec` | the `index_total` delta between the two newest samples |
| `now.query_per_sec`, `now.gc_old_millis_per_min` | *new*: sample fields from Decision 7 |
| `window.*` | the `velox-metrics-<deployment>` alias (Decision 6); `query_per_sec` and `gc_*` are *new* |
| `stalls[]` | *new*: the stall documents from Decision 7. The current stall alone is `Status.activity.blocked` |
| `integrations[]` | `k8s::integration_versions()` |
| `warnings[]` | *new, moved*: the #26 and #32 rules leave `views_create.jsx` for a pure backend function returning codes, which the wizard then consumes; plus `stalled`, `provisioning_failed` (ADR-052), `node_pressure`, `metrics_unavailable`, and any R-check at `warn` |

## Consequences

- An operator can answer "wrong size or wrong shape?" from one file, and hand
  that file to a support engineer or a capacity-analysis tool without first
  reading it for secrets — the allowlist and canary tests did that.
- The allowlist is a standing cost: every useful new fact is a reviewed
  diff, and a change to a source DTO can break the exact-set test. That
  friction is the point.
- Pseudonyms are keyed on `session_secret`. Rotating it changes every id, so
  profiles across a rotation no longer line up; accepted, and stated in the
  schema doc.
- The window is only as good as the sampler: a deployment the sampler could
  not reach, a fresh one, or one that lost the unreplicated metrics index has
  a short or empty window, and the document says so through `samples` and
  `start` instead of hiding it.
- Stall history becomes remembered state for the first time. It does not
  feed any verdict — ADR-050's invariant (the verdict is recomputed from the
  cluster) is untouched; the documents are a record, not an input.
- Building a profile fans out to every deployment's OpenSearch (a handful
  of read calls each) on demand. It is admin-initiated and rare, but on a large
  installation it takes seconds; the UI shows progress, and the handler bounds
  concurrency.
- The `series()` hit cap stays a separate defect of the Overview chart; this
  ADR avoids it rather than fixing it.
- Foreclosed: anything in the profile that is not a derived fact about size,
  shape, load or health. A request to include log samples or query text is
  answered by this ADR.

## Alternatives considered

- **Push the profile to a remote endpoint (scheduled or on demand).** Rejected
  for now. It makes VeloxSearch an egress source on clusters whose platform
  contract only promises registry egress (R6) and that may be air-gapped; it
  turns a file an admin chose to share into a stream nobody reviews each
  time; and it creates a credential and endpoint to configure, store and
  rotate. A download keeps a human deciding every time, and anything that
  needs automation can call the endpoint itself with its own session.
- **Reuse the existing DTOs** (`Status` + `ClusterCapacity` + `MetricSeries`
  in one envelope). Lost: they carry URLs, node names, allow-lists and free
  text today, and future UI fields would flow into the export unreviewed.
- **Redact by denylist** (strip known-sensitive keys from the combined
  output). Lost: a denylist fails open — a new sensitive field ships until
  someone notices. An allowlist fails closed.
- **`AdminOnly`, tenant slice later.** Lost: tenants already see their own
  deployments' `node_stats` and `metrics_series`; the profile adds counts and
  percentiles over the same data, and deferring the tenant path would push a
  second route shape rather than one scoped route.
- **Raw time series instead of percentiles.** Lost: a week of 60s samples is
  ~10k points per metric per deployment, a lossy diff between profiles, and
  more shape than a sizing question needs. `metrics_series` remains the path
  for the series itself.
- **Prometheus or a new store for history.** Lost for the same reasons the
  sampler chose its own index (#9): one more component to install and own,
  when the retention needed already exists inside each deployment.
- **Unkeyed hashes for pseudonyms.** Lost: hostnames and deployment names are
  low-entropy, so a plain hash is reversible by guessing.

## Implementation outline

Each step is one PR, merged in order; none is user-visible before step 5.

1. **`profile.rs` skeleton:** DTOs, pure `build()`, `FIELDS`, the exact-set
   and canary tests, and the schema doc under `docs/` — from fixtures, no
   route.
2. **Sources that exist today:** gathering from `cluster_capacity`,
   `list_deployments`, `node_stats`, `integration_versions`, the storage
   class and replica read, and `build_info` (#55); the window aggregation over
   today's sample fields; `GET /api/cluster_profile` with its `ROUTES` entry
   and the scope tests for both identities.
3. **Warnings server-side:** the #26/#32 rules move out of
   `views_create.jsx` into a pure backend function; the wizard reads the
   codes.
4. **Sampler additions:** `query_total`, GC collector times, stall
   documents, and the new index-template fields; `_cluster/health`,
   `_cat/indices`, watermark settings, node-pod restarts in the gatherer.
5. **UI:** Download cluster profile with preview and names toggle on the
   Capacity view, i18n keys, browser checks; and a leakage review of a real
   profile from a demo cluster, recorded on the PR.
