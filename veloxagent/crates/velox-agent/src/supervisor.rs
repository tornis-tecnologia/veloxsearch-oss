// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! How a new configuration reaches a running Fluent Bit without ever leaving
//! the host in a broken state. Pure: this module decides the steps, the
//! runtime (later) executes them.
//!
//! The sequence, and why each step exists:
//!
//! 1. **Write to a staging file**, never over the live one. A half-written
//!    live file plus a crash is a host that cannot restart its collector.
//! 2. **Dry-run the staging file** (`fluent-bit -c <staging> --dry-run`).
//!    Fluent Bit validates and exits; a typo in a policy fails here, on one
//!    host, with Fluent Bit's own message — which the agent reports to the
//!    Mesh as `RemoteConfigStatus::Failed` with that message verbatim.
//! 3. **Rename staging over live** (atomic on the same filesystem).
//! 4. **Hot reload** (`POST /api/v2/reload` on the engine's HTTP server;
//!    needs `hot_reload: on` and `http_server: on` in the service section,
//!    which the compiled config always carries). Fluent Bit ≥ 5.1.2 keeps the
//!    config directory across reloads, so includes resolve the same way.
//! 5. **Confirm**: `/api/v2/health` 200 and `hot_reload_count` incremented.
//!    If the reload did not take, **restart the process** with the new file —
//!    hot reload has had plugin-specific edge cases and the agent must never
//!    depend on it alone.
//! 6. If the restarted process is not healthy within the deadline, **roll
//!    back**: rename the previous file back, restart, report `Failed`. The
//!    last-known-good file is kept for exactly this.
//!
//! Elastic Agent restarts Beats on every policy change; Alloy re-evaluates
//! components in place. This is between the two: in-place when the engine
//! confirms it, restart when it does not, rollback when that fails too.

#![allow(dead_code)] // the runtime that executes a plan lands in fase 1

use std::path::{Path, PathBuf};

/// Where one engine's files live under the agent's state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineFiles {
    pub live: PathBuf,
    pub staging: PathBuf,
    pub previous: PathBuf,
}

impl EngineFiles {
    pub fn fluent_bit(state_dir: &Path) -> Self {
        let d = state_dir.join("fluent-bit");
        Self {
            live: d.join("fluent-bit.yaml"),
            staging: d.join("fluent-bit.yaml.staging"),
            previous: d.join("fluent-bit.yaml.previous"),
        }
    }
}

/// One step of an apply. Ordered; the runtime stops at the first failure and
/// consults [`rollback_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    WriteStaging {
        path: PathBuf,
    },
    DryRun {
        config: PathBuf,
    },
    /// `from` → `to`, atomic rename.
    Promote {
        from: PathBuf,
        to: PathBuf,
    },
    HotReload {
        endpoint: String,
    },
    ConfirmHealthy {
        endpoint: String,
        deadline_secs: u64,
    },
    RestartProcess,
}

/// Fluent Bit's HTTP server address inside the host. Loopback only — the
/// agent is the only client, and the metrics it exposes name every index.
pub const FLUENT_BIT_HTTP: &str = "http://127.0.0.1:2020";

/// The forward path: staging, validate, promote, reload, confirm.
pub fn apply_plan(files: &EngineFiles) -> Vec<Step> {
    vec![
        Step::WriteStaging {
            path: files.staging.clone(),
        },
        Step::DryRun {
            config: files.staging.clone(),
        },
        Step::Promote {
            from: files.live.clone(),
            to: files.previous.clone(),
        },
        Step::Promote {
            from: files.staging.clone(),
            to: files.live.clone(),
        },
        Step::HotReload {
            endpoint: format!("{FLUENT_BIT_HTTP}/api/v2/reload"),
        },
        Step::ConfirmHealthy {
            endpoint: format!("{FLUENT_BIT_HTTP}/api/v2/health"),
            deadline_secs: 30,
        },
    ]
}

/// When a reload does not take: restart on the new file, and if that is not
/// healthy either, put the previous file back and restart again.
pub fn rollback_plan(files: &EngineFiles) -> Vec<Step> {
    vec![
        Step::RestartProcess,
        Step::ConfirmHealthy {
            endpoint: format!("{FLUENT_BIT_HTTP}/api/v2/health"),
            deadline_secs: 30,
        },
        Step::Promote {
            from: files.previous.clone(),
            to: files.live.clone(),
        },
        Step::RestartProcess,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_file_is_never_written_directly() {
        let f = EngineFiles::fluent_bit(Path::new("/var/lib/velox-agent"));
        for s in apply_plan(&f) {
            if let Step::WriteStaging { path } = s {
                assert_eq!(path, f.staging);
                assert_ne!(path, f.live);
            }
        }
    }

    #[test]
    fn validation_precedes_promotion_and_reload() {
        let f = EngineFiles::fluent_bit(Path::new("/s"));
        let plan = apply_plan(&f);
        let pos = |pred: fn(&Step) -> bool| plan.iter().position(pred).unwrap();
        let dry = pos(|s| matches!(s, Step::DryRun { .. }));
        let promote =
            pos(|s| matches!(s, Step::Promote { to, .. } if to.ends_with("fluent-bit.yaml")));
        let reload = pos(|s| matches!(s, Step::HotReload { .. }));
        assert!(dry < promote && promote < reload);
    }

    #[test]
    fn rollback_restores_the_previous_file() {
        let f = EngineFiles::fluent_bit(Path::new("/s"));
        let plan = rollback_plan(&f);
        assert!(plan.iter().any(
            |s| matches!(s, Step::Promote { from, to } if *from == f.previous && *to == f.live)
        ));
        assert!(matches!(plan.last(), Some(Step::RestartProcess)));
    }
}
