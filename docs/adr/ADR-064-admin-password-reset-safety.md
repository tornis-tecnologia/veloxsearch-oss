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

   **Amended 2026-09-27: a restore is watched until its own Job succeeds.**
   The first cut dropped the marker and `previous-password` in the same pass
   that restored, before the restore's Job had run. If that Job failed too,
   nothing watched it any more. Now the restore's nudge rewrites the marker
   instead of removing it, and the marker carries a stage:
   `<epoch>` (the reset), `<epoch> restore <n>`, `<epoch> stalled`. The
   epoch is always the latest nudge, so a run created before it is never
   read as its verdict. A restore stamps its epoch past the failed run's
   creation plus the clock-skew tolerance, so a failed pod that has not been
   collected yet cannot pass for the restore's own.
   - **Succeeded,** at any stage: forget. This is the only point where
     `previous-password` is dropped, apart from the one-hour bound.
   - **The restore's Job failed:** retry once. The retry restores a password
     the Secret already holds, so the operator would compute the same
     checksum and treat its failed Job as applied. The retry therefore
     deletes `<name>-securityconfig-update` first, and the operator starts a
     new Job on the nudge. The runtime Role gains `delete` on `batch/jobs`
     in the app namespace, and nothing else.
   - **The retry failed too, or the Job could not be deleted:** stall. The
     marker becomes `<epoch> stalled` without a nudge, the backend logs an
     error, and a new reset is refused with a 409 whose message says
     automatic recovery has stopped. The SPA still shows its generic
     `sec_reset_busy` text for every 409. Nothing more is tried. `previous-password` stays
     until an hour after the stall.
   - **No verdict:** the hour is counted from the latest nudge, so a restore
     made late in the reset's hour still gets its full hour.

   The operator source (3.0.0-alpha) explains when this matters. It keeps an
   existing bcrypt hash only while it still matches the password. So the
   first restore gets a fresh hash, a new checksum and a new Job, and the
   cluster keeps accepting the old password with the hash it already has.
   In the one proven failure, the reset's Job never reached the cluster.
   Once the probes are back on the old password the nodes recover whatever
   the restore's Job does, so a restore Job failure there is a consistency
   problem, not an outage. The stall exists for the case this ADR already
   names as a known limitation: a cluster that no longer accepts the old
   password. There, no automatic step can help, and a person has to be told.

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
- The previous password lives in `<name>-admin-credentials` until a
  securityconfig Job of the reset or its restore succeeds, or for one hour
  after the latest nudge. With at most two restores and a stall, that is
  bounded to a few hours in the worst case.
- **Known limitation:** the backstop reads a failed Job as "the hash was never
  applied". The proven failure (`Failed to apply securityconfig after 20
  attempts`) matches that. A Job that applied the hash and then failed a later
  verification step would be restored to the old password, which the cluster
  would no longer accept.
- `tests/day2_check.py` now waits for `settled` rather than `health == green`
  before the password step. It also asserts that a reset during the post-edit
  roll gets a 409 and leaves the password unchanged.
