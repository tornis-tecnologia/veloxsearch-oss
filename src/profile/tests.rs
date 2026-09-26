// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! ADR-060 decision 4: the allowlist is enforced here, both ways, and every
//! excluded data class is seeded with a sentinel that must never come out.

use super::*;
use crate::api::{DeploymentFit, NodeCapacity, ResUse, StoragePool};
use serde_json::json;

/// 2026-09-14T12:00:00Z.
const T0: i64 = 1_789_387_200_000;
const MIN: i64 = 60_000;
const KEY: &str = "fixture-session-secret";

// Sentinels, one per excluded data class (ADR-060 decision 4).
const S_PASSWORD: &str = "SENTINEL-PASSWORD-hunter2";
const S_HOST: &str = "sentinel-node.example";
const S_HOST_2: &str = "sentinel-node-2.example";
const S_IP: &str = "203.0.113.7";
const S_INDEX: &str = "sentinel-index-7";
const S_TENANT_SLUG: &str = "sentinel-tenant-slug";
const S_DOC_BODY: &str = "SENTINEL DOCUMENT BODY";
const S_URL: &str = "https://sentinel-dashboards.example/app/home";
// Identities and free text that must never leave either.
const S_DEPLOYMENT: &str = "sentinel-deploy";
const S_NAMESPACE: &str = "velox-t-sentinel-ns";
const S_TENANT_ID: &str = "5c20e4aa-0000-4000-8000-00000000beef";
const S_STORAGE_CLASS: &str = "sentinel-storage-class";
const S_ROLE: &str = "sentinel-role";
const S_POD: &str = "sentinel-deploy-nodes-2";

/// Never in any document, whatever the mode.
const ALWAYS_ABSENT: &[&str] = &[
    S_PASSWORD,
    S_HOST,
    S_HOST_2,
    S_IP,
    S_DOC_BODY,
    S_URL,
    S_NAMESPACE,
    S_TENANT_ID,
    S_STORAGE_CLASS,
    S_ROLE,
    S_POD,
    KEY,
];

fn res(total: u64, used: u64, requested: Option<u64>) -> ResUse {
    ResUse {
        total,
        used: Some(used),
        requested,
    }
}

fn host_node(name: &str) -> NodeCapacity {
    NodeCapacity {
        name: name.to_string(),
        roles: vec!["control-plane".into(), S_ROLE.into()],
        ready: true,
        pressures: vec!["DiskPressure".into(), "SomethingNew".into()],
        kernel_version: "6.1.0-52-cloud-amd64".into(),
        cpu: res(4000, 1210, Some(2600)),
        mem: res(16_654_532_608, 9_102_190_592, Some(11_811_160_064)),
        host_disk: Some(res(107_374_182_400, 38_654_705_664, None)),
        storage: Some(res(85_899_345_920, 32_212_254_720, None)),
    }
}

fn os_node(i: u64) -> Value {
    json!({
        "name": format!("{S_DEPLOYMENT}-nodes-{i}"),
        "host": S_HOST,
        "ip": S_IP,
        "transport_address": format!("{S_IP}:9300"),
        "attributes": { "callback": S_URL },
        "roles": ["data", "cluster_manager"],
        "os": { "cpu": { "percent": 20 + i } },
        "jvm": {
            "mem": { "heap_used_percent": 60 + i, "heap_max_in_bytes": 1_610_612_736u64 },
            "gc": { "collectors": {
                "old": { "collection_time_in_millis": 4_000 },
                "young": { "collection_time_in_millis": 90_000 },
            }},
        },
        "fs": { "data": [{
            "path": "/usr/share/opensearch/data",
            "mount": format!("/mnt/{S_HOST}"),
            "total_in_bytes": 10_737_418_240u64,
            "available_in_bytes": 6_442_450_944u64 - i * 1_073_741_824,
        }]},
        "indices": {
            "docs": { "count": 4_001_437 },
            "store": { "size_in_bytes": 6_446_157_824u64 },
            "indexing": { "index_total": 900_000 },
            "search": { "query_total": 50_000 },
        },
    })
}

fn bucket(i: i64, cpu: f64, idx: u64, qry: Option<u64>, gc: Option<u64>, disk: u64) -> Value {
    let key = T0 - 7 * 24 * 60 * MIN + i * 5 * MIN;
    let v = |x: Option<u64>| json!({ "value": x });
    json!({
        "key": key,
        "doc_count": 5,
        "last_ts": { "value": key + 4 * MIN },
        "cpu": { "value": cpu },
        "heap": { "value": cpu + 30.0 },
        "disk": { "value": disk },
        "idx": { "value": idx },
        "qry": v(qry),
        "gc_old": v(gc),
        "gc_young": v(gc.map(|g| g * 20)),
    })
}

/// The maximal deployment: every `Option` set, every `Vec` non-empty, and
/// every raw source seeded with sentinels.
fn maximal_deployment() -> DeploymentInputs {
    let mut integrations = BTreeMap::new();
    integrations.insert("nginx".to_string(), "1.2.0".to_string());
    integrations.insert(S_HOST.to_string(), "1.0.0".to_string());
    DeploymentInputs {
        name: S_DEPLOYMENT.into(),
        namespace: S_NAMESPACE.into(),
        tenant_id: Some(S_TENANT_ID.into()),
        size: "medium".into(),
        purpose: "observability".into(),
        version: "3.8.0".into(),
        target_version: "3.9.0".into(),
        replicas: 3,
        memory: "3Gi".into(),
        heap: "1536m".into(),
        disk: "10Gi".into(),
        health: "yellow".into(),
        settled: false,
        stalled: true,
        provisioning_failed: true,
        current_stall: Some(StallInputs {
            at_ms: T0 - 30 * MIN,
            stage: "dashboards".into(),
            component: "Dashboards".into(),
            component_status: "Pending".into(),
            recovery_stage: "index".into(),
            recovery_index: format!("{S_INDEX}-000001"),
            secs: 1800,
            remediation: Some("dashboards_reset"),
        }),
        nodes_stats: Some(json!({
            "cluster_name": S_HOST,
            "nodes": { "a": os_node(0), "b": os_node(1), "c": os_node(2) },
        })),
        newest_sample: Some(json!({
            "@timestamp": T0 - MIN,
            "index_total": 2_700_000 - 24_612,
            "query_total": 150_000 - 750,
            "gc_old_millis": 12_000 - 40,
            "gc_young_millis": 270_000 - 600,
            "host": S_HOST,
        })),
        cat_indices: Some(json!([
            { "index": ".opendistro_security" },
            { "index": ".kibana_1" },
            { "index": format!("velox-metrics-{S_DEPLOYMENT}-000001") },
            { "index": "k8s-logs-000003" },
            { "index": "k8s-logs-000004" },
            { "index": S_INDEX },
            { "index": "app-orders-2026.09.13" },
            { "index": "app-orders-2026.09.14" },
        ])),
        cat_shards: Some(json!([
            { "prirep": "p", "state": "STARTED", "store": "2147483648", "node": S_HOST },
            { "prirep": "p", "state": "STARTED", "store": "1073741824", "index": S_INDEX },
            { "prirep": "r", "state": "STARTED", "store": "2147483648", "ip": S_IP },
            { "prirep": "r", "state": "UNASSIGNED", "store": null },
        ])),
        cluster_settings: Some(json!({
            "persistent": {
                "cluster.routing.allocation.disk.watermark.low": "90%",
                "s3.client.default.secret_key": S_PASSWORD,
                "cluster.remote.dr.seeds": [format!("{S_IP}:9300")],
            },
            "transient": {
                "cluster.routing.allocation.disk.watermark.flood_stage": "1gb",
            },
            "defaults": {
                "cluster.routing.allocation.disk.watermark.low": "85%",
                "cluster.routing.allocation.disk.watermark.high": "0.9",
                "node.name": S_HOST,
                "network.host": S_IP,
                "plugins.alerting.destination.url": S_URL,
            },
        })),
        window: Some(json!({
            "hits": { "hits": [{ "_source": { "message": S_DOC_BODY } }] },
            "aggregations": { "b": { "buckets": [
                // A pre-ADR-060 bucket: no query/GC counters yet.
                bucket(0, 10.0, 1_000_000, None, None, 17_179_869_184),
                bucket(1, 20.0, 1_120_000, Some(100_000), Some(1_000), 17_300_000_000),
                bucket(2, 70.0, 1_480_000, Some(112_000), Some(1_090), 18_000_000_000),
                bucket(3, 18.0, 1_600_000, Some(115_000), Some(1_100), 19_338_473_472),
            ]}},
        })),
        stall_docs: vec![json!({
            "@timestamp": T0 - 2 * 24 * 60 * MIN,
            "kind": "stall",
            "stage": "nodes",
            "component": "RollingRestart",
            "component_status": "Running",
            "recovery_stage": "init",
            "recovery_index_class": "system",
            "recovery_secs": 57_240,
            "remediation": "node_bounce",
            "pod": S_POD,
            "message": S_DOC_BODY,
        })],
        restarts: Some((2, 0)),
        integrations,
    }
}

/// A second, admin-namespace deployment that has not answered yet — the
/// all-`null` shape a fresh deployment produces.
fn quiet_deployment() -> DeploymentInputs {
    DeploymentInputs {
        name: "logs".into(),
        namespace: "veloxsearch".into(),
        size: "small".into(),
        purpose: "search".into(),
        version: "3.8.0".into(),
        replicas: 3,
        memory: "2Gi".into(),
        heap: "1g".into(),
        disk: "5Gi".into(),
        health: "green".into(),
        settled: true,
        ..DeploymentInputs::default()
    }
}

fn inputs(admin: bool) -> ProfileInputs {
    let mut slugs = BTreeMap::new();
    slugs.insert(S_TENANT_ID.to_string(), S_TENANT_SLUG.to_string());
    ProfileInputs {
        generated_at_ms: T0,
        admin,
        build: BuildInputs {
            version: "0.10.5".into(),
            commit: "9897468".into(),
            image_digest: Some(format!("sha256:{}", "ab".repeat(32))),
            operator_image: Some(format!("{S_HOST}:5000/opensearch-operator:2.8.0")),
        },
        // Supplied for BOTH scopes on purpose: the builder must drop them for
        // a tenant even when a caller hands them over.
        installation: Some(InstallationInputs {
            apiserver_version: Some("v1.36.3+k3s1".into()),
            capacity: Some(ClusterCapacity {
                nodes: vec![host_node(S_HOST), host_node(S_HOST_2)],
                storage: Some(StoragePool {
                    total: 257_698_037_760,
                    used: 96_636_764_160,
                    available: 161_061_273_600,
                }),
                fit: vec![DeploymentFit {
                    size: "small".into(),
                    count: 2,
                    limited_by: "mem".into(),
                }],
                ..ClusterCapacity::default()
            }),
            storage: Some(DeploymentStorage::ForeignDefault(S_STORAGE_CLASS.into())),
            longhorn_replicas: Some(3),
        }),
        quota: Some(crate::k8s::TenantQuota {
            max_deployments: 3,
            max_total_disk_gb: 100,
            max_nodes: 9,
        }),
        tenant_slugs: slugs,
        deployments: vec![maximal_deployment(), quiet_deployment()],
    }
}

fn profile(admin: bool, names: bool) -> Value {
    serde_json::to_value(build(inputs(admin), &Redaction::new(KEY, names))).unwrap()
}

/// Every leaf path, arrays as `[]`. A `null` standing for an absent object
/// (`"now": null`) is the parent of listed leaves, not a leaf of its own —
/// it carries no data, so it counts only if it is not such a parent.
fn leaves(v: &Value, path: &str, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => m
            .iter()
            .for_each(|(k, x)| leaves(x, &format!("{path}/{k}"), out)),
        Value::Array(a) => a.iter().for_each(|x| leaves(x, &format!("{path}/[]"), out)),
        Value::Null
            if FIELDS
                .iter()
                .any(|f| f.path.starts_with(&format!("{path}/"))) => {}
        _ => {
            out.insert(path.to_string());
        }
    }
}

fn leaf_set(v: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    leaves(v, "", &mut out);
    out
}

fn fields() -> BTreeSet<String> {
    FIELDS.iter().map(|f| f.path.to_string()).collect()
}

// ───────────────────────────── the two CI gates ─────────────────────────────

/// Exact set, both ways: an unlisted field fails, and so does a listed field
/// nothing emits. The two scopes are complementary (installation sections vs.
/// the quota row), so the maximal set is their union with names on.
#[test]
fn exact_set_emitted_leaves_equal_fields() {
    assert_eq!(FIELDS.len(), fields().len(), "FIELDS has a duplicate path");
    let mut emitted = leaf_set(&profile(true, true));
    emitted.extend(leaf_set(&profile(false, true)));
    let listed = fields();
    let unlisted: Vec<_> = emitted.difference(&listed).collect();
    let never: Vec<_> = listed.difference(&emitted).collect();
    assert!(
        unlisted.is_empty(),
        "emitted but not in FIELDS: {unlisted:?}"
    );
    assert!(never.is_empty(), "in FIELDS but never emitted: {never:?}");
    // And no mode emits anything outside the table.
    for (admin, names) in [(true, false), (false, false)] {
        let extra: Vec<_> = leaf_set(&profile(admin, names))
            .into_iter()
            .filter(|p| !listed.contains(p))
            .collect();
        assert!(extra.is_empty(), "admin={admin} names={names}: {extra:?}");
    }
    assert!(FIELDS.iter().all(|f| !f.note.is_empty()));
}

/// Canary (the `otel_stack::password_only_in_secret` shape): no excluded
/// sentinel survives in any mode; with names on, exactly the name sentinels
/// appear — the tenant slug only for the installation admin.
#[test]
fn canary_no_excluded_sentinel_leaves() {
    for admin in [true, false] {
        for names in [false, true] {
            let text = serde_json::to_string(&profile(admin, names)).unwrap();
            for s in ALWAYS_ABSENT {
                assert!(!text.contains(s), "admin={admin} names={names}: leaked {s}");
            }
            let named = [S_DEPLOYMENT, S_INDEX];
            for s in named {
                assert_eq!(text.contains(s), names, "admin={admin} names={names}: {s}");
            }
            assert_eq!(
                text.contains(S_TENANT_SLUG),
                names && admin,
                "admin={admin} names={names}: the tenant slug"
            );
        }
    }
}

// ───────────────────────────── scope ─────────────────────────────

#[test]
fn a_tenant_document_has_its_quota_and_no_installation_sections() {
    let t = profile(false, true);
    for key in ["kubernetes", "nodes", "storage", "fit"] {
        assert!(t.get(key).is_none(), "{key} must be absent, not null");
    }
    assert_eq!(t["scope"], "tenant");
    assert_eq!(t["quota"]["max_total_disk_bytes"], 100u64 << 30);
    assert_eq!(t["build"]["image_digest"], Value::Null);
    assert_eq!(t["build"]["operator_version"], Value::Null);
    // Host-derived warnings are installation facts too.
    let warnings = t["deployments"][0]["warnings"].to_string();
    for code in [
        "storage_single_copy",
        "node_pressure",
        "kernel_incompatible",
    ] {
        assert!(!warnings.contains(code), "tenant sees host warning {code}");
    }

    let a = profile(true, false);
    assert_eq!(a["scope"], "installation");
    assert!(a.get("quota").is_none());
    assert_eq!(a["kubernetes"]["distribution"], "k3s");
    assert_eq!(a["storage"]["class"], "foreign_default");
    assert_eq!(a["build"]["operator_version"], "2.8.0");
}

// ───────────────────────────── redaction ─────────────────────────────

#[test]
fn pseudonyms_are_stable_keyed_and_distinct() {
    let r = Redaction::new(KEY, false);
    assert_eq!(r.pseudonym("n", S_HOST), r.pseudonym("n", S_HOST));
    assert_ne!(r.pseudonym("n", S_HOST), r.pseudonym("n", S_HOST_2));
    assert_ne!(
        r.pseudonym("n", S_HOST),
        Redaction::new("another-secret", false).pseudonym("n", S_HOST),
        "rotating the secret changes every id"
    );
    let id = r.pseudonym("d", "a/b");
    assert!(id.starts_with("d-") && id.len() == 2 + PSEUDONYM_HEX);
    // Same name in two namespaces is two deployments.
    assert_ne!(r.pseudonym("d", "ns-a/logs"), r.pseudonym("d", "ns-b/logs"));
}

#[test]
fn two_builds_of_the_same_inputs_are_identical_and_sorted() {
    let (a, b) = (profile(true, false), profile(true, false));
    assert_eq!(a, b);
    let ids: Vec<&str> = a["deployments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted);
}

// ───────────────────────────── reductions ─────────────────────────────

#[test]
fn index_names_reduce_to_classes_and_families() {
    assert_eq!(index_family("k8s-logs-000003"), "k8s-logs");
    assert_eq!(index_family("app-orders-2026.09.14"), "app-orders");
    assert_eq!(index_family("app-orders-2026-09-14-000002"), "app-orders");
    assert_eq!(index_family("metrics_2026.09"), "metrics");
    assert_eq!(index_family(".ds-logs-2026.09.14-000001"), ".ds-logs");
    assert_eq!(
        index_family(S_INDEX),
        S_INDEX,
        "a short digit suffix is a name"
    );
    assert_eq!(index_family("2026.09.14"), "2026.09.14");
    assert_eq!(index_class(".kibana_1"), "system");
    assert_eq!(index_class("velox-metrics-x-000001"), "managed");
    assert_eq!(index_class("k8s-logs-000003"), "managed");
    assert_eq!(index_class("otel-v1-apm-span-000001"), "managed");
    assert_eq!(index_class("app-orders"), "user");

    let p = profile(true, false);
    let d = &p["deployments"][0];
    let i = if d["indices"].is_null() {
        &p["deployments"][1]["indices"]
    } else {
        &d["indices"]
    };
    assert_eq!(i["total"], 8);
    assert_eq!(i["families"], 6);
    assert_eq!(
        i["by_class"],
        json!({ "system": 2, "managed": 3, "user": 3 })
    );
    assert!(i.get("family_names").is_none());
}

fn maximal(p: &Value) -> &Value {
    p["deployments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| !d["indices"].is_null())
        .unwrap()
}

#[test]
fn shards_watermarks_and_live_numbers() {
    let p = profile(true, false);
    let d = maximal(&p);
    assert_eq!(
        d["shards"],
        json!({ "primaries": 2, "total": 4, "unassigned": 1, "largest_primary_bytes": 2147483648u64 })
    );
    // Tightest node: 10 GiB total, 4 GiB free. low 90% (persistent beats the
    // default) → 1 GiB must stay free; high 0.9 → same; flood 1gb (transient).
    let gib = 1_073_741_824i64;
    assert_eq!(
        d["disk_watermark_headroom_bytes"],
        json!({ "low": 4 * gib - gib, "high": 4 * gib - gib, "flood_stage": 3 * gib })
    );
    assert_eq!(d["heap_declared_bytes"], 1_610_612_736u64);
    assert_eq!(d["heap_max_bytes_observed"], 1_610_612_736u64);
    assert_eq!(d["sizing"], "preset");
    assert_eq!(d["now"]["cpu_percent"], 21.0);
    assert_eq!(d["now"]["indexing_per_sec"], 410.2);
    assert_eq!(d["now"]["query_per_sec"], 12.5);
    assert_eq!(d["now"]["gc_old_millis_per_min"], 40.0);
    assert_eq!(
        d["restarts"],
        json!({ "node_containers": 2, "dashboards": 0 })
    );
    assert_eq!(
        d["integrations"],
        json!([{ "id": "nginx", "version": "1.2.0" }])
    );
}

#[test]
fn window_states_its_coverage_and_skips_fields_old_samples_lack() {
    let p = profile(true, false);
    let w = &maximal(&p)["window"];
    assert_eq!(w["samples"], 20);
    assert_eq!(w["bucket_secs"], 300);
    assert_eq!(w["start"], "2026-09-07T12:00:00Z");
    assert_eq!(w["end"], "2026-09-07T12:19:00Z");
    assert_eq!(w["cpu_percent"], json!({ "p50": 18.0, "p95": 70.0 }));
    // Four buckets → three indexing rates (the first bucket has no delta).
    // 120k, 360k, 120k over 300s.
    assert_eq!(
        w["indexing_per_sec"],
        json!({ "p50": 400.0, "p95": 1200.0 })
    );
    // Only the three buckets carrying query_total count: 12k then 3k.
    assert_eq!(w["query_per_sec"], json!({ "p50": 10.0, "p95": 40.0 }));
    assert_eq!(
        w["disk_used_bytes"],
        json!({ "first": 17179869184u64, "last": 19338473472u64 })
    );

    let quiet = &p["deployments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["indices"].is_null())
        .unwrap()["window"];
    assert_eq!(quiet["samples"], 0);
    assert_eq!(quiet["start"], Value::Null);
    assert_eq!(quiet["cpu_percent"], Value::Null);
}

#[test]
fn a_now_rate_needs_a_recent_comparable_sample() {
    let live = Sample {
        ts: T0,
        index_total: 1_000,
        query_total: 10,
        ..Sample::default()
    };
    let stale = json!({ "@timestamp": T0 - 60 * MIN, "index_total": 0, "query_total": 0 });
    assert_eq!(now_of(&live, Some(&stale)).indexing_per_sec, None);
    // A pre-ADR-060 sample has no query_total: unknown, not a huge rate.
    let old = json!({ "@timestamp": T0 - MIN, "index_total": 400 });
    let n = now_of(&live, Some(&old));
    assert_eq!(n.indexing_per_sec, Some(10.0));
    assert_eq!(n.query_per_sec, None);
    // Counter reset (node restart) clamps to zero, as `downsample` does.
    let reset = json!({ "@timestamp": T0 - MIN, "index_total": 5_000 });
    assert_eq!(now_of(&live, Some(&reset)).indexing_per_sec, Some(0.0));
}

#[test]
fn stalls_merge_recorded_episodes_with_the_current_one() {
    let p = profile(true, false);
    let stalls = maximal(&p)["stalls"].as_array().unwrap().clone();
    assert_eq!(stalls.len(), 2);
    assert_eq!(stalls[0]["stage"], "nodes");
    assert_eq!(stalls[0]["remediation"], "node_bounce");
    assert_eq!(stalls[1]["stage"], "dashboards");
    assert_eq!(stalls[1]["recovery_index_class"], "user");

    // A current stall already recorded is not reported twice.
    let mut d = maximal_deployment();
    d.current_stall.as_mut().unwrap().stage = "nodes".into();
    d.current_stall.as_mut().unwrap().component = "RollingRestart".into();
    assert_eq!(stalls_of(&d, T0).len(), 1);
}

#[test]
fn host_warnings_are_the_wizard_rules_as_codes() {
    assert!(host_warnings(&[], None).is_empty(), "silent when unknown");
    let one = [host_node(S_HOST)];
    assert_eq!(
        host_warnings(&one, Some(&DeploymentStorage::NodeLocal("x".into()))),
        vec!["storage_single_copy", "node_pressure", "storage_node_local"]
    );
    let mut healthy = host_node(S_HOST);
    healthy.pressures.clear();
    let three = [healthy.clone(), healthy.clone(), healthy];
    assert!(host_warnings(&three, Some(&DeploymentStorage::Longhorn { default: true })).is_empty());
    assert!(kernel_incompatible(&one, "3.8.0"));
    assert!(!kernel_incompatible(&one, "3.7.0"));
}

#[test]
fn tokens_refuse_anything_that_is_not_their_shape() {
    assert_eq!(
        version_token("v1.36.3+k3s1").as_deref(),
        Some("v1.36.3+k3s1")
    );
    assert_eq!(version_token(S_HOST), None);
    assert_eq!(version_token(S_URL), None);
    assert_eq!(
        word_token("RollingRestart").as_deref(),
        Some("RollingRestart")
    );
    assert_eq!(word_token(S_INDEX), None);
    assert_eq!(
        image_tag("reg.example:5000/op/opensearch-operator:2.8.0").as_deref(),
        Some("2.8.0")
    );
    assert_eq!(image_tag("reg.example:5000/op/opensearch-operator"), None);
    assert_eq!(jvm_bytes("-Xms1g -Xmx2g"), Some(2 << 30));
    assert_eq!(commit_token("not a sha"), "unknown");
    assert_eq!(percentiles(vec![]), None);
    assert_eq!(
        percentiles((1..=100).map(f64::from).collect()),
        Some(Percentiles {
            p50: 50.0,
            p95: 95.0
        })
    );
}

/// Writes the redacted fixture profiles (the files downstream consumers build
/// against): `VELOX_PROFILE_SAMPLE_DIR=<dir> cargo test write_sample -- --ignored`.
/// A no-op without the variable, so the CI `--ignored` pass skips it.
#[test]
#[ignore]
fn write_sample_profiles() {
    let Ok(dir) = std::env::var("VELOX_PROFILE_SAMPLE_DIR") else {
        return;
    };
    for (file, admin) in [
        ("sample-profile.json", true),
        ("sample-profile-tenant.json", false),
    ] {
        let p = build(inputs(admin), &Redaction::new(KEY, false));
        let text = serde_json::to_string_pretty(&p).unwrap() + "\n";
        std::fs::write(format!("{dir}/{file}"), text).unwrap();
    }
}
