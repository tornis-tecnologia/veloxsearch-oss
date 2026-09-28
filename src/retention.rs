// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Default retention per purpose (ADR-062): the installation-wide knob, and
//! the per-deployment value stamped on the CR.
//!
//! Three layers, each with one owner:
//!   - the **installation default** — how many days an `observability` or a
//!     `security` deployment keeps its logs unless someone says otherwise.
//!     Stored next to the access settings in the `veloxsearch-config`
//!     ConfigMap (ADR-027's precedent for installation-wide settings), written
//!     only by the admin;
//!   - the **deployment's value** — stamped on the CR as an annotation at
//!     create (ADR-041: per-deployment configuration lives on the CR). The
//!     applier reads this, never the installation default, so changing the
//!     default never silently moves an existing deployment;
//!   - the **live policy** inside OpenSearch — the user's to edit. Whether
//!     velox still owns it is decided in `profiles.rs`.
//!
//! `search` has no retention at all (ADR-028): its data is kept until the user
//! deletes it, so it has no default and no knob.

use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, Patch, PatchParams};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Days a deployment keeps its logs, as chosen at create (or by an explicit
/// reset / apply-default). Rendered as an ISM age, e.g. `30d`.
pub const ANNOTATION: &str = "veloxsearch.ai/retention";
/// `<_seq_no>:<_primary_term>` of the retention policy velox last wrote. A
/// live policy with any other pair was written by someone else.
pub const STAMP_ANNOTATION: &str = "veloxsearch.ai/retention-stamp";
/// Whether [`ANNOTATION`] inherits the installation default (`default`) or
/// was chosen for this deployment (`override`). Only an inheriting deployment
/// follows the admin's "apply default to existing deployments".
pub const SOURCE_ANNOTATION: &str = "veloxsearch.ai/retention-source";

/// Where a deployment's retention value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Inherits the installation default; moves when the admin applies it.
    Default,
    /// Chosen for this deployment (the wizard's override); only the user's
    /// own "restore default" turns it back into [`Source::Default`].
    Override,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::Override => "override",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "default" => Some(Source::Default),
            "override" => Some(Source::Override),
            _ => None,
        }
    }
}

/// Does this deployment inherit the installation default, or was its value
/// chosen for it?
///
/// The source annotation decides whenever it is present. Without one the CR
/// predates it, and the rule is: no value at all (created before ADR-062)
/// inherits, and so does a value equal to a default velox would have stamped:
/// the built-in 30/90, or the installation default in force now. Anything else
/// was chosen by a person and is an override. The CR does not record which
/// default was in force at create time, so a deployment created on a default
/// the admin has since changed reads as an override. That is the safe side:
/// it is skipped, not rewritten.
pub fn source_of(
    purpose: &str,
    value: Option<&str>,
    source: Option<&str>,
    current: &Defaults,
) -> Source {
    if let Some(s) = source.and_then(Source::parse) {
        return s;
    }
    let Some(days) = value.and_then(parse_age) else {
        return Source::Default;
    };
    let builtin = Defaults::default().for_purpose(purpose);
    if Some(days) == builtin || Some(days) == current.for_purpose(purpose) {
        Source::Default
    } else {
        Source::Override
    }
}

/// The out-of-box defaults — exactly what every deployment got before the
/// knob existed, so an installation that never touches it changes nothing.
pub const BUILTIN_OBSERVABILITY_DAYS: u32 = 30;
pub const BUILTIN_SECURITY_DAYS: u32 = 90;
/// Bounds on a retention age. A day is ISM's natural unit here (the rollover
/// ages are already days); ten years is far past any log-retention need and
/// keeps a typo from reading as "forever".
pub const MIN_DAYS: u32 = 1;
pub const MAX_DAYS: u32 = 3650;

const KEY_OBSERVABILITY: &str = "retention_observability_days";
const KEY_SECURITY: &str = "retention_security_days";
/// A field manager of its own. `access::set` server-side-applies the same
/// ConfigMap as `veloxsearch` with only the access keys, so an apply under
/// that manager without them would prune ours — and ours would prune theirs.
const FIELD_MANAGER: &str = "veloxsearch-retention";

/// The installation default, per purpose that has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defaults {
    pub observability_days: u32,
    pub security_days: u32,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            observability_days: BUILTIN_OBSERVABILITY_DAYS,
            security_days: BUILTIN_SECURITY_DAYS,
        }
    }
}

impl Defaults {
    /// Days for a purpose, or `None` for `search`, which keeps data forever.
    /// Anything unknown is `observability` — the same fallback
    /// `profiles::apply` uses for legacy, unlabeled deployments.
    pub fn for_purpose(&self, purpose: &str) -> Option<u32> {
        match purpose {
            "search" => None,
            "security" => Some(self.security_days),
            _ => Some(self.observability_days),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_days(self.observability_days)?;
        validate_days(self.security_days)?;
        Ok(())
    }

    /// Read out of the ConfigMap data. A missing or unreadable key is the
    /// built-in value: a hand-edited ConfigMap must degrade to today's
    /// behaviour, not fail every create.
    fn from_data(data: &BTreeMap<String, String>) -> Self {
        let pick = |k: &str, builtin: u32| {
            data.get(k)
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|d| validate_days(*d).is_ok())
                .unwrap_or(builtin)
        };
        Self {
            observability_days: pick(KEY_OBSERVABILITY, BUILTIN_OBSERVABILITY_DAYS),
            security_days: pick(KEY_SECURITY, BUILTIN_SECURITY_DAYS),
        }
    }
}

pub fn validate_days(days: u32) -> Result<u32, String> {
    if (MIN_DAYS..=MAX_DAYS).contains(&days) {
        Ok(days)
    } else {
        Err(format!(
            "retention must be between {MIN_DAYS} and {MAX_DAYS} days, got {days}"
        ))
    }
}

/// Days → the ISM age the policy and the annotation carry.
pub fn render_age(days: u32) -> String {
    format!("{days}d")
}

/// The annotation value back into days. Only the `<n>d` form velox writes is
/// accepted; anything else reads as "no value" and the caller falls back.
pub fn parse_age(value: &str) -> Option<u32> {
    value
        .trim()
        .strip_suffix('d')
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|d| validate_days(*d).is_ok())
}

/// The days a deployment's retention policy should carry.
///
/// The CR annotation wins. Without one — every deployment created before
/// ADR-062 — the answer is the BUILT-IN value, not the installation default:
/// those deployments already run 30/90 days, and a save must not quietly move
/// them to whatever the admin set since. Moving them is the explicit
/// "apply default to existing deployments" action.
pub fn effective_days(purpose: &str, annotation: Option<&str>) -> Option<u32> {
    let builtin = Defaults::default().for_purpose(purpose)?;
    Some(annotation.and_then(parse_age).unwrap_or(builtin))
}

/// Read the installation default. `Ok(built-in)` when the ConfigMap or the
/// keys are absent; `Err` only on a real API failure.
pub async fn get() -> Result<Defaults> {
    let client = crate::k8s::client().await?;
    let api: Api<ConfigMap> = Api::namespaced(client, crate::k8s::ns());
    let cm = api
        .get_opt(crate::access::CONFIG_MAP)
        .await
        .context("reading the retention defaults")?;
    Ok(cm
        .and_then(|cm| cm.data)
        .map(|d| Defaults::from_data(&d))
        .unwrap_or_default())
}

/// Write the installation default. Touches only its two keys (see
/// [`FIELD_MANAGER`]) and no deployment — existing deployments move only
/// through the explicit apply action.
pub async fn set(defaults: &Defaults) -> Result<()> {
    defaults.validate().map_err(anyhow::Error::msg)?;
    let client = crate::k8s::client().await?;
    let api: Api<ConfigMap> = Api::namespaced(client, crate::k8s::ns());
    let cm = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": crate::access::CONFIG_MAP, "namespace": crate::k8s::ns() },
        "data": {
            KEY_OBSERVABILITY: defaults.observability_days.to_string(),
            KEY_SECURITY: defaults.security_days.to_string(),
        }
    });
    api.patch(
        crate::access::CONFIG_MAP,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&cm),
    )
    .await
    .context("saving the retention defaults")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Apply-once, shared by every retention policy velox writes (ADR-062 §3)
// ---------------------------------------------------------------------------
//
// `velox-retention` (profiles.rs) and the OTel stack's three policies
// (otel_stack.rs) follow one rule: velox writes the policy, stamps what it
// wrote, and leaves it alone once anyone else has written it. The rule lives
// here once; each caller supplies only its policy body, how it describes the
// age it wrote, and what "already current" means for its patterns.

/// `<id>=<_seq_no>:<_primary_term>` pairs, comma-separated: what velox last
/// wrote to each of the OTel stack's ISM policies. One annotation for the
/// three, like `integration-versions`; the per-policy analogue of
/// [`STAMP_ANNOTATION`].
pub const OTEL_STAMP_ANNOTATION: &str = "veloxsearch.ai/otel-retention-stamps";

/// What happened to one retention policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionOutcome {
    /// There was none; velox created it.
    Installed,
    /// velox's own policy, rewritten to the requested age.
    Updated,
    /// velox's own policy, already at the requested age.
    Unchanged,
    /// Someone edited it inside OpenSearch; left exactly as they left it.
    Customized,
}

impl RetentionOutcome {
    /// Wire name for the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
            Self::Customized => "customized",
        }
    }
}

/// `<_seq_no>:<_primary_term>` of an ISM policy document, as both the GET and
/// the PUT responses carry it. Every write to the document moves `_seq_no`,
/// which is what makes the pair a precise "did anyone else write this" check —
/// no normalization of what OpenSearch echoes back is involved.
pub fn policy_stamp(doc: &serde_json::Value) -> Option<String> {
    Some(format!(
        "{}:{}",
        doc["_seq_no"].as_i64()?,
        doc["_primary_term"].as_i64()?
    ))
}

/// The age at which the policy deletes, wherever the delete transition sits.
pub fn delete_age(doc: &serde_json::Value) -> Option<&str> {
    doc["policy"]["states"]
        .as_array()?
        .iter()
        .filter_map(|s| s["transitions"].as_array())
        .flatten()
        .find(|t| t["state_name"] == "delete")?["conditions"]["min_index_age"]
        .as_str()
}

/// Has anyone but velox written this live policy?
///
/// With a stamp — every policy velox wrote since ADR-062 — the answer is exact:
/// the live document's `_seq_no:_primary_term` is still the one velox's own
/// write produced, or someone else wrote it since.
///
/// Without one (a policy written before ADR-062, or a stamp that failed to
/// save) the answer is a heuristic, stated as such: velox's description
/// (`describe(age)`) names the age it wrote, so a policy whose description is
/// not velox's, or whose delete age no longer matches it, was edited. An edit
/// that keeps both is not detectable there — velox then overwrites it once and
/// stamps it, which is exactly what every save did before ADR-062.
pub fn is_customized(
    live: &serde_json::Value,
    stamp: Option<&str>,
    describe: fn(&str) -> String,
) -> bool {
    if let Some(stamp) = stamp.filter(|s| !s.is_empty()) {
        return policy_stamp(live).as_deref() != Some(stamp);
    }
    match delete_age(live) {
        Some(age) => live["policy"]["description"] != describe(age).as_str(),
        None => true,
    }
}

/// One ISM policy velox writes, as [`apply_once`] and [`live_state`] need it.
pub struct IsmPolicy<'a> {
    /// `<base>/_plugins/_ism/policies/<id>`.
    pub url: &'a str,
    /// For error messages.
    pub id: &'a str,
    pub body: &'a serde_json::Value,
    /// How velox's description names an age (the unstamped heuristic).
    pub describe: fn(&str) -> String,
}

/// Write `policy` unless someone else owns it now (ADR-062 §3).
///
/// Absent → create. Present and velox's own → rewrite only when `is_current`
/// says the live document differs. Present and customized → leave alone,
/// unless `force` (the explicit "restore default"). A rewrite is conditional on
/// the `_seq_no`/`_primary_term` just read, so a user edit landing in between
/// is refused rather than overwritten.
///
/// Returns the outcome and the stamp of the document velox now stands behind
/// (the one it wrote, or the live one when unchanged); `None` when customized.
/// Recording that stamp is the caller's: it knows which annotation holds it.
pub async fn apply_once(
    c: &reqwest::Client,
    auth: (&str, &str),
    policy: &IsmPolicy<'_>,
    stamp: Option<&str>,
    force: bool,
    is_current: impl Fn(&serde_json::Value) -> bool,
) -> Result<(RetentionOutcome, Option<String>)> {
    let (u, p) = auth;
    let id = policy.id;
    let resp = c
        .get(policy.url)
        .basic_auth(u, Some(p))
        .send()
        .await
        .with_context(|| format!("reading the ISM policy {id}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        // A concurrent writer that created it first makes this a 409; the
        // next attempt then finds the policy and decides from there.
        let doc = c
            .put(policy.url)
            .basic_auth(u, Some(p))
            .json(policy.body)
            .send()
            .await
            .with_context(|| format!("creating the ISM policy {id}"))?
            .error_for_status()
            .with_context(|| format!("ISM policy {id} rejected"))?
            .json::<serde_json::Value>()
            .await
            .unwrap_or_default();
        return Ok((RetentionOutcome::Installed, policy_stamp(&doc)));
    }
    let live: serde_json::Value = resp
        .error_for_status()
        .with_context(|| format!("reading the ISM policy {id}"))?
        .json()
        .await
        .with_context(|| format!("parsing the ISM policy {id}"))?;
    if !force && is_customized(&live, stamp, policy.describe) {
        return Ok((RetentionOutcome::Customized, None));
    }
    if !force && is_current(&live) {
        return Ok((RetentionOutcome::Unchanged, policy_stamp(&live)));
    }
    let (seq, term) = (
        live["_seq_no"].as_i64().unwrap_or_default(),
        live["_primary_term"].as_i64().unwrap_or_default(),
    );
    let doc = c
        .put(format!(
            "{}?if_seq_no={seq}&if_primary_term={term}",
            policy.url
        ))
        .basic_auth(u, Some(p))
        .json(policy.body)
        .send()
        .await
        .with_context(|| format!("updating the ISM policy {id}"))?
        .error_for_status()
        .with_context(|| format!("ISM policy {id} update rejected"))?
        .json::<serde_json::Value>()
        .await
        .unwrap_or_default();
    Ok((RetentionOutcome::Updated, policy_stamp(&doc)))
}

/// Where a live policy stands, read-only: `absent`, `managed` (velox's own,
/// still as velox wrote it) or `customized`, with the days it deletes at.
pub async fn live_state(
    c: &reqwest::Client,
    auth: (&str, &str),
    url: &str,
    stamp: Option<&str>,
    describe: fn(&str) -> String,
) -> Result<(&'static str, Option<u32>)> {
    let (u, p) = auth;
    let resp = c
        .get(url)
        .basic_auth(u, Some(p))
        .send()
        .await
        .context("reading the ISM policy")?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(("absent", None));
    }
    let live: serde_json::Value = resp
        .error_for_status()
        .context("reading the ISM policy")?
        .json()
        .await
        .context("parsing the ISM policy")?;
    let days = delete_age(&live).and_then(parse_age);
    let state = if is_customized(&live, stamp, describe) {
        "customized"
    } else {
        "managed"
    };
    Ok((state, days))
}

/// The OTel stamps annotation back into `id → seq:term`. Garbled entries are
/// dropped: a missing stamp falls back to the description heuristic, never to
/// a guess.
pub fn parse_stamps(value: Option<&str>) -> BTreeMap<String, String> {
    value
        .unwrap_or_default()
        .split(',')
        .filter_map(|kv| kv.trim().split_once('='))
        .filter(|(k, v)| !k.is_empty() && v.contains(':'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub fn render_stamps(stamps: &BTreeMap<String, String>) -> String {
    stamps
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Days the OTel stack's policies should carry on a deployment.
///
/// The deployment's own value (the CR annotation) first — the stack's indices
/// are kept exactly as long as the recipe indices. A CR without one (created
/// before ADR-062) falls back to the installation default, `defaults`, which
/// the caller passes as `None` when it could not be read; then the built-in.
///
/// This differs on purpose from [`effective_days`], which falls back to the
/// built-in: that function guards a policy the deployment already runs. The
/// stack's policies are written only on an explicit action (install, apply
/// default, restore default), and an explicit action takes the default.
///
/// The stack's telemetry is observability data whatever the deployment is
/// for, so a `search` deployment (which has no retention of its own and no
/// value on its CR) keeps it for the observability default.
pub fn otel_days(purpose: &str, annotation: Option<&str>, defaults: Option<&Defaults>) -> u32 {
    let purpose = if purpose == "security" {
        "security"
    } else {
        "observability"
    };
    let builtin = if purpose == "security" {
        BUILTIN_SECURITY_DAYS
    } else {
        BUILTIN_OBSERVABILITY_DAYS
    };
    annotation
        .and_then(parse_age)
        .or_else(|| defaults.and_then(|d| d.for_purpose(purpose)))
        .unwrap_or(builtin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_out_of_box_default_is_what_every_deployment_already_had() {
        let d = Defaults::default();
        assert_eq!(d.for_purpose("observability"), Some(30));
        assert_eq!(d.for_purpose("security"), Some(90));
        assert_eq!(d.for_purpose("search"), None, "search keeps data forever");
        assert_eq!(
            d.for_purpose(""),
            Some(30),
            "legacy/unlabeled = observability"
        );
    }

    #[test]
    fn the_annotation_round_trips() {
        for days in [MIN_DAYS, 7, 30, 90, 365, MAX_DAYS] {
            assert_eq!(parse_age(&render_age(days)), Some(days));
        }
        assert_eq!(render_age(45), "45d");
        assert_eq!(parse_age(" 14d "), Some(14));
        // Anything velox did not write reads as "no value", never as a guess.
        for bad in ["", "d", "30", "30h", "-3d", "0d", "99999d", "thirty"] {
            assert_eq!(parse_age(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_deployment_uses_its_own_value_and_legacy_ones_keep_the_builtin() {
        assert_eq!(effective_days("observability", Some("14d")), Some(14));
        assert_eq!(effective_days("security", Some("365d")), Some(365));
        // No annotation (pre-ADR-062) or a garbled one: the built-in value the
        // deployment already runs — never the current installation default.
        assert_eq!(effective_days("observability", None), Some(30));
        assert_eq!(effective_days("security", Some("junk")), Some(90));
        // search has no retention, whatever the CR says.
        assert_eq!(effective_days("search", Some("30d")), None);
    }

    #[test]
    fn the_configmap_degrades_to_the_builtin_per_key() {
        let mut data = BTreeMap::new();
        assert_eq!(Defaults::from_data(&data), Defaults::default());
        data.insert(KEY_OBSERVABILITY.to_string(), "14".to_string());
        data.insert(KEY_SECURITY.to_string(), "not a number".to_string());
        assert_eq!(
            Defaults::from_data(&data),
            Defaults {
                observability_days: 14,
                security_days: 90
            }
        );
        data.insert(KEY_SECURITY.to_string(), "0".to_string());
        assert_eq!(Defaults::from_data(&data).security_days, 90, "out of range");
    }

    /// The admin's apply touches only inheriting deployments, so the source
    /// decides it. An explicit annotation is final; a CR from before it
    /// existed inherits only when its value is one velox would have stamped.
    #[test]
    fn a_deployment_inherits_the_default_unless_it_chose_its_own() {
        let now = Defaults {
            observability_days: 21,
            security_days: 60,
        };
        // Explicit source wins, whatever the value.
        assert_eq!(
            source_of("observability", Some("7d"), Some("default"), &now),
            Source::Default
        );
        assert_eq!(
            source_of("observability", Some("21d"), Some("override"), &now),
            Source::Override
        );
        // Pre-ADR-062: no value at all → inherits (runs the built-in).
        assert_eq!(
            source_of("observability", None, None, &now),
            Source::Default
        );
        // No source annotation: equal to the built-in or the current default
        // → inherited; anything else → the person's choice.
        assert_eq!(
            source_of("observability", Some("30d"), None, &now),
            Source::Default
        );
        assert_eq!(
            source_of("security", Some("60d"), None, &now),
            Source::Default
        );
        assert_eq!(
            source_of("observability", Some("7d"), None, &now),
            Source::Override,
            "the live case: B created with 7d must not be moved to 21d"
        );
        // A garbled source reads as absent, not as an error.
        assert_eq!(
            source_of("observability", Some("7d"), Some("junk"), &now),
            Source::Override
        );
        for s in [Source::Default, Source::Override] {
            assert_eq!(Source::parse(s.as_str()), Some(s));
        }
    }

    /// The OTel stack's fallback chain: the deployment's value, else the
    /// installation default, else (default unreadable) the built-in.
    #[test]
    fn the_otel_stack_falls_back_value_then_default_then_builtin() {
        let now = Defaults {
            observability_days: 21,
            security_days: 60,
        };
        assert_eq!(otel_days("observability", Some("7d"), Some(&now)), 7);
        assert_eq!(otel_days("observability", None, Some(&now)), 21);
        assert_eq!(otel_days("security", Some("junk"), Some(&now)), 60);
        assert_eq!(otel_days("observability", None, None), 30);
        assert_eq!(otel_days("security", None, None), 90);
        // Telemetry is observability data on any deployment: search and
        // legacy/unlabeled ones use the observability default.
        assert_eq!(otel_days("search", None, Some(&now)), 21);
        assert_eq!(otel_days("", None, None), 30);
    }

    #[test]
    fn otel_stamps_round_trip_and_drop_garbage() {
        let mut m = BTreeMap::new();
        m.insert("logs-policy".to_string(), "3:1".to_string());
        m.insert("raw-span-policy".to_string(), "12:2".to_string());
        assert_eq!(render_stamps(&m), "logs-policy=3:1,raw-span-policy=12:2");
        assert_eq!(parse_stamps(Some(&render_stamps(&m))), m);
        assert!(parse_stamps(None).is_empty());
        assert_eq!(
            parse_stamps(Some("logs-policy=3:1,=4:1,raw-span-policy=nope,junk")).len(),
            1
        );
    }

    #[test]
    fn days_are_bounded() {
        assert!(validate_days(0).is_err());
        assert!(validate_days(MAX_DAYS + 1).is_err());
        assert_eq!(validate_days(30), Ok(30));
        assert!(Defaults {
            observability_days: 0,
            security_days: 90
        }
        .validate()
        .is_err());
    }
}
