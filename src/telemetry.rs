// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Server-only telemetry-source discovery (#65).
//!
//! A cluster that already emits telemetry — Prometheus, Hubble, an OTEL
//! collector — should light up in VeloxSearch with no extra configuration.
//! This module finds those sources. It is an extension of
//! [`crate::discovery`], not a parallel mechanism: `discover()` calls it and
//! carries the result in the same `Discovery` response the wizard's
//! monitor-at-creation step (ADR-018) already reads.
//!
//! ## Two paths to the same answer
//!
//! **Fast path — the sidecar manifest.** A cluster provisioned by sidecar
//! publishes a `ConfigMap` recording what it installed, including which
//! components are telemetry sources and at which service endpoints. When it is
//! there we read it: it is authoritative (it also carries the component name
//! and version, which no amount of scanning can tell us) and it is one GET.
//!
//! **Fallback — scan the cluster.** When the manifest is absent, we look at
//! Services directly and apply the same identification rules sidecar applied
//! when it wrote the manifest. This is the case that matters commercially: a
//! cluster VeloxSearch did not provision, and sidecar did not provision
//! either, must still light up. The two products stay independent — the
//! manifest is an optimisation and a source of provenance, never a dependency.
//!
//! ## Service identification contract v1
//!
//! [`classify`] below and sidecar's `roles/provisioning_manifest` implement
//! **the same three rules**. That is the whole reason the fallback produces
//! the same set as the manifest. Changing one side without the other breaks
//! the marriage; both sides carry a test that pins the rules, including the
//! near-misses (a node-exporter is a scrape *target*, not a source).
//!
//! | kind | a Service matches when |
//! |---|---|
//! | `hubble` | label `k8s-app` in {`hubble-relay`, `hubble`} OR name starts with `hubble-` |
//! | `prometheus` | label `app.kubernetes.io/name == prometheus` OR label `operated-prometheus == "true"` OR (name contains `prometheus` AND exposes port 9090) |
//! | `otel` | label `app.kubernetes.io/name` contains `opentelemetry` OR name contains `otel` OR exposes port 4317 / 4318 |
//!
//! ## Recipes stay pre-baked, and stay honest
//!
//! Each kind maps to ONE fixed recipe id — never anything generated. But the
//! id is only offered when the runtime catalog (ADR-039) actually carries that
//! package, because offering an ingest the core cannot install is a broken
//! promise. Until the registry publishes them, a discovered source is
//! *reported with no recipe*: visible in the wizard, honestly labelled, and
//! opt-in the moment the package ships — with no change to this code.

use crate::api::TelemetrySource;
use anyhow::Result;
use k8s_openapi::api::core::v1::{ConfigMap, Service};
use kube::api::ListParams;
use kube::{Api, Client};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// The sidecar seam
// ---------------------------------------------------------------------------

/// Where sidecar publishes its provisioning manifest. `kube-system` because it
/// is the one namespace guaranteed to exist on every cluster sidecar
/// provisions and it needs no lifecycle of its own — see that role's README.
pub const SIDECAR_MANIFEST_NS: &str = "kube-system";
pub const SIDECAR_MANIFEST_NAME: &str = "sidecar-provisioning-manifest";
pub const SIDECAR_MANIFEST_KEY: &str = "manifest.json";

/// The manifest shape this build understands. A future major means sidecar
/// changed the payload in a way we cannot read — we fall back to the scan
/// rather than guess at fields, which is exactly what an unprovisioned cluster
/// already does.
pub const SIDECAR_SCHEMA_SUPPORTED: u64 = 1;

/// `origin` values on a [`TelemetrySource`] — provenance, so the UI can say
/// "sidecar told us" versus "we found it ourselves".
pub const ORIGIN_MANIFEST: &str = "sidecar-manifest";
pub const ORIGIN_SCAN: &str = "cluster-scan";

// ---------------------------------------------------------------------------
// Kinds and their recipes
// ---------------------------------------------------------------------------

/// The telemetry sources we know how to recognise. Deliberately a closed set:
/// each one has (or will have) exactly one pre-baked ingest recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Prometheus,
    Hubble,
    Otel,
}

impl Kind {
    /// The wire value, matching sidecar's `kind` field verbatim.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Prometheus => "prometheus",
            Kind::Hubble => "hubble",
            Kind::Otel => "otel",
        }
    }

    /// The one pre-baked recipe id for this kind (ADR: fixed per source, never
    /// AI-generated). Whether it is *offerable* is a separate question — see
    /// [`offerable_recipes`].
    pub fn recipe_id(self) -> &'static str {
        // Same string as `as_str` today, and that is a coincidence worth
        // keeping separate: the wire kind is sidecar's vocabulary, the recipe
        // id is the registry's, and either can move without the other.
        match self {
            Kind::Prometheus => "prometheus",
            Kind::Hubble => "hubble",
            Kind::Otel => "otel",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s {
            "prometheus" => Some(Kind::Prometheus),
            "hubble" => Some(Kind::Hubble),
            "otel" => Some(Kind::Otel),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Service identification contract v1
// ---------------------------------------------------------------------------

/// Ports that identify a source on their own, regardless of naming.
const PROMETHEUS_PORT: i32 = 9090;
const OTLP_PORTS: [i32; 2] = [4317, 4318];

/// Every kind a `(Service, port)` pair matches.
///
/// Returns a `Vec` rather than the first hit on purpose: sidecar evaluates the
/// three rules independently and can emit one endpoint under two kinds (an
/// OTEL collector that also exposes 9090 is genuinely both). Matching that
/// exactly is what keeps the two implementations comparable.
pub(crate) fn classify(name: &str, labels: &BTreeMap<String, String>, port: i32) -> Vec<Kind> {
    let label = |k: &str| labels.get(k).map(String::as_str).unwrap_or("");
    let mut out = Vec::new();

    if name.starts_with("hubble-") || matches!(label("k8s-app"), "hubble-relay" | "hubble") {
        out.push(Kind::Hubble);
    }
    if label("app.kubernetes.io/name") == "prometheus"
        || label("operated-prometheus") == "true"
        || (name.contains("prometheus") && port == PROMETHEUS_PORT)
    {
        out.push(Kind::Prometheus);
    }
    if label("app.kubernetes.io/name").contains("opentelemetry")
        || name.contains("otel")
        || OTLP_PORTS.contains(&port)
    {
        out.push(Kind::Otel);
    }
    out
}

// ---------------------------------------------------------------------------
// The sidecar manifest (fast path)
// ---------------------------------------------------------------------------

/// Sidecar's payload, as much of it as we care about. Everything is optional
/// or defaulted: a manifest from a newer sidecar with extra fields must still
/// parse, and a field we cannot read must not lose us the whole cluster.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    #[serde(default)]
    schema_version: u64,
    #[serde(default)]
    components: Vec<ManifestComponent>,
    #[serde(default)]
    telemetry_sources: Vec<ManifestEndpoint>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestComponent {
    #[serde(default)]
    name: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    telemetry: String,
    #[serde(default)]
    chart_version: String,
    #[serde(default)]
    app_version: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEndpoint {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    service: String,
    #[serde(default)]
    port: i32,
    #[serde(default)]
    address: String,
}

/// Parse a manifest document into sources. Split out from the cluster read so
/// the shape contract is testable without a cluster.
pub(crate) fn sources_from_manifest(json: &str) -> Result<Vec<TelemetrySource>> {
    let m: Manifest = serde_json::from_str(json)?;
    if m.schema_version != SIDECAR_SCHEMA_SUPPORTED {
        anyhow::bail!(
            "sidecar manifest schemaVersion {} is not {SIDECAR_SCHEMA_SUPPORTED}",
            m.schema_version
        );
    }
    // component (namespace, kind) -> (name, version), so an endpoint can say
    // WHICH component serves it. Scanning can never recover this.
    let mut owners: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for c in &m.components {
        if c.telemetry.is_empty() {
            continue;
        }
        let version = if c.chart_version.is_empty() {
            c.app_version.clone()
        } else {
            c.chart_version.clone()
        };
        owners.insert(
            (c.namespace.clone(), c.telemetry.clone()),
            (c.name.clone(), version),
        );
    }

    let mut out = Vec::new();
    for e in m.telemetry_sources {
        if Kind::parse(&e.kind).is_none() {
            continue; // a kind a newer sidecar knows and this build does not
        }
        let owner = owners.get(&(e.namespace.clone(), e.kind.clone()));
        out.push(TelemetrySource {
            kind: e.kind,
            namespace: e.namespace,
            service: e.service,
            port: e.port,
            address: e.address,
            recipe: None, // filled in by `gate_recipes` once the catalog is known
            origin: ORIGIN_MANIFEST.to_string(),
            component: owner.map(|(n, _)| n.clone()),
            version: owner.map(|(_, v)| v.clone()).filter(|v| !v.is_empty()),
        });
    }
    Ok(out)
}

async fn from_manifest(client: &Client) -> Option<Vec<TelemetrySource>> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), SIDECAR_MANIFEST_NS);
    let cm = api.get_opt(SIDECAR_MANIFEST_NAME).await.ok()??;
    let mut data = cm.data?;
    let json = data.remove(SIDECAR_MANIFEST_KEY)?;
    match sources_from_manifest(&json) {
        Ok(v) => Some(v),
        Err(e) => {
            // Present but unreadable is a real signal — log it, then behave
            // exactly like a cluster that has no manifest at all.
            tracing::warn!("sidecar provisioning manifest is unusable: {e:#}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The scan (fallback — and the actual selling demo)
// ---------------------------------------------------------------------------

async fn from_scan(client: &Client) -> Vec<TelemetrySource> {
    let api: Api<Service> = Api::all(client.clone());
    let Ok(list) = api.list(&ListParams::default()).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for svc in list {
        let name = svc.metadata.name.clone().unwrap_or_default();
        let namespace = svc.metadata.namespace.clone().unwrap_or_default();
        let labels = svc.metadata.labels.clone().unwrap_or_default();
        let ports = svc.spec.and_then(|s| s.ports).unwrap_or_default();
        for p in ports {
            for kind in classify(&name, &labels, p.port) {
                out.push(TelemetrySource {
                    kind: kind.as_str().to_string(),
                    namespace: namespace.clone(),
                    service: name.clone(),
                    port: p.port,
                    address: format!("{name}.{namespace}.svc:{}", p.port),
                    recipe: None,
                    origin: ORIGIN_SCAN.to_string(),
                    component: None,
                    version: None,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Recipe gating
// ---------------------------------------------------------------------------

/// Which telemetry recipe ids this installation can actually install right
/// now — the intersection of the fixed per-kind ids and what the runtime
/// catalog carries. An unreachable registry yields the cached or bootstrap
/// catalog (ADR-047), so this degrades to "offer nothing" instead of failing.
async fn offerable_recipes() -> std::collections::BTreeSet<String> {
    let view = crate::catalog::view(None).await;
    let available: std::collections::BTreeSet<&str> =
        view.integrations.iter().map(|i| i.id.as_str()).collect();
    [Kind::Prometheus, Kind::Hubble, Kind::Otel]
        .into_iter()
        .map(Kind::recipe_id)
        .filter(|id| available.contains(id))
        .map(str::to_string)
        .collect()
}

/// Stamp each source with its recipe id, but only where the catalog can back
/// it. Split from the discovery so the gating rule is testable on its own.
pub(crate) fn gate_recipes(
    sources: &mut [TelemetrySource],
    offerable: &std::collections::BTreeSet<String>,
) {
    for s in sources.iter_mut() {
        s.recipe = Kind::parse(&s.kind)
            .map(Kind::recipe_id)
            .filter(|id| offerable.contains(*id))
            .map(str::to_string);
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Merge the manifest's sources with the scan's, manifest winning.
///
/// The scan still runs when the manifest is present: sidecar records what
/// *sidecar* installed, and an operator who added an OTEL collector afterwards
/// is exactly the person this feature is for. Identity is
/// `(kind, namespace, service, port)` — the same endpoint seen twice is one
/// source, and it keeps the manifest's provenance.
pub(crate) fn merge(
    manifest: Vec<TelemetrySource>,
    scan: Vec<TelemetrySource>,
) -> Vec<TelemetrySource> {
    let key = |s: &TelemetrySource| {
        (
            s.kind.clone(),
            s.namespace.clone(),
            s.service.clone(),
            s.port,
        )
    };
    let seen: std::collections::BTreeSet<_> = manifest.iter().map(key).collect();
    let mut out = manifest;
    out.extend(scan.into_iter().filter(|s| !seen.contains(&key(s))));
    out.sort_by_key(key);
    out
}

/// Everything this cluster already emits that VeloxSearch could ingest.
///
/// Never fails the caller: a cluster with no telemetry, no manifest, or no
/// permission to look returns an empty list, and `discover()` proceeds exactly
/// as it does today.
pub async fn discover(client: &Client) -> Vec<TelemetrySource> {
    let manifest = from_manifest(client).await.unwrap_or_default();
    let scan = from_scan(client).await;
    let mut merged = merge(manifest, scan);
    if !merged.is_empty() {
        gate_recipes(&mut merged, &offerable_recipes().await);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // --- contract v1: the positive cases -----------------------------------

    /// Each rule fires on the shape the real charts produce. Kept in lockstep
    /// with sidecar's `tests/validation/test_provisioning_manifest.py`.
    #[test]
    fn classify_recognises_the_three_kinds() {
        assert_eq!(
            classify("hubble-relay", &labels(&[("k8s-app", "hubble-relay")]), 80),
            vec![Kind::Hubble]
        );
        assert_eq!(
            classify("hubble-metrics", &labels(&[("k8s-app", "hubble")]), 9965),
            vec![Kind::Hubble]
        );
        assert_eq!(
            classify(
                "kube-prometheus-stack-prometheus",
                &labels(&[
                    ("app.kubernetes.io/name", "prometheus"),
                    ("operated-prometheus", "true")
                ]),
                9090
            ),
            vec![Kind::Prometheus]
        );
        assert_eq!(
            classify(
                "otel-collector",
                &labels(&[("app.kubernetes.io/name", "opentelemetry-collector")]),
                4317
            ),
            vec![Kind::Otel]
        );
    }

    /// Name-only and port-only matches, for clusters whose charts label
    /// nothing — the unprovisioned-cluster case.
    #[test]
    fn classify_falls_back_to_names_and_ports() {
        assert_eq!(
            classify("hubble-relay", &labels(&[]), 80),
            vec![Kind::Hubble]
        );
        assert_eq!(
            classify("my-prometheus", &labels(&[]), 9090),
            vec![Kind::Prometheus]
        );
        assert_eq!(classify("collector", &labels(&[]), 4318), vec![Kind::Otel]);
    }

    // --- contract v1: the near-misses that make it a contract --------------

    /// These are what stop the rules from being "anything with metrics in the
    /// name". A node-exporter is a scrape TARGET, not a telemetry source;
    /// Grafana renders telemetry, it does not serve it.
    #[test]
    fn classify_does_not_over_match() {
        /// (service name, labels, port) — a Service that must stay unclassified.
        type NearMiss = (&'static str, &'static [(&'static str, &'static str)], i32);
        let cases: &[NearMiss] = &[
            (
                "kube-prometheus-stack-prometheus-node-exporter",
                &[("app.kubernetes.io/name", "prometheus-node-exporter")],
                9100,
            ),
            (
                "kube-prometheus-stack-grafana",
                &[("app.kubernetes.io/name", "grafana")],
                80,
            ),
            ("kubernetes", &[("component", "apiserver")], 443),
            ("traefik", &[("app.kubernetes.io/name", "traefik")], 80),
            ("cilium-agent", &[("k8s-app", "cilium")], 9962),
        ];
        for (name, ls, port) in cases {
            assert!(
                classify(name, &labels(ls), *port).is_empty(),
                "{name}:{port} must not be classified as a telemetry source"
            );
        }
    }

    /// A prometheus-named Service on some other port is not a Prometheus API.
    #[test]
    fn classify_name_match_still_needs_the_port() {
        assert!(classify("my-prometheus", &labels(&[]), 3000).is_empty());
    }

    // --- the sidecar manifest ----------------------------------------------

    // Addresses in this fixture are RFC 5737 TEST-NET-1 (192.0.2.0/24), never a
    // real one. It was pasted from a live cluster originally and carried that
    // cluster's VIP into the tree; the OSS export's client-identifier gate
    // (tools/export/export-oss.sh) hard-fails on any RFC1918 or client address,
    // so a real IP here blocks the release. Nothing asserts on the value.
    const MANIFEST: &str = r#"{
      "schemaVersion": 1,
      "generator": {"product": "sidecar"},
      "cluster": {"name": "k3s-cluster", "distribution": "k3s",
                  "version": "v1.30.4+k3s1", "nodeCount": 3, "vip": "192.0.2.10"},
      "components": [
        {"name": "cilium", "namespace": "kube-system", "chart": "cilium",
         "chartVersion": "1.15.6", "appVersion": "1.15.6", "status": "deployed",
         "telemetry": "hubble", "endpoints": []},
        {"name": "longhorn", "namespace": "longhorn-system", "chart": "longhorn",
         "chartVersion": "1.6.2", "appVersion": "v1.6.2", "status": "deployed",
         "telemetry": "", "endpoints": []},
        {"name": "kube-prometheus-stack", "namespace": "monitoring",
         "chart": "kube-prometheus-stack", "chartVersion": "65.1.1",
         "appVersion": "v0.77.1", "status": "deployed",
         "telemetry": "prometheus", "endpoints": []}
      ],
      "telemetrySources": [
        {"kind": "hubble", "namespace": "kube-system", "service": "hubble-relay",
         "port": 80, "portName": "grpc", "protocol": "TCP",
         "address": "hubble-relay.kube-system.svc:80"},
        {"kind": "prometheus", "namespace": "monitoring",
         "service": "kube-prometheus-stack-prometheus", "port": 9090,
         "portName": "http-web", "protocol": "TCP",
         "address": "kube-prometheus-stack-prometheus.monitoring.svc:9090"}
      ]
    }"#;

    /// The seam: sidecar's payload becomes our sources, with the provenance a
    /// scan could never recover (which component, at which version).
    #[test]
    fn manifest_yields_sources_with_provenance() {
        let got = sources_from_manifest(MANIFEST).expect("manifest parses");
        assert_eq!(got.len(), 2);

        let hubble = got.iter().find(|s| s.kind == "hubble").unwrap();
        assert_eq!(hubble.service, "hubble-relay");
        assert_eq!(hubble.port, 80);
        assert_eq!(hubble.address, "hubble-relay.kube-system.svc:80");
        assert_eq!(hubble.component.as_deref(), Some("cilium"));
        assert_eq!(hubble.version.as_deref(), Some("1.15.6"));
        assert_eq!(hubble.origin, ORIGIN_MANIFEST);

        let prom = got.iter().find(|s| s.kind == "prometheus").unwrap();
        assert_eq!(prom.component.as_deref(), Some("kube-prometheus-stack"));
        assert_eq!(prom.version.as_deref(), Some("65.1.1"));
    }

    /// A schema major we do not understand is refused, not guessed at — the
    /// caller then behaves like a cluster with no manifest.
    #[test]
    fn manifest_refuses_an_unknown_schema() {
        let future = MANIFEST.replace("\"schemaVersion\": 1", "\"schemaVersion\": 2");
        assert!(sources_from_manifest(&future).is_err());
    }

    /// Forward compatibility the other way: unknown FIELDS are fine, and a
    /// telemetry kind a newer sidecar knows is skipped, not fatal.
    #[test]
    fn manifest_tolerates_newer_fields_and_kinds() {
        let extended = MANIFEST.replace(
            "\"telemetrySources\": [",
            "\"telemetrySources\": [
              {\"kind\": \"jaeger\", \"namespace\": \"tracing\", \"service\": \"jaeger\",
               \"port\": 14268, \"address\": \"jaeger.tracing.svc:14268\",
               \"someFutureField\": true},",
        );
        let got = sources_from_manifest(&extended).expect("still parses");
        assert_eq!(
            got.len(),
            2,
            "the unknown kind is skipped, the rest survive"
        );
        assert!(!got.iter().any(|s| s.kind == "jaeger"));
    }

    #[test]
    fn manifest_garbage_is_an_error_not_a_panic() {
        assert!(sources_from_manifest("not json").is_err());
        assert!(sources_from_manifest("{}").is_err()); // schemaVersion 0
    }

    // --- merge -------------------------------------------------------------

    fn src(kind: &str, ns: &str, svc: &str, port: i32, origin: &str) -> TelemetrySource {
        TelemetrySource {
            kind: kind.into(),
            namespace: ns.into(),
            service: svc.into(),
            port,
            address: format!("{svc}.{ns}.svc:{port}"),
            recipe: None,
            origin: origin.into(),
            component: None,
            version: None,
        }
    }

    /// The manifest is authoritative for what it names, and the scan still
    /// contributes what sidecar never installed — the OTEL collector an
    /// operator added afterwards is the whole point.
    #[test]
    fn merge_prefers_the_manifest_and_keeps_scan_extras() {
        let manifest = vec![src(
            "prometheus",
            "monitoring",
            "kps-prometheus",
            9090,
            ORIGIN_MANIFEST,
        )];
        let scan = vec![
            src(
                "prometheus",
                "monitoring",
                "kps-prometheus",
                9090,
                ORIGIN_SCAN,
            ),
            src("otel", "observability", "otel-collector", 4317, ORIGIN_SCAN),
        ];
        let got = merge(manifest, scan);
        assert_eq!(got.len(), 2, "the duplicate endpoint collapses to one");
        let prom = got.iter().find(|s| s.kind == "prometheus").unwrap();
        assert_eq!(prom.origin, ORIGIN_MANIFEST, "manifest provenance survives");
        assert!(got
            .iter()
            .any(|s| s.kind == "otel" && s.origin == ORIGIN_SCAN));
    }

    /// No manifest at all: the scan alone is the answer. This is the cluster
    /// VeloxSearch did not provision.
    #[test]
    fn merge_with_no_manifest_is_just_the_scan() {
        let scan = vec![src(
            "hubble",
            "kube-system",
            "hubble-relay",
            80,
            ORIGIN_SCAN,
        )];
        assert_eq!(merge(Vec::new(), scan.clone()), scan);
    }

    // --- the wire contract the wizard reads --------------------------------

    /// Response-shape discipline: `telemetry` is ADDITIVE. A payload from
    /// before this field existed must still deserialize, and mean "no
    /// telemetry" rather than fail — that is what keeps an older client and a
    /// newer server (or the reverse) compatible.
    #[test]
    fn discovery_stays_backward_compatible() {
        let old = r#"{"deployments":[],"detected":[]}"#;
        let d: crate::api::Discovery = serde_json::from_str(old).expect("old payload still parses");
        assert!(d.telemetry.is_empty());
    }

    /// The exact keys `frontend/views_create.jsx` reads off each source. A
    /// rename here silently empties the wizard's telemetry block, so pin it.
    #[test]
    fn telemetry_source_wire_shape_is_what_the_wizard_reads() {
        let mut s = src(
            "prometheus",
            "monitoring",
            "kps-prometheus",
            9090,
            ORIGIN_MANIFEST,
        );
        s.component = Some("kube-prometheus-stack".into());
        s.version = Some("65.1.1".into());
        s.recipe = Some("prometheus".into());
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "address",
                "component",
                "kind",
                "namespace",
                "origin",
                "port",
                "recipe",
                "service",
                "version"
            ]
        );
        assert_eq!(v["address"], "kps-prometheus.monitoring.svc:9090");
        assert_eq!(v["origin"], ORIGIN_MANIFEST);
    }

    // --- recipe gating -----------------------------------------------------

    /// A source is offered a recipe only when the catalog carries it. Today
    /// the registry publishes none of the three, so discovery reports the
    /// sources with `recipe: null` — visible, honest, not a broken promise.
    #[test]
    fn gate_recipes_offers_only_what_the_catalog_carries() {
        let mut sources = vec![
            src("prometheus", "monitoring", "p", 9090, ORIGIN_SCAN),
            src("hubble", "kube-system", "hubble-relay", 80, ORIGIN_SCAN),
        ];

        gate_recipes(&mut sources, &Default::default());
        assert!(sources.iter().all(|s| s.recipe.is_none()));

        let offerable = ["prometheus".to_string()].into_iter().collect();
        gate_recipes(&mut sources, &offerable);
        assert_eq!(sources[0].recipe.as_deref(), Some("prometheus"));
        assert_eq!(sources[1].recipe, None);
    }
}
