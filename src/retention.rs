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
