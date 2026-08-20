# VeloxSearch — Operational Premises

**What this is:** the three self-managing behaviours VeloxSearch follows when it
lands on a cluster, written down so an operator can reason about *when* each fires,
*what permissions* it needs, and *what to expect*. These behaviours already ship —
this document is the contract, not a change to it. Each premise is verified against
the code (cited as `file:line`).

It complements two neighbours:
- [`REQUIREMENTS.md`](REQUIREMENTS.md) — the platform contract (R1–R8); the
  conformity probe is its executable form.
- [`INSTALL.md`](INSTALL.md) — the per-platform install guide.

The premises map onto the requirements: **P1** is the storage side of **R3**,
**P2** is the bootstrap side of **R2/R6/R7**, **P3** is the namespace model
`install.yaml` ships.

> **Reference ADRs:** ADR-014 (zero-prerequisite self-bootstrap), ADR-022
> (vendored-manifest + server-side-apply install machinery), ADR-027 (generic
> one-manifest install + self-revoking bootstrap RBAC), ADR-031 (PVC-backed
> storage + Longhorn self-bootstrap). See [`adr/README.md`](adr/README.md).

---

## P1 — Longhorn is auto-installed only when the cluster has no durable default storage

**Premise.** OpenSearch node pools claim PVCs (ADR-031), so a deployment must never
be provisioned against storage that loses data on a pod reschedule. On first
deployment-create VeloxSearch inspects the cluster's **default StorageClass** and
decides:

| Default StorageClass | Classification | Action |
|---|---|---|
| A real distributed/CSI default (e.g. `longhorn`=`driver.longhorn.io`, Ceph/rook, EBS/PD/Azure Disk) | **durable** | **Use it untouched.** No install. |
| A **node-local** provisioner — `rancher.io/local-path` (k3s), `kubernetes.io/no-provisioner`, any `*hostpath*`, `openebs.io/local` | **not durable** | **Install Longhorn.** |
| **No default StorageClass at all** | **absent** | **Install Longhorn.** |

The classifier is `default_storage()` (`src/bootstrap.rs:103`), which finds the
StorageClass carrying the `…/is-default-class: "true"` annotation
(`sc_is_default`, `src/bootstrap.rs:66`) and tests its provisioner against the
node-local list `NODE_LOCAL_PROVISIONERS` plus any provisioner containing
`hostpath` (`provisioner_is_node_local`, `src/bootstrap.rs:55-63`). It returns one
of three states — `Real`, `NodeLocal`, `Absent` (`DefaultStorage`,
`src/bootstrap.rs:78-86`); only `Real` is treated as durable
(`is_real`/`needs_longhorn`, `src/bootstrap.rs:88-98`).

**When it fires.** The Longhorn install is **deferred to first deployment-create**,
not run during the initial bootstrap (`run_install` step 4 explicitly installs
nothing here — `src/bootstrap.rs:609-614`). The storage-ready gate
`ensure_storage_ready` (`src/bootstrap.rs:160`) runs at the top of
`create_cluster` (`src/k8s.rs:494`): it returns immediately if the default is
already durable, otherwise it installs Longhorn and only returns `Ok` once a real
default is in place (`src/bootstrap.rs:160-187`). On a real-CSI cluster (Tornis
prod's `longhorn` default) the whole premise is a no-op.

**What the install does** (`install_longhorn`, `src/bootstrap.rs:841-862`):
1. Server-side-applies the vendored `deploy/bootstrap/longhorn.yaml` bundle.
2. Waits for the `longhorn-manager` DaemonSet and the `longhorn-driver-deployer`
   Deployment to come up — these register the CSI driver that creates the
   `longhorn` StorageClass.
3. Waits for that `longhorn` StorageClass to exist
   (`wait_longhorn_storage_ready`, `src/bootstrap.rs:771`).
4. Demotes any *node-local* StorageClass still flagged default
   (`demote_node_local_defaults`, `src/bootstrap.rs:806`) so `longhorn` is the
   cluster's **sole** default — two defaults make PVC binding ambiguous.
5. Asserts a real default is now in place before returning.

Progress is surfaced, not hidden: the create flow polls `storage_status()`
(`src/bootstrap.rs:230`) and the shared install-job snapshot to render an
"installing Longhorn storage…" step and a completion notice — the install runs as
a detached job so no HTTP/proxy timeout can kill it. This is an **install-and-inform**
decision (operator call, 2026-06-30: "install it and tell the user, don't ask" —
`src/bootstrap.rs:148-159`).

### P1 — RBAC / permissions

Installing Longhorn applies **CRDs and broad ClusterRoles**, which day-to-day
runtime RBAC deliberately does not grant. The auto-install therefore runs under the
one-time `veloxsearch-bootstrap` **ClusterRoleBinding → `cluster-admin`** shipped in
`deploy/install.yaml`. The key sequencing point:

- The bootstrap binding is **kept past the cert-manager/operator install** and
  revoked only once storage is durable. A node-local/absent cluster still needs
  `cluster-admin` for this deferred Longhorn install, so `run_install` revokes the
  binding **only** when the default is already real (`src/bootstrap.rs:622-624`);
  otherwise the binding lingers until the create-flow install makes storage durable.
- `ensure_storage_ready` performs that **self-revoke** the moment the Longhorn
  install succeeds — it calls `revoke_bootstrap` (`src/bootstrap.rs:168-172`,
  `633-644`), which deletes the `veloxsearch-bootstrap` ClusterRoleBinding
  (constant `BOOTSTRAP_BINDING`, `src/bootstrap.rs:628`). After that the app runs
  on the enumerated `veloxsearch-runtime` ClusterRole only.

The enumerated runtime role already carries the **read-only** storage permissions
the classifier needs every day — `storageclasses` get/list, `longhorn.io/nodes`
get/list, `nodes`/`nodes/proxy` for the capacity panel (see the
`veloxsearch-runtime` ClusterRole in `deploy/install.yaml`). Only the **write** of
CRDs/ClusterRoles during the install itself needs the elevated binding.

> **What the operator should expect:** on a real-CSI cluster, nothing — the binding
> self-revokes at the end of the normal bootstrap. On a node-local/absent cluster,
> the `veloxsearch-bootstrap` cluster-admin binding stays until the first deployment
> triggers the Longhorn install, then disappears on its own. If you re-apply
> `install.yaml` (e.g. a component upgrade) the binding is recreated and
> re-revoked the same way (ADR-027 caveat).

### P1 — Node prerequisites and informative failure

Longhorn has its own per-node prerequisites: **`open-iscsi` installed with `iscsid`
running**, plus a **schedulable spare disk** (and `nfs-common` for RWX). VeloxSearch
cannot install host packages, so where a node can't host Longhorn volumes the
storage gate **fails with a clear message rather than leaving PVCs `Pending`
forever** (the honesty rule, ADR-031):

- A node reporting Longhorn reason `MissingDependency` (e.g. `iscsiadm not found`
  = no `open-iscsi`) is a **fatal, fail-fast** prerequisite gap — the wait aborts
  immediately with a message naming the node and telling you to install the
  prerequisites or provide a real default StorageClass
  (`reason_is_missing_prereq` → `node_issue_from_status` →
  `wait_longhorn_storage_ready`, `src/bootstrap.rs:689, 696-750, 771-801`).
- A node with **no schedulable disk** is reported but not failed fast (a fresh disk
  can take a moment to register); if the wait does time out, the error names the
  node/disk cause instead of a bare timeout (`src/bootstrap.rs:718-748, 791-800`).

On Debian/Ubuntu nodes:
`sudo apt-get install -y open-iscsi && sudo systemctl enable --now iscsid`.

---

## P2 — cert-manager and the OpenSearch operator are auto-installed if missing; deploy is gated on prerequisites

**Premise.** "The only prerequisite is a Kubernetes cluster" (ADR-014). On first
bootstrap VeloxSearch **checks** for cert-manager and the OpenSearch operator,
**installs whatever is missing** from manifests vendored at build time, and only
proceeds to deploy an OpenSearch cluster **after** the operator *and* storage have
been validated.

**Detection.** cert-manager presence is decided by a CRD probe, not guesswork
(`crd_present`): `certificates.cert-manager.io`, with readiness confirmed against
the owning Deployments (`cert-manager`, `cert-manager-webhook`,
`cert-manager-cainjector`).

The operator probe is **cluster-wide**, and CRD presence alone is not the
question (`scan_operators`/`classify_operator`, `#115`): it matches *both*
`OpenSearchCluster` CRD names the chart ships (`opensearchclusters.opensearch.org`
and the legacy `opensearchclusters.opensearch.opster.io`) *and* lists operator
controller Deployments in **every** namespace, keyed on the chart's
`app.kubernetes.io/name: opensearch-operator` label. The reason is that the
operator is cluster-scoped: an operator running in someone else's namespace still
reconciles the same CRs, so a probe scoped to our own namespace cannot see the
conflict it exists to prevent. A foreign operator is a **refusal** (overridable
with `VELOX_ALLOW_FOREIGN_OPERATOR=1`, which adopts it instead of installing
ours); operator CRDs with no controller running are a `warn`, because an
interrupted install of *our own* leaves exactly that state and failing it would
wedge the retry.

**Install source.** The bundles are vendored into the repo and embedded in the
binary at build time (`include_str!` of `deploy/bootstrap/cert-manager.yaml`,
`operator.yaml`, `longhorn.yaml` — `src/bootstrap.rs:24-29`), pinned to the
versions validated on the dev cluster (cert-manager v1.20.2, operator 3.0.2 via
`helm template … --include-crds`, ADR-022). They are applied with **server-side
apply** in retrying rounds (Namespace → CRD → rest) so CRD-registration and
webhook-startup races resolve themselves and re-runs are idempotent
(`apply_bundle`, `src/bootstrap.rs:877-916`). Nothing is pulled from a live chart
repo at runtime.

**Ordering guarantee.** The install order is explicit and load-bearing
(`run_install`, `src/bootstrap.rs:570-626`):

1. **cert-manager first** — the operator bundle contains cert-manager
   `Certificate`/`Issuer` CRs, so cert-manager must be serving before the operator
   lands (`src/bootstrap.rs:573-583`).
2. **The app namespace** is ensured before the operator is placed in it
   (`src/bootstrap.rs:585-596`).
3. **The OpenSearch operator**, waited ready (`src/bootstrap.rs:600-607`).
4. **Storage** — deferred to first create per P1 (`src/bootstrap.rs:609-614`).

**Deploy is gated on prerequisites.** No OpenSearchCluster CR is ever applied until
the prerequisites pass. Two gates enforce this:

- The conformity gate: the UI is blocked behind the bootstrap checklist until
  `status().ready` (cert-manager **and** operator ready — `src/bootstrap.rs:517`).
  If any hard requirement fails, the installer **refuses** rather than half-installs
  (`unsupported`, ADR-026 — `src/bootstrap.rs:553-556`).
- The storage gate: `create_cluster` calls `ensure_storage_ready` **before** it
  applies the OpenSearchCluster CR (`src/k8s.rs:494-496`), so a cluster is only
  provisioned once storage is durable (P1).

> **What the operator should expect:** on a cluster that already has a healthy
> cert-manager and operator, bootstrap is a no-op verification. On a virgin
> cluster, the wizard installs both from the vendored bundles and shows per-step
> progress; the main tabs stay gated until the cluster "conforms". A conflicting
> pre-existing stack (a legacy `opster.io` operator, or an unhealthy foreign
> cert-manager) is an R7 refusal, not an overwrite (`src/bootstrap.rs:451-471`).

---

## P3 — Each VeloxSearch deployment gets its own dedicated namespace

**Premise.** A VeloxSearch installation is confined to **one dedicated namespace**.
Everything it manages — the OpenSearch operator, every OpenSearchCluster it creates,
and each cluster's admin Secret/Ingress — lives in that single namespace, scoped by
the Kubernetes downward API. This is the isolation boundary for the install.

**Naming and creation.** The namespace is resolved once at startup by `ns()`
(`src/k8s.rs:22-35`), in priority order:

1. the `POD_NAMESPACE` env var (set from `metadata.namespace` via the downward API
   in `deploy/install.yaml`),
2. the service-account-mounted namespace file,
3. an **inert** off-cluster fallback of `veloxsearch-dev` (#67) — a namespace that
   does not exist in any real deployment.

The fallback is deliberately never a namespace that holds live data (esp. not
`veloxsearch-test`, Tornis prod): a dev box whose kubeconfig still points at a
live cluster must not silently drive it. When the fallback fires, `ns()` warns
loudly, and the `ensure_namespace_exists` guard then refuses every write op with
an actionable "run the bootstrap" message rather than acting on production. Off
a cluster, set `POD_NAMESPACE` explicitly to target a real namespace.

The shipped `deploy/install.yaml` creates and targets `veloxsearch-system`
(ADR-027); Tornis prod runs the same binary in `veloxsearch-test`. Because the
namespace is read from the pod's own metadata, **the same image is namespace-agnostic**
— deploy it into any namespace and it scopes all its work there. The bootstrap also
ensures the namespace exists before placing the operator in it
(`src/bootstrap.rs:585-596`).

**What is namespace-scoped.** All OpenSearch APIs are bound to `ns()`:
- OpenSearchCluster CRs — `os_api` (`src/k8s.rs:50-53`); the CR is applied with
  `"namespace": ns()` (`src/k8s.rs:537`).
- Dashboards Ingress — `ingress_api` (`src/k8s.rs:55-58`).
- Per-cluster admin Secrets — `Api::namespaced(…, ns())` (e.g. `src/k8s.rs:440`).

**Two-level naming inside the namespace.** Individual OpenSearch deployments do
**not** each get their own namespace — they coexist in the install namespace and are
kept distinct by a **collision-safe unique name** `<base>-<suffix>` (ADR-020,
`unique_name`, `src/k8s.rs:172-188`), which also becomes the deployment's subdomain
`<name>.veloxsearch.ai`. Collection agents (Fluent Bit) are the one exception: they
live in a **separate** shared `velox-agents` namespace, created idempotently
(`AGENT_NS`, `src/agents.rs:14`, `43-44`; pre-created in `install.yaml`).

**Operations are namespace-scoped.** Day-to-day runtime RBAC is namespaced to the
install namespace: the `veloxsearch-runtime` **Role** in `deploy/install.yaml`
grants Secret/ConfigMap/Ingress management **only within `veloxsearch-system`**
(a separate Role covers `velox-agents`). The cluster-scoped runtime ClusterRole is
read-only discovery plus the OpenSearchCluster lifecycle. So a VeloxSearch install
cannot mutate workloads outside its own namespace once the bootstrap binding is
revoked.

> **What the operator should expect:** one VeloxSearch install = one dedicated
> namespace (`veloxsearch-system` by default) holding the operator and every
> OpenSearch cluster it manages, plus the shared `velox-agents` namespace for
> collection agents. Multiple independent installs can coexist on one cluster by
> targeting different namespaces. To find everything an install owns,
> `kubectl get all,opensearchclusters,secrets,ingress -n <install-namespace>`.

---

## Premise ↔ requirement ↔ ADR map

| Premise | Requirement | ADR | Primary code |
|---|---|---|---|
| **P1** Longhorn auto-install | R3 (storage) | ADR-031 (amends ADR-021) | `src/bootstrap.rs:103, 160, 841` |
| **P2** operator/cert-manager auto-install + deploy gating | R2, R6, R7 | ADR-014, ADR-022 | `src/bootstrap.rs:570, 877` |
| **P3** dedicated namespace per deployment | R2 (RBAC) | ADR-020, ADR-027 | `src/k8s.rs:22, 50, 537` |

See [`adr/README.md`](adr/README.md) for what each ADR decided, and
[`REQUIREMENTS.md`](REQUIREMENTS.md) for the platform contract.
