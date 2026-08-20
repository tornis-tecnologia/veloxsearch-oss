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
use crate::k8s::{admin_creds, apply_dynamic as apply, client, delete_dynamic};
use crate::scope::Deployment;
use anyhow::Result;
use kube::Client;

/// Shared namespace every collection agent runs in. `pub(crate)` because the
/// tenant NetworkPolicy set (#81) must name exactly this namespace to let
/// agents reach a tenant's 9200 — a second literal there would be a silently
/// broken ingest path.
pub(crate) const AGENT_NS: &str = "velox-agents";
const AGENT_IMAGE: &str = "fluent/fluent-bit:3.1.9";

/// Namespace + ServiceAccount + RBAC the Fluent Bit kubernetes filter needs.
async fn ensure_rbac(client: &Client) -> Result<()> {
    apply(client, "", "v1", "Namespace", None, AGENT_NS,
        &serde_json::json!({ "apiVersion":"v1","kind":"Namespace","metadata":{"name":AGENT_NS} })).await?;
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
    if recipe == "k8s-events" {
        return r#"[SERVICE]
    Flush 5
    Log_Level info
[INPUT]
    Name kubernetes_events
    Tag k8s_events
[OUTPUT]
    Name opensearch
    Match *
    Host {os_host}
    Port 9200
    HTTP_User {os_user}
    HTTP_Passwd {os_password}
    tls On
    tls.verify Off
    Suppress_Type_Name On
    Index {index}
    Trace_Error On
"#
        .to_string();
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
pub(crate) fn fluent_bit_conf(deployment: &Deployment, recipe: &str, user: &str, password: &str) -> String {
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
    apply(&client, "", "v1", "ConfigMap", Some(AGENT_NS), &agent,
        &serde_json::json!({
            "apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":agent,"namespace":AGENT_NS},
            "data":{"fluent-bit.conf": conf}
        })).await?;

    // Log tailers need every node's /var/log → DaemonSet. The events watcher
    // talks to the API server only — one replica, or every node would index
    // each event once per node (`tails_logs` is decided by the caller).
    let mut mounts = vec![serde_json::json!(
        {"name":"config","mountPath":"/fluent-bit/etc/fluent-bit.conf","subPath":"fluent-bit.conf"})];
    let mut volumes = vec![serde_json::json!({"name":"config","configMap":{"name":agent}})];
    if tails_logs {
        mounts.push(serde_json::json!({"name":"varlog","mountPath":"/var/log"}));
        volumes.push(serde_json::json!({"name":"varlog","hostPath":{"path":"/var/log"}}));
    }
    // Hash the config into the pod template: subPath mounts never update live,
    // so a changed ConfigMap must change the template to force a rollout.
    let conf_hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        conf.hash(&mut h);
        format!("{:x}", h.finish())
    };
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
    if tails_logs {
        apply(&client, "apps", "v1", "DaemonSet", Some(AGENT_NS), &agent,
            &serde_json::json!({
                "apiVersion":"apps/v1","kind":"DaemonSet",
                "metadata":{"name":agent,"namespace":AGENT_NS,"labels":{"app":agent}},
                "spec":{ "selector":{"matchLabels":{"app":agent}}, "template": template }
            })).await?;
    } else {
        apply(&client, "apps", "v1", "Deployment", Some(AGENT_NS), &agent,
            &serde_json::json!({
                "apiVersion":"apps/v1","kind":"Deployment",
                "metadata":{"name":agent,"namespace":AGENT_NS,"labels":{"app":agent}},
                "spec":{ "replicas":1, "selector":{"matchLabels":{"app":agent}}, "template": template }
            })).await?;
    }
    Ok(())
}

/// Remove a recipe's agent for one deployment (plus any pre-rename legacy
/// `velox-agent-<recipe>` leftovers, best-effort).
pub async fn remove_agent(deployment: &Deployment, recipe: &str) -> Result<()> {
    let client = client().await?;
    for agent in [agent_name(deployment, recipe), format!("velox-agent-{recipe}")] {
        for (group, kind) in [("apps", "DaemonSet"), ("apps", "Deployment"), ("", "ConfigMap")] {
            delete_dynamic(&client, group, "v1", kind, Some(AGENT_NS), &agent).await;
        }
    }
    Ok(())
}
