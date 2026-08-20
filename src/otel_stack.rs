// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Server-only OpenTelemetry observability stack (ADR-053).
//!
//! The **second** collection option, alongside — never replacing — the Fluent
//! Bit recipes in `agents.rs`/`recipes.rs`. Those ship container logs; this
//! ships OTLP traces and metrics, which is what lights up the Trace Analytics,
//! service-map, Agent Traces and Metric-analytics screens that OpenSearch
//! Dashboards already bundles but that nothing in the product currently feeds.
//!
//! ## What this installs, and what it deliberately does not
//!
//! It installs **data producers only**. `observabilityDashboards` and
//! `queryWorkbenchDashboards` ship in every non-minimal OpenSearch Dashboards
//! distribution, and our CR names no image override — so every deployment we
//! provision *already* has the screens. They are simply empty.
//!
//! Modelled on `opensearch-project/observability-stack`, but the manifests are
//! generated here rather than vendored from its Helm chart, because that chart:
//!   * deploys its **own** OpenSearch + Dashboards (we target the operator's),
//!   * hardcodes `{{ .Release.Name }}-opensearch-dashboards:5601` in its init
//!     Jobs — the very subchart we must disable to point at our cluster,
//!   * runs `pip install requests pyyaml` at container start (dead in an
//!     air-gapped install), and
//!   * is upstream-Alpha, pinning a Data Prepper image from a personal Docker
//!     Hub account (`sgguruda62324/…:2.16.0-SNAPSHOT-rc1`). We pin the official
//!     GA build of that same version instead.
//!
//! ## Shape
//!
//! Two halves, like `integrations.rs`: everything above `mod exec` is pure and
//! unit-testable, with `manifests()` as the **single** object inventory —
//! install applies it in order, uninstall deletes it in reverse. There is no
//! second list, which is what makes the ADR-039 clean-install ⇒ clean-uninstall
//! property hold by construction rather than by review.

use crate::agents::AGENT_NS;
use crate::scope::Deployment;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Pinned images
// ---------------------------------------------------------------------------

/// Bumped when the rendered objects change shape in a way a re-install must
/// converge on. Recorded on the CR so the UI can show "update available" later.
pub const STACK_VERSION: &str = "2";

/// Data Prepper's own span index template.
///
/// Named here because it is the one object of Data Prepper's that a version
/// change of ours can invalidate: the sink installs a template per `index_type`
/// and **will not replace one that already matches its pattern**, so a
/// deployment carrying the template from the pre-rev.4 `trace-analytics-raw`
/// type keeps it, and every document the `trace-analytics-plain-raw` pipeline
/// writes is rejected with
/// `class_cast_exception: KeywordFieldMapper cannot be cast to ObjectMapper`.
/// Silently: the pod stays `Running` and only logs a WARN.
pub const SPAN_TEMPLATE: &str = "otel-v1-apm-span-index-template";

/// Write alias the span indices sit behind.
pub const SPAN_ALIAS: &str = "otel-v1-apm-span";

/// Whether an install has to clear Data Prepper's span template first.
///
/// Only when replacing a stack that predates the documented pipeline. A first
/// install has nothing stale — and deleting the template it is about to need
/// would leave new indices on dynamic mapping.
pub fn needs_span_migration(installed_version: Option<&str>) -> bool {
    matches!(installed_version, Some(v) if v != STACK_VERSION)
}

/// Upstream chart's pin (0.156.0). `-contrib` is required: the plain collector
/// image carries neither the `prometheusremotewrite` exporter nor the
/// `prometheus` receiver this config uses.
pub const OTEL_IMAGE: &str = "otel/opentelemetry-collector-contrib:0.156.0";

/// The **official** OpenSearch build. The upstream chart pins
/// `sgguruda62324/opensearch-data-prepper:2.16.0-SNAPSHOT-rc1` — a personal
/// account holding the release candidate of this very version. 2.16.0 shipped
/// GA on 2026-07-02; that is what we run.
pub const DP_IMAGE: &str = "opensearchproject/data-prepper:2.16.0";

pub const CORTEX_IMAGE: &str = "cortexproject/cortex:v1.18.1";
pub const AM_IMAGE: &str = "prom/alertmanager:v0.27.0";
/// Despite the name this is the OpenSearch exporter too (same wire API); it is
/// what the upstream "OpenSearch Cluster Health" dashboard's PromQL reads.
pub const OSEXP_IMAGE: &str = "prometheuscommunity/elasticsearch-exporter:v1.10.0";

// ---------------------------------------------------------------------------
// Index patterns
// ---------------------------------------------------------------------------

/// Written by Data Prepper's `opensearch` sink under `index_type:
/// trace-analytics-raw` (`IndexType::TRACE_ANALYTICS_RAW` → alias
/// `otel-v1-apm-span`). The sink installs the mapping itself, so this module
/// authors no index template for it.
pub const SPAN_PATTERN: &str = "otel-v1-apm-span*";

/// `index_type: otel-v2-apm-service-map`, the documented pipeline's output.
///
/// **v2, matching upstream** — and correcting an earlier note in this file
/// which claimed picking v2 "would render an empty service map". That was true
/// only of the plugin's *default* setting: `observability:traceAnalyticsServiceIndices`
/// is a UI setting (default `otel-v1-apm-service-map*`), and the APM screens
/// read whichever index pattern the `APM-Config` correlation points at. So the
/// version is a choice, not a constraint — and v1 costs the RED metrics, which
/// only the `otel_apm_service_map` processor emits.
pub const SERVICE_MAP_PATTERN: &str = "otel-v2-apm-service-map*";

/// The service map's **physical** index, as opposed to the alias above.
///
/// Data Prepper's `otel-v2-apm-service-map` sink writes to a concrete index
/// named `otel-v1-apm-service-map` and exposes it under the *alias*
/// `otel-v2-apm-service-map`. `_resolve/index/otel-v2-apm-service-map*`
/// therefore returns zero indices and one alias — so a `DELETE` on
/// `SERVICE_MAP_PATTERN` matches no index and silently deletes nothing, which
/// is how uninstall-with-delete-indices left the service map data behind while
/// reporting success. Reads keep using the alias (upstream's name, and what the
/// index pattern and ISM template are written against); only the delete needs
/// the physical name.
pub const SERVICE_MAP_INDEX: &str = "otel-v1-apm-service-map*";

/// Data Prepper's log sink (`index_type: log-analytics-plain`).
pub const LOGS_PATTERN: &str = "logs-otel-v1*";

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// `velox-otel-{deployment}-{part}` — the family sibling of
/// `agents::agent_name`, in the same namespace, so one labelled `kubectl get`
/// shows the whole stack and nothing else.
pub fn obj_name(deployment: &str, part: &str) -> String {
    format!("velox-otel-{deployment}-{part}")
}

/// The five component parts, in dependency order. Public because the API layer
/// reports readiness per component and the UI renders one chip each.
/// Data Prepper's single OTLP ingest port. The documented pipeline uses one
/// `otlp` source and routes by event type, so the separate logs listener that
/// the classic three-source shape needed (21892) no longer exists.
pub const DP_OTLP_PORT: u16 = 21890;

pub const COMPONENTS: [&str; 5] = [
    "cortex",
    "alertmanager",
    "os-exporter",
    "data-prepper",
    "collector",
];

/// SQL-plugin datasource name — **the upstream name, verbatim**.
///
/// Not per-deployment, and that is safe rather than sloppy: every deployment
/// gets its own OpenSearch **and its own Dashboards**, so there is no shared
/// namespace for these to collide in. Using the documented name is what makes
/// the upstream boards, saved queries and any future chart asset resolve
/// without rewriting their references.
pub const DATASOURCE_NAME: &str = "ObservabilityStack_Prometheus";

/// Kept as a function because most call sites read per-deployment; the value is
/// deliberately constant.
pub fn datasource_name(_deployment: &str) -> String {
    DATASOURCE_NAME.to_string()
}

/// Retention policies, by the ids the documented install uses.
///
/// Three, not one: Data Prepper **creates its own rollover-only policies at
/// startup** under exactly these ids (they are compiled into the sink jar), so
/// writing one policy of our own with a competing `ism_template` would leave
/// two policies racing for the same indices. Overriding them by id is what the
/// upstream init does, and it is the only version that converges.
pub const ISM_POLICIES: [(&str, &str); 3] = [
    ("raw-span-policy", SPAN_PATTERN),
    ("logs-policy", LOGS_PATTERN),
    ("otel-v2-apm-service-map-policy", SERVICE_MAP_PATTERN),
];

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// Prometheus scrape targets that live outside this stack. `None` means the job
/// is **omitted entirely** rather than emitted pointing at nothing.
///
/// The upstream chart hardcodes `kube-state-metrics.kube-system…:8080` and
/// `node-exporter-…kube-system…:9100`. Neither is a safe default here: on the
/// Tornis K3S the kube-prometheus-stack lives in `monitoring`, and a generic
/// customer install may have neither. A collector that logs scrape failures
/// forever is worse than one that does not scrape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScrapeTargets {
    #[serde(default)]
    pub kube_state_metrics: Option<String>,
    #[serde(default)]
    pub node_exporter: Option<String>,
}

/// One object in the inventory: enough to apply it and to delete it.
#[derive(Clone, Debug, PartialEq)]
pub struct K8sObject {
    pub group: &'static str,
    pub version: &'static str,
    pub kind: &'static str,
    pub namespace: &'static str,
    pub name: String,
    pub manifest: serde_json::Value,
}

/// `(group, kind, namespace, name)` — the identity used to prove install and
/// teardown cover the same set.
pub type ObjectKey = (&'static str, &'static str, &'static str, String);

impl K8sObject {
    pub fn key(&self) -> ObjectKey {
        (self.group, self.kind, self.namespace, self.name.clone())
    }
}

/// Advertised resource cost, in the units the UI renders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StackCost {
    pub cpu_millis: u64,
    pub mem_mib: u64,
    pub disk_gib: u64,
}

/// Per-component requests/limits, kept in one table so `resource_cost()` and
/// the manifests cannot drift (a unit test asserts they agree).
struct Res {
    cpu_req: &'static str,
    mem_req: &'static str,
    cpu_lim: &'static str,
    mem_lim: &'static str,
    cpu_req_millis: u64,
    mem_req_mib: u64,
}

const R_CORTEX: Res = Res {
    cpu_req: "100m",
    mem_req: "512Mi",
    cpu_lim: "500m",
    mem_lim: "1Gi",
    cpu_req_millis: 100,
    mem_req_mib: 512,
};
const R_AM: Res = Res {
    cpu_req: "50m",
    mem_req: "64Mi",
    cpu_lim: "200m",
    mem_lim: "128Mi",
    cpu_req_millis: 50,
    mem_req_mib: 64,
};
const R_OSEXP: Res = Res {
    cpu_req: "50m",
    mem_req: "64Mi",
    cpu_lim: "200m",
    mem_lim: "128Mi",
    cpu_req_millis: 50,
    mem_req_mib: 64,
};
// Data Prepper is the memory-hungry one, and measurably so: with the documented
// six-pipeline topology it sits at ~1.4 GiB **idle**, because `otel_traces`
// holds spans for its 180s flush interval and `otel_apm_service_map` keeps a
// window. At the 2 GiB upstream ceiling it was OOM-killed (exit 137) on this
// cluster, so the limit is 3 GiB here — a deliberate departure, taken from a
// measurement rather than from the chart.
const R_DP: Res = Res {
    cpu_req: "500m",
    mem_req: "1536Mi",
    cpu_lim: "1",
    mem_lim: "3Gi",
    cpu_req_millis: 500,
    mem_req_mib: 1536,
};
const R_OTEL: Res = Res {
    cpu_req: "200m",
    mem_req: "512Mi",
    cpu_lim: "1",
    mem_lim: "1Gi",
    cpu_req_millis: 200,
    mem_req_mib: 512,
};

/// Cortex TSDB blocks + Alertmanager silences/notification state.
const CORTEX_DISK_GIB: u64 = 20;
const AM_DISK_GIB: u64 = 2;

/// What the UI must state before the user clicks Install. Derived from the same
/// constants the manifests use.
pub fn resource_cost() -> StackCost {
    let all = [&R_CORTEX, &R_AM, &R_OSEXP, &R_DP, &R_OTEL];
    StackCost {
        cpu_millis: all.iter().map(|r| r.cpu_req_millis).sum(),
        mem_mib: all.iter().map(|r| r.mem_req_mib).sum(),
        disk_gib: CORTEX_DISK_GIB + AM_DISK_GIB,
    }
}

// ---------------------------------------------------------------------------
// Rendered configs
// ---------------------------------------------------------------------------

/// In-cluster service DNS for one of this stack's components.
fn svc(deployment: &str, part: &str) -> String {
    format!("{}.{AGENT_NS}.svc", obj_name(deployment, part))
}

/// OTel Collector config.
///
/// TLS posture, stated rather than inherited: the hop to Data Prepper is
/// plaintext gRPC and stays inside `velox-agents`, exactly like the Fluent Bit
/// agents' hop to 9200 today. Data Prepper's OTLP port is never exposed beyond
/// the namespace — the collector is the only ingest surface.
pub fn collector_config(deployment: &str, targets: &ScrapeTargets, htpasswd: &str) -> String {
    let dp = svc(deployment, "data-prepper");
    let cortex = svc(deployment, "cortex");
    let osexp = svc(deployment, "os-exporter");

    let mut jobs = String::new();
    jobs.push_str(
        "          - job_name: otel-collector\n            static_configs:\n              - targets: ['127.0.0.1:8888']\n",
    );
    jobs.push_str(&format!(
        "          - job_name: opensearch\n            static_configs:\n              - targets: ['{osexp}:9114']\n"
    ));
    // Data Prepper and Cortex self-metrics. Not optional decoration: every
    // panel on the Observability Pipeline Health board is a PromQL query over
    // `*_pipeline_*` (Data Prepper) or `cortex_*` series, so without these two
    // jobs that board renders empty no matter what the pipeline is doing.
    // Data Prepper serves its Micrometer registry at /metrics/prometheus, not
    // the default /metrics.
    jobs.push_str(&format!(
        "          - job_name: data-prepper\n            metrics_path: /metrics/prometheus\n            static_configs:\n              - targets: ['{dp}:4900']\n"
    ));
    jobs.push_str(&format!(
        "          - job_name: cortex\n            static_configs:\n              - targets: ['{cortex}:9090']\n"
    ));
    if let Some(t) = &targets.kube_state_metrics {
        jobs.push_str(&format!(
            "          - job_name: kube-state-metrics\n            static_configs:\n              - targets: ['{t}']\n"
        ));
    }
    if let Some(t) = &targets.node_exporter {
        jobs.push_str(&format!(
            "          - job_name: node-exporter\n            static_configs:\n              - targets: ['{t}']\n"
        ));
    }

    format!(
        "extensions:
  basicauth/otlp:
    htpasswd:
      inline: |
        {htpasswd}

receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317
        auth:
          authenticator: basicauth/otlp
      http:
        endpoint: 0.0.0.0:4318
        auth:
          authenticator: basicauth/otlp
  prometheus:
    config:
      scrape_configs:
{jobs}
processors:
  batch:
    timeout: 5s
    send_batch_size: 1024
  memory_limiter:
    check_interval: 1s
    limit_percentage: 80
    spike_limit_percentage: 20

exporters:
  otlp/dataprepper:
    endpoint: {dp}:21890
    tls:
      insecure: true
  prometheusremotewrite/cortex:
    endpoint: http://{cortex}:9090/api/v1/push
    tls:
      insecure: true

service:
  extensions: [basicauth/otlp]
  telemetry:
    metrics:
      # address: was removed in collector 0.15x and now fails config parsing
      # (migration.MetricsConfigV030 has invalid keys: address). The
      # replacement is an explicit OpenTelemetry-Go pull reader.
      readers:
        - pull:
            exporter:
              prometheus:
                host: 0.0.0.0
                port: 8888
  pipelines:
    traces:
      receivers: [otlp]
      processors: [memory_limiter, batch]
      exporters: [otlp/dataprepper]
    logs:
      receivers: [otlp]
      processors: [memory_limiter, batch]
      exporters: [otlp/dataprepper]
    metrics:
      receivers: [otlp, prometheus]
      processors: [memory_limiter, batch]
      exporters: [prometheusremotewrite/cortex]
"
    )
}

/// Data Prepper pipelines — the documented topology, verbatim except for our
/// host and credentials.
///
/// Deliberately upstream's shape rather than a simplified one, and the reason
/// is concrete: the `otel_apm_service_map` processor emits **two** event kinds,
/// `SERVICE_MAP` and `METRIC`, and the second is the RED metrics (rate, errors,
/// duration per service) that the APM screens read from Prometheus. The older
/// `service_map` processor emits only the first, so a stack built on it has an
/// APM page that lists services and can never fill in their latency or error
/// rate. That is what an earlier version of this module shipped.
///
/// **One deliberate departure from the documented topology, and fidelity is the
/// reason for it.** Upstream fans `otel-traces-pipeline` out to both
/// `traces-raw-pipeline` and `service-map-pipeline`, so the service map reads
/// the RAW OTLP stream. On the official 2.16.0 GA image that stream has no
/// flattened `serviceName` field — only `resource.attributes` — and
/// `OTelApmServiceMapProcessor.processSpan` is:
///
/// ```java
/// if (span.getServiceName() != null) { ... }   // no else: silent drop
/// ```
///
/// So **every** span is discarded, with no log and no metric, and `recordsIn` is
/// counted before the drop — which is why it reads as healthy. `serviceName` is
/// derived by the `otel_traces` processor, which lives on the other branch.
/// Verified by writing the raw event to a scratch index and reading its fields.
///
/// Chaining the service map behind `traces-raw-pipeline` gives it the enriched
/// spans. Measured on the live cluster: `recordsOut` 0 → 168, service-map edges
/// with both operations, and 27 series each of `request`/`error`/`fault`/
/// `latency_seconds_*` in Prometheus. Upstream's own chart pins a Data Prepper
/// snapshot RC rather than this GA, which is consistent with the raw stream
/// carrying `serviceName` there.
///
/// Two details are upstream's and are load-bearing:
///  * `delete_entries` strips the per-event `randomKey` UUID before the
///    Prometheus sink, or Cortex rejects the series for cardinality.
///  * `group_by_attributes: [telemetry.sdk.language]` must stay — the processor
///    groups by it, and dropping it collides multi-language services onto the
///    same (series, timestamp) tuple.
///
/// `insecure: true` on the sink is required, not lazy: the operator issues an
/// internal CA that nothing in this codebase currently trusts —
/// `recipes::http()` sets `danger_accept_invalid_certs(true)` for the same
/// reason. Pinning that CA is follow-up work.
///
/// **Contains the deployment's OpenSearch password**, so the caller must place
/// this in a Secret, never a ConfigMap. `password_only_in_secret` enforces it.
pub fn data_prepper_pipelines(dep: &Deployment, user: &str, password: &str) -> String {
    let deployment = dep.name();
    let host = crate::recipes::os_base(dep);
    let cortex = svc(deployment, "cortex");
    format!(
        "otlp-pipeline:
  delay: 10
  source:
    otlp:
      port: {DP_OTLP_PORT}
      ssl: false
  route:
    - logs: 'getEventType() == \"LOG\"'
    - traces: 'getEventType() == \"TRACE\"'
  sink:
    - pipeline:
        name: \"otel-logs-pipeline\"
        routes: [\"logs\"]
    - pipeline:
        name: \"otel-traces-pipeline\"
        routes: [\"traces\"]

otel-logs-pipeline:
  workers: 5
  delay: 10
  source:
    pipeline:
      name: \"otlp-pipeline\"
  buffer:
    bounded_blocking: {{}}
  processor:
    - copy_values:
        entries:
          - from_key: \"time\"
            to_key: \"@timestamp\"
  sink:
    - opensearch:
        hosts: [\"{host}\"]
        username: \"{user}\"
        password: \"{password}\"
        insecure: true
        index_type: log-analytics-plain

otel-traces-pipeline:
  delay: 100
  source:
    pipeline:
      name: \"otlp-pipeline\"
  sink:
    - pipeline:
        name: \"traces-raw-pipeline\"

traces-raw-pipeline:
  source:
    pipeline:
      name: \"otel-traces-pipeline\"
  processor:
    - otel_traces:
        trace_flush_interval: 180
  sink:
    - opensearch:
        hosts: [\"{host}\"]
        username: \"{user}\"
        password: \"{password}\"
        insecure: true
        index_type: trace-analytics-plain-raw
    - pipeline:
        name: \"service-map-pipeline\"

service-map-pipeline:
  delay: 100
  source:
    pipeline:
      name: \"traces-raw-pipeline\"
  processor:
    - otel_apm_service_map:
        group_by_attributes: [telemetry.sdk.language]
        window_duration: 10s
  route:
    - otel_apm_service_map_route: 'getEventType() == \"SERVICE_MAP\"'
    - service_processed_metrics: 'getEventType() == \"METRIC\"'
  sink:
    - opensearch:
        hosts: [\"{host}\"]
        username: \"{user}\"
        password: \"{password}\"
        insecure: true
        index_type: otel-v2-apm-service-map
        routes: [otel_apm_service_map_route]
    - pipeline:
        name: \"service-metrics-cortex-pipeline\"
        routes: [service_processed_metrics]

service-metrics-cortex-pipeline:
  delay: 100
  source:
    pipeline:
      name: \"service-map-pipeline\"
  processor:
    - delete_entries:
        with_keys:
          - \"/attributes/randomKey\"
          - \"randomKey\"
  sink:
    - prometheus:
        url: \"http://{cortex}:9090/api/v1/push\"
        insecure: true
        threshold:
          max_events: 500
          flush_interval: 5s
"
    )
}

/// Data Prepper's own server config (not a pipeline).
///
/// The `experimental` block is required, not optional hardening: in 2.16.0 both
/// `otel_apm_service_map` and the `prometheus` sink are **experimental
/// plugins**, and Data Prepper refuses to construct a pipeline that names one
/// without it — `Unable to create experimental plugin prometheus. You must
/// enable experimental plugins in data-prepper-config.yaml`. It then exits with
/// "No valid pipeline is available for execution", so the whole process
/// crash-loops rather than degrading. Copied from the documented install.
///
/// SSL off: these listeners are reachable only inside the namespace, and the
/// collector is the one ingest surface.
pub fn data_prepper_config() -> String {
    "ssl: false
peer_forwarder:
  ssl: false
experimental:
  enabled_plugins:
    processor:
      - otel_apm_service_map
    sink:
      - prometheus
"
    .to_string()
}

/// Cortex, single-binary. `auth_enabled: false` and no TLS — which is exactly
/// why the inventory ships a NetworkPolicy: anything that can reach :9090 reads
/// this deployment's metrics and can write its ruler.
pub fn cortex_config(deployment: &str) -> String {
    let am = svc(deployment, "alertmanager");
    format!(
        "target: all
auth_enabled: false

server:
  http_listen_port: 9090
  grpc_listen_port: 9095
  log_level: warn

distributor:
  shard_by_all_labels: true
  ring:
    kvstore:
      store: inmemory

ingester:
  lifecycler:
    ring:
      kvstore:
        store: inmemory
      replication_factor: 1

blocks_storage:
  backend: filesystem
  filesystem:
    dir: /data/blocks
  tsdb:
    dir: /data/tsdb
    # Go duration: no day unit exists, so 15d is rejected at startup.
    retention_period: 360h
  bucket_store:
    sync_dir: /data/tsdb-sync

compactor:
  data_dir: /data/compactor
  sharding_ring:
    kvstore:
      store: inmemory

ruler:
  enable_api: true
  alertmanager_url: http://{am}:9093
  # Scratch space Cortex downloads rule files into, not durable state — /tmp
  # exists and is writable, whereas a fresh path under /data does not exist on
  # first boot and Cortex logs it at ERROR every start.
  rule_path: /tmp/ruler

# Top-level, not nested under ruler: ruler.storage was removed and a nested
# block fails startup with: field storage not found in type ruler.Config
ruler_storage:
  backend: filesystem
  filesystem:
    dir: /data/ruler-storage

limits:
  ingestion_rate: 100000
  ingestion_burst_size: 200000
  max_global_series_per_user: 5000000
  max_global_series_per_metric: 500000
  max_label_names_per_series: 50
  compactor_blocks_retention_period: 360h
"
    )
}

/// Alertmanager with a null receiver: routing targets are the user's to add,
/// and inventing a default destination would be a surprise, not a feature.
pub fn alertmanager_config() -> String {
    "route:
  receiver: 'null'
  group_by: ['alertname']
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 12h
receivers:
  - name: 'null'
"
    .to_string()
}

/// Username every published endpoint authenticates as. One account per
/// deployment, not per person: this is a machine credential handed to an
/// exporter, and the panel shows it next to the URL.
pub const OTLP_USER: &str = "velox";

/// One published signal: the hostname, and the OTLP path it is allowed to carry.
pub const SIGNALS: [(&str, &str); 3] = [
    ("logs", "/v1/logs"),
    ("metrics", "/v1/metrics"),
    ("traces", "/v1/traces"),
];

/// Where the published routes live, when the platform is in ingress mode.
///
/// **Only the OTel Collector is published.** That is the architecture the
/// upstream diagram states: agents and services speak OTLP to the collector,
/// and everything behind it — Data Prepper, Prometheus/Cortex, Alertmanager,
/// the OpenSearch API — is reachable only inside the cluster. OpenSearch
/// Dashboards is the other external surface and already has its own Ingress
/// from the deployment itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IngressAccess {
    /// `{deployment}` — hosts are derived per signal.
    pub deployment: String,
    pub base_domain: String,
    pub class: String,
    /// Name of a `kubernetes.io/tls` Secret **in `velox-agents`**. Empty means
    /// the ingress controller's default certificate.
    pub tls_secret: String,
}

impl IngressAccess {
    /// Host for one signal: `<deployment>-<signal>.<base_domain>`.
    pub fn host(&self, signal: &str) -> String {
        format!("{}-{signal}.{}", self.deployment, self.base_domain)
    }
}

/// Everything the inventory needs to know about how this stack is reachable.
///
/// The stack needs both forms of the credential: the plaintext goes into the
/// Secret so the panel can show the user what to configure, and the bcrypt hash
/// goes into the two config files that verify it. Both are Secret-only — the
/// `password_only_in_secret` test holds them to it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EndpointAccess {
    pub password: String,
    pub password_hash: String,
    pub ingress: Option<IngressAccess>,
}

impl IngressAccess {
    pub fn for_deployment(deployment: &str, base_domain: &str, class: &str, tls: &str) -> Self {
        Self {
            deployment: deployment.to_string(),
            base_domain: base_domain.to_string(),
            class: class.to_string(),
            tls_secret: tls.to_string(),
        }
    }
}

/// Alertmanager's web config — the only authentication it has.
///
/// Alertmanager itself speaks the Prometheus `web.config.file` format and can
/// enforce basic auth without a proxy in front. That matters because the route
/// is published: an unauthenticated Alertmanager on the internet lets anyone
/// silence this deployment's alerts, and putting the check in the component
/// rather than in an ingress annotation means it holds no matter which ingress
/// controller (or none) is in front of it.
pub fn alertmanager_web_config(htpasswd_hash: &str) -> String {
    format!(
        "basic_auth_users:
  {OTLP_USER}: {htpasswd_hash}
"
    )
}

/// `user:bcrypt` line for the collector's inline htpasswd.
pub fn htpasswd_line(hash: &str) -> String {
    format!("{OTLP_USER}:{hash}")
}

// ---------------------------------------------------------------------------
// OpenSearch-side objects
// ---------------------------------------------------------------------------

/// Body for `POST {os}/_plugins/_query/_datasources`.
///
/// No `masterkey` prerequisite: verified live on OpenSearch 3.8.0 that
/// create/read/delete all succeed with no `plugins.query.datasources.
/// encryption.masterkey` in `opensearch.yml`. Encryption only gates datasources
/// that carry credentials, and Cortex runs `auth_enabled: false`. That is what
/// keeps this feature off the `additionalConfig` write path, which would roll
/// every node (ADR-048 discipline).
pub fn datasource_body(deployment: &str) -> serde_json::Value {
    let cortex = svc(deployment, "cortex");
    let am = svc(deployment, "alertmanager");
    serde_json::json!({
        "name": datasource_name(deployment),
        "connector": "prometheus",
        "allowedRoles": ["all_access"],
        "properties": {
            // Cortex serves the Prometheus query API under /prometheus while
            // its ruler admin API lives at the unprefixed root; the SQL plugin
            // has a separate property for each, and both are needed for
            // query + rule management to work.
            "prometheus.uri": format!("http://{cortex}:9090/prometheus"),
            "prometheus.ruler.uri": format!("http://{cortex}:9090"),
            "alertmanager.uri": format!("http://{am}:9093")
        }
    })
}

/// One ISM policy whose `ism_template` auto-attaches to all three otel patterns.
///
/// Must be PUT **before** Data Prepper starts: `ism_template` only attaches to
/// indices created after the policy exists (the ordering rule `profiles.rs`
/// documents), and the sink creates its indices within seconds of connecting.
pub fn ism_policy(policy_id: &str, pattern: &str, retention_days: u32) -> serde_json::Value {
    serde_json::json!({
        "policy": {
            "policy_id": policy_id,
            "description": format!("OTel telemetry retention ({retention_days}d)"),
            "default_state": "hot",
            "states": [
                { "name": "hot", "actions": [],
                  "transitions": [ { "state_name": "delete",
                      "conditions": { "min_index_age": format!("{retention_days}d") } } ] },
                { "name": "delete", "actions": [ { "delete": {} } ], "transitions": [] }
            ],
            "ism_template": [
                { "index_patterns": [pattern], "priority": 100 }
            ]
        }
    })
}

/// One index pattern as the Observability plugin wants it.
///
/// `signal_type` and `schema_mappings` are the fields that make the difference
/// between a pattern Discover can open and one the APM / Trace-Analytics
/// screens will actually adopt: the plugin filters datasets by `signalType` and
/// reads `schemaMappings` to find the trace/span/service fields. A bare
/// `title` + `timeFieldName` pattern — all `recipes::ensure_index_pattern` can
/// express — is invisible to them, which is why these are created here.
pub struct PatternSpec {
    pub id: &'static str,
    pub title: &'static str,
    /// None of the three is `@timestamp`.
    pub time_field: &'static str,
    pub signal_type: Option<&'static str>,
    pub schema_mappings: Option<&'static str>,
    pub display_name: Option<&'static str>,
}

/// The three index patterns, with the attributes the Observability plugin reads.
pub fn index_patterns() -> [PatternSpec; 3] {
    [
        PatternSpec {
            id: "velox-otel-spans",
            title: SPAN_PATTERN,
            time_field: "endTime",
            signal_type: Some("traces"),
            schema_mappings: None,
            display_name: Some("Trace Dataset - Local Cluster"),
        },
        PatternSpec {
            id: "velox-otel-service-map",
            title: SERVICE_MAP_PATTERN,
            // `timestamp`, matching upstream's init script — NOT `hashId`,
            // which this shipped as. `hashId` belongs to the legacy
            // `service_map_stateful` document shape; `otel_apm_service_map`
            // writes `sourceNode`/`targetNode`/`timestamp` and no `hashId` at
            // all, so the field the pattern pointed at does not exist on a
            // single document this stack produces.
            time_field: "timestamp",
            signal_type: None,
            schema_mappings: None,
            display_name: None,
        },
        PatternSpec {
            id: "velox-otel-logs",
            title: LOGS_PATTERN,
            time_field: "time",
            signal_type: Some("logs"),
            // Field names as Data Prepper's `log-analytics-plain` sink writes
            // them; the plugin uses this map to jump from a log line to its
            // trace.
            schema_mappings: Some(
                r#"{"otelLogs":{"timestamp":"time","traceId":"traceId","spanId":"spanId","serviceName":"resource.attributes.service.name"}}"#,
            ),
            display_name: Some("Log Dataset - Local Cluster"),
        },
    ]
}

/// Saved-object id of the trace-to-logs correlation.
pub fn trace_to_logs_id(_deployment: &str) -> String {
    "velox-otel-trace-to-logs".to_string()
}

/// Saved-object id of the APM config correlation.
pub fn apm_config_id(_deployment: &str) -> String {
    "velox-otel-apm-config".to_string()
}

/// The `correlations` saved objects the Observability plugin reads.
///
/// Not decoration and not derivable from anything else: OSD 3.8.0's APM client
/// looks up a correlation whose `correlationType` starts with `APM-Config-` and
/// takes the traces dataset, the service-map dataset and the Prometheus
/// connection from its three references. Absent it, the APM services and
/// service-map screens have nothing to read, which is exactly what an install
/// without them looks like from the UI: present in the menu, permanently empty.
///
/// `data_connection_id` is the OSD saved-object id of the Prometheus
/// connection, which only exists once the datasource is registered *through
/// Dashboards* (see `register_datasource`).
pub fn correlation_objects(
    deployment: &str,
    data_connection_id: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut out = vec![serde_json::json!({
        "id": trace_to_logs_id(deployment),
        "type": "correlations",
        "attributes": {
            "correlationType": format!("trace-to-logs-{SPAN_PATTERN}"),
            "title": format!("trace-to-logs_{SPAN_PATTERN}"),
            "version": "1.0.0",
            "entities": [
                { "tracesDataset": { "id": "references[0].id" } },
                { "logsDataset": { "id": "references[1].id" } }
            ]
        },
        "references": [
            { "name": "entities[0].index", "type": "index-pattern", "id": "velox-otel-spans" },
            { "name": "entities[1].index", "type": "index-pattern", "id": "velox-otel-logs" }
        ]
    })];
    if let Some(dc) = data_connection_id {
        out.push(serde_json::json!({
            "id": apm_config_id(deployment),
            "type": "correlations",
            "attributes": {
                "correlationType": "APM-Config-default",
                "title": "apm-config",
                "version": "1.0.0",
                "entities": [
                    { "tracesDataset": { "id": "references[0].id" } },
                    { "serviceMapDataset": { "id": "references[1].id" } },
                    { "prometheusDataSource": { "id": "references[2].id" } },
                    { "windowDuration": 60 }
                ]
            },
            "references": [
                { "name": "entities[0].index", "type": "index-pattern", "id": "velox-otel-spans" },
                { "name": "entities[1].index", "type": "index-pattern", "id": "velox-otel-service-map" },
                { "name": "entities[2].dataConnection", "type": "data-connection", "id": dc }
            ]
        }));
    }
    out
}

/// One PromQL panel of a self-monitoring board.
struct Panel {
    id: &'static str,
    title: &'static str,
    query: &'static str,
}

/// The three self-monitoring boards, as `(dashboard id suffix, title,
/// description, panels)`.
///
/// PromQL rather than index queries, so every panel is a read against the
/// deployment's own Cortex through the registered datasource. The queries are
/// the upstream chart's, **retargeted at our metric names**: Data Prepper
/// derives its metric prefixes from the pipeline names, and ours are
/// `entry-pipeline` / `raw-trace-pipeline` / `service-map-pipeline` /
/// `otel-logs-pipeline`, so upstream's `otel_traces_pipeline_*` series simply
/// do not exist here. Names verified against a live Data Prepper 2.16.0
/// `/metrics/prometheus`.
fn boards() -> Vec<(&'static str, &'static str, &'static str, Vec<Panel>)> {
    vec![
        (
            "opensearch-cluster-health-dashboard",
            "OpenSearch Cluster Health",
            "Cluster status, shards, JVM and indexing performance",
            vec![
                Panel { id: "os-cluster-status", title: "Cluster status (1 = yellow or red)",
                    query: "elasticsearch_cluster_health_status{color=\"yellow\"} or elasticsearch_cluster_health_status{color=\"red\"}" },
                Panel { id: "os-active-shards", title: "Active shards",
                    query: "elasticsearch_cluster_health_active_shards" },
                Panel { id: "os-unassigned-shards", title: "Unassigned shards",
                    query: "elasticsearch_cluster_health_unassigned_shards" },
                Panel { id: "os-docs-count", title: "Total documents",
                    query: "sum(elasticsearch_indices_docs)" },
                Panel { id: "os-indexing-rate", title: "Indexing rate (docs/sec)",
                    query: "rate(elasticsearch_indices_indexing_index_total[5m])" },
                Panel { id: "os-store-size", title: "Store size (bytes)",
                    query: "sum(elasticsearch_indices_store_size_bytes_total)" },
                Panel { id: "os-jvm-heap", title: "JVM heap used %",
                    query: "100 * elasticsearch_jvm_memory_used_bytes{area=\"heap\"} / elasticsearch_jvm_memory_max_bytes{area=\"heap\"}" },
                Panel { id: "os-jvm-gc", title: "JVM GC collection rate (sec/sec)",
                    query: "rate(elasticsearch_jvm_gc_collection_seconds_sum_total[5m])" },
                Panel { id: "os-search-rate", title: "Search rate (queries/sec)",
                    query: "rate(elasticsearch_indices_search_query_total[5m])" },
                Panel { id: "os-cpu", title: "OpenSearch CPU %",
                    query: "elasticsearch_os_cpu_percent" },
                Panel { id: "os-threadpool-rejected", title: "Thread pool rejections/sec",
                    query: "sum(rate(elasticsearch_thread_pool_rejected_count_total[5m]))" },
                Panel { id: "os-search-latency", title: "Search latency (sec/query)",
                    query: "rate(elasticsearch_indices_search_query_time_seconds_total[5m])" },
            ],
        ),
        (
            "observability-pipeline-health-dashboard",
            "Observability Pipeline Health",
            "OTel Collector throughput, Data Prepper pipelines and Cortex ingestion",
            vec![
                Panel { id: "pl-spans-received", title: "Spans received/sec (collector)",
                    query: "rate(otelcol_receiver_accepted_spans_total[5m])" },
                Panel { id: "pl-spans-exported", title: "Spans exported/sec (collector)",
                    query: "rate(otelcol_exporter_sent_spans_total[5m])" },
                Panel { id: "pl-spans-failed", title: "Spans failed to send/sec",
                    query: "rate(otelcol_exporter_send_failed_spans_total[5m])" },
                Panel { id: "pl-metric-points", title: "Metric points received/sec",
                    query: "rate(otelcol_receiver_accepted_metric_points_total[5m])" },
                Panel { id: "pl-queue-size", title: "Collector exporter queue size",
                    query: "otelcol_exporter_queue_size" },
                Panel { id: "pl-collector-mem", title: "Collector memory RSS (bytes)",
                    query: "otelcol_process_memory_rss" },
                Panel { id: "pl-dp-traces-in", title: "Data Prepper OTLP trace requests/sec",
                    query: "rate(entry_pipeline_otel_trace_source_requestsReceived_total[5m])" },
                Panel { id: "pl-dp-logs-in", title: "Data Prepper OTLP log requests/sec",
                    query: "rate(otel_logs_pipeline_otel_logs_source_requestsReceived_total[5m])" },
                Panel { id: "pl-dp-spans-written", title: "Span documents written/sec",
                    query: "rate(raw_trace_pipeline_opensearch_documentsSuccess_total[5m])" },
                Panel { id: "pl-dp-logs-written", title: "Log documents written/sec",
                    query: "rate(otel_logs_pipeline_opensearch_documentsSuccess_total[5m])" },
                Panel { id: "pl-dp-servicemap-written", title: "Service-map documents written/sec",
                    query: "rate(service_map_pipeline_opensearch_documentsSuccess_total[5m])" },
                Panel { id: "pl-dp-bulk-errors", title: "Data Prepper bulk request errors/sec",
                    query: "sum(rate(raw_trace_pipeline_opensearch_bulkRequestErrors_total[5m])) + sum(rate(otel_logs_pipeline_opensearch_bulkRequestErrors_total[5m]))" },
                Panel { id: "pl-dp-latency", title: "Trace pipeline latency (avg sec)",
                    query: "rate(raw_trace_pipeline_opensearch_PipelineLatency_seconds_sum[5m]) / rate(raw_trace_pipeline_opensearch_PipelineLatency_seconds_count[5m])" },
                Panel { id: "pl-dp-buffer", title: "Buffer capacity used %",
                    query: "entry_pipeline_BlockingBuffer_capacityUsed + otel_logs_pipeline_BlockingBuffer_capacityUsed" },
                Panel { id: "pl-cortex-ingest", title: "Cortex ingestion rate (samples/sec)",
                    query: "avg(cortex_ingester_ingestion_rate_samples_per_second)" },
                Panel { id: "pl-cortex-series", title: "Cortex active time series",
                    query: "cortex_ingester_memory_series" },
            ],
        ),
        (
            "k8s-cluster-health-dashboard",
            "Kubernetes Cluster Health",
            "Node and workload metrics — populated only when kube-state-metrics or node-exporter were given as scrape targets",
            vec![
                Panel { id: "k8s-nodes-ready", title: "Nodes ready",
                    query: "sum(kube_node_status_condition{condition=\"Ready\",status=\"true\"})" },
                Panel { id: "k8s-pods-running", title: "Pods running",
                    query: "sum(kube_pod_status_phase{phase=\"Running\"})" },
                Panel { id: "k8s-pods-pending", title: "Pods pending",
                    query: "sum(kube_pod_status_phase{phase=\"Pending\"})" },
                Panel { id: "k8s-restarts", title: "Container restarts/sec",
                    query: "sum(rate(kube_pod_container_status_restarts_total[5m]))" },
                Panel { id: "k8s-cpu", title: "Node CPU used %",
                    query: "100 - (avg by (instance) (rate(node_cpu_seconds_total{mode=\"idle\"}[5m])) * 100)" },
                Panel { id: "k8s-mem", title: "Node memory available (bytes)",
                    query: "node_memory_MemAvailable_bytes" },
            ],
        ),
    ]
}

/// Shared `explore` visualization envelope: a line chart over a PromQL series.
fn explore_visualization() -> String {
    serde_json::json!({
        "title": "", "chartType": "line",
        "params": {
            "addLegend": true, "addTimeMarker": false, "legendPosition": "bottom",
            "legendTitle": "", "lineMode": "straight", "lineStyle": "line", "lineWidth": 2,
            "showFullTimeRange": false, "standardAxes": [],
            "thresholdOptions": { "baseColor": "#00BD6B", "thresholds": [], "thresholdStyle": "off" },
            "titleOptions": { "show": false, "titleName": "" },
            "tooltipOptions": { "mode": "all" }
        },
        "axesMapping": { "color": "Series", "x": "Time", "y": "Value" }
    })
    .to_string()
}

/// Saved objects for the three self-monitoring boards: one `explore` panel per
/// query plus one `dashboard` that lays them out two per row.
///
/// `explore` is a real saved-object type in OSD 3.8.0 but only once
/// `explore.enabled` is set — which is why `install` writes the Dashboards
/// config **before** this runs. Every id is namespaced by deployment so two
/// deployments on one cluster never collide.
pub fn dashboard_objects(deployment: &str) -> Vec<serde_json::Value> {
    let ds = datasource_name(deployment);
    let viz = explore_visualization();
    let dataset = serde_json::json!({
        "id": ds, "title": ds, "type": "PROMETHEUS", "language": "PROMQL",
        "timeFieldName": "Time", "dataSource": {}, "signalType": "metrics"
    });

    let mut out = Vec::new();
    for (suffix, title, description, panels) in boards() {
        let mut grid = Vec::new();
        let mut refs = Vec::new();
        for (i, p) in panels.iter().enumerate() {
            let pid = p.id.to_string();
            let search_source = serde_json::json!({
                "query": { "query": p.query, "language": "PROMQL", "dataset": dataset },
                "filter": [],
                "indexRefName": "kibanaSavedObjectMeta.searchSourceJSON.index"
            })
            .to_string();
            out.push(serde_json::json!({
                "id": pid,
                "type": "explore",
                "attributes": {
                    "title": p.title, "description": "", "hits": 0,
                    "columns": ["_source"], "sort": [], "version": 1, "type": "metrics",
                    "visualization": viz,
                    "uiState": r#"{"activeTab":"explore_visualization_tab"}"#,
                    "kibanaSavedObjectMeta": { "searchSourceJSON": search_source }
                },
                "references": [{
                    "name": "kibanaSavedObjectMeta.searchSourceJSON.index",
                    // A virtual reference: the datasource, not a persisted
                    // index-pattern saved object.
                    "type": "index-pattern", "id": ds
                }]
            }));
            grid.push(serde_json::json!({
                "version": "3.8.0", "panelIndex": pid,
                "gridData": { "i": pid, "x": (i % 2) * 24, "y": (i / 2) * 15, "w": 24, "h": 15 },
                "panelRefName": format!("panel_{i}")
            }));
            refs.push(serde_json::json!({
                "name": format!("panel_{i}"), "type": "explore", "id": pid
            }));
        }
        out.push(serde_json::json!({
            "id": suffix,
            "type": "dashboard",
            "attributes": {
                "title": title,
                "description": description,
                "panelsJSON": serde_json::Value::Array(grid).to_string(),
                "optionsJSON": r#"{"useMargins":true,"hidePanelTitles":false}"#,
                "timeRestore": false,
                "kibanaSavedObjectMeta": { "searchSourceJSON": "{}" }
            },
            "references": serde_json::Value::Array(refs)
        }));
    }
    out
}

/// Everything this stack creates on the OpenSearch side, in the shape
/// `integrations::teardown_os` consumes — so uninstall reuses the one teardown
/// path rather than growing a parallel one.
pub fn os_teardown(deployment: &str) -> crate::integrations::Teardown {
    let pats = index_patterns();
    crate::integrations::Teardown {
        // Data Prepper's sink owns its own templates and pipelines; we author
        // none, so there is none to remove.
        ingest_pipeline: None,
        index_template: None,
        index_pattern: pats[0].id.to_string(),
        index_patterns: pats[1..].iter().map(|p| p.id.to_string()).collect(),
        // Boards, their explore panels and both correlations. `correlation_objects`
        // is asked for the full set (a data-connection id it will not read) so
        // uninstall removes the APM config even when the datasource registration
        // never landed.
        saved_objects: os_saved_objects(deployment)
            .iter()
            .filter_map(|o| o.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect(),
        ism_policies: ISM_POLICIES.iter().map(|(id, _)| id.to_string()).collect(),
        datasources: vec![datasource_name(deployment)],
    }
}

/// Every saved object this stack owns, as `{id, type, ...}` values. One list,
/// so `install`, `os_teardown` and the set-equality test all read the same
/// inventory rather than three that can drift.
pub fn os_saved_objects(deployment: &str) -> Vec<serde_json::Value> {
    let mut out = dashboard_objects(deployment);
    out.extend(correlation_objects(deployment, Some("")));
    out
}

/// `(saved-object id, type)` for everything in `os_saved_objects`, the shape
/// `integrations::teardown_os` needs to delete each one at its own endpoint.
pub fn saved_object_kinds(deployment: &str) -> std::collections::BTreeMap<String, String> {
    os_saved_objects(deployment)
        .iter()
        .filter_map(|o| {
            Some((
                o.get("id")?.as_str()?.to_string(),
                o.get("type")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The inventory
// ---------------------------------------------------------------------------

fn labels(deployment: &str) -> serde_json::Value {
    serde_json::json!({
        "app.kubernetes.io/part-of": format!("velox-otel-{deployment}"),
        "app.kubernetes.io/managed-by": "veloxsearch",
        "veloxsearch.ai/target": deployment
    })
}

/// Hash a config into the pod template so a changed ConfigMap/Secret actually
/// rolls the pod — subPath mounts never update live. Same trick, and same
/// reason, as `agents::apply_agent_workload`.
fn config_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:x}", h.finish())
}

fn config_map(deployment: &str, part: &str, file: &str, body: &str) -> K8sObject {
    let name = obj_name(deployment, part);
    K8sObject {
        group: "",
        version: "v1",
        kind: "ConfigMap",
        namespace: AGENT_NS,
        manifest: serde_json::json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": { "name": name, "namespace": AGENT_NS, "labels": labels(deployment) },
            "data": { file: body }
        }),
        name,
    }
}

fn pvc(deployment: &str, part: &str, gib: u64) -> K8sObject {
    let name = obj_name(deployment, part);
    K8sObject {
        group: "",
        version: "v1",
        kind: "PersistentVolumeClaim",
        namespace: AGENT_NS,
        manifest: serde_json::json!({
            "apiVersion": "v1", "kind": "PersistentVolumeClaim",
            "metadata": { "name": name, "namespace": AGENT_NS, "labels": labels(deployment) },
            "spec": {
                // ADR-043: Longhorn is named explicitly rather than falling
                // through to whatever the cluster's default happens to be.
                "storageClassName": crate::bootstrap::LONGHORN_SC,
                "accessModes": ["ReadWriteOnce"],
                "resources": { "requests": { "storage": format!("{gib}Gi") } }
            }
        }),
        name,
    }
}

fn service(deployment: &str, part: &str, ports: &[(&str, u16)]) -> K8sObject {
    let name = obj_name(deployment, part);
    let ports: Vec<serde_json::Value> = ports
        .iter()
        .map(|(n, p)| serde_json::json!({ "name": n, "port": p, "targetPort": p }))
        .collect();
    K8sObject {
        group: "",
        version: "v1",
        kind: "Service",
        namespace: AGENT_NS,
        manifest: serde_json::json!({
            "apiVersion": "v1", "kind": "Service",
            "metadata": { "name": name, "namespace": AGENT_NS, "labels": labels(deployment) },
            "spec": { "selector": { "app": name }, "ports": ports }
        }),
        name,
    }
}

#[allow(clippy::too_many_arguments)]
/// Gate Data Prepper on OpenSearch actually answering.
///
/// Data Prepper's OpenSearch sink retries initialization a bounded number of
/// times and then **gives up for good**: the pipelines shut down, the OTLP
/// source is never bound, and the pod sits at `0/1` indefinitely with nothing
/// but a `Failed to initialize OpenSearch sink, retrying: … System error`
/// twenty lines up in its log. `wait_settled` before the apply is not enough —
/// the gap that matters is between the pod starting and its network being
/// programmed, which is measured in seconds and lands squarely inside the
/// sink's retry budget. Upstream ships the same init container for the same
/// reason, in the same words: "Without this, sink initialization fails and the
/// OTLP source never starts."
///
/// Reuses the Data Prepper image rather than pulling a curl image: one fewer
/// thing to pre-load on an air-gapped cluster (ADR-025), and it already ships
/// `curl`.
fn init_containers(dep: &Deployment, part: &str, image: &str) -> serde_json::Value {
    let deployment = dep.name();
    if part != "data-prepper" {
        return serde_json::Value::Null;
    }
    let host = crate::recipes::os_base(dep);
    let secret = obj_name(deployment, "dp");
    serde_json::json!([{
        "name": "wait-for-opensearch",
        "image": image,
        "command": ["sh", "-c",
            format!(
                "until curl -sk -u \"$ES_USERNAME:$ES_PASSWORD\" {host}/_cluster/health \
                 | grep -qE '\"status\":\"(green|yellow)\"'; do \
                 echo 'waiting for OpenSearch'; sleep 5; done"
            )],
        "env": [
            { "name": "ES_USERNAME", "valueFrom": { "secretKeyRef": { "name": secret, "key": "ES_USERNAME" } } },
            { "name": "ES_PASSWORD", "valueFrom": { "secretKeyRef": { "name": secret, "key": "ES_PASSWORD" } } }
        ],
        "resources": { "requests": { "cpu": "10m", "memory": "32Mi" },
                       "limits": { "cpu": "100m", "memory": "64Mi" } }
    }])
}

/// Readiness that means "will accept traffic", not "the process started".
///
/// Data Prepper binds its OTLP listener **well after** the container is up —
/// pipelines are constructed first — so with the default (no probe) Kubernetes
/// marks the pod Ready and the Service starts routing to a port that answers
/// `connection refused`. The collector then retries with a growing backoff and
/// the first traces after an install can take minutes to land, or be dropped.
/// Upstream overrides the probe for the same reason, in the same words:
/// "Prevents K8s from routing traffic before the OTLP listener is ready."
///
/// Only Data Prepper needs it — it is the one component with a slow bind that
/// something else depends on. `null` leaves the pod spec exactly as before.
fn readiness_probe(part: &str) -> serde_json::Value {
    if part != "data-prepper" {
        return serde_json::Value::Null;
    }
    // The budget is generous because the bind is genuinely slow: measured at
    // roughly two minutes on this cluster with the six-pipeline topology, and
    // the probe's own events are what showed it (20 consecutive
    // `connection refused` over 8 minutes across a restart). Too tight a
    // threshold turns a slow start into a crash loop.
    serde_json::json!({
        "tcpSocket": { "port": DP_OTLP_PORT },
        "initialDelaySeconds": 20,
        "periodSeconds": 5,
        "failureThreshold": 36
    })
}

fn deployment_obj(
    dep: &Deployment,
    part: &str,
    image: &str,
    res: &Res,
    args: serde_json::Value,
    env: serde_json::Value,
    ports: &[u16],
    mounts: serde_json::Value,
    volumes: serde_json::Value,
    hash: &str,
) -> K8sObject {
    let deployment = dep.name();
    let name = obj_name(deployment, part);
    let ports: Vec<serde_json::Value> = ports
        .iter()
        .map(|p| serde_json::json!({ "containerPort": p }))
        .collect();
    K8sObject {
        group: "apps",
        version: "v1",
        kind: "Deployment",
        namespace: AGENT_NS,
        manifest: serde_json::json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": { "name": name, "namespace": AGENT_NS, "labels": labels(deployment) },
            "spec": {
                "replicas": 1,
                // RWO volumes cannot be handed over while the old pod holds
                // them, so a rolling update would deadlock on itself.
                "strategy": { "type": "Recreate" },
                "selector": { "matchLabels": { "app": name } },
                "template": {
                    "metadata": {
                        "labels": { "app": name,
                                    "app.kubernetes.io/part-of": format!("velox-otel-{deployment}") },
                        "annotations": { "veloxsearch.ai/config-hash": hash }
                    },
                    "spec": {
                        "serviceAccountName": "velox-agent",
                        "initContainers": init_containers(dep, part, image),
                        "containers": [{
                            "name": part,
                            "image": image,
                            "args": args,
                            "env": env,
                            "ports": ports,
                            "volumeMounts": mounts,
                            "readinessProbe": readiness_probe(part),
                            "resources": {
                                "requests": { "cpu": res.cpu_req, "memory": res.mem_req },
                                "limits":   { "cpu": res.cpu_lim, "memory": res.mem_lim }
                            }
                        }],
                        "volumes": volumes
                    }
                }
            }
        }),
        name,
    }
}

/// The single inventory. Applied in order, deleted in reverse.
pub fn manifests(
    dep: &Deployment,
    os_user: &str,
    os_password: &str,
    targets: &ScrapeTargets,
    endpoint: &EndpointAccess,
) -> Vec<K8sObject> {
    let deployment = dep.name();
    let mut v = Vec::new();

    let cortex_cfg = cortex_config(deployment);
    let am_cfg = alertmanager_config();
    let htpasswd = htpasswd_line(&endpoint.password_hash);
    let otel_cfg = collector_config(deployment, targets, &htpasswd);
    let am_web = alertmanager_web_config(&endpoint.password_hash);
    let dp_pipelines = data_prepper_pipelines(dep, os_user, os_password);

    // ---- config ----
    v.push(config_map(deployment, "cortex", "cortex.yaml", &cortex_cfg));
    v.push(config_map(
        deployment,
        "alertmanager",
        "alertmanager.yml",
        &am_cfg,
    ));

    // Data Prepper's pipeline carries the OpenSearch password → Secret, never a
    // ConfigMap. (`agents.rs` puts its Fluent Bit password in a ConfigMap; that
    // is a wart this module deliberately does not inherit.)
    let dp_secret = obj_name(deployment, "dp");
    v.push(K8sObject {
        group: "",
        version: "v1",
        kind: "Secret",
        namespace: AGENT_NS,
        manifest: serde_json::json!({
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
            "metadata": { "name": dp_secret, "namespace": AGENT_NS, "labels": labels(deployment) },
            "stringData": {
                "pipelines.yaml": dp_pipelines,
                "data-prepper-config.yaml": data_prepper_config(),
                // The exporter authenticates the same way; same secret, so a
                // password rotation touches exactly one object.
                "ES_USERNAME": os_user,
                "ES_PASSWORD": os_password,
                // The collector config and the Alertmanager web config now
                // carry a bcrypt hash each, which makes them credentials — so
                // they live here rather than in a ConfigMap, under the rule
                // `password_only_in_secret` enforces. The object's name is
                // historical; it is the stack's single credential Secret.
                "otel-config.yaml": otel_cfg,
                "am-web.yml": am_web,
                // Plaintext, so the panel can show what to paste into an
                // exporter. Stored rather than regenerated, or every re-apply
                // would invalidate every configured client.
                "OTLP_USERNAME": OTLP_USER,
                "OTLP_PASSWORD": endpoint.password
            }
        }),
        name: dp_secret.clone(),
    });

    // ---- storage ----
    v.push(pvc(deployment, "cortex", CORTEX_DISK_GIB));
    v.push(pvc(deployment, "alertmanager", AM_DISK_GIB));

    // ---- cortex ----
    v.push(deployment_obj(
        dep, "cortex", CORTEX_IMAGE, &R_CORTEX,
        serde_json::json!(["-config.file=/etc/cortex/cortex.yaml"]),
        serde_json::json!([]),
        &[9090, 9095],
        serde_json::json!([
            { "name": "config", "mountPath": "/etc/cortex" },
            { "name": "data", "mountPath": "/data" }
        ]),
        serde_json::json!([
            { "name": "config", "configMap": { "name": obj_name(deployment, "cortex") } },
            { "name": "data", "persistentVolumeClaim": { "claimName": obj_name(deployment, "cortex") } }
        ]),
        &config_hash(&cortex_cfg),
    ));
    v.push(service(
        deployment,
        "cortex",
        &[("http", 9090), ("grpc", 9095)],
    ));

    // ---- alertmanager ----
    v.push(deployment_obj(
        dep, "alertmanager", AM_IMAGE, &R_AM,
        serde_json::json!([
            "--config.file=/etc/alertmanager/alertmanager.yml",
            "--web.config.file=/etc/alertmanager-web/am-web.yml",
            "--storage.path=/alertmanager"
        ]),
        serde_json::json!([]),
        &[9093],
        serde_json::json!([
            { "name": "config", "mountPath": "/etc/alertmanager" },
            { "name": "web", "mountPath": "/etc/alertmanager-web" },
            { "name": "data", "mountPath": "/alertmanager" }
        ]),
        serde_json::json!([
            { "name": "config", "configMap": { "name": obj_name(deployment, "alertmanager") } },
            { "name": "web", "secret": { "secretName": dp_secret, "items": [
                { "key": "am-web.yml", "path": "am-web.yml" } ] } },
            { "name": "data", "persistentVolumeClaim": { "claimName": obj_name(deployment, "alertmanager") } }
        ]),
        &config_hash(&format!("{am_cfg}{am_web}")),
    ));
    v.push(service(deployment, "alertmanager", &[("http", 9093)]));

    // ---- opensearch prometheus exporter ----
    v.push(deployment_obj(
        dep, "os-exporter", OSEXP_IMAGE, &R_OSEXP,
        serde_json::json!([
            format!("--es.uri={}", crate::recipes::os_base(dep)),
            "--es.ssl-skip-verify",
            "--es.all",
            "--es.indices"
        ]),
        serde_json::json!([
            { "name": "ES_USERNAME", "valueFrom": { "secretKeyRef": { "name": dp_secret, "key": "ES_USERNAME" } } },
            { "name": "ES_PASSWORD", "valueFrom": { "secretKeyRef": { "name": dp_secret, "key": "ES_PASSWORD" } } }
        ]),
        &[9114],
        serde_json::json!([]),
        serde_json::json!([]),
        &config_hash(os_password),
    ));
    v.push(service(deployment, "os-exporter", &[("http", 9114)]));

    // ---- data prepper ----
    v.push(deployment_obj(
        dep, "data-prepper", DP_IMAGE, &R_DP,
        serde_json::json!([]),
        serde_json::json!([]),
        &[21890, 2021, 4900],
        serde_json::json!([
            { "name": "pipelines", "mountPath": "/usr/share/data-prepper/pipelines/pipelines.yaml", "subPath": "pipelines.yaml" },
            { "name": "pipelines", "mountPath": "/usr/share/data-prepper/config/data-prepper-config.yaml", "subPath": "data-prepper-config.yaml" }
        ]),
        serde_json::json!([
            { "name": "pipelines", "secret": { "secretName": dp_secret } }
        ]),
        &config_hash(&dp_pipelines),
    ));
    v.push(service(
        deployment,
        "data-prepper",
        &[("otlp", 21890), ("http-source", 2021), ("metrics", 4900)],
    ));

    // ---- collector ----
    v.push(deployment_obj(
        dep,
        "collector",
        OTEL_IMAGE,
        &R_OTEL,
        serde_json::json!(["--config=/etc/otel/otel-config.yaml"]),
        serde_json::json!([]),
        &[4317, 4318, 8888],
        serde_json::json!([{ "name": "config", "mountPath": "/etc/otel" }]),
        serde_json::json!([
            { "name": "config", "secret": { "secretName": dp_secret, "items": [
                { "key": "otel-config.yaml", "path": "otel-config.yaml" } ] } }
        ]),
        &config_hash(&otel_cfg),
    ));
    v.push(service(
        deployment,
        "collector",
        &[("otlp-grpc", 4317), ("otlp-http", 4318), ("metrics", 8888)],
    ));

    // ---- network policy ----
    //
    // Cortex and Alertmanager have no authentication at all, so this is part of
    // the stack, not polish: without it any pod in the cluster reads this
    // deployment's metrics and can write its ruler. OTLP (4317/4318) stays open
    // cluster-wide — that is the ingest surface user workloads push to.
    let np = obj_name(deployment, "policy");
    // Open ports, in the sense of "reachable from any pod in the cluster".
    // Only OTLP, because the collector is the only published surface and the
    // ingress controller runs in its own namespace — anything not in this list
    // is reachable solely from the app namespace and the stack's own pods.
    // Cortex (9090), Alertmanager (9093) and the exporter (9114) all stay out:
    // the first and third have no authentication at all, and the second is no
    // longer published.
    let open_ports = vec![
        serde_json::json!({ "port": 4317, "protocol": "TCP" }),
        serde_json::json!({ "port": 4318, "protocol": "TCP" }),
    ];

    v.push(K8sObject {
        group: "networking.k8s.io",
        version: "v1",
        kind: "NetworkPolicy",
        namespace: AGENT_NS,
        manifest: serde_json::json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
            "metadata": { "name": np, "namespace": AGENT_NS, "labels": labels(deployment) },
            "spec": {
                "podSelector": { "matchLabels": {
                    "app.kubernetes.io/part-of": format!("velox-otel-{deployment}") } },
                "policyTypes": ["Ingress"],
                "ingress": [
                    { "ports": open_ports },
                    { "from": [
                        { "namespaceSelector": { "matchLabels": {
                            "kubernetes.io/metadata.name": crate::k8s::ns() } } },
                        { "podSelector": { "matchLabels": {
                            "app.kubernetes.io/part-of": format!("velox-otel-{deployment}") } } }
                      ] }
                ]
            }
        }),
        name: np,
    });

    // ---- published routes ----
    //
    // The stack is useless to an application that does not run in this cluster
    // unless its ingest endpoint is reachable, so OTLP and Alertmanager get
    // real hosts rather than a port-forward instruction. Both are authenticated
    // *by the components themselves* (the collector's basicauth extension, the
    // Alertmanager web config), not by an ingress annotation — so the check
    // survives a different ingress controller, and applies to in-cluster
    // traffic too.
    //
    // Cortex is deliberately NOT published: it has no authentication of any
    // kind, and its reader is Dashboards, which already has a route.
    if let Some(ing) = &endpoint.ingress {
        // One host per signal, each restricted to that signal's OTLP path — so
        // `…-logs` really only carries logs. Three hosts pointing at `/` would
        // all accept everything and the name would be decoration.
        for (signal, path) in SIGNALS {
            v.push(ingress_obj(
                deployment,
                signal,
                &ing.host(signal),
                path,
                ing,
            ));
        }
    }

    v
}

/// One published route.
fn ingress_obj(
    deployment: &str,
    part: &str,
    host: &str,
    path: &str,
    ing: &IngressAccess,
) -> K8sObject {
    let name = obj_name(deployment, part);
    let mut spec = serde_json::json!({
        "ingressClassName": ing.class,
        "rules": [{
            "host": host,
            "http": { "paths": [{
                "path": path, "pathType": "Exact",
                "backend": { "service": {
                    "name": obj_name(deployment, "collector"),
                    "port": { "number": 4318 } } }
            }]}
        }]
    });
    if !ing.tls_secret.is_empty() {
        spec["tls"] = serde_json::json!([{ "hosts": [host], "secretName": ing.tls_secret }]);
    }
    K8sObject {
        group: "networking.k8s.io",
        version: "v1",
        kind: "Ingress",
        namespace: AGENT_NS,
        manifest: serde_json::json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
            "metadata": { "name": name, "namespace": AGENT_NS, "labels": labels(deployment) },
            "spec": spec
        }),
        name,
    }
}

/// Every object key `manifests()` creates.
pub fn created_objects(dep: &Deployment) -> BTreeSet<ObjectKey> {
    manifests(
        dep,
        "u",
        "p",
        &ScrapeTargets::default(),
        &EndpointAccess::default(),
    )
    .iter()
    .map(|o| o.key())
    .collect()
}

// ---------------------------------------------------------------------------
// Executors
// ---------------------------------------------------------------------------

use anyhow::{bail, Context, Result};

/// Live readiness of one component.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ComponentState {
    pub name: String,
    pub ready: i32,
    pub desired: i32,
}

/// What the Integrations panel renders.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StackState {
    pub installed: bool,
    pub version: String,
    pub components: Vec<ComponentState>,
    pub datasource: bool,
    /// Self-monitoring boards live in OpenSearch Dashboards, written by the
    /// deferred phase. Surfaced because "installed" and "the screens are there"
    /// are minutes apart, and a panel that does not say so reads as broken.
    pub boards: u32,
    /// Path into the deployment's Observability workspace, relative to its
    /// Dashboards URL. Empty until the workspace exists.
    pub workspace_path: String,
    pub span_docs: u64,

    // ---- endpoints ----
    //
    // Every address a user needs, in one payload, because "where do I point my
    // exporter" and "where is my data" were the two questions the first version
    // of this panel could not answer. Public fields are empty when the platform
    // is in port-forward mode; the internal ones always hold.
    /// The three published OTLP endpoints, one host per signal, each restricted
    /// to its own path. Empty in port-forward mode.
    pub otlp_logs_url: String,
    pub otlp_metrics_url: String,
    pub otlp_traces_url: String,
    pub otlp_grpc: String,
    pub otlp_http: String,
    pub alertmanager_portforward: String,
    /// Username for both published endpoints. The password is behind its own
    /// reveal call, like the cluster admin credentials.
    pub otlp_user: String,
    /// OpenSearch itself, in-cluster (the API a client library would use).
    pub opensearch_url: String,
    /// OpenSearch Dashboards, as a browser can reach it.
    pub dashboards_url: String,
    /// Non-fatal detail. "Still starting" is never a 5xx.
    pub error: String,
}

/// Retention window for the otel indices, matching what the purpose profile
/// applies to the recipe indices (ADR-028): 90d for security, 30d otherwise.
fn retention_days(purpose: &str) -> u32 {
    if purpose == "security" {
        90
    } else {
        30
    }
}

/// Install the stack for one deployment.
///
/// **OpenSearch side first, then Kubernetes** — not cosmetic ordering: an ISM
/// `ism_template` only auto-attaches to indices created *after* the policy
/// exists (the rule `profiles.rs` documents), and Data Prepper's sink creates
/// its indices within seconds of connecting. Applying the workloads first would
/// leave the very first day of telemetry outside retention forever.
pub async fn install(dep: &Deployment, targets: ScrapeTargets) -> Result<()> {
    let deployment = dep.name();
    // 1. Gate. Data Prepper authenticates immediately and Cortex/Alertmanager
    //    want volumes, so a rolling cluster is refused by name rather than
    //    producing five crash-looping pods.
    crate::k8s::wait_settled(dep, 300)
        .await
        .context("deployment is not settled; wait for it to go green and retry")?;

    let status = crate::k8s::get_deployment(dep)
        .await
        .context("reading deployment status")?
        .ok_or_else(|| anyhow::anyhow!("no such deployment: {deployment}"))?;
    let (user, password) = crate::k8s::admin_creds(dep).await;
    if password.is_empty() {
        bail!("no admin credentials for {deployment}; cannot configure telemetry");
    }

    // 2. Capacity. Refusing with the numbers beats five Pending pods. Skipped
    //    when the cluster does not report requests (metrics-server absent):
    //    an unknown is not a refusal.
    if let Ok(cap) = crate::capacity::cluster_capacity().await {
        let cost = resource_cost();
        if let (Some(cpu_req), Some(mem_req)) = (cap.cpu.requested, cap.mem.requested) {
            let free_cpu = cap.cpu.total.saturating_sub(cpu_req);
            let free_mem_mib = cap.mem.total.saturating_sub(mem_req) / (1024 * 1024);
            if free_cpu < cost.cpu_millis || free_mem_mib < cost.mem_mib {
                bail!(
                    "not enough free capacity for the observability stack: needs {}m CPU / {} MiB, \
                     cluster has {free_cpu}m / {free_mem_mib} MiB unreserved",
                    cost.cpu_millis,
                    cost.mem_mib
                );
            }
        }
    }

    // 3. Migration, before anything writes. Replacing an older stack means Data
    //    Prepper's span template on this cluster was installed for a different
    //    `index_type`; it has to go while the old pod is still the one holding
    //    it, so the new pod installs the right one at startup.
    if needs_span_migration((!status.otel_stack.is_empty()).then_some(status.otel_stack.as_str())) {
        tracing::info!("otel stack: clearing the stale span template for {deployment}");
        clear_span_template(dep).await;
    }

    // 4. Retention first: an ISM `ism_template` only auto-attaches to indices
    //    created *after* the policy exists, and Data Prepper creates its
    //    indices seconds after it connects.
    crate::recipes::ensure_tenant(dep).await;
    ensure_ism(dep, retention_days(&status.purpose)).await?;

    // 5. Kubernetes side, in inventory order.
    let client = crate::k8s::client().await?;
    let endpoint = endpoint_access(deployment).await?;
    if let Some(ing) = &endpoint.ingress {
        if !ing.tls_secret.is_empty() {
            // Best-effort: a missing or unreadable certificate must not block
            // the install, it just means the routes fall back to the ingress
            // controller's default one.
            if let Err(e) = crate::k8s::copy_tls_secret(&client, &ing.tls_secret, AGENT_NS).await {
                tracing::warn!("otel stack: TLS certificate for {deployment} not copied: {e:#}");
            }
        }
    }
    for o in manifests(dep, &user, &password, &targets, &endpoint) {
        crate::k8s::apply_dynamic(
            &client,
            o.group,
            o.version,
            o.kind,
            Some(o.namespace),
            &o.name,
            &o.manifest,
        )
        .await
        .with_context(|| format!("applying {} {}", o.kind, o.name))?;
    }

    // 6. Turn on the Dashboards features the stack's screens are made of, and
    //    roll the pod so the new config is read. Everything after this point —
    //    the `explore` panels, the `data-connection` the APM correlation
    //    references — is rejected by an OSD still running the default config,
    //    which is exactly what an install without this step looked like: menus
    //    present, screens permanently empty.
    //
    //    Reverted on failure rather than left behind: an unrecognised key is a
    //    fatal boot error, and a deployment whose UI never comes back is a far
    //    worse outcome than a stack without its boards.
    let has_dashboards = crate::k8s::has_dashboards(dep).await;
    // Carried into the deferred task: the export has to happen here, before
    // multi-tenancy is switched off, but the workspace it lands in does not
    // exist until the Dashboards pod has rolled.
    let mut pending_import: Option<String> = None;
    if has_dashboards {
        crate::k8s::set_dashboards_otel_config(dep, true)
            .await
            .context("enabling the observability features in OpenSearch Dashboards")?;
        // 6a. The stack cannot work without workspaces — the Observability nav
        //     group, and with it Agent Monitoring and Application performance,
        //     only renders inside one. So it turns the next-gen UI on, but as
        //     the *stack's* requirement (`chosen: false`), which is what lets
        //     uninstall put it back if the user never asked for it themselves.
        //     Already on and chosen stays chosen: `set_next_ui` only clears the
        //     annotation when disabling.
        if !crate::k8s::next_ui_state(dep).await.0 {
            crate::k8s::set_next_ui(dep, true, false)
                .await
                .context("enabling the next-generation UI the observability stack requires")?;
        }
        // 6b. The cluster half of the same decision. Both halves or the two
        //     sides disagree; see `set_multitenancy`. Counted BEFORE the flip,
        //     because afterwards the tenant indices are still there but nothing
        //     resolves them — the number would read as zero and the migration
        //     would never be offered.
        let tenant_objects = tenant_saved_objects(dep).await;
        //     Export while the tenant is still resolvable. Conditional on
        //     there being anything: the common case is a deployment whose
        //     saved objects were always Global, and that must cost nothing.
        pending_import = match tenant_objects {
            Some(0) => None,
            Some(n) => {
                tracing::info!(
                    "otel stack: {deployment} has {n} saved objects in tenant \
                     indices; exporting them for the Observability workspace"
                );
                export_tenant_objects(dep).await
            }
            None => {
                tracing::warn!(
                    "otel stack: could not read the tenant saved-object count for \
                     {deployment} — treating as unknown, not as zero; nothing is \
                     migrated and the tenant indices are left untouched"
                );
                None
            }
        };
        set_multitenancy(dep, false).await;
    }

    // 7. Record only after every apply succeeded — a partial install must never
    //    read as installed.
    crate::k8s::set_otel_stack(dep, Some(STACK_VERSION)).await?;

    // 8. Everything with a minutes-long wait in it is deferred, so the request
    //    returns now: the Dashboards pod has to roll before its new features
    //    exist, Cortex has to answer before OpenSearch will accept the
    //    datasource (it validates the connector on registration), and image
    //    pulls are minutes. Same shape as the ADR-018 deferred apply.
    //
    //    Order inside the task is forced: the boards are `explore` saved
    //    objects, a type that does not exist until the config lands, and every
    //    panel plus the APM correlation references the datasource.
    let d = dep.clone();
    let migrating =
        needs_span_migration((!status.otel_stack.is_empty()).then_some(status.otel_stack.as_str()));
    tokio::spawn(async move {
        if has_dashboards {
            if let Err(e) = wait_dashboards_features(&d, 600).await {
                // Revert rather than leave a deployment carrying config its UI
                // could not boot with.
                tracing::error!("otel stack: dashboards features for {d}: {e:#}");
                let _ = crate::k8s::set_dashboards_otel_config(&d, false).await;
                return;
            }
        }
        if migrating {
            // The alias's write index still carries the old mapping, so the
            // fresh template only takes effect on the NEXT index. Rolling over
            // gets one without deleting anything a user might still want to
            // read.
            if let Err(e) = wait_available(&d, "data-prepper", 900).await {
                tracing::error!("otel stack: data-prepper never became available for {d}: {e:#}");
            } else {
                roll_span_alias(&d).await;
            }
        }
        if let Err(e) = wait_available(&d, "cortex", 900).await {
            tracing::error!("otel stack: cortex never became available for {d}: {e:#}");
            return;
        }
        let dc = match register_datasource(&d).await {
            Ok(id) => {
                tracing::info!("otel stack: datasource registered for {d}");
                Some(id)
            }
            Err(e) => {
                tracing::error!("otel stack: datasource registration for {d}: {e:#}");
                None
            }
        };
        // The workspace has to exist before anything is written, because the
        // saved objects are addressed *through* it — written outside, they are
        // invisible from inside, which is the whole reason the Observability
        // screens looked bare.
        let ws = if has_dashboards {
            match ensure_workspace(&d).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!("otel stack: observability workspace for {d}: {e:#}");
                    String::new()
                }
            }
        } else {
            String::new()
        };
        if !ws.is_empty() {
            if let Some(id) = &dc {
                associate_datasource(&d, &ws, id).await;
            }
            // Bring the tenant's saved objects across, if the pre-flip export
            // found any. Before `ensure_index_patterns`, so that a pattern this
            // stack owns wins over an imported one of the same id rather than
            // being overwritten by it.
            if let Some(ndjson) = pending_import {
                match import_objects(&d, &ws, &ndjson).await {
                    Ok(()) => tracing::info!(
                        "otel stack: migrated the tenant's saved objects into the \
                         Observability workspace for {d}"
                    ),
                    // Non-fatal on purpose: the export is a copy and the tenant
                    // indices are untouched, so a failure here loses nothing —
                    // re-enabling multi-tenancy brings the originals back.
                    Err(e) => tracing::error!(
                        "otel stack: migrating the tenant's saved objects for {d}: {e:#} \
                         — the originals are still in the tenant indices"
                    ),
                }
            }
        }
        ensure_index_patterns(&d, &ws).await;
        ensure_dashboards(&d, &ws, dc.as_deref()).await;
        if !ws.is_empty() {
            apply_ui_settings(&d, &ws).await;
        }
        tracing::info!("otel stack: dashboards wired for {d} (workspace {ws:?})");
    });

    Ok(())
}

/// Remove everything `install` created.
///
/// Deletes `manifests()` **in reverse**: the delete list *is* the install list,
/// so there is no second inventory that could drift out of sync (the property
/// `install_set_equals_teardown_set` guards).
///
/// Deliberately left behind, and said so in the UI: the telemetry indices
/// themselves and the data on the deleted PVs — the same contract
/// `recipes::disable` documents for log data.
pub async fn uninstall(dep: &Deployment, delete_indices: bool) -> Result<()> {
    let deployment = dep.name();
    let client = crate::k8s::client().await?;
    // Credentials are irrelevant to a delete; the manifests are only walked for
    // their identity, so placeholders keep the password out of this path.
    // The published routes are deleted by name whatever the access config says
    // now, because an admin who switched the platform to port-forward mode
    // between install and uninstall would otherwise leave two Ingresses behind
    // — the one case where the inventory at uninstall time is not the one that
    // was installed. Best-effort and idempotent, like every delete here.
    let endpoint = EndpointAccess {
        ingress: Some(IngressAccess::default()),
        ..Default::default()
    };
    for o in manifests(dep, "", "", &ScrapeTargets::default(), &endpoint)
        .iter()
        .rev()
    {
        crate::k8s::delete_dynamic(
            &client,
            o.group,
            o.version,
            o.kind,
            Some(o.namespace),
            &o.name,
        )
        .await;
    }

    // The Dashboards half of the datasource, before the engine half goes: its
    // id is a UUID Dashboards mints, so it cannot live in the static teardown
    // list and has to be looked up. Leaving it behind is what made a
    // re-install find a saved object with no engine entry.
    if let Some(id) = data_connection_id(dep).await {
        let dash = crate::recipes::dashboards_base(dep);
        if let Ok(c) = crate::recipes::http() {
            let (u, p) = crate::k8s::admin_creds(dep).await;
            let _ = c
                .delete(format!(
                    "{dash}/api/saved_objects/data-connection/{id}?force=true"
                ))
                .basic_auth(&u, Some(&p))
                .header("osd-xsrf", "true")
                .send()
                .await;
        }
    }

    // Delete the workspace before the objects: it takes its contents with it,
    // and the generic teardown below then only has the global-space leftovers
    // (index patterns from an older install) to sweep.
    remove_workspace(dep).await;

    let teardown = os_teardown(deployment);
    let kinds = saved_object_kinds(deployment);
    crate::integrations::teardown_os(dep, &teardown, &kinds).await;

    // Multi-tenancy goes back on: the stack is what required it off, so the
    // stack leaving is what restores it. Symmetric with install, and the reason
    // the recipe path can resume scoping by tenant afterwards —
    // `recipes::tenant_scope` re-reads the marker, which is cleared at the end
    // of this function. Objects migrated into the workspace stay there; this
    // only makes the tenant's originals resolvable again.
    // Only if the user never asked for the new UI themselves. One that did
    // keeps its workspaces — and therefore keeps `multitenancy_enabled: false`,
    // because that is a consequence of workspaces rather than of this stack.
    let (_, ui_chosen) = crate::k8s::next_ui_state(dep).await;
    if !ui_chosen {
        crate::k8s::set_next_ui(dep, false, false)
            .await
            .context("reverting the next-generation UI")?;
        set_multitenancy(dep, true).await;
    }

    // Put the Dashboards config back the way we found it and roll the pod, so a
    // deployment that no longer has the stack no longer carries its feature
    // flags either. Best-effort on the roll: the config is what matters, and
    // the operator's next reconcile picks it up regardless.
    crate::k8s::set_dashboards_otel_config(dep, false)
        .await
        .context("reverting the OpenSearch Dashboards observability features")?;

    // Optional, and destructive in a way nothing else here is: the telemetry
    // itself. Everything above removes machinery that can be rebuilt by
    // re-installing; this removes data, and the only way back is a snapshot
    // taken beforehand. Off by default, and the UI makes the user say it.
    if delete_indices {
        delete_telemetry_indices(dep).await;
    }

    crate::k8s::set_otel_stack(dep, None).await?;
    Ok(())
}

/// Delete the indices the stack wrote. Irreversible without a snapshot.
async fn delete_telemetry_indices(dep: &Deployment) {
    let deployment = dep.name();
    let base = crate::recipes::os_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    // `SERVICE_MAP_INDEX` as well as the alias pattern: deleting through the
    // alias alone is a no-op (see the constant), and deleting the alias pattern
    // is still worth attempting so the alias itself does not outlive its index.
    for pattern in [
        SPAN_PATTERN,
        SERVICE_MAP_PATTERN,
        SERVICE_MAP_INDEX,
        LOGS_PATTERN,
    ] {
        match c
            .delete(format!("{base}/{pattern}"))
            .basic_auth(&u, Some(&p))
            .send()
            .await
        {
            Ok(r) => tracing::info!(
                "otel stack: deleted {pattern} for {deployment} ({})",
                r.status()
            ),
            Err(e) => tracing::error!("otel stack: deleting {pattern} for {deployment}: {e}"),
        }
    }
}

/// Best-effort live state for the panel. A component still pulling its image is
/// `ready: 0`, never an error — the same rule `api::node_stats` follows.
pub async fn status(dep: &Deployment) -> Result<StackState> {
    let deployment = dep.name();
    let installed = crate::k8s::get_deployment(dep)
        .await
        .ok()
        .flatten()
        .map(|s| !s.otel_stack.is_empty())
        .unwrap_or(false);

    let access = crate::access::get().await.unwrap_or_default();
    let routes = access.ingress_enabled().then(|| {
        IngressAccess::for_deployment(deployment, &access.base_domain, &access.ingress_class, "")
    });

    let mut st = StackState {
        installed,
        version: if installed {
            STACK_VERSION.into()
        } else {
            String::new()
        },
        otlp_grpc: format!("{}:4317", svc(deployment, "collector")),
        otlp_http: format!("http://{}:4318", svc(deployment, "collector")),
        otlp_logs_url: routes
            .as_ref()
            .map(|r| format!("https://{}/v1/logs", r.host("logs")))
            .unwrap_or_default(),
        otlp_metrics_url: routes
            .as_ref()
            .map(|r| format!("https://{}/v1/metrics", r.host("metrics")))
            .unwrap_or_default(),
        otlp_traces_url: routes
            .as_ref()
            .map(|r| format!("https://{}/v1/traces", r.host("traces")))
            .unwrap_or_default(),
        alertmanager_portforward: format!(
            "kubectl -n {AGENT_NS} port-forward svc/{} 9093:9093",
            obj_name(deployment, "alertmanager")
        ),
        otlp_user: OTLP_USER.to_string(),
        // The PUBLIC route when there is one; the in-cluster Service otherwise.
        // The panel lists what a user outside the cluster can actually use.
        opensearch_url: access
            .opensearch_url(deployment)
            .unwrap_or_else(|| crate::recipes::os_base(dep)),
        dashboards_url: access.dashboard_url(deployment).unwrap_or_else(|| {
            crate::access::AccessConfig::portforward_cmd(dep.namespace(), deployment)
        }),
        ..Default::default()
    };
    if !installed {
        return Ok(st);
    }

    let client = crate::k8s::client().await?;
    for part in COMPONENTS {
        st.components
            .push(component_state(&client, deployment, part).await);
    }

    st.datasource = datasource_exists(dep).await;
    let ws = workspace_id(dep).await.unwrap_or_default();
    // Relative on purpose: the panel joins it to whatever Dashboards URL the
    // deployment actually has (ingress host or port-forward), which this layer
    // does not know.
    st.workspace_path = if ws.is_empty() {
        String::new()
    } else {
        // Lands on Alerting rather than the workspace's landing page: the
        // Alertmanager has no route of its own by design, so this link is the
        // way in to it — and the `/w/{id}` prefix still puts the user inside
        // the Observability workspace, where the rest of the nav lives.
        format!("/w/{ws}/app/observability-alerting")
    };
    st.boards = boards_present(dep, &ws).await;
    st.span_docs = crate::recipes::doc_count_of(dep, SPAN_PATTERN)
        .await
        .unwrap_or(0);
    Ok(st)
}

async fn component_state(client: &kube::Client, deployment: &str, part: &str) -> ComponentState {
    use k8s_openapi::api::apps::v1::Deployment as KDeployment;
    use kube::Api;
    let name = obj_name(deployment, part);
    let api: Api<KDeployment> = Api::namespaced(client.clone(), AGENT_NS);
    let (ready, desired) = match api.get_opt(&name).await {
        Ok(Some(d)) => {
            let s = d.status.unwrap_or_default();
            (
                s.available_replicas.unwrap_or(0),
                d.spec.and_then(|s| s.replicas).unwrap_or(1),
            )
        }
        _ => (0, 1),
    };
    ComponentState {
        name: part.to_string(),
        ready,
        desired,
    }
}

/// Poll a component's Deployment until it reports at least one available
/// replica. Shaped like `k8s::wait_settled`: bounded, and it names what it was
/// waiting on when it gives up.
pub async fn wait_available(dep: &Deployment, part: &str, secs: u64) -> Result<()> {
    let deployment = dep.name();
    let client = crate::k8s::client().await?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if component_state(&client, deployment, part).await.ready >= 1 {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "{} did not become available within {secs}s",
                obj_name(deployment, part)
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

/// PUT the ISM policy, handling the "already exists" case the way
/// `profiles::ensure_retention` does: a 409 means re-PUT with the current
/// `_seq_no`/`_primary_term`, not give up.
async fn ensure_ism(dep: &Deployment, days: u32) -> Result<()> {
    let base = crate::recipes::os_base(dep);
    let c = crate::recipes::http()?;
    let (u, p) = crate::k8s::admin_creds(dep).await;

    for (policy_id, pattern) in ISM_POLICIES {
        let url = format!("{base}/_plugins/_ism/policies/{policy_id}");
        let body = ism_policy(policy_id, pattern, days);

        let resp = c
            .put(&url)
            .basic_auth(&u, Some(&p))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("creating ISM policy {policy_id}"))?;
        if resp.status().is_success() {
            continue;
        }
        if resp.status() != reqwest::StatusCode::CONFLICT {
            bail!("ISM policy {policy_id} PUT returned {}", resp.status());
        }

        // Already there — Data Prepper writes its own rollover-only version of
        // these under the same ids at startup, so the conflicting case is the
        // normal one, not the exception. Re-PUT with the current sequence.
        let cur: serde_json::Value = c
            .get(&url)
            .basic_auth(&u, Some(&p))
            .send()
            .await
            .with_context(|| format!("reading ISM policy {policy_id}"))?
            .json()
            .await
            .with_context(|| format!("parsing ISM policy {policy_id}"))?;
        let seq = cur.get("_seq_no").and_then(|v| v.as_u64()).unwrap_or(0);
        let term = cur
            .get("_primary_term")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let updated = c
            .put(format!("{url}?if_seq_no={seq}&if_primary_term={term}"))
            .basic_auth(&u, Some(&p))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("updating ISM policy {policy_id}"))?;
        if !updated.status().is_success() {
            bail!(
                "ISM policy {policy_id} update returned {}",
                updated.status()
            );
        }
    }
    Ok(())
}

/// Wait until the *new* Dashboards config is actually live.
///
/// Not a pod-readiness check, and the difference matters: the operator hashes
/// the rendered `opensearch_dashboards.yml` into the pod template
/// (`checksum/dashboards.yml`) and rolls the pod itself, but it does so on its
/// own reconcile cadence. Until then the OLD pod is perfectly ready, so
/// `readyReplicas >= 1` returns true immediately and everything after this
/// point would be written against an OSD that still rejects the `explore` and
/// `data-connection` types.
///
/// So the probe asks Dashboards itself: `plugin:explore` appears in
/// `/api/status` only when `explore.enabled` is set. If OSD instead fails to
/// boot on a bad key, the old pod keeps answering without that plugin and this
/// times out — which is the signal the caller uses to revert.
async fn wait_dashboards_features(dep: &Deployment, secs: u64) -> Result<()> {
    let deployment = dep.name();
    let dash = crate::recipes::dashboards_base(dep);
    let c = crate::recipes::http()?;
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let live = async {
            let body: serde_json::Value = c
                .get(format!("{dash}/api/status"))
                .basic_auth(&u, Some(&p))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            let statuses = body.get("status")?.get("statuses")?.as_array()?;
            Some(
                statuses
                    .iter()
                    .filter_map(|s| s.get("id")?.as_str())
                    .any(|id| id.starts_with("plugin:explore@")),
            )
        }
        .await
        .unwrap_or(false);
        if live {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "OpenSearch Dashboards for {deployment} did not report the observability \
                 features within {secs}s"
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

/// Resolve the credential and the published routes for one deployment.
///
/// The credential is **read back from the Secret when it is already there**.
/// Regenerating it on every apply would silently invalidate every exporter
/// somebody had already configured — the endpoint is the product here, so its
/// password has to be stable across re-installs and password rotations.
async fn endpoint_access(deployment: &str) -> Result<EndpointAccess> {
    use k8s_openapi::api::core::v1::Secret;
    use kube::Api;

    let client = crate::k8s::client().await?;
    let secrets: Api<Secret> = Api::namespaced(client, AGENT_NS);
    let existing = secrets
        .get_opt(&obj_name(deployment, "dp"))
        .await
        .ok()
        .flatten()
        .and_then(|s| s.data)
        .and_then(|d| d.get("OTLP_PASSWORD").cloned())
        .and_then(|v| String::from_utf8(v.0).ok())
        .filter(|v| !v.is_empty());

    let password = match existing {
        Some(p) => p,
        None => crate::k8s::gen_token().context("generating the telemetry credential")?,
    };
    let password_hash = crate::k8s::bcrypt_hash(&password)?;

    let access = crate::access::get().await.unwrap_or_default();
    let ingress = access.ingress_enabled().then(|| {
        IngressAccess::for_deployment(
            deployment,
            &access.base_domain,
            &access.ingress_class,
            // The Ingress lives in `velox-agents`, and a Secret reference is
            // namespace-local — `install` copies the certificate across.
            access.tls_secret.trim(),
        )
    });

    Ok(EndpointAccess {
        password,
        password_hash,
        ingress,
    })
}

/// Name of the Dashboards workspace this stack owns — **the documented one**.
///
/// Not per-deployment: every deployment has its own Dashboards, so there is
/// exactly one of these per environment and no collision to avoid. Using the
/// upstream name keeps any chart-authored asset that references it resolvable.
///
/// OpenSearch Dashboards ships **no** default workspace — verified on a clean
/// 3.8.0 with `workspace.enabled`: `/api/workspaces/_list` returns `total: 0`,
/// and `/app/workspace_initial` exists precisely so a human creates the first
/// one. So this is created — and deliberately NOT made the landing workspace:
/// `defaultWorkspace` is global and outlives the workspace it names, so users
/// land on the home page and pick, which is also where they can choose one
/// themselves.
pub const WORKSPACE_NAME: &str = "Observability Stack";

pub fn workspace_name(_deployment: &str) -> String {
    WORKSPACE_NAME.to_string()
}

/// Prefix a Dashboards API path with the workspace when there is one.
///
/// Workspace-scoped saved objects are addressed as `/w/{id}/api/...`; the same
/// path without the prefix writes into the global space, where the Observability
/// workspace cannot see it. Verified live: objects created without the prefix
/// report `total: 0` from inside the workspace.
fn osd_url(dash: &str, workspace: &str, path: &str) -> String {
    if workspace.is_empty() {
        format!("{dash}{path}")
    } else {
        format!("{dash}/w/{workspace}{path}")
    }
}

/// Turn Security-plugin multi-tenancy on or off cluster-wide.
///
/// Upstream states this as a prerequisite, not a suggestion: with the Security
/// plugin installed, multi-tenancy must be off when workspaces are on, because
/// both scope the same saved objects and an object written under one tenant is
/// invisible to a session resolving another. Setting only the Dashboards half
/// (`opensearch_security.multitenancy.enabled`) leaves the cluster half saying
/// the opposite, so both are written.
///
/// `PUT _plugins/_security/api/tenancy/config` rather than an edit to the
/// security `config.yml`: no securityconfig reload, no node restart, and it is
/// symmetric — uninstall restores what install found.
///
/// Best-effort and non-fatal. A cluster that refuses this call is a cluster
/// where tenancy was never ours to manage, and the install should still
/// complete; the mismatch surfaces as empty screens, which is what the check
/// asserts against.
pub async fn set_multitenancy(deployment: &Deployment, enabled: bool) {
    let base = crate::recipes::os_base(deployment);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    match c
        .put(format!("{base}/_plugins/_security/api/tenancy/config"))
        .basic_auth(&u, Some(&p))
        .json(&serde_json::json!({ "multitenancy_enabled": enabled }))
        .send()
        .await
    {
        Ok(r) => tracing::info!(
            "otel stack: multitenancy_enabled={enabled} for {deployment} ({})",
            r.status()
        ),
        Err(e) => tracing::error!("otel stack: setting multitenancy for {deployment}: {e}"),
    }
}

/// Is this `.kibana*` index a tenant's, as opposed to the Global tenant's?
///
/// Global is `.kibana` plus its migration generations `.kibana_1`, `.kibana_2`,
/// … — a numeric suffix and nothing else. Tenants get
/// `.kibana_<hash>_<tenant>`, and private tenants `.kibana_<hash>_<user>`. The
/// Security plugin strips special characters from those names, so this
/// classifies by shape instead of trying to reconstruct the name.
fn is_tenant_index(index: &str) -> bool {
    let Some(rest) = index.strip_prefix(".kibana") else {
        return false;
    };
    let rest = rest.trim_start_matches('_');
    !rest.is_empty() && !rest.chars().all(|c| c.is_ascii_digit())
}

/// How many saved objects live in this deployment's named tenant.
///
/// Detection, so the migration is conditional rather than universal. Tenant
/// saved objects live in `.kibana_<hash>_<tenant>` (the hash is the Security
/// plugin's, and it strips special characters, so the tenant name is matched
/// loosely rather than reconstructed). `None` means the question could not be
/// answered — which is not the same as zero and must not be reported as "safe".
pub async fn tenant_saved_objects(deployment: &Deployment) -> Option<u64> {
    let base = crate::recipes::os_base(deployment);
    let c = crate::recipes::http().ok()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let body = c
        .get(format!(
            "{base}/_cat/indices/.kibana*?h=index,docs.count&format=json"
        ))
        .basic_auth(&u, Some(&p))
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;
    // `.kibana`, `.kibana_1`, `.kibana_2` … are the Global tenant's migration
    // generations. Anything carrying a non-numeric suffix is a named or private
    // tenant, and those are the only ones a workspace migration has to move.
    let mut total = 0u64;
    for row in body.as_array()? {
        let Some(idx) = row.get("index").and_then(|v| v.as_str()) else {
            continue;
        };
        if !is_tenant_index(idx) {
            continue;
        }
        total += row
            .get("docs.count")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
    }
    Some(total)
}

/// Saved-object types worth carrying from a tenant into the workspace.
///
/// `config` is deliberately absent: it holds per-space UI settings, and copying
/// one space's over another's is how a migration breaks the destination.
const MIGRATABLE_TYPES: [&str; 5] = [
    "dashboard",
    "visualization",
    "search",
    "index-pattern",
    "query",
];

/// Export a tenant's saved objects as ndjson, for import into the workspace.
///
/// **Must run before `set_multitenancy(false)`.** Once multi-tenancy is off the
/// tenant indices are still on disk but nothing resolves them, so this returns
/// an empty export and the data looks like it was never there.
async fn export_tenant_objects(deployment: &Deployment) -> Option<String> {
    let dash = crate::recipes::dashboards_base(deployment);
    let c = crate::recipes::http().ok()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let body = c
        .post(format!("{dash}/api/saved_objects/_export"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .header("securitytenant", crate::recipes::tenant_name(deployment))
        .json(&serde_json::json!({
            "type": MIGRATABLE_TYPES,
            "includeReferencesDeep": true,
        }))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    (!body.trim().is_empty()).then_some(body)
}

/// Import an ndjson export into the deployment's Observability workspace.
///
/// `_import` takes multipart/form-data and nothing else. The body is built by
/// hand rather than pulling in reqwest's `multipart` feature for one field —
/// one file part with a fixed boundary is deterministic and short.
///
/// `overwrite=true` because this is re-runnable: a partially completed
/// migration must converge on a retry rather than collide with itself.
async fn import_objects(deployment: &Deployment, workspace: &str, ndjson: &str) -> Result<()> {
    let dash = crate::recipes::dashboards_base(deployment);
    let c = crate::recipes::http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    const BOUNDARY: &str = "veloxsearchtenantmigration";
    let body = format!(
        "--{BOUNDARY}\r\n         Content-Disposition: form-data; name=\"file\"; filename=\"export.ndjson\"\r\n         Content-Type: application/ndjson\r\n\r\n         {ndjson}\r\n         --{BOUNDARY}--\r\n"
    );
    let r = c
        .post(osd_url(
            &dash,
            workspace,
            "/api/saved_objects/_import?overwrite=true",
        ))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body)
        .send()
        .await
        .context("importing the tenant's saved objects")?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "import rejected ({status}): {}",
            text.chars().take(300).collect::<String>()
        );
    }
    Ok(())
}

/// Create (or find) this deployment's Observability workspace and return its id.
///
/// The `use-case-observability` feature is the whole point: it is what makes
/// Dashboards render the Observability nav group, and therefore the Agent
/// Monitoring and Application performance sections. A workspace with any other
/// use case lists none of them.
pub async fn ensure_workspace(dep: &Deployment) -> Result<String> {
    let deployment = dep.name();
    if let Some(id) = workspace_id(dep).await {
        return Ok(id);
    }
    let dash = crate::recipes::dashboards_base(dep);
    let c = crate::recipes::http()?;
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let body: serde_json::Value = c
        .post(format!("{dash}/api/workspaces"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .json(&serde_json::json!({
            "attributes": {
                "name": workspace_name(deployment),
                "description": "OTel traces, logs and metrics for this deployment",
                "features": ["use-case-observability"],
            }
        }))
        .send()
        .await
        .context("creating the observability workspace")?
        .json()
        .await
        .context("parsing the workspace response")?;
    body.get("result")
        .and_then(|r| r.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Dashboards accepted the workspace but returned no id"))
}

/// Id of this deployment's workspace, if it exists.
async fn workspace_id(dep: &Deployment) -> Option<String> {
    let deployment = dep.name();
    let dash = crate::recipes::dashboards_base(dep);
    let c = crate::recipes::http().ok()?;
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let want = workspace_name(deployment);
    let body: serde_json::Value = c
        .post(format!("{dash}/api/workspaces/_list"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .json(&serde_json::json!({ "perPage": 100 }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    body.get("result")?
        .get("workspaces")?
        .as_array()?
        .iter()
        .find(|w| w.get("name").and_then(|v| v.as_str()) == Some(want.as_str()))
        .and_then(|w| w.get("id")?.as_str().map(str::to_string))
}

/// Drop Data Prepper's span index template so the next start reinstalls the one
/// matching the configured `index_type`.
///
/// Best-effort: a 404 means there is nothing stale, which is the good case.
async fn clear_span_template(dep: &Deployment) {
    let base = crate::recipes::os_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let _ = c
        .delete(format!("{base}/_template/{SPAN_TEMPLATE}"))
        .basic_auth(&u, Some(&p))
        .send()
        .await;
}

/// Roll the span alias so writes move to an index built from the new template.
///
/// Keeps the previous indices readable — the alias still covers them — which is
/// why this is a rollover and not a delete. No-op when the alias does not exist
/// yet (a first install), since Data Prepper creates it on first write.
async fn roll_span_alias(dep: &Deployment) {
    let deployment = dep.name();
    let base = crate::recipes::os_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    match c
        .post(format!("{base}/{SPAN_ALIAS}/_rollover"))
        .basic_auth(&u, Some(&p))
        .json(&serde_json::json!({}))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            tracing::info!("otel stack: rolled {SPAN_ALIAS} for {deployment}")
        }
        Ok(r) => tracing::info!(
            "otel stack: no rollover for {deployment} ({}), nothing to migrate",
            r.status()
        ),
        Err(e) => tracing::error!("otel stack: rolling {SPAN_ALIAS} for {deployment}: {e}"),
    }
}

/// Attach the Prometheus connection to the workspace.
///
/// The `data-connection` saved object is created by the directquery route in
/// the **global** space — it is the one object here we do not write ourselves,
/// so it cannot be created through `/w/{id}`. Without this association the
/// workspace's Metrics screens do not list it, and the APM correlation points
/// at something the workspace cannot see. Idempotent: re-associating is a no-op.
async fn associate_datasource(dep: &Deployment, workspace: &str, data_connection_id: &str) {
    let dash = crate::recipes::dashboards_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let _ = c
        .post(format!("{dash}/api/workspaces/_associate"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .json(&serde_json::json!({
            "workspaceId": workspace,
            "savedObjects": [{ "type": "data-connection", "id": data_connection_id }],
        }))
        .send()
        .await;
}

/// The Dashboards settings that decide what the Observability screens do.
///
/// * `observability:apmEnabled` — the plugin's own words: "When enabled and the
///   Discover Traces feature is active, APM Services and Topology Map pages are
///   available in the navigation. Otherwise, Trace Analytics pages are shown as
///   fallback." Off, the APM half of the experience silently degrades.
/// * `observability:alertManagerSelectedDatasources` — which datasource the
///   Alerts page opens against; without it that page loads empty.
async fn apply_ui_settings(dep: &Deployment, workspace: &str) {
    let deployment = dep.name();
    let dash = crate::recipes::dashboards_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    // `defaultWorkspace` is deliberately NOT set. It is a global setting, so it
    // would send every user of this Dashboards into our workspace whether or
    // not that is where they were going — and it outlives the workspace: delete
    // the workspace and the setting still points at it, landing users on a dead
    // id with no way back but editing advanced settings. Users land on the home
    // page and pick, which is also where they can set it themselves.

    // The observability ones are workspace-scoped: written through `/w/{id}`
    // they land on the workspace object itself (confirmed live — they come back
    // under its `uiSettings`), which is where the screens read them.
    let _ = c
        .post(osd_url(
            &dash,
            workspace,
            "/api/opensearch-dashboards/settings",
        ))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .json(&serde_json::json!({ "changes": {
            "observability:apmEnabled": true,
            "observability:alertManagerSelectedDatasources": [datasource_name(deployment)],
            // The legacy Trace Analytics screens default to the v1 patterns and
            // are the fallback whenever the APM experience is unavailable. The
            // documented pipeline writes the service map to v2, so without this
            // that fallback reads an index nothing writes.
            "observability:traceAnalyticsSpanIndices": SPAN_PATTERN,
            "observability:traceAnalyticsServiceIndices": SERVICE_MAP_PATTERN,
        }}))
        .send()
        .await;
}

/// Drop the workspace (and with it every saved object inside) and put the
/// settings back.
async fn remove_workspace(dep: &Deployment) {
    let dash = crate::recipes::dashboards_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    // `null` on a setting removes the user value, restoring the OSD default —
    // the same shape `set_monitor` uses on an annotation.
    let _ = c
        .post(format!("{dash}/api/opensearch-dashboards/settings"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .json(&serde_json::json!({ "changes": {
            "defaultWorkspace": serde_json::Value::Null,
            "observability:apmEnabled": serde_json::Value::Null,
            "observability:alertManagerSelectedDatasources": serde_json::Value::Null,
            "observability:traceAnalyticsSpanIndices": serde_json::Value::Null,
            "observability:traceAnalyticsServiceIndices": serde_json::Value::Null,
        }}))
        .send()
        .await;
    if let Some(id) = workspace_id(dep).await {
        let _ = c
            .delete(format!("{dash}/api/workspaces/{id}"))
            .basic_auth(&u, Some(&p))
            .header("osd-xsrf", "true")
            .send()
            .await;
    }
}

/// How long to wait for Data Prepper's sinks to create their indices before
/// giving up on a pattern's field list.
///
/// The sinks create the index *and* its template on connect, before the first
/// document, so this waits on a startup — not on the user sending telemetry.
const FIELDS_WAIT_SECS: u64 = 600;

/// Fetch the field list OSD would cache for `pattern`, retrying until the
/// backing index exists.
///
/// Returns `None` if the deadline passes with nothing to read.
async fn fields_for(
    c: &reqwest::Client,
    dash: &str,
    workspace: &str,
    u: &str,
    p: &str,
    pattern: &str,
    deadline: std::time::Instant,
) -> Option<Vec<serde_json::Value>> {
    loop {
        if let Ok(resp) = c
            .get(osd_url(
                dash,
                workspace,
                "/api/index_patterns/_fields_for_wildcard",
            ))
            .query(&[("pattern", pattern)])
            .basic_auth(u, Some(p))
            .header("osd-xsrf", "true")
            .send()
            .await
        {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(f) = body.get("fields").and_then(|f| f.as_array()) {
                    if !f.is_empty() {
                        return Some(f.clone());
                    }
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

/// Create the three index patterns with the Observability attributes **and**
/// their cached field lists, in one write each.
///
/// The field list is not optional polish. A pattern created through the raw
/// saved-objects API lands with no `fields` attribute — OSD caches fields on
/// the saved object rather than reading the mapping per query — and until then
/// every screen that aggregates on one of them dies with "Could not locate that
/// index-pattern-field (id: endTime)", which is exactly what the Traces screen
/// showed on a fresh install.
///
/// Two things this has to get right, both learned the hard way:
///
/// * **Wait for the index.** This runs right after the components report ready,
///   which is *before* Data Prepper's sinks have created `otel-v1-apm-span-*`
///   and friends. Reading the field list then returns nothing, every time — so
///   the pattern shipped fieldless on every fresh install. `fields_for` retries
///   until the sink has created the index; it needs no telemetry to have been
///   sent, only the sink to have connected.
/// * **One write, not two.** The previous shape POSTed the pattern with
///   `overwrite=true` and then PUT the fields. On a re-install the POST wiped a
///   good field list, and any failure in between left the pattern worse than it
///   was found. Fields are resolved first and written with the rest.
async fn ensure_index_patterns(dep: &Deployment, workspace: &str) {
    let dash = crate::recipes::dashboards_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(FIELDS_WAIT_SECS);

    for spec in index_patterns() {
        let mut attrs = serde_json::json!({
            "title": spec.title,
            "timeFieldName": spec.time_field,
        });
        if let Some(s) = spec.signal_type {
            attrs["signalType"] = serde_json::json!(s);
        }
        if let Some(s) = spec.schema_mappings {
            attrs["schemaMappings"] = serde_json::json!(s);
        }
        if let Some(s) = spec.display_name {
            attrs["displayName"] = serde_json::json!(s);
        }
        match fields_for(&c, &dash, workspace, &u, &p, spec.title, deadline).await {
            Some(fields) => {
                attrs["fields"] = serde_json::json!(serde_json::Value::Array(fields).to_string());
            }
            // Writing the pattern anyway beats writing nothing: Discover can
            // still open it and fill the list on first visit. Loud, because a
            // pattern without fields is the defect above.
            None => tracing::error!(
                "otel stack: no fields for {} on {dep} after {FIELDS_WAIT_SECS}s — \
                 the Observability screens that aggregate on it will fail until it is refreshed",
                spec.title
            ),
        }
        let _ = c
            .post(osd_url(
                &dash,
                workspace,
                &format!(
                    "/api/saved_objects/index-pattern/{}?overwrite=true",
                    spec.id
                ),
            ))
            .basic_auth(&u, Some(&p))
            .header("osd-xsrf", "true")
            .json(&serde_json::json!({ "attributes": attrs }))
            .send()
            .await;
    }
}

/// Create the boards, their panels and the correlations.
///
/// No `securitytenant` header, unlike the recipe path: the APM correlation
/// references a `data-connection` saved object that Dashboards creates itself
/// when the datasource is registered, outside any tenant we choose. Putting our
/// half in a private tenant would leave a correlation pointing at a reference
/// the plugin cannot resolve.
async fn ensure_dashboards(dep: &Deployment, workspace: &str, data_connection_id: Option<&str>) {
    let deployment = dep.name();
    let mut objs = dashboard_objects(deployment);
    objs.extend(correlation_objects(deployment, data_connection_id));
    if objs.is_empty() {
        return;
    }
    let dash = crate::recipes::dashboards_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let resp = c
        .post(osd_url(
            &dash,
            workspace,
            "/api/saved_objects/_bulk_create?overwrite=true",
        ))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .json(&objs)
        .send()
        .await;
    // `_bulk_create` answers 200 with a per-object `error` for anything it
    // rejected, so a bare "did the request succeed" tells us nothing. This runs
    // in a background task where the only way a rejection reaches a human is
    // the log.
    match resp {
        Err(e) => tracing::error!("otel stack: creating dashboards for {deployment}: {e}"),
        Ok(r) => {
            let status = r.status();
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            let failed: Vec<String> = body
                .get("saved_objects")
                .and_then(|v| v.as_array())
                .map(|objs| {
                    objs.iter()
                        .filter(|o| o.get("error").is_some())
                        .filter_map(|o| {
                            Some(format!(
                                "{}: {}",
                                o.get("id")?.as_str()?,
                                o.get("error")?.get("message")?.as_str()?
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !status.is_success() || !failed.is_empty() {
                tracing::error!(
                    "otel stack: dashboards for {deployment} returned {status}; rejected: {failed:?}"
                );
            }
        }
    }
}

/// Register the Prometheus datasource **through Dashboards**, and return the id
/// of the `data-connection` saved object it creates.
///
/// Deliberately not the OpenSearch `_plugins/_query/_datasources` API, which is
/// where this started: that call registers the connector engine-side only. The
/// Metrics UI lists connections from the OSD saved objects, and the APM
/// correlation references one by saved-object id — neither of which the
/// engine-side entry produces. `POST {osd}/api/directquery/dataconnections`
/// creates both halves in one call, and is the same route the upstream
/// observability-stack init job uses.
///
/// Idempotency is awkward here because the OpenSearch side rejects a duplicate
/// name (400 "A datasource already exists with name") while the OSD side may
/// still be missing its saved object — the state a pre-rev.2 install leaves
/// behind.
///
/// The rule is therefore: the pair is good only when **both** halves are
/// present. Trusting the saved object alone is not theoretical — it shipped
/// once and a re-install short-circuited without ever re-creating the
/// engine-side entry, so the panel reported no datasource while the boards
/// rendered. Anything short of both halves is torn down (a duplicate name is a
/// hard 400 from OpenSearch, and a stale saved object shadows the new one) and
/// registered again through Dashboards.
pub async fn register_datasource(dep: &Deployment) -> Result<String> {
    let deployment = dep.name();
    let name = datasource_name(deployment);
    let existing = data_connection_id(dep).await;
    if datasource_exists(dep).await {
        if let Some(id) = existing {
            return Ok(id);
        }
    }

    let base = crate::recipes::os_base(dep);
    let dash = crate::recipes::dashboards_base(dep);
    let c = crate::recipes::http()?;
    let (u, p) = crate::k8s::admin_creds(dep).await;

    // Clear whichever half is present. 404 on either is fine.
    let _ = c
        .delete(format!("{base}/_plugins/_query/_datasources/{name}"))
        .basic_auth(&u, Some(&p))
        .send()
        .await;
    if let Some(id) = &existing {
        let _ = c
            .delete(format!(
                "{dash}/api/saved_objects/data-connection/{id}?force=true"
            ))
            .basic_auth(&u, Some(&p))
            .header("osd-xsrf", "true")
            .send()
            .await;
    }

    let resp = c
        .post(format!("{dash}/api/directquery/dataconnections"))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .json(&datasource_body(deployment))
        .send()
        .await
        .context("registering prometheus datasource")?;
    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("datasource registration returned {code}: {}", body.trim());
    }

    data_connection_id(dep).await.ok_or_else(|| {
        anyhow::anyhow!("datasource {name} registered but no data-connection saved object appeared")
    })
}

/// Rotate the credential the published endpoints check, and return the new one.
///
/// **What this actually costs, measured against the inventory rather than
/// guessed:** the password's bcrypt hash is baked into two config files — the
/// collector's inline htpasswd and the Alertmanager web config — and both are
/// hashed into their pods' `config-hash` annotation. So a rotation re-applies
/// the manifests, which rolls the **collector** and the **Alertmanager** pods.
/// Data Prepper, Cortex and the exporter are untouched; the OpenSearch admin
/// credential is a different secret entirely and does not move.
///
/// The user-visible consequence is the one the UI has to state plainly: every
/// exporter already configured with the old password starts getting 401 the
/// moment the collector comes back, and there is no grace period — the
/// collector holds exactly one credential.
pub async fn reset_credentials(dep: &Deployment) -> Result<(String, String)> {
    let deployment = dep.name();
    let status = crate::k8s::get_deployment(dep)
        .await
        .context("reading deployment status")?
        .ok_or_else(|| anyhow::anyhow!("no such deployment: {deployment}"))?;
    if status.otel_stack.is_empty() {
        bail!("the observability stack is not installed on {deployment}");
    }
    let (user, password) = crate::k8s::admin_creds(dep).await;
    if password.is_empty() {
        bail!("no admin credentials for {deployment}");
    }

    let new = crate::k8s::gen_token().context("generating the telemetry credential")?;
    let mut endpoint = endpoint_access(deployment).await?;
    endpoint.password_hash = crate::k8s::bcrypt_hash(&new)?;
    endpoint.password = new.clone();

    let client = crate::k8s::client().await?;
    let targets = ScrapeTargets::default();
    for o in manifests(dep, &user, &password, &targets, &endpoint) {
        crate::k8s::apply_dynamic(
            &client,
            o.group,
            o.version,
            o.kind,
            Some(o.namespace),
            &o.name,
            &o.manifest,
        )
        .await
        .with_context(|| format!("applying {} {}", o.kind, o.name))?;
    }
    Ok((OTLP_USER.to_string(), new))
}

/// The credential for the published endpoints, read from the Secret.
///
/// Its own call rather than a field on the 15-second status poll, for the same
/// reason the cluster admin password has one: a secret that rides a background
/// poll ends up in every proxy log and browser cache between here and the user.
pub async fn credentials(dep: &Deployment) -> Result<(String, String)> {
    let deployment = dep.name();
    use k8s_openapi::api::core::v1::Secret;
    use kube::Api;
    let client = crate::k8s::client().await?;
    let secrets: Api<Secret> = Api::namespaced(client, AGENT_NS);
    let data = secrets
        .get_opt(&obj_name(deployment, "dp"))
        .await
        .context("reading the telemetry credential")?
        .and_then(|s| s.data)
        .unwrap_or_default();
    let get = |k: &str| {
        data.get(k)
            .and_then(|v| String::from_utf8(v.0.clone()).ok())
            .unwrap_or_default()
    };
    let (u, p) = (get("OTLP_USERNAME"), get("OTLP_PASSWORD"));
    if p.is_empty() {
        bail!("no telemetry credential for {deployment}; is the stack installed?");
    }
    Ok((u, p))
}

/// How many of this stack's boards exist in Dashboards right now.
///
/// Counted by asking for each id rather than listing, so a deployment sharing a
/// cluster with another never counts its neighbour's.
async fn boards_present(dep: &Deployment, workspace: &str) -> u32 {
    let deployment = dep.name();
    let dash = crate::recipes::dashboards_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return 0;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let mut n = 0;
    for o in dashboard_objects(deployment) {
        if o.get("type").and_then(|t| t.as_str()) != Some("dashboard") {
            continue;
        }
        let Some(id) = o.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if matches!(
            c.get(osd_url(&dash, workspace, &format!("/api/saved_objects/dashboard/{id}")))
                .basic_auth(&u, Some(&p))
                .header("osd-xsrf", "true")
                .send()
                .await,
            Ok(r) if r.status().is_success()
        ) {
            n += 1;
        }
    }
    n
}

/// Saved-object id of this deployment's Prometheus `data-connection`, if any.
async fn data_connection_id(dep: &Deployment) -> Option<String> {
    let deployment = dep.name();
    let dash = crate::recipes::dashboards_base(dep);
    let c = crate::recipes::http().ok()?;
    let (u, p) = crate::k8s::admin_creds(dep).await;
    let name = datasource_name(deployment);
    let body: serde_json::Value = c
        .get(format!(
            "{dash}/api/saved_objects/_find?per_page=1000&type=data-connection"
        ))
        .basic_auth(&u, Some(&p))
        .header("osd-xsrf", "true")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    body.get("saved_objects")?.as_array()?.iter().find_map(|o| {
        (o.get("attributes")?.get("connectionId")?.as_str()? == name)
            .then(|| o.get("id")?.as_str().map(str::to_string))
            .flatten()
    })
}

async fn datasource_exists(dep: &Deployment) -> bool {
    let deployment = dep.name();
    let base = crate::recipes::os_base(dep);
    let Ok(c) = crate::recipes::http() else {
        return false;
    };
    let (u, p) = crate::k8s::admin_creds(dep).await;
    matches!(
        c.get(format!(
            "{base}/_plugins/_query/_datasources/{}",
            datasource_name(deployment)
        ))
        .basic_auth(&u, Some(&p))
        .send()
        .await,
        Ok(r) if r.status().is_success()
    )
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "logs-ab12";

    /// The scoped counterpart of [`D`]. Tests that only build strings keep
    /// using `D`; anything that renders a namespaced object needs the token,
    /// and it carries the app namespace so the existing `{name}.{ns}.svc`
    /// assertions still describe an admin-scope deployment.
    fn d() -> Deployment {
        Deployment::for_test(D, crate::k8s::ns(), None)
    }

    fn dns_ok(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 63
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !s.starts_with('-')
            && !s.ends_with('-')
    }

    #[test]
    fn names_dns_safe_and_bounded() {
        for o in manifests(
            &d(),
            "admin",
            "pw",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        ) {
            assert!(dns_ok(&o.name), "bad object name: {}", o.name);
        }
        // ADR-020 caps a deployment name at 30 chars (base + "-xxxx"); the
        // longest suffix here is "-data-prepper".
        let longest_d = "a".repeat(30);
        for o in manifests(
            &Deployment::for_test(&longest_d, crate::k8s::ns(), None),
            "admin",
            "pw",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        ) {
            assert!(
                o.name.len() <= 63,
                "name over 63: {} ({})",
                o.name,
                o.name.len()
            );
        }
    }

    #[test]
    fn install_set_equals_teardown_set() {
        // Uninstall walks `manifests()` in reverse, so the two sets are equal by
        // construction. This test is the guard that a future edit does not
        // introduce a second, hand-maintained delete list.
        let created = created_objects(&d());
        let deleted: BTreeSet<ObjectKey> = manifests(
            &d(),
            "u",
            "p",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        )
        .iter()
        .rev()
        .map(|o| o.key())
        .collect();
        assert_eq!(created, deleted);
        assert_eq!(
            created.len(),
            16,
            "expected 16 objects, got {}",
            created.len()
        );

        // Ingress mode adds exactly the three signal routes, and uninstall
        // walks the same list — so a deployment with routes still tears down
        // to nothing.
        let with_routes = EndpointAccess {
            ingress: Some(IngressAccess::for_deployment(
                D,
                "example.com",
                "traefik",
                "tls",
            )),
            ..Default::default()
        };
        let routed: BTreeSet<ObjectKey> =
            manifests(&d(), "u", "p", &ScrapeTargets::default(), &with_routes)
                .iter()
                .map(|o| o.key())
                .collect();
        assert_eq!(routed.len(), 19);
        assert!(created.is_subset(&routed));
    }

    #[test]
    fn published_routes_are_authenticated() {
        // The two routes are reachable from the internet, and neither component
        // has authentication of its own by default. Anything that removes these
        // checks publishes an open write endpoint and an open alert console.
        let ep = EndpointAccess {
            password: "s3cret".into(),
            password_hash: "$2y$12$abc".into(),
            ingress: Some(IngressAccess::for_deployment(
                D,
                "example.com",
                "traefik",
                "tls",
            )),
        };
        let objs = manifests(&d(), "u", "p", &ScrapeTargets::default(), &ep);
        let secret = objs
            .iter()
            .find(|o| o.kind == "Secret")
            .expect("credential secret");
        let sd = &secret.manifest["stringData"];
        let otel = sd["otel-config.yaml"].as_str().unwrap();
        assert!(otel.contains("basicauth/otlp"));
        assert!(otel.contains("authenticator: basicauth/otlp"));
        assert!(otel.contains("extensions: [basicauth/otlp]"));
        assert!(sd["am-web.yml"]
            .as_str()
            .unwrap()
            .contains("basic_auth_users"));

        let am = objs
            .iter()
            .find(|o| o.name.ends_with("-alertmanager") && o.kind == "Deployment")
            .expect("alertmanager");
        let args = serde_json::to_string(&am.manifest).unwrap();
        assert!(args.contains("--web.config.file="));

        // Only the collector is published — the upstream architecture has one
        // external ingest surface, and everything behind it (Cortex,
        // Alertmanager, Data Prepper, the OpenSearch API) stays in-cluster.
        // Cortex and the exporter have no authentication at all; publishing
        // either would hand over this deployment's metrics and its ruler.
        let routes: Vec<&K8sObject> = objs.iter().filter(|o| o.kind == "Ingress").collect();
        assert_eq!(routes.len(), 3);
        for r in &routes {
            let backend = &r.manifest["spec"]["rules"][0]["http"]["paths"][0]["backend"]["service"];
            assert_eq!(backend["name"].as_str().unwrap(), obj_name(D, "collector"));
            assert_eq!(backend["port"]["number"].as_u64().unwrap(), 4318);
        }
        // Each host carries only its own signal, so the name is not decoration.
        for (signal, path) in SIGNALS {
            let r = routes
                .iter()
                .find(|o| o.name.ends_with(signal))
                .unwrap_or_else(|| panic!("no route for {signal}"));
            let rule = &r.manifest["spec"]["rules"][0];
            assert!(rule["host"].as_str().unwrap().contains(signal));
            assert_eq!(rule["http"]["paths"][0]["path"].as_str().unwrap(), path);
            assert_eq!(
                rule["http"]["paths"][0]["pathType"].as_str().unwrap(),
                "Exact"
            );
        }
    }

    #[test]
    fn the_policy_opens_only_authenticated_ports() {
        // A published route is worthless if the NetworkPolicy denies the
        // ingress controller — that is what a 502 on the Alertmanager route
        // turned out to be — and dangerous if it opens a port whose component
        // has no authentication.
        let ports = |ep: &EndpointAccess| -> Vec<u64> {
            let objs = manifests(&d(), "u", "p", &ScrapeTargets::default(), ep);
            let np = objs.iter().find(|o| o.kind == "NetworkPolicy").unwrap();
            np.manifest["spec"]["ingress"][0]["ports"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["port"].as_u64().unwrap())
                .collect()
        };

        assert_eq!(ports(&EndpointAccess::default()), vec![4317, 4318]);

        let published = EndpointAccess {
            ingress: Some(IngressAccess::for_deployment(
                D,
                "example.com",
                "traefik",
                "",
            )),
            ..Default::default()
        };
        // Publishing changes nothing here: the collector's OTLP ports were
        // already the only open ones, and no other component is published.
        // Cortex (9090), Alertmanager (9093) and the exporter (9114) all
        // authenticate nothing that the cluster at large should reach, so they
        // stay behind the from-selector.
        let open = ports(&published);
        assert_eq!(open, vec![4317, 4318]);
    }

    #[test]
    fn os_created_equals_os_teardown() {
        let t = os_teardown(D);
        let torn = crate::integrations::teardown_resources(&t);

        let mut created = BTreeSet::new();
        for spec in index_patterns() {
            created.insert((
                crate::integrations::ResourceKind::IndexPattern,
                spec.id.to_string(),
            ));
        }
        for o in os_saved_objects(D) {
            let id = o.get("id").and_then(|v| v.as_str()).unwrap().to_string();
            created.insert((crate::integrations::ResourceKind::SavedObject, id));
        }
        for (id, _) in ISM_POLICIES {
            created.insert((crate::integrations::ResourceKind::IsmPolicy, id.to_string()));
        }
        created.insert((
            crate::integrations::ResourceKind::Datasource,
            datasource_name(D),
        ));
        assert_eq!(created, torn);
    }

    #[test]
    fn otel_patterns_disjoint_from_recipe_patterns() {
        // Decision 5 of ADR-053: the two collection modes coexist because they
        // never write the same indices. Proven, not assumed.
        let recipe: BTreeSet<String> = crate::profiles::log_patterns().into_iter().collect();
        for p in [SPAN_PATTERN, SERVICE_MAP_PATTERN, LOGS_PATTERN] {
            assert!(!recipe.contains(p), "{p} collides with a recipe pattern");
            let stem = p.trim_end_matches('*');
            for r in &recipe {
                let rstem = r.trim_end_matches('*');
                assert!(
                    !stem.starts_with(rstem) && !rstem.starts_with(stem),
                    "{p} overlaps recipe pattern {r}"
                );
            }
        }
    }

    #[test]
    fn boards_are_self_consistent() {
        // Same rule `recipes::dashboard_objects_are_self_consistent` enforces:
        // a dashboard whose panel reference points at nothing renders as an
        // error card, and there is no way to notice that from Rust at runtime.
        let objs = os_saved_objects(D);
        let ids: BTreeSet<String> = objs
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), objs.len(), "duplicate saved-object id");

        let mut boards = 0;
        for o in &objs {
            if o["type"] != "dashboard" {
                continue;
            }
            boards += 1;
            let refs = o["references"].as_array().unwrap();
            assert!(!refs.is_empty(), "board {} has no panels", o["id"]);
            let panels: serde_json::Value =
                serde_json::from_str(o["attributes"]["panelsJSON"].as_str().unwrap()).unwrap();
            assert_eq!(panels.as_array().unwrap().len(), refs.len());
            for r in refs {
                let rid = r["id"].as_str().unwrap();
                assert!(
                    ids.contains(rid),
                    "board {} references missing {rid}",
                    o["id"]
                );
            }
        }
        assert_eq!(boards, 3, "expected three self-monitoring boards");
    }

    #[test]
    fn saved_objects_use_the_documented_names() {
        // The ids are NOT namespaced per deployment, and that is the point:
        // every deployment has its own OpenSearch Dashboards, so there is no
        // shared space to collide in, and the documented names are what let a
        // chart-authored asset resolve its references. A per-deployment prefix
        // would be ours alone and would break exactly that.
        let a: BTreeSet<String> = os_saved_objects("logs-ab12")
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_string())
            .collect();
        let b: BTreeSet<String> = os_saved_objects("logs-cd34")
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(a, b);

        for id in [
            "opensearch-cluster-health-dashboard",
            "observability-pipeline-health-dashboard",
            "k8s-cluster-health-dashboard",
        ] {
            assert!(a.contains(id), "{id} missing from the saved-object set");
        }
        assert_eq!(
            datasource_name("logs-ab12"),
            "ObservabilityStack_Prometheus"
        );
        assert_eq!(workspace_name("logs-ab12"), "Observability Stack");
    }

    /// Deleting telemetry must name the service map's physical index.
    ///
    /// `otel-v2-apm-service-map` is an alias; a wildcard delete on it matches
    /// no index and reports success while deleting nothing.
    #[test]
    fn the_service_map_is_deleted_by_its_physical_index() {
        assert_eq!(SERVICE_MAP_PATTERN, "otel-v2-apm-service-map*");
        assert_eq!(SERVICE_MAP_INDEX, "otel-v1-apm-service-map*");
        assert_ne!(SERVICE_MAP_INDEX, SERVICE_MAP_PATTERN);
    }

    /// The three time fields, pinned against upstream's init script.
    ///
    /// This exists because the service map shipped pointing at `hashId`, a
    /// field of the *legacy* `service_map_stateful` document shape that
    /// `otel_apm_service_map` never writes — so the pattern named a field
    /// absent from every document the stack produces. The failure is silent:
    /// the screen renders, and it is empty.
    #[test]
    fn index_patterns_use_the_documented_time_fields() {
        let by_title: std::collections::BTreeMap<&str, &str> = index_patterns()
            .iter()
            .map(|p| (p.title, p.time_field))
            .collect();
        assert_eq!(by_title["logs-otel-v1*"], "time");
        assert_eq!(by_title["otel-v1-apm-span*"], "endTime");
        assert_eq!(by_title["otel-v2-apm-service-map*"], "timestamp");
    }

    #[test]
    fn dashboards_config_carries_the_load_bearing_keys() {
        // The three that make the difference between "the Observability menu
        // exists" and "the Observability screens have something to read":
        // `explore` registers the saved-object type the PromQL boards are made
        // of, and `data_source` registers the `data-connection` type the APM
        // correlation references. Verified live on OpenSearch Dashboards 3.8.0.
        let keys: BTreeSet<&str> = crate::k8s::otel_dashboards_config()
            .iter()
            .map(|(k, _)| *k)
            .collect();
        for k in [
            "explore.enabled",
            "data_source.enabled",
            "observability.alertManager.enabled",
        ] {
            assert!(keys.contains(k), "{k} missing from the dashboards config");
        }
        // This build rejects it at boot with a fatal InvalidConfigurationError.
        assert!(!keys.contains("query_enhancements.ppl.lint.enabled"));
    }

    /// Global's migration generations must never be counted as tenant data.
    ///
    /// The count decides whether a migration is offered at all, so a false
    /// positive nags every install and a false negative silently strands a
    /// customer's dashboards. `.kibana_2` is Global's second generation, not a
    /// tenant named "2".
    #[test]
    fn tenant_indices_are_told_apart_from_global() {
        for global in [".kibana", ".kibana_1", ".kibana_2", ".kibana_42"] {
            assert!(!is_tenant_index(global), "{global} is Global, not a tenant");
        }
        for tenant in [
            ".kibana_92668751_admin_1",
            ".kibana_-152937574_veloxtestev10ns41",
            ".kibana_3775548_veloxlogsab12_1",
        ] {
            assert!(is_tenant_index(tenant), "{tenant} is a tenant index");
        }
        // Not a saved-objects index at all.
        assert!(!is_tenant_index("otel-v1-apm-span-000001"));
    }

    /// The UI keys and the stack keys must not overlap.
    ///
    /// They are applied by two different field managers over the same granular
    /// map, which is what lets uninstalling the stack leave the UI standing. A
    /// key claimed by both would be owned by whichever applied last, and the
    /// stack's uninstall would silently take the new UI down with it.
    #[test]
    fn the_ui_and_the_stack_own_disjoint_config_keys() {
        let ui: BTreeSet<&str> = crate::k8s::next_ui_config()
            .iter()
            .map(|(k, _)| *k)
            .collect();
        let stack: BTreeSet<&str> = crate::k8s::otel_dashboards_config()
            .iter()
            .map(|(k, _)| *k)
            .collect();
        assert!(
            ui.is_disjoint(&stack),
            "shared keys: {:?}",
            ui.intersection(&stack).collect::<Vec<_>>()
        );
        // The three that define the new UI, and the reason each is in this
        // group rather than the stack's.
        assert!(ui.contains("workspace.enabled"));
        assert!(ui.contains("uiSettings.overrides.home:useNewHomePage"));
        assert!(ui.contains("opensearch_security.multitenancy.enabled"));
        // The theme is already Next by default (`DEFAULT_THEME_VERSION = v8`,
        // whose label is "Next (preview)"); pinning it would only remove the
        // user's choice.
        assert!(!ui.contains("theme:version"));
    }

    /// Landing users in a workspace is not ours to decide.
    ///
    /// `defaultWorkspace` is global and outlives the workspace it names: delete
    /// the workspace and every user still lands on a dead id. Users land on the
    /// home page and choose.
    #[test]
    fn the_default_workspace_is_only_ever_cleared() {
        // Split so this assertion does not match its own source text.
        let key = format!("\"{}\"", concat!("default", "Workspace"));
        for (n, line) in include_str!("otel_stack.rs").lines().enumerate() {
            if line.contains(&key) && !line.contains("Null") {
                panic!(
                    "line {} assigns the landing workspace; it may only be cleared: {}",
                    n + 1,
                    line.trim()
                );
            }
        }
    }

    /// Workspaces and tenants cannot both scope the same saved objects.
    ///
    /// Upstream states it as a prerequisite rather than a suggestion: with the
    /// Security plugin installed, multi-tenancy must be off when workspaces are
    /// on. Leaving it on is not an error anyone reports — an object written
    /// under one tenant is simply invisible to a session that resolves another,
    /// which presents as the Observability screens being empty.
    #[test]
    fn workspaces_require_multitenancy_off() {
        // Both live in the UI group now: multi-tenancy off is a consequence of
        // workspaces, so it must travel with them rather than with the stack.
        let cfg: std::collections::BTreeMap<&str, &str> =
            crate::k8s::next_ui_config().iter().copied().collect();
        assert_eq!(cfg.get("workspace.enabled"), Some(&"true"));
        assert_eq!(
            cfg.get("opensearch_security.multitenancy.enabled"),
            Some(&"false"),
            "workspaces are on; multi-tenancy must be off"
        );
        // Datasets is gated on the nav groups, which read this setting.
        assert_eq!(
            cfg.get("uiSettings.overrides.home:useNewHomePage"),
            Some(&"true")
        );
    }

    #[test]
    fn collector_scrapes_every_component_a_board_reads() {
        let cfg = collector_config(D, &ScrapeTargets::default(), "u:hash");
        for job in ["otel-collector", "opensearch", "data-prepper", "cortex"] {
            assert!(
                cfg.contains(&format!("job_name: {job}")),
                "{job} not scraped"
            );
        }
        // Data Prepper does not serve the default path.
        assert!(cfg.contains("metrics_path: /metrics/prometheus"));
    }

    #[test]
    fn configs_are_per_deployment() {
        let a = "logs-ab12";
        let b = "logs-cd34";
        for cfg in [
            collector_config(a, &ScrapeTargets::default(), "u:hash"),
            cortex_config(a),
            data_prepper_pipelines(&Deployment::for_test(a, crate::k8s::ns(), None), "u", "p"),
        ] {
            assert!(!cfg.contains(b), "config for {a} mentions {b}:\n{cfg}");
        }
    }

    #[test]
    fn password_only_in_secret() {
        let pw = "s3cr3t-unlikely-token";
        let user = "admin";
        for o in manifests(
            &d(),
            user,
            pw,
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        ) {
            let body = serde_json::to_string(&o.manifest).unwrap();
            if o.kind == "Secret" {
                assert!(body.contains(pw), "the Secret must carry the password");
            } else {
                assert!(
                    !body.contains(pw),
                    "{}/{} leaks the password outside the Secret",
                    o.kind,
                    o.name
                );
            }
        }
    }

    #[test]
    fn config_hash_changes_with_password() {
        let one = manifests(
            &d(),
            "admin",
            "pw-one",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        );
        let two = manifests(
            &d(),
            "admin",
            "pw-two",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        );
        let hash = |v: &[K8sObject], part: &str| -> String {
            v.iter()
                .find(|o| o.kind == "Deployment" && o.name.ends_with(part))
                .unwrap()
                .manifest
                .pointer("/spec/template/metadata/annotations/veloxsearch.ai~1config-hash")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        };
        // A rotated password must roll both pods that authenticate with it —
        // subPath mounts never update live, so only a template change does it.
        assert_ne!(hash(&one, "data-prepper"), hash(&two, "data-prepper"));
        assert_ne!(hash(&one, "os-exporter"), hash(&two, "os-exporter"));
    }

    #[test]
    fn scrape_jobs_omitted_when_targets_absent() {
        let none = collector_config(D, &ScrapeTargets::default(), "u:hash");
        assert!(!none.contains("kube-state-metrics"));
        assert!(!none.contains("node-exporter"));

        let some = collector_config(
            D,
            &ScrapeTargets {
                kube_state_metrics: Some("ksm.monitoring.svc:8080".into()),
                node_exporter: Some("ne.monitoring.svc:9100".into()),
            },
            "u:hash",
        );
        assert!(some.contains("ksm.monitoring.svc:8080"));
        assert!(some.contains("ne.monitoring.svc:9100"));
        // Never the upstream chart's kube-system literals.
        assert!(!some.contains("kube-state-metrics.kube-system"));
    }

    #[test]
    fn resource_cost_matches_manifests() {
        let cost = resource_cost();
        let objs = manifests(
            &d(),
            "u",
            "p",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        );

        let mut cpu = 0u64;
        let mut mem = 0u64;
        let mut disk = 0u64;
        for o in &objs {
            if o.kind == "Deployment" {
                let r = o
                    .manifest
                    .pointer("/spec/template/spec/containers/0/resources/requests")
                    .unwrap();
                cpu += parse_cpu_millis(r["cpu"].as_str().unwrap());
                mem += parse_mem_mib(r["memory"].as_str().unwrap());
            }
            if o.kind == "PersistentVolumeClaim" {
                let s = o
                    .manifest
                    .pointer("/spec/resources/requests/storage")
                    .unwrap()
                    .as_str()
                    .unwrap();
                disk += s.trim_end_matches("Gi").parse::<u64>().unwrap();
            }
        }
        assert_eq!(
            cost.cpu_millis, cpu,
            "advertised CPU must equal summed requests"
        );
        assert_eq!(
            cost.mem_mib, mem,
            "advertised memory must equal summed requests"
        );
        assert_eq!(
            cost.disk_gib, disk,
            "advertised disk must equal summed PVCs"
        );
    }

    fn parse_cpu_millis(s: &str) -> u64 {
        match s.strip_suffix('m') {
            Some(n) => n.parse().unwrap(),
            None => s.parse::<u64>().unwrap() * 1000,
        }
    }
    fn parse_mem_mib(s: &str) -> u64 {
        if let Some(n) = s.strip_suffix("Gi") {
            n.parse::<u64>().unwrap() * 1024
        } else {
            s.trim_end_matches("Mi").parse().unwrap()
        }
    }

    #[test]
    fn pvcs_pin_longhorn() {
        for o in manifests(
            &d(),
            "u",
            "p",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        ) {
            if o.kind == "PersistentVolumeClaim" {
                assert_eq!(
                    o.manifest["spec"]["storageClassName"].as_str().unwrap(),
                    crate::bootstrap::LONGHORN_SC,
                    "ADR-043: deployment storage is Longhorn, named explicitly"
                );
            }
        }
    }

    #[test]
    fn every_deployment_has_a_service_and_recreate_strategy() {
        let objs = manifests(
            &d(),
            "u",
            "p",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        );
        for part in COMPONENTS {
            let dep = objs
                .iter()
                .find(|o| o.kind == "Deployment" && o.name == obj_name(D, part));
            assert!(dep.is_some(), "no Deployment for component {part}");
            // RWO volumes cannot be handed between two live pods.
            assert_eq!(
                dep.unwrap().manifest["spec"]["strategy"]["type"]
                    .as_str()
                    .unwrap(),
                "Recreate"
            );
            assert!(
                objs.iter()
                    .any(|o| o.kind == "Service" && o.name == obj_name(D, part)),
                "no Service for component {part}"
            );
        }
    }

    #[test]
    fn the_pipeline_emits_red_metrics() {
        // The reason the topology is upstream's and not a simpler one: only
        // `otel_apm_service_map` emits METRIC events alongside SERVICE_MAP, and
        // those are the per-service rate/error/duration series the APM screens
        // read from Prometheus. The older `service_map` processor emits neither,
        // which is how an earlier version of this module shipped an APM page
        // that could list services and never fill in their numbers.
        let p = data_prepper_pipelines(&d(), "u", "p");
        assert!(p.contains("otel_apm_service_map"));
        assert!(
            !p.contains("- service_map:"),
            "the v1 processor emits no RED metrics"
        );
        assert!(p.contains("index_type: otel-v2-apm-service-map"));
        assert!(SERVICE_MAP_PATTERN.starts_with("otel-v2-"));

        // The METRIC route has to reach a Prometheus sink, or the events are
        // produced and dropped.
        assert!(p.contains("service_processed_metrics: 'getEventType() == \"METRIC\"'"));
        assert!(p.contains("- prometheus:"));
        assert!(p.contains(&format!("http://{}:9090/api/v1/push", svc(D, "cortex"))));
        // Cortex rejects the series for cardinality if this survives.
        assert!(p.contains("delete_entries"));
        assert!(p.contains("randomKey"));
        // Dropping this collides multi-language services onto one series.
        assert!(p.contains("group_by_attributes: [telemetry.sdk.language]"));

        // The service map MUST read the enriched stream, not the raw one. On the
        // raw OTLP stream `serviceName` is absent and the processor drops every
        // span silently — measured, and the single reason this stack shipped
        // once with no RED metrics at all.
        let smap = p
            .split("service-map-pipeline:")
            .nth(1)
            .expect("service-map-pipeline");
        let source = smap.split("processor:").next().unwrap();
        assert!(
            source.contains("traces-raw-pipeline"),
            "the service map must source from the post-otel_traces stream, got: {source}"
        );
        assert!(
            !source.contains("otel-traces-pipeline"),
            "sourcing the RAW stream means serviceName is null and every span is dropped"
        );

        // Both plugins the RED path depends on are EXPERIMENTAL in 2.16.0.
        // Naming one without enabling it does not degrade — Data Prepper exits
        // with "No valid pipeline is available for execution" and the pod
        // crash-loops, which is how this shipped broken once.
        let cfg = data_prepper_config();
        assert!(cfg.contains("experimental:"));
        assert!(cfg.contains("- otel_apm_service_map"));
        assert!(cfg.contains("- prometheus"));
    }

    #[test]
    fn data_prepper_waits_for_opensearch_before_starting() {
        // Its sink gives up permanently after a bounded retry, leaving the pod
        // at 0/1 forever with the OTLP port never bound — so the wait belongs
        // before the process starts, not in its own retry loop.
        let objs = manifests(
            &d(),
            "u",
            "p",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        );
        let dp = objs
            .iter()
            .find(|o| o.kind == "Deployment" && o.name.ends_with("-data-prepper"))
            .unwrap();
        let init = &dp.manifest["spec"]["template"]["spec"]["initContainers"][0];
        assert_eq!(init["name"].as_str().unwrap(), "wait-for-opensearch");
        // Same image as the main container: nothing extra to pre-load on an
        // air-gapped cluster.
        assert_eq!(init["image"].as_str().unwrap(), DP_IMAGE);
        let cmd = serde_json::to_string(&init["command"]).unwrap();
        assert!(cmd.contains("_cluster/health"));
        assert!(cmd.contains("green|yellow"));
        // The password reaches it from the Secret, never inline.
        assert!(
            !cmd.contains("$ES_PASSWORD:"),
            "credential must not be interpolated into the command"
        );
        assert_eq!(
            init["env"][1]["valueFrom"]["secretKeyRef"]["name"]
                .as_str()
                .unwrap(),
            obj_name(D, "dp")
        );

        for o in objs.iter().filter(|o| o.kind == "Deployment") {
            if o.name.ends_with("-data-prepper") {
                continue;
            }
            assert!(o.manifest["spec"]["template"]["spec"]["initContainers"].is_null());
        }
    }

    #[test]
    fn data_prepper_is_ready_only_when_it_can_receive() {
        // Without this the Service routes to a pod whose OTLP listener has not
        // bound yet, the collector gets `connection refused`, and the first
        // traces after an install arrive minutes late or not at all.
        let objs = manifests(
            &d(),
            "u",
            "p",
            &ScrapeTargets::default(),
            &EndpointAccess::default(),
        );
        let dp = objs
            .iter()
            .find(|o| o.kind == "Deployment" && o.name.ends_with("-data-prepper"))
            .unwrap();
        let probe = &dp.manifest["spec"]["template"]["spec"]["containers"][0]["readinessProbe"];
        assert_eq!(
            probe["tcpSocket"]["port"].as_u64().unwrap(),
            DP_OTLP_PORT as u64
        );

        // Nothing else gains a probe it did not have.
        for o in objs.iter().filter(|o| o.kind == "Deployment") {
            if o.name.ends_with("-data-prepper") {
                continue;
            }
            assert!(
                o.manifest["spec"]["template"]["spec"]["containers"][0]["readinessProbe"].is_null(),
                "{} grew an unintended readiness probe",
                o.name
            );
        }
    }

    #[test]
    fn the_span_template_migration_only_runs_on_an_upgrade() {
        // Deleting the template on a FIRST install would leave new indices on
        // dynamic mapping — Data Prepper installs it at startup and never
        // reinstalls it later. So the migration is gated on replacing a stack
        // that predates the documented pipeline.
        assert!(
            !needs_span_migration(None),
            "a first install has nothing stale"
        );
        assert!(
            !needs_span_migration(Some(STACK_VERSION)),
            "re-installing the same version must not clear the template it needs"
        );
        assert!(
            needs_span_migration(Some("1")),
            "an older stack must be migrated"
        );
    }

    #[test]
    fn one_otlp_port_carries_every_signal() {
        // The documented pipeline routes by event type from a single `otlp`
        // source, so the separate logs listener is gone — and the collector
        // must not still be exporting to it.
        let p = data_prepper_pipelines(&d(), "u", "p");
        assert!(p.contains(&format!("port: {DP_OTLP_PORT}")));
        assert!(!p.contains("otel_logs_source"));
        assert!(!p.contains("otel_trace_source"));

        let cfg = collector_config(D, &ScrapeTargets::default(), "u:hash");
        assert!(!cfg.contains(":21892"));
        assert!(cfg.contains(&format!("{}:{DP_OTLP_PORT}", svc(D, "data-prepper"))));
    }

    /// Dev tool, not a test: emit the rendered inventory as a JSON array so it
    /// can be applied to a scratch cluster with
    /// `cargo test -- --ignored --nocapture dump_manifests | kubectl apply -f -`.
    /// Ignored by default; reads its inputs from the environment so no
    /// credential is ever compiled in.
    #[test]
    #[ignore]
    fn dump_manifests() {
        // The rendered configs embed `{deployment}.{ns()}.svc`, and off-cluster
        // `ns()` deliberately falls back to the inert `veloxsearch-dev` (#67).
        // Dumping with that fallback yields manifests whose OpenSearch sink
        // silently points at a namespace that does not exist — Data Prepper
        // then blocks its gRPC sources forever waiting on the sink. Fail here
        // instead of shipping that to a cluster.
        assert_ne!(
            crate::k8s::ns(),
            "veloxsearch-dev",
            "set POD_NAMESPACE to the app namespace before dumping manifests"
        );
        let d = std::env::var("VELOX_DUMP_DEPLOYMENT").expect("set VELOX_DUMP_DEPLOYMENT");
        let u = std::env::var("VELOX_DUMP_USER").unwrap_or_else(|_| "admin".into());
        let p = std::env::var("VELOX_DUMP_PASSWORD").expect("set VELOX_DUMP_PASSWORD");
        let targets = ScrapeTargets {
            kube_state_metrics: std::env::var("VELOX_DUMP_KSM").ok(),
            node_exporter: std::env::var("VELOX_DUMP_NE").ok(),
        };
        let items: Vec<serde_json::Value> = manifests(
            &Deployment::for_test(&d, crate::k8s::ns(), None),
            &u,
            &p,
            &targets,
            &EndpointAccess::default(),
        )
        .into_iter()
        .map(|o| o.manifest)
        .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "apiVersion": "v1", "kind": "List", "items": items })
            )
            .unwrap()
        );
    }

    #[test]
    fn collector_never_talks_to_another_deployments_data_prepper() {
        let cfg = collector_config(D, &ScrapeTargets::default(), "u:hash");
        assert!(cfg.contains(&format!("{}:21890", svc(D, "data-prepper"))));
        assert!(cfg.contains(&format!("http://{}:9090/api/v1/push", svc(D, "cortex"))));
    }
}
