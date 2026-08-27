// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! What a deployment is doing right now, and whether it has settled (ADR-050).
//!
//! Pure: no cluster calls, no I/O. `k8s.rs` gathers the signals and `api.rs`
//! hands the verdict to the UI, which renders it and never re-derives it.
//!
//! The module exists because of one observed failure. On 2026-08-07, during the
//! ADR-049 conformance run, a cluster reported **`health: green` with 2 of 3
//! nodes ready**: the operator restarts one node at a time, and between two
//! restarts every pod is briefly up. Everything that keyed off `health ==
//! "green"` — the provisioning panel, `wait_green`, the test harness — believed
//! it and moved on while the roll still had nodes to go.
//!
//! The signal that distinguishes the two cases was already being fetched and
//! never used for this: the StatefulSet's `updatedReplicas`. During a roll it
//! climbs 0→N while `readyReplicas` oscillates, so there is always a window
//! where ready equals desired and updated does not. Comparing those two needs
//! no timer, no debounce and nothing kept in memory — which is also ADR-048's
//! rule: a deployment's state must be readable from the cluster alone, so it
//! survives a backend restart.
//!
//! # A second observed failure: the 16-hour "25%" (issue #131)
//!
//! A production deployment sat on `RESTARTING THE NODES — Starting the nodes
//! 0/3 — 25%` for **sixteen hours**, and the screen disagreed with itself while
//! it did: the NODES tile read `3/3 — all ready`, the progress counter read
//! `6/3` and then `8/3` (more than the total), the elapsed clock kept resetting
//! to zero, and documents kept climbing the whole time.
//!
//! Everything needed to explain it was already being read or one cheap call
//! away. The operator was looping
//! `Couldn't proceed with rolling restart for Pod …-nodes-0 because waiting for
//! health to be green` every 11 seconds, because a peer recovery of
//! `.opendistro_security` had been stuck in `init` at 0.0% for 15.9h with 7
//! replicas unassigned behind it. Primaries were fine — which is exactly why
//! every tile looked healthy.
//!
//! Three rules follow, and they are what the rest of this module now encodes:
//!
//! 1. **A verdict that stops moving must say why.** Past
//!    [`STALL_AFTER_SECS`] without progress the activity is `stalled` and
//!    carries [`Blocked`] — health colour, unassigned shards, the operator's
//!    pending component, and the oldest active recovery with its stage and age.
//!    Facts, not a sentence: the SPA composes the wording in the user's
//!    language (ADR-048's no-prose-over-the-wire rule).
//! 2. **One pair of numbers, clamped.** Every node counter on the screen comes
//!    from [`Activity::nodes_ready`]/[`Activity::nodes_total`] and the sub-progress
//!    counter can never exceed its total. `6/3` was two different objects — the
//!    StatefulSet's live count over the CR spec's request — divided by each
//!    other with nothing holding them together.
//! 3. **Working and converged are different claims.** [`Activity::serving`]
//!    says the first, `settled` says the second. Both were true statements
//!    about that deployment; the UI could make neither.

use serde::Serialize;

use crate::upgrade::UpgradeState;

// ───────────────────────────── stages ─────────────────────────────

/// The provisioning ladder, in order. `percent` is a function of the furthest
/// stage reached plus sub-progress inside it, so it cannot walk backwards
/// (ADR-050 invariant 2).
///
/// The weights are deliberately uneven: the node stage is the widest because it
/// is where most of the wall-clock goes, and the early stages are narrow but
/// *named*, which is the point — a user watching "Provisionando volumes" for
/// three minutes knows something the old `0%` bar could not tell them.
const LADDER: &[(&str, u8)] = &[
    ("storage", 5),
    ("accepted", 10),
    ("volumes", 25),
    ("nodes", 70),
    ("security", 85),
    ("dashboards", 95),
    ("settling", 100),
];

/// Where the `nodes` stage begins — the floor its sub-progress interpolates
/// from. Kept derived rather than written twice.
fn stage_floor(stage: &str) -> u8 {
    let mut floor = 0;
    for (name, ceiling) in LADDER {
        if *name == stage {
            return floor;
        }
        floor = *ceiling;
    }
    floor
}

fn stage_ceiling(stage: &str) -> u8 {
    LADDER
        .iter()
        .find(|(name, _)| *name == stage)
        .map(|(_, c)| *c)
        .unwrap_or(100)
}

/// Interpolate inside a stage. `sub` is clamped to 0.0..=1.0, and the result
/// never reaches the stage's ceiling — reaching the ceiling is what *entering
/// the next stage* means.
fn percent_for(stage: &str, sub: f32) -> u8 {
    let floor = stage_floor(stage) as f32;
    let ceiling = stage_ceiling(stage) as f32;
    let sub = sub.clamp(0.0, 1.0);
    (floor + (ceiling - floor) * sub).round() as u8
}

/// How long a deployment may show no progress before the panel stops guessing
/// and starts explaining (issue #131).
///
/// Four minutes is not a new number: it is the threshold ADR-050 already chose
/// for switching the panel to its troubleshoot wording. What changes is *who
/// owns it*. It was a constant in `views_activity.jsx` compared against a clock
/// the client started itself, so it measured "how long this component has been
/// mounted" — which a tab switch reset. Here it is compared against a duration
/// read off the cluster, and the client renders the verdict (invariant 3).
pub const STALL_AFTER_SECS: i64 = 240;

// ───────────────────────────── input ─────────────────────────────

/// The cluster's own account of why it is not converging: `_cluster/health` and
/// `_recovery?active_only=true`, gathered by `k8s.rs`.
///
/// Only fetched for a deployment that is ALREADY stalled — `k8s.rs` evaluates
/// once without it and asks OpenSearch only when that first verdict says it is
/// worth asking. A settled deployment therefore still costs exactly what it
/// cost before (invariant 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterBlock {
    /// `unassigned_shards` from `_cluster/health`.
    pub unassigned_shards: i32,
    /// The longest-running ACTIVE recovery, which is the one holding green
    /// back: in the incident, `.opendistro_security` shard 0, stage `init`,
    /// 0.0% transferred, 15.9 hours old.
    pub recovery_index: String,
    pub recovery_stage: String,
    pub recovery_secs: i64,
    /// The pod a previous stall-remediation pass bounced (#27) — set only
    /// after the bounce happened, so the panel can state a fact, not an
    /// intention.
    pub remediated_node: Option<String>,
}

impl Default for ClusterBlock {
    /// `-1`, not `0`. "We did not get an answer" and "there are zero unassigned
    /// shards" are opposite facts and the UI has to be able to tell them apart
    /// — printing "0 unassigned shards" because a call timed out is exactly the
    /// kind of confident wrongness this module exists to stop.
    fn default() -> Self {
        Self {
            unassigned_shards: -1,
            recovery_index: String::new(),
            recovery_stage: String::new(),
            recovery_secs: 0,
            remediated_node: None,
        }
    }
}

/// Why a stalled deployment is stuck — structured facts, never a sentence.
///
/// The SPA turns these into words in the user's language; the wire carries no
/// prose except the cluster's own vocabulary (`yellow`, `RollingRestart`,
/// `init`, an index name), which is reproduced verbatim for the same reason
/// `upgrade.reason` and `snapshot.last_error` are (ADR-045 UI rule 5).
///
/// Every field is independently optional: the Kubernetes half (health,
/// component) is free and always present, the OpenSearch half (shards,
/// recovery) is absent when the cluster did not answer, and the UI says only
/// what it actually knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocked {
    /// Cluster health colour, verbatim from the CR ("yellow").
    pub health: String,
    /// Shards with no home. `-1` = unknown (see [`ClusterBlock::default`]).
    pub unassigned_shards: i32,
    /// The first `componentsStatus` row the operator has NOT finished, which is
    /// the operator naming its own pending work ("RollingRestart"/"Running").
    pub component: String,
    pub component_status: String,
    /// The oldest active shard recovery, when the cluster reported one.
    pub recovery_index: String,
    pub recovery_stage: String,
    pub recovery_secs: i64,
    /// Set when the app already bounced the wedged node itself (#27); the
    /// UI states what was done while the stall persists.
    pub remediated_node: Option<String>,
}

impl Default for Blocked {
    fn default() -> Self {
        let c = ClusterBlock::default();
        Self {
            health: String::new(),
            unassigned_shards: c.unassigned_shards,
            component: String::new(),
            component_status: String::new(),
            recovery_index: c.recovery_index,
            recovery_stage: c.recovery_stage,
            recovery_secs: c.recovery_secs,
            remediated_node: None,
        }
    }
}

/// Everything `k8s.rs` gathered about a deployment, in one struct so the rules
/// below are a pure function of it.
#[derive(Debug, Clone, Default)]
pub struct ActivityInput {
    /// `.status.phase` on the CR ("PENDING" / "RUNNING").
    pub phase: String,
    /// `.status.health` — the field this module exists to stop trusting alone.
    pub health: String,
    /// `.status.initialized` — the operator's own "security bootstrap done".
    /// Declared in the CRD since the beginning and never read until now.
    pub initialized: bool,
    /// StatefulSet `readyReplicas`.
    pub nodes_ready: i32,
    /// How many nodes the deployment is SUPPOSED to have —
    /// `spec.nodePools[0].replicas`, the number the user chose.
    ///
    /// Deliberately not the StatefulSet's `.status.replicas`: the operator
    /// creates the StatefulSet with **one** replica and scales it up, so during
    /// a create that field reports 1 while three nodes were asked for
    /// (measured live 2026-08-08). Comparing `ready` against it would declare a
    /// one-node cluster settled — the same false-ready this module exists to
    /// prevent, arriving by a different road.
    pub nodes_desired: i32,
    /// Pods already on the current StatefulSet revision. **The signal that
    /// distinguishes a settled cluster from a rolling one.** Compared against
    /// `nodes_desired` above, so a StatefulSet that has not been scaled to full
    /// size yet can never satisfy it either.
    pub nodes_updated: i32,
    /// Data PVCs in `Bound`, and how many exist.
    pub pvcs_bound: i32,
    pub pvcs_total: i32,
    /// The `<name>-dashboards` Deployment has at least one ready replica.
    pub dashboards_ready: bool,
    /// Reduced upgrade state (ADR-048) — an upgrade in flight is an activity.
    pub upgrade: UpgradeState,
    /// `.status.componentsStatus[]` as `(component, status)` pairs. An input,
    /// not the answer: a healthy cluster was observed reporting a single row
    /// (`RollingRestart: Finished`), so this can confirm work is happening but
    /// cannot prove none is (ADR-050 option D).
    pub components: Vec<(String, String)>,
    /// **Seconds since anything in the node pool last moved**, measured from
    /// the cluster: the newest node pod's `creationTimestamp`, falling back to
    /// the CR's when no pod exists yet. `0` = unknown, and an unknown age never
    /// produces a stall claim.
    ///
    /// Deliberately not "seconds since this stage was entered", which would
    /// need remembered state (invariant 6). A roll advances by replacing a pod,
    /// so the newest pod's age IS the time since the last real advance — and it
    /// is immune to the two things that corrupted the old clock: an operator
    /// reconcile (writes CR status, creates no pod) and a tab switch (remounts
    /// the component, touches nothing in the cluster).
    pub since_secs: i64,
    /// What OpenSearch says about the block, when it was worth asking. See
    /// [`ClusterBlock`].
    pub cluster: Option<ClusterBlock>,
}

/// `componentsStatus` values that mean "this reconciler is done". Anything else
/// present means something is still moving.
///
/// Public because `k8s.rs`'s short-circuit has to test the same clause it is
/// standing in for — the fast path must be strictly stronger than the predicate
/// (ADR-050 invariant 5), which it cannot be if it re-spells the rule.
pub fn component_is_terminal(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "finished" | "upgraded" | "completed" | "done" | ""
    )
}

// ───────────────────────────── verdict ─────────────────────────────

/// What the UI renders. Every field is an answer, never an input to a rule the
/// client re-runs (ADR-050 invariant 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Activity {
    /// `idle` | `creating` | `upgrading` | `restarting`.
    pub kind: &'static str,
    /// The ladder rung: `storage`|`accepted`|`volumes`|`nodes`|`security`|
    /// `dashboards`|`settling`, or `ready` when idle.
    pub stage: &'static str,
    pub percent: u8,
    /// Sub-progress in words, already interpolated ("2/3"). Empty when the
    /// stage has no meaningful sub-progress.
    pub detail: String,
    /// **The predicate.** Everything else on this struct is presentation.
    pub settled: bool,
    /// Whether mutating controls for THIS deployment should refuse. Distinct
    /// from `!settled` on purpose: a future activity might be worth showing
    /// without locking anything.
    pub locks_edits: bool,
    /// **The one node pair on the screen.** Nodes ready, and how many the user
    /// asked for. Every counter the SPA renders — the NODES tile, the panel's
    /// meta line, the stage sub-progress — is derived from these two, so they
    /// cannot disagree with each other the way `3/3 all ready` disagreed with
    /// `Starting the nodes 0/3` (issue #131).
    ///
    /// `nodes_ready` is clamped to `nodes_total`: a node pool larger than the
    /// spec asks for is a real state (the operator will not scale down while
    /// health is not green), but "6 of 3 ready" is not a sentence about it.
    pub nodes_ready: i32,
    pub nodes_total: i32,
    /// Seconds since the node pool last moved — see [`ActivityInput::since_secs`].
    /// The panel's clock renders this instead of counting from its own mount.
    pub since_secs: i64,
    /// Nothing has advanced for [`STALL_AFTER_SECS`]. Stronger than "slow": a
    /// stalled activity is expected to carry a `blocked` that names the cause.
    pub stalled: bool,
    /// **The deployment is doing its job**, whatever else is unfinished:
    /// primaries up, the cluster answering, data flowing. Orthogonal to
    /// `settled`, and both were true at once in the incident. The panel says
    /// both, because saying only "25%" made a working cluster look dead.
    pub serving: bool,
    /// Why it is stuck, when it is. All-empty (and `unassigned_shards: -1`)
    /// when the deployment is not stalled — the SPA reads `stalled` first.
    pub blocked: Blocked,
}

impl Default for Activity {
    fn default() -> Self {
        Self::idle()
    }
}

impl Activity {
    /// The settled deployment: nothing in flight, nothing locked.
    pub fn idle() -> Self {
        Self {
            kind: "idle",
            stage: "ready",
            percent: 100,
            detail: String::new(),
            settled: true,
            locks_edits: false,
            nodes_ready: 0,
            nodes_total: 0,
            since_secs: 0,
            stalled: false,
            serving: true,
            blocked: Blocked::default(),
        }
    }
}

/// Has this deployment finished everything it was doing?
///
/// Every clause is here because dropping it lets a real in-flight state read as
/// finished:
///   * `health == green` — necessary, and historically mistaken for sufficient.
///   * `nodes_ready == nodes_desired` — obvious, and still not enough.
///   * **`nodes_updated == nodes_desired`** — the roll has no pending revision.
///     This is the clause the 2026-08-07 false green needed (invariant 1).
///   * `initialized` — the operator finished the security bootstrap; before
///     that the cluster answers but rejects our credentials.
///   * `dashboards_ready` — the deployment includes its Dashboards; a cluster
///     whose UI is down is not done coming up.
///   * no upgrade in flight, no non-terminal component.
pub fn settled_of(i: &ActivityInput) -> bool {
    i.health == "green"
        && i.nodes_desired > 0
        && i.nodes_ready == i.nodes_desired
        && i.nodes_updated == i.nodes_desired
        && i.initialized
        && i.dashboards_ready
        && !i.upgrade.in_flight()
        && !i
            .components
            .iter()
            .any(|(_, status)| !component_is_terminal(status))
}

/// The node pair every counter on the screen is derived from: ready, and the
/// number the user asked for.
///
/// The clamp is the fix for `6/3` (issue #131). The two numbers come from two
/// different objects — `readyReplicas` off the live StatefulSet, the total off
/// `spec.nodePools[0].replicas` on the CR — and nothing in Kubernetes keeps
/// them in step. They diverge for a whole legitimate reason: the operator
/// refuses to reconcile the node pool while health is not green, so a
/// deployment that was scaled down (or is mid-roll on a cluster that will not
/// go green) keeps more pods alive than the spec asks for, indefinitely. That
/// state deserves to be *shown* — as `3/3` plus a stall reason — not printed
/// as a fraction greater than one.
fn nodes_of(i: &ActivityInput) -> (i32, i32) {
    let total = i.nodes_desired.max(0);
    (i.nodes_ready.clamp(0, total), total)
}

/// Is the deployment doing its job right now, whatever has not finished?
///
/// Written down because the incident was BOTH: every primary up, ingestion
/// live, documents climbing from 2,005 to 6,581 — and a roll that had not moved
/// since the day before. A UI that can only say "25%" turns a working cluster
/// into an apparent outage, and a UI that only says "all ready" hides sixteen
/// hours of stuck convergence. The two claims are separate fields for that
/// reason.
///
/// `red` is excluded because red means a primary is missing — the one case
/// where the data really is not all there.
fn serving_of(i: &ActivityInput) -> bool {
    i.initialized
        && i.nodes_desired > 0
        && i.nodes_ready >= i.nodes_desired
        && matches!(i.health.as_str(), "green" | "yellow")
}

/// What the cluster and the operator say is holding this deployment open.
///
/// Assembled from whatever is available, in two independent halves: the
/// Kubernetes half (health colour, the operator's first unfinished component)
/// costs nothing and is always present; the OpenSearch half (unassigned shards,
/// the oldest recovery) is present only when `k8s.rs` decided the deployment
/// was stalled enough to be worth two HTTP calls, and stays at its "unknown"
/// default otherwise.
fn blocked_of(i: &ActivityInput) -> Blocked {
    // The operator naming its own pending work. `RollingRestart: Running` was
    // present for the whole 16 hours and nothing rendered it.
    let (component, component_status) = i
        .components
        .iter()
        .find(|(_, status)| !component_is_terminal(status))
        .map(|(c, s)| (c.clone(), s.clone()))
        .unwrap_or_default();
    let c = i.cluster.clone().unwrap_or_default();
    Blocked {
        health: i.health.clone(),
        unassigned_shards: c.unassigned_shards,
        component,
        component_status,
        recovery_index: c.recovery_index,
        recovery_stage: c.recovery_stage,
        recovery_secs: c.recovery_secs,
        remediated_node: c.remediated_node,
    }
}

/// The furthest ladder rung this deployment has reached, and its sub-progress.
fn stage_of(i: &ActivityInput) -> (&'static str, f32, String) {
    // Nothing about the node pool is known yet: the StatefulSet does not exist,
    // which on this product means the storage gate or the CR apply is still in
    // progress (`create_cluster` installs Longhorn before applying the CR).
    if i.nodes_desired == 0 {
        return if i.phase.is_empty() {
            ("storage", 0.0, String::new())
        } else {
            ("accepted", 0.0, String::new())
        };
    }

    // Volumes: PVCs are claimed with the StatefulSet, and a pod cannot start
    // before its own volume binds. This is the stage that used to read 0%.
    if i.pvcs_total > 0 && i.pvcs_bound < i.pvcs_total {
        return (
            "volumes",
            i.pvcs_bound as f32 / i.pvcs_total as f32,
            format!("{}/{}", i.pvcs_bound, i.pvcs_total),
        );
    }

    // Nodes: on a create this is pods becoming ready; on a roll it is pods
    // reaching the new revision, which is the "nó N de 3" ADR-048 already
    // shows. Same rung, different numerator, because it is the same wait.
    let (_, total) = nodes_of(i);
    let rolling = i.initialized && i.nodes_updated < i.nodes_desired;
    // Clamped for the same reason `nodes_of` clamps: `updatedReplicas` and
    // `readyReplicas` are counts of live pods, `total` is what the CR asks for,
    // and a stuck operator lets the first outgrow the second (issue #131).
    let done = if rolling {
        i.nodes_updated.clamp(0, total)
    } else {
        i.nodes_ready.clamp(0, total)
    };
    if done < total {
        return (
            "nodes",
            done as f32 / total as f32,
            format!("{done}/{total}"),
        );
    }

    if !i.initialized {
        return ("security", 0.0, String::new());
    }
    if !i.dashboards_ready {
        return ("dashboards", 0.0, String::new());
    }
    // Everything is up; what remains is the cluster reporting green and any
    // reconciler still finishing.
    ("settling", 0.0, String::new())
}

/// Classify the activity without keeping any state.
///
/// `initialized` is the pivot: it flips exactly once, when the operator
/// finishes the security bootstrap, so before it the deployment is being
/// created and after it any unsettledness is a restart of some kind. A cluster
/// whose `initialized` genuinely regressed would read as `creating` again —
/// which is an accurate description of what is happening to it.
fn kind_of(i: &ActivityInput, settled: bool) -> &'static str {
    if settled {
        return "idle";
    }
    if !i.initialized {
        return "creating";
    }
    if i.upgrade.in_flight() {
        return "upgrading";
    }
    "restarting"
}

/// The whole verdict, from the gathered signals.
pub fn evaluate(i: &ActivityInput) -> Activity {
    let (nodes_ready, nodes_total) = nodes_of(i);
    let settled = settled_of(i);
    if settled {
        return Activity {
            nodes_ready,
            nodes_total,
            since_secs: i.since_secs,
            ..Activity::idle()
        };
    }
    let kind = kind_of(i, settled);
    let (stage, sub, detail) = stage_of(i);
    let percent = percent_for(stage, sub);
    // A stall is a claim about the world, so it needs a duration read off the
    // world. `since_secs == 0` means we could not measure one (no pod, no CR
    // timestamp), and an unmeasured deployment is never accused of stalling.
    let stalled = i.since_secs >= STALL_AFTER_SECS;
    Activity {
        kind,
        stage,
        // Invariant 2: an unsettled deployment never shows 100%.
        percent: percent.min(99),
        detail,
        settled,
        // Everything that is not settled involves nodes coming or going, which
        // is exactly when a configuration write would race a restart.
        locks_edits: true,
        nodes_ready,
        nodes_total,
        since_secs: i.since_secs,
        stalled,
        serving: serving_of(i),
        // Only carried while stalled: before the threshold the panel's job is
        // to say "this is normal, wait", and dressing an ordinary 40-second
        // node start in a diagnosis would teach the user to ignore it.
        blocked: if stalled {
            blocked_of(i)
        } else {
            Blocked::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A healthy, finished deployment.
    fn steady() -> ActivityInput {
        ActivityInput {
            phase: "RUNNING".into(),
            health: "green".into(),
            initialized: true,
            nodes_ready: 3,
            nodes_desired: 3,
            nodes_updated: 3,
            pvcs_bound: 3,
            pvcs_total: 3,
            dashboards_ready: true,
            upgrade: UpgradeState::Idle,
            components: vec![("RollingRestart".into(), "Finished".into())],
            since_secs: 30,
            cluster: None,
        }
    }

    /// The production deployment of issue #131, at hour sixteen: the operator
    /// looping `Couldn't proceed with rolling restart … waiting for health to
    /// be green`, a peer recovery of `.opendistro_security` stuck in `init` at
    /// 0.0% for 15.9h, 7 replicas unassigned behind it — and every primary up,
    /// serving, ingesting.
    fn stuck_roll() -> ActivityInput {
        ActivityInput {
            health: "yellow".into(),
            // Every pod ready…
            nodes_ready: 3,
            nodes_desired: 3,
            // …and not one of them on the new revision, for 16 hours.
            nodes_updated: 0,
            since_secs: 57_240,
            components: vec![("RollingRestart".into(), "Running".into())],
            cluster: Some(ClusterBlock {
                unassigned_shards: 7,
                recovery_index: ".opendistro_security".into(),
                recovery_stage: "init".into(),
                recovery_secs: 57_240,
                remediated_node: None,
            }),
            ..steady()
        }
    }

    #[test]
    fn a_steady_deployment_is_idle() {
        let a = evaluate(&steady());
        assert!(a.settled);
        assert_eq!(a.kind, "idle");
        assert_eq!(a.percent, 100);
        assert!(!a.locks_edits);
    }

    /// ADR-050 invariant 1 — THE regression test.
    ///
    /// Observed live on 2026-08-07: the operator rolls one node at a time, so
    /// between two restarts every pod is up and the cluster reports green while
    /// the roll still has nodes to go. Anything keyed on health alone believes
    /// it. If someone ever simplifies `settled_of` back to health, this fails.
    #[test]
    fn a_rolling_restart_is_never_settled() {
        let mid_roll = ActivityInput {
            // Every pod is up and the cluster says green...
            health: "green".into(),
            nodes_ready: 3,
            nodes_desired: 3,
            // ...but only one of them is on the new revision.
            nodes_updated: 1,
            ..steady()
        };
        let a = evaluate(&mid_roll);
        assert!(
            !a.settled,
            "green with ready==desired but updated<desired is a rolling restart, not a settled cluster"
        );
        assert_eq!(a.kind, "restarting");
        assert_eq!(a.stage, "nodes");
        assert_eq!(
            a.detail, "1/3",
            "the roll counts updated pods, not ready ones"
        );
        assert!(a.locks_edits);
    }

    /// The second false-ready, found live on 2026-08-08 while checking the node
    /// tile: the operator creates the StatefulSet with ONE replica and scales
    /// it up, so `.status.replicas` reports 1 for a 3-node cluster during a
    /// create. A one-node OpenSearch answers green happily. If `nodes_desired`
    /// were the StatefulSet's count, everything here would line up at 1/1/1 and
    /// the deployment would be declared finished with two nodes missing.
    ///
    /// `nodes_desired` is therefore the SPEC's replica count, and `k8s.rs`
    /// feeds it from `spec.nodePools[0].replicas`.
    #[test]
    fn a_statefulset_scaled_only_partway_is_never_settled() {
        let scaling_up = ActivityInput {
            health: "green".into(),
            // The user asked for three...
            nodes_desired: 3,
            // ...and exactly one exists, is ready, and is current.
            nodes_ready: 1,
            nodes_updated: 1,
            ..steady()
        };
        let a = evaluate(&scaling_up);
        assert!(
            !a.settled,
            "1 of 3 nodes cannot be settled — the StatefulSet is still being scaled up"
        );
        assert_eq!(a.stage, "nodes");
        assert_eq!(
            a.detail, "1/3",
            "progress counts toward the requested size, not the current one"
        );
    }

    #[test]
    fn security_bootstrap_and_dashboards_both_hold_it_open() {
        let no_security = ActivityInput {
            initialized: false,
            ..steady()
        };
        let a = evaluate(&no_security);
        assert!(!a.settled);
        assert_eq!(a.kind, "creating");
        assert_eq!(a.stage, "security");

        let no_dashboards = ActivityInput {
            dashboards_ready: false,
            ..steady()
        };
        let a = evaluate(&no_dashboards);
        assert!(!a.settled);
        assert_eq!(a.stage, "dashboards");
    }

    #[test]
    fn an_unfinished_component_holds_it_open() {
        let restarting = ActivityInput {
            components: vec![("RollingRestart".into(), "Running".into())],
            ..steady()
        };
        assert!(!evaluate(&restarting).settled);

        // Terminal vocabularies the operator actually uses.
        for done in ["Finished", "Upgraded", "finished", ""] {
            let i = ActivityInput {
                components: vec![("Upgrader".into(), done.into())],
                ..steady()
            };
            assert!(evaluate(&i).settled, "`{done}` should count as terminal");
        }
    }

    #[test]
    fn an_upgrade_in_flight_is_an_activity() {
        let upgrading = ActivityInput {
            upgrade: UpgradeState::Upgrading {
                pool: "nodes".into(),
                from: "3.7.0".into(),
                to: "3.8.0".into(),
            },
            nodes_updated: 2,
            ..steady()
        };
        let a = evaluate(&upgrading);
        assert!(!a.settled);
        assert_eq!(a.kind, "upgrading");
        assert!(a.locks_edits);
    }

    /// ADR-050 invariant 2: the ladder only ever climbs, tops out at 100, and
    /// never reads 100 while unsettled.
    #[test]
    fn percent_is_monotonic_across_a_provisioning_sequence() {
        // A create, in the order it really happens.
        let seq: Vec<ActivityInput> = vec![
            // 1. storage gate: no CR status, no StatefulSet yet
            ActivityInput {
                phase: String::new(),
                health: "unknown".into(),
                ..Default::default()
            },
            // 2. CR accepted, StatefulSet not created yet
            ActivityInput {
                phase: "PENDING".into(),
                health: "unknown".into(),
                ..Default::default()
            },
            // 3. volumes binding
            ActivityInput {
                phase: "PENDING".into(),
                health: "red".into(),
                nodes_desired: 3,
                pvcs_total: 3,
                pvcs_bound: 1,
                ..Default::default()
            },
            ActivityInput {
                phase: "PENDING".into(),
                health: "red".into(),
                nodes_desired: 3,
                pvcs_total: 3,
                pvcs_bound: 3,
                ..Default::default()
            },
            // 4. nodes coming up
            ActivityInput {
                phase: "RUNNING".into(),
                health: "red".into(),
                nodes_ready: 1,
                nodes_updated: 1,
                nodes_desired: 3,
                pvcs_total: 3,
                pvcs_bound: 3,
                ..Default::default()
            },
            ActivityInput {
                phase: "RUNNING".into(),
                health: "yellow".into(),
                nodes_ready: 2,
                nodes_updated: 2,
                nodes_desired: 3,
                pvcs_total: 3,
                pvcs_bound: 3,
                ..Default::default()
            },
            // 5. all nodes up, security still bootstrapping
            ActivityInput {
                phase: "RUNNING".into(),
                health: "yellow".into(),
                nodes_ready: 3,
                nodes_updated: 3,
                nodes_desired: 3,
                pvcs_total: 3,
                pvcs_bound: 3,
                ..Default::default()
            },
            // 6. security done, dashboards still starting
            ActivityInput {
                initialized: true,
                dashboards_ready: false,
                health: "green".into(),
                ..ActivityInput {
                    phase: "RUNNING".into(),
                    nodes_ready: 3,
                    nodes_updated: 3,
                    nodes_desired: 3,
                    pvcs_total: 3,
                    pvcs_bound: 3,
                    ..Default::default()
                }
            },
            // 7. everything up, cluster not green yet
            ActivityInput {
                health: "yellow".into(),
                ..steady()
            },
            // 8. settled
            steady(),
        ];

        let mut last = 0u8;
        for (n, i) in seq.iter().enumerate() {
            let a = evaluate(i);
            assert!(
                a.percent >= last,
                "step {n} went backwards: {last}% → {}% (stage {})",
                a.percent,
                a.stage
            );
            assert!(a.percent <= 100);
            if !a.settled {
                assert!(a.percent < 100, "step {n} reads 100% but is not settled");
            }
            last = a.percent;
        }
        assert_eq!(last, 100, "the sequence must end settled");
    }

    #[test]
    fn the_early_stages_are_named_rather_than_zero() {
        // The old bar sat at 0% through PVC provisioning and image pull — the
        // longest, most confusing part of a create. Each early stage now has
        // its own floor, so something moves.
        let storage = evaluate(&ActivityInput {
            health: "unknown".into(),
            ..Default::default()
        });
        assert_eq!(storage.stage, "storage");

        let accepted = evaluate(&ActivityInput {
            phase: "PENDING".into(),
            health: "unknown".into(),
            ..Default::default()
        });
        assert_eq!(accepted.stage, "accepted");
        assert!(accepted.percent >= 5);

        let volumes = evaluate(&ActivityInput {
            phase: "PENDING".into(),
            nodes_desired: 3,
            pvcs_total: 3,
            pvcs_bound: 1,
            ..Default::default()
        });
        assert_eq!(volumes.stage, "volumes");
        assert_eq!(volumes.detail, "1/3");
        assert!(volumes.percent > accepted.percent);
    }

    /// A deployment with no StatefulSet reports `(0,0,0)` — indistinguishable
    /// from "nothing up yet", which is what it is. It must not read as settled.
    #[test]
    fn zero_nodes_is_never_settled() {
        let a = evaluate(&ActivityInput {
            health: "green".into(),
            initialized: true,
            dashboards_ready: true,
            ..Default::default()
        });
        assert!(!a.settled, "a deployment with no nodes cannot be ready");
    }

    // ── issue #131: the 16-hour "25%" ───────────────────────────────────

    /// The whole defect in one assertion. Sixteen hours of no progress must
    /// produce a verdict that NAMES the cause, from signals the backend can
    /// already see. Vague is a failed fix: the panel that ran for those sixteen
    /// hours did say something — "this is taking longer than usual" — and that
    /// sentence is why nobody looked further.
    #[test]
    fn a_stalled_roll_explains_itself() {
        let a = evaluate(&stuck_roll());
        assert!(!a.settled);
        assert_eq!(a.kind, "restarting");
        assert!(a.stalled, "16 hours without progress is a stall");
        assert_eq!(
            a.since_secs, 57_240,
            "the age comes from the cluster, not a client clock"
        );

        // Every fact the incident's screenshots and logs contained, on the
        // wire, so the SPA can compose "waiting for green — 7 unassigned
        // shards, a shard recovery stuck in init for 15h".
        assert_eq!(a.blocked.health, "yellow");
        assert_eq!(a.blocked.unassigned_shards, 7);
        assert_eq!(a.blocked.component, "RollingRestart");
        assert_eq!(a.blocked.component_status, "Running");
        assert_eq!(a.blocked.recovery_index, ".opendistro_security");
        assert_eq!(a.blocked.recovery_stage, "init");
        assert_eq!(a.blocked.recovery_secs, 57_240);
    }

    /// Below the threshold a deployment is just starting, and saying otherwise
    /// would make the diagnosis worthless by crying wolf on every create.
    #[test]
    fn a_young_activity_is_not_stalled_and_carries_no_diagnosis() {
        let young = ActivityInput {
            since_secs: STALL_AFTER_SECS - 1,
            ..stuck_roll()
        };
        let a = evaluate(&young);
        assert!(!a.stalled);
        assert_eq!(a.blocked, Blocked::default());
        assert_eq!(
            a.blocked.unassigned_shards, -1,
            "unknown must not read as zero unassigned shards"
        );
    }

    /// No measurable age (no pod yet, no CR timestamp) is not a stall. A stall
    /// is a claim about elapsed time and we refuse to make it without one.
    #[test]
    fn an_unmeasurable_age_never_claims_a_stall() {
        let a = evaluate(&ActivityInput {
            since_secs: 0,
            ..stuck_roll()
        });
        assert!(!a.stalled);
    }

    /// When the cluster does not answer, the Kubernetes half still does. The
    /// panel degrades to "the operator is still running RollingRestart and the
    /// cluster is yellow" instead of showing nothing.
    #[test]
    fn a_stall_without_opensearch_still_names_what_kubernetes_knows() {
        let a = evaluate(&ActivityInput {
            cluster: None,
            ..stuck_roll()
        });
        assert!(a.stalled);
        assert_eq!(a.blocked.health, "yellow");
        assert_eq!(a.blocked.component_status, "Running");
        assert_eq!(a.blocked.unassigned_shards, -1, "not asked is not zero");
        assert!(a.blocked.recovery_index.is_empty());
    }

    /// `6/3` and `8/3` were on screen for hours. A count of live pods over a
    /// count the CR asks for is not a fraction, and the operator produces the
    /// mismatch on purpose: it will not shrink the pool while health is not
    /// green, which is precisely the state this panel renders.
    #[test]
    fn a_counter_can_never_exceed_its_total() {
        for extra in [1, 3, 5] {
            let oversized = ActivityInput {
                nodes_ready: 3 + extra,
                nodes_updated: 3 + extra,
                nodes_desired: 3,
                ..stuck_roll()
            };
            let a = evaluate(&oversized);
            assert_eq!(a.nodes_total, 3);
            assert_eq!(
                a.nodes_ready,
                3,
                "{} ready pods against a 3-node spec must read 3/3, never {}/3",
                3 + extra,
                3 + extra
            );
            assert!(
                a.detail.is_empty() || a.detail == "3/3",
                "sub-progress read `{}`, which exceeds the total",
                a.detail
            );
        }
    }

    /// Invariant behind UI rule "the screen cannot contradict itself": the tile
    /// and the progress row are the same two numbers, so there is no second
    /// place for a different answer to come from.
    #[test]
    fn the_node_pair_is_one_source() {
        let a = evaluate(&stuck_roll());
        assert_eq!((a.nodes_ready, a.nodes_total), (3, 3));
        // The stage counter is a DIFFERENT question (pods on the new revision),
        // which is why the label has to say so — but it is bounded by the same
        // total.
        assert_eq!(a.detail, "0/3");
        assert!(a.nodes_ready <= a.nodes_total);
    }

    /// Working and converged are two claims. The incident was both: primaries
    /// up and documents climbing, next to a roll that had not moved in 16h.
    #[test]
    fn serving_and_settled_are_independent() {
        let a = evaluate(&stuck_roll());
        assert!(a.serving, "primaries up and yellow is a cluster that works");
        assert!(!a.settled, "…and one that has not finished converging");

        // A cluster missing a primary is not serving.
        let red = evaluate(&ActivityInput {
            health: "red".into(),
            ..stuck_roll()
        });
        assert!(!red.serving);

        // Nor is one whose nodes are not all up yet — a create at 1 of 3.
        let creating = evaluate(&ActivityInput {
            initialized: false,
            health: "red".into(),
            nodes_ready: 1,
            nodes_updated: 1,
            ..stuck_roll()
        });
        assert!(!creating.serving);
    }

    /// A settled deployment carries the pair too, so the tile has one source in
    /// the steady state as well — and never claims a stall.
    #[test]
    fn an_idle_deployment_still_reports_its_nodes() {
        let a = evaluate(&steady());
        assert!(a.settled);
        assert_eq!((a.nodes_ready, a.nodes_total), (3, 3));
        assert!(!a.stalled);
        assert!(a.serving);
    }

    #[test]
    fn the_ladder_is_ordered_and_ends_at_100() {
        let mut last = 0u8;
        for (name, ceiling) in LADDER {
            assert!(
                *ceiling > last,
                "stage `{name}` does not advance the ladder"
            );
            last = *ceiling;
        }
        assert_eq!(last, 100);
    }
}
