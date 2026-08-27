// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Host-cluster capacity & health (the "Capacidade" panel).
//!
//! Unlike `metrics.rs` — which reports the health of each *OpenSearch* cluster
//! via its own `_nodes/stats` — this module reports the health of the **K3S
//! cluster underneath**: per-node CPU / memory / disk, node conditions, and a
//! "how many more OpenSearch deployments fit?" estimate. The answer the admin
//! needs before clicking Create.
//!
//! Sources (all read-only, all best-effort — any one being unavailable degrades
//! that section to `None`/`false` rather than failing the whole panel):
//!   * core `Node`            → CPU/mem capacity+allocatable, conditions, roles
//!   * `metrics.k8s.io`       → live CPU/mem *used* (same data as `kubectl top`)
//!   * core `Pod` (all ns)    → summed `requests` per node = scheduling headroom
//!   * kubelet Summary API    → host root-fs disk (via `nodes/proxy`)
//!   * `longhorn.io` `Node`   → persistent storage pool free/total
//!
//! Live trend is NOT stored here — the SPA keeps a small ring-buffer of samples
//! it polls every few seconds (matching the Overview tab), so there is no new
//! server-side time-series to manage.

use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, ListParams};
use kube::Client;
use std::collections::BTreeMap;

use crate::api::{ClusterCapacity, DeploymentFit, NodeCapacity, ResUse, StoragePool};

/// A deployment is always 3 nodes (ADR-016) — the multiplier the fit estimator
/// applies to a preset's per-node requests.
const NODES_PER_DEPLOYMENT: u64 = 3;

/// Assemble the whole panel payload. The five sources run sequentially but each
/// is cheap (a couple of list calls + one tiny request per node); the dominant
/// cost is the kubelet summary fan-out over ~3 nodes.
pub async fn cluster_capacity() -> Result<ClusterCapacity> {
    let client = crate::k8s::client().await?;

    let nodes_api: Api<Node> = Api::all(client.clone());
    let node_list = nodes_api
        .list(&ListParams::default())
        .await
        .context("listing nodes")?;

    // Best-effort enrichers — an empty map just means that column reads "n/d".
    let used = node_metrics(&client).await.unwrap_or_default();
    let metrics_available = !used.is_empty();
    let requested = pod_requests(&client).await.unwrap_or_default();
    let longhorn = longhorn_disks(&client).await.unwrap_or_default();

    let mut nodes = Vec::with_capacity(node_list.items.len());
    let (mut cpu_total, mut cpu_used, mut cpu_req) = (0u64, 0u64, 0u64);
    let (mut mem_total, mut mem_used, mut mem_req) = (0u64, 0u64, 0u64);

    for node in &node_list.items {
        let name = node.metadata.name.clone().unwrap_or_default();
        let status = node.status.as_ref();

        // Allocatable is what the scheduler can actually hand out — the honest
        // denominator for "room for another deployment". Capacity ≈ allocatable
        // on K3S (tiny system reservation), so we use allocatable throughout.
        let alloc = status.and_then(|s| s.allocatable.as_ref());
        let cpu_cap = alloc
            .and_then(|a| a.get("cpu"))
            .map(|q| parse_cpu_millis(&q.0))
            .unwrap_or(0);
        let mem_cap = alloc
            .and_then(|a| a.get("memory"))
            .map(|q| parse_qty_bytes(&q.0))
            .unwrap_or(0);

        let (n_cpu_used, n_mem_used) = used.get(&name).copied().unwrap_or((0, 0));
        let (n_cpu_req, n_mem_req) = requested.get(&name).copied().unwrap_or((0, 0));

        cpu_total += cpu_cap;
        cpu_used += n_cpu_used;
        cpu_req += n_cpu_req;
        mem_total += mem_cap;
        mem_used += n_mem_used;
        mem_req += n_mem_req;

        // Node conditions: Ready + any active *Pressure* the admin should see.
        let mut ready = false;
        let mut pressures = Vec::new();
        if let Some(conds) = status.and_then(|s| s.conditions.as_ref()) {
            for c in conds {
                let active = c.status == "True";
                match c.type_.as_str() {
                    "Ready" => ready = active,
                    "MemoryPressure" | "DiskPressure" | "PIDPressure" if active => {
                        pressures.push(c.type_.clone())
                    }
                    _ => {}
                }
            }
        }

        let host_disk = host_disk(&client, &name)
            .await
            .map(|(total, used, _avail)| ResUse {
                total,
                used: Some(used),
                requested: None,
            });

        let storage = longhorn.get(&name).map(|&(max, avail)| ResUse {
            total: max,
            used: Some(max.saturating_sub(avail)),
            requested: None,
        });

        nodes.push(NodeCapacity {
            name,
            roles: node_roles(node),
            ready,
            pressures,
            kernel_version: status
                .and_then(|s| s.node_info.as_ref())
                .map(|i| i.kernel_version.clone())
                .unwrap_or_default(),
            cpu: ResUse {
                total: cpu_cap,
                used: metrics_available.then_some(n_cpu_used),
                requested: Some(n_cpu_req),
            },
            mem: ResUse {
                total: mem_cap,
                used: metrics_available.then_some(n_mem_used),
                requested: Some(n_mem_req),
            },
            host_disk,
            storage,
        });
    }

    // Cluster-wide persistent pool = sum of every node's Longhorn disks.
    let storage = if longhorn.is_empty() {
        None
    } else {
        let (mut total, mut avail) = (0u64, 0u64);
        for &(max, a) in longhorn.values() {
            total += max;
            avail += a;
        }
        Some(StoragePool {
            total,
            used: total.saturating_sub(avail),
            available: avail,
        })
    };

    let cpu_headroom = cpu_total.saturating_sub(cpu_req);
    let mem_headroom = mem_total.saturating_sub(mem_req);
    let storage_free = storage.as_ref().map(|s| s.available);
    let fit = crate::k8s::PRESET_SIZES
        .iter()
        .map(|size| deployment_fit(size, cpu_headroom, mem_headroom, storage_free))
        .collect();

    Ok(ClusterCapacity {
        nodes,
        cpu: ResUse {
            total: cpu_total,
            used: metrics_available.then_some(cpu_used),
            requested: Some(cpu_req),
        },
        mem: ResUse {
            total: mem_total,
            used: metrics_available.then_some(mem_used),
            requested: Some(mem_req),
        },
        storage,
        fit,
        metrics_available,
    })
}

/// How many additional deployments of `size` fit, and which resource runs out
/// first. Uses scheduling headroom (allocatable − requested) for CPU/mem and
/// the free persistent pool for disk; each deployment costs 3× the per-node
/// preset request. Disk is only a limiter when a persistent pool is known.
fn deployment_fit(
    size: &str,
    cpu_headroom: u64,
    mem_headroom: u64,
    storage_free: Option<u64>,
) -> DeploymentFit {
    let (cpu_s, mem_s, disk_s) = crate::k8s::preset_requests(size);
    let cpu_per = parse_cpu_millis(&cpu_s).max(1) * NODES_PER_DEPLOYMENT;
    let mem_per = parse_qty_bytes(&mem_s).max(1) * NODES_PER_DEPLOYMENT;
    let disk_per = parse_qty_bytes(&disk_s).max(1) * NODES_PER_DEPLOYMENT;

    let by_cpu = cpu_headroom / cpu_per;
    let by_mem = mem_headroom / mem_per;
    let by_disk = storage_free.map(|f| f / disk_per);

    // Pick the binding constraint (smallest count); disk only counts if known.
    let mut count = by_cpu.min(by_mem);
    let mut limited_by = if by_cpu <= by_mem { "cpu" } else { "mem" };
    if let Some(d) = by_disk {
        if d < count {
            count = d;
            limited_by = "disk";
        }
    }

    DeploymentFit {
        size: size.to_string(),
        count,
        limited_by: limited_by.to_string(),
    }
}

/// Standard `node-role.kubernetes.io/<role>` labels → role names (control-plane,
/// etcd, master, …). Skips the rest of the (noisy) node label set.
fn node_roles(node: &Node) -> Vec<String> {
    node.metadata
        .labels
        .as_ref()
        .map(|labels| {
            labels
                .keys()
                .filter_map(|k| k.strip_prefix("node-role.kubernetes.io/"))
                .filter(|r| !r.is_empty())
                .map(|r| r.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Live CPU(millicores)/mem(bytes) used per node from metrics-server. Returns
/// an empty map (→ `metrics_available=false`) if the API is absent or denied.
async fn node_metrics(client: &Client) -> Result<BTreeMap<String, (u64, u64)>> {
    let gvk = GroupVersionKind::gvk("metrics.k8s.io", "v1beta1", "NodeMetrics");
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &ApiResource::from_gvk(&gvk));
    let list = api.list(&ListParams::default()).await?;
    let mut out = BTreeMap::new();
    for item in list {
        let Some(name) = item.metadata.name.clone() else {
            continue;
        };
        let usage = item.data.get("usage");
        let cpu = usage
            .and_then(|u| u.get("cpu"))
            .and_then(|v| v.as_str())
            .map(parse_cpu_millis)
            .unwrap_or(0);
        let mem = usage
            .and_then(|u| u.get("memory"))
            .and_then(|v| v.as_str())
            .map(parse_qty_bytes)
            .unwrap_or(0);
        out.insert(name, (cpu, mem));
    }
    Ok(out)
}

/// Sum of pod `requests` (cpu millicores, mem bytes) per scheduled node — what
/// the scheduler has already committed, i.e. `kubectl describe node`'s
/// "Allocated resources". Terminated pods are excluded.
async fn pod_requests(client: &Client) -> Result<BTreeMap<String, (u64, u64)>> {
    let api: Api<Pod> = Api::all(client.clone());
    let list = api.list(&ListParams::default()).await?;
    let mut out: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for pod in list {
        let Some(spec) = pod.spec else { continue };
        let Some(node) = spec.node_name.clone() else {
            continue;
        };
        if let Some(phase) = pod.status.as_ref().and_then(|s| s.phase.as_ref()) {
            if phase == "Succeeded" || phase == "Failed" {
                continue;
            }
        }
        let (mut cpu, mut mem) = (0u64, 0u64);
        for c in &spec.containers {
            if let Some(req) = c.resources.as_ref().and_then(|r| r.requests.as_ref()) {
                if let Some(q) = req.get("cpu") {
                    cpu += parse_cpu_millis(&q.0);
                }
                if let Some(q) = req.get("memory") {
                    mem += parse_qty_bytes(&q.0);
                }
            }
        }
        let e = out.entry(node).or_default();
        e.0 += cpu;
        e.1 += mem;
    }
    Ok(out)
}

/// Host root-filesystem `(capacity, used, available)` bytes via the kubelet
/// Summary API (`/api/v1/nodes/<n>/proxy/stats/summary` → `node.fs`). `None`
/// when `nodes/proxy` is denied or the kubelet is unreachable.
async fn host_disk(client: &Client, node: &str) -> Option<(u64, u64, u64)> {
    let path = format!("/api/v1/nodes/{node}/proxy/stats/summary");
    let req = http::Request::get(path).body(Vec::new()).ok()?;
    let v: serde_json::Value = client.request(req).await.ok()?;
    let fs = v.get("node")?.get("fs")?;
    let cap = fs.get("capacityBytes")?.as_u64()?;
    let used = fs.get("usedBytes").and_then(|x| x.as_u64()).unwrap_or(0);
    let avail = fs
        .get("availableBytes")
        .and_then(|x| x.as_u64())
        .unwrap_or_else(|| cap.saturating_sub(used));
    Some((cap, used, avail))
}

/// Persistent storage pool per node `(storageMaximum, storageAvailable)` bytes,
/// summed across each Longhorn node's disks. Empty when Longhorn is not the
/// storage layer (then the panel hides the storage section). Tries the current
/// CRD version, then the older one.
async fn longhorn_disks(client: &Client) -> Result<BTreeMap<String, (u64, u64)>> {
    for version in ["v1beta2", "v1beta1"] {
        let gvk = GroupVersionKind::gvk("longhorn.io", version, "Node");
        let api: Api<DynamicObject> = Api::all_with(client.clone(), &ApiResource::from_gvk(&gvk));
        let Ok(list) = api.list(&ListParams::default()).await else {
            continue;
        };
        let mut out = BTreeMap::new();
        for item in list {
            let Some(name) = item.metadata.name.clone() else {
                continue;
            };
            let (mut max, mut avail) = (0u64, 0u64);
            if let Some(disks) = item
                .data
                .get("status")
                .and_then(|s| s.get("diskStatus"))
                .and_then(|d| d.as_object())
            {
                for disk in disks.values() {
                    max += disk
                        .get("storageMaximum")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                    avail += disk
                        .get("storageAvailable")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                }
            }
            if max > 0 {
                out.insert(name, (max, avail));
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    Ok(BTreeMap::new())
}

/// Parse a K8s CPU quantity to **millicores**: `"500m"`, `"2"`, nanocores
/// `"123456789n"` (metrics-server) and the rarer `u`/`k` suffixes.
fn parse_cpu_millis(s: &str) -> u64 {
    let s = s.trim();
    let num = |n: &str| n.trim().parse::<f64>().unwrap_or(0.0);
    if let Some(n) = s.strip_suffix('n') {
        (num(n) / 1_000_000.0) as u64
    } else if let Some(n) = s.strip_suffix('u') {
        (num(n) / 1_000.0) as u64
    } else if let Some(n) = s.strip_suffix('m') {
        num(n) as u64
    } else if let Some(n) = s.strip_suffix('k') {
        (num(n) * 1_000_000.0) as u64
    } else {
        (num(s) * 1000.0) as u64
    }
}

/// Parse a K8s memory/storage quantity to **bytes**: binary (`Ki`/`Mi`/`Gi`/
/// `Ti`/`Pi`/`Ei`) and decimal (`k`/`M`/`G`/`T`/`P`/`E`) suffixes, else plain.
fn parse_qty_bytes(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    let num = |n: &str| n.trim().parse::<f64>().unwrap_or(0.0);
    const KI: f64 = 1024.0;
    // Binary suffixes first so "Ki" isn't mis-read as the decimal "K".
    for (suf, mult) in [
        ("Ki", KI),
        ("Mi", KI * KI),
        ("Gi", KI * KI * KI),
        ("Ti", KI * KI * KI * KI),
        ("Pi", KI.powi(5)),
        ("Ei", KI.powi(6)),
    ] {
        if let Some(n) = s.strip_suffix(suf) {
            return (num(n) * mult) as u64;
        }
    }
    for (suf, mult) in [
        ("E", 1e18),
        ("P", 1e15),
        ("T", 1e12),
        ("G", 1e9),
        ("M", 1e6),
        ("k", 1e3),
        ("K", 1e3),
    ] {
        if let Some(n) = s.strip_suffix(suf) {
            return (num(n) * mult) as u64;
        }
    }
    num(s) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_millicores() {
        assert_eq!(parse_cpu_millis("500m"), 500);
        assert_eq!(parse_cpu_millis("2"), 2000);
        assert_eq!(parse_cpu_millis("4"), 4000);
        // metrics-server reports nanocores: 250m worth = 250_000_000n.
        assert_eq!(parse_cpu_millis("250000000n"), 250);
        assert_eq!(parse_cpu_millis("1500u"), 1); // microcores → ~1.5m → 1
        assert_eq!(parse_cpu_millis(""), 0);
    }

    #[test]
    fn quantity_bytes() {
        assert_eq!(parse_qty_bytes("4Gi"), 4 * 1024 * 1024 * 1024);
        assert_eq!(parse_qty_bytes("512Mi"), 512 * 1024 * 1024);
        // Node/metrics-server memory comes as Ki — must not be read as decimal K.
        assert_eq!(parse_qty_bytes("8000000Ki"), 8_000_000 * 1024);
        assert_eq!(parse_qty_bytes("1G"), 1_000_000_000);
        assert_eq!(parse_qty_bytes("1048576"), 1_048_576); // plain bytes
        assert_eq!(parse_qty_bytes(""), 0);
    }

    #[test]
    fn fit_picks_binding_constraint() {
        // 4 vCPU / 4Gi free, 100Gi pool. small = 500m/2Gi/5Gi per node ×3.
        // cpu: 4000/(500*3)=2; mem: 4Gi/(2Gi*3)=0; disk: 100Gi/(5Gi*3)=6 → mem-bound, 0.
        let f = deployment_fit(
            "small",
            4000,
            4 * 1024 * 1024 * 1024,
            Some(100 * 1024 * 1024 * 1024),
        );
        assert_eq!(f.count, 0);
        assert_eq!(f.limited_by, "mem");

        // Plenty of cpu+mem, tiny disk pool → disk-bound.
        let f = deployment_fit(
            "small",
            100_000,
            200u64 * 1024 * 1024 * 1024,
            Some(20 * 1024 * 1024 * 1024),
        );
        assert_eq!(f.limited_by, "disk");
        assert_eq!(f.count, 1); // 20Gi / (5Gi*3) = 1
    }
}
