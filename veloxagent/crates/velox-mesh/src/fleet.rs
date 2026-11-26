// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! What an operator sees when they look at the fleet, decided here and
//! nowhere else. Pure: no clock of its own, no database, no HTTP — every
//! rule below is a function of values the caller read, which is what makes
//! "why is this agent red?" a test instead of an argument.
//!
//! ## Status is derived, never stored
//!
//! A status column is a lie the moment the writer that would have updated it
//! dies. So the Mesh stores facts — `last_seen`, the config status the agent
//! reported, the hash it echoed — and [`status`] turns them into the word on
//! the screen at read time. The order of the checks is the design:
//!
//! 1. **unenrolled** — the operator removed it; nothing else matters.
//! 2. **offline** — no check-in within [`OFFLINE_AFTER`]. An agent that is
//!    not talking cannot be called healthy on the strength of what it said
//!    five minutes ago, whatever that was.
//! 3. **failed** — it checked in and said the configuration was refused. The
//!    engine's own message travels with it.
//! 4. **updating** — it checked in on an older revision than the policy's
//!    current one, or said it is applying. This is the rollout state, and it
//!    is normal rather than alarming.
//! 5. **degraded** — current and applied, but an engine is unhealthy.
//! 6. **healthy** — current, applied, engines up.
//!
//! "up/down" on the screen is [`Status::is_up`]: offline and unenrolled are
//! down, everything else is up, because an agent that is talking is up even
//! while it is updating or complaining.

use serde::{Deserialize, Serialize};

/// No check-in for this long and an agent is offline. Fleet uses a fixed five
/// minutes and the number is not worth a setting: it is long enough for a
/// 30-second poll to miss nine times, and short enough that a dead host is
/// noticed within a coffee break.
pub const OFFLINE_AFTER: u64 = 300;

/// What the agent said about the last configuration it was handed. Mirrors
/// OpAMP's `RemoteConfigStatuses` so the wire word and the stored word are
/// the same word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConfigStatus {
    Unset,
    Applied,
    Applying,
    Failed,
}

impl ConfigStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigStatus::Unset => "UNSET",
            ConfigStatus::Applied => "APPLIED",
            ConfigStatus::Applying => "APPLYING",
            ConfigStatus::Failed => "FAILED",
        }
    }
    pub fn parse(s: &str) -> ConfigStatus {
        match s {
            "APPLIED" => ConfigStatus::Applied,
            "APPLYING" => ConfigStatus::Applying,
            "FAILED" => ConfigStatus::Failed,
            _ => ConfigStatus::Unset,
        }
    }
}

/// The facts [`status`] decides from. Everything here is read straight out of
/// `mesh_agents` plus the policy's current revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFacts {
    pub unenrolled: bool,
    /// Unix seconds of the last check-in.
    pub last_seen: u64,
    /// The revision the agent reported applying, if any.
    pub applied_revision: Option<u64>,
    /// The revision its policy is on now. `None` = no policy assigned.
    pub current_revision: Option<u64>,
    pub config_status: ConfigStatus,
    /// Whether every supervised engine reported healthy.
    pub engines_healthy: bool,
}

/// The word on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Healthy,
    Degraded,
    Updating,
    Failed,
    Offline,
    Unenrolled,
}

impl Status {
    /// Up/down, the one question the fleet screen answers at a glance. An
    /// agent that is talking is **up**, even while it is updating or
    /// reporting a bad config — those are states of a live agent, and
    /// colouring them "down" would hide the hosts that are genuinely gone.
    pub fn is_up(self) -> bool {
        !matches!(self, Status::Offline | Status::Unenrolled)
    }
}

/// Derive an agent's status. `now` is passed in, never read from the clock,
/// so the offline boundary is testable to the second.
pub fn status(f: &AgentFacts, now: u64) -> Status {
    if f.unenrolled {
        return Status::Unenrolled;
    }
    if now.saturating_sub(f.last_seen) > OFFLINE_AFTER {
        return Status::Offline;
    }
    if f.config_status == ConfigStatus::Failed {
        return Status::Failed;
    }
    // Mid-rollout: either it told us it is applying, or it last confirmed an
    // older revision than the policy now has.
    let behind = match (f.current_revision, f.applied_revision) {
        (Some(current), Some(applied)) => applied < current,
        // Assigned a policy and has not confirmed anything yet.
        (Some(_), None) => true,
        (None, _) => false,
    };
    if f.config_status == ConfigStatus::Applying || behind {
        return Status::Updating;
    }
    if !f.engines_healthy {
        return Status::Degraded;
    }
    Status::Healthy
}

/// How a policy's rollout stands: what the UI means by "12 of 14 on
/// revision 7". Counted from the agents' own echoes, never from a counter
/// the Mesh incremented when it sent the config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Rollout {
    pub total: usize,
    /// Confirmed on the current revision.
    pub current: usize,
    /// Alive and not yet on it.
    pub updating: usize,
    /// Alive and refused it, with the engine's reason on the agent's row.
    pub failed: usize,
    pub offline: usize,
    /// True once every agent that can answer has confirmed the revision.
    /// Offline agents do not block it — they are counted and shown, but a
    /// host that is switched off must not make a rollout look stuck forever.
    /// A field rather than a method so the console reads the verdict instead
    /// of reimplementing the rule.
    pub complete: bool,
}

/// Tally a policy's agents. `statuses` is what [`status`] returned for each.
pub fn rollout(statuses: &[Status]) -> Rollout {
    let mut r = Rollout {
        total: statuses.len(),
        ..Rollout::default()
    };
    for s in statuses {
        match s {
            Status::Healthy | Status::Degraded => r.current += 1,
            Status::Updating => r.updating += 1,
            Status::Failed => r.failed += 1,
            Status::Offline => r.offline += 1,
            // An unenrolled agent is not part of the policy's rollout.
            Status::Unenrolled => r.total -= 1,
        }
    }
    r.complete = r.updating == 0 && r.failed == 0 && r.total > 0;
    r
}

/// Where the operator is in the guided onboarding. The first run through is
/// **ordered** — a policy with no Mesh to hold it, or an agent with no policy
/// to run, is a dead end — so the console walks it and this decides which
/// step is live. Afterwards every screen is reachable directly; the gate is
/// a first-time guide, not a permanent cage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingStep {
    /// No Mesh configured or reachable.
    InstallMesh,
    /// Mesh up, no policy yet.
    CreatePolicy,
    /// A policy exists, nothing has enrolled.
    EnrolAgent,
    /// An agent enrolled but has not confirmed its policy yet.
    AwaitFirstApply,
    /// Done; the console stops guiding.
    Done,
}

/// Decide the step from the three facts the console can always obtain.
pub fn onboarding(mesh_ready: bool, policies: usize, agents: &[Status]) -> OnboardingStep {
    if !mesh_ready {
        return OnboardingStep::InstallMesh;
    }
    if policies == 0 {
        return OnboardingStep::CreatePolicy;
    }
    if agents.is_empty() {
        return OnboardingStep::EnrolAgent;
    }
    // An agent that has confirmed anything — healthy, degraded or even
    // failed — has completed the journey: the loop closed, and a refused
    // config is a thing to fix rather than a step still pending.
    if agents
        .iter()
        .any(|s| matches!(s, Status::Healthy | Status::Degraded | Status::Failed))
    {
        return OnboardingStep::Done;
    }
    OnboardingStep::AwaitFirstApply
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn facts() -> AgentFacts {
        AgentFacts {
            unenrolled: false,
            last_seen: NOW,
            applied_revision: Some(7),
            current_revision: Some(7),
            config_status: ConfigStatus::Applied,
            engines_healthy: true,
        }
    }

    #[test]
    fn a_current_agent_with_healthy_engines_is_healthy_and_up() {
        let s = status(&facts(), NOW);
        assert_eq!(s, Status::Healthy);
        assert!(s.is_up());
    }

    /// The offline boundary is exact, and it is the ONLY thing that decides
    /// up/down. One second inside the window is still up.
    #[test]
    fn offline_is_decided_by_the_last_check_in_alone() {
        let f = facts();
        assert_eq!(
            status(&f, NOW + OFFLINE_AFTER),
            Status::Healthy,
            "the boundary itself is up"
        );
        assert_eq!(status(&f, NOW + OFFLINE_AFTER + 1), Status::Offline);
        assert!(!status(&f, NOW + OFFLINE_AFTER + 1).is_up());

        // A silent agent is offline whatever it last said — including that it
        // was perfectly healthy, which is exactly the stale-status trap.
        let mut noisy = facts();
        noisy.config_status = ConfigStatus::Failed;
        noisy.engines_healthy = false;
        assert_eq!(status(&noisy, NOW + 10_000), Status::Offline);
    }

    /// A refused configuration beats "updating": the operator must see the
    /// failure, not a spinner.
    #[test]
    fn a_refused_configuration_is_failed_and_still_up() {
        let mut f = facts();
        f.config_status = ConfigStatus::Failed;
        f.applied_revision = Some(6);
        let s = status(&f, NOW);
        assert_eq!(s, Status::Failed);
        assert!(
            s.is_up(),
            "a host that is talking is up, even when it refuses the config"
        );
    }

    /// The rollout states: behind the current revision, or explicitly
    /// applying, or never confirmed anything.
    #[test]
    fn an_agent_behind_the_current_revision_is_updating() {
        let mut f = facts();
        f.applied_revision = Some(6);
        assert_eq!(status(&f, NOW), Status::Updating);

        let mut f = facts();
        f.config_status = ConfigStatus::Applying;
        assert_eq!(status(&f, NOW), Status::Updating);

        let mut f = facts();
        f.applied_revision = None;
        assert_eq!(
            status(&f, NOW),
            Status::Updating,
            "assigned but never confirmed"
        );

        // No policy assigned at all is not "updating" — there is nothing to
        // apply, so an idle healthy agent reads healthy.
        let mut f = facts();
        f.current_revision = None;
        f.applied_revision = None;
        assert_eq!(status(&f, NOW), Status::Healthy);
    }

    #[test]
    fn an_unhealthy_engine_degrades_a_current_agent() {
        let mut f = facts();
        f.engines_healthy = false;
        assert_eq!(status(&f, NOW), Status::Degraded);
        assert!(status(&f, NOW).is_up());
    }

    #[test]
    fn unenrolled_wins_over_everything() {
        let mut f = facts();
        f.unenrolled = true;
        f.last_seen = 0;
        let s = status(&f, NOW);
        assert_eq!(s, Status::Unenrolled);
        assert!(!s.is_up());
    }

    /// "12 of 14 on revision 7", counted from echoes.
    #[test]
    fn rollout_counts_each_state_and_excludes_unenrolled() {
        let r = rollout(&[
            Status::Healthy,
            Status::Healthy,
            Status::Degraded,
            Status::Updating,
            Status::Failed,
            Status::Offline,
            Status::Unenrolled,
        ]);
        assert_eq!(
            r.total, 6,
            "the unenrolled agent is not part of the rollout"
        );
        assert_eq!(r.current, 3, "degraded is still ON the revision");
        assert_eq!((r.updating, r.failed, r.offline), (1, 1, 1));
        assert!(!r.complete);
    }

    /// An offline host must not make a rollout look stuck forever; a failing
    /// one must.
    #[test]
    fn a_rollout_completes_despite_offline_agents_but_not_despite_failures() {
        assert!(rollout(&[Status::Healthy, Status::Offline]).complete);
        assert!(!rollout(&[Status::Healthy, Status::Failed]).complete);
        assert!(!rollout(&[Status::Healthy, Status::Updating]).complete);
        assert!(
            !rollout(&[]).complete,
            "nothing to roll out is not 'complete'"
        );
    }

    /// The first run is ordered, and each step unlocks only when the previous
    /// one actually produced something.
    #[test]
    fn onboarding_walks_mesh_then_policy_then_agent() {
        assert_eq!(onboarding(false, 0, &[]), OnboardingStep::InstallMesh);
        assert_eq!(
            onboarding(false, 3, &[Status::Healthy]),
            OnboardingStep::InstallMesh,
            "no Mesh means step one, whatever else exists"
        );
        assert_eq!(onboarding(true, 0, &[]), OnboardingStep::CreatePolicy);
        assert_eq!(onboarding(true, 1, &[]), OnboardingStep::EnrolAgent);
        assert_eq!(
            onboarding(true, 1, &[Status::Updating]),
            OnboardingStep::AwaitFirstApply
        );
        assert_eq!(
            onboarding(true, 1, &[Status::Offline]),
            OnboardingStep::AwaitFirstApply
        );
    }

    /// The loop is closed once an agent CONFIRMS — including a confirmation
    /// that the config was refused, which is a problem to fix rather than a
    /// step still pending.
    #[test]
    fn onboarding_is_done_once_an_agent_confirms_anything() {
        for s in [Status::Healthy, Status::Degraded, Status::Failed] {
            assert_eq!(onboarding(true, 1, &[s]), OnboardingStep::Done, "{s:?}");
        }
        assert_eq!(
            onboarding(true, 1, &[Status::Updating, Status::Healthy]),
            OnboardingStep::Done,
            "one agent through the loop is enough to stop guiding"
        );
    }

    #[test]
    fn config_status_round_trips_through_the_stored_word() {
        for s in [
            ConfigStatus::Unset,
            ConfigStatus::Applied,
            ConfigStatus::Applying,
            ConfigStatus::Failed,
        ] {
            assert_eq!(ConfigStatus::parse(s.as_str()), s);
        }
        assert_eq!(ConfigStatus::parse("nonsense"), ConfigStatus::Unset);
    }
}
