# ADR-063 — The Dashboards first-boot fix goes through the CR, not the operator's Deployment

**Status:** accepted (supersedes ADR-055 decision 1; decisions 2 and 3 stand)
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
- `spec.dashboards.replicas` is rendered verbatim into the Deployment.

## Decision

1. **The first boot is deferred, not lengthened.** A new deployment's CR is
   created with `spec.dashboards.replicas: 0` and the annotation
   `veloxsearch.ai/dashboards-hold: first-boot`. The metrics sampler (every
   tick, every deployment — the #47 re-arm precedent) releases the hold with
   one JSON merge patch (`replicas: 1`, annotation removed) once the
   security plugin is initialized and health is green, or once the CR is 20
   minutes old and initialized, so a cluster that stays yellow still gets
   Dashboards. On a settled cluster the migration takes seconds, well inside
   the fixed ~210s budget.
2. **The remediation holds through the CR too:** hold (`replicas: 0`,
   reason `remediation`), wait for the pods to go, delete `.kibana_1`,
   release. A backend that dies mid-pass leaves the annotation, and the
   sampler releases it; it skips a hold whose remediation this process is
   still running.
3. **Saves never place or lift a hold.** `create_cluster` is also the save
   path; it reads the hold back off the CR and re-applies the same replicas
   and annotation, as it already does for the versions (ADR-048).
4. **No write on Deployments.** The Deployment patch is removed, and the
   runtime ClusterRole loses `patch` on `apps/deployments`. A test pins that.

Deployments created before this change carry no hold and are untouched
except by the remediation, which now actually quiesces.

## Consequences

- Dashboards comes up up to one sampler interval (60s by default) after the
  cluster turns green, rather than racing it. The dashboards rung of the
  activity ladder covers that wait.
- `Recreate` is not achievable. A Dashboards template change (for example a
  version upgrade) still rolls with the operator's `RollingUpdate`. The first
  boot needs no roll, so the incident's trigger is covered. A second pod
  racing a migration after an upgrade is left to the remediation.
- If a future operator exposes `spec.dashboards` probes, a longer startup
  budget can be set there as well. The hold stays useful either way.
