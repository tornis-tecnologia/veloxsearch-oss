// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Server-only monitoring recipes (pre-baked, ADR: not AI-generated).
//!
//! A recipe configures the OpenSearch deployment to receive a kind of data and
//! deploys the collection agent that ships it (see `agents`). Recipes:
//!   - `nginx`      → nginx access logs (grok-parsed) from nginx containers
//!   - `kubernetes` → all cluster/pod logs (monitor K3S itself)
//!   - `postgres` / `redis` / `mysql` / `traefik` / `mongo` / `rabbitmq` /
//!     `kafka` → server logs from the matching containers (grok/json-parsed)
//!   - `ssh` / `k8s-audit` → security sources (node auth log + API-server
//!     audit log; parsed, host-tailed — not container logs)

use crate::scope::Deployment;
use anyhow::{Context, Result};

pub const NGINX_INDEX: &str = "nginx-logs";
pub const K8S_INDEX: &str = "k8s-logs";
pub const PG_INDEX: &str = "postgres-logs";
pub const EVENTS_INDEX: &str = "k8s-events";
// round-2 catalog (#7): data-store / broker / proxy logs + security sources.
pub const REDIS_INDEX: &str = "redis-logs";
pub const MYSQL_INDEX: &str = "mysql-logs";
pub const TRAEFIK_INDEX: &str = "traefik-logs";
pub const MONGO_INDEX: &str = "mongo-logs";
pub const RABBITMQ_INDEX: &str = "rabbitmq-logs";
pub const KAFKA_INDEX: &str = "kafka-logs";
pub const SSH_INDEX: &str = "ssh-logs";
pub const K8S_AUDIT_INDEX: &str = "k8s-audit-logs";
/// All recipe ids (used for whole-deployment cleanup and retention patterns).
pub const RECIPES: &[&str] = &[
    "nginx",
    "kubernetes",
    "postgres",
    "k8s-events",
    "redis",
    "mysql",
    "traefik",
    "mongo",
    "rabbitmq",
    "kafka",
    "ssh",
    "k8s-audit",
];

pub(crate) fn os_base(deployment: &Deployment) -> String {
    os_base_in(deployment.name(), deployment.namespace())
}

/// The same URL, addressed by name + namespace. Private to the crate and only
/// reachable from a caller that already resolved the deployment — the same
/// arrangement as `k8s::data_pvcs` / `data_pvcs_in`, for the same caller:
/// `status_from`, which is handed an object the scope layer just listed and so
/// never had a [`Deployment`] token to pass down.
pub(crate) fn os_base_in(name: &str, namespace: &str) -> String {
    format!("https://{name}.{namespace}.svc:9200")
}

/// OpenSearch Dashboards multitenancy: each deployment's saved objects
/// (dashboards, visualizations, index-patterns) live in their own tenant rather
/// than the shared Global tenant, so they're isolated per deployment. Derived
/// from the deployment name (already DNS-safe, ADR-020).
pub fn tenant_name(deployment: &Deployment) -> String {
    format!("velox-{deployment}")
}

/// The tenant to scope saved objects to, or `None` when this deployment uses
/// workspaces instead.
///
/// Two scoping mechanisms cannot both own the same saved objects. With
/// `workspace.enabled` on, the `securitytenant` header segregates nothing —
/// verified live: an object written under `velox-{deployment}` reads back from
/// Global and no tenant index is created. Worse, `ensure_tenant` stops being
/// called in that mode, so a header naming a tenant that no longer exists would
/// be a request for a resource we deliberately did not create.
///
/// Keyed on **workspaces**, not on the observability stack. The stack was only
/// ever the first thing that turned workspaces on; once the next-generation UI
/// is a choice of its own, a deployment can have workspaces without the stack,
/// and that deployment must not go on writing tenant headers.
///
/// Callers must send the `securitytenant` header only for `Some`.
pub(crate) async fn tenant_scope(deployment: &Deployment) -> Option<String> {
    if crate::k8s::workspaces_enabled(deployment).await {
        None
    } else {
        Some(tenant_name(deployment))
    }
}

/// Dashboards base URL (plain HTTP on 5601, internal service).
pub(crate) fn dashboards_base(deployment: &Deployment) -> String {
    format!("http://{deployment}-dashboards.{}.svc:5601", deployment.namespace())
}

/// Make dark mode the **default** for a deployment's Dashboards — written once.
///
/// Two ways to do this exist and they are different products:
///
/// * `uiSettings.overrides.theme:darkMode` in `additionalConfig` LOCKS the
///   toggle — the setting greys out and no user can change it. That is not a
///   default, it is a policy.
/// * a re-asserted settings write is worse: it looks like a default until the
///   next provisioning pass silently undoes whatever the user chose.
///
/// So this goes through the settings API and is scheduled as a provisioning
/// `Item`, which is applied exactly once and recorded on the CR. After that
/// nothing ever looks at the value again — flip it in Advanced Settings and it
/// stays flipped. Same reasoning that keeps `theme:version` unset (see
/// `k8s::next_ui_config`): ship a good starting point, do not take the choice.
///
/// A deployment with no Dashboards is `Ok(())`, not an error — there is nothing
/// to configure, and failing would retry forever against something that will
/// never exist.
pub(crate) async fn set_dark_mode_default(deployment: &Deployment) -> Result<()> {
    if !crate::k8s::has_dashboards(deployment).await {
        return Ok(());
    }
    let dash = dashboards_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let resp = c
        .post(format!("{dash}/api/opensearch-dashboards/settings"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .json(&serde_json::json!({ "changes": { "theme:darkMode": true } }))
        .send()
        .await
        .context("writing theme:darkMode to Dashboards")?;
    if !resp.status().is_success() {
        anyhow::bail!("Dashboards refused theme:darkMode: HTTP {}", resp.status());
    }
    Ok(())
}

pub(crate) fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true) // operator-generated internal certs
        .build()
        .context("building http client")
}

/// The OpenSearch index a recipe writes to.
pub fn recipe_index(recipe: &str) -> &'static str {
    match recipe {
        "kubernetes" => K8S_INDEX,
        "postgres" => PG_INDEX,
        "k8s-events" => EVENTS_INDEX,
        "redis" => REDIS_INDEX,
        "mysql" => MYSQL_INDEX,
        "traefik" => TRAEFIK_INDEX,
        "mongo" => MONGO_INDEX,
        "rabbitmq" => RABBITMQ_INDEX,
        "kafka" => KAFKA_INDEX,
        "ssh" => SSH_INDEX,
        "k8s-audit" => K8S_AUDIT_INDEX,
        _ => NGINX_INDEX,
    }
}

/// Apply a recipe: configure OpenSearch (if needed), create the Dashboards
/// index-pattern so data is browsable, and deploy the agent.
pub async fn apply(deployment: &Deployment, recipe: &str) -> Result<()> {
    if !RECIPES.contains(&recipe) {
        anyhow::bail!("unknown recipe: {recipe}");
    }
    configure_os(deployment, recipe).await?;
    // Saved objects below land in this deployment's own Dashboards tenant —
    // unless the ADR-053 stack owns the Dashboards, in which case they are
    // scoped by workspace and creating a tenant would be a lie about where they
    // live (see `tenant_scope`).
    if tenant_scope(deployment).await.is_some() {
        ensure_tenant(deployment).await;
    }
    let index = recipe_index(recipe);
    // Every recipe's ingest pipeline normalizes its own time field into
    // `@timestamp`, so the pattern's time field is the same for all of them.
    ensure_index_pattern(deployment, index, &format!("{index}*"), "@timestamp").await;
    ensure_dashboard(deployment, recipe).await;
    crate::agents::deploy_agent(deployment, recipe).await?;
    Ok(())
}

/// The Dashboards URL slug for a recipe's out-of-the-box dashboard.
pub fn dashboard_id(recipe: &str) -> &'static str {
    match recipe {
        "kubernetes" => "velox-k8s-overview",
        "postgres" => "velox-pg-overview",
        "k8s-events" => "velox-events-overview",
        "redis" => "velox-redis-overview",
        "mysql" => "velox-mysql-overview",
        "traefik" => "velox-traefik-overview",
        "mongo" => "velox-mongo-overview",
        "rabbitmq" => "velox-rabbitmq-overview",
        "kafka" => "velox-kafka-overview",
        "ssh" => "velox-ssh-overview",
        "k8s-audit" => "velox-k8s-audit-overview",
        _ => "velox-nginx-overview",
    }
}

fn jstr(v: serde_json::Value) -> String {
    v.to_string()
}

/// A visualization saved-object. `query` (kuery) scopes the panel, e.g.
/// "response >= 400" for an error-rate panel.
fn viz(
    id: &str,
    title: &str,
    vtype: &str,
    params: serde_json::Value,
    aggs: serde_json::Value,
    ip: &str,
    query: Option<&str>,
) -> serde_json::Value {
    let vs = jstr(serde_json::json!({"title":title,"type":vtype,"params":params,"aggs":aggs}));
    let src = jstr(serde_json::json!({
        "query":{"query": query.unwrap_or(""),"language":"kuery"},"filter":[],
        "indexRefName":"kibanaSavedObjectMeta.searchSourceJSON.index"
    }));
    serde_json::json!({
        "type":"visualization","id":id,
        "attributes":{"title":title,"visState":vs,"uiStateJSON":"{}","description":"",
            "kibanaSavedObjectMeta":{"searchSourceJSON":src}},
        "references":[{"name":"kibanaSavedObjectMeta.searchSourceJSON.index","type":"index-pattern","id":ip}]
    })
}

fn dashboard_obj(id: &str, title: &str, panels: &[(&str, i64, i64, i64, i64)]) -> serde_json::Value {
    let mut pj = Vec::new();
    let mut refs = Vec::new();
    for (i, (vid, x, y, w, h)) in panels.iter().enumerate() {
        let n = i + 1;
        pj.push(serde_json::json!({
            "version":"3.7.0","gridData":{"x":x,"y":y,"w":w,"h":h,"i":n.to_string()},
            "panelIndex":n.to_string(),"embeddableConfig":{},"panelRefName":format!("panel_{n}")
        }));
        refs.push(serde_json::json!({"name":format!("panel_{n}"),"type":"visualization","id":vid}));
    }
    serde_json::json!({
        "type":"dashboard","id":id,
        "attributes":{"title":title,"hits":0,"description":"",
            "panelsJSON":jstr(serde_json::Value::Array(pj)),
            "optionsJSON":"{\"useMargins\":true,\"hidePanelTitles\":false}",
            "version":1,"timeRestore":true,"timeTo":"now","timeFrom":"now-24h",
            "kibanaSavedObjectMeta":{"searchSourceJSON":"{\"query\":{\"query\":\"\",\"language\":\"kuery\"},\"filter\":[]}"}},
        "references":serde_json::Value::Array(refs)
    })
}

fn line_params() -> serde_json::Value {
    serde_json::json!({
        "type":"line","grid":{"categoryLines":false},
        "categoryAxes":[{"id":"CategoryAxis-1","type":"category","position":"bottom","show":true,
            "scale":{"type":"linear"},"labels":{"show":true,"filter":true,"truncate":100},"title":{}}],
        "valueAxes":[{"id":"ValueAxis-1","name":"LeftAxis-1","type":"value","position":"left","show":true,
            "scale":{"type":"linear","mode":"normal"},"labels":{"show":true,"rotate":0,"filter":false,"truncate":100},
            "title":{"text":"Count"}}],
        "seriesParams":[{"show":true,"type":"line","mode":"normal","data":{"label":"Count","id":"1"},
            "valueAxis":"ValueAxis-1","drawLinesBetweenPoints":true,"showCircles":true}],
        "addTooltip":true,"addLegend":true,"legendPosition":"right","times":[],"addTimeMarker":false
    })
}
fn pie_params() -> serde_json::Value {
    serde_json::json!({"type":"pie","addTooltip":true,"addLegend":true,"legendPosition":"right",
        "isDonut":true,"labels":{"show":false,"values":true,"last_level":true,"truncate":100}})
}
fn table_params() -> serde_json::Value {
    serde_json::json!({"perPage":10,"showPartialRows":false,"showMetricsAtAllLevels":false,
        "showTotal":false,"totalFunc":"sum","percentageCol":""})
}
/// Big-number panel (Requests 24h, Unique clients, …).
fn metric_params() -> serde_json::Value {
    serde_json::json!({"addTooltip":true,"addLegend":false,"type":"metric",
        "metric":{"percentageMode":false,"useRanges":false,"colorSchema":"Green to Red",
            "metricColorMode":"None","colorsRange":[{"type":"range","from":0,"to":10000}],
            "labels":{"show":true},"invertColors":false,
            "style":{"bgFill":"#000","bgColor":false,"labelColor":false,"subText":"","fontSize":48}}})
}
fn count_agg() -> serde_json::Value {
    serde_json::json!({"id":"1","enabled":true,"type":"count","schema":"metric","params":{}})
}
fn sum_agg(field: &str) -> serde_json::Value {
    serde_json::json!({"id":"1","enabled":true,"type":"sum","schema":"metric","params":{"field":field}})
}
fn cardinality_agg(field: &str) -> serde_json::Value {
    serde_json::json!({"id":"1","enabled":true,"type":"cardinality","schema":"metric","params":{"field":field}})
}
fn datehist_agg() -> serde_json::Value {
    serde_json::json!({"id":"2","enabled":true,"type":"date_histogram","schema":"segment",
        "params":{"field":"@timestamp","interval":"auto","min_doc_count":1}})
}
fn terms_agg(id: &str, schema: &str, field: &str, size: i64) -> serde_json::Value {
    serde_json::json!({"id":id,"enabled":true,"type":"terms","schema":schema,
        "params":{"field":field,"size":size,"order":"desc","orderBy":"1"}})
}

/// Out-of-the-box dashboards, modeled on what Elastic ships for the same
/// sources: a metrics row (volume + uniqueness + error trend), a main
/// time-series split by the dominant dimension, distribution donuts, and
/// top-N tables. All panels share the dashboard's time range (last 24h).
pub(crate) fn dashboard_objects(recipe: &str) -> Vec<serde_json::Value> {
    let ip = recipe_index(recipe);
    match recipe {
        "kubernetes" => vec![
            // metrics row
            viz("velox-k8s-total","Log events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-k8s-active-pods","Active pods","metric",metric_params(),
                serde_json::json!([cardinality_agg("kubernetes.pod_name.keyword")]),ip,None),
            viz("velox-k8s-errors","Error mentions over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,Some("message: error or log: error")),
            // main trend + distribution
            viz("velox-k8s-timeline","Logs over time by namespace","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),
                    terms_agg("3","group","kubernetes.namespace_name.keyword",8)]),ip,None),
            viz("velox-k8s-ns","Logs by namespace","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","kubernetes.namespace_name.keyword",10)]),ip,None),
            // streams + top-N
            viz("velox-k8s-stream","stdout vs stderr","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","stream.keyword",4)]),ip,None),
            viz("velox-k8s-pod","Top pods","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","kubernetes.pod_name.keyword",15)]),ip,None),
            viz("velox-k8s-containers","Top containers","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","kubernetes.container_name.keyword",15)]),ip,None),
            viz("velox-k8s-nodes","Logs by node","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","kubernetes.host.keyword",10)]),ip,None),
            dashboard_obj("velox-k8s-overview","Kubernetes / K3S Logs", &[
                ("velox-k8s-total",       0,  0, 10,  8),
                ("velox-k8s-active-pods", 10, 0, 10,  8),
                ("velox-k8s-errors",      20, 0, 28,  8),
                ("velox-k8s-timeline",    0,  8, 32, 15),
                ("velox-k8s-ns",          32, 8, 16, 15),
                ("velox-k8s-stream",      0, 23, 16, 15),
                ("velox-k8s-pod",         16,23, 32, 15),
                ("velox-k8s-containers",  0, 38, 24, 15),
                ("velox-k8s-nodes",       24,38, 24, 15),
            ]),
        ],
        "postgres" => vec![
            viz("velox-pg-total","Log events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-pg-errors","Errors over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,
                Some("level: ERROR or level: FATAL or level: PANIC")),
            viz("velox-pg-timeline","Log volume by level","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","level",8)]),ip,None),
            viz("velox-pg-levels","Levels","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","level",8)]),ip,None),
            viz("velox-pg-messages","Top messages","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","pg_message.keyword",10)]),ip,None),
            viz("velox-pg-dbs","Databases / users","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","pg_db",10),
                    terms_agg("3","bucket","pg_user",10)]),ip,None),
            dashboard_obj("velox-pg-overview","PostgreSQL Overview", &[
                ("velox-pg-total",    0,  0, 10,  8),
                ("velox-pg-errors",   10, 0, 38,  8),
                ("velox-pg-timeline", 0,  8, 32, 15),
                ("velox-pg-levels",   32, 8, 16, 15),
                ("velox-pg-messages", 0, 23, 32, 15),
                ("velox-pg-dbs",      32,23, 16, 15),
            ]),
        ],
        "k8s-events" => vec![
            viz("velox-events-total","Events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-events-warn","Warnings over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,Some("type: Warning")),
            viz("velox-events-timeline","Events over time by type","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","type",4)]),ip,None),
            viz("velox-events-reasons","Reasons","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","reason",12)]),ip,None),
            viz("velox-events-top","Top reasons","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","reason",15)]),ip,None),
            viz("velox-events-ns","Events by namespace","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","metadata.namespace",12)]),ip,None),
            viz("velox-events-kinds","Involved object kinds","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","involvedObject.kind",8)]),ip,None),
            dashboard_obj("velox-events-overview","Kubernetes Events", &[
                ("velox-events-total",    0,  0, 10,  8),
                ("velox-events-warn",     10, 0, 38,  8),
                ("velox-events-timeline", 0,  8, 32, 15),
                ("velox-events-reasons",  32, 8, 16, 15),
                ("velox-events-top",      0, 23, 24, 15),
                ("velox-events-ns",       24,23, 24, 15),
                ("velox-events-kinds",    0, 38, 24, 15),
            ]),
        ],
        "redis" => vec![
            viz("velox-redis-total","Log events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-redis-warn","Warnings over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,Some("level_sym: \"#\"")),
            viz("velox-redis-timeline","Log volume by role","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","role",6)]),ip,None),
            viz("velox-redis-levels","Levels","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","level_sym",6)]),ip,None),
            viz("velox-redis-messages","Top messages","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","redis_message.keyword",10)]),ip,None),
            viz("velox-redis-roles","Roles","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","role",6)]),ip,None),
            dashboard_obj("velox-redis-overview","Redis Overview", &[
                ("velox-redis-total",    0,  0, 10,  8),
                ("velox-redis-warn",     10, 0, 38,  8),
                ("velox-redis-timeline", 0,  8, 32, 15),
                ("velox-redis-levels",   32, 8, 16, 15),
                ("velox-redis-messages", 0, 23, 32, 15),
                ("velox-redis-roles",    32,23, 16, 15),
            ]),
        ],
        "mysql" => vec![
            viz("velox-mysql-total","Log events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-mysql-errors","Errors over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,
                Some("level: Error or level: ERROR")),
            viz("velox-mysql-timeline","Log volume by level","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","level",8)]),ip,None),
            viz("velox-mysql-levels","Levels","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","level",8)]),ip,None),
            viz("velox-mysql-subsystems","Subsystems","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","subsystem",10)]),ip,None),
            viz("velox-mysql-messages","Top messages","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","mysql_message.keyword",10)]),ip,None),
            dashboard_obj("velox-mysql-overview","MySQL / MariaDB Overview", &[
                ("velox-mysql-total",      0,  0, 10,  8),
                ("velox-mysql-errors",     10, 0, 38,  8),
                ("velox-mysql-timeline",   0,  8, 32, 15),
                ("velox-mysql-levels",     32, 8, 16, 15),
                ("velox-mysql-messages",   0, 23, 32, 15),
                ("velox-mysql-subsystems", 32,23, 16, 15),
            ]),
        ],
        "traefik" => vec![
            viz("velox-traefik-reqs","Requests","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-traefik-clients","Unique clients","metric",metric_params(),
                serde_json::json!([cardinality_agg("clientip")]),ip,None),
            viz("velox-traefik-errors","Errors (4xx/5xx) over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,Some("response >= 400")),
            viz("velox-traefik-timeline","Requests over time by status","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","response",6)]),ip,None),
            viz("velox-traefik-status","Status codes","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","response",10)]),ip,None),
            viz("velox-traefik-routers","Top routers","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","traefik_router.keyword",10)]),ip,None),
            viz("velox-traefik-top","Top requests","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","request",10)]),ip,None),
            viz("velox-traefik-ips","Top clients","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","clientip",10)]),ip,None),
            dashboard_obj("velox-traefik-overview","Traefik Overview", &[
                ("velox-traefik-reqs",    0,  0, 10,  8),
                ("velox-traefik-clients", 10, 0, 10,  8),
                ("velox-traefik-errors",  20, 0, 28,  8),
                ("velox-traefik-timeline",0,  8, 32, 15),
                ("velox-traefik-status",  32, 8, 16, 15),
                ("velox-traefik-routers", 0, 23, 24, 15),
                ("velox-traefik-top",     24,23, 24, 15),
                ("velox-traefik-ips",     0, 38, 48, 15),
            ]),
        ],
        "mongo" => vec![
            viz("velox-mongo-total","Log events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-mongo-errors","Errors over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,Some("s: E or s: F")),
            viz("velox-mongo-timeline","Log volume by severity","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","s",6)]),ip,None),
            viz("velox-mongo-sev","Severity","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","s",6)]),ip,None),
            viz("velox-mongo-comp","Components","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","c",12)]),ip,None),
            viz("velox-mongo-messages","Top messages","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","msg.keyword",10)]),ip,None),
            dashboard_obj("velox-mongo-overview","MongoDB Overview", &[
                ("velox-mongo-total",    0,  0, 10,  8),
                ("velox-mongo-errors",   10, 0, 38,  8),
                ("velox-mongo-timeline", 0,  8, 32, 15),
                ("velox-mongo-sev",      32, 8, 16, 15),
                ("velox-mongo-messages", 0, 23, 32, 15),
                ("velox-mongo-comp",     32,23, 16, 15),
            ]),
        ],
        "rabbitmq" => vec![
            viz("velox-rabbitmq-total","Log events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-rabbitmq-errors","Errors over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,
                Some("level: error or level: critical")),
            viz("velox-rabbitmq-timeline","Log volume by level","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","level",8)]),ip,None),
            viz("velox-rabbitmq-levels","Levels","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","level",8)]),ip,None),
            viz("velox-rabbitmq-messages","Top messages","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","rmq_message.keyword",10)]),ip,None),
            dashboard_obj("velox-rabbitmq-overview","RabbitMQ Overview", &[
                ("velox-rabbitmq-total",    0,  0, 10,  8),
                ("velox-rabbitmq-errors",   10, 0, 38,  8),
                ("velox-rabbitmq-timeline", 0,  8, 32, 15),
                ("velox-rabbitmq-levels",   32, 8, 16, 15),
                ("velox-rabbitmq-messages", 0, 23, 48, 15),
            ]),
        ],
        "kafka" => vec![
            viz("velox-kafka-total","Log events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-kafka-errors","Errors over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,
                Some("level: ERROR or level: FATAL")),
            viz("velox-kafka-timeline","Log volume by level","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","level",8)]),ip,None),
            viz("velox-kafka-levels","Levels","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","level",8)]),ip,None),
            viz("velox-kafka-loggers","Top loggers","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","logger",10)]),ip,None),
            viz("velox-kafka-messages","Top messages","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","kafka_message.keyword",10)]),ip,None),
            dashboard_obj("velox-kafka-overview","Kafka Overview", &[
                ("velox-kafka-total",    0,  0, 10,  8),
                ("velox-kafka-errors",   10, 0, 38,  8),
                ("velox-kafka-timeline", 0,  8, 32, 15),
                ("velox-kafka-levels",   32, 8, 16, 15),
                ("velox-kafka-loggers",  0, 23, 24, 15),
                ("velox-kafka-messages", 24,23, 24, 15),
            ]),
        ],
        "ssh" => vec![
            viz("velox-ssh-total","Auth events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-ssh-srcips","Unique source IPs","metric",metric_params(),
                serde_json::json!([cardinality_agg("src_ip")]),ip,None),
            viz("velox-ssh-failed","Failed logins over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,Some("auth_result: Failed")),
            viz("velox-ssh-outcomes","Outcomes","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","auth_result",6)]),ip,None),
            viz("velox-ssh-ips","Top source IPs","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","src_ip",15)]),ip,None),
            viz("velox-ssh-users","Top users","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","auth_user",15)]),ip,None),
            viz("velox-ssh-methods","Auth methods","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","auth_method",6)]),ip,None),
            dashboard_obj("velox-ssh-overview","SSH / Auth Logs", &[
                ("velox-ssh-total",    0,  0, 10,  8),
                ("velox-ssh-srcips",   10, 0, 10,  8),
                ("velox-ssh-failed",   20, 0, 28,  8),
                ("velox-ssh-outcomes", 0,  8, 16, 15),
                ("velox-ssh-ips",      16, 8, 32, 15),
                ("velox-ssh-users",    0, 23, 24, 15),
                ("velox-ssh-methods",  24,23, 24, 15),
            ]),
        ],
        "k8s-audit" => vec![
            viz("velox-k8s-audit-total","Audit events","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-k8s-audit-denied","Denied / errors over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,
                Some("responseStatus.code >= 400")),
            viz("velox-k8s-audit-verbs","Verbs","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","verb",10)]),ip,None),
            viz("velox-k8s-audit-users","Top users","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","user.username",15)]),ip,None),
            viz("velox-k8s-audit-resources","Top resources","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","objectRef.resource",15)]),ip,None),
            viz("velox-k8s-audit-ips","Top source IPs","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","sourceIPs",15)]),ip,None),
            viz("velox-k8s-audit-codes","Response codes","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","responseStatus.code",8)]),ip,None),
            dashboard_obj("velox-k8s-audit-overview","Kubernetes Audit", &[
                ("velox-k8s-audit-total",     0,  0, 10,  8),
                ("velox-k8s-audit-denied",    10, 0, 38,  8),
                ("velox-k8s-audit-verbs",     0,  8, 16, 15),
                ("velox-k8s-audit-users",     16, 8, 32, 15),
                ("velox-k8s-audit-resources", 0, 23, 24, 15),
                ("velox-k8s-audit-ips",       24,23, 24, 15),
                ("velox-k8s-audit-codes",     0, 38, 48, 15),
            ]),
        ],
        _ => vec![
            // metrics row
            viz("velox-nginx-reqs","Requests","metric",metric_params(),
                serde_json::json!([count_agg()]),ip,None),
            viz("velox-nginx-clients","Unique clients","metric",metric_params(),
                serde_json::json!([cardinality_agg("clientip")]),ip,None),
            viz("velox-nginx-errors","Errors (4xx/5xx) over time","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg()]),ip,Some("response >= 400")),
            // main trend + distribution
            viz("velox-nginx-timeline","Requests over time by status","line",line_params(),
                serde_json::json!([count_agg(),datehist_agg(),terms_agg("3","group","response",6)]),ip,None),
            viz("velox-nginx-status","Status codes","pie",pie_params(),
                serde_json::json!([count_agg(),terms_agg("2","segment","response",10)]),ip,None),
            // traffic + top-N
            viz("velox-nginx-bytes","Traffic (bytes) over time","line",line_params(),
                serde_json::json!([sum_agg("bytes"),datehist_agg()]),ip,None),
            viz("velox-nginx-top","Top requests","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","request",10)]),ip,None),
            viz("velox-nginx-ips","Top clients","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","clientip",10)]),ip,None),
            viz("velox-nginx-agents","Top user agents","table",table_params(),
                serde_json::json!([count_agg(),terms_agg("2","bucket","agent.keyword",10)]),ip,None),
            dashboard_obj("velox-nginx-overview","Nginx Overview", &[
                ("velox-nginx-reqs",    0,  0, 10,  8),
                ("velox-nginx-clients", 10, 0, 10,  8),
                ("velox-nginx-errors",  20, 0, 28,  8),
                ("velox-nginx-timeline",0,  8, 32, 15),
                ("velox-nginx-status",  32, 8, 16, 15),
                ("velox-nginx-bytes",   0, 23, 24, 15),
                ("velox-nginx-top",     24,23, 24, 15),
                ("velox-nginx-ips",     0, 38, 24, 15),
                ("velox-nginx-agents",  24,38, 24, 15),
            ]),
        ],
    }
}

/// Create this deployment's Dashboards tenant (best-effort) via the OpenSearch
/// security API, so the saved objects written below are isolated to it instead
/// of polluting the shared Global tenant. Idempotent (PUT upsert). Needs
/// multitenancy enabled on the cluster — a rejection is non-fatal and the saved
/// objects simply fall back to Global.
pub(crate) async fn ensure_tenant(deployment: &Deployment) {
    let base = os_base(deployment);
    let Ok(c) = http() else { return };
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let _ = c
        .put(format!(
            "{base}/_plugins/_security/api/tenants/{}",
            tenant_name(deployment)
        ))
        .basic_auth(&u, Some(&p))
        .json(&serde_json::json!({
            "description": format!("VeloxSearch saved objects for {deployment}")
        }))
        .send()
        .await;
}

/// Create the out-of-the-box dashboard for a recipe (best-effort), scoped to the
/// deployment's tenant via the `securitytenant` header.
async fn ensure_dashboard(deployment: &Deployment, recipe: &str) {
    let dash = dashboards_base(deployment);
    let Ok(c) = http() else { return };
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let mut req = c
        .post(format!("{dash}/api/saved_objects/_bulk_create?overwrite=true"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true");
    if let Some(t) = tenant_scope(deployment).await {
        req = req.header("securitytenant", t);
    }
    let _ = req.json(&dashboard_objects(recipe)).send().await;
}

/// Create the OpenSearch Dashboards index-pattern (best-effort) so the data is
/// visible in Discover, scoped to the deployment's tenant via the
/// `securitytenant` header. NOTE: Dashboards serves plain HTTP on 5601.
/// `time_field` is a parameter rather than a constant because the OTel stack's
/// three patterns each use a different one (`endTime`, `hashId`, `time`) — the
/// recipes all use `@timestamp`, which is what their ingest pipelines normalize
/// into (ADR-053).
pub(crate) async fn ensure_index_pattern(
    deployment: &Deployment,
    id: &str,
    title: &str,
    time_field: &str,
) {
    let dash = dashboards_base(deployment);
    let Ok(c) = http() else { return };
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let mut req = c
        .post(format!("{dash}/api/saved_objects/index-pattern/{id}?overwrite=true"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true");
    if let Some(t) = tenant_scope(deployment).await {
        req = req.header("securitytenant", t);
    }
    let _ = req
        .json(&serde_json::json!({
            "attributes": { "title": title, "timeFieldName": time_field }
        }))
        .send()
        .await;
}

/// Disable a recipe: remove its collection agent AND every OpenSearch object it
/// created — the ingest pipeline, index template, index-pattern and saved
/// objects (dashboard + visualizations). Historically this only removed the
/// agent, orphaning all of the above in the customer's cluster (ADR-039
/// "Uninstall becomes real"). The teardown set is derived in-binary from the
/// recipe's own definitions — the same shape a package manifest declares — and
/// executed through the shared `integrations::teardown_os`, so the built-in
/// recipes and the package engine uninstall identically. Already-indexed *data*
/// (the log documents) is intentionally left; only the config objects go.
pub async fn disable(deployment: &Deployment, recipe: &str) -> Result<()> {
    crate::agents::remove_agent(deployment, recipe).await?;
    let (teardown, kinds) = recipe_teardown(recipe);
    crate::integrations::teardown_os(deployment, &teardown, &kinds).await;
    Ok(())
}

/// Build a recipe's teardown inventory from its own in-binary definitions: the
/// pipeline + template ids (both `recipe` / `recipe_index`, absent for the
/// pipeline-less `kubernetes` recipe), the index-pattern id, and every
/// saved-object id (with its type, for the Dashboards DELETE API) from
/// `dashboard_objects`. This is exactly the data a package's `teardown` block
/// carries — kept in lockstep with what `apply()` creates so clean install ⇒
/// clean uninstall.
pub(crate) fn recipe_teardown(
    recipe: &str,
) -> (
    crate::integrations::Teardown,
    std::collections::BTreeMap<String, String>,
) {
    // Only `kubernetes` installs no pipeline/template (dynamic mapping); every
    // other recipe PUTs a pipeline id == recipe and a template id == its index.
    let has_os_config = recipe != "kubernetes";
    let index = recipe_index(recipe).to_string();

    let mut saved = Vec::new();
    let mut kinds = std::collections::BTreeMap::new();
    for obj in dashboard_objects(recipe) {
        let kind = obj["type"].as_str().unwrap_or_default().to_string();
        let id = obj["id"].as_str().unwrap_or_default().to_string();
        kinds.insert(id.clone(), kind);
        saved.push(id);
    }

    let teardown = crate::integrations::Teardown {
        ingest_pipeline: has_os_config.then(|| recipe.to_string()),
        index_template: has_os_config.then(|| index.clone()),
        index_pattern: index,
        saved_objects: saved,
        // A recipe creates exactly one index-pattern and no ISM policy or
        // datasource of its own — those fields exist for the OTel stack
        // (ADR-053), which reuses this same teardown path.
        index_patterns: Vec::new(),
        ism_policies: Vec::new(),
        datasources: Vec::new(),
    };
    (teardown, kinds)
}

/// Pure per-recipe OpenSearch config: the ingest-pipeline body (PUT under the
/// recipe id) and the index-template body (PUT under the recipe's index).
/// `None` for `kubernetes` (arbitrary logs: dynamic mapping, no pipeline) and
/// for ids outside the catalog.
///
/// This is the in-binary catalog that #74 froze verbatim into the registry
/// packages' `pipeline.json` / `index-template.json` (`integrations/<id>/` in
/// the veloxsearch-registry repo, #105). The `registry_golden` tests assert
/// byte-equivalence between the two, so any edit here must regenerate the
/// packages (`VELOX_REGISTRY_PATH=… VELOX_UPDATE_REGISTRY=1 cargo test`) and
/// land them in the registry repo — until the compiled-in catalog is removed
/// once the catalog client (#75) lands.
///
/// Per-recipe log-format notes (what each pipeline parses):
///   - nginx:     `%{COMBINEDAPACHELOG}`; non-access lines (startup/error)
///                still index (`ignore_failure`).
///   - postgres:  the stderr log format of the official images
///                (`%m [%p] LEVEL:  msg`, plus the optional `user@db`
///                connection prefix); initdb/entrypoint lines still index.
///   - k8s-events: the agent ships the structured Event object; the pipeline
///                derives @timestamp from the event's own time fields (the FB
///                opensearch output can't inject one).
///   - redis:     `<pid>:<role> <REDISTIMESTAMP> <level-symbol> <message>`
///                (Redis 3.0+ default); `role` is M/S/C/X, `level_sym` is
///                `.`/`-`/`*`/`#`.
///   - mysql:     MySQL 8 / MariaDB error log `<ts> <thread> [<level>]
///                [<code>] [<subsystem>] <message>` — the bracketed
///                code/subsystem are MySQL-8-only, so optional.
///   - traefik:   access log (CLF): leading fields match
///                `%{COMBINEDAPACHELOG}` (clientip/verb/request/response/bytes
///                land exactly like nginx); optional trailing
///                router/service/duration captured when present.
///   - mongo:     (4.4+) structured JSON per line (`{"t":{"$date":..},...}`)
///                parsed onto the root; @timestamp from `t.$date`.
///   - rabbitmq:  `<ISO8601 ts> [<level>] <<pid>> <message>` (default
///                single-line format).
///   - kafka:     log4j `[<ts>] <LEVEL> <message> (<logger>)`; trailing
///                `(logger)` captured when present; comma before ms.
///   - ssh:       SECURITY SOURCE — node auth log (syslog frame), not
///                container logs. First pattern lifts ssh auth detail
///                (result/method/user/source ip/port); second is the generic
///                syslog fallback. Syslog has no year → current year assumed.
///                NOTE for #8: Sigma `auth`/`ssh` field mapping targets
///                `src_ip` (ip), `auth_result`/`auth_method`/`auth_user`/
///                `program` (keyword).
///   - k8s-audit: SECURITY SOURCE — API-server audit log (structured JSON,
///                one Event per line) parsed onto the root; @timestamp from
///                `requestReceivedTimestamp`. NOTE for #8: Sigma `k8s-audit`
///                rules map onto `verb`/`level`/`stage`/`requestURI`,
///                `user.username`, `sourceIPs` (ip),
///                `objectRef.{resource,namespace,name}`,
///                `responseStatus.code` (integer).
pub(crate) fn recipe_os_config(recipe: &str) -> Option<(serde_json::Value, serde_json::Value)> {
    let (description, processors, mappings) = match recipe {
        "nginx" => (
            "VeloxSearch nginx access-log pipeline",
            grok_date_processors(
                serde_json::json!(["%{COMBINEDAPACHELOG}"]),
                "timestamp",
                serde_json::json!(["dd/MMM/yyyy:HH:mm:ss Z"]),
            ),
            serde_json::json!({
                "@timestamp": { "type": "date" },
                "clientip":  { "type": "ip" },
                "response":  { "type": "integer" },
                "bytes":     { "type": "long" },
                "verb":      { "type": "keyword" },
                "request":   { "type": "keyword" }
            }),
        ),
        "postgres" => (
            "VeloxSearch postgres server-log pipeline",
            grok_date_processors(
                serde_json::json!([
                    "^%{TIMESTAMP_ISO8601:pg_time} %{WORD:pg_tz} \\[%{POSINT:pid}\\] (?:%{DATA:pg_user}@%{DATA:pg_db} )?%{WORD:level}:\\s+%{GREEDYDATA:pg_message}"
                ]),
                "pg_time",
                serde_json::json!(["yyyy-MM-dd HH:mm:ss.SSS", "yyyy-MM-dd HH:mm:ss"]),
            ),
            serde_json::json!({
                "@timestamp":  { "type": "date" },
                "level":       { "type": "keyword" },
                "pid":         { "type": "integer" },
                "pg_user":     { "type": "keyword" },
                "pg_db":       { "type": "keyword" },
                "pg_message":  { "type": "text",
                                 "fields": { "keyword": { "type": "keyword", "ignore_above": 512 } } }
            }),
        ),
        "k8s-events" => (
            "VeloxSearch k8s-events pipeline (@timestamp from event times)",
            serde_json::json!([
                { "date": { "field": "lastTimestamp", "target_field": "@timestamp",
                            "formats": ["ISO8601"], "ignore_failure": true }},
                { "date": { "if": "ctx['@timestamp'] == null", "field": "eventTime",
                            "target_field": "@timestamp", "formats": ["ISO8601"],
                            "ignore_failure": true }},
                { "date": { "if": "ctx['@timestamp'] == null",
                            "field": "metadata.creationTimestamp",
                            "target_field": "@timestamp", "formats": ["ISO8601"],
                            "ignore_failure": true }}
            ]),
            serde_json::json!({
                "@timestamp": { "type": "date" },
                "type":       { "type": "keyword" },
                "reason":     { "type": "keyword" },
                "message":    { "type": "text" },
                "count":      { "type": "long" },
                "metadata":   { "properties": { "namespace": { "type": "keyword" } } },
                "involvedObject": { "properties": {
                    "kind": { "type": "keyword" },
                    "name": { "type": "keyword" }
                }},
                "source": { "properties": { "component": { "type": "keyword" } } }
            }),
        ),
        "redis" => (
            "VeloxSearch redis server-log pipeline",
            grok_date_processors(
                serde_json::json!([
                    "^%{POSINT:pid}:%{DATA:role} %{REDISTIMESTAMP:redis_time} %{DATA:level_sym} %{GREEDYDATA:redis_message}"
                ]),
                "redis_time",
                serde_json::json!(["dd MMM yyyy HH:mm:ss.SSS", "dd MMM HH:mm:ss.SSS"]),
            ),
            serde_json::json!({
                "@timestamp":    { "type": "date" },
                "pid":           { "type": "integer" },
                "role":          { "type": "keyword" },
                "level_sym":     { "type": "keyword" },
                "redis_message": { "type": "text",
                                   "fields": { "keyword": { "type": "keyword", "ignore_above": 512 } } }
            }),
        ),
        "mysql" => (
            "VeloxSearch mysql/mariadb server-log pipeline",
            grok_date_processors(
                serde_json::json!([
                    "^%{TIMESTAMP_ISO8601:mysql_time}\\s+%{NUMBER:thread_id} \\[%{DATA:level}\\] (?:\\[%{DATA:err_code}\\] )?(?:\\[%{DATA:subsystem}\\] )?%{GREEDYDATA:mysql_message}"
                ]),
                "mysql_time",
                serde_json::json!(["ISO8601", "yyyy-MM-dd HH:mm:ss"]),
            ),
            serde_json::json!({
                "@timestamp":    { "type": "date" },
                "thread_id":     { "type": "long" },
                "level":         { "type": "keyword" },
                "err_code":      { "type": "keyword" },
                "subsystem":     { "type": "keyword" },
                "mysql_message": { "type": "text",
                                   "fields": { "keyword": { "type": "keyword", "ignore_above": 512 } } }
            }),
        ),
        "traefik" => (
            "VeloxSearch traefik access-log pipeline",
            grok_date_processors(
                serde_json::json!([
                    "%{COMBINEDAPACHELOG} %{NUMBER:request_count} \"%{DATA:traefik_router}\" \"%{DATA:traefik_service}\" %{NUMBER:duration_ms}ms",
                    "%{COMBINEDAPACHELOG}"
                ]),
                "timestamp",
                serde_json::json!(["dd/MMM/yyyy:HH:mm:ss Z"]),
            ),
            serde_json::json!({
                "@timestamp":      { "type": "date" },
                "clientip":        { "type": "ip" },
                "response":        { "type": "integer" },
                "bytes":           { "type": "long" },
                "verb":            { "type": "keyword" },
                "request":         { "type": "keyword" },
                "duration_ms":     { "type": "float" },
                "traefik_router":  { "type": "text",
                                     "fields": { "keyword": { "type": "keyword", "ignore_above": 256 } } },
                "traefik_service": { "type": "keyword" }
            }),
        ),
        "mongo" => (
            "VeloxSearch mongodb server-log pipeline",
            json_date_processors("t.$date"),
            serde_json::json!({
                "@timestamp": { "type": "date" },
                "s":          { "type": "keyword" },
                "c":          { "type": "keyword" },
                "ctx":        { "type": "keyword" },
                "id":         { "type": "long" },
                "msg":        { "type": "text",
                                "fields": { "keyword": { "type": "keyword", "ignore_above": 512 } } }
            }),
        ),
        "rabbitmq" => (
            "VeloxSearch rabbitmq server-log pipeline",
            grok_date_processors(
                serde_json::json!([
                    "^%{TIMESTAMP_ISO8601:rmq_time} \\[%{DATA:level}\\] <%{DATA:rmq_pid}> %{GREEDYDATA:rmq_message}"
                ]),
                "rmq_time",
                serde_json::json!(["ISO8601", "yyyy-MM-dd HH:mm:ss.SSSSSSZ", "yyyy-MM-dd HH:mm:ss.SSSZ"]),
            ),
            serde_json::json!({
                "@timestamp":  { "type": "date" },
                "level":       { "type": "keyword" },
                "rmq_pid":     { "type": "keyword" },
                "rmq_message": { "type": "text",
                                 "fields": { "keyword": { "type": "keyword", "ignore_above": 512 } } }
            }),
        ),
        "kafka" => (
            "VeloxSearch kafka server-log pipeline",
            grok_date_processors(
                serde_json::json!([
                    "^\\[%{TIMESTAMP_ISO8601:kafka_time}\\] %{LOGLEVEL:level}\\s+%{GREEDYDATA:kafka_message} \\(%{NOTSPACE:logger}\\)$",
                    "^\\[%{TIMESTAMP_ISO8601:kafka_time}\\] %{LOGLEVEL:level}\\s+%{GREEDYDATA:kafka_message}"
                ]),
                "kafka_time",
                serde_json::json!(["yyyy-MM-dd HH:mm:ss,SSS", "ISO8601"]),
            ),
            serde_json::json!({
                "@timestamp":    { "type": "date" },
                "level":         { "type": "keyword" },
                "logger":        { "type": "keyword" },
                "kafka_message": { "type": "text",
                                   "fields": { "keyword": { "type": "keyword", "ignore_above": 512 } } }
            }),
        ),
        "ssh" => (
            "VeloxSearch ssh/auth security-log pipeline",
            grok_date_processors(
                serde_json::json!([
                    "%{SYSLOGTIMESTAMP:syslog_time} %{SYSLOGHOST:host} %{DATA:program}(?:\\[%{POSINT:pid}\\])?: %{WORD:auth_result} %{WORD:auth_method} for (?:invalid user )?%{DATA:auth_user} from %{IP:src_ip} port %{NUMBER:src_port}",
                    "%{SYSLOGTIMESTAMP:syslog_time} %{SYSLOGHOST:host} %{DATA:program}(?:\\[%{POSINT:pid}\\])?: %{GREEDYDATA:auth_message}"
                ]),
                "syslog_time",
                serde_json::json!(["MMM d HH:mm:ss", "MMM dd HH:mm:ss"]),
            ),
            serde_json::json!({
                "@timestamp":   { "type": "date" },
                "host":         { "type": "keyword" },
                "program":      { "type": "keyword" },
                "pid":          { "type": "integer" },
                "auth_result":  { "type": "keyword" },
                "auth_method":  { "type": "keyword" },
                "auth_user":    { "type": "keyword" },
                "src_ip":       { "type": "ip" },
                "src_port":     { "type": "integer" },
                "auth_message": { "type": "text" }
            }),
        ),
        "k8s-audit" => (
            "VeloxSearch k8s audit-log pipeline",
            json_date_processors("requestReceivedTimestamp"),
            serde_json::json!({
                "@timestamp":     { "type": "date" },
                "level":          { "type": "keyword" },
                "stage":          { "type": "keyword" },
                "verb":           { "type": "keyword" },
                "requestURI":     { "type": "keyword" },
                "sourceIPs":      { "type": "ip" },
                "user":           { "properties": { "username": { "type": "keyword" } } },
                "objectRef":      { "properties": {
                    "resource":  { "type": "keyword" },
                    "namespace": { "type": "keyword" },
                    "name":      { "type": "keyword" }
                }},
                "responseStatus": { "properties": { "code": { "type": "integer" } } }
            }),
        ),
        _ => return None,
    };
    let index = recipe_index(recipe);
    let pipeline = serde_json::json!({ "description": description, "processors": processors });
    let template = serde_json::json!({
        "index_patterns": [format!("{index}*")],
        "template": {
            "settings": { "index.default_pipeline": recipe },
            "mappings": { "properties": mappings }
        }
    });
    Some((pipeline, template))
}

/// Install a recipe's OpenSearch config, if it has one: PUT the ingest
/// pipeline (id = recipe) and the index template (id = the recipe's index,
/// matching `{index}*`). No-op for `kubernetes` (dynamic mapping). This is
/// what the per-recipe `configure_*` functions did by hand, now fed from the
/// pure `recipe_os_config` catalog.
async fn configure_os(deployment: &Deployment, recipe: &str) -> Result<()> {
    let Some((pipeline, template)) = recipe_os_config(recipe) else {
        return Ok(());
    };
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;

    c.put(format!("{base}/_ingest/pipeline/{recipe}"))
        .basic_auth(&u, Some(&p))
        .json(&pipeline)
        .send()
        .await
        .with_context(|| format!("creating {recipe} pipeline"))?
        .error_for_status()
        .with_context(|| format!("{recipe} pipeline rejected"))?;

    c.put(format!("{base}/_index_template/{}", recipe_index(recipe)))
        .basic_auth(&u, Some(&p))
        .json(&template)
        .send()
        .await
        .with_context(|| format!("creating {recipe} template"))?
        .error_for_status()
        .with_context(|| format!("{recipe} template rejected"))?;
    Ok(())
}

/// A grok + date pipeline (the common log-line case). `patterns` are tried in
/// order; non-matching lines still index (`ignore_failure`). The date is read
/// from `time_field` using `time_formats` (best-effort).
fn grok_date_processors(
    patterns: serde_json::Value,
    time_field: &str,
    time_formats: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!([
        { "grok": { "field": "message", "patterns": patterns,
                    "ignore_missing": true, "ignore_failure": true } },
        { "date": { "field": time_field, "target_field": "@timestamp",
                    "formats": time_formats, "ignore_failure": true } }
    ])
}

/// A json + date pipeline (services that log structured JSON, e.g. mongo,
/// k8s audit). The line is parsed onto the root document; the date is read
/// from `time_field`.
fn json_date_processors(time_field: &str) -> serde_json::Value {
    serde_json::json!([
        { "json": { "field": "message", "add_to_root": true, "ignore_failure": true } },
        { "date": { "field": time_field, "target_field": "@timestamp",
                    "formats": ["ISO8601"], "ignore_failure": true } }
    ])
}

/// Document count in a recipe's index. A wildcard `_count` over a missing
/// index legitimately returns 0; any non-success response is an ERROR — it
/// must never masquerade as "0 docs" or the UI flaps between states on
/// transient OpenSearch hiccups.
pub async fn doc_count(deployment: &Deployment, recipe: &str) -> Result<u64> {
    doc_count_of(deployment, &format!("{}*", recipe_index(recipe))).await
}

/// Document count over an arbitrary index pattern. Shared with the OTel stack
/// (ADR-053), which counts `otel-v1-apm-span*` rather than a recipe index, so
/// the "non-success is an ERROR, never 0 docs" rule lives in exactly one place.
pub async fn doc_count_of(deployment: &Deployment, pattern: &str) -> Result<u64> {
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let resp = c
        .get(format!("{base}/{pattern}/_count"))
        .basic_auth(&u, Some(&p))
        .send()
        .await
        .context("querying doc count")?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("doc count query returned {status}");
    }
    let body: serde_json::Value = resp.json().await.context("parsing count")?;
    Ok(body.get("count").and_then(|v| v.as_u64()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The Dashboards tenant is per-deployment and namespaced, so two
    /// deployments never share a saved-object space.
    #[test]
    fn tenant_name_is_per_deployment() {
        let dep = |n: &str| Deployment::for_test(n, "veloxsearch-test", None);
        assert_eq!(tenant_name(&dep("logs-ab12")), "velox-logs-ab12");
        assert_ne!(tenant_name(&dep("a")), tenant_name(&dep("b")));
        assert!(tenant_name(&dep("x")).starts_with("velox-"));
    }

    /// The service DNS a recipe writes to is built from the deployment's OWN
    /// namespace (#80/ADR-044): two tenants' same-named deployments resolve to
    /// different clusters, and a handle from tenant A can never address B's.
    #[test]
    fn opensearch_urls_follow_the_deployments_namespace() {
        let a = Deployment::for_test("logs-ab12", "velox-t-acme", Some("tenant-a"));
        let b = Deployment::for_test("logs-ab12", "velox-t-globex", Some("tenant-b"));
        assert_eq!(os_base(&a), "https://logs-ab12.velox-t-acme.svc:9200");
        assert_ne!(os_base(&a), os_base(&b));
        assert_ne!(dashboards_base(&a), dashboards_base(&b));
    }

    /// Round-2 ids map to their dedicated index const (not the nginx fallback).
    #[test]
    fn recipe_index_maps_round2() {
        assert_eq!(recipe_index("redis"), REDIS_INDEX);
        assert_eq!(recipe_index("mysql"), MYSQL_INDEX);
        assert_eq!(recipe_index("traefik"), TRAEFIK_INDEX);
        assert_eq!(recipe_index("mongo"), MONGO_INDEX);
        assert_eq!(recipe_index("rabbitmq"), RABBITMQ_INDEX);
        assert_eq!(recipe_index("kafka"), KAFKA_INDEX);
        assert_eq!(recipe_index("ssh"), SSH_INDEX);
        assert_eq!(recipe_index("k8s-audit"), K8S_AUDIT_INDEX);
    }

    /// Every catalog id has a velox-* dashboard slug, and they are all unique
    /// (a duplicate slug would have two recipes overwrite each other's saved
    /// dashboard object).
    #[test]
    fn dashboard_ids_unique_and_namespaced() {
        let mut seen = HashSet::new();
        for r in RECIPES {
            let id = dashboard_id(r);
            assert!(id.starts_with("velox-"), "{r} -> {id}");
            assert!(seen.insert(id), "duplicate dashboard id {id}");
        }
    }

    /// For every catalog id, `dashboard_objects` ends with a dashboard whose id
    /// is `dashboard_id(recipe)`, and every panel reference resolves to a viz
    /// object defined earlier in the same bundle (catches a mistyped panel id).
    #[test]
    fn dashboard_objects_are_self_consistent() {
        for r in RECIPES {
            let objs = dashboard_objects(r);
            assert!(objs.len() >= 2, "{r}: too few objects");
            let (dash, vizzes) = objs.split_last().unwrap();
            assert_eq!(dash["type"], "dashboard", "{r}: last object not a dashboard");
            assert_eq!(dash["id"], dashboard_id(r), "{r}: dashboard id mismatch");

            let viz_ids: HashSet<&str> =
                vizzes.iter().map(|v| v["id"].as_str().unwrap()).collect();
            for rf in dash["references"].as_array().unwrap() {
                let id = rf["id"].as_str().unwrap();
                assert!(viz_ids.contains(id), "{r}: panel {id} has no viz");
            }
        }
    }
}
