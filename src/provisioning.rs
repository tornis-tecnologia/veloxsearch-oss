// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The work a create/save defers until the cluster settles — and what happens
//! when it does not settle in time (ADR-052).
//!
//! Pure: no cluster calls, no I/O, no clock. `k8s.rs` reads and writes the
//! record on the CR and runs the applier; `api.rs` hands the verdict to the UI.
//! The split is the `activity.rs` / `upgrade.rs` / `snapshot.rs` shape.
//!
//! The module exists because of one observed failure, twice in two days. A
//! create defers the purpose profile (ADR-028) and the selected monitors
//! (ADR-018) into a background task that first waits for the cluster to settle,
//! because OpenSearch is not answering yet. That wait was a single 600s attempt
//! and a timeout `return`ed — permanently. On 2026-08-13 an unrelated
//! cluster-side stall (a hung peer recovery holding up the operator's rolling
//! restart) pushed `tornis-hj27` past ten minutes:
//!
//! ```text
//! ERROR veloxsearch::api::server: deployment tornis-hj27 did not settle
//! within 600s (stuck at nodes (25%)); profile + selected monitors were not applied
//! ```
//!
//! The cluster then came up green and healthy, its CR still carrying
//! `veloxsearch.ai/monitors: kubernetes,nginx` and
//! `veloxsearch.ai/purpose: observability` — with no collection agents and no
//! profile. The annotations said what the user asked for, the deployment looked
//! finished, and the only evidence to the contrary was one server-side log line
//! nobody reads. ADR-050 made this materially more likely by tightening the
//! wait from "green" to "settled".
//!
//! Two properties follow, and they are what this module encodes:
//!
//! 1. **The intent is already on the CR.** The purpose is a label and the
//!    monitors are an annotation, both written by the create itself. So the
//!    outstanding work never has to be *remembered* — it is re-derived from the
//!    cluster on every read, which is ADR-048's and ADR-050's rule (invariant
//!    6: no provisioning state in memory, so a backend restart changes
//!    nothing). What the record below adds is the one fact the CR does not
//!    already carry: which of those items have actually been **applied**.
//! 2. **A slow settle is not a failure.** A cluster that settles at minute
//!    eleven must still get its configuration, so the wait is a bounded
//!    schedule of widening attempts rather than one shot.

use serde::{Deserialize, Serialize};

/// The CR annotation the record is stored in. Absent means "nothing is
/// outstanding" — which is both the terminal state of a successful provision
/// (the applier removes it) and the state of every deployment created before
/// ADR-052 shipped, so no existing deployment is retroactively reported
/// incomplete.
pub const ANNOTATION: &str = "veloxsearch.ai/provisioning";

/// The reserved `done` key prefix for the purpose profile.
const PROFILE_PREFIX: &str = "profile:";
/// The reserved `done` key prefix for a selected monitor.
const MONITOR_PREFIX: &str = "monitor:";
/// The reserved `done` key prefix for a one-time Dashboards default.
const DASHBOARDS_PREFIX: &str = "dashboards:";

// ─────────────────────────── the deferred work ───────────────────────────

/// One unit of deferred work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// The purpose profile (ADR-028). Carries the purpose it was applied for,
    /// so changing the purpose on a live deployment invalidates the record
    /// instead of reading as already done — that is exactly what `save_cluster`
    /// defers, and a save that changes `observability` → `search` must re-run.
    Profile(String),
    /// One selected monitor: a built-in recipe (ADR-018) or a catalog package
    /// (ADR-039). Which one it is, the applier decides; this module only tracks
    /// the id.
    Monitor(String),
    /// A one-time Dashboards default, applied once and never re-asserted.
    ///
    /// Being an `Item` is the whole design: the record makes it run exactly
    /// once, so the value is a STARTING POINT rather than a policy. A user who
    /// changes it in Advanced Settings keeps their choice, because nothing ever
    /// reads it back. Carries the setting name so adding a second default later
    /// does not silently re-run the first.
    DashboardsDefault(String),
}

impl Item {
    /// The key recorded in [`Record::done`]. **A wire format**: it is written
    /// into an annotation on a live cluster, so an old record must keep parsing
    /// after an upgrade. Never repurpose a prefix.
    pub fn key(&self) -> String {
        match self {
            Item::Profile(purpose) => format!("{PROFILE_PREFIX}{purpose}"),
            Item::Monitor(id) => format!("{MONITOR_PREFIX}{id}"),
            Item::DashboardsDefault(k) => format!("{DASHBOARDS_PREFIX}{k}"),
        }
    }
}

/// Everything a settled deployment owes, in the order it must be applied.
///
/// The profile comes first and that ordering is load-bearing, not cosmetic: the
/// profile installs the retention ISM policy whose `ism_template` auto-attaches
/// to log indices *created later*, and the monitors are what create them
/// (`profiles.rs` module docs). Applying a monitor first leaves its indices
/// outside retention until something re-attaches them.
///
/// `search` deployments carry no monitors by construction — the profile
/// installs no agents (ADR-028) and both `create_cluster` and `save_cluster`
/// clear the list server-side — so nothing special is needed here; the CR's
/// annotation is simply empty and the plan is the profile alone.
pub fn plan(purpose: &str, monitors: &[String]) -> Vec<Item> {
    let mut out = vec![Item::Profile(purpose.to_string())];
    let mut seen: Vec<&str> = Vec::new();
    for m in monitors {
        let m = m.trim();
        if m.is_empty() || seen.contains(&m) {
            continue;
        }
        seen.push(m);
        out.push(Item::Monitor(m.to_string()));
    }
    // Last: it touches only the deployment's Dashboards UI, so it must not sit
    // between the profile and the monitors whose ordering IS load-bearing.
    out.push(Item::DashboardsDefault(DARK_MODE.to_string()));
    out
}

/// The Dashboards default this build ships. A new deployment starts dark; the
/// user owns it from then on (see `recipes::set_dark_mode_default`).
pub const DARK_MODE: &str = "theme:darkMode";

// ───────────────────────────── the record ─────────────────────────────

/// What the applier has managed to do so far, stored on the CR.
///
/// Deliberately *only* the applied set and the failure story. The work itself
/// is re-derived from the purpose label and the monitors annotation, so this
/// record can never disagree with the deployment's own configuration: if the
/// user edits the monitors while a retry is pending, the next attempt plans
/// against the new list, not against a stale copy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    /// [`Item::key`] values already applied successfully.
    pub done: Vec<String>,
    /// Attempts consumed from [`SETTLE_BUDGETS`]. Persisted so the schedule is
    /// not silently restarted by a backend restart — a crash-looping pod would
    /// otherwise retry forever.
    pub attempts: u32,
    /// Why the last attempt did not finish, verbatim. Rendered to the user the
    /// same way `snapshot.last_error` and `upgrade.reason` are (ADR-049/048):
    /// the cluster's own words, not our paraphrase of them.
    pub last_error: String,
    /// RFC 3339, supplied by the caller — this module has no clock.
    pub updated_at: String,
    /// The schedule ran out with work outstanding. The record stays on the CR:
    /// giving up on the *timer* is not giving up on the *deployment*, and a
    /// user-initiated retry starts a fresh schedule.
    pub exhausted: bool,
}

impl Record {
    /// A fresh record for work that is about to start.
    pub fn started(now: &str) -> Self {
        Self {
            updated_at: now.to_string(),
            ..Self::default()
        }
    }

    /// Hand a fresh attempt schedule to an existing record: a save that defers
    /// new work, or a user who asked for a retry.
    ///
    /// `done` is deliberately KEPT. The applied set is a fact about the
    /// cluster, not about this run, and discarding it would make every save
    /// re-install every monitor. What is outstanding stays derived — a purpose
    /// change invalidates its own key, and a monitor ticked in the edit tab is
    /// simply not in `done` yet — so keeping the set can never hide new work.
    pub fn restart(&mut self, now: &str) {
        self.attempts = 0;
        self.exhausted = false;
        self.last_error.clear();
        self.updated_at = now.to_string();
    }

    /// Record one item as applied. Idempotent — re-applying an item that is
    /// already recorded must not grow the list, because the applier is allowed
    /// (and expected) to re-run items whose success it could not confirm.
    pub fn mark_done(&mut self, item: &Item, now: &str) {
        let key = item.key();
        if !self.done.contains(&key) {
            self.done.push(key);
        }
        self.updated_at = now.to_string();
    }

    /// Record an attempt that did not finish the work.
    pub fn mark_failed(&mut self, error: &str, now: &str) {
        self.attempts = self.attempts.saturating_add(1);
        self.last_error = error.to_string();
        self.updated_at = now.to_string();
    }

    /// Record that the schedule is spent.
    pub fn mark_exhausted(&mut self, now: &str) {
        self.exhausted = true;
        self.updated_at = now.to_string();
    }

    /// What is still owed, given the deployment's current configuration.
    pub fn pending(&self, purpose: &str, monitors: &[String]) -> Vec<Item> {
        plan(purpose, monitors)
            .into_iter()
            .filter(|i| !self.done.iter().any(|k| *k == i.key()))
            .collect()
    }
}

/// Read the record out of the annotation value.
///
/// A malformed value parses as "nothing has been applied", never as an error.
/// The record is a hint about work that is idempotent by construction: reading
/// it pessimistically re-applies upserts (harmless), while treating it as fatal
/// would strand a deployment on a corrupt string. `None` — no annotation — is
/// the same shape, but see [`state_from`], which distinguishes the two.
pub fn parse(value: Option<&str>) -> Record {
    value
        .map(|v| serde_json::from_str::<Record>(v).unwrap_or_default())
        .unwrap_or_default()
}

/// Render the record for the annotation. Always compact JSON on one line —
/// annotations are read by humans in `kubectl describe`.
pub fn render(record: &Record) -> String {
    // A struct of owned primitives cannot fail to serialize; the fallback keeps
    // the applier's write path infallible rather than adding an error arm that
    // can never be taken.
    serde_json::to_string(record).unwrap_or_else(|_| "{}".to_string())
}

// ──────────────────────────── the retry schedule ────────────────────────────

/// How long each attempt waits for the cluster to settle, in seconds.
///
/// The first attempt keeps the 600s that shipped with ADR-050, because the
/// common case is a create that settles in about five minutes and this change
/// must not make it slower or noisier. The later attempts widen: a cluster that
/// has not settled in ten minutes is blocked on something slow — a hung peer
/// recovery (the 2026-08-13 cause), an image pull, a PVC waiting for a node —
/// not on something that will fail fast, so polling it harder buys nothing.
///
/// Bounded on purpose. The alternative, an unbounded retry, is a background
/// task per deployment that never exits and never tells anyone; the record on
/// the CR is what makes giving up on the timer safe, because the outstanding
/// work stays visible and a user-initiated retry starts a fresh schedule.
pub const SETTLE_BUDGETS: &[u64] = &[600, 900, 1800, 1800, 1800];

/// Ceiling on the delay between attempts (seconds).
const MAX_RETRY_DELAY: u64 = 300;

/// The settle budget for attempt `n` (0-based), or `None` when the schedule is
/// spent.
pub fn settle_budget(attempt: u32) -> Option<u64> {
    SETTLE_BUDGETS.get(attempt as usize).copied()
}

/// How long to wait before attempt `n` (0-based), in seconds.
///
/// Zero before the first attempt — a create must start waiting immediately.
/// After that, exponential and capped. This is not throttling the *settle*
/// wait, which is its own budget above and dominates when the cluster is slow;
/// it throttles the case where the cluster settles fine and an item keeps
/// failing (OpenSearch reachable but refusing, the catalog registry down), so
/// a broken monitor does not spin through the whole schedule in seconds.
pub fn retry_delay(attempt: u32) -> u64 {
    if attempt == 0 {
        return 0;
    }
    // 30, 60, 120, 240, … capped.
    30u64
        .saturating_mul(1u64 << (attempt - 1).min(16))
        .min(MAX_RETRY_DELAY)
}

// ──────────────────────────── the user-facing state ────────────────────────

/// What the UI is told about deferred provisioning (ADR-052).
///
/// The point of this struct is that the answer travels in the DTO instead of
/// living only in a log line. The UI renders it and never re-derives it — the
/// same discipline as `activity` (ADR-050) and `upgrade_options.blocked_reason`
/// (ADR-048).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvisioningState {
    /// `complete` | `pending` | `failed`.
    pub state: &'static str,
    /// The purpose profile has not been applied yet.
    pub profile_pending: bool,
    /// Monitor ids the user selected that are not installed yet. Ids, not
    /// prose — the UI names them.
    pub monitors_pending: Vec<String>,
    pub attempts: u32,
    pub last_error: String,
    pub updated_at: String,
}

impl ProvisioningState {
    /// Nothing is outstanding.
    pub fn complete() -> Self {
        Self {
            state: "complete",
            ..Self::default()
        }
    }
}

/// Derive the reported state from the CR alone: the purpose label, the monitors
/// annotation, and the record annotation.
///
/// No annotation ⇒ `complete`. That is the terminal state the applier writes by
/// *removing* the record, and it is also every deployment that predates this
/// change — which is the reason absence means complete rather than unknown: the
/// alternative would flip the whole existing estate to "incomplete" on deploy.
pub fn state_from(
    purpose: &str,
    monitors: &[String],
    annotation: Option<&str>,
) -> ProvisioningState {
    let Some(raw) = annotation else {
        return ProvisioningState::complete();
    };
    let record = parse(Some(raw));
    let pending = record.pending(purpose, monitors);
    // A Dashboards default is cosmetic and never blocks: a deployment whose
    // profile and monitors have all landed is PROVISIONED, and reporting it as
    // "pending" over a UI preference — with nothing listed, because the DTO
    // does not surface these — would be a worse lie than saying complete.
    let blocking = pending
        .iter()
        .any(|i| !matches!(i, Item::DashboardsDefault(_)));
    if !blocking {
        // Also covers the empty case: a record with nothing outstanding is a
        // transient the applier clears on its way out; report it as what it is
        // rather than as pending work.
        return ProvisioningState::complete();
    }
    ProvisioningState {
        state: if record.exhausted {
            "failed"
        } else {
            "pending"
        },
        profile_pending: pending.iter().any(|i| matches!(i, Item::Profile(_))),
        monitors_pending: pending
            .iter()
            .filter_map(|i| match i {
                Item::Monitor(id) => Some(id.clone()),
                // Neither is a monitor. A Dashboards default is deliberately
                // not surfaced in the DTO: it is cosmetic, it never blocks, and
                // listing it would make a healthy deployment read as
                // "provisioning pending" in the UI over a UI preference.
                Item::Profile(_) | Item::DashboardsDefault(_) => None,
            })
            .collect(),
        attempts: record.attempts,
        last_error: record.last_error,
        updated_at: record.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Dashboards default is planned, applied once, and never re-planned.
    ///
    /// This is the whole reason it is an `Item`: "dark by default" has to mean
    /// a starting point, not a setting the next provisioning pass puts back.
    #[test]
    fn the_dashboards_default_is_planned_once_and_not_reasserted() {
        let p = plan("observability", &monitors(&["kubernetes"]));
        let item = Item::DashboardsDefault(DARK_MODE.to_string());
        assert!(p.contains(&item), "a new deployment starts dark: {p:?}");
        assert_eq!(
            p.last(),
            Some(&item),
            "it must come after the profile/monitor ordering, which is load-bearing"
        );

        // Once recorded, it is no longer owed — a later pass must not write it
        // again over whatever the user has since chosen.
        let mut rec = Record::default();
        rec.mark_done(&item, "2026-08-18T00:00:00Z");
        assert!(
            !rec.pending("observability", &monitors(&["kubernetes"]))
                .contains(&item),
            "a re-asserted default is not a default"
        );
    }

    /// It is cosmetic, so it must never make a provisioned deployment read as
    /// unfinished — the UI would show "pending" with nothing listed.
    #[test]
    fn a_pending_dashboards_default_alone_reads_as_complete() {
        let item = Item::DashboardsDefault(DARK_MODE.to_string());
        let mut rec = Record::default();
        rec.mark_done(
            &Item::Profile("observability".into()),
            "2026-08-18T00:00:00Z",
        );
        let raw = render(&rec);
        let st = state_from("observability", &[], Some(&raw));
        assert_eq!(
            st.state, "complete",
            "only a cosmetic default was outstanding"
        );
        assert!(!st.profile_pending);
        assert!(st.monitors_pending.is_empty());
        let _ = item;
    }

    /// The plan minus the cosmetic Dashboards default.
    ///
    /// The tests below are about the profile/monitor contract; the Dashboards
    /// default is asserted on its own in
    /// `the_dashboards_default_is_planned_once_and_not_reasserted`. Stripping it
    /// here keeps each test about its own subject instead of restating the whole
    /// plan every time one is added.
    fn work(p: Vec<Item>) -> Vec<Item> {
        p.into_iter()
            .filter(|i| !matches!(i, Item::DashboardsDefault(_)))
            .collect()
    }

    fn monitors(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// The ordering is a correctness constraint, not presentation: the profile
    /// installs the retention ISM policy whose `ism_template` attaches to
    /// indices created afterwards, and the monitors are what create them. If
    /// someone reorders `plan`, log indices silently escape retention.
    #[test]
    fn the_profile_is_planned_before_any_monitor() {
        let p = plan("observability", &monitors(&["kubernetes", "nginx"]));
        assert_eq!(p[0], Item::Profile("observability".into()));
        assert_eq!(p[1], Item::Monitor("kubernetes".into()));
        assert_eq!(p[2], Item::Monitor("nginx".into()));
    }

    /// The monitors annotation is a comma-joined string parsed back out of the
    /// CR, so blanks and repeats are reachable without anyone typing them.
    /// Planning the same monitor twice would install it twice.
    #[test]
    fn plan_drops_blank_and_repeated_monitor_ids() {
        let p = work(plan(
            "observability",
            &monitors(&["nginx", " nginx ", "", "kubernetes"]),
        ));
        assert_eq!(
            p,
            vec![
                Item::Profile("observability".into()),
                Item::Monitor("nginx".into()),
                Item::Monitor("kubernetes".into()),
            ]
        );
    }

    /// The whole point of tracking `done`: a second run must be a no-op rather
    /// than a re-install.
    #[test]
    fn applied_items_stop_being_pending() {
        let mut r = Record::started("t0");
        assert_eq!(
            work(r.pending("observability", &monitors(&["nginx"]))).len(),
            2
        );
        r.mark_done(&Item::Profile("observability".into()), "t1");
        r.mark_done(&Item::Monitor("nginx".into()), "t2");
        assert!(work(r.pending("observability", &monitors(&["nginx"]))).is_empty());
    }

    /// The applier re-runs items it could not confirm, and `mark_done` is
    /// reached again on the second success. The record must not grow without
    /// bound — it lives in an annotation, which has a size limit.
    #[test]
    fn marking_the_same_item_done_twice_is_idempotent() {
        let mut r = Record::started("t0");
        r.mark_done(&Item::Monitor("nginx".into()), "t1");
        r.mark_done(&Item::Monitor("nginx".into()), "t2");
        assert_eq!(r.done, vec!["monitor:nginx".to_string()]);
        assert_eq!(r.updated_at, "t2");
    }

    /// `save_cluster` can change the purpose of a live deployment, and that is
    /// precisely when the profile must run again (retention windows move; a
    /// switch to `search` must strip the agents). Keying the record on the
    /// purpose makes the change invalidate itself.
    #[test]
    fn changing_the_purpose_makes_the_profile_pending_again() {
        let mut r = Record::started("t0");
        r.mark_done(&Item::Profile("observability".into()), "t1");
        assert!(work(r.pending("observability", &[])).is_empty());
        assert_eq!(
            work(r.pending("search", &[])),
            vec![Item::Profile("search".into())]
        );
    }

    /// Removing a monitor in the edit tab while a retry is pending must not
    /// leave the applier chasing an item the user withdrew. The plan is derived
    /// from the CR every time, never from a copy taken at create.
    #[test]
    fn deselecting_a_monitor_withdraws_it_from_the_pending_work() {
        let r = Record::started("t0");
        assert_eq!(
            work(r.pending("observability", &monitors(&["nginx"]))).len(),
            2
        );
        let pending = work(r.pending("observability", &[]));
        assert_eq!(pending, vec![Item::Profile("observability".into())]);
    }

    #[test]
    fn a_record_round_trips_through_the_annotation() {
        let mut r = Record::started("t0");
        r.mark_done(&Item::Monitor("kubernetes".into()), "t1");
        r.mark_failed("did not settle", "t2");
        r.mark_exhausted("t3");
        assert_eq!(parse(Some(&render(&r))), r);
    }

    /// A record is a hint about work that is idempotent anyway, so a value we
    /// cannot parse must degrade to "re-apply everything" — never to an error
    /// that strands the deployment. Reachable: anything with write access to
    /// the CR can put a string there.
    #[test]
    fn a_corrupt_record_reads_as_nothing_applied_rather_than_failing() {
        let r = parse(Some("{not json"));
        assert_eq!(r, Record::default());
        assert_eq!(
            work(r.pending("observability", &monitors(&["nginx"]))).len(),
            2
        );
    }

    /// Absence is the terminal state, and it is also every deployment created
    /// before ADR-052. If this ever returns `pending`, deploying this change
    /// reports the entire existing estate as incomplete.
    #[test]
    fn a_deployment_without_the_annotation_is_complete() {
        let s = state_from("observability", &monitors(&["nginx", "kubernetes"]), None);
        assert_eq!(s.state, "complete");
        assert!(!s.profile_pending);
        assert!(s.monitors_pending.is_empty());
    }

    /// The defect this module exists for, expressed as the contract the UI
    /// consumes: after the wait timed out, the deployment must report *which*
    /// configuration is missing and why — not look finished.
    #[test]
    fn an_unapplied_deployment_reports_what_is_missing_and_why() {
        let mut r = Record::started("t0");
        r.mark_failed(
            "deployment tornis-hj27 did not settle within 600s (stuck at nodes (25%))",
            "t1",
        );
        let s = state_from(
            "observability",
            &monitors(&["kubernetes", "nginx"]),
            Some(&render(&r)),
        );
        assert_eq!(s.state, "pending");
        assert!(s.profile_pending);
        assert_eq!(s.monitors_pending, monitors(&["kubernetes", "nginx"]));
        assert_eq!(s.attempts, 1);
        assert!(s.last_error.contains("did not settle"));
    }

    /// A partial application is the common shape of a failure here (the profile
    /// lands, a catalog install 502s), and reporting it as all-or-nothing would
    /// send the user looking for the wrong thing.
    #[test]
    fn a_partial_application_reports_only_the_remainder() {
        let mut r = Record::started("t0");
        r.mark_done(&Item::Profile("observability".into()), "t1");
        r.mark_done(&Item::Monitor("kubernetes".into()), "t2");
        r.mark_failed("catalog registry unreachable", "t3");
        let s = state_from(
            "observability",
            &monitors(&["kubernetes", "nginx"]),
            Some(&render(&r)),
        );
        assert_eq!(s.state, "pending");
        assert!(!s.profile_pending);
        assert_eq!(s.monitors_pending, monitors(&["nginx"]));
    }

    /// `failed` and `pending` must be distinguishable, because they call for
    /// different things from the user: `pending` means a retry is still coming
    /// on its own, `failed` means nothing further will happen unless they ask.
    #[test]
    fn an_exhausted_schedule_reports_failed_not_pending() {
        let mut r = Record::started("t0");
        r.mark_failed("did not settle", "t1");
        r.mark_exhausted("t2");
        let s = state_from("observability", &monitors(&["nginx"]), Some(&render(&r)));
        assert_eq!(s.state, "failed");
    }

    /// The applier clears the annotation when it finishes, but the clearing
    /// patch can fail on its own. A record with nothing outstanding must not
    /// leave a healthy deployment flagged forever.
    #[test]
    fn a_record_with_nothing_outstanding_is_complete() {
        let mut r = Record::started("t0");
        r.mark_done(&Item::Profile("observability".into()), "t1");
        r.mark_done(&Item::Monitor("nginx".into()), "t2");
        let s = state_from("observability", &monitors(&["nginx"]), Some(&render(&r)));
        assert_eq!(s.state, "complete");
    }

    /// A retry must actually retry — an exhausted record whose counter survived
    /// would make the button do nothing, which is worse than no button.
    #[test]
    fn restarting_clears_the_schedule_but_keeps_what_was_applied() {
        let mut r = Record::started("t0");
        r.mark_done(&Item::Monitor("kubernetes".into()), "t1");
        r.mark_failed("catalog registry unreachable", "t2");
        r.mark_exhausted("t3");
        r.restart("t4");
        assert_eq!(r.attempts, 0);
        assert!(!r.exhausted);
        assert!(r.last_error.is_empty());
        assert_eq!(r.done, vec!["monitor:kubernetes".to_string()]);
        assert!(
            settle_budget(r.attempts).is_some(),
            "a retry has attempts to spend"
        );
    }

    /// The edit tab writes the monitors annotation on save. Before ADR-052 the
    /// save path applied only the profile, so a monitor ticked there was
    /// recorded on the CR and never installed — the same silent skip by a
    /// different door. Restarting the record must leave it pending.
    #[test]
    fn a_monitor_added_by_a_save_is_pending_after_a_restart() {
        let mut r = Record::started("t0");
        r.mark_done(&Item::Profile("observability".into()), "t1");
        r.mark_done(&Item::Monitor("nginx".into()), "t2");
        r.restart("t3");
        assert_eq!(
            work(r.pending("observability", &monitors(&["nginx", "kubernetes"]))),
            vec![Item::Monitor("kubernetes".into())]
        );
    }

    /// The bound is the promise: a stuck deployment retries for roughly two
    /// hours and then stops on its own. If someone widens the schedule, this
    /// says so out loud rather than letting background tasks accumulate.
    #[test]
    fn the_settle_schedule_is_bounded_and_widens() {
        assert_eq!(
            settle_budget(0),
            Some(600),
            "the first attempt keeps ADR-050's 600s"
        );
        assert!(
            SETTLE_BUDGETS.windows(2).all(|w| w[0] <= w[1]),
            "budgets never shrink"
        );
        assert_eq!(settle_budget(SETTLE_BUDGETS.len() as u32), None);
        let total: u64 = SETTLE_BUDGETS.iter().sum();
        assert_eq!(total, 6900, "≈1h55m of settle waiting before giving up");
    }

    /// The 2026-08-13 case: the cluster settled well past 600s. The first
    /// attempt's budget must not be the whole story.
    #[test]
    fn a_cluster_that_settles_at_minute_eleven_still_has_attempts_left() {
        assert!(
            settle_budget(1).is_some(),
            "a second attempt must exist, or minute 11 is still a permanent skip"
        );
        assert!(settle_budget(0).unwrap() + settle_budget(1).unwrap() > 660);
    }

    /// A create must start waiting at once; a failing item must not spin.
    #[test]
    fn the_retry_delay_is_zero_first_then_backs_off_under_a_cap() {
        assert_eq!(retry_delay(0), 0);
        assert_eq!(retry_delay(1), 30);
        assert_eq!(retry_delay(2), 60);
        assert!(
            (0..1000).all(|n| retry_delay(n) <= MAX_RETRY_DELAY),
            "no overflow, capped"
        );
    }
}
