# ADR-066 — Operator upgrade from Settings, behind a one-time grant

**Status:** proposed
**Date:** 2026-10-27

Amends [ADR-057](ADR-057-upgrade-contract.md) (operator drift is reported,
never applied) and builds on ADR-027 (the app holds no standing cluster-admin;
see the [index](README.md)). Issue #176.

## Context

ADR-057 keeps a VeloxSearch upgrade from ever touching the OpenSearch operator.
A release that vendors a newer operator leaves the running one in place, and R9
reports the drift. The remedy was a hand-run `kubectl apply` of the release's
operator bundle, which DEPLOY.md itself flagged as untested as an upgrade.

That left two problems:

- **The admin cannot tell what the upgrade will do.** Whether the new CRDs drop
  a field a live cluster uses, whether a deployment goes down while its nodes
  restart, whether anything is mid-flight — none of it was shown anywhere.
- **The drift does not get fixed.** 0.13.0 vendors operator `3.0.0`, which ends
  the double-roll of #160. Every existing install keeps `3.0.0-alpha` until
  someone runs the command, and prod did too.

The upgrade needs cluster-admin. It replaces the operator's CRDs and its
ClusterRoles, and Kubernetes only lets a subject grant permissions it already
holds. ADR-027 removes that power from the app after the first bootstrap, on
purpose.

## Decision

Settings gets an **OpenSearch operator** card with two halves.

### 1. The impact report (read-only, no new rights)

`GET /api/operator_upgrade_status`, admin-only, computed by
`src/operator_upgrade.rs` from rights the runtime role already holds:

- **CRD changes.** For each CRD in the vendored bundle, it compares the
  installed CRD's schema field paths per served version with the vendored ones,
  and lists the versions each side serves that the other does not. A removed
  field that a live `OpenSearchCluster` sets is a **blocker**: applying the
  bundle would strand that object.
- **Per deployment**, for every `OpenSearchCluster` (the operator is
  cluster-wide, so is its upgrade, across tenants): health, and node pools with
  replicas. The report assumes every pool may restart once, one pod at a time.
  This is conservative: whether a given operator change rolls pods cannot be
  derived from manifests. A pool of one is **down** for its restart (warning).
  No `OpensearchSnapshotPolicy` covering a deployment is a warning.
- **Blockers:** the running operator is not the one VeloxSearch installed, or it
  could not be read; it is not Ready; a deployment is not green; an ADR-048
  version upgrade is in flight; a bootstrap install is running; or a breaking
  CRD field is removed (above).

Findings carry a stable `code` with `deployment` and `subject`. The UI renders
the sentence (ADR-019); `detail` is the English fallback.

### 2. The upgrade (one-time grant)

A cluster admin arms the button by creating a dedicated binding:

```sh
kubectl create clusterrolebinding veloxsearch-operator-upgrade \
  --clusterrole=cluster-admin --serviceaccount=<ns>:veloxsearch
```

It must be a binding **separate** from `veloxsearch-bootstrap`. The revoke watch
deletes that one within a minute of bootstrap being complete, so it could never
survive until the click.

`POST /api/operator_upgrade` recomputes the whole verdict server-side, refuses
with 409 and the reason when anything blocks, then:

1. applies the vendored operator bundle exactly as bootstrap does
   (`apply_bundle`, field manager `veloxsearch-bootstrap`, server-side apply
   with force);
2. waits up to 300 s for the operator controller to be Ready;
3. **deletes the grant**, on success and on failure alike.

An unused grant is deleted by the revoke watch once it is older than 30 minutes,
and never while the upgrade runs. The app can delete it because, while it
exists, the app holds cluster-admin: the same self-revoke ADR-027 uses. The
runtime role gains nothing.

### What ADR-057 still holds

A VeloxSearch upgrade still never touches the operator. The operator changes
only through this explicit, admin-armed action, which is the "separate,
explicit step" ADR-057 called for, now with a button and a report.

## Consequences

- An admin sees the cost before paying it, and the common case (additive CRDs,
  green multi-replica deployments) becomes one command plus one click.
- cluster-admin is held only between the admin creating the grant and the
  upgrade ending, at most 30 minutes when unused. While it exists, the app has
  the same power the first bootstrap had.
- "Each pool may restart once" can overstate the restarts, but it never
  understates them. The admin plans for the worst case.
- The report does not look inside OpenSearch: an index with zero replicas still
  loses availability while its node restarts, even in a green cluster with
  several nodes. Covering that needs a per-index query and is left out until
  someone needs it.

## Alternatives considered

- **Standing RBAC for the app** (update CRDs, the operator Deployment and
  ClusterRoles). Pure one-click, but it reverses ADR-027: a compromised app could
  rewrite every cluster's CRDs at any time. Rejected by the operator, 2026-10-27.
- **Report plus a copy-paste command.** No security change, but the admin still
  runs an apply that nothing checks against the report. Rejected as not being
  the button that was asked for.
