// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   api.jsx — REST wrappers + DTO adapter (issue #27)

   Talks to the JSON endpoints that mirror the `#[server]` fns in
   src/app.rs. Path is /api/<endpoint>; SSE is /api/events. Method
   matches the backend router (src/api.rs): pure no-arg reads are GET,
   everything with a body (mutations + arg'd reads) is POST.
     GET  /api/auth_state, bootstrap_status, discover,
          list_deployments, access_settings   (reads; auth_state public)
     SSE  /api/events  -> Vec<ClusterStatus> every 3s   (auth-gated)
     POST everything else, JSON body matching the old fn params

   The adapter maps backend DTOs (ClusterStatus / ClusterMetrics /
   MonitoringStatus) onto the shape the prototype views consume.
   ============================================================ */

const API_BASE = "/api";
const EVENTS_URL = `${API_BASE}/events`;

// One fetch path for every call. Never throws for a *successful* response;
// surfaces a non-2xx as an Error the caller can show in the UI (no
// console.error noise — issue #32 fails on those).
async function call(endpoint, body, method) {
  const m = method || "POST";
  const opts = { method: m, headers: { Accept: "application/json" }, credentials: "same-origin" };
  if (m !== "GET" && m !== "HEAD") {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body || {});
  }
  const res = await fetch(`${API_BASE}/${endpoint}`, opts);
  if (!res.ok) {
    let msg = `request failed (${res.status})`;
    try {
      const txt = await res.text();
      if (txt) {
        try { const j = JSON.parse(txt); msg = j.error || j.message || txt; }
        catch (e) { msg = txt; }
      }
    } catch (e) { /* body already consumed / unreadable */ }
    const err = new Error(msg);
    err.status = res.status;
    throw err;
  }
  // Result<(), _> endpoints return an empty body.
  const txt = await res.text();
  if (!txt) return null;
  try { return JSON.parse(txt); } catch (e) { return txt; }
}

const API = {
  EVENTS_URL,

  // ── auth / boot ──────────────────────────────────────────────
  authState: () => call("auth_state", null, "GET"),
  login: (username, password) => call("login", { username, password }),
  setupAdmin: (username, password, confirm) => call("setup_admin", { username, password, confirm }),
  logout: () => call("logout", {}),

  // ── deployments ──────────────────────────────────────────────
  listDeployments: () => call("list_deployments", null, "GET"),
  // Host-cluster capacity & health (Capacidade panel). -> ClusterCapacity
  clusterCapacity: () => call("cluster_capacity", null, "GET"),
  getDeployment: (name) => call("get_deployment", { name }),
  createCluster: (p) => call("create_cluster", p), // -> final unique name (String)
  saveCluster: (p) => call("save_cluster", p),

  // ── sizing (ADR-016) ─────────────────────────────────────────
  // Presets resolved from the backend's k8s::sizing() (single source of truth);
  // customSizing resolves Advanced inputs (heap = half memory, always 3 nodes).
  sizingPresets: () => call("sizing_presets", null, "GET"), // -> [SizingProfile]
  customSizing: (memory, disk) => call("custom_sizing", { memory, disk }), // -> SizingProfile
  deleteCluster: (name) => call("delete_cluster", { name }),
  nodeStats: (name) => call("node_stats", { name }), // -> ClusterMetrics (empty until green)
  // Cluster-health time-series (#9). window_minutes/buckets optional; the
  // backend defaults to a 60-min look-back at 60 buckets. -> MetricSeries
  // (empty until the sampler has written points).
  metricsSeries: (name, window_minutes, buckets) =>
    call("metrics_series", { name, window_minutes, buckets }),
  dashboardCredentials: (name) => call("dashboard_credentials", { name }), // -> { username, password }
  resetAdminPassword: (name, new_password) => call("reset_admin_password", { name, new_password }),

  // ── version upgrade (ADR-048) ────────────────────────────────
  // Versions the create wizard offers: the newest few confirmed upstream
  // (both images published), or the pinned catalog when offline.
  // -> { versions: [String], default, discovered }
  availableVersions: () => call("available_versions", null, "GET"),
  // What this deployment can upgrade to and why it can't right now. The rules
  // (no downgrade, ≤1 major, green cluster, nothing in flight) live in the
  // backend catalog — the UI only renders what it is handed.
  // -> { version, dashboards_version, targets: [{ version, note }],
  //      blocked_reason, dashboards_behind, upgrade: {…} }
  upgradeOptions: (name) => call("upgrade_options", { name }),
  // Starts the rolling upgrade (nodes first, Dashboards after). Irreversible:
  // the operator rejects downgrades. Returns 400 with the pre-flight's reason
  // and an untouched CR when refused.
  upgradeCluster: (name, version, opts = {}) =>
    call("upgrade_cluster", {
      name,
      version,
      allow_untested: !!opts.allowUntested,
      confirm_unverified: !!opts.confirmUnverified,
    }),

  // ── auth provider (ADR-045) ──────────────────────────────────
  // Which identity provider a deployment's OpenSearch users authenticate
  // against. Credentials are write-only: the GET returns them as the
  // `secret_kept` sentinel, and sending that sentinel back means "unchanged".
  // -> { spec, public_url, kinds_available, redirect_blocked_reason,
  //      builtin_roles, break_glass_user, secret_kept }
  authProvider: (name) => call("auth_provider", { name }),
  saveAuthProvider: (name, spec) => call("save_auth_provider", { name, spec }),
  // Reachability probe. Never writes — a failure leaves the deployment as is.
  // -> { ok, checks: [String], error }
  testAuthProvider: (name, spec) => call("test_auth_provider", { name, spec }),

  // ── deployment activity (ADR-050) ────────────────────────────
  // What the cluster is saying about this deployment right now: Kubernetes
  // Events, pod container state and the operator's componentsStatus. Read-only.
  // NOT container logs — see the ADR. -> [{ at, severity, source, object, title, detail }]
  deploymentActivityLog: (name) => call("deployment_activity_log", { name }),

  // ── snapshot repository + policy (ADR-049) ───────────────────
  // Where a deployment's backups go, and when they are taken. Credentials are
  // write-only, same contract as the auth provider: the GET returns them as the
  // `secret_kept` sentinel and sending it back means "unchanged".
  // -> { config, state, has_credentials, deployment }
  snapshotConfig: (name) => call("snapshot_config", { name }),
  // Dry run: would saving THIS config restart the nodes, and is it allowed?
  // Nothing is written. -> { will_restart, repo, policy }
  planSnapshotConfig: (name, config) => call("plan_snapshot_config", { name, config }),
  saveSnapshotConfig: (name, config) => call("save_snapshot_config", { name, config }),
  // Non-writing `_snapshot/<repo>/_verify` — every node tries to reach the
  // bucket. -> { ok, checks, error }
  verifySnapshotRepo: (name) => call("verify_snapshot_repo", { name }),

  // ── monitoring ───────────────────────────────────────────────
  monitoringStatus: (deployment, recipe) => call("monitoring_status", { deployment, recipe }), // -> { doc_count, receiving }
  discover: () => call("discover", null, "GET"),

  // ── integrations catalog (ADR-039/ADR-047, #75/#76) ──────────
  // The Integrations tab's source of truth. `catalog` ALWAYS resolves — an
  // unreachable registry comes back as source "cache"/"bootstrap" with
  // `stale` + `error` set, which the tab renders as a state rather than a
  // failure. Passing the deployment adds installed_version/update_available
  // per row. -> { schema_version, integrations: [CatalogItem], source,
  //               age_seconds, stale, error }
  catalog: (deployment) => call("catalog", { deployment: deployment || null }),
  // Download → verify signature → apply through the engine → record id@version.
  // `version` is an expectation, not a selector: a mismatch is an error rather
  // than a silent downgrade. Both are Result<(), _> → empty body.
  catalogInstall: (deployment, id, version) =>
    call("catalog_install", { deployment, id, version: version || null }),
  catalogUninstall: (deployment, id) => call("catalog_uninstall", { deployment, id }),
  // ── OTel observability stack (ADR-053) ───────────────────────
  // The SECOND collection option, additive to the recipes above: it ships OTLP
  // traces + metrics, which is what fills the Trace Analytics, service-map and
  // Metric-analytics screens OpenSearch Dashboards already bundles.
  // Static: what it is made of and what it costs, so the panel never hardcodes
  // an image tag or a resource number.
  // -> { version, components: [{ key, image }], cpu_millis, mem_mib, disk_gib }
  otelStackInfo: () => call("otel_stack_info", null, "GET"),
  // `targets` optionally overrides the Prometheus scrape endpoints; omitted
  // means the corresponding job is left out rather than pointed at a guess.
  installOtelStack: (deployment, targets = {}) => call("install_otel_stack", { deployment, ...targets }),
  // `deleteIndices` also drops the telemetry indices — irreversible without a
  // snapshot, so it is an explicit argument rather than a default.
  uninstallOtelStack: (deployment, deleteIndices = false) =>
    call("uninstall_otel_stack", { deployment, delete_indices: !!deleteIndices }),
  // Best-effort: a component still pulling its image is ready:0, never an error.
  // -> { installed, version, components: [{ name, ready, desired }], datasource,
  //      span_docs, otlp_grpc, otlp_http, alertmanager_portforward, error }
  otelStackStatus: (deployment) => call("otel_stack_status", { deployment }),
  // On demand only — the password for the published OTLP/Alertmanager routes
  // must not ride the 15-second status poll.
  otelCredentials: (deployment) => call("otel_stack_credentials", { deployment }),
  // Turns workspaces + the new navigation on or off. Rolls the Dashboards pod
  // and moves where saved objects are scoped (workspaces vs the deployment's
  // tenant), so the caller confirms first.
  setNextUi: (deployment, enabled) => call("set_next_ui", { deployment, enabled: !!enabled }),
  setIpAllowList: (deployment, cidrs) => call("set_ip_allow_list", { deployment, cidrs }),
  // Rolls the collector and Alertmanager pods and invalidates every exporter
  // still holding the old password — the UI confirms before calling it.
  resetOtelCredentials: (deployment) => call("reset_otel_credentials", { deployment }),
  // The only admin-password path: the value is generated, never chosen.
  resetAdminPasswordRandom: (name) => call("reset_admin_password_random", { name }),

  // ── access settings ──────────────────────────────────────────
  getAccessSettings: () => call("access_settings", null, "GET"),
  saveAccessSettings: (mode, base_domain, ingress_class, tls_secret = "", tls_cert = "", tls_key = "") =>
    call("save_access_settings", { mode, base_domain, ingress_class, tls_secret, tls_cert, tls_key }),

  // ── bootstrap / conformity ───────────────────────────────────
  bootstrapStatus: () => call("bootstrap_status", null, "GET"),
  bootstrapEnsure: () => call("bootstrap_ensure", {}),

  // ── storage (ADR-031) ────────────────────────────────────────
  // Read-only classification of the cluster's default storage. The create flow
  // polls it to learn whether creating a cluster will auto-install Longhorn
  // (needs_longhorn) and to show the install progress + completion notice.
  // -> { durable, needs_longhorn, default_class, detail, installing, error,
  //      missing_packages: [{ node, package, reason,
  //                           install: { debian, ubuntu, arch } | null }] }
  storageStatus: () => call("storage_status", null, "GET"),
};

// ─────────────────────────── adapters ──────────────────────────

const MiB = 1024 * 1024;
const GiB = 1024 * 1024 * 1024;

// ClusterStatus -> deployment row/detail shape the views expect.
function adaptDeployment(cs) {
  const monitors = cs.monitors || [];
  return {
    id: cs.name,
    name: cs.name,
    size: cs.size || "",
    purpose: cs.purpose || "observability",
    health: cs.health || "unknown",
    phase: cs.phase || "",
    // ONE node pair for the whole SPA (ADR-050, issue #131). The server now
    // sends `nodes_ready`/`nodes_desired` already agreed with `activity` —
    // ready clamped, over the count the USER asked for
    // (`spec.nodePools[0].replicas`). This used to be three derivations of two
    // facts: the tile divided by `replicas`, the activity panel divided by the
    // StatefulSet's live pod count, and nothing clamped either — which is how
    // "3/3 all ready" and "0/3" and "6/3" ended up on one screen at one moment.
    // `node_count` survives only as the name the views already use.
    node_count: cs.nodes_desired || cs.replicas || 0,
    nodes_ready: cs.nodes_ready ?? 0,
    nodes_desired: cs.nodes_desired || cs.replicas || 0,
    mem: cs.memory || "",
    disk: cs.disk || "",
    heap: cs.heap || "",
    // The deployment's own opensearch.yml additions. The Edit form MUST seed
    // itself with this: sending an empty config prunes the key from the CR
    // (server-side apply), which silently erased custom settings.
    extra_config: cs.extra_config || "",
    monitors,
    // Non-empty = the OTel observability stack is installed here (ADR-053).
    // Rides the SSE stream like everything else, so the Integrations tab knows
    // which panel to render without an extra request.
    otel_stack: cs.otel_stack || "",
    // The next-generation UI (workspaces + new navigation). A deployment-level
    // choice; the observability stack requires it but does not own it, and
    // `next_ui_chosen` is what says whether uninstalling it would revert this.
    next_ui: !!cs.next_ui,
    next_ui_chosen: !!cs.next_ui_chosen,
    // Empty = open, the default posture.
    ip_allow_list: cs.ip_allow_list || [],
    dashboard_url: cs.dashboard_url || null,
    opensearch_url: cs.opensearch_url || null,
    dashboard_portforward: cs.dashboard_portforward || null,
    auth_kind: cs.auth_kind || "internal",
    // Version + live upgrade state (ADR-048). Both come off the CR, so they
    // survive a reload and a backend restart.
    version: cs.version || "",
    target_version: cs.target_version || "",
    dashboards_version: cs.dashboards_version || "",
    upgrade: cs.upgrade || { state: "idle" },
    // Newest upstream release the hourly check found, when this deployment can
    // legally move to it — drives the "Upgrade v3.8.0" tag (ADR-048 rev. 2).
    suggested_version: cs.suggested_version || "",
    // CR creation time — the Status cards render it as a coarse age.
    created_at: cs.created_at || "",
    // Snapshot repository + schedule (ADR-049). Absent means "not configured",
    // which is the default and a valid state — never an error.
    snapshot: cs.snapshot || { configured: false },
    // What this deployment is doing, and whether it settled (ADR-050). The
    // fallback is deliberately "idle": a missing field must never lock the UI
    // shut, same defensive default as `upgrade`.
    // `stalled`/`blocked`/`since_secs`/`serving` ride along (issue #131); the
    // fallback keeps every one of them harmless, since a missing field must
    // never invent a stall or lock the UI shut.
    activity: cs.activity || {
      kind: "idle", stage: "ready", percent: 100, settled: true, locks_edits: false,
      nodes_ready: 0, nodes_total: 0, since_secs: 0, stalled: false, serving: true,
      blocked: { health: "", unassigned_shards: -1, component: "", component_status: "", recovery_index: "", recovery_stage: "", recovery_secs: 0, remediated_node: null },
    },
  };
}

// NodeStat -> the node-card shape (heap in MiB, disk in GiB — matches the
// prototype's OverviewTab math). Guards against zero totals so the metric
// bars never compute NaN widths.
function adaptNodeStat(ns) {
  const heapTotal = Math.max(1, Math.round((ns.heap_max_bytes || 0) / MiB));
  // Disk meter = the data-path PVC (ADR-031), not the node disk. Prefer the
  // PVC capacity when known; fall back to the fs.data total otherwise.
  const totalBytes = ns.pvc_capacity_bytes || ns.disk_total_bytes || 0;
  const diskTotal = Math.max(0.1, +((totalBytes / GiB).toFixed(1)));
  const diskUsed = +((((ns.disk_total_bytes || 0) - (ns.disk_available_bytes || 0)) / GiB).toFixed(1));
  // "" (no PVC info) is treated as bound so we still show the data-path meter;
  // an explicit non-"Bound" phase (e.g. "Pending") shows the phase, not a 0/0 bar.
  const pvcPhase = ns.pvc_phase || "";
  return {
    name: ns.name,
    roles: ns.roles || [],
    cpu: ns.cpu_percent || 0,
    heapUsed: Math.round((ns.heap_used_bytes || 0) / MiB),
    heapTotal,
    diskUsed: Math.max(0, diskUsed),
    diskTotal,
    pvcPhase,
    pvcBound: pvcPhase === "" || pvcPhase === "Bound",
    docs: ns.docs || 0,
  };
}

// ClusterMetrics -> { nodes:[…], total_docs, store_size_bytes }
function adaptMetrics(metrics) {
  return {
    nodes: (metrics?.nodes || []).map(adaptNodeStat),
    total_docs: metrics?.total_docs || 0,
    store_size_bytes: metrics?.store_size_bytes || 0,
  };
}

// MetricSeries -> the sparkline shape the Overview's time-series view consumes
// (disk in GiB to match the node-card meters; cpu/heap stay percent). Guards
// every field so a partial point never renders NaN.
function adaptSeries(series) {
  return {
    deployment: series?.deployment || "",
    points: (series?.points || []).map((p) => ({
      ts: p.ts || 0,
      cpu: p.cpu_percent || 0,
      heap: p.heap_percent || 0,
      diskUsed: +(((p.disk_used_bytes || 0) / GiB).toFixed(2)),
      docs: p.docs || 0,
      rate: p.indexing_rate || 0,
    })),
  };
}

export { API, adaptDeployment, adaptNodeStat, adaptMetrics, adaptSeries };
