// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Hourly upstream version check (ADR-048 rev. 2).
//!
//! A background poller asks the public registry which OpenSearch versions
//! exist, keeps the newest stable one in memory, and the deployment screen
//! turns that into an "Upgrade v3.8.0" tag when it is a legal upgrade for that
//! deployment.
//!
//! This is a deliberate amendment to ADR-048, which rejected "query the
//! registry at runtime" as the SOURCE of the target list. It still is not:
//! `upgrade::CATALOG` remains the tested list, a discovered version is labelled
//! as such (`note: "latest"`, not `"current"`), and it goes through the exact
//! same pre-flight — semver, no downgrade, at most one major ahead, and both
//! image tags resolvable. What changes is only that the app now *tells the user
//! a newer version exists* instead of waiting for a VeloxSearch release to say
//! so.
//!
//! Properties that keep it honest:
//!  * **Never blocks anything.** No network, no answer, no suggestion — the
//!    catalog path is untouched and no screen degrades.
//!  * **Both images or nothing.** A tag that exists for the nodes but not for
//!    Dashboards is not offered: the two-phase upgrade would strand the
//!    deployment (ADR-048 invariant 3).
//!  * **In-memory only.** Nothing is written to the cluster, so a restart just
//!    re-checks; there is no stale state to reconcile.

use anyhow::{bail, Context, Result};
use std::sync::RwLock;

/// What the last successful check found.
#[derive(Debug, Clone, Default)]
pub struct Feed {
    /// Newest stable version that exists for BOTH images, "" when unknown.
    pub version: String,
    /// The newest few usable versions, newest first — what the create wizard
    /// offers as its version choices.
    pub recent: Vec<String>,
    /// Unix seconds of the last successful check (0 = never).
    pub checked_at: u64,
    /// Last error, when the most recent check failed. Shown nowhere by
    /// default — it exists so a support question has an answer.
    pub error: String,
}

fn cell() -> &'static RwLock<Feed> {
    static FEED: std::sync::OnceLock<RwLock<Feed>> = std::sync::OnceLock::new();
    FEED.get_or_init(|| RwLock::new(Feed::default()))
}

/// The newest version discovered upstream ("" when we never got an answer).
pub fn latest() -> String {
    cell().read().map(|f| f.version.clone()).unwrap_or_default()
}

/// Snapshot of the feed (for diagnostics / the API).
pub fn snapshot() -> Feed {
    cell().read().map(|f| f.clone()).unwrap_or_default()
}

/// Is this one of the versions the last check discovered? Used by the upgrade
/// pre-flight and by the create flow so a version WE offered is accepted
/// without the "untested" override — the user is clicking what we showed them,
/// not typing a version.
pub fn is_discovered(version: &str) -> bool {
    let v = version.trim();
    !v.is_empty()
        && cell()
            .read()
            .map(|f| f.version == v || f.recent.iter().any(|r| r == v))
            .unwrap_or(false)
}

/// How many versions the create wizard offers.
pub const OFFER_COUNT: usize = 3;

/// Versions to offer when creating a deployment, newest first: what the hourly
/// check found, falling back to the pinned catalog when it never answered
/// (offline install — the wizard must still work).
pub fn create_choices() -> Vec<String> {
    let discovered = cell().read().map(|f| f.recent.clone()).unwrap_or_default();
    if !discovered.is_empty() {
        return discovered;
    }
    crate::upgrade::CATALOG
        .iter()
        .take(OFFER_COUNT)
        .map(|e| e.version.to_string())
        .collect()
}

/// The version to suggest for a deployment running `current`: the discovered
/// one, when it is a legal upgrade from it. Empty otherwise (including when the
/// deployment already runs it).
pub fn suggestion_for(current: &str) -> String {
    let latest = latest();
    if latest.is_empty() || crate::upgrade::validate(current, &latest).is_err() {
        return String::new();
    }
    latest
}

/// Poll interval. `VELOX_VERSION_CHECK_SECS=0` turns the check off entirely
/// (air-gapped installs, or anyone who does not want the app reaching out).
fn interval_secs() -> Option<u64> {
    match std::env::var("VELOX_VERSION_CHECK_SECS").ok().as_deref() {
        None => Some(3600),
        Some("0") | Some("off") => None,
        // A too-small interval would hammer the registry for nothing: versions
        // are published weekly at best.
        Some(s) => Some(s.parse::<u64>().unwrap_or(3600).max(300)),
    }
}

/// Background hourly check. Spawned from `main`; best-effort forever.
pub async fn run_poller() {
    let Some(secs) = interval_secs() else {
        tracing::info!("upstream version check disabled (VELOX_VERSION_CHECK_SECS=0)");
        return;
    };
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(secs));
    tracing::info!("upstream version check started (every {secs}s)");
    loop {
        tick.tick().await;
        match check_once().await {
            Ok(v) => tracing::info!("upstream version check: usable versions {v:?}"),
            Err(e) => tracing::debug!("upstream version check failed: {e:#}"),
        }
    }
}

/// One check: list the node image's tags, take the newest stable versions that
/// are at most one major ahead of what we ship, and keep only those whose
/// Dashboards image carries the same tag.
pub async fn check_once() -> Result<Vec<String>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let result = discover().await;
    match &result {
        Ok(vs) => {
            if let Ok(mut f) = cell().write() {
                f.version = vs.first().cloned().unwrap_or_default();
                f.recent = vs.clone();
                f.checked_at = now;
                f.error.clear();
            }
        }
        Err(e) => {
            if let Ok(mut f) = cell().write() {
                f.error = format!("{e:#}");
            }
        }
    }
    result
}

async fn discover() -> Result<Vec<String>> {
    let tags = list_tags(crate::upgrade::IMAGE_NODES).await?;
    // Bounded by the same rule the operator enforces, measured from the version
    // we ship: never suggest a jump the operator would refuse anyway.
    let base = crate::upgrade::Version::parse(crate::upgrade::DEFAULT_VERSION)?;
    let mut candidates = crate::upgrade::stable_versions(&tags);
    candidates.retain(|v| v.major <= base.major + 1);
    candidates.sort();
    // Newest first, keeping only tags that ALSO exist for Dashboards — one
    // image without the other would strand a two-phase upgrade, and would give
    // the create wizard a version it cannot actually deploy.
    let mut out = Vec::new();
    for v in candidates.iter().rev() {
        if out.len() == OFFER_COUNT {
            break;
        }
        let tag = v.to_string();
        if image_tag_exists(crate::upgrade::IMAGE_DASHBOARDS, &tag).await? {
            out.push(tag);
        } else {
            tracing::debug!("skipping {tag}: no matching dashboards image");
        }
    }
    if out.is_empty() {
        bail!("no usable version found upstream");
    }
    Ok(out)
}

/// Tag names of a public Docker Hub repository (paged, newest first).
async fn list_tags(repo: &str) -> Result<Vec<String>> {
    let http = client()?;
    let mut out = Vec::new();
    let mut url = format!("https://hub.docker.com/v2/repositories/{repo}/tags?page_size=100&ordering=last_updated");
    // Two pages is 200 tags — far past every 3.x release, and bounded so a
    // paginated registry can never turn this into an endless walk.
    for _ in 0..2 {
        let body: serde_json::Value = http
            .get(&url)
            .send()
            .await
            .context("registry tag listing unreachable")?
            .json()
            .await
            .context("registry tag listing was not JSON")?;
        for r in body.get("results").and_then(|v| v.as_array()).unwrap_or(&vec![]) {
            if let Some(n) = r.get("name").and_then(|v| v.as_str()) {
                out.push(n.to_string());
            }
        }
        match body.get("next").and_then(|v| v.as_str()) {
            Some(next) if !next.is_empty() => url = next.to_string(),
            _ => break,
        }
    }
    if out.is_empty() {
        bail!("registry returned no tags for {repo}");
    }
    Ok(out)
}

/// Does this tag exist in the public registry? `Ok(false)` is a definitive "no
/// such tag"; `Err` means we could not tell (offline / air-gapped install).
/// Used both by the feed and by the upgrade pre-flight (ADR-048 invariant 3).
pub async fn image_tag_exists(repo: &str, tag: &str) -> Result<bool> {
    let http = client()?;
    // Anonymous pull token (Docker Hub hands one to everybody for public repos).
    let token: serde_json::Value = http
        .get(format!(
            "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:pull"
        ))
        .send()
        .await
        .context("registry auth unreachable")?
        .json()
        .await
        .context("registry auth returned no token")?;
    let token = token
        .get("token")
        .and_then(|v| v.as_str())
        .context("registry auth returned no token")?;
    let resp = http
        .head(format!("https://registry-1.docker.io/v2/{repo}/manifests/{tag}"))
        .bearer_auth(token)
        .header(
            "Accept",
            "application/vnd.oci.image.index.v1+json, \
             application/vnd.docker.distribution.manifest.list.v2+json, \
             application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await
        .context("registry unreachable")?;
    if resp.status().is_success() {
        return Ok(true);
    }
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    bail!("registry answered {} for {repo}:{tag}", resp.status())
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .context("building registry client")
}
