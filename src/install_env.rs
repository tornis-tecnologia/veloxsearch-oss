// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The binary runs on whatever `veloxsearch-env` the install already has
//! (#128, ADR-057).
//!
//! `upgrade.yaml` does not carry the `veloxsearch-env` ConfigMap: the operator's
//! settings there survive an upgrade. The price is that a new binary starts on
//! a ConfigMap an OLDER `install.yaml` created, so it must not need a key only
//! a newer one adds. Two properties of `deploy/install.yaml` hold that:
//!
//! 1. No env var is sourced with a non-optional `configMapKeyRef`. A missing
//!    key there is `CreateContainerConfigError`, i.e. the upgrade never starts.
//!    `envFrom` is fine: an absent key is simply an unset variable.
//! 2. Every key the shipped ConfigMap sets has a default in code, and that
//!    default behaves exactly like the shipped value. An install that predates
//!    the key then runs as a fresh install would.
//!
//! A new key in `veloxsearch-env` fails (2) until it has an entry in
//! [`DEFAULTS`], which forces the question "what does the binary do without
//! it?" at review time rather than on someone's upgrade.

use serde::Deserialize;
use serde_yaml::Value;

const INSTALL_YAML: &str = include_str!("../deploy/install.yaml");

fn docs() -> Vec<Value> {
    serde_yaml::Deserializer::from_str(INSTALL_YAML)
        .filter_map(|d| Value::deserialize(d).ok())
        .filter(|v| !v.is_null())
        .collect()
}

fn named<'a>(docs: &'a [Value], kind: &str, name: &str) -> &'a Value {
    docs.iter()
        .find(|v| v["kind"].as_str() == Some(kind) && v["metadata"]["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("deploy/install.yaml has no {kind} {name}"))
}

fn seq(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_sequence().into_iter().flatten()
}

/// How the binary resolves one `veloxsearch-env` key, given its raw value
/// (`None` = absent from the Pod env). The result is rendered with `{:?}` so
/// settings of different types share one table. Each entry calls the same pure
/// function the runtime read goes through.
type Resolve = fn(Option<&str>) -> String;

const DEFAULTS: &[(&str, Resolve)] = &[
    // main.rs: EnvFilter::try_from_default_env().unwrap_or(DEFAULT_LOG_FILTER)
    ("RUST_LOG", |v| {
        v.unwrap_or(crate::DEFAULT_LOG_FILTER).to_string()
    }),
    ("VELOX_COOKIE_SECURE", |v| {
        format!("{:?}", crate::auth::cookie_secure_from(v))
    }),
    ("VELOX_PG_ENABLED", |v| {
        format!("{:?}", crate::db::flag_on(v.map(String::from)))
    }),
    ("VELOX_MULTITENANT_AUTH", |v| {
        format!("{:?}", crate::tenants::flag_on(v.map(String::from)))
    }),
    // No host is dev-mode (mail is logged, not sent).
    ("VELOX_SMTP_HOST", |v| {
        let host = crate::mail::setting(v.map(String::from));
        format!(
            "{:?}",
            crate::mail::resolve_smtp(host, None, None, None, None).ok()
        )
    }),
    // The TLS mode only matters once a host is set.
    ("VELOX_SMTP_TLS", |v| {
        let tls = crate::mail::setting(v.map(String::from));
        let relay = Some("relay.example".to_string());
        format!(
            "{:?}",
            crate::mail::resolve_smtp(relay, None, tls, None, None).ok()
        )
    }),
    ("VELOX_SMTP_USER", |v| {
        let user = crate::mail::setting(v.map(String::from));
        let relay = Some("relay.example".to_string());
        let pw = Some("pw".to_string());
        format!(
            "{:?}",
            crate::mail::resolve_smtp(relay, None, None, user, pw).ok()
        )
    }),
    ("VELOX_MAIL_FROM", |v| {
        crate::mail::from_address_from(v.map(String::from))
    }),
    ("VELOX_PUBLIC_URL", |v| {
        crate::mail::public_url_from(v.map(String::from))
    }),
];

#[test]
fn no_env_var_needs_a_configmap_key_to_exist() {
    let docs = docs();
    let mut checked = 0;
    for doc in &docs {
        let pod = &doc["spec"]["template"]["spec"];
        let containers = seq(&pod["containers"]).chain(seq(&pod["initContainers"]));
        for c in containers {
            for e in seq(&c["env"]) {
                let key_ref = &e["valueFrom"]["configMapKeyRef"];
                if key_ref.is_null() {
                    continue;
                }
                checked += 1;
                assert_eq!(
                    key_ref["optional"].as_bool(),
                    Some(true),
                    "{} {}: env {} reads ConfigMap key {:?} without `optional: true`; \
                     an upgrade whose ConfigMap predates that key would never start \
                     (#128). Mark it optional and default it in code, or use envFrom.",
                    doc["kind"].as_str().unwrap_or("?"),
                    doc["metadata"]["name"].as_str().unwrap_or("?"),
                    e["name"].as_str().unwrap_or("?"),
                    key_ref["key"].as_str().unwrap_or("?"),
                );
            }
        }
    }

    // Not vacuous: the app does take veloxsearch-env, through envFrom.
    let app = named(&docs, "Deployment", "veloxsearch");
    let env_from = seq(&app["spec"]["template"]["spec"]["containers"])
        .flat_map(|c| seq(&c["envFrom"]))
        .any(|f| f["configMapRef"]["name"].as_str() == Some("veloxsearch-env"));
    assert!(
        env_from,
        "the veloxsearch Deployment should take veloxsearch-env via envFrom \
         ({checked} configMapKeyRef env vars checked)"
    );
}

#[test]
fn every_veloxsearch_env_key_has_a_code_default_matching_the_shipped_value() {
    let docs = docs();
    let data = named(&docs, "ConfigMap", "veloxsearch-env")["data"]
        .as_mapping()
        .expect("veloxsearch-env has data");
    assert!(!data.is_empty());

    for (k, v) in data {
        let key = k.as_str().expect("string key");
        let shipped = v
            .as_str()
            .unwrap_or_else(|| panic!("{key}: value must be a string"));
        let Some((_, resolve)) = DEFAULTS.iter().find(|(name, _)| *name == key) else {
            panic!(
                "veloxsearch-env ships {key}, but install_env::DEFAULTS has no entry for it. \
                 Upgrades keep the operator's ConfigMap (#128), so an install from an older \
                 release will not have {key}: give it a default in code and add it here."
            );
        };
        assert_eq!(
            resolve(None),
            resolve(Some(shipped)),
            "{key}: the binary's default when the key is absent differs from the value \
             deploy/install.yaml ships ({shipped:?}). An upgraded install without the key \
             would behave unlike a fresh one (#128)."
        );
    }

    // Keep the table honest the other way too: no entry for a key that is gone.
    for (name, _) in DEFAULTS {
        assert!(
            data.contains_key(Value::String((*name).to_string())),
            "install_env::DEFAULTS lists {name}, which veloxsearch-env no longer ships"
        );
    }
}

/// The resolvers are real: a non-default value does change the outcome, so the
/// equality above is not two constants compared.
#[test]
fn each_default_is_distinguishable_from_a_non_default_value() {
    let non_default = [
        ("RUST_LOG", "debug"),
        ("VELOX_COOKIE_SECURE", "1"),
        ("VELOX_PG_ENABLED", "1"),
        ("VELOX_MULTITENANT_AUTH", "1"),
        ("VELOX_SMTP_HOST", "relay.example"),
        ("VELOX_SMTP_TLS", "tls"),
        ("VELOX_SMTP_USER", "mailer"),
        ("VELOX_MAIL_FROM", "Ops <ops@example.com>"),
        ("VELOX_PUBLIC_URL", "https://velox.example"),
    ];
    assert_eq!(non_default.len(), DEFAULTS.len());
    for (key, value) in non_default {
        let (_, resolve) = DEFAULTS
            .iter()
            .find(|(name, _)| *name == key)
            .unwrap_or_else(|| panic!("{key} not in DEFAULTS"));
        assert_ne!(
            resolve(None),
            resolve(Some(value)),
            "{key}: resolver ignores its input"
        );
    }
}
