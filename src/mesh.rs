// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Does this installation have a Velox Mesh, and can the console reach it?
//!
//! Velox Mesh is a separate binary, image and release (ADR-068): the core does
//! not embed it, start it or depend on its crates. What the core needs is one
//! fact — whether an operator has installed one and pointed us at it — so the
//! console can either open the fleet screens or explain how to get there.
//! That fact is `VELOX_MESH_URL`, the same env-var convention as
//! `VELOX_REGISTRY_URL`, and unset means **not installed**: a feature that
//! defaults to off and preserves prior behaviour, like every other flag here.
//!
//! The probe is deliberately shallow. `GET /healthz` with a short timeout
//! separates the three states an operator actually has to tell apart:
//!
//! | Configured | Reachable | What the console shows |
//! | --- | --- | --- |
//! | no | — | how to install Velox Mesh |
//! | yes | no | the URL and the error — a deploy that did not come up, or a NetworkPolicy |
//! | yes | yes | the fleet |
//!
//! Collapsing the middle row into "not installed" is the failure mode worth
//! avoiding: an operator who *did* install it would be told to install it
//! again.

use serde::Serialize;
use std::time::Duration;

/// How long the console waits for the Mesh before calling it unreachable.
/// Short on purpose: this answer gates a screen's first paint.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// What the console needs to decide which Velox Mesh screen to render.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct MeshStatus {
    /// `VELOX_MESH_URL` is set. False ⇒ the install guide.
    pub configured: bool,
    /// The configured base URL, so the screen can name what it tried.
    pub url: Option<String>,
    /// `None` when not configured; otherwise the result of `GET /healthz`.
    pub reachable: Option<bool>,
    /// Why the probe failed, for the operator — not a stack trace.
    pub error: Option<String>,
}

/// The configured Mesh base URL, trimmed of a trailing slash so joining a
/// path never produces `//`.
pub fn configured_url() -> Option<String> {
    std::env::var("VELOX_MESH_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

/// Read the status, probing the Mesh when one is configured.
pub async fn status() -> MeshStatus {
    let Some(url) = configured_url() else {
        return MeshStatus::default();
    };
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return MeshStatus {
                configured: true,
                url: Some(url),
                reachable: Some(false),
                error: Some(e.to_string()),
            }
        }
    };
    match client.get(format!("{url}/healthz")).send().await {
        Ok(r) if r.status().is_success() => MeshStatus {
            configured: true,
            url: Some(url),
            reachable: Some(true),
            error: None,
        },
        Ok(r) => MeshStatus {
            configured: true,
            url: Some(url),
            reachable: Some(false),
            error: Some(format!("the Mesh answered {} at /healthz", r.status())),
        },
        Err(e) => MeshStatus {
            configured: true,
            url: Some(url),
            reachable: Some(false),
            // `e.to_string()` on reqwest carries the URL already; the operator
            // needs the reason, not our formatting of it.
            error: Some(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset is "not installed", and that is the state a fresh install is in.
    /// The three fields must agree, because the console branches on them.
    #[test]
    fn unconfigured_is_the_default_and_says_nothing_else() {
        let s = MeshStatus::default();
        assert!(!s.configured);
        assert_eq!(s.url, None);
        assert_eq!(
            s.reachable, None,
            "an unconfigured Mesh is not 'unreachable'"
        );
        assert_eq!(s.error, None);
    }

    /// A trailing slash in the operator's env var must not become `//healthz`.
    #[test]
    fn the_url_is_normalised() {
        // `configured_url` reads the process environment, so drive the same
        // normalisation the function applies rather than mutating it here.
        let norm = |s: &str| s.trim().trim_end_matches('/').to_string();
        assert_eq!(norm("http://velox-mesh:4320/"), "http://velox-mesh:4320");
        assert_eq!(norm("  http://velox-mesh:4320  "), "http://velox-mesh:4320");
        assert_eq!(norm("http://velox-mesh:4320"), "http://velox-mesh:4320");
    }

    /// Configured-but-unreachable is its OWN state: an operator who installed
    /// the Mesh and hit a NetworkPolicy must not be told to install it again.
    #[test]
    fn configured_but_unreachable_is_distinct_from_not_installed() {
        let down = MeshStatus {
            configured: true,
            url: Some("http://velox-mesh:4320".into()),
            reachable: Some(false),
            error: Some("connection refused".into()),
        };
        assert_ne!(down.configured, MeshStatus::default().configured);
        assert!(down.error.is_some(), "the operator is told why");
        assert!(down.url.is_some(), "and what was tried");
    }
}
