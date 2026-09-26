# Cluster profile — schema version 1

`GET /api/cluster_profile[?names=true]` returns one JSON document describing
the size, shape, load and health of the deployments you can see. It is a
read-only capacity export for operators. The document leaves the cluster only
as that response: there is no scheduled push, no upload endpoint and no
setting that enables one ([ADR-060](adr/ADR-060-cluster-profile-export.md)).

The authoritative field list is `profile::FIELDS` in `src/profile.rs`. A test
fails if the code emits a field that is not listed there, or if a listed field
is never emitted, so this page and that table stay in sync.

## Contract

- **Versioning.** `schema_version` is an integer. Additive fields keep the
  version, and consumers must ignore unknown keys. A removal, a rename or a
  unit change bumps it.
- **Units.** Bytes, millicores and per-second rates, except GC time, which is
  milliseconds of collection per minute. Percentages run from 0 to 100.
  Timestamps are RFC 3339 UTC with second precision.
- **Stable order.** Keys come in declaration order, and arrays are sorted by
  `id` (stalls by time, warnings alphabetically). Two profiles of the same
  installation can be diffed field by field. `generated_at` and the live
  values are the only expected churn.
- **Unknown is `null`.** A source that could not be read (a deployment still
  starting, an OpenSearch that did not answer, a sampler window not yet
  recorded) yields `null`, or `samples: 0`. Nothing is estimated. A key that
  does not apply to the scope, such as the installation sections in a tenant
  document, is absent rather than `null`.

## Scopes

| `scope` | Who | Carries |
| --- | --- | --- |
| `installation` | the installation admin | `kubernetes`, `nodes`, `storage`, `fit`, and every deployment |
| `tenant` | a tenant session | `quota` (its headroom) and its own deployments only |

## Identities and the `names` switch

Node, deployment and tenant identities are **keyed pseudonyms**:
`HMAC-SHA256(session_secret, "velox-profile:v1:" + identity)`, truncated to
12 hex characters and prefixed `n-`, `d-` or `t-`. They are stable across
profiles of the same installation and cannot be reversed without the key.
Rotating the session secret changes every id, so profiles taken on either
side of a rotation no longer line up.

With `?names=true`, the real deployment name (`name`) and index family names
(`indices.family_names`) are added. The installation admin also gets the
tenant slug (`tenant_slug`). The pseudonymous `id` and `tenant` stay, so
profiles taken in both modes still line up. `names_included` records which
mode produced the file. Host node names are never included, because they are
hostnames.

## Never included, in any mode

Document contents, mappings and field names; credentials of any kind;
hostnames, IP addresses, URLs, service addresses and ingress domains; free
text written by users or by the cluster (extra configuration, IP allow-lists,
auth-provider settings, Kubernetes Event messages, provisioning errors); raw
index names (they are reduced to counts by class unless `names=true`); and
the name of a remediated pod (reduced to the kind of remediation).

## Fields

| Path | Meaning |
| --- | --- |
| `schema_version` | `1` |
| `generated_at` | when the document was built |
| `scope` | `installation` \| `tenant` |
| `names_included` | whether `?names=true` produced it |
| `build.version`, `build.commit` | the running VeloxSearch build |
| `build.image_digest` | `sha256:` digest of the running image (admin; else `null`) |
| `build.operator_version` | the OpenSearch operator image tag (admin; else `null`) |
| `kubernetes.version` | API server version string |
| `kubernetes.distribution` | `k3s` \| `rke2` \| `eks` \| `gke` \| `unknown` |
| `nodes[].id` | host node pseudonym |
| `nodes[].roles[]` | `control-plane` \| `etcd` \| `worker` \| `other` |
| `nodes[].ready`, `nodes[].pressures[]` | node conditions (`MemoryPressure`, `DiskPressure`, `PIDPressure`) |
| `nodes[].kernel` | kernel release |
| `nodes[].cpu_millis.{allocatable,requested,used}` | millicores; `used` is `null` without metrics-server |
| `nodes[].memory_bytes.{allocatable,requested,used}` | bytes |
| `nodes[].host_disk_bytes.{total,used}` | kubelet root filesystem |
| `nodes[].storage_bytes.{total,used}` | Longhorn disks on the node |
| `storage.class` | `longhorn` \| `foreign_default` \| `node_local` \| `absent` \| `unknown` |
| `storage.longhorn_replicas` | copies per volume on the `longhorn` StorageClass |
| `storage.pool_bytes.{total,used,available}` | the Longhorn pool |
| `fit[].{size,count,limited_by}` | how many more deployments of each preset fit, and what runs out first (`cpu` \| `mem` \| `disk`) |
| `quota.{max_deployments,max_total_disk_bytes,max_nodes}` | the tenant's quota (tenant documents only) |
| `deployments[].id`, `.tenant` | pseudonyms; `tenant` is `null` for an admin-namespace deployment |
| `deployments[].name`, `.tenant_slug` | only with `names=true` (see above) |
| `deployments[].size` | `small` \| `medium` \| `large` \| `custom` |
| `deployments[].sizing` | `preset` when memory, disk and node count equal the size's preset, else `custom` |
| `deployments[].purpose` | `observability` \| `security` \| `search` \| `other` |
| `deployments[].opensearch_version`, `.target_version` | running version; target while an upgrade is in flight |
| `deployments[].nodes` | requested node count |
| `deployments[].memory_limit_bytes`, `.heap_declared_bytes`, `.disk_per_node_bytes` | declared per node |
| `deployments[].heap_max_bytes_observed` | largest `heap_max` OpenSearch reports |
| `deployments[].health`, `.settled` | `green` \| `yellow` \| `red` \| `unknown`; the ADR-050 predicate |
| `deployments[].indices.{total,families}` | index count; distinct names after stripping rollover and date suffixes |
| `deployments[].indices.by_class.{system,managed,user}` | dot-prefixed; written by VeloxSearch itself; the rest |
| `deployments[].shards.{primaries,total,unassigned,largest_primary_bytes}` | shard counts and the largest primary |
| `deployments[].store_bytes`, `.docs` | store size and doc count summed across nodes (replicas included) |
| `deployments[].disk_watermark_headroom_bytes.{low,high,flood_stage}` | free bytes above each disk watermark on the tightest node; negative means the watermark is already crossed |
| `deployments[].restarts.{node_containers,dashboards}` | container restarts since each pod was created |
| `deployments[].now.{cpu_percent,heap_percent}` | mean across nodes, from one live `_nodes/stats` call |
| `deployments[].now.{indexing_per_sec,query_per_sec,gc_old_millis_per_min,gc_young_millis_per_min}` | that call's counters against the newest recorded sample; `null` when there is no sample from the last 15 minutes |
| `deployments[].window.{start,end,samples,bucket_secs}` | coverage of the recorded history: at most 7 days, in 300-second buckets |
| `deployments[].window.{cpu_percent,heap_percent,indexing_per_sec,query_per_sec,gc_old_millis_per_min,gc_young_millis_per_min}.{p50,p95}` | percentiles over the buckets; `null` when no bucket carries the field |
| `deployments[].window.disk_used_bytes.{first,last}` | data-path disk used at the start and end of the window |
| `deployments[].stalls[].{at,stage,component,component_status,recovery_stage,recovery_index_class,secs,remediation}` | stall episodes: the current one plus any recorded in the window |
| `deployments[].integrations[].{id,version}` | installed integrations |
| `deployments[].warnings[]` | `stalled`, `provisioning_failed`, `metrics_unavailable`; for the admin also `storage_single_copy`, `storage_node_local`, `node_pressure`, `kernel_incompatible` |

## Coverage caveats

The window only covers what the metrics sampler recorded. That history lives
in each deployment's own `velox-metrics-<deployment>` index with no replica
and is kept for about 7 days. A new deployment, one the sampler could not
reach, or one that lost that index will show a short or empty window. Query
and GC rates cover only samples recorded by a version that collects them.
Older samples lack those counters, so the percentiles are computed over the
samples that do carry them, and are `null` until there are some.
