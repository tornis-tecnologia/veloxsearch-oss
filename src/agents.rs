// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Server-only collection-agent deployment.
//!
//! "Enable monitoring" actually ships logs: this deploys a Fluent Bit DaemonSet
//! (per recipe) that tails container logs and writes to the target OpenSearch
//! deployment. Shared RBAC + namespace are created idempotently.

// The kube plumbing (`apply_dynamic` / `delete_dynamic`) lives in `k8s.rs`,
// which owns every other `Api`/`ApiResource` construction in the crate, and is
// shared with the OTel stack provisioner (`otel_stack.rs`, ADR-053) so both
// produce byte-identical objects under the same field manager.
use crate::k8s::{admin_creds, apply_dynamic as apply, client, delete_dynamic, get_dynamic};
use crate::scope::Deployment;
use anyhow::Result;
use kube::Client;

/// Shared namespace every collection agent runs in. `pub(crate)` because the
/// tenant NetworkPolicy set (#81) must name exactly this namespace to let
/// agents reach a tenant's 9200 — a second literal there would be a silently
/// broken ingest path.
pub(crate) const AGENT_NS: &str = "velox-agents";
const AGENT_IMAGE: &str = "fluent/fluent-bit:3.1.9";

/// The k8s-events record filter (#104): drops `metadata.managedFields` and
/// prunes empty maps, which otherwise flatten to dot-only field names that
/// OpenSearch rejects — every event 400s and nothing is indexed. Static engine
/// content (ADR-039: never interpolated, no credentials), shipped as its own
/// ConfigMap key because the fluent-bit image has no shell to write it.
const PRUNE_LUA: &str = include_str!("agents/k8s_events_prune.lua");
/// ConfigMap key and in-container path of `PRUNE_LUA`. A config that names
/// the path gets the script mounted next to it (`agent_manifests`).
const PRUNE_LUA_KEY: &str = "prune.lua";
const PRUNE_LUA_PATH: &str = "/fluent-bit/etc/prune.lua";
/// The `[FILTER]` block that runs `PRUNE_LUA` on every event record. One
/// constant for the template and the #104 repair of existing collectors, so a
/// repaired config is byte-identical to a freshly rendered one.
const EVENTS_PRUNE_FILTER: &str = "[FILTER]
    Name lua
    Match k8s_events
    script /fluent-bit/etc/prune.lua
    call prune
";

/// Namespace + ServiceAccount + RBAC the Fluent Bit kubernetes filter needs.
async fn ensure_rbac(client: &Client) -> Result<()> {
    apply(
        client,
        "",
        "v1",
        "Namespace",
        None,
        AGENT_NS,
        &serde_json::json!({ "apiVersion":"v1","kind":"Namespace","metadata":{"name":AGENT_NS} }),
    )
    .await?;
    apply(client, "", "v1", "ServiceAccount", Some(AGENT_NS), "velox-agent",
        &serde_json::json!({ "apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":"velox-agent","namespace":AGENT_NS} })).await?;
    apply(client, "rbac.authorization.k8s.io", "v1", "ClusterRole", None, "velox-agent",
        &serde_json::json!({
            "apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRole",
            "metadata":{"name":"velox-agent"},
            "rules":[
                {"apiGroups":[""],"resources":["pods","namespaces"],"verbs":["get","list","watch"]},
                // kubernetes_events input (k8s-events recipe)
                {"apiGroups":["","events.k8s.io"],"resources":["events"],"verbs":["get","list","watch"]}
            ]
        })).await?;
    apply(client, "rbac.authorization.k8s.io", "v1", "ClusterRoleBinding", None, "velox-agent",
        &serde_json::json!({
            "apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRoleBinding",
            "metadata":{"name":"velox-agent"},
            "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":"velox-agent"},
            "subjects":[{"kind":"ServiceAccount","name":"velox-agent","namespace":AGENT_NS}]
        })).await?;
    Ok(())
}

/// Fluent Bit config TEMPLATE for a built-in recipe: the collector config with
/// `{os_host}` / `{os_user}` / `{os_password}` / `{index}` holes from the
/// closed interpolation set (ADR-039, docs/integrations/interpolation.md).
/// This is exactly the `agent.conf.tmpl` shipped in the recipe's registry
/// package (`integrations/<id>/` in the veloxsearch-registry repo, #105) —
/// the `registry_golden` tests assert byte-equivalence, so any edit here must
/// regenerate the packages (`VELOX_REGISTRY_PATH=… VELOX_UPDATE_REGISTRY=1
/// cargo test`) and land them in the registry repo.
pub(crate) fn fluent_bit_conf_template(recipe: &str) -> String {
    // k8s-events is not a log tailer: it watches the API server's Event
    // stream (in-cluster defaults: kube url/token/CA from the SA mount).
    // NOTE: this FB version's opensearch output has no Include_Time_Key (es
    // output only) — @timestamp comes from the k8s-events ingest pipeline.
    // Every event passes EVENTS_PRUNE_FILTER first (#104): raw Event objects
    // carry empty maps the OpenSearch mapper rejects.
    if recipe == "k8s-events" {
        return format!(
            r#"[SERVICE]
    Flush 5
    Log_Level info
[INPUT]
    Name kubernetes_events
    Tag k8s_events
{EVENTS_PRUNE_FILTER}[OUTPUT]
    Name opensearch
    Match *
    Host {{os_host}}
    Port 9200
    HTTP_User {{os_user}}
    HTTP_Passwd {{os_password}}
    tls On
    tls.verify Off
    Suppress_Type_Name On
    Index {{index}}
    Trace_Error On
"#
        );
    }
    // Security sources are NODE files, not container logs: the kubernetes
    // metadata filter and CRI multiline parser don't apply. ssh/auth lines and
    // k8s-audit JSON are single-line; the recipe's ingest pipeline parses them.
    // Both live under /var/log, already host-mounted by the tailer DaemonSet.
    if let Some(path) = match recipe {
        "ssh" => Some("/var/log/auth.log,/var/log/secure"),
        "k8s-audit" => Some("/var/log/kubernetes/audit/audit.log"),
        _ => None,
    } {
        return format!(
            r#"[SERVICE]
    Flush 5
    Log_Level info
[INPUT]
    Name tail
    Tag host.{recipe}
    Path {path}
    Read_from_Head On
    Mem_Buf_Limit 5MB
    Skip_Long_Lines On
[FILTER]
    Name modify
    Match host.*
    Rename log message
[OUTPUT]
    Name opensearch
    Match *
    Host {{os_host}}
    Port 9200
    HTTP_User {{os_user}}
    HTTP_Passwd {{os_password}}
    tls On
    tls.verify Off
    Suppress_Type_Name On
    Index {{index}}
    Trace_Error On
"#
        );
    }
    // (path glob, rename log->message, read_from_head)
    // kubernetes: only new logs (Read_from_Head Off) — reading all history of
    // every container fills disk fast. nginx/postgres + round-2 store/broker
    // recipes: read existing logs too. mysql also matches mariadb containers.
    let (path, rename, read_head) = match recipe {
        "kubernetes" => ("/var/log/containers/*.log", false, "Off"),
        "postgres" => ("/var/log/containers/*postgres*.log", true, "On"),
        "redis" => ("/var/log/containers/*redis*.log", true, "On"),
        "mysql" => (
            "/var/log/containers/*mysql*.log,/var/log/containers/*mariadb*.log",
            true,
            "On",
        ),
        "traefik" => ("/var/log/containers/*traefik*.log", true, "On"),
        "mongo" => ("/var/log/containers/*mongo*.log", true, "On"),
        "rabbitmq" => ("/var/log/containers/*rabbitmq*.log", true, "On"),
        "kafka" => ("/var/log/containers/*kafka*.log", true, "On"),
        _ => ("/var/log/containers/*nginx*.log", true, "On"),
    };
    let modify = if rename {
        "[FILTER]\n    Name modify\n    Match kube.*\n    Rename log message\n"
    } else {
        ""
    };
    format!(
        r#"[SERVICE]
    Flush 5
    Log_Level info
[INPUT]
    Name tail
    Tag kube.*
    Path {path}
    Exclude_Path /var/log/containers/*velox-agent*.log
    multiline.parser cri
    Read_from_Head {read_head}
    Mem_Buf_Limit 5MB
    Skip_Long_Lines On
[FILTER]
    Name kubernetes
    Match kube.*
    Kube_Tag_Prefix kube.var.log.containers.
    Merge_Log Off
    Keep_Log On
{modify}[OUTPUT]
    Name opensearch
    Match *
    Host {{os_host}}
    Port 9200
    HTTP_User {{os_user}}
    HTTP_Passwd {{os_password}}
    tls On
    tls.verify Off
    Suppress_Type_Name On
    Index {{index}}
    Trace_Error On
"#
    )
}

/// Build the fluent-bit.conf for a recipe targeting `deployment`'s OpenSearch.
/// `user`/`password` are the deployment's admin credentials (resolved from its
/// Secret via `admin_creds`) — never a hardcoded default. Renders the recipe's
/// template through the engine's closed-set interpolation
/// (`integrations::render`), so the built-in path and the package path
/// (`agent.conf.tmpl`) produce byte-identical configs by construction.
pub(crate) fn fluent_bit_conf(
    deployment: &Deployment,
    recipe: &str,
    user: &str,
    password: &str,
) -> String {
    let vars = crate::integrations::Vars::new(
        deployment,
        crate::recipes::recipe_index(recipe),
        recipe,
        user,
        password,
    );
    crate::integrations::render(&fluent_bit_conf_template(recipe), &vars)
        .expect("built-in agent template uses only closed interpolation tokens")
}

/// Agent name is per deployment×recipe: each managed OpenSearch deployment
/// gets its own collector, so enabling a recipe on deployment B never
/// repoints (steals) deployment A's agent — they collect independently.
fn agent_name(deployment: &Deployment, recipe: &str) -> String {
    format!("velox-agent-{deployment}-{recipe}")
}

/// Deploy (or update) the Fluent Bit agent for a built-in recipe. Renders the
/// recipe's config from `fluent_bit_conf` and applies the workload. k8s-events
/// is the sole non-tailer (API-server watcher → Deployment, not DaemonSet).
pub async fn deploy_agent(deployment: &Deployment, recipe: &str) -> Result<()> {
    // Authenticate the collector with the deployment's per-cluster admin creds
    // from its Secret (via admin_creds) — never a hardcoded default.
    let (user, password) = admin_creds(deployment).await;
    let conf = fluent_bit_conf(deployment, recipe, &user, &password);
    apply_agent_workload(deployment, recipe, &conf, recipe != "k8s-events").await
}

/// Deploy (or update) an agent from a **pre-rendered** Fluent Bit config — the
/// integration-engine path (ADR-039). Same k8s apply as `deploy_agent`, but the
/// config is supplied by the package (already interpolated with the closed
/// variable set) instead of built from the in-binary `fluent_bit_conf`. `agent`
/// is the package id (names the workload per deployment×id); `tails_logs`
/// selects DaemonSet (host `/var/log` tailer) vs single-replica Deployment
/// (API-server watcher, no host mount).
pub async fn deploy_rendered_agent(
    deployment: &Deployment,
    agent: &str,
    conf: &str,
    tails_logs: bool,
) -> Result<()> {
    apply_agent_workload(deployment, agent, conf, tails_logs).await
}

/// Apply the collector workload (RBAC + ConfigMap + DaemonSet/Deployment) for a
/// deployment×id from an already-rendered Fluent Bit config. The single k8s
/// code path shared by the built-in recipes (`deploy_agent`) and the package
/// engine (`deploy_rendered_agent`), so both produce byte-identical objects.
async fn apply_agent_workload(
    deployment: &Deployment,
    recipe: &str,
    conf: &str,
    tails_logs: bool,
) -> Result<()> {
    let client = client().await?;
    ensure_rbac(&client).await?;

    let agent = agent_name(deployment, recipe);
    let (config_map, workload) = agent_manifests(deployment, &agent, conf, tails_logs);
    apply(
        &client,
        "",
        "v1",
        "ConfigMap",
        Some(AGENT_NS),
        &agent,
        &config_map,
    )
    .await?;
    let kind = if tails_logs {
        "DaemonSet"
    } else {
        "Deployment"
    };
    apply(
        &client,
        "apps",
        "v1",
        kind,
        Some(AGENT_NS),
        &agent,
        &workload,
    )
    .await?;
    Ok(())
}

/// The collector's ConfigMap and workload (DaemonSet when `tails_logs`, else a
/// single-replica Deployment), built off-cluster so the shape is unit-tested.
/// A config that runs the prune script (`PRUNE_LUA_PATH`, #104) gets the script
/// as a second ConfigMap key and a second subPath mount — subPath because the
/// directory already holds the image's own files.
fn agent_manifests(
    deployment: &Deployment,
    agent: &str,
    conf: &str,
    tails_logs: bool,
) -> (serde_json::Value, serde_json::Value) {
    let prune = conf.contains(PRUNE_LUA_PATH);
    let mut data = serde_json::json!({ "fluent-bit.conf": conf });
    let mut mounts = vec![serde_json::json!(
        {"name":"config","mountPath":"/fluent-bit/etc/fluent-bit.conf","subPath":"fluent-bit.conf"})];
    let mut volumes = vec![serde_json::json!({"name":"config","configMap":{"name":agent}})];
    if prune {
        data[PRUNE_LUA_KEY] = PRUNE_LUA.into();
        mounts.push(serde_json::json!(
            {"name":"config","mountPath":PRUNE_LUA_PATH,"subPath":PRUNE_LUA_KEY}));
    }
    // Log tailers need every node's /var/log → DaemonSet. The events watcher
    // talks to the API server only — one replica, or every node would index
    // each event once per node (`tails_logs` is decided by the caller).
    if tails_logs {
        mounts.push(serde_json::json!({"name":"varlog","mountPath":"/var/log"}));
        volumes.push(serde_json::json!({"name":"varlog","hostPath":{"path":"/var/log"}}));
    }
    // Hash the mounted files into the pod template: subPath mounts never update
    // live, so a changed ConfigMap must change the template to force a rollout.
    let conf_hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        conf.hash(&mut h);
        if prune {
            PRUNE_LUA.hash(&mut h);
        }
        format!("{:x}", h.finish())
    };
    let config_map = serde_json::json!({
        "apiVersion":"v1","kind":"ConfigMap",
        "metadata":{"name":agent,"namespace":AGENT_NS},
        "data": data
    });
    let template = serde_json::json!({
        "metadata":{"labels":{"app":agent},
            "annotations":{"veloxsearch.ai/target": deployment.name(),
                           "veloxsearch.ai/config-hash": conf_hash}},
        "spec":{
            "serviceAccountName":"velox-agent",
            "tolerations":[{"operator":"Exists"}],
            "containers":[{
                "name":"fluent-bit","image":AGENT_IMAGE,
                "volumeMounts": mounts,
                "resources":{"requests":{"memory":"64Mi","cpu":"50m"},"limits":{"memory":"128Mi","cpu":"200m"}}
            }],
            "volumes": volumes
        }
    });
    let workload = if tails_logs {
        serde_json::json!({
            "apiVersion":"apps/v1","kind":"DaemonSet",
            "metadata":{"name":agent,"namespace":AGENT_NS,"labels":{"app":agent}},
            "spec":{ "selector":{"matchLabels":{"app":agent}}, "template": template }
        })
    } else {
        serde_json::json!({
            "apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":agent,"namespace":AGENT_NS,"labels":{"app":agent}},
            "spec":{ "replicas":1, "selector":{"matchLabels":{"app":agent}}, "template": template }
        })
    };
    (config_map, workload)
}

/// #104 upgrade repair: every k8s-events collector created before the prune
/// filter shipped ingests nothing (each event 400s on the mapper). Runs once per
/// start as the installation admin, in the style of the ingress backfill
/// (`access::backfill_on_startup`): for each deployment, a collector that is
/// velox-managed and missing any part of the fix is re-applied through the same
/// path a fresh install takes. Idempotent — an already-repaired collector, an
/// absent one, or one velox does not manage is left untouched. Best-effort:
/// off-cluster or during an API outage it logs and gives up, never blocking the
/// server.
pub async fn repair_events_collectors_on_startup() {
    let client = match client().await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("k8s-events collector repair skipped: {e:#}");
            return;
        }
    };
    let deployments = match crate::k8s::scoped_deployments(&crate::scope::Scope::Admin).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("k8s-events collector repair: listing deployments failed: {e:#}");
            return;
        }
    };
    for dep in deployments {
        match repair_events_collector(&client, &dep).await {
            Ok(true) => tracing::info!("repaired k8s-events collector for {dep} (#104)"),
            Ok(false) => {}
            Err(e) => tracing::warn!("k8s-events collector repair for {dep}: {e:#}"),
        }
    }
}

/// Repair one deployment's k8s-events collector if it needs it; `Ok(true)`
/// when something was re-applied.
async fn repair_events_collector(client: &Client, dep: &Deployment) -> Result<bool> {
    let agent = agent_name(dep, "k8s-events");
    let get = |kind: &'static str, group: &'static str| {
        let agent = agent.clone();
        async move { get_dynamic(client, group, "v1", kind, Some(AGENT_NS), &agent).await }
    };
    let Some(workload) = get("Deployment", "apps").await? else {
        return Ok(false);
    };
    let Some(config_map) = get("ConfigMap", "").await? else {
        return Ok(false);
    };
    let Some(conf) = events_collector_repair(dep, &workload, &config_map) else {
        return Ok(false);
    };
    apply_agent_workload(dep, "k8s-events", &conf, false).await?;
    Ok(true)
}

/// The decision behind the #104 repair, pure so it is unit-tested: the config
/// to re-apply, or `None` to leave the collector alone. `None` when the
/// workload is not velox's collector for `dep` (name label + target annotation
/// both written by `agent_manifests`), when the config is not an events watcher
/// with an `[OUTPUT]` to insert before, and when the filter, the current
/// script and its mount are all already in place. The existing config is kept
/// (with its credentials) and only gains `EVENTS_PRUNE_FILTER`, so a repaired
/// config is byte-identical to a fresh render.
fn events_collector_repair(
    dep: &Deployment,
    workload: &serde_json::Value,
    config_map: &serde_json::Value,
) -> Option<String> {
    let agent = agent_name(dep, "k8s-events");
    fn str_at<'a>(v: &'a serde_json::Value, ptr: &str) -> Option<&'a str> {
        v.pointer(ptr).and_then(|x| x.as_str())
    }
    if str_at(workload, "/metadata/labels/app") != Some(agent.as_str())
        || str_at(
            workload,
            "/spec/template/metadata/annotations/veloxsearch.ai~1target",
        ) != Some(dep.name())
    {
        return None;
    }
    let conf = str_at(config_map, "/data/fluent-bit.conf")?;
    if !conf.contains("Name kubernetes_events") {
        return None;
    }
    let has_filter = conf.contains(EVENTS_PRUNE_FILTER);
    let script_current = str_at(config_map, "/data/prune.lua") == Some(PRUNE_LUA);
    let mounted = workload
        .pointer("/spec/template/spec/containers")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("volumeMounts")?.as_array())
        .flatten()
        .any(|m| m.get("mountPath").and_then(|p| p.as_str()) == Some(PRUNE_LUA_PATH));
    if has_filter && script_current && mounted {
        return None;
    }
    if has_filter {
        return Some(conf.to_string());
    }
    conf.contains("\n[OUTPUT]\n").then(|| {
        conf.replacen(
            "\n[OUTPUT]\n",
            &format!("\n{EVENTS_PRUNE_FILTER}[OUTPUT]\n"),
            1,
        )
    })
}

/// Remove a recipe's agent for one deployment (plus any pre-rename legacy
/// `velox-agent-<recipe>` leftovers, best-effort).
pub async fn remove_agent(deployment: &Deployment, recipe: &str) -> Result<()> {
    let client = client().await?;
    for agent in [
        agent_name(deployment, recipe),
        format!("velox-agent-{recipe}"),
    ] {
        for (group, kind) in [
            ("apps", "DaemonSet"),
            ("apps", "Deployment"),
            ("", "ConfigMap"),
        ] {
            delete_dynamic(&client, group, "v1", kind, Some(AGENT_NS), &agent).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep() -> Deployment {
        Deployment::for_test("logs-ab12", crate::k8s::ns(), None)
    }

    fn events_conf() -> String {
        fluent_bit_conf(&dep(), "k8s-events", "admin", "s3cr3t-pw")
    }

    /// What a collector shipped before #104 carried: the same render without
    /// the prune filter, and the workload built from it (no script, no mount).
    fn pre_fix_collector() -> (String, serde_json::Value, serde_json::Value) {
        let conf = events_conf().replace(EVENTS_PRUNE_FILTER, "");
        let agent = agent_name(&dep(), "k8s-events");
        let (cm, workload) = agent_manifests(&dep(), &agent, &conf, false);
        (conf, cm, workload)
    }

    fn mount_paths(workload: &serde_json::Value) -> Vec<String> {
        workload["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["mountPath"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn events_config_runs_the_prune_filter_before_the_output() {
        let conf = events_conf();
        let filter = conf.find(EVENTS_PRUNE_FILTER).expect("prune filter");
        assert!(filter < conf.find("[OUTPUT]").unwrap());
        assert!(conf.contains(PRUNE_LUA_PATH));
        // The filter matches the input's tag, or it would filter nothing.
        assert!(
            conf.contains("Tag k8s_events") && EVENTS_PRUNE_FILTER.contains("Match k8s_events")
        );
    }

    #[test]
    fn only_the_events_recipe_ships_the_script() {
        for recipe in crate::recipes::RECIPES {
            let conf = fluent_bit_conf(&dep(), recipe, "admin", "pw");
            assert_eq!(
                conf.contains(PRUNE_LUA_PATH),
                *recipe == "k8s-events",
                "{recipe}"
            );
        }
    }

    #[test]
    fn events_collector_mounts_the_script_from_its_config_map() {
        let agent = agent_name(&dep(), "k8s-events");
        let (cm, workload) = agent_manifests(&dep(), &agent, &events_conf(), false);
        assert_eq!(cm["data"][PRUNE_LUA_KEY], PRUNE_LUA);
        assert_eq!(workload["kind"], "Deployment");
        let mounts = &workload["spec"]["template"]["spec"]["containers"][0]["volumeMounts"];
        assert!(mounts.as_array().unwrap().contains(&serde_json::json!(
            {"name":"config","mountPath":PRUNE_LUA_PATH,"subPath":PRUNE_LUA_KEY}
        )));
    }

    #[test]
    fn tailers_do_not_get_the_script() {
        let agent = agent_name(&dep(), "nginx");
        let conf = fluent_bit_conf(&dep(), "nginx", "admin", "pw");
        let (cm, workload) = agent_manifests(&dep(), &agent, &conf, true);
        assert!(cm["data"].get(PRUNE_LUA_KEY).is_none());
        assert_eq!(workload["kind"], "DaemonSet");
        assert!(!mount_paths(&workload).contains(&PRUNE_LUA_PATH.to_string()));
    }

    #[test]
    fn the_script_is_static_content() {
        // ADR-039: shipped verbatim, never rendered — so it must not name an
        // interpolation token, and no credential can reach it.
        for token in crate::integrations::CLOSED_TOKENS {
            assert!(!PRUNE_LUA.contains(&format!("{{{token}}}")), "{token}");
        }
        assert!(PRUNE_LUA.contains("function prune(tag, timestamp, record)"));
        assert!(PRUNE_LUA.contains("managedFields"));
    }

    #[test]
    fn repair_turns_a_pre_fix_collector_into_a_fresh_render() {
        let (_, cm, workload) = pre_fix_collector();
        assert_eq!(
            events_collector_repair(&dep(), &workload, &cm),
            Some(events_conf())
        );
    }

    #[test]
    fn repair_is_a_no_op_on_a_fixed_collector() {
        let agent = agent_name(&dep(), "k8s-events");
        let (cm, workload) = agent_manifests(&dep(), &agent, &events_conf(), false);
        assert_eq!(events_collector_repair(&dep(), &workload, &cm), None);
        // …including one this repair itself produced.
        let (_, old_cm, old_workload) = pre_fix_collector();
        let repaired = events_collector_repair(&dep(), &old_workload, &old_cm).unwrap();
        let (cm, workload) = agent_manifests(&dep(), &agent, &repaired, false);
        assert_eq!(events_collector_repair(&dep(), &workload, &cm), None);
    }

    #[test]
    fn repair_finishes_a_half_patched_collector() {
        // Filter in the config but no mount (the first step of the manual
        // playbook): Fluent Bit fails Lua init, so it still needs the apply.
        let (_, mut cm, workload) = pre_fix_collector();
        cm["data"]["fluent-bit.conf"] = events_conf().into();
        assert_eq!(
            events_collector_repair(&dep(), &workload, &cm),
            Some(events_conf())
        );
        // A stale script is re-shipped too.
        let agent = agent_name(&dep(), "k8s-events");
        let (mut cm, workload) = agent_manifests(&dep(), &agent, &events_conf(), false);
        cm["data"][PRUNE_LUA_KEY] = "-- old".into();
        assert!(events_collector_repair(&dep(), &workload, &cm).is_some());
    }

    #[test]
    fn repair_never_touches_a_collector_velox_does_not_manage() {
        let (_, cm, workload) = pre_fix_collector();
        let mut foreign_target = workload.clone();
        foreign_target["spec"]["template"]["metadata"]["annotations"]["veloxsearch.ai/target"] =
            "someone-else".into();
        assert_eq!(events_collector_repair(&dep(), &foreign_target, &cm), None);
        let mut unlabelled = workload.clone();
        unlabelled["metadata"]["labels"] = serde_json::json!({});
        assert_eq!(events_collector_repair(&dep(), &unlabelled, &cm), None);
        // Not an events watcher, or no [OUTPUT] to anchor on: leave it.
        let mut tailer = cm.clone();
        tailer["data"]["fluent-bit.conf"] = fluent_bit_conf(&dep(), "nginx", "a", "b").into();
        assert_eq!(events_collector_repair(&dep(), &workload, &tailer), None);
        let mut no_output = cm.clone();
        no_output["data"]["fluent-bit.conf"] =
            "[INPUT]\n    Name kubernetes_events\n    Tag k8s_events\n".into();
        assert_eq!(events_collector_repair(&dep(), &workload, &no_output), None);
    }
}
