// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   views_capacity.jsx — "Capacidade" panel (issue: cluster-capacity)

   Host-cluster (K3S) health & capacity: cluster-wide CPU / memory /
   storage gauges, a "how many more deployments fit" estimate, and a
   per-node grid. Polls GET /api/cluster_capacity every 5s and keeps a
   small client-side ring-buffer of samples for the live sparklines —
   no server-side time-series (matches the Overview tab's cadence).

   All bytes/millicores formatting and theming reuse the existing
   design system; the only new primitive is <Gauge> in ui.jsx.
   ============================================================ */
import { useEffect, useRef, useState } from "react";
import { STR, sizeMeta } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Gauge, MiniMeter, StatTile, Icon } from "./ui.jsx";

const MiB = 1024 ** 2, GiB = 1024 ** 3, TiB = 1024 ** 4;
const HIST = 60; // ring-buffer length (~5 min at 5s)

// Short suffixes (T/G/M/K), matching the deployment tiles. These land in tight
// meter captions where the unit was often wider than the number.
function fmtBytes(b) {
  b = b || 0;
  if (b >= TiB) return (b / TiB).toFixed(1) + " T";
  if (b >= GiB) return (b / GiB).toFixed(b >= 10 * GiB ? 0 : 1) + " G";
  if (b >= MiB) return (b / MiB).toFixed(0) + " M";
  return Math.round(b / 1024) + " K";
}
// millicores -> vCPU
function fmtCores(m) {
  const c = (m || 0) / 1000;
  return (c >= 10 ? c.toFixed(0) : c.toFixed(1)) + " vCPU";
}
function pctOf(v, total) { return total > 0 ? (v / total) * 100 : 0; }

// A ResUse axis -> the live value to chart/measure: used when metrics-server is
// answering, otherwise what the scheduler has reserved.
function liveVal(res, metricsOn) {
  if (!res) return 0;
  if (metricsOn && res.used != null) return res.used;
  return res.requested != null ? res.requested : (res.used || 0);
}

// Minimal sparkline (same shape as the Overview's Spark) — kept local so this
// view doesn't depend on the deployment view's internals.
function Spark({ values, max, color }) {
  const n = values.length;
  if (n < 2) return <div className="spark-placeholder" />;
  const hi = max || Math.max(1, ...values);
  const pts = values.map((v, i) => {
    const x = (i / (n - 1)) * 100;
    const y = 28 - Math.max(0, Math.min(1, v / hi)) * 26 - 1;
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  }).join(" ");
  return (
    <svg className="spark" viewBox="0 0 100 28" preserveAspectRatio="none"
      style={{ width: "100%", height: 28, display: "block", marginTop: 8 }}>
      <polyline points={pts} fill="none" stroke={color || "var(--accent)"}
        strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

function fitText(t, f) {
  if (!f || f.count <= 0) return { head: t.cap_fit_none, tone: "red" };
  const head = f.count === 1 ? t.cap_fit_count_one : t.cap_fit_count_n.replace("{n}", f.count);
  return { head, tone: f.count >= 2 ? "green" : "yellow" };
}
function limitText(t, by) {
  return by === "cpu" ? t.cap_limited_cpu : by === "mem" ? t.cap_limited_mem : t.cap_limited_disk;
}

function CapacityView({ lang }) {
  const t = STR[lang];
  const [cap, setCap] = useState(null);
  const [err, setErr] = useState(false);
  const [updated, setUpdated] = useState(null);
  const hist = useRef({ cpu: [], mem: [], nodes: {} }); // ring-buffers

  useEffect(() => {
    let alive = true;
    const push = (arr, v) => { arr.push(v); if (arr.length > HIST) arr.shift(); };
    const load = () => API.clusterCapacity()
      .then(c => {
        if (!alive || !c) return;
        const on = c.metrics_available;
        push(hist.current.cpu, pctOf(liveVal(c.cpu, on), c.cpu.total));
        push(hist.current.mem, pctOf(liveVal(c.mem, on), c.mem.total));
        (c.nodes || []).forEach(n => {
          const a = hist.current.nodes[n.name] || (hist.current.nodes[n.name] = []);
          push(a, pctOf(liveVal(n.cpu, on), n.cpu.total));
        });
        setCap(c);
        setErr(false);
        setUpdated(new Date());
      })
      .catch(() => { if (alive) setErr(true); });
    load();
    const h = setInterval(load, 5000);
    return () => { alive = false; clearInterval(h); };
  }, []);

  if (!cap && err) {
    return (
      <div className="view-enter">
        <h1 className="page-title">{t.cap_title}</h1>
        <p className="hint">{t.cap_error}</p>
      </div>
    );
  }
  if (!cap) {
    return (
      <div className="view-enter">
        <h1 className="page-title">{t.cap_title}</h1>
        <p className="hint">{t.cap_loading}</p>
      </div>
    );
  }

  const on = cap.metrics_available;

  // Cluster-wide storage gauge: persistent pool if present, else aggregate host
  // disk across nodes, else hidden.
  const hostAgg = (cap.nodes || []).reduce((a, n) => {
    if (n.host_disk) { a.total += n.host_disk.total; a.used += n.host_disk.used || 0; }
    return a;
  }, { total: 0, used: 0 });
  let store = null;
  if (cap.storage) store = { used: cap.storage.used, total: cap.storage.total, kind: t.cap_persistent };
  else if (hostAgg.total) store = { used: hostAgg.used, total: hostAgg.total, kind: t.cap_host_disk };

  const cpuLive = liveVal(cap.cpu, on), memLive = liveVal(cap.mem, on);

  return (
    <div className="view-enter">
      <div className="cap-head">
        <div>
          <h1 className="page-title" style={{ marginBottom: 4 }}>{t.cap_title}</h1>
          <p className="lead" style={{ margin: 0 }}>{t.cap_lead}</p>
        </div>
        <div className="cap-live">
          <span className="statusdot green" />
          <span>{t.cap_live}</span>
          {updated && <span className="cap-ts">{t.cap_updated} {updated.toLocaleTimeString(lang === "pt" ? "pt-BR" : lang === "es" ? "es-ES" : "en-US")}</span>}
        </div>
      </div>

      {!on && (
        <div className="cap-notice"><Icon name="activity" size={15} />{t.cap_no_metrics}</div>
      )}

      {/* ── "room for new deployments" — the one actionable answer on this
             page, so it leads instead of sitting between two metric blocks ── */}
      <div className="stat-row">
        {(cap.fit || []).map(f => {
          const ft = fitText(t, f);
          return (
            <StatTile key={f.size}
              label={sizeMeta(f.size).label}
              value={f.count > 0 ? f.count : "0"}
              sub={f.count > 0 ? limitText(t, f.limited_by) : ft.head}
              tone={f.count > 0 ? "ok" : "warn"} />
          );
        })}
      </div>
      <p className="hint" style={{ marginTop: -6, marginBottom: 22 }}>{t.cap_fit_hint}</p>

      {/* ── cluster-wide gauges ── */}
      <h3 className="section-title">{t.cap_title}</h3>
      <div className="cap-gauges">
        <Gauge label={t.cap_cpu} pct={pctOf(cpuLive, cap.cpu.total)}
          sub={`${fmtCores(cpuLive)} / ${fmtCores(cap.cpu.total)}`} />
        <Gauge label={t.cap_mem} pct={pctOf(memLive, cap.mem.total)}
          sub={`${fmtBytes(memLive)} / ${fmtBytes(cap.mem.total)}`} />
        {store && (
          <Gauge label={`${t.cap_storage}`} pct={pctOf(store.used, store.total)}
            sub={`${fmtBytes(store.used)} / ${fmtBytes(store.total)}`} />
        )}
      </div>

      {/* ── per-node rows — same shape as the deployment overview, so a node
             means the same thing on both screens ── */}
      <h3 className="section-title" style={{ marginTop: 30 }}>{t.cap_nodes_h}</h3>
      <div className="node-rows">
        {(cap.nodes || []).map(n => {
          const dot = !n.ready ? "red" : n.pressures.length ? "yellow" : "green";
          const cpuV = liveVal(n.cpu, on), memV = liveVal(n.mem, on);
          const hot = n.pressures.length > 0 || !n.ready;
          return (
            <div className={`node-row ${n.storage ? "wide" : ""} ${hot ? "hot" : ""}`} key={n.name}>
              <div className="nname" style={{ flexDirection: "column", alignItems: "flex-start", gap: 4 }}>
                <span style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
                  <span className={`statusdot ${dot}`} title={n.ready ? t.cap_ready : t.cap_notready} />
                  <span title={n.name}>{n.name}</span>
                </span>
                <span className="cap-roles">
                  {n.roles.map(r => <span className="badge" key={r}>{r}</span>)}
                  {n.pressures.map(p => <span className="badge red" key={p}>{p}</span>)}
                </span>
                <Spark values={hist.current.nodes[n.name] || []} max={100} />
              </div>
              <MiniMeter label={t.cap_cpu} value={`${fmtCores(cpuV)} / ${fmtCores(n.cpu.total)}`} pct={pctOf(cpuV, n.cpu.total)} />
              <MiniMeter label={t.cap_mem} value={`${fmtBytes(memV)} / ${fmtBytes(n.mem.total)}`} pct={pctOf(memV, n.mem.total)} />
              {n.host_disk
                ? <MiniMeter label={t.cap_host_disk} value={`${fmtBytes(n.host_disk.used)} / ${fmtBytes(n.host_disk.total)}`} pct={pctOf(n.host_disk.used, n.host_disk.total)} />
                : <MiniMeter label={t.cap_host_disk} value={t.cap_na} pct={0} />}
              {n.storage && (
                <MiniMeter label={t.cap_persistent} value={`${fmtBytes(n.storage.used)} / ${fmtBytes(n.storage.total)}`} pct={pctOf(n.storage.used, n.storage.total)} />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export { CapacityView };
