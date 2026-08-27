# VeloxSearch — Platform Requirements (v1)

**What this is:** the contract between VeloxSearch and the cluster it installs into.
A cluster that meets every requirement below is **supported**: the wizard must work
end-to-end on it (install → first-run setup → bootstrap → create deployment → data in
dashboards). A cluster that doesn't is **unsupported**: the app must *say so clearly at
the conformity screen* — never hang, never half-install.

Requirements-first policy (ADR-026): we keep this envelope deliberately narrow and make
everything inside it bulletproof before broadening. The conformity probe
(`src/bootstrap.rs`) is the executable form of this document — every requirement maps to
a probe check.

For the self-managing behaviours behind these requirements — when Longhorn is
auto-installed (R3), how cert-manager/the operator are auto-installed and gated
(R2/R6/R7), and the dedicated-namespace model (R2 RBAC) — see
[`PREMISES.md`](PREMISES.md).

## Requirements

| ID | Requirement | Why | Probe | On failure (UX) |
|----|-------------|-----|-------|-----------------|
| **R1** | Kubernetes **≥ 1.30** | Tested envelope; older APIs unverified | `/version` | Fail: "Kubernetes X.Y found, 1.30+ required" |
| **R2** | **cluster-admin at install time** (the `veloxsearch-bootstrap` binding in `install.yaml`) | Bootstrap creates CRDs, namespaces, RBAC. The app **deletes this binding itself** once bootstrap completes (ADR-027) | `SelfSubjectAccessReview` on CRD create | Fail: "insufficient permissions — apply install.yaml as a cluster admin" |
| **R3** | **Longhorn** — the `longhorn` StorageClass (provisioner `driver.longhorn.io`) is the **only supported deployment storage** (ADR-043, amends ADR-031) | OpenSearch nodes claim PVCs pinned to `storageClass: longhorn` (ADR-031/043): one predictable, tested storage path. Foreign CSI defaults (EBS/gp2, Ceph…) are **no longer accepted** — Longhorn is installed and used regardless; node-local provisioners (`rancher.io/local-path`, hostpath, `kubernetes.io/no-provisioner`) were never acceptable (data lost on reschedule) | List `storageclasses`; check for the `longhorn` SC **by name + provisioner** | Remediate (not fail): missing → VeloxSearch auto-installs Longhorn (install + inform, never ask). Nodes missing prerequisite packages (`open-iscsi`, nfs client, `dmsetup`) are surfaced **in-app, per node, with per-distro (Debian/Ubuntu/Arch) install commands**; cluster creation is refused until Longhorn is usable |
| **R4** | **Resource floor** (small preset): ≥ **8 GiB** allocatable RAM and ≥ **2 vCPU** free on schedulable nodes; ≥ 25 GiB provisionable disk | small = 3×2Gi OpenSearch requests + Dashboards + operator + cert-manager + agent. Recommended single node: **12 GiB RAM / 4 vCPU / 60 GB disk** | Sum node `allocatable` minus running pod requests | Warn/Fail: "cluster has X free of 8Gi needed — smallest preset won't schedule" |
| **R5** | **amd64** nodes | Only arch we build/test (image side-loaded today) | `nodeInfo.architecture` | Fail: "arm64 detected — unsupported in v1" |
| **R6** | **Outbound registry egress** (docker.io, quay.io, cr.fluentbit.io) | Bootstrap pulls cert-manager, operator, OpenSearch, Fluent Bit images | Indirect — surfaces as pull errors during bootstrap | Bootstrap step shows pod image-pull events |
| **R7** | **No conflicting pre-existing stack**: cert-manager (if present) must be healthy and ≥ 1.16; **no OpenSearch operator running in any namespace** | We install and own cert-manager v1.20 + operator 3.0.2 (ADR-022). Two cluster-wide operators would reconcile the same `OpenSearchCluster` CRs; foreign/broken installs = brownfield, out of v1 scope | `opensearchclusters.opensearch.org` / `opensearchclusters.opensearch.opster.io` CRD presence (the chart ships both) **+ a cluster-wide scan for an operator Deployment in any namespace** (`#115`) | Fail: "foreign OpenSearch operator `<ns>/<name>` — remove it, or set `VELOX_ALLOW_FOREIGN_OPERATOR=1` to use it instead of installing ours". Operator CRDs with no controller running are a `warn`, not a refusal |
| **R8** | Ingress controller **optional** | Access modes (ADR-027): `port-forward` always works; `ingress` mode offered only when an IngressClass exists *and* the user supplies a domain | List `ingressclasses` (informational) | Never fails — determines which access modes the UI offers |

## Tested platforms (the conformance fleet, ADR-026)

| Cluster | Purpose | Distro | Shape | Last verified |
|---|---|---|---|---|---|
| 3-node k3s | multi-node, real distributed storage | k3s v1.36.3, 3 nodes (fleet `ct5`) | **real `longhorn` (`driver.longhorn.io`) default** → Longhorn bootstrap no-ops, node pools claim it; PVC-backed | **2026-08-25**: install + conformity + wizard create + 3/3 nodes green + Longhorn volumes `attached/healthy` (fleet run, v0.8.0). Day-2 surface (edit/credentials unlocked) blocked by #27 — post-green rolling restart stalls on `.plugins-ml-config` shard recovery |
| **k3s-greenfield** | must **pass** everything | k3s v1.36.3, 1 node, 16Gi (fleet `ct1`) | local-path (node-local) default → **bootstraps Longhorn** → PVCs Bound → PVC-backed journey (ADR-031) | **2026-08-25** (fleet run, v0.8.0): install + conformity + Longhorn self-install + **PVCs Bound on `longhorn`** verified; deployment-green **blocked by #26** — the bundle's `default-replica-count=3` cannot schedule on one node, volumes go `faulted` |
| **k0s-bare** | absent-default Longhorn bootstrap + port-forward honesty | k0s v1.36.3, 1 node, 16Gi (fleet `ct2`) | no default SC → **bootstraps Longhorn** → PVCs Bound; no ingress → port-forward-only (R8) | **2026-08-25** (fleet run, v0.8.0): first-run journey passes to the main tabs, absent-default remediation offered, R8 port-forward-only honesty confirmed; deployment-green shares the #26 single-node blocker |
| **`k3s-undersized`** | **refusal honesty**: R4 + R7 hard-fail end-to-end | k3s v1.36.3, 1 node, 4Gi RAM (fleet `ct3`) | undersized (4Gi < R4 floor) **and** a foreign OpenSearch operator pre-installed in namespace `opensearch` (R7 brownfield) → conformity reports `r4`=fail + `r7`=fail → installer refuses, no half-install | **2026-08-25 — the refusal run finally executed** (fixture from #34/#115, first live run ever): both fails fire in one probe pass, the wizard refuses informatively, zero objects half-installed |

Expected-compatible but untested: vanilla kubeadm, k0s + a StorageClass, EKS/GKE/AKS. minikube (node-local `standard` default) now has **continuous smoke evidence** via the trunk CI lane (`Smoke (minikube)`, merged 2026-08-24) — install-and-boot only, not a conformance run. Per-platform install steps for minikube/k0s/k3s/k8s live in [`INSTALL.md`](INSTALL.md).

**Known incompatibility (kernel, [#32](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/32)):** OpenSearch 3.8.0's bundled JDK crashes at boot on Debian kernel `6.1.0-52` (`ClassNotFoundException` in the javaagent bootstrap; reproduced with a bare `ctr run` of the image, no Kubernetes involved — byte-identical images boot on `6.1.0-40`). The wizard warns at version selection and review when a host node reports an affected kernel; the fix is a host kernel change or a different OpenSearch version. The affected matrix beyond this observed combination is unmapped.

> **Refusal/honesty coverage (#34).** ADR-031 turned R3 from a hard fail into a remediation
> (bootstrap Longhorn), so `k0s-bare` was repurposed off the old R3 hard-fail test — leaving the
> refusal paths without a live fixture. **`k3s-undersized`** restores live coverage for
> **R4** (undersized: 4Gi < the 8Gi floor, fail band `<6Gi`) and **R7** (brownfield: a
> foreign OpenSearch operator running in namespace `opensearch`, the condition
> `check_requirements()` actually hard-fails on — a foreign cert-manager is only a `warn`,
> and so are operator CRDs with no controller behind them). Both fire in one probe
> pass, so the unsupported-cluster refusal is exercised end-to-end. **R5 (arm64)** still has
> no live fixture and can't get one on the amd64 fleet (no arm64 image); it
> remains covered by `src/bootstrap.rs` unit tests + this manual note. Outstanding live
> acceptance for `k3s-undersized`: provision the fixture and confirm the wizard refuses
> with the R4/R7 messages above.

## Explicitly out of scope for v1 (→ "broaden support" phase)

- Brownfield clusters (foreign cert-manager versions, existing operators, conflicting CRDs)
- Kubernetes < 1.30; arm64; air-gapped/registry-mirror installs
- Choosing a non-default StorageClass in the UI; OpenShift; Windows nodes
