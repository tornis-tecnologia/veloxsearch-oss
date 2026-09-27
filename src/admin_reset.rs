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
//!    Secret and the nodes recover.

use std::fmt;

/// Annotation on the `OpenSearchCluster` that forces the operator to reconcile.
/// Its value is the reset's epoch seconds.
pub const NUDGE_ANNOTATION: &str = "veloxsearch.ai/security-reset";

/// Annotation on the `OpenSearchCluster` that marks a reset as unconfirmed:
/// present from the nudge until the backstop sees the securityconfig Job
/// succeed, fail (and restores), or the window pass. Its value is the reset's
/// epoch seconds. Written in the same apply as [`NUDGE_ANNOTATION`], so a nudge
/// that landed always carries it.
pub const PENDING_ANNOTATION: &str = "veloxsearch.ai/security-reset-pending";

/// Key in `<name>-admin-credentials` holding the password the reset replaced.
/// The Secret already holds the current password, so the previous one is kept
/// at the same trust level — never in a log, an annotation or the database.
pub const PREVIOUS_PASSWORD_KEY: &str = "previous-password";

/// How long the backstop waits for a verdict before it gives up and forgets
/// the previous password. The securityconfig Job either reaches the cluster
/// and applies the hash within minutes, or exhausts its 20 attempts; an hour
/// with neither means the operator did not run it at all, and holding the old
/// password longer buys nothing.
pub const WINDOW_SECS: i64 = 3600;

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
}

/// May a reset start now? `settled` is ADR-050's predicate for the deployment;
/// `pending` is whether [`PENDING_ANNOTATION`] is on its CR.
///
/// A settled deployment is one whose nodes are all ready on the current
/// revision with nothing rolling — the one state where the securityconfig Job
/// can reach the cluster through its Service. A pending reset is refused too:
/// a second reset would overwrite the only copy of the password the first one
/// replaced, before anyone knows whether the first one took.
pub fn gate(settled: bool, pending: bool) -> Result<(), Refusal> {
    if pending {
        Err(Refusal::ResetPending)
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
/// rollback); without, it stays until the backstop's nudge has landed.
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

/// What to do about a pending reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backstop {
    /// No verdict yet — look again on the next read.
    Wait,
    /// Done with this reset: drop the pending marker and the previous password.
    Forget,
    /// The reset's Job failed: put the previous password back and nudge again.
    Restore,
}

/// Parse [`PENDING_ANNOTATION`]'s value. `None` means unreadable, which the
/// caller treats as [`Backstop::Forget`]: a marker we cannot date cannot be
/// matched to a Job, and restoring on a guess is worse than doing nothing.
pub fn parse_pending(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok().filter(|s| *s > 0)
}

/// Decide what a pending reset needs, from the reset's time, the newest update
/// run, and now (all epoch seconds).
///
/// A run created before the reset belongs to an earlier reconcile and says
/// nothing about this reset, so it is ignored. The operator replaces the Job
/// when the reset changes its content, so a failed run of THIS reset is final:
/// nothing will retry it, and the probes are already using the new password.
pub fn backstop(pending_since: i64, run: Option<&UpdateRun>, now: i64) -> Backstop {
    let this_reset = run.filter(|r| r.created_secs + CLOCK_SKEW_SECS >= pending_since);
    match this_reset.map(|r| r.phase.as_str()) {
        Some("Succeeded") => Backstop::Forget,
        Some("Failed") => Backstop::Restore,
        _ if now - pending_since >= WINDOW_SECS => Backstop::Forget,
        _ => Backstop::Wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. gate ───────────────────────────────────────────────────────

    #[test]
    fn a_settled_deployment_may_reset() {
        assert_eq!(gate(true, false), Ok(()));
    }

    #[test]
    fn a_rolling_deployment_is_refused() {
        // the #115 trigger: green between two node restarts, not settled
        assert_eq!(gate(false, false), Err(Refusal::NotSettled));
    }

    #[test]
    fn an_unconfirmed_reset_blocks_the_next_one_even_when_settled() {
        // the second reset would overwrite the only copy of the old password
        assert_eq!(gate(true, true), Err(Refusal::ResetPending));
        assert_eq!(gate(false, true), Err(Refusal::ResetPending));
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
    fn the_backstop_restore_keeps_the_copy_until_its_nudge_lands() {
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

    #[test]
    fn a_failed_run_of_this_reset_restores() {
        // the live #115 shape: Job created seconds after the nudge, Failed
        assert_eq!(
            backstop(T, Some(&run(T + 5, "Failed")), T + 600),
            Backstop::Restore
        );
    }

    #[test]
    fn a_succeeded_run_of_this_reset_forgets() {
        assert_eq!(
            backstop(T, Some(&run(T + 5, "Succeeded")), T + 60),
            Backstop::Forget
        );
    }

    #[test]
    fn a_running_or_missing_run_waits_inside_the_window() {
        assert_eq!(
            backstop(T, Some(&run(T + 5, "Running")), T + 60),
            Backstop::Wait
        );
        assert_eq!(
            backstop(T, Some(&run(T + 5, "Pending")), T + 60),
            Backstop::Wait
        );
        // operator not reconciled yet: no run at all
        assert_eq!(backstop(T, None, T + 60), Backstop::Wait);
    }

    #[test]
    fn a_run_from_before_the_reset_is_not_its_verdict() {
        // an older failed Job must never trigger a restore of THIS reset
        let old = run(T - CLOCK_SKEW_SECS - 1, "Failed");
        assert_eq!(backstop(T, Some(&old), T + 60), Backstop::Wait);
        let old_ok = run(T - 3600, "Succeeded");
        assert_eq!(backstop(T, Some(&old_ok), T + 60), Backstop::Wait);
    }

    #[test]
    fn clock_skew_inside_the_tolerance_still_matches() {
        let skewed = run(T - CLOCK_SKEW_SECS, "Failed");
        assert_eq!(backstop(T, Some(&skewed), T + 60), Backstop::Restore);
    }

    #[test]
    fn no_verdict_by_the_end_of_the_window_forgets() {
        assert_eq!(backstop(T, None, T + WINDOW_SECS - 1), Backstop::Wait);
        assert_eq!(backstop(T, None, T + WINDOW_SECS), Backstop::Forget);
        let old = run(T - 3600, "Failed");
        assert_eq!(backstop(T, Some(&old), T + WINDOW_SECS), Backstop::Forget);
    }

    #[test]
    fn a_verdict_wins_over_the_window() {
        // a failure seen late is still a failure: restoring is the point
        assert_eq!(
            backstop(T, Some(&run(T + 5, "Failed")), T + 10 * WINDOW_SECS),
            Backstop::Restore
        );
    }

    #[test]
    fn the_pending_marker_parses_or_is_unreadable() {
        assert_eq!(parse_pending("1790000000"), Some(1_790_000_000));
        assert_eq!(parse_pending(" 1790000000 "), Some(1_790_000_000));
        assert_eq!(parse_pending(""), None);
        assert_eq!(parse_pending("yesterday"), None);
        assert_eq!(parse_pending("0"), None);
        assert_eq!(parse_pending("-5"), None);
    }
}
