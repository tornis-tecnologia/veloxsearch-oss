# ADR-063 — The Dashboards first-boot fix goes through the CR, not the operator's Deployment

**Status:** accepted (supersedes ADR-055 decision 1 and the trigger in decision 2; ADR-055 decision 3 and its container-log refusal stand)
**Date:** 2026-09-26

## Context

ADR-055 let VeloxSearch patch the operator's Dashboards Deployment (#46): a
`Recreate` strategy and a 30-minute startup budget, applied once after
create, plus a 0/1 scale of `spec.replicas` for the `.kibana_1`
remediation. A live run of 0.10.5 (2026-09-24) showed the patch is accepted
and then **reverted within about one second**: the operator owns the
Deployment, watches it, and re-applies its rendered spec on every change.
ADR-055's own premise — "fields the operator does not fight over" — was
wrong. The same revert applies to the remediation's scale, so its "quiesce"
step never held either.

What the CR can say, checked against the vendored operator (chart 3.0.2,
app `3.0.0-alpha`, `deploy/bootstrap/operator.yaml`) and the upstream
builder (`pkg/builders/dashboards.go`, tags `v3.0.0-alpha` and `v3.0.0`):

- `spec.dashboards` has no probe, strategy or pod-template field. The
  builder hardcodes one probe object for startup/liveness/readiness
  (`initialDelaySeconds 10`, `periodSeconds 20`, `failureThreshold 10`) and
  `strategy: RollingUpdate`. `v3.0.0` only adds explicit `rollingUpdate`
  defaults. Upgrading the operator would not change this.
- `spec.dashboards.additionalConfig` reaches `opensearch_dashboards.yml`,
  but no Dashboards setting shortens a first migration or breaks the
  "another instance is migrating" wait safely (`migrations.skip` would
  leave Dashboards without its saved-objects index).
- `spec.dashboards.replicas` is rendered into the Deployment, but a **zero
  is not stored as sent at create.** Upstream declares it `int32` with
  `omitempty` and `+kubebuilder:default=1`. The operator's first write to a
  new CR is a full-object update (it adds its finalizers) that drops a 0
  from the body, and the API server then defaults it back to 1. This was
  observed live on 2026-09-26: 3 of 3 creates read `replicas: 1` 2–10s
  after being created with 0, with the operator as the writer in
  `managedFields`. A first cut of this ADR assumed "rendered verbatim" and
  its hold never held. A zero written *after* that first update does stick:
  the Deployment goes to 0, and the operator leaves it there.

## Decision

1. **The first boot is deferred, not lengthened.** A new deployment's CR is
   created with `spec.dashboards.replicas: 0` and the annotation
   `veloxsearch.ai/dashboards-hold: first-boot`. The annotation is the
   durable intent. The zero is enforced whenever the stored value drifts
   from it:
   - **Right after create,** a spawned watch polls the CR every 2s (for at
     most 3 minutes). It writes the zero again whenever the stored value is
     not 0, and stops once the operator's `Opensearch` finalizer is on a CR
     that still reads 0. The finalizer marks the operator's first write. The
     Deployment it rendered from the dropped zero runs for a few seconds at
     most, long before a migration can start.
   - **The metrics sampler** (every tick, every deployment — the #47 re-arm
     precedent) makes the same decision as a backstop: after a backend
     restart, and after any later full update by the operator.
   - **Release** is one JSON merge patch (`replicas: 1`, annotation removed).
     Only the sampler releases, and only on velox's own verdict, never on the
     CR's `status.health`. That field lags: in the 2026-09-27 run a release
     fired on a CR "green" 18s after the operator's post-bootstrap restart
     re-created nodes-0, while the real health went unknown → red → yellow.
     Release needs two things:
     - `activity::nodes_settled_of`: `settled_of` without its Dashboards
       clause. Every node ready and on its revision, security initialized,
       green, nothing rolling.
     - A still node pool: no node pod created for 120s, on the same clock the
       activity panel uses.

     The ceiling stays. Once the CR is 20 minutes old and initialized, the
     hold is released anyway, so a cluster that never settles still gets
     Dashboards. On a settled cluster the migration takes seconds, well
     inside the fixed ~210s budget.
   - **A serving Dashboards is never scaled down by the hold.** If a
     Dashboards replica is already Ready, the hold was lost and the first
     boot already happened. Only the annotation is removed. Scaling a
     serving Dashboards to zero would cause an outage and protect nothing.
   - **Every hold write** carries the `resourceVersion` it was decided on.
     A concurrent write makes it fail with a conflict, so an enforce racing
     a release can never leave a zero without the annotation that releases
     it. The next poll decides again.

   Other mechanisms were weighed and rejected. `dashboards.enable: false`
   does survive `omitempty`, because false and absent mean the same. But
   then the Deployment does not exist, and velox reads a missing Dashboards
   Deployment as "no Dashboards": the ladder would call it done and deferred
   provisioning would skip its Dashboards items. A Deployment patch is what
   this ADR removes.
2. **The remediation holds through the CR too:** hold (`replicas: 0`,
   reason `remediation`), wait for the pods to go, delete `.kibana_1`,
   release. A backend that dies mid-pass leaves the annotation, and the
   sampler releases it; it skips a hold whose remediation this process is
   still running.

   **Its trigger** was never reachable in the deadlock's real shape. A
   planted deadlock on 2026-09-27 (k8s 1.34) restarted 5 times and armed 0
   times, for two reasons:
   - **Wrong signal.** On each startup-probe kill Dashboards exits 0
     (`Completed`) and is restarted at once. It shows `Running` with a
     growing `restartCount` and never `CrashLoopBackOff`, which the trigger
     required. It now arms on `restartCount ≥ 3` with the last termination
     at most 15 minutes old, whatever the waiting reason. `CrashLoopBackOff`
     counts as "dying now" only when the termination time is unreadable.
     The one-shot cooldown and the rung gate (never-ready Dashboards only)
     are unchanged. Container logs stay unread.
   - **Wrong clock.** On an otherwise settled cluster, status takes a fast
     path that set `since_secs: 0`, so the Dashboards rung could never read
     as stalled and its diagnosis never ran. That path now measures the
     Dashboards pod's own age. A fresh boot is young and never a stall. A
     boot restarted in the same pod ages into one. A held Dashboards has no
     pod and is never flagged.
3. **Saves never move Dashboards replicas.** `create_cluster` is also the
   save path. For an existing CR it re-applies the stored
   `spec.dashboards.replicas` and hold annotation exactly as read, the same
   way it preserves the versions (ADR-048). It can neither start a held
   Dashboards nor stop a running one; enforcing the hold is the enforcer's
   job alone. The first cut re-applied 0 for any held CR, and a no-op save
   scaled a running Dashboards to 0 in the live run.
4. **No write on Deployments.** The Deployment patch is removed, and the
   runtime ClusterRole loses `patch` on `apps/deployments`. A test pins that.

Deployments created before this change carry no hold and are untouched
except by the remediation, which now actually quiesces.

## Consequences

- Dashboards comes up up to one sampler interval (60s by default) after the
  cluster turns green, rather than racing it. The post-create watch only
  enforces the hold and does not release it. The dashboards rung of the
  activity ladder covers that wait.
- `Recreate` is not achievable. A Dashboards template change (for example a
  version upgrade) still rolls with the operator's `RollingUpdate`. The first
  boot needs no roll, so the incident's trigger is covered. A second pod
  racing a migration after an upgrade is left to the remediation.
- If a future operator exposes `spec.dashboards` probes, a longer startup
  budget can be set there as well. The hold stays useful either way.
