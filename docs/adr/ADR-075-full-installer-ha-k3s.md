# ADR-075 — A full installer: three bare servers to an HA k3s with VeloxSearch

**Status:** proposed
**Date:** 2026-11-26

## Context

VeloxSearch installs *into* a cluster (ADR-027): `kubectl apply -f
install.yaml` and the app does the rest. "No cluster yet?" was answered by
INSTALL.md §0 with k3s's one-liner and a list of node packages to remember.
That path makes a single node with node-local storage, which ADR-061 accepts
for evaluation but which is not what a customer should run in production:
one disk, one copy, no quorum.

A production customer starts with three Linux servers. Getting from there to
a redundant VeloxSearch takes a specific set of decisions that the manual
path leaves to each operator, and each one is easy to get wrong:

- k3s's `local-path` provisioner is a default StorageClass. While it is,
  VeloxSearch uses it (ADR-061) and never installs Longhorn, so the
  "redundant" cluster keeps one copy of every shard. Once Longhorn is
  there, k3s's addon reconciler keeps restoring local-path as a second
  default. The app re-demotes it on an interval (#133/#134), which treats
  the symptom: the provisioner is still there to win the next race.
- Longhorn needs packages, a running iscsid and kernel modules on every
  node (#15), and multipathd stays off its devices.
- OpenSearch refuses to start below `vm.max_map_count=262144`.
- etcd wants three servers, synchronised clocks and its ports open.

## Decision

1. **One script, `deploy/install-veloxsearch-full.sh`, published as a
   release asset** next to `install.yaml`, covered by `SHA256SUMS`. It is
   versioned with the release whose manifest it installs.
2. **Three k3s servers on embedded etcd.** The first runs `cluster-init`
   and the others join it with node 1's full server token
   (`K10<ca-hash>::server:…`), which pins the cluster CA. Every server also
   gets the following:
   - secrets encryption;
   - etcd snapshots every 6 h;
   - a pinned k3s version, the line the conformance fleet ran (v1.36);
   - **`disable: local-storage`**.
3. **Longhorn is installed by the script, before VeloxSearch.** The bundle is
   `deploy/bootstrap/longhorn.yaml` from the same release, applied with the
   app's own field manager (`veloxsearch-bootstrap`). The app then sees "a
   `longhorn` StorageClass already present", which is the ADR-043 path: no
   drift, and the replica sizing is still reconciled. Because no other
   default exists, the control-plane Postgres claim also lands on Longhorn,
   with three replicas.
4. **Node preparation is part of the install**, as are its checks:
   - Hard preflight: systemd, amd64, at least 4 GiB, 2 vCPU, 30 GiB free on
     `/var/lib`, free ports, a unique hostname, no other kubelet, and egress.
   - Packages: open-iscsi, the NFS client and cryptsetup, plus the
     modules-load and sysctl files.
   - Swap off, with an fstab backup.
   - A multipathd blacklist, but only when no multipath device is in use.
   - nm-cloud-setup disabled (k3s requirement).
   - Firewall ports opened in firewalld or ufw when either is active.
   - The supported families are Debian, Ubuntu, RHEL-like, Amazon Linux and
     SUSE.
5. **Two ways to drive it:**
   - `install --peers B,C` runs on node 1. It reaches the others over SSH
     (key-based, BatchMode) and runs every peer's preflight *before* changing
     node 1. The join token travels on stdin, never in an argument.
   - `init` / `join` / `platform` is the same thing, one node at a time,
     for sites without SSH between servers.
6. **Agents, and a single server.** `join --agent` adds a machine as a
   k3s agent: it runs workloads and Longhorn replicas but holds no etcd
   member and no API server, so the etcd quorum stays the three servers'.
   It takes the same server token, by `--token-file`, `--token-stdin` or
   `VELOX_K3S_TOKEN`, and refuses to run without one. `init` and
   `platform --expect-nodes N` (or `install --expect-nodes 1`) also make a
   single-server cluster, with or without agents, for evaluation and small
   sites; it has no control-plane redundancy. This absorbs the
   one-server-plus-agents installer of #192, so there is one from-zero
   script. A separate agent-only token (k3s `agent-token`), so agents never
   hold a secret that admits a server, is a possible follow-up.
7. **The admin stays out of it.** The script ends at "open `/setup`". The
   admin password is chosen in the browser and never passes through a
   script, an argument or a log. cert-manager and the operator stay the
   app's first-run job (bootstrap.rs), as on any cluster.

## Consequences

- The cluster tolerates the loss of any one node. The etcd quorum holds,
  Longhorn serves every volume from a surviving replica, and the console
  Deployment (one replica) reschedules within the node-eviction window.
- Without a VIP or a load balancer in front, users reach the console by any
  node's address. `--tls-san` adds a VIP or DNS name to the API
  certificate. Providing that VIP stays the customer's job; kube-vip is a
  possible follow-up.
- Releases before this ADR do not carry `longhorn.yaml` as an asset. The
  script then reads it at the release's own tag, whose ruleset forbids
  moving it.
- A run under systemd on real machines is the only full test. CI
  shellchecks the script, and the preflight and prerequisite phases are
  exercised in distribution containers.
