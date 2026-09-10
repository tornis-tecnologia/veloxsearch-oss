# ADR-055 — VeloxSearch may surgically patch the operator's Dashboards Deployment

**Status:** accepted
**Date:** 2026-09-10

## Context

The opensearch-operator (chart 3.0.2) hardcodes the Dashboards Deployment's
startup probe at a ~210s budget, and OpenSearch Dashboards does not bind
`:5601` until its saved-objects migrations complete. A first boot that starts
while the cluster is still settling — the normal case during a VeloxSearch
create — can therefore be killed mid-migration, leaving `.kibana_1`
half-migrated. Every later boot hits `resource_already_exists_exception`,
interprets it as "another instance is migrating", waits forever, and is
killed again. Observed live (issue #46): 235 restarts over 17 hours on a
cluster that was green in every other respect, recovered only by hand.

Everything until now treated operator-owned objects as read-only: the
runtime grant holds `get/list/watch` on `apps/deployments`, and the one
write to a foreign lifecycle (the #27 node bounce) deletes a *pod*, betting
on the operator recreating it.

## Decision

1. **Read-only is dropped for exactly one object type and three fields.**
   The runtime ClusterRole gains `patch` on `apps/deployments` — nothing
   else — used for:
   - a server-side apply under field manager `veloxsearch-dashboards-
     survivability` claiming `spec.strategy.type` and the Dashboards
     container's `spec.template.spec.containers[].startupProbe` (the
     survivability patch applied at create), and
   - scalar merge patches of `spec.replicas` 0/1 during the one-shot
     `.kibana_1` remediation.
2. **Container logs stay unread.** The remediation trigger is the restart
   count plus the verbatim waiting reason (`CrashLoopBackOff`) — both
   already granted. `pods/log` would widen the tenant boundary the runtime
   grant exists to respect (ADR-044; the refusal is documented at
   `activity_log`).
3. **The `.kibana_1` deletion is safe by construction, not by hope.** The
   remediation only arms from a stall whose stage is exactly `dashboards` —
   a Deployment that has never had a ready replica has never served a
   request, so the index cannot hold anything but aborted migration state.
   It is unreachable for any deployment past the rung.

## Consequences

- The operator and VeloxSearch now co-own one Deployment. The operator
  reconciles replicas toward the CR; our remediation's 0→delete→1 window is
  the same "the operator will close it" bet as the #27 bounce, and the
  survivability fields are claimed under a dedicated field manager so
  neither side's apply prunes the other's.
- A future operator release that exposes probe configuration makes the
  survivability patch redundant; it stays idempotent and harmless until
  then, and is the thing to delete when that lands.
