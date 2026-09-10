# ADR-056 — A healed dependency re-arms one exhausted provisioning wave

**Status:** accepted (amends ADR-052 rule 4)
**Date:** 2026-09-10

## Context

ADR-052 bounded deferred provisioning: five widening settle attempts
(~1h55m), then the record flips `exhausted`, the deployment reports `failed`
with what is missing, and **a human** re-arms via the retry route. The rule's
reasoning was that the persisted counter must stop *automatic* retry loops.

The incident (issue #47) exposed the corner the rule boxed us into: the
attempts died because **Dashboards was unreachable** — every saved-object
import goes through its API — and when the dependency later became healthy,
nothing retried. The counter had done its job; the deployment stayed empty
for a day while its overview said "receiving" (self-telemetry, see the same
issue). A human with the retry button was the only recovery path, and the
button was not even rendered yet.

## Decision

1. **The retry loop the counter guards against is a *blind* loop.** A
   re-arm triggered by an observed state change — the dependency the attempts
   died on is now healthy — is not that loop. `Record::rearm_eligible`
   (pure, tested) says when: exhausted, with blocking work still owed, using
   the same cosmetic-item judgment `state_from` uses.
2. **The metrics sampler is the trigger home.** It is the one always-on
   poller that survives a backend restart and already visits every
   deployment; the check reads the CR record and Dashboards readiness back
   from the cluster each tick, so invariant 6 holds — nothing is remembered.
   `restart()` clears `exhausted` on the CR before the wave spawns, which is
   what makes the re-arm naturally once-per-heal: the next tick is a no-op.
3. **The wave is bounded exactly like a human retry**: the same two calls
   the retry route makes (`begin` + `spawn`), the same CR counter, the same
   give-up if it exhausts again. One wave per healing event, never a cycle.
4. **"Receiving" must be a claim about monitor data.** The overview tile's
   verdict now derives from a bulk doc count over the deployment's monitor
   indices (carried on `metrics_series`, at its existing poll cadence — the
   SSE stream gains no per-frame REST calls). A deployment with no monitors
   says "no monitors installed" instead of claiming anything. And the
   give-up itself is now a first-class banner with the retry button, above
   the tabs, riding the SSE frames.

## Consequences

- A dependency outage that outlasts the schedule no longer requires a human
  to notice a log line; the deployment self-recovers within one sampler
  tick (~60s) of the dependency healing.
- A deployment that exhausts twice over two separate outages earns two
  waves — that is correct: each healing event is a new fact about the world.
- The retry route and the automatic re-arm share one code path, so they can
  never diverge in what they apply.
