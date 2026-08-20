// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Snapshot repository + scheduled snapshot policy — pure renderers and rules
//! (ADR-049).
//!
//! Pure: no cluster calls, no I/O. `k8s.rs` owns the writes (the CR slice under
//! its own field manager, the credentials Secret and the
//! `OpensearchSnapshotPolicy` CR) and `api.rs` exposes this to the UI.
//!
//! Two properties from the operator shape everything here:
//!
//! 1. **`settings` is `map[string]string`.** The API server rejects a boolean
//!    or a number inside a repository's settings, with an error that reads like
//!    an unrelated schema fault. So every value is stringified *by
//!    construction* — `repo_entry()` cannot emit a non-string (invariant 5).
//! 2. **Credentials live in the keystore, never in the repository body.** The
//!    `repository-s3` plugin reads `s3.client.<alias>.access_key` /
//!    `secret_key` from the keystore, which the operator populates from a
//!    Secret **at pod start**. That is why a credential change is the one edit
//!    that restarts the nodes (`needs_restart`), and why `repo_entry()` carries
//!    no key material at all (invariant 1).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// The OpenSearch snapshot repository name. Deliberately the same constant the
/// ISM `hot → snapshot → delete` policy already references
/// (`profiles.rs::SNAPSHOT_REPO`), so configuring a repository here makes that
/// policy's snapshot state work instead of silently no-op'ing.
pub const REPO_NAME: &str = "velox-snapshots";

/// The `repository-s3` client alias the keystore keys are scoped to. One
/// OpenSearch cluster per deployment means one alias is enough — per-deployment
/// isolation is the cluster boundary, not the alias.
pub const CLIENT_ALIAS: &str = "default";

/// Sentinel returned instead of a stored secret, and accepted back on save to
/// mean "keep what is stored" (the ADR-045 `SecretInput` contract).
pub const SECRET_KEPT: &str = "secret_kept";

/// Suffix of the per-deployment Secret holding the S3 credentials. The full
/// name is built by `k8s::snapshot_secret_name`, which is also what
/// `owned_secret_names()` must list so the Secret dies with the deployment.
pub const SECRET_SUFFIX: &str = "snapshot-s3";

/// Keys inside that Secret. They are mapped onto keystore entries by
/// `keystore_entry()`; the names only matter in that they must agree.
pub const KEY_ACCESS: &str = "accessKey";
pub const KEY_SECRET: &str = "secretKey";

/// The plugin that talks S3. **Not** bundled in `opensearchproject/opensearch`
/// — verified on 3.7.0: the distribution ships `repository-url` as a module and
/// no `repository-s3` anywhere (2026-08-07). So it has to be declared in the
/// CR's plugin lists, which is part of the pod spec — hence the rolling restart
/// on first configuration, and hence the rule that a deployment without
/// snapshots never carries this entry.
///
/// The operator installs it with `bin/opensearch-plugin install --batch`, which
/// downloads from `artifacts.opensearch.org`: an air-gapped cluster needs the
/// plugin pre-baked into a custom image (ADR-049).
pub const S3_PLUGIN: &str = "repository-s3";

/// Server-side encryption mode for the repository. **Must be set explicitly.**
///
/// Found live on 2026-08-07, against MinIO: leaving this unset makes
/// `repository-s3` ask for server-side encryption anyway, and MinIO answers
/// **501 "Server side encryption specified but KMS is not configured"** — every
/// snapshot fails with an error that names KMS, which nobody configured and
/// nobody asked for. `AES256` fails identically, because MinIO implements even
/// SSE-S3 through its KMS.
///
/// `bucket_default` is the honest answer for both worlds: it sends no
/// encryption directive and lets the bucket's own policy decide. On AWS that
/// means the bucket's default encryption (SSE-S3, on by default since 2023)
/// still applies; on MinIO without a KMS it means plain writes, which is what a
/// bucket with no encryption configured is asking for. Choosing SSE-KMS with a
/// specific key is an advanced case this form deliberately does not expose
/// (ADR-015, basics first).
///
/// The accepted values are `AES256`, `aws:kms` and `bucket_default` — an empty
/// string is rejected by OpenSearch, so this cannot be "unset".
pub const SSE_TYPE: &str = "bucket_default";

// ───────────────────────────── config ─────────────────────────────

/// The snapshot configuration of one deployment. Also the API DTO — the UI
/// edits exactly this object, so there is no second shape to keep in sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotConfig {
    /// Whether snapshots are configured at all. `false` is the default state of
    /// a deployment and a fully valid one (ADR-049 invariant 6).
    pub enabled: bool,
    pub bucket: String,
    /// Prefix inside the bucket. Empty means "the deployment name" — filled in
    /// by `with_defaults` so a tenant's deployments never collide in a shared
    /// bucket (the ADR-042 `base_path`-per-deployment rule).
    pub base_path: String,
    /// S3 endpoint. Empty = AWS S3 public endpoints (the plugin's default).
    pub endpoint: String,
    pub region: String,
    /// Required by MinIO and harmless against most external S3 — defaults on.
    pub path_style_access: bool,
    /// Write-only: read back as `SECRET_KEPT`, never in clear.
    pub access_key: String,
    /// Write-only: read back as `SECRET_KEPT`, never in clear.
    pub secret_key: String,
    pub policy: PolicyConfig,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bucket: String::new(),
            base_path: String::new(),
            endpoint: String::new(),
            region: "us-east-1".to_string(),
            path_style_access: true,
            access_key: String::new(),
            secret_key: String::new(),
            policy: PolicyConfig::default(),
        }
    }
}

/// The scheduled snapshot policy (`OpensearchSnapshotPolicy` → OpenSearch SLM).
/// Every default here is already a working daily backup, which is the point:
/// the wizard step is skippable *and* the user who enables it without opening
/// "Política padrão" gets something real.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    pub enabled: bool,
    /// 5-field cron, in `timezone`.
    pub cron: String,
    pub timezone: String,
    /// Index pattern to snapshot.
    pub indices: String,
    pub include_global_state: bool,
    /// Retention: drop snapshots older than this, but never below `min_count`.
    pub max_age_days: u32,
    pub max_count: u32,
    pub min_count: u32,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cron: "0 2 * * *".to_string(),
            timezone: "UTC".to_string(),
            indices: "*".to_string(),
            include_global_state: true,
            max_age_days: 7,
            max_count: 14,
            min_count: 3,
        }
    }
}

/// Live state of the configuration, read back from the CRs (never edited by the
/// UI). `repo_state`/`policy_state` carry the operator's own vocabulary —
/// `PENDING | CREATED | ERROR | IGNORED` — and `last_error` its `reason`
/// verbatim (ADR-045 UI rule 5).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotState {
    pub configured: bool,
    pub repo: String,
    pub schedule: String,
    pub policy_state: String,
    pub last_error: String,
}

/// The name of the `OpensearchSnapshotPolicy` CR (and of the SLM policy inside
/// OpenSearch) for one deployment.
pub fn policy_name(deployment: &str) -> String {
    format!("{deployment}-daily")
}

/// Fill in the defaults that depend on the deployment: an empty `base_path`
/// becomes the deployment name, an empty region the S3 default.
pub fn with_defaults(deployment: &str, cfg: &SnapshotConfig) -> SnapshotConfig {
    let mut out = cfg.clone();
    out.bucket = out.bucket.trim().to_string();
    out.endpoint = out.endpoint.trim().trim_end_matches('/').to_string();
    if out.base_path.trim().is_empty() {
        out.base_path = deployment.to_string();
    } else {
        out.base_path = out.base_path.trim().trim_matches('/').to_string();
    }
    if out.region.trim().is_empty() {
        out.region = "us-east-1".to_string();
    }
    out.policy.cron = out.policy.cron.trim().to_string();
    if out.policy.timezone.trim().is_empty() {
        out.policy.timezone = "UTC".to_string();
    }
    if out.policy.indices.trim().is_empty() {
        out.policy.indices = "*".to_string();
    }
    out
}

// ───────────────────────────── validation ─────────────────────────────

/// Refuse a configuration before anything is written (ADR-049 invariant 3).
/// Every message is a sentence an operator can act on — it is surfaced verbatim
/// as the 400 body.
pub fn validate(cfg: &SnapshotConfig) -> Result<()> {
    if !cfg.enabled {
        return Ok(());
    }
    let bucket = cfg.bucket.trim();
    if bucket.is_empty() {
        bail!("informe o bucket S3 onde os snapshots serão gravados.");
    }
    if !valid_bucket(bucket) {
        bail!(
            "bucket inválido ({bucket}): use 3 a 63 caracteres, apenas letras minúsculas, \
             números, ponto e hífen, começando e terminando com letra ou número."
        );
    }
    let endpoint = cfg.endpoint.trim();
    let scheme_ok = endpoint.starts_with("http://") || endpoint.starts_with("https://");
    if !endpoint.is_empty() && !scheme_ok {
        bail!(
            "endpoint inválido ({endpoint}): informe a URL completa, \
             por exemplo http://minio.minio.svc:9000 ou https://s3.amazonaws.com."
        );
    }
    if cfg.base_path.contains("..") {
        bail!("caminho base inválido: não pode conter '..'.");
    }
    if cfg.policy.enabled {
        validate_policy(&cfg.policy)?;
    }
    Ok(())
}

fn validate_policy(p: &PolicyConfig) -> Result<()> {
    let fields = p.cron.split_whitespace().count();
    if fields != 5 {
        bail!(
            "agendamento inválido ('{}'): use um cron de 5 campos, por exemplo '0 2 * * *' \
             (todo dia às 02:00).",
            p.cron
        );
    }
    if p.max_age_days == 0 {
        bail!("retenção inválida: guarde os snapshots por pelo menos 1 dia.");
    }
    if p.max_count == 0 {
        bail!("retenção inválida: o número máximo de snapshots precisa ser pelo menos 1.");
    }
    if p.min_count > p.max_count {
        bail!(
            "retenção inválida: o mínimo de snapshots ({}) não pode ser maior que o máximo ({}).",
            p.min_count,
            p.max_count
        );
    }
    if p.indices.trim().is_empty() {
        bail!("informe quais índices entram no snapshot ('*' para todos).");
    }
    Ok(())
}

/// S3 bucket naming rules, the subset every provider agrees on.
fn valid_bucket(b: &str) -> bool {
    let n = b.len();
    if !(3..=63).contains(&n) {
        return false;
    }
    if !b
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        return false;
    }
    let first = b.chars().next().unwrap_or(' ');
    let last = b.chars().last().unwrap_or(' ');
    let edge_ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    edge_ok(first) && edge_ok(last) && !b.contains("..")
}

// ───────────────────────────── renderers ─────────────────────────────

/// `http` / `https` for the repository's `protocol` setting, derived from the
/// endpoint. MinIO in-cluster is plain HTTP; anything else defaults to HTTPS.
pub fn protocol_of(endpoint: &str) -> &'static str {
    if endpoint.trim().starts_with("http://") {
        "http"
    } else {
        "https"
    }
}

/// The `spec.general.snapshotRepositories[]` entry.
///
/// **Every value is a string** — the CRD types `settings` as
/// `map[string]string` and the API server rejects anything else (invariant 5).
/// **No credential appears here** — they reach the node through the keystore
/// (invariant 1). Both properties are asserted structurally in the tests below,
/// because both failures are silent-looking in production.
pub fn repo_entry(cfg: &SnapshotConfig) -> serde_json::Value {
    let mut settings = serde_json::Map::new();
    let mut put = |k: &str, v: String| {
        settings.insert(k.to_string(), serde_json::Value::String(v));
    };
    put("bucket", cfg.bucket.clone());
    put("base_path", cfg.base_path.clone());
    put("client", CLIENT_ALIAS.to_string());
    put("region", cfg.region.clone());
    put("path_style_access", cfg.path_style_access.to_string());
    put("server_side_encryption_type", SSE_TYPE.to_string());
    if !cfg.endpoint.trim().is_empty() {
        // The plugin wants the host without the scheme, and the scheme as
        // `protocol` — passing "http://host:9000" as `endpoint` makes it build
        // "https://http://host:9000" and fail with an opaque DNS error.
        let host = cfg
            .endpoint
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        put("endpoint", host.to_string());
        put("protocol", protocol_of(&cfg.endpoint).to_string());
    }

    serde_json::json!({
        "name": REPO_NAME,
        "type": "s3",
        "settings": serde_json::Value::Object(settings),
    })
}

/// The `spec.general.keystore[]` entry mapping the deployment's Secret onto the
/// keystore keys `repository-s3` reads.
pub fn keystore_entry(secret_name: &str) -> serde_json::Value {
    serde_json::json!({
        "secret": { "name": secret_name },
        "keyMappings": {
            KEY_ACCESS: format!("s3.client.{CLIENT_ALIAS}.access_key"),
            KEY_SECRET: format!("s3.client.{CLIENT_ALIAS}.secret_key"),
        }
    })
}

/// The whole `OpensearchSnapshotPolicy` CR for one deployment.
///
/// `policyName` is always set explicitly: the CRD schema lists it as `required`
/// even though the operator docs call it optional, so omitting it is rejected
/// by the API server before the reconciler's `metadata.name` fallback is ever
/// reached.
pub fn policy_cr(namespace: &str, deployment: &str, cfg: &SnapshotConfig) -> serde_json::Value {
    let name = policy_name(deployment);
    let p = &cfg.policy;
    serde_json::json!({
        "apiVersion": "opensearch.org/v1",
        "kind": "OpensearchSnapshotPolicy",
        "metadata": { "name": name, "namespace": namespace },
        "spec": {
            "policyName": name,
            "enabled": true,
            "description": format!("VeloxSearch scheduled snapshot for {deployment}"),
            "opensearchCluster": { "name": deployment },
            "creation": {
                "schedule": { "cron": { "expression": p.cron, "timezone": p.timezone } },
                "timeLimit": "1h"
            },
            "deletion": {
                "schedule": { "cron": { "expression": "0 3 * * *", "timezone": p.timezone } },
                "timeLimit": "1h",
                "deleteCondition": {
                    "maxAge": format!("{}d", p.max_age_days),
                    "maxCount": p.max_count,
                    "minCount": p.min_count
                }
            },
            "snapshotConfig": {
                "repository": REPO_NAME,
                "indices": p.indices,
                "includeGlobalState": p.include_global_state,
                "ignoreUnavailable": true,
                "partial": false,
                "dateFormat": "yyyy-MM-dd-HH-mm",
                "dateFormatTimezone": p.timezone
            }
        }
    })
}

/// Human-readable schedule for the status chip ("diário 02:00"). Falls back to
/// the raw cron for anything that is not a plain daily schedule — better an
/// honest cron than a wrong sentence.
pub fn schedule_label(p: &PolicyConfig) -> String {
    let f: Vec<&str> = p.cron.split_whitespace().collect();
    if f.len() == 5 && f[2] == "*" && f[3] == "*" && f[4] == "*" {
        if let (Ok(min), Ok(hour)) = (f[0].parse::<u32>(), f[1].parse::<u32>()) {
            return format!("{hour:02}:{min:02}");
        }
    }
    p.cron.clone()
}

// ───────────────────────────── the restart rule ─────────────────────────────

/// Does applying `new` over `old` restart the nodes?
///
/// The keystore is populated **at pod start** and the operator has no reload
/// hook, so any change to the credentials — first configuration, rotation, or
/// turning snapshots off — only takes effect after the pods roll. Everything
/// else is reconciled against the live cluster with no restart: the repository
/// body is re-registered over the OpenSearch API, and the policy is a separate
/// CR entirely.
///
/// This is the one boolean the UI is handed; it never derives it (invariant 4).
pub fn needs_restart(old: Option<&SnapshotConfig>, new: &SnapshotConfig) -> bool {
    let was_on = old.map(|o| o.enabled).unwrap_or(false);
    if was_on != new.enabled {
        // Turning it on adds the keystore entry; turning it off removes it.
        return true;
    }
    if !new.enabled {
        return false;
    }
    // Enabled on both sides: only new key material moves the keystore. The
    // sentinel means "keep what is stored", i.e. no change.
    let changed = |v: &str| !v.is_empty() && v != SECRET_KEPT;
    changed(&new.access_key) || changed(&new.secret_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SnapshotConfig {
        SnapshotConfig {
            enabled: true,
            bucket: "velox-snap".into(),
            base_path: "logs-abcd".into(),
            endpoint: "http://minio.minio.svc:9000".into(),
            region: "us-east-1".into(),
            path_style_access: true,
            access_key: "AKIA".into(),
            secret_key: "s3cr3t".into(),
            policy: PolicyConfig::default(),
        }
    }

    /// ADR-049 invariant 1 (and the test ADR-042 specified and never got):
    /// the repository body carries no key material.
    #[test]
    fn repo_entry_has_no_credentials() {
        let v = repo_entry(&cfg());
        let s = serde_json::to_string(&v).unwrap();
        assert!(
            !s.contains("AKIA"),
            "access key leaked into the repo body: {s}"
        );
        assert!(
            !s.contains("s3cr3t"),
            "secret key leaked into the repo body: {s}"
        );
        for k in ["access_key", "secret_key", "accessKey", "secretKey"] {
            assert!(!s.contains(k), "credential key `{k}` in the repo body: {s}");
        }
        assert_eq!(v["type"], "s3");
        assert_eq!(v["name"], REPO_NAME);
        let set = v["settings"].as_object().unwrap();
        for k in ["bucket", "base_path", "endpoint", "path_style_access"] {
            assert!(set.contains_key(k), "missing setting `{k}`");
        }
    }

    /// Regression for the live failure of 2026-08-07: without this setting the
    /// plugin asks for server-side encryption on its own and MinIO refuses
    /// every write with a 501 about a KMS nobody configured.
    #[test]
    fn encryption_is_left_to_the_bucket() {
        let set = repo_entry(&cfg())["settings"].as_object().unwrap().clone();
        assert_eq!(
            set.get("server_side_encryption_type")
                .and_then(|v| v.as_str()),
            Some("bucket_default"),
            "the repository must not request SSE itself — MinIO rejects both \
             AES256 and aws:kms without a KMS"
        );
    }

    /// ADR-049 invariant 5: `settings` is `map[string]string` in the CRD — a
    /// bool or a number is rejected by the API server.
    #[test]
    fn every_setting_is_a_string() {
        for c in [
            cfg(),
            SnapshotConfig {
                path_style_access: false,
                endpoint: String::new(),
                ..cfg()
            },
        ] {
            for (k, v) in repo_entry(&c)["settings"].as_object().unwrap() {
                assert!(v.is_string(), "setting `{k}` is not a string: {v}");
            }
        }
    }

    #[test]
    fn endpoint_is_split_into_host_and_protocol() {
        let v = repo_entry(&cfg());
        assert_eq!(v["settings"]["endpoint"], "minio.minio.svc:9000");
        assert_eq!(v["settings"]["protocol"], "http");

        let https = SnapshotConfig {
            endpoint: "https://s3.amazonaws.com/".into(),
            ..cfg()
        };
        let v = repo_entry(&https);
        assert_eq!(v["settings"]["endpoint"], "s3.amazonaws.com");
        assert_eq!(v["settings"]["protocol"], "https");
    }

    /// No endpoint = AWS's own; the plugin resolves it from the region, so we
    /// must not emit an empty `endpoint` setting.
    #[test]
    fn empty_endpoint_is_omitted() {
        let c = SnapshotConfig {
            endpoint: String::new(),
            ..cfg()
        };
        let set = repo_entry(&c)["settings"].as_object().unwrap().clone();
        assert!(!set.contains_key("endpoint"));
        assert!(!set.contains_key("protocol"));
    }

    #[test]
    fn keystore_entry_maps_both_keys() {
        let v = keystore_entry("logs-abcd-snapshot-s3");
        assert_eq!(v["secret"]["name"], "logs-abcd-snapshot-s3");
        assert_eq!(v["keyMappings"][KEY_ACCESS], "s3.client.default.access_key");
        assert_eq!(v["keyMappings"][KEY_SECRET], "s3.client.default.secret_key");
    }

    /// The CRD lists `policyName` as required, contradicting the docs.
    #[test]
    fn policy_cr_always_names_the_policy() {
        let v = policy_cr("velox", "logs-abcd", &cfg());
        assert_eq!(v["spec"]["policyName"], "logs-abcd-daily");
        assert_eq!(v["metadata"]["name"], "logs-abcd-daily");
        assert_eq!(v["metadata"]["namespace"], "velox");
        assert_eq!(v["spec"]["opensearchCluster"]["name"], "logs-abcd");
        assert_eq!(v["spec"]["snapshotConfig"]["repository"], REPO_NAME);
        assert_eq!(
            v["spec"]["creation"]["schedule"]["cron"]["expression"],
            "0 2 * * *"
        );
        assert_eq!(v["spec"]["deletion"]["deleteCondition"]["maxAge"], "7d");
        assert_eq!(v["spec"]["deletion"]["deleteCondition"]["maxCount"], 14);
        assert_eq!(v["spec"]["deletion"]["deleteCondition"]["minCount"], 3);
    }

    #[test]
    fn policy_cr_carries_the_schedule_it_was_given() {
        let c = SnapshotConfig {
            policy: PolicyConfig {
                cron: "30 4 * * 0".into(),
                timezone: "America/Sao_Paulo".into(),
                max_age_days: 30,
                indices: "logs-*".into(),
                ..PolicyConfig::default()
            },
            ..cfg()
        };
        let v = policy_cr("velox", "logs-abcd", &c);
        assert_eq!(
            v["spec"]["creation"]["schedule"]["cron"]["expression"],
            "30 4 * * 0"
        );
        assert_eq!(
            v["spec"]["creation"]["schedule"]["cron"]["timezone"],
            "America/Sao_Paulo"
        );
        assert_eq!(v["spec"]["deletion"]["deleteCondition"]["maxAge"], "30d");
        assert_eq!(v["spec"]["snapshotConfig"]["indices"], "logs-*");
    }

    /// ADR-049 invariant 4: only credential transitions restart the nodes.
    #[test]
    fn needs_restart_only_on_credential_transitions() {
        let on = cfg();
        let stored = SnapshotConfig {
            access_key: SECRET_KEPT.into(),
            secret_key: SECRET_KEPT.into(),
            ..cfg()
        };

        // First configuration and turning it off: the keystore moves.
        assert!(needs_restart(None, &on));
        assert!(needs_restart(
            Some(&on),
            &SnapshotConfig {
                enabled: false,
                ..cfg()
            }
        ));
        // Rotation: new key material.
        assert!(needs_restart(Some(&on), &on));

        // Policy-only edit, credentials kept: nothing restarts.
        let policy_only = SnapshotConfig {
            policy: PolicyConfig {
                cron: "0 3 * * *".into(),
                ..PolicyConfig::default()
            },
            ..stored.clone()
        };
        assert!(!needs_restart(Some(&on), &policy_only));

        // Repository-body edit, credentials kept: still no restart.
        let body_only = SnapshotConfig {
            bucket: "outro-bucket".into(),
            base_path: "outro".into(),
            ..stored.clone()
        };
        assert!(!needs_restart(Some(&on), &body_only));

        // Never configured and still not: nothing to do.
        assert!(!needs_restart(None, &SnapshotConfig::default()));
    }

    #[test]
    fn validate_accepts_the_defaults_of_an_enabled_config() {
        assert!(validate(&cfg()).is_ok());
        // Disabled configs are never validated — an empty form must save.
        assert!(validate(&SnapshotConfig::default()).is_ok());
    }

    #[test]
    fn validate_refuses_bad_input() {
        let cases: Vec<(SnapshotConfig, &str)> = vec![
            (
                SnapshotConfig {
                    bucket: String::new(),
                    ..cfg()
                },
                "bucket",
            ),
            (
                SnapshotConfig {
                    bucket: "Bucket_Com_Maiuscula".into(),
                    ..cfg()
                },
                "bucket inválido",
            ),
            (
                SnapshotConfig {
                    endpoint: "minio.minio.svc:9000".into(),
                    ..cfg()
                },
                "endpoint inválido",
            ),
            (
                SnapshotConfig {
                    policy: PolicyConfig {
                        cron: "0 2 * *".into(),
                        ..PolicyConfig::default()
                    },
                    ..cfg()
                },
                "agendamento inválido",
            ),
            (
                SnapshotConfig {
                    policy: PolicyConfig {
                        min_count: 20,
                        max_count: 5,
                        ..PolicyConfig::default()
                    },
                    ..cfg()
                },
                "retenção inválida",
            ),
            (
                SnapshotConfig {
                    policy: PolicyConfig {
                        max_age_days: 0,
                        ..PolicyConfig::default()
                    },
                    ..cfg()
                },
                "retenção inválida",
            ),
        ];
        for (c, needle) in cases {
            let e = validate(&c).expect_err("should have been refused");
            let msg = format!("{e:#}");
            assert!(msg.contains(needle), "message {msg:?} lacks {needle:?}");
        }
    }

    #[test]
    fn with_defaults_fills_base_path_from_the_deployment() {
        let c = with_defaults(
            "logs-abcd",
            &SnapshotConfig {
                base_path: "  ".into(),
                region: String::new(),
                endpoint: "http://minio:9000/".into(),
                ..cfg()
            },
        );
        assert_eq!(c.base_path, "logs-abcd");
        assert_eq!(c.region, "us-east-1");
        assert_eq!(c.endpoint, "http://minio:9000");
    }

    /// The wire contract with the SPA. `frontend/views_snapshot.jsx`'s
    /// `emptyConfig()` is this exact object — if a field is renamed on either
    /// side, the value silently falls back to its default (`#[serde(default)]`)
    /// instead of failing, so the drift has to be caught here.
    #[test]
    fn the_frontend_payload_deserializes_field_for_field() {
        let wire = serde_json::json!({
            "enabled": true,
            "bucket": "velox-snapshots",
            "base_path": "",
            "endpoint": "http://minio.velox-minio.svc:9000",
            "region": "us-east-1",
            "path_style_access": true,
            "access_key": "veloxtest",
            "secret_key": "veloxtest123",
            "policy": {
                "enabled": true,
                "cron": "0 2 * * *",
                "timezone": "UTC",
                "indices": "*",
                "include_global_state": true,
                "max_age_days": 7,
                "max_count": 14,
                "min_count": 3
            }
        });
        let c: SnapshotConfig = serde_json::from_value(wire).expect("wire shape");
        assert_eq!(c.bucket, "velox-snapshots");
        assert_eq!(c.endpoint, "http://minio.velox-minio.svc:9000");
        assert_eq!(c.access_key, "veloxtest");
        assert!(c.path_style_access);
        assert_eq!(c.policy.cron, "0 2 * * *");
        assert_eq!(c.policy.max_age_days, 7);
        assert_eq!(c.policy.min_count, 3);
        assert!(c.policy.include_global_state);
        // The wizard sends this shape too, and its defaults are already valid.
        assert!(validate(&c).is_ok());

        // A skipped wizard step sends nothing at all — that must deserialize
        // into the unconfigured default, not fail.
        let empty: SnapshotConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!empty.enabled);
        assert!(validate(&empty).is_ok());
    }

    #[test]
    fn schedule_label_reads_daily_schedules() {
        let daily = PolicyConfig::default();
        assert_eq!(schedule_label(&daily), "02:00");
        let weekly = PolicyConfig {
            cron: "30 4 * * 0".into(),
            ..PolicyConfig::default()
        };
        assert_eq!(schedule_label(&weekly), "30 4 * * 0");
    }
}
