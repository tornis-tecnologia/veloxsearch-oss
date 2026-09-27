// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The rules around an admin-password reset (#115, ADR-064).
//!
//! Pure: no cluster calls, no I/O, no clock. `k8s.rs` gathers the facts and
//! performs the writes; `api.rs` maps [`ResetError`] onto a status code. The
//! split is the `activity.rs` / `provisioning.rs` shape.
//!
//! A reset rewrites `<name>-admin-credentials` and nudges the operator, which
//! then does two things in ONE reconcile: it copies the new password into
//! `<name>-admin-password` — the Secret the node readiness and startup probes
//! authenticate with — and it starts a one-shot securityconfig Job
//! (`BackoffLimit: 0`) that pushes the new hash to the cluster. If that Job
//! fails, the probes use a password the cluster does not know, every node goes
//! unready, the Service loses its endpoints, and the operator never re-runs a
//! Job whose content it considers already applied. Found live on a kind
//! cluster: a reset that landed during a rolling restart left the deployment
//! permanently down.
//!
//! Three rules follow, one per function below:
//!
//! 1. [`gate`] — **refuse the reset unless the deployment has settled**
//!    (ADR-050) and no earlier reset is still unconfirmed. This closes the
//!    proven trigger.
//! 2. [`outcome`] — **a reset is applied or it is not, and the answer says
//!    which.** The Secret is written first, so a failed nudge is rolled back,
//!    and a rollback that also fails is reported as a partial reset.
//! 3. [`backstop`] — **a reset whose Job failed is undone.** The previous
//!    password is kept in the credentials Secret for the reset window, and a
//!    failed Job restores it; the operator then copies it back into the probe
//!    Secret and the nodes recover. The restore's own Job is watched the same
//!    way, retried once if it fails, and a second failure is marked stalled
//!    on the CR instead of looping.

use std::fmt;

/// Annotation on the `OpenSearchCluster` that forces the operator to reconcile.
/// Its value is the reset's epoch seconds.
pub const NUDGE_ANNOTATION: &str = "veloxsearch.ai/security-reset";

/// Annotation on the `OpenSearchCluster` that marks a reset as unconfirmed:
/// present from the nudge until the backstop sees a securityconfig Job
/// succeed or the window pass. Its value is a [`Pending`] rendered by
/// [`Pending::value`]: the epoch seconds of the latest nudge, then the stage
/// (`1790000000`, `1790000000 restore 1`, `1790000000 stalled`). Written in
/// the same apply as [`NUDGE_ANNOTATION`], so a nudge that landed always
/// carries it.
pub const PENDING_ANNOTATION: &str = "veloxsearch.ai/security-reset-pending";

/// Key in `<name>-admin-credentials` holding the password the reset replaced.
/// The Secret already holds the current password, so the previous one is kept
/// at the same trust level — never in a log, an annotation or the database.
pub const PREVIOUS_PASSWORD_KEY: &str = "previous-password";

/// How long the backstop waits for a verdict before it gives up and forgets
/// the previous password. The securityconfig Job either reaches the cluster
/// and applies the hash within minutes, or exhausts its 20 attempts; an hour
/// with neither means the operator did not run it at all, and holding the old
/// password longer buys nothing. Counted from the latest nudge, so a restore
/// gets its own window rather than whatever the reset left of it.
pub const WINDOW_SECS: i64 = 3600;

/// How many restores the backstop runs for one reset: the restore, and one
/// retry if the restore's own Job fails. After that it marks the reset
/// [`Stage::Stalled`] instead of looping.
pub const MAX_RESTORES: u8 = 2;

/// Tolerance between the backend's clock (which stamps the reset) and the API
/// server's (which stamps the Job's pod). Both run in the same cluster.
pub const CLOCK_SKEW_SECS: i64 = 30;

// ───────────────────────────── 1. the gate ─────────────────────────────

/// Why a reset was refused before anything was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The deployment is creating, rolling, upgrading or unhealthy.
    NotSettled,
    /// An earlier reset has not been confirmed applied yet.
    ResetPending,
    /// An earlier reset failed and so did restoring the previous password;
    /// the backstop stopped and a person has to look.
    ResetStalled,
}

/// May a reset start now? `settled` is ADR-050's predicate for the deployment;
/// `pending` is [`PENDING_ANNOTATION`]'s value on its CR, if any — any value,
/// readable or not, refuses.
///
/// A settled deployment is one whose nodes are all ready on the current
/// revision with nothing rolling — the one state where the securityconfig Job
/// can reach the cluster through its Service. A pending reset is refused too:
/// a second reset would overwrite the only copy of the password the first one
/// replaced, before anyone knows whether the first one took.
pub fn gate(settled: bool, pending: Option<&str>) -> Result<(), Refusal> {
    if let Some(value) = pending {
        match parse_pending(value) {
            Some(Pending {
                stage: Stage::Stalled,
                ..
            }) => Err(Refusal::ResetStalled),
            _ => Err(Refusal::ResetPending),
        }
    } else if !settled {
        Err(Refusal::NotSettled)
    } else {
        Ok(())
    }
}

// ───────────────────────────── 2. the outcome ─────────────────────────────

/// What a reset request actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Secret rewritten and the operator nudged.
    Applied,
    /// The nudge failed and the Secret was restored: the password is unchanged.
    NotApplied,
    /// The nudge failed and so did the restore: the Secret holds the NEW
    /// password, which the operator applies on its next reconcile.
    Partial,
}

/// The verdict once the Secret has been rewritten. `nudged` is whether the
/// reconcile annotation landed; `rolled_back` is `None` when no rollback was
/// attempted, else whether the Secret restore succeeded.
pub fn outcome(nudged: bool, rolled_back: Option<bool>) -> Outcome {
    match (nudged, rolled_back) {
        (true, _) => Outcome::Applied,
        (false, Some(true)) => Outcome::NotApplied,
        (false, _) => Outcome::Partial,
    }
}

/// A reset that did not complete. `api.rs` maps [`ResetError::Refused`] to
/// 409 and the rest to 500; every message says what state the password is in.
#[derive(Debug)]
pub enum ResetError {
    Refused(Refusal),
    /// The password is unchanged. `cause` is the nudge error.
    NotApplied {
        cause: String,
    },
    /// The password changed without the operator being told. `cause` is the
    /// nudge error, `rollback` the restore error.
    Partial {
        cause: String,
        rollback: String,
    },
}

impl fmt::Display for ResetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(Refusal::NotSettled) => f.write_str(
                "the deployment is still changing (creating, restarting nodes or \
                 upgrading) — reset the admin password once it has settled",
            ),
            Self::Refused(Refusal::ResetPending) => f.write_str(
                "an earlier admin-password reset is still being applied — try \
                 again once it has finished",
            ),
            Self::Refused(Refusal::ResetStalled) => f.write_str(
                "an earlier admin-password reset could not be applied, and \
                 restoring the previous password failed twice — the automatic \
                 recovery has stopped; check the deployment's securityconfig \
                 update Job before resetting again",
            ),
            Self::NotApplied { cause } => write!(
                f,
                "the admin password was NOT changed: the operator could not be \
                 asked to apply it ({cause})"
            ),
            Self::Partial { cause, rollback } => write!(
                f,
                "the admin password WAS changed but the operator has not applied \
                 it yet; it will on its next reconcile. Reveal the current \
                 password in the Security tab. (nudge: {cause}; rollback: {rollback})"
            ),
        }
    }
}

impl std::error::Error for ResetError {}

/// The Secret merge patch that starts a reset: the new password, and the one
/// it replaces kept under [`PREVIOUS_PASSWORD_KEY`] for the backstop.
pub fn start_patch(user: &str, new_password: &str, previous: &str) -> serde_json::Value {
    let mut string_data = serde_json::Map::new();
    string_data.insert("username".into(), user.into());
    string_data.insert("password".into(), new_password.into());
    string_data.insert(PREVIOUS_PASSWORD_KEY.into(), previous.into());
    serde_json::json!({ "stringData": string_data })
}

/// The Secret merge patch that puts `previous` back as the password. With
/// `drop_kept`, the kept copy goes in the same write (the nudge-failure
/// rollback); without, it stays until the restore's own Job has succeeded
/// (or the window ends) — the backstop's restore.
pub fn restore_patch(previous: &str, drop_kept: bool) -> serde_json::Value {
    let mut patch = serde_json::json!({ "stringData": { "password": previous } });
    if drop_kept {
        patch["data"] = drop_kept_patch()["data"].clone();
    }
    patch
}

/// The Secret merge patch that removes the kept password (`null` deletes a key
/// in a merge patch).
pub fn drop_kept_patch() -> serde_json::Value {
    let mut data = serde_json::Map::new();
    data.insert(PREVIOUS_PASSWORD_KEY.into(), serde_json::Value::Null);
    serde_json::json!({ "data": data })
}

// ───────────────────────────── 3. the backstop ─────────────────────────────

/// The newest pod of the deployment's securityconfig update Job. With
/// `BackoffLimit: 0` the Job has exactly one pod, and its phase is the Job's
/// verdict — which is why the backstop reads pods (already readable) and not
/// Jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRun {
    /// The pod's `creationTimestamp`, epoch seconds.
    pub created_secs: i64,
    /// The pod's `status.phase`: `Pending` | `Running` | `Succeeded` | `Failed`.
    pub phase: String,
}

/// Where a pending reset is — the stage half of [`PENDING_ANNOTATION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The reset's own Job has not given a verdict yet.
    Reset,
    /// The `n`th restore of the previous password is waiting on its Job.
    Restore(u8),
    /// Every restore failed. Nothing more is tried; the marker stays so the
    /// gate can say so, until the window from the stall has passed.
    Stalled,
}

/// A parsed [`PENDING_ANNOTATION`]: the epoch seconds of the latest nudge (a
/// securityconfig run created before it belongs to an earlier nudge), and
/// the stage it was written at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    pub since: i64,
    pub stage: Stage,
}

impl Pending {
    /// The annotation value, the inverse of [`parse_pending`].
    pub fn value(&self) -> String {
        match self.stage {
            Stage::Reset => self.since.to_string(),
            Stage::Restore(n) => format!("{} restore {n}", self.since),
            Stage::Stalled => format!("{} stalled", self.since),
        }
    }
}

/// What to do about a pending reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backstop {
    /// No verdict yet — look again on the next read.
    Wait,
    /// Done with this reset: drop the pending marker and the previous password.
    Forget,
    /// The latest Job failed: put the previous password back, nudge again, and
    /// write `next` as the marker. The previous password is kept — the
    /// restore is only done when its own Job succeeds.
    Restore { next: Pending },
    /// The last allowed restore failed too: write `next` (stalled) as the
    /// marker and stop acting.
    Stall { next: Pending },
}

/// Parse [`PENDING_ANNOTATION`]'s value. `None` means unreadable, which the
/// backstop treats as [`Backstop::Forget`]: a marker we cannot date cannot be
/// matched to a Job, and restoring on a guess is worse than doing nothing.
/// A bare number is a reset (the only form before restores were watched).
pub fn parse_pending(value: &str) -> Option<Pending> {
    let mut words = value.split_whitespace();
    let since = words.next()?.parse::<i64>().ok().filter(|s| *s > 0)?;
    let stage = match (words.next(), words.next()) {
        (None, _) => Stage::Reset,
        (Some("stalled"), None) => Stage::Stalled,
        (Some("restore"), Some(n)) => Stage::Restore(n.parse::<u8>().ok().filter(|n| *n > 0)?),
        _ => return None,
    };
    // Anything after the stage is not a marker this code wrote.
    words.next().is_none().then_some(Pending { since, stage })
}

/// The epoch a restore stamps on its marker: now, but never close enough to
/// the failed run for that run to pass as the restore's own. With
/// [`CLOCK_SKEW_SECS`] of tolerance, a run created up to 30s before `since`
/// still counts as "this nudge's" — and a Job that fails fast, or a failed
/// pod the API has not collected yet, must not read as the restore failing.
fn restore_since(now: i64, failed_created: i64) -> i64 {
    now.max(failed_created + CLOCK_SKEW_SECS + 1)
}

/// Decide what a pending reset needs, from its marker, the newest update run,
/// and now (all epoch seconds).
///
/// A run created before the latest nudge belongs to an earlier reconcile and
/// says nothing about it, so it is ignored. The operator replaces the Job
/// when the nudge changes its content, so a failed run of THIS nudge is
/// final: nothing will retry it on its own.
///
/// - **Succeeded**, at any stage: forget — the only confirmation there is.
/// - **Failed** after the reset: restore (1). After restore `n`: restore
///   again while `n <` [`MAX_RESTORES`], else stall.
/// - **Stalled**: nothing more is tried; forget once its window has passed.
/// - No verdict by the end of the window: forget.
pub fn backstop(pending: &Pending, run: Option<&UpdateRun>, now: i64) -> Backstop {
    let this_nudge = run.filter(|r| r.created_secs + CLOCK_SKEW_SECS >= pending.since);
    let phase = this_nudge.map(|r| r.phase.as_str());
    let restore = |attempt: u8, failed: &UpdateRun| Backstop::Restore {
        next: Pending {
            since: restore_since(now, failed.created_secs),
            stage: Stage::Restore(attempt),
        },
    };
    match (pending.stage, phase, this_nudge) {
        (_, Some("Succeeded"), _) => Backstop::Forget,
        (Stage::Reset, Some("Failed"), Some(r)) => restore(1, r),
        (Stage::Restore(n), Some("Failed"), Some(r)) if n < MAX_RESTORES => restore(n + 1, r),
        (Stage::Restore(_), Some("Failed"), _) => Backstop::Stall {
            next: Pending {
                since: now,
                stage: Stage::Stalled,
            },
        },
        _ if now - pending.since >= WINDOW_SECS => Backstop::Forget,
        _ => Backstop::Wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. gate ───────────────────────────────────────────────────────

    #[test]
    fn a_settled_deployment_may_reset() {
        assert_eq!(gate(true, None), Ok(()));
    }

    #[test]
    fn a_rolling_deployment_is_refused() {
        // the #115 trigger: green between two node restarts, not settled
        assert_eq!(gate(false, None), Err(Refusal::NotSettled));
    }

    #[test]
    fn an_unconfirmed_reset_blocks_the_next_one_even_when_settled() {
        // the second reset would overwrite the only copy of the old password
        assert_eq!(gate(true, Some("1790000000")), Err(Refusal::ResetPending));
        assert_eq!(gate(false, Some("1790000000")), Err(Refusal::ResetPending));
        // a restore still being watched is a pending reset too
        assert_eq!(
            gate(true, Some("1790000000 restore 1")),
            Err(Refusal::ResetPending)
        );
        // an unreadable marker still refuses: it may guard a kept password
        assert_eq!(gate(true, Some("garbage")), Err(Refusal::ResetPending));
    }

    #[test]
    fn a_stalled_reset_is_refused_saying_so() {
        assert_eq!(
            gate(true, Some("1790000000 stalled")),
            Err(Refusal::ResetStalled)
        );
        let s = ResetError::Refused(Refusal::ResetStalled).to_string();
        assert!(s.contains("stopped") && s.contains("failed twice"), "{s}");
    }

    #[test]
    fn refusals_say_what_to_wait_for() {
        let s = ResetError::Refused(Refusal::NotSettled).to_string();
        assert!(s.contains("settled"), "{s}");
        let s = ResetError::Refused(Refusal::ResetPending).to_string();
        assert!(s.contains("earlier"), "{s}");
    }

    // ── 2. outcome ────────────────────────────────────────────────────

    #[test]
    fn a_landed_nudge_is_applied() {
        assert_eq!(outcome(true, None), Outcome::Applied);
    }

    #[test]
    fn a_failed_nudge_with_a_restored_secret_changed_nothing() {
        assert_eq!(outcome(false, Some(true)), Outcome::NotApplied);
    }

    #[test]
    fn a_failed_nudge_and_a_failed_rollback_is_partial_never_silent() {
        assert_eq!(outcome(false, Some(false)), Outcome::Partial);
        // no rollback attempted is the same claim: the Secret was written
        assert_eq!(outcome(false, None), Outcome::Partial);
    }

    #[test]
    fn every_failure_message_states_the_password() {
        let na = ResetError::NotApplied {
            cause: "webhook 500".into(),
        }
        .to_string();
        assert!(
            na.contains("NOT changed") && na.contains("webhook 500"),
            "{na}"
        );
        let p = ResetError::Partial {
            cause: "webhook 500".into(),
            rollback: "conflict".into(),
        }
        .to_string();
        assert!(p.contains("WAS changed") && p.contains("conflict"), "{p}");
    }

    #[test]
    fn the_start_patch_keeps_the_replaced_password() {
        let p = start_patch("admin", "new-pw", "old-pw");
        assert_eq!(p["stringData"]["username"], "admin");
        assert_eq!(p["stringData"]["password"], "new-pw");
        assert_eq!(p["stringData"][PREVIOUS_PASSWORD_KEY], "old-pw");
        assert!(p.get("data").is_none());
    }

    #[test]
    fn the_rollback_restores_and_drops_the_kept_copy_in_one_write() {
        let p = restore_patch("old-pw", true);
        assert_eq!(p["stringData"]["password"], "old-pw");
        assert!(p["data"][PREVIOUS_PASSWORD_KEY].is_null());
        assert!(p["data"]
            .as_object()
            .unwrap()
            .contains_key(PREVIOUS_PASSWORD_KEY));
    }

    #[test]
    fn the_backstop_restore_keeps_the_copy() {
        let p = restore_patch("old-pw", false);
        assert_eq!(p["stringData"]["password"], "old-pw");
        assert!(p.get("data").is_none());
        assert!(p["stringData"].get(PREVIOUS_PASSWORD_KEY).is_none());
    }

    // ── 3. backstop ───────────────────────────────────────────────────

    const T: i64 = 1_790_000_000;

    fn run(created_secs: i64, phase: &str) -> UpdateRun {
        UpdateRun {
            created_secs,
            phase: phase.into(),
        }
    }

    fn reset(since: i64) -> Pending {
        Pending {
            since,
            stage: Stage::Reset,
        }
    }

    fn at(since: i64, stage: Stage) -> Pending {
        Pending { since, stage }
    }

    #[test]
    fn a_failed_run_of_this_reset_restores_and_keeps_watching() {
        // the live #115 shape: Job created seconds after the nudge, Failed
        assert_eq!(
            backstop(&reset(T), Some(&run(T + 5, "Failed")), T + 600),
            Backstop::Restore {
                next: at(T + 600, Stage::Restore(1))
            }
        );
    }

    #[test]
    fn a_succeeded_run_of_this_reset_forgets() {
        assert_eq!(
            backstop(&reset(T), Some(&run(T + 5, "Succeeded")), T + 60),
            Backstop::Forget
        );
    }

    #[test]
    fn a_running_or_missing_run_waits_inside_the_window() {
        assert_eq!(
            backstop(&reset(T), Some(&run(T + 5, "Running")), T + 60),
            Backstop::Wait
        );
        assert_eq!(
            backstop(&reset(T), Some(&run(T + 5, "Pending")), T + 60),
            Backstop::Wait
        );
        // operator not reconciled yet: no run at all
        assert_eq!(backstop(&reset(T), None, T + 60), Backstop::Wait);
    }

    #[test]
    fn a_run_from_before_the_reset_is_not_its_verdict() {
        // an older failed Job must never trigger a restore of THIS reset
        let old = run(T - CLOCK_SKEW_SECS - 1, "Failed");
        assert_eq!(backstop(&reset(T), Some(&old), T + 60), Backstop::Wait);
        let old_ok = run(T - 3600, "Succeeded");
        assert_eq!(backstop(&reset(T), Some(&old_ok), T + 60), Backstop::Wait);
    }

    #[test]
    fn clock_skew_inside_the_tolerance_still_matches() {
        let skewed = run(T - CLOCK_SKEW_SECS, "Failed");
        assert!(matches!(
            backstop(&reset(T), Some(&skewed), T + 60),
            Backstop::Restore { .. }
        ));
    }

    #[test]
    fn no_verdict_by_the_end_of_the_window_forgets() {
        assert_eq!(
            backstop(&reset(T), None, T + WINDOW_SECS - 1),
            Backstop::Wait
        );
        assert_eq!(backstop(&reset(T), None, T + WINDOW_SECS), Backstop::Forget);
        let old = run(T - 3600, "Failed");
        assert_eq!(
            backstop(&reset(T), Some(&old), T + WINDOW_SECS),
            Backstop::Forget
        );
    }

    #[test]
    fn a_verdict_wins_over_the_window() {
        // a failure seen late is still a failure: restoring is the point
        assert!(matches!(
            backstop(&reset(T), Some(&run(T + 5, "Failed")), T + 10 * WINDOW_SECS),
            Backstop::Restore { .. }
        ));
    }

    // ── 3b. the restore is watched until its own Job succeeds ─────────

    #[test]
    fn a_restore_is_not_done_until_its_own_job_succeeds() {
        // the #115 follow-up: after the restore, the reset's failed run is
        // still the newest pod for a moment — it must not be read again,
        // and nothing is forgotten while the restore's Job runs
        let restoring = at(T + 600, Stage::Restore(1));
        let failed_reset_run = run(T + 5, "Failed");
        assert_eq!(
            backstop(&restoring, Some(&failed_reset_run), T + 610),
            Backstop::Wait
        );
        assert_eq!(
            backstop(&restoring, Some(&run(T + 605, "Running")), T + 640),
            Backstop::Wait
        );
        assert_eq!(backstop(&restoring, None, T + 640), Backstop::Wait);
        // confirmed: now, and only now, the kept password goes
        assert_eq!(
            backstop(&restoring, Some(&run(T + 605, "Succeeded")), T + 700),
            Backstop::Forget
        );
    }

    #[test]
    fn a_failed_restore_is_retried_once_then_stalls() {
        let first = at(T + 600, Stage::Restore(1));
        assert_eq!(
            backstop(&first, Some(&run(T + 605, "Failed")), T + 900),
            Backstop::Restore {
                next: at(T + 900, Stage::Restore(2))
            }
        );
        let retry = at(T + 900, Stage::Restore(MAX_RESTORES));
        assert_eq!(
            backstop(&retry, Some(&run(T + 905, "Failed")), T + 1200),
            Backstop::Stall {
                next: at(T + 1200, Stage::Stalled)
            }
        );
    }

    #[test]
    fn a_stalled_reset_is_left_alone_until_its_window_ends() {
        let stalled = at(T, Stage::Stalled);
        // the failure that stalled it is still the newest run: no loop
        let failed = run(T - 10, "Failed");
        assert_eq!(backstop(&stalled, Some(&failed), T + 60), Backstop::Wait);
        assert_eq!(
            backstop(&stalled, Some(&failed), T + WINDOW_SECS - 1),
            Backstop::Wait
        );
        // the existing 1h bound is where the kept password goes
        assert_eq!(
            backstop(&stalled, Some(&failed), T + WINDOW_SECS),
            Backstop::Forget
        );
        // a success seen after all (someone fixed it by hand) still confirms
        assert_eq!(
            backstop(&stalled, Some(&run(T + 30, "Succeeded")), T + 60),
            Backstop::Forget
        );
    }

    #[test]
    fn a_restore_with_no_verdict_is_forgotten_at_its_own_window() {
        // the window runs from the restore's nudge, not the reset's: a
        // restore made late in the reset's hour still gets its full hour
        let restoring = at(T + 3000, Stage::Restore(1));
        assert_eq!(backstop(&restoring, None, T + 3700), Backstop::Wait);
        assert_eq!(
            backstop(&restoring, None, T + 3000 + WINDOW_SECS),
            Backstop::Forget
        );
    }

    #[test]
    fn a_fast_failing_run_cannot_pass_for_the_restore() {
        // the reset's Job failed within seconds and the backstop acted
        // right away: the marker is pushed past the skew tolerance so that
        // failed pod never reads as the restore's own verdict
        let quick = run(T + 2, "Failed");
        let Backstop::Restore { next } = backstop(&reset(T), Some(&quick), T + 10) else {
            panic!("expected a restore");
        };
        assert_eq!(next.since, T + 2 + CLOCK_SKEW_SECS + 1);
        assert_eq!(backstop(&next, Some(&quick), T + 20), Backstop::Wait);
        // while a run the restore's nudge started a second later does count
        assert_eq!(
            backstop(&next, Some(&run(T + 3, "Succeeded")), T + 40),
            Backstop::Forget
        );
    }

    #[test]
    fn the_pending_marker_round_trips_or_is_unreadable() {
        for p in [
            reset(T),
            at(T, Stage::Restore(1)),
            at(T, Stage::Restore(2)),
            at(T, Stage::Stalled),
        ] {
            assert_eq!(parse_pending(&p.value()), Some(p), "{}", p.value());
        }
        assert_eq!(parse_pending(" 1790000000 "), Some(reset(T)));
        assert_eq!(parse_pending(""), None);
        assert_eq!(parse_pending("yesterday"), None);
        assert_eq!(parse_pending("0"), None);
        assert_eq!(parse_pending("-5"), None);
        assert_eq!(parse_pending("1790000000 restore"), None);
        assert_eq!(parse_pending("1790000000 restore 0"), None);
        assert_eq!(parse_pending("1790000000 restore x"), None);
        assert_eq!(parse_pending("1790000000 stalled 3"), None);
        assert_eq!(parse_pending("1790000000 later"), None);
    }
}
