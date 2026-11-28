# ADR-075 — The integration install contract: working, waiting for data, or blocked

**Status:** proposed
**Date:** 2026-11-28

## Context

Installing an integration meant writing its objects: ingest pipeline, index
template, index pattern, dashboards, collector. The install returned 200 as
soon as the writes were accepted, and the card said *Installed*. Nothing
checked that the dashboard the user was promised could ever render.

The Kubernetes Audit integration, installed on a stock HA cluster, showed
how wide that gap is. Every panel was broken, for three independent reasons,
and the install reported success:

1. **No data source.** API-server audit logging was off, so there was no
   audit log to read. Where it is on, the log is on the control-plane hosts,
   and on a controller-only control plane (k0s controllers, managed control
   planes) those hosts run no kubelet: no pod can be scheduled there, whatever
   the DaemonSet tolerates.
2. **Index patterns without fields.** The installers saved the pattern with
   only `title` and `timeFieldName`. OpenSearch Dashboards 3.x never fetches
   the fields of a *saved* pattern when it loads one, so every panel failed
   with "Could not locate that index-pattern-field (id: verb)". #198 / #199
   attach `_fields_for_wildcard` at install, but on the 1.x path the index
   does not exist yet at that point, so the list is still empty.
3. **No refresh.** A pattern created before its index existed stayed
   fieldless forever, even with data flowing, and a re-install
   (`overwrite=true`) wiped any list a human had refreshed by hand.

A fourth problem turned up on the same install: the purpose-profile
retention policy every integration index inherits has a `snapshot` state
pointing at `velox-snapshots` whether or not that repository exists. That
step is not a no-op. ISM fails the managed index there and stops, and the
index is never deleted.

The rule this ADR sets: **if a person adds an integration, the button press
does everything needed for it to work. If something outside VeloxSearch's
control prevents that, the install says so precisely, with the exact
remediation, instead of rendering broken panels.**

## Decision

1. **Install is a sequence with a verdict, not a write.**
   provision (pipeline, template, **the index itself**, pattern with fields,
   dashboards, collector) → follow → verify → report. Each step is in
   `integrations::install_backend` / `apply_package` and the new
   `integration_contract` module.

2. **The index is created at install, from its template.** Right after the
   index template is written, the install creates the (empty) index. Its
   mapping is then what `_fields_for_wildcard` reads, so #199's
   `attach_pattern_fields` writes the pattern **with** its fields in the same
   `_bulk_create`, before any document exists. The collector writes into that
   index unchanged. Fields that only dynamic mapping creates (`agent.keyword`
   for nginx, everything for `kubernetes`, which has no template) appear with
   the first documents and are handled by point 4.

3. **Three states, read from the cluster.** `integration_contract::evaluate`
   returns one of:
   - `working`: documents arrive, every field a panel aggregates on is in
     the pattern, and the panels' fields carry values;
   - `waiting_for_data` with `expected` and `source`, e.g. "log lines from
     containers whose log file matches `/var/log/containers/*redis*.log`",
     "from /var/log/containers on every node (the collector DaemonSet)". The
     description is read off the **rendered collector config**, not a table,
     so a registry package that changes its paths is described as it collects;
   - `blocked` with `reason` and `remediation`: a preflight blocker while the
     index is empty, no collector deployed or none schedulable, a panel field
     the documents never carry, or documents in which no panel field (other
     than the time axis) has a value, which means the lines were not parsed.

   The panel fields come from the package's rendered saved objects (each
   visualization's `aggs[].params.field`, grouped by the pattern it
   references), so the verification looks at exactly what was installed.
   Panel *queries* (kuery) are not parsed: a field named only by a query
   breaks the panel's filter, not its rendering.

4. **Evaluate is the reconcile.** When a panel field is missing from the
   saved pattern, `evaluate` reads `_fields_for_wildcard` and writes the list
   onto the pattern (a partial update of `fields` only). It is idempotent and
   runs:
   - after every install, every 15 s for up to 15 minutes, until `working`
     (`follow_install`, one follower per deployment and integration). This is
     what fixes a fresh install when its first documents land, with nobody
     watching;
   - on every `POST /api/integration_status` (the card polls it);
   - once per process start for every installed integration of every
     deployment (`reconcile_on_startup`), so upgrading VeloxSearch heals the
     patterns that earlier versions saved without fields.

5. **Preflight before commit.** `POST /api/integration_preflight` returns
   findings (`blocker` or notice, `reason`, `remediation`) before anything is
   written. The add dialog shows them under the cluster target. They are
   shown, not enforced: a user may install now and enable audit logging
   tomorrow, and the card then moves from `blocked` to `working` on its own.
   Detected today:
   - the app may not create the collector workload in `velox-agents` (SSAR);
   - **k8s-audit:** no node carries a control-plane role label, so no kubelet
     runs where the audit log lives; and `apiserver_audit_event_total` is 0
     or absent in the API server's `/metrics`, meaning audit logging is off.
     This is the one signal every distribution exposes. The runtime
     ClusterRole gains `get` on the non-resource URL `/metrics` for it. The
     remediation gives the flags, a minimal policy, and where each
     distribution takes them (kubeadm, k3s, k0s);
   - container-log sources: no running container whose log file the glob
     matches (a notice: the integration waits for data);
   - host-file sources (`ssh`): journald-only nodes have none of the files
     (a notice; the core cannot read a host's filesystem before it deploys
     a collector).

6. **The install routes answer with the status.** `/catalog_install` and the
   cluster target of `/add_integration` return `status` (the same object as
   `/integration_status`) instead of a bare 200. Never a silent success.

7. **Retention without a dangling step.** The retention policy is written
   `hot → delete` when `velox-snapshots` is not registered on the cluster,
   and `hot → snapshot → delete` when it is. "Current" now includes the
   snapshot step, so configuring snapshots (a save, which re-applies the
   profile) puts the step back, and removing them takes it out. The
   description stays the same so the ADR-062 heuristic still recognises
   velox's own unstamped policy.

## Audit of the catalog (1.x, cluster target)

Desk audit of the twelve packages the registry serves (`catalog.json`,
2026-11-28) against the four failure modes, with what this ADR changes. In
the "fields at install" column, *at install* means the index template maps
every panel field and point 2 resolves them before any document exists;
*on data* means some panel field exists only through dynamic mapping, so
point 4 refreshes the pattern when it appears.

| Integration | Source (from its collector config) | Fields at install | Known blocker → message | Retention |
|---|---|---|---|---|
| k8s-audit | host file `/var/log/kubernetes/audit/audit.log`, written on control-plane hosts | at install | audit logging off → **blocked**, flags + policy + per-distro placement; no control-plane kubelet → **blocked**, ship from the host / `--enable-worker` | fixed (pt. 7) |
| ssh | host files `/var/log/auth.log`, `/var/log/secure` | at install | journald-only nodes → notice; empty index → waiting, names the paths | fixed |
| k8s-events | API event stream (1-replica Deployment) | at install | none known; the app cannot deploy → blocked | fixed |
| kubernetes | all container logs | **on data** (no template) | none known | fixed |
| nginx | `/var/log/containers/*nginx*.log` | on data (`agent.keyword` is dynamic) | no nginx container → notice/waiting; non-default log format → blocked (no panel field parsed) | fixed |
| postgres, redis, mysql, traefik, mongo, rabbitmq, kafka | `/var/log/containers/*<name>*.log` (mysql also `*mariadb*`) | at install | no matching container → notice/waiting; unparsed format → blocked | fixed |

Placement: every collector already tolerates all taints (`tolerations:
[{operator: Exists}]`), so a control-plane node that runs a kubelet does get
a collector pod. The only placement gap is the one no in-cluster change can
close: control-plane hosts without a kubelet. That is a `blocked` with a
host-side remediation, not a fix.

Found during this audit and left open on purpose (no change here):

- **Rollover has no alias.** The `hot` state's `rollover` action needs
  `plugins.index_state_management.rollover_alias`, and the collectors write a
  concrete index rather than an alias. Fixing that changes the index layout
  and the migration of live indices, which needs its own ADR.
- **Spec 2.0 packages (Mesh path, ADR-074)** create their data stream before
  the saved objects, so #199 already gives their patterns fields at install.
  The status contract covers the cluster target of 1.x packages. Extending it
  to Mesh activations, whose sources are on hosts the core cannot inspect, is
  follow-up work.

## Consequences

- The card on the Data tab shows *Working*, *Waiting for data* (what, from
  where) or *Blocked* (why, the remediation), and the pending checks. It
  polls every 15 s until the integration works, then every 60 s.
- A status read writes to Dashboards (the field refresh), so
  `/integration_status` is a TenantScoped, resolve-before-mutate route.
- The core now creates integration indices itself, as the admin. The
  collector account (ADR-070) needs no more than it had, and less on first
  write.
- The startup pass costs one evaluation per installed integration per
  deployment: two Dashboards reads, one search, and, only for an empty index,
  the preflight's node/pod lists and one `/metrics` read.
- On an HA control plane `/metrics` answers from one API server. An
  installation that enables audit logging on some API servers only is
  reported as whichever one answered.
