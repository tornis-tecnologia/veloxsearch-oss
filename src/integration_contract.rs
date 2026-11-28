// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! ADR-075 — the integration install contract.
//!
//! Writing an integration's objects is not the same as the integration
//! working. Every way that gap opened before this module existed produced the
//! same screen — a dashboard where every panel is broken — and a success
//! toast. The contract closes it:
//!
//! 1. **Preflight** ([`preflight`]): before anything is written, the blockers
//!    the core can detect — the permissions the install needs, and for the
//!    Kubernetes audit log whether the API server writes one at all and
//!    whether any node can run a collector where it lives.
//! 2. **Provision** (`integrations::apply_package`): templates, the index
//!    itself (so the pattern gets its fields at install), the pattern with its
//!    field list, dashboards, collector.
//! 3. **Follow** ([`follow_install`]): after the write, [`evaluate`] runs until
//!    the dashboards work, refreshing index-pattern fields as data lands.
//! 4. **Report** ([`IntegrationStatus`]): `working`, `waiting_for_data` (what
//!    is expected, from where) or `blocked` (why, and the exact remediation).
//!    Never a silent success.
//!
//! [`evaluate`] is idempotent and is also the reconcile: a status read, a
//! re-install and the startup pass ([`reconcile_on_startup`]) all heal an
//! index pattern that was saved without fields.

use crate::scope::Deployment;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// How long [`follow_install`] keeps evaluating a fresh install, and how often.
const FOLLOW_FOR: std::time::Duration = std::time::Duration::from_secs(15 * 60);
const FOLLOW_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

/// Where the API server's audit log is read from (the k8s-audit collector).
const AUDIT_LOG: &str = "/var/log/kubernetes/audit/audit.log";

/// Where an integration stands. Serialized with a `state` tag so the console
/// switches on one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    /// Documents arrive and every panel's fields resolve.
    Working,
    /// Installed correctly; nothing to show yet. `expected` is what will fill
    /// the dashboard, `source` where the collector reads it.
    WaitingForData { expected: String, source: String },
    /// Something prevents the dashboards from ever working until the
    /// remediation is applied.
    Blocked { reason: String, remediation: String },
}

/// One verified fact behind a [`State`], for the console's detail list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// `POST /api/integration_status` and the install routes' answer.
#[derive(Debug, Clone, Serialize)]
pub struct IntegrationStatus {
    pub id: String,
    #[serde(flatten)]
    pub state: State,
    /// Documents in the integration's index pattern.
    pub docs: u64,
    pub checks: Vec<Check>,
}

/// A preflight result. A `blocker` means the dashboards cannot work on this
/// cluster as it is; the rest are what the user should know before pressing
/// Install. Either way the install is the user's call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub blocker: bool,
    pub reason: String,
    pub remediation: String,
}

/// `POST /api/integration_preflight`.
#[derive(Debug, Clone, Serialize)]
pub struct Preflight {
    pub id: String,
    pub findings: Vec<Finding>,
}

// ---------------------------------------------------------------------------
// Pure: what a package collects and what its dashboards read.
// ---------------------------------------------------------------------------

/// Where a rendered collector config reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Container log files under `/var/log/containers` (the globs).
    ContainerLogs(Vec<String>),
    /// Files on the node itself.
    HostFiles(Vec<String>),
    /// The API server's Event stream.
    ApiEvents,
    /// No in-cluster collection (a host-only selection).
    None,
}

/// Read the source off a rendered Fluent Bit config rather than a per-id
/// table, so a registry package that changes its paths is described as it
/// actually collects.
pub fn source_of(conf: Option<&str>) -> Source {
    let Some(conf) = conf else {
        return Source::None;
    };
    if conf.contains("Name kubernetes_events") {
        return Source::ApiEvents;
    }
    let paths: Vec<String> = conf
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Path "))
        .flat_map(|p| p.split(','))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if paths.is_empty() {
        Source::None
    } else if paths.iter().all(|p| p.starts_with("/var/log/containers/")) {
        Source::ContainerLogs(paths)
    } else {
        Source::HostFiles(paths)
    }
}

/// `(expected, source)` for a `waiting_for_data` status.
pub fn describe_source(id: &str, source: &Source) -> (String, String) {
    match source {
        Source::ContainerLogs(globs) => (
            format!(
                "log lines from containers whose log file matches {}",
                globs.join(" or ")
            ),
            "/var/log/containers on every node (the collector DaemonSet)".into(),
        ),
        Source::HostFiles(paths) if id == "k8s-audit" => (
            format!("API-server audit events appended to {}", paths.join(", ")),
            "the control-plane hosts, where kube-apiserver writes its audit log".into(),
        ),
        Source::HostFiles(paths) => (
            format!("lines appended to {}", paths.join(" or ")),
            "the filesystem of every node the collector runs on".into(),
        ),
        Source::ApiEvents => (
            "Kubernetes Events".into(),
            "the API server's event stream (one collector replica)".into(),
        ),
        Source::None => (
            "documents from the hosts this integration is collected on".into(),
            "outside the cluster (no in-cluster input is enabled)".into(),
        ),
    }
}

/// The fixed part of a container-log glob: `*nginx*.log` → `nginx`.
fn glob_needle(glob: &str) -> Option<&str> {
    let file = glob.rsplit('/').next()?;
    let needle = file.trim_end_matches(".log").trim_matches('*');
    (!needle.is_empty() && !needle.contains('*')).then_some(needle)
}

/// Index patterns in a rendered saved-objects ndjson: `(id, title)`.
pub fn index_patterns(ndjson: &str) -> Vec<(String, String)> {
    ndjson
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|o| o["type"] == "index-pattern")
        .filter_map(|o| {
            Some((
                o["id"].as_str()?.to_string(),
                o["attributes"]["title"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// The fields each visualization aggregates on, grouped by the index pattern
/// it reads: pattern id → field → panel titles. These are exactly the fields
/// whose absence from the pattern makes Dashboards draw "Could not locate that
/// index-pattern-field". Panel *queries* (kuery) are not parsed: a field only
/// a query names fails the panel's filter, not its rendering.
pub fn panel_fields(ndjson: &str) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
    let mut out: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for o in ndjson
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|o| o["type"] == "visualization")
    {
        let Some(pattern) = o["references"].as_array().and_then(|r| {
            r.iter()
                .find(|r| r["type"] == "index-pattern")
                .and_then(|r| r["id"].as_str())
        }) else {
            continue;
        };
        let Some(vs) = o["attributes"]["visState"]
            .as_str()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        else {
            continue;
        };
        let title = vs["title"].as_str().unwrap_or_default().to_string();
        for agg in vs["aggs"].as_array().into_iter().flatten() {
            if let Some(f) = agg["params"]["field"].as_str() {
                out.entry(pattern.to_string())
                    .or_default()
                    .entry(f.to_string())
                    .or_default()
                    .push(title.clone());
            }
        }
    }
    out
}

/// Field names in a saved pattern's `fields` attribute (a JSON string).
fn saved_field_names(attrs: &serde_json::Value) -> BTreeSet<String> {
    attrs["fields"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(s).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|f| f["name"].as_str().map(str::to_owned))
        .collect()
}

/// The decision, pure: everything [`evaluate`] measured → a state.
pub struct Measured<'a> {
    pub id: &'a str,
    pub source: &'a Source,
    pub docs: u64,
    /// Patterns Dashboards does not have (the install could not write them).
    pub patterns_missing: Vec<String>,
    /// Panel fields still absent from their pattern after the refresh.
    pub fields_unresolved: Vec<String>,
    /// Panel fields resolved but with no value in any document.
    pub fields_empty: Vec<String>,
    /// Every panel field of the package.
    pub fields_wanted: Vec<String>,
    /// `(desired, ready)` of the collector; `None` when none is deployed.
    pub collector: Option<(i64, i64)>,
    /// The preflight blockers that apply right now.
    pub blockers: Vec<Finding>,
}

pub fn decide(m: &Measured) -> State {
    if let Some(p) = m.patterns_missing.first() {
        return State::Blocked {
            reason: format!("the index pattern {p} is not in Dashboards"),
            remediation: "Press Install again once Dashboards is reachable: the install writes \
                          the dashboards and their index pattern into it."
                .into(),
        };
    }
    if m.docs == 0 {
        if let Some(b) = m.blockers.first() {
            return State::Blocked {
                reason: b.reason.clone(),
                remediation: b.remediation.clone(),
            };
        }
        match (m.source, m.collector) {
            (Source::None, _) => {}
            (_, None) => {
                return State::Blocked {
                    reason: "no collector is deployed for this integration".into(),
                    remediation: "Press Install again: it deploys the collector.".into(),
                }
            }
            (_, Some((0, _))) => {
                return State::Blocked {
                    reason: "the collector is scheduled on no node".into(),
                    remediation:
                        "Check that the cluster has Ready nodes and that the velox-agents \
                                  namespace is not restricted by a quota or admission policy."
                            .into(),
                }
            }
            _ => {}
        }
        let (expected, source) = describe_source(m.id, m.source);
        return State::WaitingForData { expected, source };
    }
    if !m.fields_unresolved.is_empty() {
        return State::Blocked {
            reason: format!(
                "documents arrive but the index pattern still lacks {}",
                m.fields_unresolved.join(", ")
            ),
            remediation: "The documents do not carry these fields, so the panels that \
                          aggregate on them cannot render. Check that the source writes the \
                          format this integration's ingest pipeline parses."
                .into(),
        };
    }
    // Every panel but the time axis empty: nothing was parsed.
    let content: Vec<&String> = m
        .fields_wanted
        .iter()
        .filter(|f| *f != "@timestamp")
        .collect();
    if !content.is_empty() && content.iter().all(|f| m.fields_empty.contains(f)) {
        return State::Blocked {
            reason: format!(
                "documents arrive but none carries the fields the dashboard reads ({})",
                m.fields_empty.join(", ")
            ),
            remediation: "The ingest pipeline is not parsing these lines: the source is not \
                          writing the log format this integration expects (for container \
                          logs, the image's default format)."
                .into(),
        };
    }
    State::Working
}

// ---------------------------------------------------------------------------
// Live: preflight.
// ---------------------------------------------------------------------------

fn audit_remediation() -> String {
    format!(
        "Enable API-server audit logging on every control-plane node with a policy and a log \
         file at the path the collector reads:\n\
         \n  --audit-policy-file=/etc/kubernetes/audit-policy.yaml\
         \n  --audit-log-path={AUDIT_LOG}\
         \n  --audit-log-maxage=7 --audit-log-maxbackup=3 --audit-log-maxsize=100\n\
         \nMinimal policy (/etc/kubernetes/audit-policy.yaml):\n\
         \n  apiVersion: audit.k8s.io/v1\
         \n  kind: Policy\
         \n  rules:\
         \n    - level: Metadata\n\
         \nWhere the flags go: kubeadm — ClusterConfiguration apiServer.extraArgs plus \
         extraVolumes for both paths; k3s — kube-apiserver-arg in /etc/rancher/k3s/config.yaml; \
         k0s — spec.api.extraArgs in k0s.yaml. Then restart the API server."
    )
}

fn no_control_plane_kubelet() -> Finding {
    Finding {
        blocker: true,
        reason: "no control-plane node of this cluster runs a kubelet, so no collector pod can \
                 run where kube-apiserver writes its audit log (controller-only control plane: \
                 k0s controllers without --enable-worker, or a managed control plane)"
            .into(),
        remediation: format!(
            "Ship {AUDIT_LOG} from each control-plane host: run any log shipper there that \
             writes to this integration's index as veloxagent-user, or make the controllers \
             nodes (k0s: --enable-worker), after which the collector lands on them by itself. \
             A managed control plane exposes its audit log only through the provider's own \
             log service."
        ),
    }
}

/// Can the app's service account do `verb` on `resource`?
async fn can(
    client: &kube::Client,
    verb: &str,
    group: &str,
    resource: &str,
    ns: Option<&str>,
) -> Option<bool> {
    use k8s_openapi::api::authorization::v1::{
        ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
    };
    let ssar = SelfSubjectAccessReview {
        spec: SelfSubjectAccessReviewSpec {
            resource_attributes: Some(ResourceAttributes {
                verb: Some(verb.into()),
                group: Some(group.into()),
                resource: Some(resource.into()),
                namespace: ns.map(str::to_owned),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let api: kube::Api<SelfSubjectAccessReview> = kube::Api::all(client.clone());
    api.create(&Default::default(), &ssar)
        .await
        .ok()
        .map(|r| r.status.map(|s| s.allowed).unwrap_or(false))
}

/// Sum of `apiserver_audit_event_total` in an API server's `/metrics`.
/// `None` when the metric is not exposed at all.
pub fn audit_events(metrics: &str) -> Option<f64> {
    let mut seen = false;
    let mut sum = 0.0;
    for l in metrics.lines() {
        if let Some(rest) = l.strip_prefix("apiserver_audit_event_total") {
            if let Some(v) = rest.rsplit(' ').next().and_then(|v| v.parse::<f64>().ok()) {
                seen = true;
                sum += v;
            }
        }
    }
    seen.then_some(sum)
}

/// The k8s-audit blockers: no node where the log lives, or no log at all.
async fn audit_findings(client: &kube::Client) -> Vec<Finding> {
    use k8s_openapi::api::core::v1::Node;
    let mut out = Vec::new();
    let nodes: kube::Api<Node> = kube::Api::all(client.clone());
    if let Ok(list) = nodes.list(&Default::default()).await {
        let control_plane = list.items.iter().any(|n| {
            n.metadata.labels.as_ref().is_some_and(|l| {
                l.contains_key("node-role.kubernetes.io/control-plane")
                    || l.contains_key("node-role.kubernetes.io/master")
            })
        });
        if !control_plane {
            out.push(no_control_plane_kubelet());
        }
    }
    if let Ok(req) = http::Request::get("/metrics").body(Vec::new()) {
        match client.request_text(req).await {
            Ok(text) => match audit_events(&text) {
                Some(n) if n > 0.0 => {}
                _ => out.push(Finding {
                    blocker: true,
                    reason: "API-server audit logging is disabled: kube-apiserver has produced \
                             no audit events (apiserver_audit_event_total is 0), so there is \
                             no audit log to collect"
                        .into(),
                    remediation: audit_remediation(),
                }),
            },
            Err(e) => out.push(Finding {
                blocker: false,
                reason: format!("could not check whether API-server audit logging is on ({e})"),
                remediation: format!(
                    "Make sure kube-apiserver runs with --audit-policy-file and \
                     --audit-log-path={AUDIT_LOG}; the dashboard stays empty otherwise."
                ),
            }),
        }
    }
    out
}

/// Running containers whose log file a container-log glob would match.
async fn matching_containers(client: &kube::Client, globs: &[String]) -> Option<Vec<String>> {
    use k8s_openapi::api::core::v1::Pod;
    let needles: Vec<&str> = globs.iter().filter_map(|g| glob_needle(g)).collect();
    if needles.is_empty() {
        return None;
    }
    let pods: kube::Api<Pod> = kube::Api::all(client.clone());
    let list = pods.list(&Default::default()).await.ok()?;
    let mut hits = Vec::new();
    for p in &list.items {
        let (Some(name), Some(ns)) = (p.metadata.name.as_deref(), p.metadata.namespace.as_deref())
        else {
            continue;
        };
        if ns == crate::agents::AGENT_NS {
            continue;
        }
        for c in p.spec.iter().flat_map(|s| s.containers.iter()) {
            // The kubelet's file name: <pod>_<namespace>_<container>-<id>.log
            let file = format!("{name}_{ns}_{}", c.name);
            if needles.iter().any(|n| file.contains(n)) {
                hits.push(format!("{ns}/{name}"));
            }
        }
    }
    Some(hits)
}

/// Everything the core can tell about installing `pkg` on `deployment`
/// before a byte is written.
pub async fn preflight_package(
    deployment: &Deployment,
    pkg: &velox_integrations::Package,
    version: &str,
    selection: &velox_integrations::InputSelection,
) -> Result<Preflight> {
    let id = pkg.manifest.id.clone();
    let conf = crate::integrations::collector_config(deployment, pkg, version, selection)?;
    let source = source_of(conf.as_deref());
    let client = crate::k8s::client().await?;
    let mut findings = Vec::new();

    if !matches!(source, Source::None) {
        let ns = Some(crate::agents::AGENT_NS);
        let (group, resource) = match source {
            Source::ApiEvents => ("apps", "deployments"),
            _ => ("apps", "daemonsets"),
        };
        if can(&client, "create", group, resource, ns).await == Some(false) {
            findings.push(Finding {
                blocker: true,
                reason: format!(
                    "VeloxSearch may not create {resource} in the {} namespace, so it cannot \
                     deploy the collector",
                    crate::agents::AGENT_NS
                ),
                remediation: format!(
                    "Re-apply VeloxSearch's install.yaml: its Role veloxsearch-runtime in {} \
                     grants this.",
                    crate::agents::AGENT_NS
                ),
            });
        }
    }
    match &source {
        Source::HostFiles(_) if id == "k8s-audit" => findings.extend(audit_findings(&client).await),
        Source::HostFiles(paths) => findings.push(Finding {
            blocker: false,
            reason: format!("the collector reads {} on every node", paths.join(" or ")),
            remediation: "Nodes that log only to the systemd journal (no rsyslog) have none of \
                          these files; install rsyslog there, or the dashboard stays empty."
                .into(),
        }),
        Source::ContainerLogs(globs) => {
            if let Some(hits) = matching_containers(&client, globs).await {
                if hits.is_empty() {
                    findings.push(Finding {
                        blocker: false,
                        reason: format!(
                            "no running container's log file matches {}",
                            globs.join(" or ")
                        ),
                        remediation: "The dashboard fills once such a container runs on this \
                                      cluster; until then the integration waits for data."
                            .into(),
                    });
                }
            }
        }
        Source::ApiEvents | Source::None => {}
    }
    Ok(Preflight { id, findings })
}

/// [`preflight_package`] for a catalog id, at the version and inputs an
/// install would use.
pub async fn preflight(deployment: &Deployment, id: &str) -> Result<Preflight> {
    let pkg = crate::catalog::package_for(id).await?;
    let selection = crate::k8s::integration_inputs(deployment)
        .await
        .ok()
        .and_then(|m| m.get(id).cloned())
        .unwrap_or_default();
    let version = crate::k8s::integration_versions(deployment)
        .await
        .ok()
        .and_then(|m| m.get(id).cloned())
        .unwrap_or_else(|| crate::catalog::BOOTSTRAP_VERSION.to_string());
    preflight_package(deployment, &pkg, &version, &selection).await
}

// ---------------------------------------------------------------------------
// Live: evaluate (= verify + reconcile).
// ---------------------------------------------------------------------------

struct Dash<'a> {
    c: &'a reqwest::Client,
    base: String,
    tenant: Option<String>,
    u: &'a str,
    p: &'a str,
}

impl Dash<'_> {
    fn req(&self, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let r = r
            .basic_auth(self.u, Some(self.p))
            .header("osd-xsrf", "true");
        match &self.tenant {
            Some(t) => r.header("securitytenant", t),
            None => r,
        }
    }

    /// The saved pattern's attributes, `None` when Dashboards has no such
    /// pattern.
    async fn pattern(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let r = self
            .req(self.c.get(format!(
                "{}/api/saved_objects/index-pattern/{id}",
                self.base
            )))
            .send()
            .await
            .context("reading index pattern")?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body: serde_json::Value = r.error_for_status()?.json().await?;
        Ok(Some(body["attributes"].clone()))
    }

    async fn fields_for(&self, title: &str) -> Result<Vec<serde_json::Value>> {
        let body: serde_json::Value = self
            .req(self.c.get(format!(
                "{}/api/index_patterns/_fields_for_wildcard",
                self.base
            )))
            .query(&[("pattern", title)])
            .send()
            .await?
            .json()
            .await?;
        Ok(body["fields"].as_array().cloned().unwrap_or_default())
    }

    /// Partial update: only `fields` changes.
    async fn write_fields(&self, id: &str, fields: &[serde_json::Value]) -> Result<()> {
        let body = serde_json::json!({ "attributes": {
            "fields": serde_json::Value::Array(fields.to_vec()).to_string()
        }});
        self.req(self.c.put(format!(
            "{}/api/saved_objects/index-pattern/{id}",
            self.base
        )))
        .json(&body)
        .send()
        .await?
        .error_for_status()
        .context("writing index-pattern fields")?;
        Ok(())
    }
}

/// Documents in `title`, and how many carry a value for each of `fields`,
/// in one search.
async fn value_counts(
    deployment: &Deployment,
    c: &reqwest::Client,
    (u, p): (&str, &str),
    title: &str,
    fields: &[&String],
) -> Result<(u64, BTreeMap<String, u64>)> {
    let mut aggs = serde_json::Map::new();
    for (i, f) in fields.iter().enumerate() {
        aggs.insert(
            format!("f{i}"),
            serde_json::json!({ "filter": { "exists": { "field": f } } }),
        );
    }
    let base = crate::recipes::os_base(deployment);
    let r = c
        .post(format!("{base}/{title}/_search"))
        .basic_auth(u, Some(p))
        .json(&serde_json::json!({ "size": 0, "track_total_hits": true, "aggs": aggs }))
        .send()
        .await
        .context("counting documents")?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok((0, BTreeMap::new()));
    }
    let body: serde_json::Value = r.error_for_status()?.json().await?;
    let docs = body["hits"]["total"]["value"].as_u64().unwrap_or(0);
    let counts = fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let n = body["aggregations"][format!("f{i}")]["doc_count"]
                .as_u64()
                .unwrap_or(0);
            ((*f).clone(), n)
        })
        .collect();
    Ok((docs, counts))
}

/// Verify one installed integration and heal what the core owns: an index
/// pattern lacking a panel's field gets the live field list written onto it.
/// Idempotent; safe to run from a status poll.
pub async fn evaluate(
    deployment: &Deployment,
    pkg: &velox_integrations::Package,
    version: &str,
    selection: &velox_integrations::InputSelection,
) -> Result<IntegrationStatus> {
    let id = pkg.manifest.id.clone();
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let objs =
        crate::integrations::backend_saved_objects(deployment, pkg, version, selection, &u, &p)?;
    let panels = panel_fields(&objs);
    let conf = crate::integrations::collector_config(deployment, pkg, version, selection)?;
    let source = source_of(conf.as_deref());
    let c = crate::recipes::http()?;
    let dash = Dash {
        c: &c,
        base: crate::recipes::dashboards_base(deployment),
        tenant: crate::recipes::tenant_scope(deployment).await,
        u: &u,
        p: &p,
    };

    let mut checks = Vec::new();
    let mut docs = 0;
    let mut patterns_missing = Vec::new();
    let mut fields_unresolved = Vec::new();
    let mut fields_empty = Vec::new();
    let mut fields_wanted = Vec::new();
    for (pid, title) in index_patterns(&objs) {
        let wanted = panels.get(&pid).cloned().unwrap_or_default();
        fields_wanted.extend(wanted.keys().cloned());
        let Some(attrs) = dash.pattern(&pid).await? else {
            patterns_missing.push(pid.clone());
            continue;
        };
        let mut have = saved_field_names(&attrs);
        let lacking = have.is_empty() || wanted.keys().any(|f| !have.contains(f));
        if lacking {
            match dash.fields_for(&title).await {
                Ok(live) if !live.is_empty() => {
                    let names: BTreeSet<String> = live
                        .iter()
                        .filter_map(|f| f["name"].as_str().map(str::to_owned))
                        .collect();
                    if names != have {
                        dash.write_fields(&pid, &live).await?;
                        checks.push(Check {
                            name: "index_pattern_fields".into(),
                            ok: true,
                            detail: format!("refreshed {pid}: {} fields", names.len()),
                        });
                        have = names;
                    }
                }
                Ok(_) => {}
                Err(e) => checks.push(Check {
                    name: "index_pattern_fields".into(),
                    ok: false,
                    detail: format!("reading fields for {title}: {e:#}"),
                }),
            }
        }
        let wanted_names: Vec<&String> = wanted.keys().collect();
        let (n, counts) = value_counts(deployment, &c, (&u, &p), &title, &wanted_names).await?;
        docs += n;
        for (f, panels) in &wanted {
            let resolved = have.contains(f);
            let values = counts.get(f).copied().unwrap_or(0);
            if !resolved {
                fields_unresolved.push(f.clone());
            } else if n > 0 && values == 0 {
                fields_empty.push(f.clone());
            }
            checks.push(Check {
                name: format!("panel_field:{f}"),
                ok: resolved && values > 0,
                detail: format!(
                    "{} — {}, {values} documents with a value",
                    panels.join(", "),
                    if resolved {
                        "in the index pattern"
                    } else {
                        "NOT in the index pattern"
                    }
                ),
            });
        }
    }

    let collector = if matches!(source, Source::None) {
        None
    } else {
        crate::agents::collector_readiness(deployment, &id)
            .await
            .unwrap_or(None)
    };
    if let Some((desired, ready)) = collector {
        checks.push(Check {
            name: "collector".into(),
            ok: desired > 0 && ready == desired,
            detail: format!("{ready}/{desired} collector pods ready"),
        });
    }
    // Blockers explain an empty index; with documents arriving they are moot.
    let blockers = if docs == 0 {
        preflight_package(deployment, pkg, version, selection)
            .await
            .map(|p| p.findings.into_iter().filter(|f| f.blocker).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let state = decide(&Measured {
        id: &id,
        source: &source,
        docs,
        patterns_missing,
        fields_unresolved,
        fields_empty,
        fields_wanted,
        collector,
        blockers,
    });
    Ok(IntegrationStatus {
        id,
        state,
        docs,
        checks,
    })
}

/// [`evaluate`] for what the cluster has installed under `id`; `None` when
/// it is not installed there.
pub async fn status(deployment: &Deployment, id: &str) -> Result<Option<IntegrationStatus>> {
    let Some((pkg, version, selection)) = crate::catalog::installed_package(deployment, id).await?
    else {
        return Ok(None);
    };
    evaluate(deployment, &pkg, &version, &selection)
        .await
        .map(Some)
}

/// Deployments × ids being followed right now, so a re-install does not stack
/// a second follower on the first.
static FOLLOWING: std::sync::Mutex<BTreeSet<String>> = std::sync::Mutex::new(BTreeSet::new());

/// After an install: evaluate every [`FOLLOW_EVERY`] until the integration
/// works, for at most [`FOLLOW_FOR`]. This is what refreshes an index
/// pattern's fields when the first documents land, with nobody watching.
pub fn follow_install(
    deployment: &Deployment,
    pkg: &velox_integrations::Package,
    version: &str,
    selection: &velox_integrations::InputSelection,
) {
    let key = format!("{deployment}/{}", pkg.manifest.id);
    if !FOLLOWING
        .lock()
        .map(|mut s| s.insert(key.clone()))
        .unwrap_or(false)
    {
        return;
    }
    let (dep, pkg, version, selection) = (
        deployment.clone(),
        pkg.clone(),
        version.to_string(),
        selection.clone(),
    );
    tokio::spawn(async move {
        let until = std::time::Instant::now() + FOLLOW_FOR;
        let mut last = None;
        while std::time::Instant::now() < until {
            match evaluate(&dep, &pkg, &version, &selection).await {
                Ok(s) if s.state == State::Working => {
                    tracing::info!("integration {} on {dep}: working ({} docs)", s.id, s.docs);
                    last = None;
                    break;
                }
                Ok(s) => last = Some(s.state),
                Err(e) => tracing::debug!("integration {key}: evaluate failed: {e:#}"),
            }
            tokio::time::sleep(FOLLOW_EVERY).await;
        }
        if let Some(state) = last {
            tracing::warn!("integration {key}: not working after the install window: {state:?}");
        }
        if let Ok(mut s) = FOLLOWING.lock() {
            s.remove(&key);
        }
    });
}

/// Once per start: evaluate every installed integration of every deployment,
/// which heals the index patterns an earlier version saved without fields.
pub async fn reconcile_on_startup() {
    let deployments = match crate::k8s::scoped_deployments(&crate::scope::Scope::Admin).await {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!("integration reconcile skipped: {e:#}");
            return;
        }
    };
    for dep in deployments {
        let mut ids: BTreeSet<String> = crate::k8s::integration_versions(&dep)
            .await
            .unwrap_or_default()
            .into_keys()
            .collect();
        ids.extend(crate::k8s::monitors_of(&dep).await);
        for id in ids {
            match status(&dep, &id).await {
                Ok(Some(s)) => tracing::info!("integration {id} on {dep}: {:?}", s.state),
                Ok(None) => {}
                Err(e) => tracing::warn!("integration {id} on {dep}: reconcile failed: {e:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_is_read_off_the_rendered_config() {
        for r in crate::recipes::RECIPES {
            let conf = crate::agents::fluent_bit_conf_template(r);
            let s = source_of(Some(&conf));
            match *r {
                "k8s-events" => assert_eq!(s, Source::ApiEvents),
                "ssh" | "k8s-audit" => assert!(matches!(s, Source::HostFiles(_)), "{r}: {s:?}"),
                _ => assert!(matches!(s, Source::ContainerLogs(_)), "{r}: {s:?}"),
            }
        }
        assert_eq!(source_of(None), Source::None);
        let audit = source_of(Some(&crate::agents::fluent_bit_conf_template("k8s-audit")));
        assert_eq!(audit, Source::HostFiles(vec![AUDIT_LOG.to_string()]));
    }

    #[test]
    fn glob_needles() {
        assert_eq!(
            glob_needle("/var/log/containers/*nginx*.log"),
            Some("nginx")
        );
        assert_eq!(glob_needle("/var/log/containers/*.log"), None);
    }

    /// Every built-in dashboard's field panels are found, keyed by the
    /// pattern they read — the k8s-audit set is the one seen broken.
    #[test]
    fn panel_fields_cover_the_audit_dashboard() {
        let pkg = crate::recipes::bootstrap_package("k8s-audit").unwrap();
        let p = panel_fields(&pkg.saved_objects);
        let fields: BTreeSet<&str> = p["k8s-audit-logs"].keys().map(String::as_str).collect();
        for f in [
            "@timestamp",
            "verb",
            "user.username",
            "objectRef.resource",
            "sourceIPs",
            "responseStatus.code",
        ] {
            assert!(fields.contains(f), "{f} missing from {fields:?}");
        }
        assert_eq!(
            index_patterns(&pkg.saved_objects),
            vec![("k8s-audit-logs".into(), "{index}*".into())]
        );
        for r in crate::recipes::RECIPES {
            let pkg = crate::recipes::bootstrap_package(r).unwrap();
            assert!(
                !panel_fields(&pkg.saved_objects).is_empty(),
                "{r} has no field panels"
            );
        }
    }

    #[test]
    fn audit_metric_parse() {
        let m =
            "# HELP x\napiserver_audit_event_total 0\napiserver_audit_requests_rejected_total 0\n";
        assert_eq!(audit_events(m), Some(0.0));
        assert_eq!(
            audit_events("apiserver_audit_event_total 1.234e+06\n"),
            Some(1_234_000.0)
        );
        assert_eq!(audit_events("foo 1\n"), None);
    }

    fn measured<'a>(source: &'a Source) -> Measured<'a> {
        Measured {
            id: "k8s-audit",
            source,
            docs: 0,
            patterns_missing: vec![],
            fields_unresolved: vec![],
            fields_empty: vec![],
            fields_wanted: [
                "@timestamp",
                "verb",
                "user.username",
                "objectRef.resource",
                "sourceIPs",
                "responseStatus.code",
            ]
            .map(String::from)
            .to_vec(),
            collector: Some((3, 3)),
            blockers: vec![],
        }
    }

    #[test]
    fn no_docs_with_a_blocker_is_blocked_not_waiting() {
        let src = Source::HostFiles(vec![AUDIT_LOG.into()]);
        let mut m = measured(&src);
        assert!(matches!(decide(&m), State::WaitingForData { .. }));
        m.blockers = vec![no_control_plane_kubelet()];
        match decide(&m) {
            State::Blocked { reason, .. } => assert!(reason.contains("kubelet")),
            s => panic!("{s:?}"),
        }
        // Documents arriving make the blocker moot.
        m.docs = 10;
        assert_eq!(decide(&m), State::Working);
    }

    #[test]
    fn unparsed_or_unresolved_fields_block() {
        let src = Source::ContainerLogs(vec!["/var/log/containers/*nginx*.log".into()]);
        let mut m = measured(&src);
        m.docs = 5;
        m.fields_unresolved = vec!["verb".into()];
        assert!(matches!(decide(&m), State::Blocked { .. }));
        m.fields_unresolved.clear();
        m.fields_empty = m.fields_wanted[1..].to_vec();
        assert!(
            matches!(decide(&m), State::Blocked { .. }),
            "all but @timestamp empty"
        );
        m.fields_empty.truncate(1);
        assert_eq!(
            decide(&m),
            State::Working,
            "one empty panel is not a broken dashboard"
        );
    }

    #[test]
    fn collector_absent_or_unscheduled_blocks() {
        let src = Source::ContainerLogs(vec!["/var/log/containers/*redis*.log".into()]);
        let mut m = measured(&src);
        m.collector = None;
        assert!(matches!(decide(&m), State::Blocked { .. }));
        m.collector = Some((0, 0));
        assert!(matches!(decide(&m), State::Blocked { .. }));
        m.patterns_missing = vec!["p".into()];
        m.collector = Some((1, 1));
        assert!(matches!(decide(&m), State::Blocked { .. }));
    }

    #[test]
    fn status_serializes_with_a_state_tag() {
        let s = IntegrationStatus {
            id: "nginx".into(),
            state: State::WaitingForData {
                expected: "e".into(),
                source: "s".into(),
            },
            docs: 0,
            checks: vec![],
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["state"], "waiting_for_data");
        assert_eq!(v["expected"], "e");
    }
}
