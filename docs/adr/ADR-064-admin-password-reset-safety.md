# ADR-064 — An admin-password reset only starts on a settled deployment, is applied or not, and is undone if the operator's Job fails

**Status:** proposed
**Date:** 2026-09-26

## Context

The cluster admin password cannot be changed through the security REST API (the
`admin` user is reserved), so a reset works through the operator:
`reset_admin_password` rewrites `<name>-admin-credentials` and annotates the
`OpenSearchCluster` to force a reconcile. In that reconcile the operator:

- copies the password into `<name>-admin-password`, the Secret the node
  **readiness and startup probes** mount and authenticate with
  (`curl -u admin:<password> --fail`); and
- starts `<name>-securityconfig-update`, a one-shot Job (`BackoffLimit: 0`) that
  pushes the new hash to the cluster through the cluster Service.

The operator never re-runs that Job. It stamps the Job with a checksum of the
security config, and a Job with the same checksum counts as already applied,
whether it succeeded or not.

Issue #115, found live on a kind cluster: a reset that landed while the
deployment was still rolling after a memory change made the Job fail
(`Failed to apply securityconfig after 20 attempts`). The probes switched to a
password the cluster did not know, every node turned unready, the Service lost
its last endpoint, and nothing could reach the cluster to apply the hash. The
outage was permanent. A second defect showed up in the same run: the Secret is
written before the annotation, so when the annotation patch failed (the
operator's webhook answering 500), the API returned 500 while the password had
already changed.

The SPA already locks the reset button while ADR-050's `locks_edits` is set. The
API never checked, and `wait_green`-style callers (including our own day-2 test)
reached it during the roll.

## Decision

Three rules, in `src/admin_reset.rs` (pure) with the writes in `k8s.rs`:

1. **Gate.** A reset is refused with **409** unless the deployment is settled
   (ADR-050's `activity.settled`) and no earlier reset is still pending. A
   settled deployment has every node ready on the current revision, which is the
   only state where the Job can reach the cluster. A second reset while one is
   pending would overwrite the only copy of the password the first one replaced.
   The SPA renders the 409 in the user's language (`sec_reset_busy`).
2. **Applied or not.** The reset reads the current password first and keeps it
   in the same Secret under `previous-password`, the same trust level as the
   current password: never in a log, an annotation or the database. The nudge
   writes two annotations in one server-side apply, `veloxsearch.ai/security-reset`
   (the reconcile trigger) and `veloxsearch.ai/security-reset-pending` (the
   marker, the reset's epoch seconds). If the nudge fails, the Secret is rolled
   back and the API says **the password was NOT changed**. If the rollback
   fails too, the API says **the password WAS changed** and has not been applied
   yet. A 500 never hides a changed password.
3. **Backstop.** `status_from` looks at any CR that carries the pending marker.
   A CR without one pays nothing, and the sampler lists every deployment on its
   own clock, so the backstop also runs when nobody has the UI open. It reads
   the newest pod of the securityconfig Job (pods, not Jobs: the runtime role
   can already read pods, and with `BackoffLimit: 0` the one pod's phase is the
   Job's verdict). A run created before the reset is ignored.
   - **Succeeded:** forget. Drop the marker and `previous-password`.
   - **Failed:** restore. Write `previous-password` back as the password and
     nudge again without the marker. The operator copies the old password into
     the probe Secret (the cluster still has the old hash), the nodes pass
     their probes again, and the replacement Job applies a hash the cluster
     already has.
   - **No verdict after an hour:** forget. The Job either reaches the cluster
     within minutes or exhausts its attempts. An hour with neither means the
     operator never ran it, and keeping the old password longer does not help.

This is the ADR-050 stall-remediation pattern (#27, #46): a pure decision, an
idempotent action, and state read from the cluster alone, so a backend restart
changes nothing.

## Considered and not done

**Password-independent probes.** The operator accepts a custom
startup/readiness `command` per node pool. A probe that passes on HTTP 200 *or*
401 would make readiness independent of the password entirely. It is not done
here:

- ADR-057 forbids an upgrade from changing an existing `OpenSearchCluster`
  spec, so it could only apply to new deployments. Existing deployments, which
  are where the outage happened, would still depend on the three rules above.
- `node_pool()` is shared by create and every save, and the saves re-apply the
  whole node pool. Limiting the probe change to new deployments (or to saves
  that already roll) means creates and saves have to agree on it. Otherwise the
  first memory edit on an old deployment would add the probe and roll it.
- A node answering 401 is not necessarily a node that is ready to serve. That
  weakens the readiness signal the operator's rolling restart relies on.

With the gate closing the trigger and the backstop recovering from the failure,
this is not worth doing now. Revisit it if a reset failure is ever seen on a
settled deployment.

## Consequences

- A reset from the API or the SPA during a create, roll or upgrade gets a 409
  and changes nothing.
- The previous password lives in `<name>-admin-credentials` for at most the
  reset window (until the Job's verdict, or one hour).
- **Known limitation:** the backstop reads a failed Job as "the hash was never
  applied". The proven failure (`Failed to apply securityconfig after 20
  attempts`) matches that. A Job that applied the hash and then failed a later
  verification step would be restored to the old password, which the cluster
  would no longer accept.
- `tests/day2_check.py` now waits for `settled` rather than `health == green`
  before the password step. It also asserts that a reset during the post-edit
  roll gets a 409 and leaves the password unchanged.
