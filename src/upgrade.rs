// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Deployment version upgrade — catalog, pre-flight and state reduction
//! (ADR-048, MR1 / #111).
//!
//! Pure: no cluster calls, no I/O. `k8s.rs` owns the writes (patch
//! `spec.general.version`, then `spec.dashboards.version`) and `api.rs` exposes
//! this to the UI. Everything here is unit-testable without a cluster.
//!
//! The shape of this module follows from one property: **the operator rejects
//! downgrades, so an upgrade cannot be taken back**. Hence a pinned catalog of
//! targets we have actually tested (not free text, not a live registry
//! listing), and a validation that refuses — in our words — exactly what the
//! operator refuses in its `pkg/reconcilers/upgrade.go`, *before* anything is
//! written to the CR.

use anyhow::{bail, Result};

/// The version a **newly created** deployment gets. The single home of the
/// literal that ADR-040 last moved by hand across four files; every other write
/// path preserves whatever the CR already runs (ADR-048 invariant 1).
pub const DEFAULT_VERSION: &str = "3.7.0";

/// Images whose tags the pre-flight resolves before the first patch (invariant
/// 3) — both, so the two-phase write can never strand a cluster with upgraded
/// nodes and an unpullable Dashboards.
pub const IMAGE_NODES: &str = "opensearchproject/opensearch";
pub const IMAGE_DASHBOARDS: &str = "opensearchproject/opensearch-dashboards";

// ───────────────────────────── version ─────────────────────────────

/// A `MAJOR.MINOR.PATCH` OpenSearch version. Ordering is field-wise, which is
/// what both the operator's own comparison and our catalog filter need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Strict parse — three numeric components, nothing else. Deliberately
    /// unforgiving: `3.7` or `3.7.0-beta` would produce an unpullable image tag
    /// on the first restarted node, and that pod is not revertible through the
    /// operator (option B in the ADR).
    pub fn parse(s: &str) -> Result<Version> {
        let s = s.trim();
        let mut it = s.split('.');
        let mut next = |what: &str| -> Result<u32> {
            match it.next().map(|p| p.parse::<u32>()) {
                Some(Ok(n)) => Ok(n),
                _ => bail!("'{s}' is not a valid version: missing or non-numeric {what} (expected MAJOR.MINOR.PATCH, e.g. 3.7.0)"),
            }
        };
        let major = next("major")?;
        let minor = next("minor")?;
        let patch = next("patch")?;
        if it.next().is_some() {
            bail!("'{s}' is not a valid version: too many components (expected MAJOR.MINOR.PATCH, e.g. 3.7.0)");
        }
        Ok(Version { major, minor, patch })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ───────────────────────────── catalog ─────────────────────────────

/// One tested upgrade target. `note` is a stable id (`current`, `lts`), not
/// prose — the UI translates it (ADR-019 i18n rule), the backend never ships a
/// user-facing sentence in one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogEntry {
    pub version: &'static str,
    pub note: &'static str,
}

/// The pinned catalog, newest first. A version is in this list because we
/// deployed and tested it — shipping a new target means shipping a release, and
/// that cost is the point (ADR-048, option A).
pub const CATALOG: &[CatalogEntry] = &[
    // 3.8.0 earned its place the way this list is meant to be earned: a live
    // rolling upgrade 3.7.0 → 3.8.0 on k3s (2026-08-06), green afterwards with
    // the indices intact. `DEFAULT_VERSION` stays at 3.7.0 — what a NEW
    // deployment is created with is a separate decision (ADR-040).
    CatalogEntry { version: "3.8.0", note: "current" },
    CatalogEntry { version: "3.7.0", note: "previous" },
    CatalogEntry { version: "3.6.0", note: "lts" },
];

/// Is this version one we ship as a tested target?
pub fn in_catalog(v: &str) -> bool {
    CATALOG.iter().any(|e| e.version == v.trim())
}

/// Catalog entries that are a legal upgrade from `current`, newest first.
///
/// Filtered by the operator's OWN rules (strictly greater, at most one major
/// ahead) so the UI can never offer a target the operator would refuse. An
/// unparseable `current` yields nothing: we do not guess what an unknown CR is
/// running.
pub fn targets_for(current: &str) -> Vec<CatalogEntry> {
    let Ok(cur) = Version::parse(current) else {
        return Vec::new();
    };
    CATALOG
        .iter()
        .filter(|e| match Version::parse(e.version) {
            Ok(t) => t > cur && t.major <= cur.major + 1,
            Err(_) => false,
        })
        .copied()
        .collect()
}

/// Stable `MAJOR.MINOR.PATCH` versions among a registry's tag names.
///
/// Everything else is dropped: `latest`, `2`, `2.19`, `3.7.0-beta1`,
/// `3.7.0.arm64`. Pre-release tags are the ones that matter here — suggesting a
/// beta as an upgrade would be suggesting an irreversible move onto untested
/// bits (ADR-048 rev. 2).
pub fn stable_versions(tags: &[String]) -> Vec<Version> {
    let mut out: Vec<Version> = tags.iter().filter_map(|t| Version::parse(t).ok()).collect();
    out.sort();
    out.dedup();
    out
}

/// Pre-flight rule 1 (ADR-048 invariant 2): the version pair itself.
///
/// Returns the operator's refusals in our words — `pkg/reconcilers/upgrade.go`
/// records a rejected version only as a `Warning`/`Upgrade` Event and a
/// terminal error that does NOT stop its other reconcilers, i.e. upstream it
/// fails silently. Catching it here is what makes it visible.
pub fn validate(current: &str, requested: &str) -> Result<()> {
    let cur = Version::parse(current)?;
    let req = Version::parse(requested)?;
    if req == cur {
        bail!("{req} is already the running version");
    }
    if req < cur {
        bail!(
            "{req} is older than the running {cur} — the operator rejects downgrades and there is \
             no rollback; restore a snapshot into a new deployment instead"
        );
    }
    if req.major > cur.major + 1 {
        bail!(
            "{req} is more than one major version ahead of {cur} — upgrade to the latest {}.x \
             first",
            cur.major + 1
        );
    }
    Ok(())
}

// ─────────────────────────── upgrade state ───────────────────────────

/// Where an upgrade is, reduced from what the CR reports. Lives on the CR, not
/// in the backend — it survives a page reload and a backend restart (ADR-048
/// consequences), and no in-memory bookkeeping is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpgradeState {
    /// Spec and status agree; nothing in flight. Also the `Default`, so
    /// `ActivityInput` (ADR-050) can be built field-by-field in tests without
    /// naming an upgrade state that is not what the case is about.
    #[default]
    Idle,
    /// The spec asks for a new version, the operator has not started the pool.
    Pending { from: String, to: String },
    /// A node pool is being rolled, one node at a time.
    Upgrading { pool: String, from: String, to: String },
    /// Every pool finished and the cluster reports the target version.
    Finished { version: String },
    /// The operator refused or could not complete. `reason` is the upstream
    /// string verbatim (ADR-045 UI rule 5).
    Failed { reason: String },
}

impl UpgradeState {
    /// Is an upgrade in flight? The pre-flight refuses to start a second one.
    pub fn in_flight(&self) -> bool {
        matches!(self, UpgradeState::Pending { .. } | UpgradeState::Upgrading { .. })
    }

    /// Stable id for the DTO/UI (`idle`, `pending`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            UpgradeState::Idle => "idle",
            UpgradeState::Pending { .. } => "pending",
            UpgradeState::Upgrading { .. } => "upgrading",
            UpgradeState::Finished { .. } => "finished",
            UpgradeState::Failed { .. } => "failed",
        }
    }
}

const UPGRADER: &str = "Upgrader";

/// Reduce the CR's own reporting into one state.
///
/// * `spec_version` — `spec.general.version` (what we asked for).
/// * `status_version` — `status.version` (what the operator considers running).
/// * `components` — `status.componentsStatus[]` verbatim.
/// * `warning` — the message of the most recent `Warning`/`Upgrade` Event on
///   the CR, when there is one. The operator writes validation refusals ONLY
///   there (verified in `upgrade.go`: `recorder.AnnotatedEventf(...)` +
///   `AsTerminal(err)`, never into `componentsStatus`), so without this input a
///   refused upgrade is indistinguishable from one that never started.
///
/// A warning only means `Failed` while the spec and the running version still
/// disagree — a stale Event from a previous, since-completed attempt must not
/// paint a healthy deployment red.
pub fn upgrade_state(
    spec_version: &str,
    status_version: &str,
    components: &serde_json::Value,
    warning: Option<&str>,
) -> UpgradeState {
    let spec_v = spec_version.trim();
    let status_v = status_version.trim();
    // No status version yet (fresh CR): the operator has not reported, and a
    // creation is not an upgrade.
    let diverged = !status_v.is_empty() && !spec_v.is_empty() && status_v != spec_v;

    let mut pending = false;
    let mut upgrading: Option<String> = None;
    if let Some(arr) = components.as_array() {
        for c in arr {
            if c.get("component").and_then(|v| v.as_str()) != Some(UPGRADER) {
                continue;
            }
            let status = c.get("status").and_then(|v| v.as_str()).unwrap_or_default();
            let pool = c
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            match status {
                "Upgrading" => upgrading = Some(pool),
                "Pending" => pending = true,
                // "Finished" (per pool) and "Upgraded" (cluster) are terminal
                // and carry no work — the version comparison below decides.
                _ => {}
            }
        }
    }

    if let Some(pool) = upgrading {
        return UpgradeState::Upgrading {
            pool,
            from: status_v.to_string(),
            to: spec_v.to_string(),
        };
    }
    if diverged {
        if let Some(w) = warning.map(str::trim).filter(|w| !w.is_empty()) {
            return UpgradeState::Failed { reason: w.to_string() };
        }
        return UpgradeState::Pending {
            from: status_v.to_string(),
            to: spec_v.to_string(),
        };
    }
    if pending {
        return UpgradeState::Pending {
            from: status_v.to_string(),
            to: spec_v.to_string(),
        };
    }
    UpgradeState::Idle
}

/// Convergence check for the second phase: nodes are done when the operator
/// reports the target as running and no pool is still rolling.
pub fn nodes_upgraded(state: &UpgradeState, status_version: &str, target: &str) -> bool {
    !state.in_flight() && status_version.trim() == target.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_and_orders() {
        assert_eq!(v("3.7.0"), Version { major: 3, minor: 7, patch: 0 });
        assert!(v("3.7.0") > v("3.6.9"));
        assert!(v("3.10.0") > v("3.9.0")); // not a string comparison
        assert!(v("2.19.0") < v("3.0.0"));
    }

    #[test]
    fn refuses_malformed_versions() {
        for bad in ["", "3.7", "3.7.0.1", "3.7.x", "v3.7.0", "3.7.0-beta1", "latest"] {
            assert!(Version::parse(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn targets_never_offer_a_downgrade_or_a_two_major_jump() {
        // Property over a grid: whatever the catalog holds, every offered
        // target is strictly ahead and at most one major up.
        for major in 1..=4u32 {
            for minor in [0u32, 5, 7, 20] {
                let cur = format!("{major}.{minor}.0");
                for t in targets_for(&cur) {
                    let tv = v(t.version);
                    assert!(tv > v(&cur), "{} offered from {cur}", t.version);
                    assert!(tv.major <= major + 1, "{} offered from {cur}", t.version);
                }
            }
        }
    }

    #[test]
    fn targets_for_the_newest_catalog_entry_are_empty() {
        assert!(targets_for(CATALOG[0].version).is_empty());
        // A deployment created today (DEFAULT_VERSION) is offered only what is
        // ahead of it.
        assert_eq!(
            targets_for(DEFAULT_VERSION).iter().map(|e| e.version).collect::<Vec<_>>(),
            vec!["3.8.0"]
        );
        // The legacy 3.0.0 deployment (ADR-044) sees every catalog entry.
        let t = targets_for("3.0.0");
        assert_eq!(
            t.iter().map(|e| e.version).collect::<Vec<_>>(),
            vec!["3.8.0", "3.7.0", "3.6.0"]
        );
    }

    #[test]
    fn targets_for_an_unknown_version_are_empty_not_a_guess() {
        assert!(targets_for("").is_empty());
        assert!(targets_for("latest").is_empty());
    }

    #[test]
    fn catalog_is_parseable_newest_first_and_contains_the_default() {
        assert!(in_catalog(DEFAULT_VERSION));
        let mut prev: Option<Version> = None;
        for e in CATALOG {
            let cur = v(e.version);
            if let Some(p) = prev {
                assert!(cur < p, "catalog must be newest-first");
            }
            prev = Some(cur);
        }
    }

    #[test]
    fn stable_versions_drops_everything_that_is_not_a_release() {
        let tags: Vec<String> = [
            "latest", "3", "3.7", "3.7.0", "3.6.0", "3.8.0-beta1", "2.19.1",
            "3.7.0.arm64", "", "3.7.0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let v = stable_versions(&tags);
        assert_eq!(
            v.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
            vec!["2.19.1", "3.6.0", "3.7.0"] // sorted, deduped, no pre-releases
        );
    }

    #[test]
    fn validate_refuses_what_the_operator_refuses() {
        assert!(validate("3.0.0", "3.7.0").is_ok());
        assert!(validate("3.7.0", "4.0.0").is_ok()); // exactly one major ahead

        let same = validate("3.7.0", "3.7.0").unwrap_err().to_string();
        assert!(same.contains("already"), "{same}");

        let down = validate("3.7.0", "3.6.0").unwrap_err().to_string();
        assert!(down.contains("downgrade") && down.contains("no rollback"), "{down}");

        let jump = validate("3.7.0", "5.0.0").unwrap_err().to_string();
        assert!(jump.contains("more than one major") && jump.contains("4.x"), "{jump}");

        assert!(validate("3.7.0", "3.7").is_err());
        assert!(validate("nonsense", "3.7.0").is_err());
    }

    fn comps(entries: &[(&str, &str, &str)]) -> serde_json::Value {
        serde_json::Value::Array(
            entries
                .iter()
                .map(|(component, status, description)| {
                    serde_json::json!({
                        "component": component,
                        "status": status,
                        "description": description
                    })
                })
                .collect(),
        )
    }

    #[test]
    fn state_idle_when_spec_and_status_agree() {
        assert_eq!(
            upgrade_state("3.7.0", "3.7.0", &serde_json::Value::Null, None),
            UpgradeState::Idle
        );
        // A component from another reconciler must not be read as an upgrade.
        let other = comps(&[("Security", "Pending", "")]);
        assert_eq!(upgrade_state("3.7.0", "3.7.0", &other, None), UpgradeState::Idle);
    }

    #[test]
    fn state_pending_when_the_spec_moved_ahead() {
        let s = upgrade_state("3.7.0", "3.0.0", &serde_json::Value::Null, None);
        assert_eq!(
            s,
            UpgradeState::Pending { from: "3.0.0".into(), to: "3.7.0".into() }
        );
        assert!(s.in_flight());
    }

    #[test]
    fn state_upgrading_names_the_pool() {
        let c = comps(&[("Upgrader", "Upgrading", "nodes")]);
        assert_eq!(
            upgrade_state("3.7.0", "3.0.0", &c, None),
            UpgradeState::Upgrading {
                pool: "nodes".into(),
                from: "3.0.0".into(),
                to: "3.7.0".into()
            }
        );
    }

    #[test]
    fn state_finished_pools_with_the_version_reached_is_idle() {
        let c = comps(&[("Upgrader", "Finished", "nodes")]);
        assert_eq!(upgrade_state("3.7.0", "3.7.0", &c, None), UpgradeState::Idle);
    }

    #[test]
    fn state_failed_only_while_the_versions_still_disagree() {
        let msg = "Failed to validation version: version requested is downgrade";
        // Refused: spec asks for something the operator won't do.
        assert_eq!(
            upgrade_state("3.7.0", "3.0.0", &serde_json::Value::Null, Some(msg)),
            UpgradeState::Failed { reason: msg.into() }
        );
        // Stale warning from a previous attempt that has since completed.
        assert_eq!(
            upgrade_state("3.7.0", "3.7.0", &serde_json::Value::Null, Some(msg)),
            UpgradeState::Idle
        );
        // An empty event message is not a failure.
        assert_eq!(
            upgrade_state("3.7.0", "3.0.0", &serde_json::Value::Null, Some("  ")),
            UpgradeState::Pending { from: "3.0.0".into(), to: "3.7.0".into() }
        );
    }

    #[test]
    fn a_rolling_pool_outranks_a_warning() {
        // The operator is actually moving nodes — show progress, not an error.
        let c = comps(&[("Upgrader", "Upgrading", "nodes")]);
        let s = upgrade_state("3.7.0", "3.0.0", &c, Some("some older warning"));
        assert!(matches!(s, UpgradeState::Upgrading { .. }));
    }

    #[test]
    fn fresh_cluster_without_a_status_version_is_idle() {
        assert_eq!(
            upgrade_state("3.7.0", "", &serde_json::Value::Null, None),
            UpgradeState::Idle
        );
    }

    #[test]
    fn nodes_upgraded_needs_both_the_version_and_a_settled_pool() {
        assert!(nodes_upgraded(&UpgradeState::Idle, "3.7.0", "3.7.0"));
        assert!(!nodes_upgraded(&UpgradeState::Idle, "3.0.0", "3.7.0"));
        let rolling = UpgradeState::Upgrading {
            pool: "nodes".into(),
            from: "3.0.0".into(),
            to: "3.7.0".into(),
        };
        assert!(!nodes_upgraded(&rolling, "3.7.0", "3.7.0"));
    }
}
