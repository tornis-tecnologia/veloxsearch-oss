# ADR-061 — Flexible default storage: Longhorn when present, otherwise the cluster's default StorageClass

**Status:** accepted (ratified by the operator, 2026-09-20)
**Date:** 2026-09-20

## Context

ADR-043 made Longhorn the *only* supported deployment storage: the create gate
(`ensure_storage_ready`) force-installs Longhorn — host packages
`open-iscsi`/`dmsetup`, `iscsi_tcp`/`dm_thin_pool` kernel modules — and refuses
to create a cluster otherwise. On minimal distros nothing tells the user about
those node dependencies until Longhorn crashloops mid-wizard.

For a one-node evaluation this is day-0 friction with no payoff: the evaluator
does not care which CSI backs a single node's volumes, but the current gate
makes a storage stack a hard prerequisite for *any* create. Observed live
2026-09-19 on greenfield k3s v1.36.4 and k0s v1.30.4 VMs on the Incus fabric
(wiki: "veloxsearch day-0 onboarding autopsy" — the Longhorn ambush section).
Meanwhile prod and every multi-node fixture already run Longhorn and benefit
from it.

## Decision

Storage becomes **flexible** — three cases, evaluated by
`classify_storage` at the create gate:

1. **`longhorn` SC present → use it.** Node-pool PVCs are pinned to it
   (ADR-043), replica sizing is reconciled (`#26`), `durable = true`.
   Unchanged behaviour.
2. **No Longhorn, but a default StorageClass exists → use the cluster
   default.** No install, no refusal:
   - *Foreign CSI default* (EBS, PD, Ceph…): `durable = true`.
   - *Node-local default* (local-path, hostPath, no-provisioner):
     `durable = false` with a durability warning — data is lost on pod
     reschedule; installing Longhorn for HA storage is the operator's call,
     surfaced on the R3 row.
   In both cases node-pool PVCs **omit `storageClassName`** so claims bind
   through the class the cluster already defaulted.
3. **No default StorageClass at all → today's Longhorn auto-bootstrap is the
   remediation** (nothing else to ride). The k0s-bare fixture contract is
   preserved.

The R3 row becomes pass / warn / warn (Longhorn / foreign / node-local /
absent respectively — foreign passes, the other two warn); `needs_longhorn`
is true only for case 3; the bootstrap binding self-revokes once storage is
usable however that happened, since a deferred install is no longer owed on
cases 1–2.

## Consequences

- Day-0 evaluation on a stock k3s works end-to-end without touching storage;
  the trade (single-copy, node-bound volumes) is stated in the wizard-adjacent
  surfaces, not hidden — snapshots (ADR-047) are the durability answer there.
- PVC provisioning now has two shapes: pinned (Longhorn) and riding-the-
  default (omitted field). Code that assumed `storageClass: longhorn` in every
  manifest had to become conditional (`k8s.rs` node pools, `otel_stack.rs`
  stack PVCs); identity-only uses (delete lists) are unaffected.
- A deployment created on node-local storage that later needs real HA gets it
  by installing Longhorn and recreating deployments — migration of existing
  PVCs between classes is out of scope.
- Snapshot/restore and volume expansion (`#16`) now consult the *actual*
  backing class (`allowVolumeExpansion`), not an assumed Longhorn.

## Alternatives considered

- **Keep Longhorn mandatory (ADR-043 as-is).** Lost: the day-0 evaluation
  experience is the product's front door; every refusal there costs the
  project a user before the value shows.
- **Install Longhorn silently on node-local defaults (pre-061 remediation).**
  Lost twice: it still ambushes minimal distros with host-package
  requirements, and it overrides an explicit cluster configuration choice
  without asking.
- **Accept node-local defaults with no warning.** Lost: silent data loss on
  reschedule violates the honesty rule (ADR-031); the warning is the contract.
