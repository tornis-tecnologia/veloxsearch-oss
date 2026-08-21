// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Purpose-profile application (ADR-017 / ADR-028): what actually makes an
//! `observability` deployment different from `security` or `search`.
//!
//! Profiles are applied AFTER a deployment reports green (the deferred task in
//! `app.rs`) and re-applied on every save — everything here must be an
//! idempotent upsert. Per-profile effects:
//!   - `observability` → ISM retention: rotate → snapshot → delete at 30 days
//!   - `security`      → ISM retention (rotate → snapshot → delete, 90d) + detectors
//!   - `search`        → NO agents (enforced at create/save), search-tuned
//!                       index template, no retention — data kept forever
//!
//! Ordering matters: the profile runs BEFORE the monitoring recipes in the
//! deferred task, so the ISM `ism_template` exists before the log indices are
//! created and auto-attaches to them. For pre-existing indices (purpose
//! changed on a live deployment) we additionally best-effort attach/detach.

use crate::recipes::{http, os_base, recipe_index, K8S_INDEX, NGINX_INDEX, RECIPES, SSH_INDEX};
use crate::scope::Deployment;
use anyhow::{bail, Context, Result};

/// Single retention policy per deployment; the purpose decides its age limit.
const POLICY_ID: &str = "velox-retention";
/// Snapshot repository the retention policy backs indices up to before they are
/// deleted. Registered best-effort by `ensure_retention`; it is an `fs` repo, so
/// the cluster must expose a shared `path.repo` for the snapshot action to
/// actually run (live-cluster prerequisite — see ADR-005/028 notes).
const SNAPSHOT_REPO: &str = "velox-snapshots";
/// Roll the active write index over once it reaches either bound (rotation).
const ROLLOVER_AGE: &str = "1d";
const ROLLOVER_SIZE: &str = "25gb";
/// Index template applied by the `search` profile.
const SEARCH_TEMPLATE: &str = "velox-search-defaults";
/// Indices the user's own application writes to under the `search` profile.
const SEARCH_PATTERN: &str = "app-*";

/// Retention covers every index the recipe catalog can write — derived, so a
/// new recipe can't silently escape the purpose profile's retention window.
///
/// `pub(crate)` so `otel_stack` can assert its own patterns are disjoint from
/// these: the two collection modes coexist only because they never write the
/// same indices (ADR-053 decision 5), and that is proven, not assumed.
pub(crate) fn log_patterns() -> Vec<String> {
    RECIPES
        .iter()
        .map(|r| format!("{}*", recipe_index(r)))
        .collect()
}

/// Apply the purpose profile to a green deployment. Idempotent.
pub async fn apply(deployment: &Deployment, purpose: &str) -> Result<()> {
    match purpose {
        "security" => {
            ensure_retention(deployment, "90d").await?;
            ensure_detectors(deployment).await
        }
        "search" => {
            remove_retention(deployment).await;
            ensure_search_template(deployment).await
        }
        // observability is also the fallback for legacy/unlabeled deployments
        _ => ensure_retention(deployment, "30d").await,
    }
}

/// The retention ISM policy document (`hot → snapshot → delete`). Pure so the
/// state machine can be asserted without a live cluster.
fn retention_policy(age: &str) -> serde_json::Value {
    serde_json::json!({
        "policy": {
            "description": format!("VeloxSearch purpose-profile retention ({age}): rotate → snapshot → delete"),
            "default_state": "hot",
            "states": [
                { "name": "hot", "actions": [
                    { "rollover": { "min_index_age": ROLLOVER_AGE, "min_primary_shard_size": ROLLOVER_SIZE } }
                ], "transitions": [
                    { "state_name": "snapshot", "conditions": { "min_index_age": ROLLOVER_AGE } }
                ]},
                { "name": "snapshot", "actions": [
                    { "snapshot": { "repository": SNAPSHOT_REPO, "snapshot": "{{ctx.index}}" } }
                ], "transitions": [
                    { "state_name": "delete", "conditions": { "min_index_age": age } }
                ]},
                { "name": "delete", "actions": [{ "delete": {} }], "transitions": [] }
            ],
            "ism_template": [{ "index_patterns": log_patterns(), "priority": 100 }]
        }
    })
}

/// Upsert the retention ISM policy. The lifecycle is
/// `hot → snapshot → delete`:
///   - **hot** (rotation): roll the active write index over once it hits
///     `ROLLOVER_AGE` or `ROLLOVER_SIZE`, then move on after `ROLLOVER_AGE`.
///   - **snapshot** (backup): snapshot the rolled index into `SNAPSHOT_REPO`,
///     then move on once it reaches the retention `age`.
///   - **delete**: drop the index at `age` — the original retention contract.
///
/// The `ism_template` auto-attaches it to log indices created later; for indices
/// that already exist we best-effort attach (add) and update (change_policy).
///
/// Prerequisites that only a live cluster can satisfy: the snapshot action needs
/// `SNAPSHOT_REPO` registered against a shared `path.repo` (registered
/// best-effort below — a rejection here is non-fatal), and the rollover action
/// needs managed indices to carry a `rollover_alias` write alias. Where those
/// are absent the policy is still accepted and valid; only the snapshot/rollover
/// steps no-op until the prerequisites exist.
async fn ensure_retention(deployment: &Deployment, age: &str) -> Result<()> {
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;

    // ADR-049: registration moved OUT of here. This function used to
    // best-effort register `SNAPSHOT_REPO` as an `fs` repo, which needed a
    // shared `path.repo` no multi-tenant cluster can safely provide (ADR-042) —
    // so it almost never worked, and the ISM snapshot state silently no-op'd.
    // The repository is now an S3 target the user configures per deployment,
    // written onto the CR and reconciled by the operator. This policy simply
    // references it: where the user configured one, the snapshot state works;
    // where they did not, the state no-ops exactly as it did before.
    let policy = retention_policy(age);

    let url = format!("{base}/_plugins/_ism/policies/{POLICY_ID}");
    let resp = c
        .put(&url)
        .basic_auth(&u, Some(&p))
        .json(&policy)
        .send()
        .await
        .context("creating ISM retention policy")?;
    if resp.status() == reqwest::StatusCode::CONFLICT {
        // Policy exists: OpenSearch demands optimistic-concurrency params.
        let cur: serde_json::Value = c
            .get(&url)
            .basic_auth(&u, Some(&p))
            .send()
            .await
            .context("fetching existing ISM policy")?
            .json()
            .await
            .context("parsing existing ISM policy")?;
        let (seq, term) = (
            cur["_seq_no"].as_i64().unwrap_or_default(),
            cur["_primary_term"].as_i64().unwrap_or_default(),
        );
        c.put(format!("{url}?if_seq_no={seq}&if_primary_term={term}"))
            .basic_auth(&u, Some(&p))
            .json(&policy)
            .send()
            .await
            .context("updating ISM retention policy")?
            .error_for_status()
            .context("ISM policy update rejected")?;
    } else {
        resp.error_for_status().context("ISM policy rejected")?;
    }

    // Pre-existing indices: ism_template only covers indices created after the
    // policy, so attach/update explicitly. Both calls legitimately fail when
    // no indices match yet (fresh deployment) — best-effort by design.
    for pat in log_patterns() {
        let _ = c
            .post(format!("{base}/_plugins/_ism/add/{pat}"))
            .basic_auth(&u, Some(&p))
            .json(&serde_json::json!({ "policy_id": POLICY_ID }))
            .send()
            .await;
        let _ = c
            .post(format!("{base}/_plugins/_ism/change_policy/{pat}"))
            .basic_auth(&u, Some(&p))
            .json(&serde_json::json!({ "policy_id": POLICY_ID }))
            .send()
            .await;
    }
    Ok(())
}

/// `search` keeps data forever: detach the retention policy from any managed
/// indices and drop it. Both best-effort — absent policy is the desired state.
async fn remove_retention(deployment: &Deployment) {
    let base = os_base(deployment);
    let Ok(c) = http() else { return };
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    for pat in log_patterns() {
        let _ = c
            .post(format!("{base}/_plugins/_ism/remove/{pat}"))
            .basic_auth(&u, Some(&p))
            .send()
            .await;
    }
    let _ = c
        .delete(format!("{base}/_plugins/_ism/policies/{POLICY_ID}"))
        .basic_auth(&u, Some(&p))
        .send()
        .await;
}

/// SA field-alias mappings (ADR-028 caveat 1, the lever this issue wires up).
/// The KEY is the schema field the log type's pre-packaged Sigma rules query;
/// `path` is the field the #7 ingest pipelines actually produce. Without these
/// the rules compile but match nothing (the rules ask for ECS-style fields, the
/// data has our parsed names), so detection efficacy ≈ 0. SA uses dotted
/// ECS-style alias names (the live `mappings/view` returned `dns.question.name`
/// &c., research §3c). `timestamp→@timestamp` is the ONE correspondence verified
/// live on 3.0.0; the rest target the documented ECS schema and want a live
/// `mappings/view` to confirm acceptance per log type. Safe regardless: the
/// mappings call is `partial` + best-effort, so an alias a log type doesn't know
/// is silently tolerated — a wrong guess is a no-op, never a profile failure.
///
/// nginx COMBINEDAPACHELOG fields (`recipes::configure_nginx`) → `others_web`.
const NGINX_ALIASES: &[(&str, &str)] = &[
    ("timestamp", "@timestamp"),
    ("http.request.method", "verb"),
    ("http.response.status_code", "response"),
    ("source.ip", "clientip"),
    ("url.original", "request"),
    ("user_agent.original", "agent"),
    ("http.response.bytes", "bytes"),
];

/// ssh/auth syslog fields (`recipes::configure_ssh`, #7) → `linux`. The linux
/// log type carries SSH/auth system-activity rules; map the parsed auth fields
/// onto the source/user/process/outcome schema those rules query.
const SSH_ALIASES: &[(&str, &str)] = &[
    ("timestamp", "@timestamp"),
    ("source.ip", "src_ip"),
    ("source.port", "src_port"),
    ("user.name", "auth_user"),
    ("process.name", "program"),
    ("host.hostname", "host"),
    ("event.outcome", "auth_result"),
];

/// Security Analytics detectors over the log indices, loaded with every
/// pre-packaged Sigma rule of the matching log type. API shapes verified live
/// against OpenSearch 3.0.0 (docs/research/2026-06-11-profile-apis-*.md):
/// there is no kubernetes/nginx log type (§3a), so nginx access logs map to
/// `others_web` (49 rules) and host/auth logs to `linux` (188 rules). Each
/// detector ships its field-alias mappings so its rules match the parsed fields
/// the #7 recipes produce (ADR-028 caveat 1) instead of compiling dead.
async fn ensure_detectors(deployment: &Deployment) -> Result<()> {
    // nginx access logs → web threat rules, aliased onto the grok'd web fields.
    ensure_detector(
        deployment,
        "velox-nginx-threats",
        "others_web",
        &format!("{NGINX_INDEX}*"),
        NGINX_ALIASES,
    )
    .await?;
    // ssh/auth logs (#7 security source) → linux system-activity/auth rules,
    // aliased onto the parsed auth fields. This is where the SSH brute-force /
    // failed-login rules get fields to fire on.
    ensure_detector(
        deployment,
        "velox-ssh-threats",
        "linux",
        &format!("{SSH_INDEX}*"),
        SSH_ALIASES,
    )
    .await?;
    // Raw container logs: the line stays unparsed in `log`/`message`, so there
    // are no security fields to alias — auto mode. Efficacy here grows only once
    // container lines are parsed; the detector is correct plumbing meanwhile.
    ensure_detector(
        deployment,
        "velox-k8s-threats",
        "linux",
        &format!("{K8S_INDEX}*"),
        &[],
    )
    .await
    // NOTE (#8): the k8s-audit source (#7, `k8s-audit-logs`) is intentionally NOT
    // given a detector — SA 3.0.0 has no `kubernetes` log type (research §3a), so
    // no pre-packaged Sigma rules target API-server audit events; forcing `linux`
    // would load 188 host-auditd rules that query fields audit events lack. Its
    // fields are already strongly typed by the recipe, so it is ready for a
    // custom-rules detector (or a future SA kubernetes log type) without changes.
}

/// Build the SA field-mapping request body. Always `partial` + best-effort.
/// With aliases it declares them (schema field → `{type: alias, path: <our
/// field>}`); with an empty slice it is auto mode (the server maps what it can,
/// e.g. `timestamp`). Shape verified live on 3.0.0 (research §3c).
fn mappings_body(pattern: &str, log_type: &str, aliases: &[(&str, &str)]) -> serde_json::Value {
    let mut body = serde_json::json!({
        "index_name": pattern,
        "rule_topic": log_type,
        "partial": true,
    });
    if !aliases.is_empty() {
        let mut props = serde_json::Map::new();
        for (alias, path) in aliases {
            props.insert(
                (*alias).to_string(),
                serde_json::json!({ "type": "alias", "path": path }),
            );
        }
        body["alias_mappings"] = serde_json::json!({ "properties": props });
    }
    body
}

/// Upsert one detector (find by name → update by id, else create), first
/// installing its field-alias mappings so the rules match parsed fields.
async fn ensure_detector(
    deployment: &Deployment,
    name: &str,
    log_type: &str,
    pattern: &str,
    aliases: &[(&str, &str)],
) -> Result<()> {
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;

    // Every pre-packaged rule of the log type — there is no "all" shortcut,
    // ids must be enumerated. `rule` is a NESTED mapping: without the nested
    // wrapper the search silently returns 0 hits.
    let rules: serde_json::Value = c
        .post(format!(
            "{base}/_plugins/_security_analytics/rules/_search?pre_packaged=true"
        ))
        .basic_auth(&u, Some(&p))
        .json(&serde_json::json!({
            "size": 500,
            "query": { "nested": { "path": "rule", "query": {
                "bool": { "must": [{ "match": { "rule.category": log_type } }] }
            }}}
        }))
        .send()
        .await
        .context("searching pre-packaged Sigma rules")?
        .error_for_status()
        .context("Sigma rule search rejected")?
        .json()
        .await
        .context("parsing Sigma rule search")?;
    let rule_ids: Vec<serde_json::Value> = rules["hits"]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h["_id"].as_str())
                .map(|id| serde_json::json!({ "id": id }))
                .collect()
        })
        .unwrap_or_default();
    if rule_ids.is_empty() {
        bail!("no pre-packaged Sigma rules found for log type {log_type}");
    }

    // Field-alias mappings (ADR-028 caveat 1): alias the parsed fields the #7
    // pipelines produce onto the schema this log type's Sigma rules query, so
    // the rules actually match. Best-effort + `partial`: an alias the log type
    // doesn't recognise is tolerated (200), so this never blocks the detector.
    let _ = c
        .post(format!("{base}/_plugins/_security_analytics/mappings"))
        .basic_auth(&u, Some(&p))
        .json(&mappings_body(pattern, log_type, aliases))
        .send()
        .await;

    let detector = serde_json::json!({
        "name": name,
        "detector_type": log_type,
        "enabled": true,
        "schedule": { "period": { "interval": 5, "unit": "MINUTES" } },
        "inputs": [{ "detector_input": {
            "description": format!("VeloxSearch security profile: {log_type} threat detection over {pattern}"),
            "indices": [pattern],
            "pre_packaged_rules": rule_ids,
            "custom_rules": []
        }}],
        "triggers": []
    });

    // The detector doc lives under a NESTED `detector` field — same story as
    // rules: a flat term query returns 0 hits. Before the first detector ever
    // exists the backing config index is absent and the search errors — treat
    // any failure as "not found" and let create be the source of truth.
    let existing_id = match c
        .post(format!(
            "{base}/_plugins/_security_analytics/detectors/_search"
        ))
        .basic_auth(&u, Some(&p))
        .json(&serde_json::json!({
            "query": { "nested": { "path": "detector", "query": {
                "term": { "detector.name.keyword": name }
            }}}
        }))
        .send()
        .await
    {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["hits"]["hits"][0]["_id"].as_str().map(str::to_owned)),
        Err(_) => None,
    };

    let resp = match existing_id {
        Some(id) => c
            .put(format!(
                "{base}/_plugins/_security_analytics/detectors/{id}"
            ))
            .basic_auth(&u, Some(&p))
            .json(&detector)
            .send()
            .await
            .context("updating detector")?,
        None => c
            .post(format!("{base}/_plugins/_security_analytics/detectors"))
            .basic_auth(&u, Some(&p))
            .json(&detector)
            .send()
            .await
            .context("creating detector")?,
    };
    resp.error_for_status().context("detector rejected")?;
    Ok(())
}

/// `search` profile: an index template tuning the user's own indices
/// (`app-*`) for serving search — near-real-time refresh and a replica for
/// read throughput. No retention: indices live until the user deletes them.
async fn ensure_search_template(deployment: &Deployment) -> Result<()> {
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let template = serde_json::json!({
        "index_patterns": [SEARCH_PATTERN],
        "priority": 10,
        "template": {
            "settings": {
                "number_of_replicas": 1,
                "refresh_interval": "1s"
            }
        }
    });
    c.put(format!("{base}/_index_template/{SEARCH_TEMPLATE}"))
        .basic_auth(&u, Some(&p))
        .json(&template)
        .send()
        .await
        .context("creating search index template")?
        .error_for_status()
        .context("search index template rejected")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retention policy is a `hot → snapshot → delete` lifecycle: rotation
    /// (rollover) in hot, backup (snapshot) in the middle, delete at `age`.
    #[test]
    fn retention_policy_covers_rotate_snapshot_delete() {
        let p = retention_policy("30d");
        let states = p["policy"]["states"].as_array().unwrap();
        assert_eq!(p["policy"]["default_state"], "hot");
        let names: Vec<&str> = states.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["hot", "snapshot", "delete"]);

        // hot rotates, then transitions into snapshot.
        let hot = &states[0];
        assert!(
            hot["actions"][0].get("rollover").is_some(),
            "hot must rollover"
        );
        assert_eq!(hot["transitions"][0]["state_name"], "snapshot");

        // snapshot backs up to the velox repo, then transitions into delete.
        let snap = &states[1];
        assert_eq!(snap["actions"][0]["snapshot"]["repository"], SNAPSHOT_REPO);
        assert_eq!(snap["transitions"][0]["state_name"], "delete");

        // delete is terminal and fires at the requested retention age.
        let del = &states[2];
        assert!(
            del["actions"][0].get("delete").is_some(),
            "delete must delete"
        );
        assert!(del["transitions"].as_array().unwrap().is_empty());
        assert_eq!(snap["transitions"][0]["conditions"]["min_index_age"], "30d");
    }

    /// The age threshold the caller passes is the delete trigger (retention
    /// contract preserved across the new lifecycle).
    #[test]
    fn retention_age_is_the_delete_trigger() {
        let p = retention_policy("90d");
        let states = p["policy"]["states"].as_array().unwrap();
        assert_eq!(
            states[1]["transitions"][0]["conditions"]["min_index_age"],
            "90d"
        );
    }

    #[test]
    fn auto_mode_sends_no_alias_mappings() {
        // Empty slice (raw container logs) → auto mode: index/topic/partial only.
        let b = mappings_body("k8s-logs*", "linux", &[]);
        assert_eq!(b["index_name"], "k8s-logs*");
        assert_eq!(b["rule_topic"], "linux");
        assert_eq!(b["partial"], true);
        assert!(b.get("alias_mappings").is_none());
    }

    #[test]
    fn every_alias_is_a_path_alias_onto_a_parsed_field() {
        // Matches the live-verified §3c shape: properties.<schema field> =
        // {type: alias, path: <our parsed field>}.
        let b = mappings_body("nginx-logs*", "others_web", NGINX_ALIASES);
        assert_eq!(b["partial"], true);
        let props = &b["alias_mappings"]["properties"];
        for (alias, path) in NGINX_ALIASES {
            assert_eq!(props[*alias]["type"], "alias", "{alias} must be an alias");
            assert_eq!(props[*alias]["path"], *path, "{alias} must point at {path}");
        }
        // Spot-check the web-rule schema fields land on the COMBINEDAPACHELOG names.
        assert_eq!(props["http.request.method"]["path"], "verb");
        assert_eq!(props["http.response.status_code"]["path"], "response");
        assert_eq!(props["source.ip"]["path"], "clientip");
    }

    #[test]
    fn ssh_aliases_cover_the_auth_fields_linux_rules_query() {
        let b = mappings_body("ssh-logs*", "linux", SSH_ALIASES);
        let props = &b["alias_mappings"]["properties"];
        assert_eq!(props["source.ip"]["path"], "src_ip");
        assert_eq!(props["source.port"]["path"], "src_port");
        assert_eq!(props["user.name"]["path"], "auth_user");
        assert_eq!(props["process.name"]["path"], "program");
        assert_eq!(props["event.outcome"]["path"], "auth_result");
    }

    #[test]
    fn alias_tables_keep_the_one_verified_mapping() {
        // timestamp→@timestamp is the only correspondence verified to map on
        // 3.0.0; every parsed source must retain it so detectors keep a time
        // field regardless of what else the log type accepts.
        for table in [NGINX_ALIASES, SSH_ALIASES] {
            assert!(
                table
                    .iter()
                    .any(|(a, p)| *a == "timestamp" && *p == "@timestamp"),
                "alias table must keep timestamp→@timestamp"
            );
        }
    }
}
