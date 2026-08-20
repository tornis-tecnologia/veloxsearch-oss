// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Per-node cluster metrics from OpenSearch `_nodes/stats` (meeting 3, jun 12).
//!
//! Feeds the Overview's visual node blocks — per-node health, CPU, JVM heap and
//! disk — and seeds indexing volumetry. One source, authenticated via
//! `k8s::admin_creds`, so no extra Kubernetes RBAC (metrics-server) is needed.

use crate::api::{ClusterMetrics, MetricPoint, MetricSeries, NodeStat};
use crate::recipes::{http, os_base};
use crate::scope::Deployment;
use anyhow::{Context, Result};
use serde_json::json;

/// Query `_nodes/stats` and shape it into the per-node blocks the Overview
/// renders. Cluster totals are summed across nodes (shard-level, so docs
/// include replicas — fine for an at-a-glance figure; disk is genuine usage).
pub async fn node_stats(deployment: &Deployment) -> Result<ClusterMetrics> {
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let body: serde_json::Value = c
        .get(format!("{base}/_nodes/stats/os,jvm,fs,indices"))
        .basic_auth(&u, Some(&p))
        .send()
        .await
        .context("querying _nodes/stats")?
        .error_for_status()
        .context("_nodes/stats rejected")?
        .json()
        .await
        .context("parsing _nodes/stats")?;

    // PVC binding state per node data-volume (ADR-031): an unbound volume must
    // read as Pending, never a zeroed/node-sized meter. Best-effort — an empty
    // map degrades to the `fs.data` totals below.
    let pvcs = crate::k8s::data_pvcs(deployment).await;

    let mut nodes = Vec::new();
    let mut total_docs = 0u64;
    let mut store_size_bytes = 0u64;

    if let Some(map) = body.get("nodes").and_then(|n| n.as_object()) {
        for n in map.values() {
            // Tolerant nested getters: a missing path is 0, never an error —
            // a node mid-start simply shows zeroed meters rather than failing
            // the whole Overview.
            let getu = |path: &[&str]| -> u64 {
                let mut cur = n;
                for k in path {
                    match cur.get(k) {
                        Some(v) => cur = v,
                        None => return 0,
                    }
                }
                cur.as_u64().unwrap_or(0)
            };
            let geti = |path: &[&str]| -> i64 {
                let mut cur = n;
                for k in path {
                    match cur.get(k) {
                        Some(v) => cur = v,
                        None => return 0,
                    }
                }
                cur.as_i64().unwrap_or(0)
            };

            let docs = getu(&["indices", "docs", "count"]);
            total_docs += docs;
            store_size_bytes += getu(&["indices", "store", "size_in_bytes"]);

            // Disk = the data-path mount (`fs.data[]`), NOT the node fs
            // (`fs.total`) — that is the deployment's PVC per ADR-031. Sum
            // across data paths (normally one, the PVC); absent when a node is
            // mid-start, in which case the PVC phase below reads it as Pending
            // instead of a misleading node-sized bar.
            let (disk_total_bytes, disk_available_bytes) = {
                let (mut t, mut a) = (0u64, 0u64);
                if let Some(arr) = n.pointer("/fs/data").and_then(|v| v.as_array()) {
                    for d in arr {
                        t += d
                            .get("total_in_bytes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        a += d
                            .get("available_in_bytes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                }
                (t, a)
            };

            let name = n
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // The node's own PVC: STS volumeClaimTemplate `data` + pod name
            // (= OpenSearch node name) → `data-<node.name>`.
            let pvc = pvcs.get(&format!("data-{name}"));
            let pvc_phase = pvc.map(|p| p.phase.clone()).unwrap_or_default();
            let pvc_capacity_bytes = pvc.map(|p| p.capacity_bytes).unwrap_or(0);

            nodes.push(NodeStat {
                name,
                roles: n
                    .get("roles")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                cpu_percent: geti(&["os", "cpu", "percent"]),
                heap_percent: geti(&["jvm", "mem", "heap_used_percent"]),
                heap_used_bytes: getu(&["jvm", "mem", "heap_used_in_bytes"]),
                heap_max_bytes: getu(&["jvm", "mem", "heap_max_in_bytes"]),
                disk_total_bytes,
                disk_available_bytes,
                pvc_phase,
                pvc_capacity_bytes,
                docs,
            });
        }
    }
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ClusterMetrics {
        nodes,
        total_docs,
        store_size_bytes,
    })
}

// ───────────────────────── time-series ("second moment", #9) ───────────────
//
// The Overview above renders POINT-IN-TIME values. This section adds the
// time-series: a lightweight background sampler folds `_nodes/stats` into one
// cluster-aggregate `Sample` per tick and appends it to a dedicated, BOUNDED
// `velox-metrics-<deployment>` index (rollover → delete via ISM, mirroring the
// retention pattern in `profiles.rs` without touching that file). The read path
// (`series`) pulls the recent raw samples and downsamples them in Rust — kept a
// pure function (`downsample`) so the bucketing/DTO logic is unit-testable with
// no live OpenSearch.
//
// Single-source, OpenSearch-native by design: no Prometheus, no extra pod. The
// app already authenticates to each deployment's OpenSearch via `admin_creds`.

/// Shared ISM policy attached (via `ism_template`) to every `velox-metrics-*`
/// index: roll over a backing index, then drop it once it ages out — so the
/// series can never grow unbounded.
const METRICS_POLICY_ID: &str = "velox-metrics-retention";
/// Roll the write index over once it reaches either bound, then delete the
/// rolled index at `METRICS_RETENTION_AGE`. Small numbers: a health series is
/// low-volume and disposable.
const METRICS_ROLLOVER_AGE: &str = "1d";
const METRICS_ROLLOVER_SIZE: &str = "256mb";
const METRICS_RETENTION_AGE: &str = "7d";

/// The rollover alias every sample is written through (one write index behind
/// it). Backing indices are `<alias>-NNNNNN`.
fn metrics_alias(deployment: &Deployment) -> String {
    format!("velox-metrics-{deployment}")
}

/// One recorded cluster-aggregate sample. Mirrors a document in the metrics
/// index and the shape `downsample` folds; `index_total` is OpenSearch's
/// monotonic cumulative indexing-op counter, the basis for the indexing rate.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// Epoch milliseconds (UTC).
    pub ts: i64,
    pub cpu_percent: f64,
    pub heap_percent: f64,
    pub disk_used_bytes: u64,
    pub docs: u64,
    pub index_total: u64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Background sampler: every interval, append one aggregate sample per
/// deployment. Best-effort throughout — a not-yet-green cluster (OpenSearch not
/// answering) is logged at debug and retried next tick, never fatal. Spawned
/// once at startup (`main`).
pub async fn run_sampler() {
    let secs = std::env::var("VELOX_METRICS_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .max(5);
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(secs));
    tracing::info!("cluster-health sampler started (every {secs}s)");
    loop {
        tick.tick().await;
        // The sampler is platform work, not a request: it runs as the
        // installation admin (the super-tenant), which is the one scope that
        // legitimately covers every tenant's deployments. It samples what it
        // just listed — it never takes a name from anywhere else.
        let deployments = match crate::k8s::scoped_deployments(&crate::scope::Scope::Admin).await {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!("sampler: listing deployments: {e:#}");
                continue;
            }
        };
        for dep in deployments {
            if let Err(e) = sample_once(&dep).await {
                tracing::debug!("sampler: {dep}: {e:#}");
            }
        }
    }
}

/// Ensure storage, take one sample, record it. Each step best-effort upstream.
async fn sample_once(deployment: &Deployment) -> Result<()> {
    ensure_metrics_index(deployment).await?;
    let sample = collect_sample(deployment).await?;
    record_sample(deployment, &sample).await
}

/// Fold `_nodes/stats` into one cluster-aggregate sample: CPU/heap are meaned
/// across nodes, disk used / docs / indexing ops are summed.
async fn collect_sample(deployment: &Deployment) -> Result<Sample> {
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let body: serde_json::Value = c
        .get(format!("{base}/_nodes/stats/os,jvm,fs,indices"))
        .basic_auth(&u, Some(&p))
        .send()
        .await
        .context("sampling _nodes/stats")?
        .error_for_status()
        .context("_nodes/stats rejected")?
        .json()
        .await
        .context("parsing _nodes/stats")?;

    let (mut cpu_sum, mut heap_sum) = (0i64, 0i64);
    let (mut n, mut disk_used, mut docs, mut index_total) = (0u64, 0u64, 0u64, 0u64);
    if let Some(map) = body.get("nodes").and_then(|v| v.as_object()) {
        for node in map.values() {
            n += 1;
            cpu_sum += node
                .pointer("/os/cpu/percent")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            heap_sum += node
                .pointer("/jvm/mem/heap_used_percent")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            docs += node
                .pointer("/indices/docs/count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            index_total += node
                .pointer("/indices/indexing/index_total")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // Disk used = data-path total − available (ADR-031: the PVC mount,
            // not the node fs). Summed across nodes for a cluster figure.
            if let Some(arr) = node.pointer("/fs/data").and_then(|v| v.as_array()) {
                for d in arr {
                    let t = d
                        .get("total_in_bytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let a = d
                        .get("available_in_bytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    disk_used += t.saturating_sub(a);
                }
            }
        }
    }
    let nf = n.max(1) as f64;
    Ok(Sample {
        ts: now_ms(),
        cpu_percent: cpu_sum as f64 / nf,
        heap_percent: heap_sum as f64 / nf,
        disk_used_bytes: disk_used,
        docs,
        index_total,
    })
}

/// Append a sample through the rollover alias. Dynamic mapping is pinned by the
/// index template `ensure_metrics_index` installs.
async fn record_sample(deployment: &Deployment, s: &Sample) -> Result<()> {
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let alias = metrics_alias(deployment);
    c.post(format!("{base}/{alias}/_doc"))
        .basic_auth(&u, Some(&p))
        .json(&json!({
            "@timestamp": s.ts,
            "cpu_percent": s.cpu_percent,
            "heap_percent": s.heap_percent,
            "disk_used_bytes": s.disk_used_bytes,
            "docs": s.docs,
            "index_total": s.index_total,
        }))
        .send()
        .await
        .context("recording metrics sample")?
        .error_for_status()
        .context("metrics sample rejected")?;
    Ok(())
}

/// Idempotently provision bounded storage for a deployment's series:
///   1. the shared rollover→delete ISM policy (created once),
///   2. a per-deployment index template (rollover alias + typed mappings),
///   3. the bootstrap write index behind the alias.
///
/// Mirrors `profiles::ensure_retention` (we must not edit `profiles.rs`).
async fn ensure_metrics_index(deployment: &Deployment) -> Result<()> {
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let alias = metrics_alias(deployment);

    // (1) Shared policy — create only when absent (a 404 GET). We never need to
    // mutate it per tick, so we skip the seq_no/primary_term update dance.
    let policy_url = format!("{base}/_plugins/_ism/policies/{METRICS_POLICY_ID}");
    let exists = c
        .get(&policy_url)
        .basic_auth(&u, Some(&p))
        .send()
        .await
        .context("checking metrics ISM policy")?
        .status()
        .is_success();
    if !exists {
        // Best-effort: a concurrent create (another deployment's first tick)
        // races to the same shared id and 409s — harmless, the policy exists.
        let _ = c
            .put(&policy_url)
            .basic_auth(&u, Some(&p))
            .json(&metrics_policy())
            .send()
            .await;
    }

    // (2) Per-deployment index template — idempotent PUT.
    c.put(format!("{base}/_index_template/velox-metrics-{deployment}"))
        .basic_auth(&u, Some(&p))
        .json(&json!({
            "index_patterns": [format!("{alias}-*")],
            "template": {
                "settings": {
                    // A health series is disposable; no replicas keeps it cheap.
                    "index.number_of_replicas": 0,
                    "plugins.index_state_management.rollover_alias": alias,
                },
                "mappings": { "properties": {
                    "@timestamp": { "type": "date", "format": "epoch_millis" },
                    "cpu_percent": { "type": "float" },
                    "heap_percent": { "type": "float" },
                    "disk_used_bytes": { "type": "long" },
                    "docs": { "type": "long" },
                    "index_total": { "type": "long" },
                }},
            },
        }))
        .send()
        .await
        .context("installing metrics index template")?
        .error_for_status()
        .context("metrics index template rejected")?;

    // (3) Bootstrap the write index once. The alias must carry exactly one
    // `is_write_index` for both indexing and ISM rollover to target it.
    let has_alias = c
        .get(format!("{base}/_alias/{alias}"))
        .basic_auth(&u, Some(&p))
        .send()
        .await
        .context("checking metrics alias")?
        .status()
        .is_success();
    if !has_alias {
        let _ = c
            .put(format!("{base}/{alias}-000001"))
            .basic_auth(&u, Some(&p))
            .json(&json!({ "aliases": { alias.clone(): { "is_write_index": true } } }))
            .send()
            .await;
    }
    Ok(())
}

/// The rollover→delete ISM policy document. Shared across deployments; its
/// `ism_template` auto-attaches it to any `velox-metrics-*` index.
fn metrics_policy() -> serde_json::Value {
    json!({
        "policy": {
            "description": "VeloxSearch cluster-health metrics retention: rollover → delete",
            "default_state": "hot",
            "states": [
                { "name": "hot",
                  "actions": [{ "rollover": {
                      "min_index_age": METRICS_ROLLOVER_AGE,
                      "min_primary_shard_size": METRICS_ROLLOVER_SIZE
                  }}],
                  "transitions": [{ "state_name": "delete",
                      "conditions": { "min_index_age": METRICS_RETENTION_AGE } }] },
                { "name": "delete", "actions": [{ "delete": {} }], "transitions": [] }
            ],
            "ism_template": [{ "index_patterns": ["velox-metrics-*"], "priority": 100 }]
        }
    })
}

/// Recent cluster-health series for a deployment: pull the raw samples in the
/// look-back window (sorted, capped) and downsample them into `buckets`
/// time-buckets. Best-effort — a missing index / unreachable cluster surfaces
/// as an error the API layer degrades to an empty series.
pub async fn series(
    deployment: &Deployment,
    window_minutes: i64,
    buckets: usize,
) -> Result<MetricSeries> {
    let window_minutes = window_minutes.clamp(1, 7 * 24 * 60);
    let buckets = buckets.clamp(1, 1000);
    let base = os_base(deployment);
    let c = http()?;
    let (u, p) = crate::k8s::admin_creds(deployment).await;
    let alias = metrics_alias(deployment);

    let body: serde_json::Value = c
        .post(format!("{base}/{alias}/_search"))
        .basic_auth(&u, Some(&p))
        .json(&json!({
            // Bounded: at a 60s cadence a 7-day window is ~10k points; the cap
            // keeps a misconfiguration from pulling an unbounded result set.
            "size": 10_000,
            "sort": [{ "@timestamp": "asc" }],
            "_source": ["@timestamp", "cpu_percent", "heap_percent",
                        "disk_used_bytes", "docs", "index_total"],
            "query": { "range": { "@timestamp": {
                "gte": format!("now-{window_minutes}m"), "format": "epoch_millis"
            }}},
        }))
        .send()
        .await
        .context("querying metrics series")?
        .error_for_status()
        .context("metrics series query rejected")?
        .json()
        .await
        .context("parsing metrics series")?;

    let mut samples: Vec<Sample> = Vec::new();
    if let Some(hits) = body.pointer("/hits/hits").and_then(|v| v.as_array()) {
        for h in hits {
            let src = match h.get("_source") {
                Some(s) => s,
                None => continue,
            };
            let f = |k: &str| src.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let i = |k: &str| src.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
            let uu = |k: &str| src.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            samples.push(Sample {
                ts: i("@timestamp"),
                cpu_percent: f("cpu_percent"),
                heap_percent: f("heap_percent"),
                disk_used_bytes: uu("disk_used_bytes"),
                docs: uu("docs"),
                index_total: uu("index_total"),
            });
        }
    }

    // Span the requested window into `buckets` equal slots. OpenSearch already
    // sorted ascending, but `downsample` re-sorts defensively.
    let bucket_ms = ((window_minutes * 60_000) / buckets as i64).max(1);
    Ok(MetricSeries {
        deployment: deployment.to_string(),
        points: downsample(&samples, bucket_ms),
    })
}

/// Downsample raw samples into time-buckets. Pure (no I/O) so it is unit-tested
/// directly. Buckets are aligned to absolute epoch (`ts / bucket_ms`); CPU and
/// heap are meaned within a bucket, disk and docs take the bucket's last value,
/// and the indexing rate is the monotonic `index_total` delta between
/// consecutive buckets over their elapsed wall-time, clamped at 0 across a
/// counter reset. Returns points in ascending time order.
pub fn downsample(samples: &[Sample], bucket_ms: i64) -> Vec<MetricPoint> {
    if samples.is_empty() || bucket_ms <= 0 {
        return Vec::new();
    }
    let mut sorted: Vec<Sample> = samples.to_vec();
    sorted.sort_by_key(|s| s.ts);

    // Pass 1: fold into raw buckets. Same-bucket samples are adjacent once
    // sorted, so merging into the trailing bucket is sufficient.
    struct Raw {
        start: i64,
        last_ts: i64,
        sum_cpu: f64,
        sum_heap: f64,
        n: u64,
        disk: u64,
        docs: u64,
        idx: u64,
    }
    let mut raws: Vec<Raw> = Vec::new();
    for s in &sorted {
        let start = s.ts.div_euclid(bucket_ms) * bucket_ms;
        match raws.last_mut() {
            Some(r) if r.start == start => {
                r.sum_cpu += s.cpu_percent;
                r.sum_heap += s.heap_percent;
                r.n += 1;
                r.last_ts = s.ts;
                r.disk = s.disk_used_bytes;
                r.docs = s.docs;
                r.idx = s.index_total;
            }
            _ => raws.push(Raw {
                start,
                last_ts: s.ts,
                sum_cpu: s.cpu_percent,
                sum_heap: s.heap_percent,
                n: 1,
                disk: s.disk_used_bytes,
                docs: s.docs,
                idx: s.index_total,
            }),
        }
    }

    // Pass 2: shape into points, deriving the rate from consecutive buckets.
    let mut points = Vec::with_capacity(raws.len());
    let mut prev: Option<(i64, u64)> = None; // (last_ts, index_total)
    for r in &raws {
        let indexing_rate = match prev {
            Some((pt, pidx)) if r.last_ts > pt && r.idx >= pidx => {
                let dt = (r.last_ts - pt) as f64 / 1000.0;
                if dt > 0.0 {
                    (r.idx - pidx) as f64 / dt
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        points.push(MetricPoint {
            ts: r.start,
            cpu_percent: r.sum_cpu / r.n as f64,
            heap_percent: r.sum_heap / r.n as f64,
            disk_used_bytes: r.disk,
            docs: r.docs,
            indexing_rate,
        });
        prev = Some((r.last_ts, r.idx));
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(ts: i64, cpu: f64, heap: f64, disk: u64, docs: u64, idx: u64) -> Sample {
        Sample {
            ts,
            cpu_percent: cpu,
            heap_percent: heap,
            disk_used_bytes: disk,
            docs,
            index_total: idx,
        }
    }

    #[test]
    fn downsample_empty_or_bad_width_is_empty() {
        assert!(downsample(&[], 1000).is_empty());
        assert!(downsample(&[s(0, 1.0, 1.0, 1, 1, 1)], 0).is_empty());
        assert!(downsample(&[s(0, 1.0, 1.0, 1, 1, 1)], -5).is_empty());
    }

    #[test]
    fn downsample_means_cpu_heap_and_keeps_last_disk_docs() {
        // Two samples in the SAME 10s bucket: cpu/heap averaged, disk/docs = last.
        let pts = downsample(
            &[
                s(1_000, 20.0, 40.0, 100, 5, 1_000),
                s(6_000, 40.0, 60.0, 200, 9, 1_500),
            ],
            10_000,
        );
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].ts, 0); // 1000.div_euclid(10000)*10000
        assert!((pts[0].cpu_percent - 30.0).abs() < 1e-9);
        assert!((pts[0].heap_percent - 50.0).abs() < 1e-9);
        assert_eq!(pts[0].disk_used_bytes, 200);
        assert_eq!(pts[0].docs, 9);
        assert_eq!(pts[0].indexing_rate, 0.0); // first bucket → no prior delta
    }

    #[test]
    fn downsample_splits_buckets_and_derives_indexing_rate() {
        // Bucket A: last_ts=1000, idx=1000. Bucket B: last_ts=11000, idx=2000.
        // rate = (2000-1000) / ((11000-1000)/1000) = 1000 / 10 = 100 docs/s.
        let pts = downsample(
            &[
                s(1_000, 10.0, 10.0, 100, 1, 1_000),
                s(11_000, 10.0, 10.0, 100, 2, 2_000),
            ],
            10_000,
        );
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].indexing_rate, 0.0);
        assert!((pts[1].indexing_rate - 100.0).abs() < 1e-9);
    }

    #[test]
    fn downsample_clamps_rate_across_counter_reset() {
        // index_total drops (node restart) → rate must not go negative.
        let pts = downsample(
            &[
                s(1_000, 0.0, 0.0, 0, 0, 5_000),
                s(11_000, 0.0, 0.0, 0, 0, 100),
            ],
            10_000,
        );
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[1].indexing_rate, 0.0);
    }

    #[test]
    fn downsample_sorts_unordered_input() {
        let pts = downsample(
            &[
                s(11_000, 10.0, 10.0, 0, 0, 0),
                s(1_000, 10.0, 10.0, 0, 0, 0),
            ],
            10_000,
        );
        assert_eq!(pts.len(), 2);
        assert!(pts[0].ts < pts[1].ts);
    }
}
