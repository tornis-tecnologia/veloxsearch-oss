# ADR-065 — Longhorn by default, with an opt-out

**Status:** proposed
**Date:** 2026-09-28

Amends [ADR-061](ADR-061-flexible-default-storage.md).

## Context

ADR-061 (ratified 2026-09-20) made storage flexible and, in doing so, made the
*absence* of Longhorn the default outcome on the most common evaluation
cluster. `classify_storage` sorts a host into four cases, and on a stock k3s —
`local-path` as the default StorageClass — it answers `NodeLocal`, which means:
use what is there, install nothing, warn about durability. `views_create.jsx`
states the posture in a comment: *"We don't ASK — we inform"*.

That fixed a real problem. Forcing Longhorn meant every create dragged in host
packages (`open-iscsi`, `dmsetup`) and kernel modules (`iscsi_tcp`,
`dm_thin_pool`) that nothing announced until Longhorn crashlooped mid-wizard —
the "Longhorn ambush" of the 2026-09-19 day-0 autopsy.

But it overshot. The operator now has no way to say "yes, install it" short of
applying the vendored bundle by hand, and three things make that worse than it
sounds:

1. **The choice is silently made for them.** A single-node evaluation and a
   three-node cluster destined for production take the same path, and the
   second one quietly lands on node-local storage whose data does not survive a
   reschedule. The warning is real but it is a warning, not a decision point.

2. **The window closes.** Installing Longhorn needs the `veloxsearch-bootstrap`
   cluster-admin binding, which the app revokes itself once storage is usable
   however it got there (ADR-027, and ADR-061 explicitly extends the revoke to
   cases 1–2). By the time an operator concludes they want Longhorn, the app can
   no longer install it: the binding is gone and only re-applying
   `install.yaml` brings it back. Observed on this project's own test cluster
   2026-09-28 — `veloxsearch-bootstrap` absent, `local-path` default, and no
   product path to Longhorn at all.

3. **Migration is out of scope.** ADR-061 says so plainly: a deployment created
   on node-local storage that later needs HA "gets it by installing Longhorn and
   recreating deployments". So the default silently chosen at first boot is,
   for every existing deployment, permanent. A default that is cheap to change
   later can afford to be implicit. This one cannot.

## Decision

The installation makes an **explicit, recorded storage choice**, presented once,
at first run, with **Longhorn pre-selected**.

`classify_storage` keeps its four cases; what changes is what happens in two of
them and who decides:

| `classify_storage` | Today (ADR-061) | This ADR |
| --- | --- | --- |
| `Longhorn` | Use it | Unchanged |
| `ForeignDefault` (EBS, PD, Ceph…) | Use it, no install | Unchanged — a real CSI default is durable, and stacking Longhorn on it is a choice, not a default. Offered, not pre-selected. |
| `NodeLocal` (local-path, hostPath) | Use it, warn, no install | **Ask, with "install Longhorn" pre-selected.** Opting out keeps today's behaviour, warning and all. |
| `Absent` | Auto-install Longhorn | Unchanged |

Four properties make this a decision rather than a prompt:

1. **Pre-flight before the question, not after the answer.** The screen states
   whether the node prerequisites are actually satisfied — `iscsid` running,
   `iscsi_tcp` loadable, `dmsetup` present — checked per node before Longhorn
   is offered as the default. Where they are missing, the option is still
   offered, with the exact remediation and the warning that this is what
   ADR-061 was written about. The ambush was never the install; it was
   discovering the dependency at crashloop time.

2. **The choice is recorded, and the window stays open.** The answer is written
   to the installation config ConfigMap (the ADR-034/062 shape:
   non-secret, installation-level, admin-owned), so it is not re-asked and
   `ensure_storage_ready` honours it. Opting out **does not** revoke the
   bootstrap binding on its own: the revoke waits until the recorded choice is
   satisfied, so "not now" stays reversible without re-applying `install.yaml`.
   An operator who opts out and changes their mind gets a working button, not a
   support thread.

3. **Installation-level, therefore admin-only.** This is a `bootstrap_*`-shaped
   decision (`AdminOnly` in `api::ROUTES`), surfaced on the first-run bootstrap
   screen and the R3 row — never in the create wizard. A tenant creating a
   deployment must not be asked which CSI the cluster runs, and ADR-061's "we
   inform, we do not ask" stays exactly right *for that screen*.

4. **Unattended installs keep a default, and it is the safe one.** With no
   answer recorded and no human to ask — CI, scripted installs — the behaviour
   is Longhorn-when-prerequisites-are-met, falling back to ADR-061's use-what-is-there
   when they are not. An install that cannot ask must not block.

## Consequences

- The day-0 path on a stock k3s grows one screen and, if the operator accepts
  the default, several minutes of Longhorn install. That is the cost, and it is
  paid by the evaluator ADR-061 was protecting. It is accepted because the
  alternative charges a much larger cost to the operator who did not realise a
  choice was being made — and charges it later, when the fix is recreating
  every deployment.
- `ensure_storage_ready` stops being a pure function of `classify_storage` and
  becomes a function of classification **plus** recorded intent. The deferred-revoke
  contract gets a third reason to hold the binding, alongside ADR-031's.
- The conformance fixtures gain a case: `k3s-greenfield` must now assert both
  answers, not just the current one. `k0s-bare` (case `Absent`) is unchanged.
- An operator who opts out is in exactly ADR-061's world, by their own decision.
  Nothing about the node-local path is removed — it stops being implicit.
- Two defects found while validating this on a live cluster are in scope for the
  implementation, because both make "install Longhorn" quietly wrong today:
  - `reconcile_longhorn_sizing` patches the `Setting` CRD at `spec.value`, but
    the CRD has no `spec` (schema is `[apiVersion, kind, metadata, status,
    value]` in v1beta1 and v1beta2). The API server prunes the field and answers
    `patched (no change)`, so `default-replica-count` and
    `replica-soft-anti-affinity` are never applied — on a one-node cluster every
    volume claimed outside our StorageClass asks for 3 replicas and cannot
    schedule. Verified 2026-09-28.
  - `demote_node_local_defaults` runs once and best-effort. On k3s the
    `local-storage` addon is re-applied by the k3s deploy controller, which
    restores `local-path`'s default-class annotation — leaving two defaults.
    Observed on a cluster where the demotion had run six weeks earlier.

## Alternatives considered

- **Leave ADR-061 as is; document the manual path.** Cheapest, and rejected:
  the manual path is `kubectl apply` of a vendored bundle plus a StorageClass
  demotion plus a replica resize that the product otherwise does for you. That
  is not an operator task, it is a re-implementation of `install_longhorn`.
- **Always install Longhorn (revert to ADR-043).** Rejected for exactly the
  reasons ADR-061 gives, which have not changed. Day-0 evaluation on a minimal
  distro must not require a storage stack.
- **Ask in the create wizard instead.** Rejected: the decision is
  installation-wide and admin-only, and the wizard is the one screen a tenant
  reaches. It would also ask once per deployment a question that has one answer
  per cluster.
- **Infer from node count — single node means node-local, multi-node means
  Longhorn.** Tempting and wrong. A single-node production box is a real
  deployment shape, and a three-node lab is a real evaluation shape. Inferring
  intent from topology reproduces the present bug with extra steps.
- **Ask, but pre-select "use what is there".** Rejected on the asymmetry of the
  costs: accepting an unwanted Longhorn install costs minutes at first boot;
  declining a wanted one costs recreating every deployment later, because
  ADR-061 puts PVC migration out of scope.
