// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Deployment detail — one header, six tabs grouped by TASK:

     overview   is it alive and taking data?      node_stats + metrics_series
     data       how does data get in?             catalog + OTel stack
     backup     can I get it back?                snapshot_config
     access     who gets in, from where?          credentials + auth provider + IP list
     config     size, retention, UI, delete       save_cluster + retention + next_ui
     activity   what is it doing right now?       activity + deployment_activity_log

   The old tabs were grouped by implementation (Edit / Security / Auth), so
   "give my team access" meant opening three of them. Delete lives at the
   bottom of Configuration and in the header's ⋯ menu — not under every tab.

   Numbers: one source per fact. The storage tile, the node rows and the
   trend chart all read `capacity_enforced` the same way (#95), so a
   local-path volume can no longer show 2930% on one line and 2 GB on the
   next. Health is a word through <StatusPill>; "green" never reaches the
   screen.
   ============================================================ */
import { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { STR, sizeMeta } from "./i18n.jsx";
import { API, adaptMetrics, adaptSeries } from "./api.jsx";
import {
  Icon, copyText, Copyable, Btn, Confirm, Dialog, Field, FactField, FormSection, MiniMeter, StatTile,
  Banner, Tabs, Menu, PageHeader, StatusPill, healthTone,
  fmtInt, fmtDec, fmtShort, fmtBytes, fmtQty, fmtGib,
} from "./ui.jsx";
import { AuthProviderTab } from "./views_auth_provider.jsx";
import { SnapshotTab } from "./views_snapshot.jsx";
import { ActivityPanel, ActivityDetails, lockReason, LockNotice } from "./views_activity.jsx";
import { relAge } from "./views_status.jsx";

function fmt(s, ...args) {
  return args.reduce((acc, v, i) => acc.replaceAll(`{${i}}`, v), s || "");
}

// Per-node percentages, derived in one place so the row meters and the
// emphasis threshold can never disagree.
function heapPct(n) { return n.heapTotal ? (n.heapUsed / n.heapTotal) * 100 : 0; }
function nodeDiskPct(n) { return n.diskTotal ? (n.diskUsed / n.diskTotal) * 100 : 0; }

// The size name in the user's language; a custom size keeps what the CR says.
function sizeLabel(size, t) { return t[`size_${size}`] || sizeMeta(size).label; }

// Parse a K8s storage quantity to bytes for the resize guard's shrink check.
function parseDisk(s) {
  if (!s) return null;
  const m = String(s).trim().match(/^([\d.]+)\s*(Gi|Mi|Ki|Ti|G|M|K|T)?$/);
  if (!m) return null;
  const mult = {
    Ki: 1024, Mi: 1024 ** 2, Gi: 1024 ** 3, Ti: 1024 ** 4,
    K: 1e3, M: 1e6, G: 1e9, T: 1e12, "": 1,
  }[m[2] || ""];
  return parseFloat(m[1]) * mult;
}

// A "nice" axis ceiling: 1 / 2 / 5 × 10^k just above the peak.
function niceMax(v) {
  if (!(v > 0)) return 1;
  const p = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 2, 5, 10]) if (m * p >= v) return m * p;
  return 10 * p;
}
function hhmm(ts) {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/* --- LineChart: axis, grid, area, time labels. Dependency-free SVG; the
   viewBox leaves room for the outermost labels. --- */
function LineChart({ points, lang, fmtY, height = 170 }) {
  const t = STR[lang];
  if (!points || points.length < 2) return <div className="chart-empty">{t.chart_no_data}</div>;
  const W = 640, H = height, L = 44, R = 12, T = 12, B = 24;
  const maxV = niceMax(Math.max(...points.map(p => p.v)));
  const t0 = points[0].ts, t1 = points[points.length - 1].ts;
  const x = (ts) => L + ((ts - t0) / Math.max(1, t1 - t0)) * (W - L - R);
  const y = (v) => T + (1 - Math.max(0, Math.min(1, v / maxV))) * (H - T - B);
  const path = points.map((p, i) => `${i ? "L" : "M"}${x(p.ts).toFixed(1)} ${y(p.v).toFixed(1)}`).join(" ");
  const last = points[points.length - 1];
  const ticks = [0, 0.5, 1];
  const xt = [0, 1 / 3, 2 / 3, 1].map(f => t0 + f * (t1 - t0));
  return (
    <svg viewBox={`0 0 ${W} ${H}`} role="img" aria-label={`${fmtY(last.v)} · ${hhmm(t0)}–${hhmm(t1)}`}>
      {ticks.map(f => (
        <g key={f}>
          <line className={f === 0 ? "axis" : "grid"} x1={L} x2={W - R} y1={y(f * maxV)} y2={y(f * maxV)} />
          <text x={L - 6} y={y(f * maxV) + 3} textAnchor="end">{fmtY(f * maxV)}</text>
        </g>
      ))}
      <path className="area" d={`${path} L${x(t1).toFixed(1)} ${y(0)} L${x(t0).toFixed(1)} ${y(0)} Z`} />
      <path className="line" d={path} />
      <circle className="end" cx={x(last.ts)} cy={y(last.v)} r="4" />
      {xt.map((ts, i) => (
        <text key={i} x={x(ts)} y={H - 6} textAnchor={i === 0 ? "start" : i === 3 ? "end" : "middle"}>{hhmm(ts)}</text>
      ))}
    </svg>
  );
}

/* --- Sparkline: a trend with no axis, for the secondary cards. --- */
function Spark({ values, max, color }) {
  const n = values.length;
  if (n === 0) return null;
  const hi = max || Math.max(1, ...values);
  const pts = values
    .map((v, i) => {
      const x = n === 1 ? 100 : (i / (n - 1)) * 100;
      const y = 28 - Math.max(0, Math.min(1, v / hi)) * 26 - 1;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg className="spark" viewBox="0 0 100 28" preserveAspectRatio="none"
      style={{ width: "100%", height: 28, display: "block", marginTop: 6 }} aria-hidden="true">
      <polyline points={pts} fill="none" stroke={color || "var(--accent)"}
        strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round"
        vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

// The windows the trend chart offers → (minutes, buckets) for metrics_series.
const WINDOWS = [["1h", 60, 60], ["6h", 360, 72], ["24h", 1440, 96]];

/* --- A REFUSED upgrade only (ADR-048 invariant 4). --- */
function UpgradeFailure({ u, lang }) {
  const t = STR[lang];
  if (!u || u.state !== "failed") return null;
  return (
    <Banner tone="danger" icon="shield" title={t.upg_state_failed} testid="upgrade-failed">
      {u.reason && <pre>{u.reason}</pre>}
    </Banner>
  );
}

/* --- Version upgrade (ADR-048): the header button and its dialog. --- */
function UpgradeControl({ d, lang, onToast, openUpgrade }) {
  const t = STR[lang];
  const [opts, setOpts] = useState(null);
  const [open, setOpen] = useState(!!openUpgrade);
  const [target, setTarget] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [custom, setCustom] = useState("");
  const [ack, setAck] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const state = (d.upgrade && d.upgrade.state) || "idle";

  useEffect(() => { if (openUpgrade) setOpen(true); }, [openUpgrade]);

  useEffect(() => {
    let alive = true;
    API.upgradeOptions(d.id)
      .then(o => {
        if (!alive) return;
        setOpts(o);
        setTarget(prev => prev || o.suggested_version || (o.targets && o.targets[0] ? o.targets[0].version : ""));
      })
      .catch(() => { if (alive) setOpts(null); });
    return () => { alive = false; };
  }, [d.id, d.version, state, d.dashboards_version, d.suggested_version]);

  const version = d.version || (opts && opts.version) || "";
  const targets = (opts && opts.targets) || [];
  const dashBehind = !!(opts && opts.dashboards_behind);
  const suggested = d.suggested_version || (opts && opts.suggested_version) || "";
  const blocked = (opts && opts.blocked_reason) || "";
  const canOffer = targets.length > 0 || dashBehind;
  const chosen = advanced && custom.trim() ? custom.trim() : (dashBehind && !targets.length ? version : target);

  async function submit() {
    setBusy(true);
    setErr("");
    try {
      await API.upgradeCluster(d.id, chosen, { allowUntested: advanced, confirmUnverified: ack });
      setOpen(false);
      setAck(false);
      onToast(t.upg_started);
    } catch (e) {
      setErr(e.message || String(e));
    } finally {
      setBusy(false);
    }
  }

  if (!version) return null;
  return (
    <>
      {canOffer && state === "idle" && (
        <Btn variant="ghost" icon="arrowUp" data-testid="upgrade-open"
          onClick={() => { setErr(""); if (suggested) setTarget(suggested); setOpen(true); }}>
          {dashBehind && !targets.length
            ? t.upg_dash_retry
            : suggested ? `${t.upg_btn} ${suggested}` : t.upg_btn}
        </Btn>
      )}

      <Dialog open={open} onClose={() => !busy && setOpen(false)} title={t.upg_h} width={520} busy={busy} testid="upgrade-dialog">
        {blocked ? (
          <>
            <p className="lead" style={{ marginTop: 0 }}><b>{t.upg_blocked_h}</b></p>
            <p className="hint">{blocked}</p>
          </>
        ) : (
          <>
            {dashBehind && (
              <p className="hint">{fmt(t.upg_dash_behind, version, d.dashboards_version || "?")}</p>
            )}
            {targets.length > 0 && (
              <Field label={t.upg_target} htmlFor="upg-target">
                <select id="upg-target" className="select mono" value={target} disabled={advanced}
                  onChange={e => setTarget(e.target.value)}>
                  {targets.map(o => (
                    <option key={o.version} value={o.version}>
                      {o.version} · {t[`upg_note_${o.note}`] || o.note}
                    </option>
                  ))}
                </select>
              </Field>
            )}

            <h4 className="section-title" style={{ marginTop: 18 }}>{t.upg_blast_h}</h4>
            <p className="hint">{t.upg_blast_p}</p>
            <h4 className="section-title" style={{ marginTop: 14, color: "var(--danger-text)" }}>{t.upg_noback_h}</h4>
            <p className="hint">{t.upg_noback_p}</p>
            <p className="hint" style={{ marginTop: 8 }}>
              {d.snapshot && d.snapshot.configured
                ? fmt(t.upg_snapshot_ok, d.snapshot.schedule || "")
                : t.upg_snapshot_p}
            </p>

            <details style={{ marginTop: 12 }} open={advanced}>
              <summary style={{ cursor: "pointer", fontSize: 13 }}
                onClick={e => { e.preventDefault(); setAdvanced(a => !a); }}>{t.upg_advanced}</summary>
              <div style={{ marginTop: 10 }}>
                <Field label={t.upg_custom} hint={t.upg_custom_hint} htmlFor="upg-custom">
                  <input id="upg-custom" className="input mono" value={custom} placeholder="3.8.0"
                    onChange={e => setCustom(e.target.value)} />
                </Field>
                <label className="check-row" style={{ alignItems: "flex-start", fontSize: 12.5 }}>
                  <input type="checkbox" checked={ack} onChange={e => setAck(e.target.checked)} />
                  <span>{t.upg_unverified}</span>
                </label>
              </div>
            </details>
          </>
        )}

        {err && <p className="field-err" style={{ marginTop: 12 }}>{err}</p>}

        <div className="dialog-actions">
          <Btn variant="outline" onClick={() => setOpen(false)} disabled={busy}>{t.cancel}</Btn>
          <Btn variant="primary" icon="arrowUp" disabled={busy || !!blocked || !chosen} onClick={submit}>
            {t.upg_confirm}
          </Btn>
        </div>
      </Dialog>
    </>
  );
}

/* --- #47: a spent provisioning schedule says so, in place, with the way out. --- */
function ProvisioningBanner({ d, lang, onToast }) {
  const t = STR[lang];
  const p = d.provisioning;
  const [busy, setBusy] = useState(false);
  if (!p || p.state !== "failed") return null;
  const retry = async () => {
    setBusy(true);
    try {
      await API.retryProvisioning(d.id);
      onToast(t.prov_retry_sent);
    } catch {
      onToast(t.prov_retry_fail, false);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Banner tone="warn" title={t.prov_failed_title} testid="prov-banner"
      action={<Btn data-testid="prov-retry" onClick={retry} disabled={busy}>{busy ? t.prov_retrying : t.prov_retry}</Btn>}>
      {fmt(t.prov_failed_body, p.attempts)}
      {p.last_error ? <pre data-testid="prov-last-error">{p.last_error}</pre> : null}
    </Banner>
  );
}

/* --- #26: one copy per volume on a sub-3-node host cluster. Said next to the
   thing that mitigates it (the backup), with the way to it. --- */
function SingleCopyBanner({ d, lang, hostNodes, onTab }) {
  const t = STR[lang];
  if (!(hostNodes.length > 0 && hostNodes.length < 3)) return null;
  const snap = d.snapshot || {};
  const bak = snap.configured
    ? (snap.schedule ? fmt(t.single_copy_bak_sched, snap.schedule) : fmt(t.single_copy_bak, t.snap_chip_on))
    : t.single_copy_nobak;
  return (
    <Banner tone="warn" title={t.single_copy_h} testid="single-copy"
      action={<Btn variant="outline" size="sm" onClick={() => onTab("backup")}>{t.single_copy_cta}</Btn>}>
      {fmt(t.single_copy_p, hostNodes.length)} {bak}
    </Banner>
  );
}

/* ============================================================
   Overview
   ============================================================ */
function OverviewTab({ d, lang, onToast, hostNodes, onTab }) {
  const t = STR[lang];
  const [nodes, setNodes] = useState([]);
  const [totals, setTotals] = useState({ docs: 0, dataBytes: 0, enforced: true });
  const [win, setWin] = useState(0);
  const [points, setPoints] = useState([]);
  const [monitorDocs, setMonitorDocs] = useState(null);
  const [lastOk, setLastOk] = useState(0);
  const [, setTick] = useState(0);
  const url = d.dashboard_url;

  // Live per-node stats. While the deployment is still provisioning
  // OpenSearch isn't answering — stay quiet, but remember WHEN the last good
  // answer came in.
  useEffect(() => {
    let alive = true;
    const load = () => API.nodeStats(d.id)
      .then(m => {
        if (!alive) return;
        const a = adaptMetrics(m);
        setNodes(a.nodes);
        setTotals({ docs: a.total_docs, dataBytes: a.data_used_bytes, enforced: a.capacity_enforced });
        setLastOk(Date.now());
      })
      .catch(() => {});
    load();
    const h = setInterval(load, 5000);
    return () => { alive = false; clearInterval(h); };
  }, [d.id]);

  // The series: feeds the chart, the doc delta and the "receiving" claim
  // (monitor docs, #47) — fetched once per window, never twice.
  useEffect(() => {
    let alive = true;
    const [, minutes, buckets] = WINDOWS[win];
    const load = () => API.metricsSeries(d.id, minutes, buckets)
      .then(s => { if (alive) { const a = adaptSeries(s); setPoints(a.points); setMonitorDocs(a.monitorDocs); } })
      .catch(() => {});
    load();
    const h = setInterval(load, 10000);
    return () => { alive = false; clearInterval(h); };
  }, [d.id, win]);

  useEffect(() => {
    const h = setInterval(() => setTick(x => x + 1), 1000);
    return () => clearInterval(h);
  }, []);

  const last = points.length ? points[points.length - 1] : null;
  const first = points.length ? points[0] : null;
  const rate = last ? last.rate : 0;
  const hasMonitors = monitorDocs !== null;
  const monitorReceiving = hasMonitors && monitorDocs > 0;
  const integCount = (d.monitors || []).length + (d.otel_stack ? 1 : 0);
  // One storage story for the whole tab (#95): under a node-local provisioner
  // the requested capacity is not enforced, so no percentage anywhere.
  const enforced = totals.enforced !== false;
  const reservedGib = nodes.reduce((s, n) => s + (n.pvcBound ? n.diskTotal : 0), 0);
  const usedGib = nodes.reduce((s, n) => s + (n.pvcBound ? n.diskUsed : 0), 0);
  const diskPct = enforced && reservedGib ? (usedGib / reservedGib) * 100 : 0;
  const act = d.activity || { kind: "idle" };
  const nodesOk = d.nodes_ready === d.node_count && d.node_count > 0;
  const nodesSub = !nodesOk
    ? (d.phase || t.ov_nodes_degraded)
    : act.kind === "idle"
      ? t.ov_nodes_ok
      : `${t.ov_nodes_ok} · ${t[`act_badge_${act.kind}`] || ""}`;
  const staleSecs = lastOk ? Math.floor((Date.now() - lastOk) / 1000) : null;
  const stale = staleSecs !== null && staleSecs > 30;
  const docsDelta = first && last ? last.docs - first.docs : 0;

  const chartPts = useMemo(() => points.map(p => ({ ts: p.ts, v: p.rate })), [points]);

  return (
    <div className="view-enter">
      <UpgradeFailure u={d.upgrade} lang={lang} />
      {/* Creation, upgrade and restart share one panel (ADR-050). */}
      <ActivityPanel d={d} lang={lang} />

      {/* The four questions an operator opens this screen to answer. */}
      <div className="stat-row">
        <StatTile label={t.documents} value={fmtInt(totals.docs, lang)} testid="tile-docs"
          sub={points.length > 1 ? fmt(t.ov_docs_delta_window, docsDelta > 0 ? `+${fmtShort(docsDelta, lang)}` : t.steady) : "—"} />
        <StatTile label={t.ov_ingestion} value={last ? fmtDec(rate, lang, 1) : "—"} unit={last ? t.ov_chart_unit : null} testid="tile-rate"
          sub={!hasMonitors ? t.ov_no_monitors
            : (monitorReceiving ? fmt(t.ov_receiving_from, integCount || 1) : t.ov_no_data)}
          tone={hasMonitors ? (monitorReceiving ? "ok" : "warn") : ""} />
        <StatTile label={t.ov_data_stored} value={nodes.length ? fmtBytes(totals.dataBytes, lang) : "—"} testid="tile-storage"
          sub={!nodes.length ? t.ov_awaiting
            : enforced && reservedGib ? `${fmtInt(diskPct, lang)}% ${fmt(t.ov_data_of, fmtGib(reservedGib, lang))}`
            : t.ov_storage_not_enforced}
          tone={enforced && diskPct >= 85 ? "warn" : ""} />
        <StatTile label={t.nodes_h} value={`${d.nodes_ready}/${d.node_count}`} testid="tile-nodes"
          sub={nodesSub}
          tone={nodesOk && act.kind === "idle" ? "ok" : "warn"} />
      </div>

      <div className="ov-grid">
        <div className="card">
          <div className="card-head">
            <h3>{t.ov_chart_h}</h3>
            <span className="sub">{t.ov_chart_unit}</span>
            <div className="right">
              <span className="hint" style={{ color: stale ? "var(--warn-text)" : undefined, fontSize: 12 }}>
                {staleSecs === null ? t.ov_connecting : fmt(t.ov_updated, `${staleSecs}s`)}
              </span>
              <div className="seg" role="group" aria-label={t.ov_window}>
                {WINDOWS.map(([label], i) => (
                  <button key={label} type="button" aria-pressed={win === i} onClick={() => setWin(i)}>{label}</button>
                ))}
              </div>
            </div>
          </div>
          <div className="chart" data-testid="ingestion-chart">
            <LineChart points={chartPts} lang={lang} fmtY={v => fmtDec(v, lang, v >= 10 ? 0 : 1)} />
          </div>
        </div>

        <div className="card">
          <div className="card-head"><h3><Icon name="plug" size={14} /> {t.ov_endpoints_h}</h3></div>
          <div className="ep-list">
            {url && (
              <div className="ep-row">
                <span className="lbl">{t.otel_ep_dashboards}</span>
                <span className="val">
                  <Copyable text={url} t={t} onCopy={onToast} />
                  <a href={url} target="_blank" rel="noreferrer" className="btn-link" style={{ fontSize: 12.5 }}>{t.open}<Icon name="ext" size={12} /></a>
                </span>
              </div>
            )}
            {d.opensearch_url && (
              <div className="ep-row">
                <span className="lbl">{t.otel_ep_opensearch}</span>
                <span className="val"><Copyable text={d.opensearch_url} t={t} onCopy={onToast} /></span>
              </div>
            )}
            {!url && d.dashboard_portforward && (
              <div className="ep-row">
                <span className="lbl">{t.portfwd_access}</span>
                <span className="val"><Copyable text={d.dashboard_portforward} t={t} onCopy={onToast} /></span>
              </div>
            )}
            <div className="ep-row">
              <span className="lbl">{t.ov_credentials}</span>
              <span className="val"><button type="button" className="btn-link" onClick={() => onTab("access")}>{t.ov_see_access} <Icon name="chevR" size={12} /></button></span>
            </div>
          </div>
        </div>
      </div>

      {/* One row per node: three meters side by side read as a comparison. A
          cell is emphasised only past the bar's own threshold — never the
          whole row, and never because of a number that means nothing here. */}
      <div className="card" style={{ marginBottom: 14 }}>
        <div className="card-head">
          <h3>{t.nodes_h}</h3>
          <span className="sub">{fmt(t.ov_nodes_ready_of, d.nodes_ready, d.node_count)}</span>
        </div>
        {nodes.length === 0 ? (
          <p className="hint" style={{ padding: "14px 16px" }}>{t.ov_gathering}</p>
        ) : (
          <table className="dtable node-table" data-testid="node-table">
            <thead>
              <tr><th>{t.cap_node}</th><th>{t.cpu}</th><th>{t.heap}</th><th>{enforced ? t.ov_node_data : t.ov_node_docs}</th><th>{t.ov_node_status}</th></tr>
            </thead>
            <tbody>
              {nodes.map(n => {
                const hp = heapPct(n);
                const dp = nodeDiskPct(n);
                return (
                  <tr key={n.name}>
                    <td className="nm" data-label={t.cap_node} title={n.name}>{n.name}</td>
                    <td data-label={t.cpu}><MiniMeter label="" value={`${fmtInt(n.cpu, lang)}%`} pct={n.cpu} /></td>
                    <td data-label={t.heap}><MiniMeter label="" value={`${fmtInt(hp, lang)}%`} pct={hp} /></td>
                    <td data-label={enforced ? t.ov_node_data : t.ov_node_docs}>
                      {enforced
                        ? (n.pvcBound
                          ? <MiniMeter label="" value={`${fmtGib(n.diskUsed, lang)} / ${fmtGib(n.diskTotal, lang)}`} pct={dp} />
                          : <MiniMeter label="" value={n.pvcPhase || t.ov_node_pending} pct={null} />)
                        : <span className="num">{fmtInt(n.docs, lang)}</span>}
                    </td>
                    <td data-label={t.ov_node_status}>
                      <span className={`pill sm ${n.pvcBound && d.health !== "red" ? "green" : "yellow"}`}>
                        <span className="dot" />{n.pvcBound ? t.ov_node_ready : (n.pvcPhase || t.ov_node_pending)}
                      </span>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>

      {/* Trends: the follow-up question, never the first one. */}
      <h3 className="section-title" style={{ marginTop: 22 }}>{t.mt_h} <span style={{ textTransform: "none", letterSpacing: 0, fontWeight: 400 }}>· {WINDOWS[win][0]}</span></h3>
      {points.length < 2 ? (
        <p className="hint" data-testid="metrics-timeseries">{t.mt_empty}</p>
      ) : (
        <div className="spark-grid" data-testid="metrics-timeseries">
          {[
            { label: t.cpu, val: `${fmtInt(last.cpu, lang)}%`, values: points.map(p => p.cpu), max: 100 },
            { label: t.heap, val: `${fmtInt(last.heap, lang)}%`, values: points.map(p => p.heap), max: 100 },
            enforced
              ? { label: t.mt_disk, val: fmtGib(last.diskUsed, lang), values: points.map(p => p.diskUsed) }
              : { label: t.documents, val: fmtShort(last.docs, lang), values: points.map(p => p.docs) },
            { label: t.mt_indexing, val: `${fmtDec(last.rate, lang, 1)} ${t.ov_chart_unit}`, values: points.map(p => p.rate) },
          ].map(c => (
            <div className="card spark-card" key={c.label}>
              <div className="metric-head">
                <span className="lbl">{c.label}</span>
                <span className="val tnum">{c.val}</span>
              </div>
              <Spark values={c.values} max={c.max} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/* ============================================================
   Configuration
   ============================================================ */
// Retention (ADR-062): what this deployment keeps, and whether its user has
// customized the policy inside OpenSearch.
function RetentionPanel({ d, lang, locked }) {
  const t = STR[lang];
  const [st, setSt] = useState(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [err, setErr] = useState("");

  useEffect(() => {
    let live = true;
    API.retentionStatus(d.id)
      .then(s => { if (live) setSt(s); })
      .catch(e => { if (live) setErr(e.message); });
    return () => { live = false; };
  }, [d.id]);

  async function reset() {
    setConfirm(false);
    setBusy(true);
    setErr("");
    try { setSt(await API.resetRetention(d.id)); }
    catch (e) { setErr(e.message); }
    finally { setBusy(false); }
  }

  const days = n => t.ret_days_value.replaceAll("{0}", String(n));
  return (
    <div data-testid="retention-panel">
      {!st && !err && <p className="hint">{t.ret_loading}</p>}
      {st && (
        <>
          <div className="kvrow"><span className="k">{t.ret_label}</span>
            <span className="v" data-testid="retention-days">{st.days ? days(st.days) : t.ret_forever}</span>
          </div>
          {st.state !== "none" && (
            <div className="kvrow"><span className="k">{t.ret_state}</span>
              <span className="v sans" data-testid="retention-state" title={st.detail || undefined}>
                {t["ret_state_" + st.state] || st.state}
              </span>
            </div>
          )}
          {st.source && (
            <div className="kvrow"><span className="k">{t.ret_source}</span>
              <span className="v sans" data-testid="retention-source">{t["ret_source_" + st.source] || st.source}</span>
            </div>
          )}
          {(st.otel || []).length > 0 && (
            <div data-testid="retention-otel">
              <div className="kvrow"><span className="k">{t.ret_otel_h}</span><span className="v" /></div>
              {st.otel.map(o => (
                <div className="kvrow" key={o.policy_id}>
                  <span className="k mono">{o.policy_id}</span>
                  <span className="v sans" title={o.detail || undefined}>
                    {t["ret_state_" + o.state] || o.state}{o.days ? ` · ${days(o.days)}` : ""}
                  </span>
                </div>
              ))}
            </div>
          )}
          {(st.state === "customized" || (st.otel || []).some(o => o.state === "customized")) &&
            <p className="hint" style={{ marginTop: 10 }}>{t.ret_customized_p}</p>}
          {st.days && <p className="hint" style={{ marginTop: 8 }} data-testid="retention-when">{t.ret_when_p}</p>}
          {st.default_days && (
            <Btn variant="outline" disabled={busy || !!locked} style={{ marginTop: 12 }}
              data-testid="retention-reset" onClick={() => setConfirm(true)}>
              {busy ? t.saving : `${t.ret_reset} · ${days(st.default_days)}`}
            </Btn>
          )}
        </>
      )}
      {err && <p className="field-err">{err}</p>}
      <Confirm open={confirm} title={t.ret_reset} body={t.ret_reset_confirm}
        confirmLabel={t.ret_reset} cancelLabel={t.cancel} icon="check" variant="primary"
        confirmTestid="retention-reset-confirm"
        onCancel={() => setConfirm(false)} onConfirm={reset} />
    </div>
  );
}

// The next-generation Dashboards UI: a deployment-level choice (ADR-053).
function NextUiPanel({ d, lang, onToast, locked }) {
  const t = STR[lang];
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [confirm, setConfirm] = useState(false);
  const on = !!d.next_ui;
  const heldByStack = on && !!d.otel_stack;

  async function apply() {
    setConfirm(false);
    setBusy(true);
    setErr("");
    try {
      await API.setNextUi(d.id, !on);
      onToast(!on ? t.nextui_on_done : t.nextui_off_done);
    } catch (e) {
      setErr(e.message || String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap", marginBottom: 8 }}>
        <span className={`badge ${on ? "green" : ""}`}>{on ? t.nextui_state_on : t.nextui_state_off}</span>
        <span className="hint">{on ? t.nextui_scope_ws : t.nextui_scope_tenant}</span>
      </div>
      {heldByStack && <p className="hint" style={{ marginBottom: 8 }}>{t.nextui_held}</p>}
      <Btn icon={on ? "chevL" : "check"} variant={on ? "outline" : "primary"}
        disabled={busy || !!locked || heldByStack}
        onClick={() => setConfirm(true)}>
        {busy ? t.saving : (on ? t.nextui_disable : t.nextui_enable)}
      </Btn>
      {err && <div className="field-err">{err}</div>}
      <Confirm open={confirm} title={on ? t.nextui_disable : t.nextui_enable}
        confirmLabel={on ? t.nextui_disable : t.nextui_enable}
        cancelLabel={t.cancel} icon="layers" variant={on ? "danger" : "primary"}
        onCancel={() => setConfirm(false)} onConfirm={apply}>
        <p className="hint">{on ? t.nextui_confirm_off : t.nextui_confirm_on}</p>
        <p className="hint">{t.nextui_confirm_roll}</p>
      </Confirm>
    </div>
  );
}

function ConfigTab({ d, lang, onSave, onToast, locked, onDeleteRequest }) {
  const t = STR[lang];
  const preset = sizeMeta(d.size);
  const [size, setSize] = useState(["small", "medium", "large"].includes(d.size) ? d.size : "small");
  const [mem, setMem] = useState(d.mem || preset.mem);
  const [disk, setDisk] = useState(d.disk || preset.disk);
  const [extra, setExtra] = useState(d.extra_config || "");
  const [showAdv, setShowAdv] = useState(!!(d.extra_config || "").trim());
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  // The JVM heap is operator-managed (#55/ADR-035): resolved from the backend
  // so the UI never computes the rule.
  const [jvmHeap, setJvmHeap] = useState(d.heap || sizeMeta(size).heap);
  useEffect(() => {
    let live = true;
    API.customSizing(mem, "")
      .then((p) => { if (live && p && p.heap) setJvmHeap(p.heap); })
      .catch(() => {});
    return () => { live = false; };
  }, [mem]);

  function pickSize(k) {
    setSize(k);
    const m = sizeMeta(k);
    setMem(m.mem);
    setDisk(m.disk);
  }

  // Resize guard (#16): disk can only grow — the backend enforces the same.
  const curBytes = parseDisk(d.disk);
  const newBytes = parseDisk(disk);
  const diskShrink = curBytes != null && newBytes != null && newBytes < curBytes;

  const base = ["small", "medium", "large"].includes(d.size) ? d.size : "small";
  const dirty = size !== base
    || mem !== (d.mem || preset.mem)
    || disk !== (d.disk || preset.disk)
    || extra !== (d.extra_config || "");

  function reset() {
    setSize(base);
    setMem(d.mem || preset.mem);
    setDisk(d.disk || preset.disk);
    setExtra(d.extra_config || "");
  }

  async function save() {
    if (diskShrink) return;
    setBusy(true);
    try {
      await onSave({
        name: d.id, size, purpose: d.purpose,
        nodes: String(d.node_count || preset.nodes),
        memory: mem, disk, config: extra,
        monitors: (d.monitors || []).join(","),
      });
    } catch (e) { /* the shell toasted it */ }
    finally { setBusy(false); }
  }

  return (
    <div className="view-enter">
      <LockNotice reason={locked} />

      <FormSection title={t.cfg_h_size} desc={t.cfg_d_size} testid="cfg-size"
        actions={<>
          <Btn variant="primary" icon="check" disabled={busy || diskShrink || !dirty || !!locked}
            data-testid="edit-save" onClick={() => setConfirm(true)}>{busy ? t.saving : t.save}</Btn>
          {dirty && <Btn variant="outline" disabled={busy} onClick={reset}>{t.discard}</Btn>}
          {dirty && <span className="hint">{t.cfg_unsaved}</span>}
        </>}>
        <Field label={t.size} htmlFor="cfg-size">
          <select id="cfg-size" className="select" value={size} onChange={e => pickSize(e.target.value)}>
            {["small", "medium", "large"].map(k => <option key={k} value={k}>{sizeLabel(k, t)} · {fmtQty(sizeMeta(k).mem, lang)} · {fmtQty(sizeMeta(k).disk, lang)}</option>)}
          </select>
        </Field>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: 14 }}>
          <Field label={t.node_mem} tip={t.node_mem_hint} htmlFor="cfg-mem">
            <input id="cfg-mem" className="input mono" value={mem} onChange={e => setMem(e.target.value)} />
          </Field>
          <Field label={t.disk} hint={t.disk_hint} htmlFor="cfg-disk" error={diskShrink ? t.disk_shrink_err.replace(/\{cur\}/g, d.disk) : ""}>
            <input id="cfg-disk" className={`input mono${diskShrink ? " invalid" : ""}`} value={disk} onChange={e => setDisk(e.target.value)} />
          </Field>
        </div>
        {/* Facts, not disabled inputs: a disabled field says "you could change
            this"; a fact says you cannot, and why. */}
        <FactField label={t.node_count} value={<span data-testid="edit-nodes">{fmt(t.cfg_nodes_fact, d.node_count || preset.nodes)}</span>} />
        <FactField label={t.jvm} value={<span data-testid="edit-heap"><b>{fmtQty(jvmHeap, lang)}</b> <span className="hint" style={{ display: "inline" }}>{t.cfg_heap_fact.replace("{0}", "")}</span></span>} />
      </FormSection>

      <FormSection title={t.cfg_h_adv} desc={t.cfg_d_adv} testid="cfg-advanced">
        {!showAdv ? (
          <button type="button" className="btn-link" onClick={() => setShowAdv(true)}><Icon name="chevR" size={13} />{t.cfg_show_adv}</button>
        ) : (
          <>
            <Field label={t.extra} hint={t.extra_hint} htmlFor="cfg-extra">
              <textarea id="cfg-extra" className="textarea mono" value={extra} onChange={e => setExtra(e.target.value)}
                placeholder={"indices.query.bool.max_clause_count: 2048"} />
            </Field>
            {!extra.trim() && <button type="button" className="btn-link" onClick={() => setShowAdv(false)}><Icon name="chevD" size={13} />{t.cfg_hide_adv}</button>}
          </>
        )}
      </FormSection>

      <FormSection title={t.cfg_h_ret} desc={t.cfg_d_ret} testid="cfg-retention">
        <RetentionPanel d={d} lang={lang} locked={locked} />
      </FormSection>

      <FormSection title={t.cfg_h_ui} desc={t.cfg_d_ui} testid="cfg-ui">
        <NextUiPanel d={d} lang={lang} onToast={onToast} locked={locked} />
      </FormSection>

      {/* Never locked, by decision (ADR-050 invariant 4): a provision that
          never finishes is exactly when the user most needs to remove it. */}
      <FormSection title={<span style={{ color: "var(--danger-text)" }}>{t.cfg_h_danger}</span>} desc={t.cfg_d_danger} testid="cfg-danger">
        <Btn variant="danger" icon="trash" data-testid="delete-deployment" onClick={onDeleteRequest}>{t.delete_dep}</Btn>
        {locked && <p className="hint" style={{ marginTop: 8 }}>{t.act_delete_ok}</p>}
      </FormSection>

      {/* A save restarts the nodes: state the blast radius before acting. */}
      <Confirm open={confirm} title={t.edit_confirm_h} body={t.edit_confirm_p}
        confirmLabel={t.save} cancelLabel={t.cancel} icon="check" variant="primary"
        onCancel={() => setConfirm(false)}
        onConfirm={() => { setConfirm(false); save(); }} />
    </div>
  );
}

/* ============================================================
   Data & integrations
   ============================================================ */
const INTEG_POLL_MS = 15000;

// Integration id → catalog category. The catalog is RUNTIME data (#76); an
// unknown id lands in "other" rather than hiding.
const INTEG_CAT = {
  kubernetes: "k8s", "k8s-events": "k8s",
  postgres: "db", mysql: "db", mongo: "db", redis: "db",
  kafka: "mq", rabbitmq: "mq",
  nginx: "web", traefik: "web",
  ssh: "sec", "k8s-audit": "sec",
};
const CATS = ["all", "k8s", "db", "mq", "web", "sec", "other"];
function monogram(id, title) {
  const base = (title || id || "?").replace(/[^A-Za-z0-9 ]/g, " ").trim();
  const parts = base.split(/\s+/);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return base.slice(0, 2);
}

function EndpointRow({ t, label, value, href, onCopy }) {
  if (!value) return null;
  return (
    <div className="ep-row">
      <span className="lbl">{label}</span>
      <span className="val">
        <Copyable text={value} t={t} onCopy={onCopy} />
        {href && <a href={href} target="_blank" rel="noreferrer" className="btn-link" style={{ fontSize: 12.5 }}>{t.open}<Icon name="ext" size={12} /></a>}
      </span>
    </div>
  );
}

// The credential the published OTLP endpoints check. Behind a reveal, fetched
// only when asked.
function OtelCredentials({ d, t, user, onToast, locked }) {
  const [creds, setCreds] = useState(null);
  const [show, setShow] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [confirming, setConfirming] = useState(false);

  async function reveal() {
    if (creds) { setShow(s => !s); return; }
    setBusy(true); setErr("");
    try {
      const c = await API.otelCredentials(d.id);
      if (c && c.password) { setCreds(c); setShow(true); }
      else setErr(t.otel_cred_missing);
    } catch (e) {
      setErr(e.message || t.otel_cred_missing);
    } finally {
      setBusy(false);
    }
  }

  async function reset() {
    setBusy(true); setErr("");
    try {
      const c = await API.resetOtelCredentials(d.id);
      setCreds(c); setShow(true);
      onToast(t.otel_cred_reset_done);
    } catch (e) {
      setErr(e.message || String(e));
    } finally {
      setBusy(false); setConfirming(false);
    }
  }

  return (
    <div style={{ display: "grid", gap: 8 }}>
      <div className="ep-row">
        <span className="lbl">{t.cred_user}</span>
        <span className="val"><Copyable text={(creds && creds.username) || user} t={t} onCopy={onToast} /></span>
      </div>
      <div className="ep-row">
        <span className="lbl">{t.cred_pass}</span>
        <span className="val">
          {creds && show
            ? <Copyable text={creds.password} t={t} onCopy={onToast} />
            : <span className="copyfield"><code>{"•".repeat(12)}</code></span>}
          <Btn variant="outline" size="sm" icon={show ? "eyeOff" : "eye"} disabled={busy} onClick={reveal}>
            {busy ? t.cred_loading : creds ? (show ? t.hide : t.show) : t.cred_reveal}
          </Btn>
          <Btn variant="outline" size="sm" icon="shield" disabled={busy || !!locked} onClick={() => setConfirming(true)}>
            {t.otel_cred_reset}
          </Btn>
        </span>
      </div>
      {err && <div className="field-err">{err}</div>}
      <Confirm open={confirming} icon="shield" title={t.otel_cred_reset_h} body={t.otel_cred_reset_p}
        confirmLabel={t.otel_cred_reset} cancelLabel={t.cancel}
        onCancel={() => setConfirming(false)} onConfirm={reset}>
        <ul style={{ margin: "4px 0 10px 18px", fontSize: 12.5, color: "var(--text-2)", lineHeight: 1.7 }}>
          <li>{t.otel_cred_reset_b1}</li>
          <li>{t.otel_cred_reset_b2}</li>
          <li>{t.otel_cred_reset_b3}</li>
        </ul>
      </Confirm>
    </div>
  );
}

/* --- The OTel observability stack (ADR-053): a compact card with the value
   and the cost; the component list and the addresses open on demand. --- */
function ObservabilityStackPanel({ d, lang, locked, onToggle, onToast }) {
  const t = STR[lang];
  const dashUrl = d.dashboard_url || "";
  const [info, setInfo] = useState(null);
  const [st, setSt] = useState(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [dropData, setDropData] = useState(false);
  const [details, setDetails] = useState(false);
  const installed = !!d.otel_stack;

  useEffect(() => {
    let alive = true;
    API.otelStackInfo().then(i => alive && setInfo(i)).catch(() => {});
    return () => { alive = false; };
  }, []);

  useEffect(() => {
    if (!installed) { setSt(null); return; }
    let alive = true;
    const load = async () => {
      if (document.hidden) return;
      try {
        const s = await API.otelStackStatus(d.id);
        if (alive) setSt(s);
      } catch { /* still starting is not an error worth showing */ }
    };
    load();
    const h = setInterval(load, INTEG_POLL_MS);
    return () => { alive = false; clearInterval(h); };
  }, [d.id, installed]);

  async function act(install, deleteIndices = false) {
    setBusy(true); setErr("");
    try { await onToggle(d.id, install, deleteIndices); }
    catch (e) { setErr(e.message || String(e)); }
    finally { setBusy(false); }
  }

  const cost = info
    ? t.otel_cost
        .replace("{0}", fmtDec(info.cpu_millis / 1000, lang, 1))
        .replace("{1}", fmtDec(info.mem_mib / 1024, lang, 1) + " GiB")
        .replace("{2}", info.disk_gib)
    : "";
  const compLabel = {
    collector: t.otel_comp_collector,
    "data-prepper": t.otel_comp_data_prepper,
    cortex: t.otel_comp_cortex,
    alertmanager: t.otel_comp_alertmanager,
    "os-exporter": t.otel_comp_osexporter,
  };

  return (
    <div className={`card${installed ? " on" : ""}`} data-testid="otel-stack" data-installed={installed ? "true" : "false"} style={{ marginBottom: 18 }}>
      <div className="otel-card">
        <div style={{ minWidth: 0 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
            <b style={{ fontWeight: 600 }}>{t.otel_h}</b>
            <span className={`badge ${installed ? "green" : ""}`}>{installed ? t.otel_installed : t.otel_not_installed}</span>
            {!installed && <span className="badge blue">{t.otel_recommended}</span>}
          </div>
          <div style={{ color: "var(--text-2)", marginTop: 4, fontSize: 13.5 }}>{t.otel_lead}</div>
          {!installed && info && <div className="cost">{t.otel_cost_h}: {cost}</div>}
          {installed && st && (
            <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginTop: 8 }}>
              {(st.components || []).map(c => (
                <span key={c.name} className={`badge ${c.ready >= 1 ? "green" : ""}`}>
                  <Icon name={c.ready >= 1 ? "check" : "clock"} size={11} />{(compLabel[c.name] || c.name).split(" — ")[0]}
                </span>
              ))}
              <span className={`badge ${st.span_docs > 0 ? "green" : ""}`}>
                {st.span_docs > 0 ? `${fmtInt(st.span_docs, lang)} ${t.otel_spans}` : t.integ_waiting}
              </span>
            </div>
          )}
        </div>
        <div className="acts">
          <Btn variant="outline" size="sm" icon={details ? "chevD" : "chevR"} onClick={() => setDetails(x => !x)} data-testid="otel-details">
            {details ? t.otel_hide_details : t.otel_details}
          </Btn>
          {installed ? (
            <Btn variant="danger" size="sm" icon="trash" disabled={busy || !!locked} onClick={() => setConfirming(true)} data-testid="otel-uninstall">
              {t.otel_uninstall}
            </Btn>
          ) : (
            <Btn variant="primary" size="sm" icon="plus" disabled={busy || !!locked} onClick={() => act(true)} data-testid="otel-install">
              {busy ? t.otel_installing : t.otel_install}
            </Btn>
          )}
        </div>
      </div>

      {err && <div className="field-err" style={{ padding: "0 16px 12px" }}>{err}</div>}

      {details && (
        <div style={{ borderTop: "1px solid var(--border)", padding: "14px 16px", display: "grid", gap: 18 }}>
          {!installed && (
            <>
              <div>
                <div className="section-title">{t.otel_adds}</div>
                <ul style={{ margin: 0, paddingLeft: 18, fontSize: 13, color: "var(--text-2)", display: "grid", gap: 3 }}>
                  <li>{t.otel_add_traces}</li><li>{t.otel_add_agents}</li><li>{t.otel_add_metrics}</li><li>{t.otel_add_alerts}</li>
                </ul>
              </div>
              {info && (
                <div>
                  <div className="section-title">{t.otel_comp_h}</div>
                  <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12.5, color: "var(--text-3)", display: "grid", gap: 3 }}>
                    {info.components.map(c => (
                      <li key={c.key}>{compLabel[c.key] || c.key} <span className="mono" style={{ fontSize: 11.5 }}>{c.image}</span></li>
                    ))}
                  </ul>
                </div>
              )}
            </>
          )}
          {installed && st && (
            <>
              <div>
                <div className="section-title">{t.otel_send_h}</div>
                <p className="hint" style={{ marginBottom: 10 }}>{t.otel_send_p}</p>
                <div style={{ display: "grid", gap: 8 }}>
                  <EndpointRow t={t} label={t.otel_ep_otlp_logs} value={st.otlp_logs_url} onCopy={onToast} />
                  <EndpointRow t={t} label={t.otel_ep_otlp_metrics} value={st.otlp_metrics_url} onCopy={onToast} />
                  <EndpointRow t={t} label={t.otel_ep_otlp_http} value={st.otlp_traces_url} onCopy={onToast} />
                </div>
                <p className="hint" style={{ marginTop: 8 }}>{t.otel_send_how}</p>
              </div>
              <div>
                <div className="section-title">{t.otel_cred_h}</div>
                <p className="hint" style={{ marginBottom: 10 }}>{t.otel_cred_p}</p>
                <OtelCredentials d={d} t={t} user={st.otlp_user} onToast={onToast} locked={locked} />
              </div>
              <div>
                <div className="section-title">{t.otel_screens_h}</div>
                <p className="hint" style={{ marginBottom: 10 }}>{t.otel_screens_p}</p>
                {dashUrl && st.workspace_path && (
                  <EndpointRow t={t} label={t.otel_ep_workspace} value={dashUrl + st.workspace_path} href={dashUrl + st.workspace_path} onCopy={onToast} />
                )}
                <ul style={{ margin: "10px 0 0 18px", fontSize: 12.5, color: "var(--text-2)", lineHeight: 1.7 }}>
                  <li>{t.otel_screen_traces}</li><li>{t.otel_screen_agent}</li><li>{t.otel_screen_metrics}</li>
                  <li>{t.otel_screen_alerts}</li><li>{t.otel_screen_boards}</li>
                </ul>
              </div>
            </>
          )}
        </div>
      )}

      <Confirm open={confirming} title={t.otel_uninstall_h} body={t.otel_uninstall_p}
        confirmLabel={t.otel_uninstall} cancelLabel={t.cancel}
        requireText={d.id} requireLabel={fmt(t.otel_uninstall_type, d.id)}
        onCancel={() => { setConfirming(false); setDropData(false); }}
        onConfirm={() => { setConfirming(false); act(false, dropData); setDropData(false); }}>
        <label className="check-row" style={{ alignItems: "flex-start", margin: "4px 0 12px", color: "var(--text-2)" }}>
          <input type="checkbox" checked={dropData} style={{ marginTop: 3 }} onChange={e => setDropData(e.target.checked)} />
          <span>
            {t.otel_uninstall_drop}
            <span style={{ display: "block", color: dropData ? "var(--danger-text)" : "var(--text-3)", fontSize: 12, marginTop: 3 }}>
              {dropData ? t.otel_uninstall_drop_on : t.otel_uninstall_drop_off}
            </span>
          </span>
        </label>
      </Confirm>
    </div>
  );
}

function loc(map, lang, alt) {
  if (!map) return alt;
  return map[lang] || map.en || Object.values(map)[0] || alt;
}

function fmtAge(sec, t) {
  if (sec == null) return "—";
  if (sec < 5) return t.integ_age_now;
  if (sec < 90) return t.integ_age_s.replace("{n}", String(sec));
  if (sec < 5400) return t.integ_age_m.replace("{n}", String(Math.round(sec / 60)));
  return t.integ_age_h.replace("{n}", String(Math.round(sec / 3600)));
}

// The runtime catalog (ADR-039 / ADR-047, #76). Installed first, then the
// catalog in a grid with search and categories. The degraded-registry states
// are RENDERED, not hidden.
function DataTab({ d, lang, onToggleStack, onToast, locked }) {
  const t = STR[lang];
  const [view, setView] = useState(null);
  const [loadErr, setLoadErr] = useState("");
  const [status, setStatus] = useState({});
  const [busy, setBusy] = useState({});
  const [itemErr, setItemErr] = useState({});
  const [q, setQ] = useState("");
  const [cat, setCat] = useState("all");

  const reload = useCallback(async () => {
    try {
      const v = await API.catalog(d.id);
      setView(v);
      setLoadErr("");
    } catch (e) {
      setLoadErr(e.message);
    }
  }, [d.id]);

  useEffect(() => { reload(); }, [reload]);

  const items = view ? view.integrations || [] : [];
  const monitors = d.monitors || [];
  const isInstalled = (it) => it.installed_version != null || monitors.includes(it.id);
  const installedSig = items.filter(isInstalled).map(it => it.id).join(",");

  useEffect(() => {
    if (!installedSig) return undefined;
    let alive = true;
    const load = async () => {
      if (document.hidden) return;
      for (const k of installedSig.split(",")) {
        try {
          const s = await API.monitoringStatus(d.id, k);
          if (!alive) return;
          setStatus(c => ({ ...c, [k]: s }));
        } catch {
          if (!alive) return;
          setStatus(c => ({ ...c, [k]: undefined }));
        }
      }
    };
    load();
    const h = setInterval(load, INTEG_POLL_MS);
    return () => { alive = false; clearInterval(h); };
  }, [d.id, installedSig]);

  async function act(id, kind, version) {
    setBusy(b => ({ ...b, [id]: kind }));
    setItemErr(e => ({ ...e, [id]: "" }));
    try {
      if (kind === "uninstall") await API.catalogUninstall(d.id, id);
      else await API.catalogInstall(d.id, id, version);
      onToast(kind === "uninstall" ? t.integ_uninstalled_toast : t.integ_installed_toast);
      await reload();
    } catch (e) {
      setItemErr(er => ({ ...er, [id]: e.status === 404 ? t.integ_dep_gone : e.message }));
    } finally {
      setBusy(b => ({ ...b, [id]: null }));
    }
  }

  if (loadErr) {
    return (
      <div className="view-enter" data-testid="integrations-tab">
        <p className="lead" style={{ marginBottom: 16 }}>{t.integ_lead}</p>
        <Banner tone="danger" title={t.integ_load_err} testid="catalog-error"
          action={<Btn variant="outline" size="sm" icon="clock" onClick={reload}>{t.integ_retry}</Btn>}>
          <span className="mono">{loadErr}</span>
        </Banner>
      </div>
    );
  }
  if (!view) {
    return (
      <div className="view-enter" data-testid="integrations-tab">
        <p className="lead" style={{ marginBottom: 16 }}>{t.integ_lead}</p>
        <p className="hint" data-testid="catalog-loading">{t.integ_loading}</p>
      </div>
    );
  }

  const age = fmtAge(view.age_seconds, t);
  const dashUrl = d.dashboard_url;
  const installedItems = items.filter(isInstalled);
  const needle = q.trim().toLowerCase();
  const catOf = (it) => INTEG_CAT[it.id] || "other";
  const counts = CATS.reduce((m, c) => { m[c] = items.filter(it => !isInstalled(it) && (c === "all" || catOf(it) === c)).length; return m; }, {});
  const available = items.filter(it => !isInstalled(it)
    && (cat === "all" || catOf(it) === cat)
    && (!needle || loc(it.title, lang, it.id).toLowerCase().includes(needle) || it.id.includes(needle) || loc(it.summary, lang, "").toLowerCase().includes(needle)));

  const rowStatus = (it) => {
    const st = status[it.id];
    const count = st ? st.doc_count : 0;
    const lastMs = st && st.last_doc_at ? Date.parse(st.last_doc_at) : NaN;
    const lastAge = Number.isFinite(lastMs) ? Math.max(0, Math.round((Date.now() - lastMs) / 1000)) : null;
    if (st && st.receiving) return { live: true, text: `${t.receiving} · ${fmtInt(count, lang)} ${t.docs}` };
    if (!st || count === 0) return { live: false, text: t.no_data };
    if (st.state === "untimed") return { live: false, text: `${fmtInt(count, lang)} ${t.docs} — ${t.integ_recency_unknown}` };
    return { live: false, text: `${t.integ_no_recent}${lastAge != null ? ` — ${t.integ_last_data.replace("{age}", fmtAge(lastAge, t))}` : ""} · ${fmtInt(count, lang)} ${t.docs}` };
  };

  return (
    <div className="view-enter" data-testid="integrations-tab">
      <LockNotice reason={locked} />
      <p className="lead" style={{ marginBottom: 18 }}>{t.integ_lead}</p>

      {/* 1. what is already sending data */}
      <div className="section-title">{t.integ_installed_h} <span className="tab-count">{installedItems.length}</span></div>
      {installedItems.length === 0 ? (
        <p className="hint" style={{ marginBottom: 18 }} data-testid="integ-none-installed">{t.integ_none_installed}</p>
      ) : (
        <div style={{ display: "grid", gap: 8, marginBottom: 18 }}>
          {installedItems.map(it => {
            const state = it.update_available ? "update" : "installed";
            const rs = rowStatus(it);
            const working = busy[it.id];
            const err = itemErr[it.id];
            return (
              <div className="card installed-row" key={it.id} data-testid={`integration-${it.id}`} data-state={state}>
                <span className="integ-mono on">{monogram(it.id, loc(it.title, lang, it.id))}</span>
                <div style={{ minWidth: 0 }}>
                  <div className="ttl">
                    <b>{loc(it.title, lang, it.id)}</b>
                    <span className="badge green mono" data-testid={`installed-${it.id}`}>
                      <Icon name="check" size={11} />{t.integ_installed} {it.installed_version || `(${t.integ_version_unknown})`}
                    </span>
                    {state === "update" && <span className="badge yellow" data-testid={`update-badge-${it.id}`}>{t.integ_update_badge} · {it.version}</span>}
                    {it.builtin && <span className="badge">{t.integ_builtin}</span>}
                  </div>
                  <div className="d">{loc(it.summary, lang, "")}</div>
                  <div className={rs.live ? "live" : "idle"} style={{ marginTop: 4 }}>{rs.live && <Icon name="spark" size={12} style={{ verticalAlign: -2, marginRight: 4 }} />}{rs.text}</div>
                  {err && <div className="field-err" data-testid={`error-${it.id}`}>{err}</div>}
                </div>
                <div className="acts">
                  {state === "update" && (
                    <Btn variant="primary" size="sm" icon="arrowUp" disabled={!!working || !!locked}
                      data-testid={`update-${it.id}`} onClick={() => act(it.id, "install", it.version)}>
                      {working === "install" ? t.integ_installing : t.integ_update}
                    </Btn>
                  )}
                  {dashUrl && <a href={`${dashUrl}/app/dashboards`} target="_blank" rel="noreferrer" className="btn btn-outline btn-sm">{t.open_dashboard}<Icon name="ext" size={12} /></a>}
                  <Menu label={t.more_actions} items={[
                    { label: t.integ_uninstall, icon: "trash", danger: true, disabled: !!working || !!locked, testid: `uninstall-${it.id}`, onClick: () => act(it.id, "uninstall") },
                  ]} />
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* 2. the second collection option (ADR-053) */}
      <ObservabilityStackPanel d={d} lang={lang} locked={locked} onToggle={onToggleStack} onToast={onToast} />

      {/* 3. the catalog */}
      <div className="section-title" style={{ justifyContent: "space-between" }}>
        <span>{t.integ_catalog_h}</span>
        {view.source === "registry"
          ? <span data-testid="catalog-source" data-source="registry" style={{ textTransform: "none", letterSpacing: 0, fontWeight: 400 }}>{t.integ_src_registry.replace("{age}", age)}</span>
          : null}
      </div>
      {view.source !== "registry" && (
        <Banner tone="warn" testid="catalog-degraded" title={view.source === "cache" ? t.integ_stale_h : t.integ_bootstrap_h}
          className="catalog-degraded">
          <span data-source={view.source}>{view.source === "cache" ? t.integ_stale_p.replace("{age}", age) : t.integ_bootstrap_p}</span>
          {view.error && <pre data-testid="catalog-error-reason">{view.error}</pre>}
        </Banner>
      )}
      {items.length === 0 ? (
        <p className="hint" data-testid="catalog-empty">{t.integ_empty}</p>
      ) : (
        <div className="integ-layout">
          <div>
            <label className="search" style={{ position: "relative", display: "flex", alignItems: "center", marginBottom: 10 }}>
              <Icon name="search" size={14} style={{ position: "absolute", left: 10, color: "var(--text-3)", pointerEvents: "none" }} />
              <input className="input" style={{ paddingLeft: 32 }} value={q} onChange={e => setQ(e.target.value)}
                placeholder={t.integ_search_ph} aria-label={t.integ_search_ph} data-testid="integ-search" />
            </label>
            <nav className="catnav" aria-label={t.integ_catalog_h}>
              {CATS.filter(c => c === "all" || counts[c] > 0).map(c => (
                <button type="button" key={c} aria-pressed={cat === c} onClick={() => setCat(c)}>
                  <span>{t[`integ_cat_${c}`]}</span><i>{counts[c]}</i>
                </button>
              ))}
            </nav>
          </div>
          <div>
            {available.length === 0 ? (
              <p className="hint" data-testid="integ-no-match">{counts.all === 0 ? t.integ_all_installed : t.integ_no_match}</p>
            ) : (
              <div className="integ-grid">
                {available.map(it => {
                  const working = busy[it.id];
                  const err = itemErr[it.id];
                  return (
                    <div className="card integ-card" key={it.id} data-testid={`integration-${it.id}`} data-state="available">
                      <div className="top">
                        <span className="integ-mono">{monogram(it.id, loc(it.title, lang, it.id))}</span>
                        <b title={loc(it.title, lang, it.id)}>{loc(it.title, lang, it.id)}</b>
                      </div>
                      <div className="d">{loc(it.summary, lang, "")}</div>
                      {!it.compatible && (
                        <p className="hint" style={{ color: "var(--warn-text)" }} data-testid={`incompatible-${it.id}`}>
                          {t.integ_incompatible.replace("{v}", it.min_core_version || "?")}
                        </p>
                      )}
                      {err && <div className="field-err" data-testid={`error-${it.id}`}>{err}</div>}
                      <div className="ft">
                        <Btn variant="outline" size="sm" icon="plus" disabled={!!working || !!locked || !it.compatible}
                          data-testid={`install-${it.id}`} onClick={() => act(it.id, "install", it.version)}>
                          {working === "install" ? t.integ_installing : t.integ_install}
                        </Btn>
                        <span className="badge mono" title={`${t.integ_version} ${it.version}`}>{it.version}</span>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/* ============================================================
   Access
   ============================================================ */
// The per-deployment OpenSearch admin credentials. The password is fetched
// ONLY on an explicit reveal click — never eagerly into the page.
function CredentialsPanel({ d, lang, onToast, onReset, locked }) {
  const t = STR[lang];
  const [creds, setCreds] = useState(null);
  const [show, setShow] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [fresh, setFresh] = useState(null);

  async function doReset() {
    setResetting(true);
    try {
      const c = await onReset(d.id);
      if (c && c.password) { setCreds(c); setShow(true); setFresh(c); }
    } catch { /* the handler already toasted the reason */ }
    finally { setResetting(false); setConfirming(false); }
  }

  async function reveal() {
    if (creds) { setShow(s => !s); return; }
    setBusy(true);
    setErr(false);
    try {
      const c = await API.dashboardCredentials(d.id);
      if (c && c.username) { setCreds(c); setShow(true); }
      else setErr(true);
    } catch {
      setErr(true);
    } finally {
      setBusy(false);
    }
  }

  async function copyFresh() {
    const ok = await copyText(fresh.password);
    onToast(ok ? t.copied : t.copy_failed, ok);
    if (ok) setFresh(null);
  }

  const label = busy ? t.cred_loading : creds ? (show ? t.hide : t.show) : t.cred_reveal;

  return (
    <div className="card pad" style={{ marginBottom: 16 }} data-testid="cluster-credentials">
      <h3 className="section-title"><Icon name="shield" size={14} /> {t.cred_h}</h3>
      <p className="hint" style={{ marginBottom: 12 }}>{t.cred_desc}</p>

      <div style={{ display: "grid", gap: 8 }}>
        <div className="ep-row">
          <span className="lbl">{t.cred_user}</span>
          <span className="val">
            {creds ? <Copyable text={creds.username} t={t} onCopy={onToast} /> : <span className="hint">{t.cred_user_hidden}</span>}
          </span>
        </div>
        <div className="ep-row">
          <span className="lbl">{t.cred_pass}</span>
          <span className="val">
            {creds && show
              ? <Copyable text={creds.password} t={t} onCopy={onToast} />
              : <span className="copyfield"><code>{"•".repeat(12)}</code></span>}
            <Btn variant="outline" size="sm" icon={show ? "eyeOff" : "eye"} disabled={busy} onClick={reveal}
              data-testid="reveal-credentials">{label}</Btn>
          </span>
        </div>
      </div>
      {err && <p className="field-err">{t.cred_error}</p>}

      <div className="hr" style={{ margin: "16px 0 12px" }} />
      <div style={{ fontSize: 13.5, fontWeight: 600, marginBottom: 4 }}>{t.sec_reset_h}</div>
      <p className="hint" style={{ marginBottom: 10 }}>{t.sec_reset_desc}</p>
      <Btn variant="danger" icon="shield" data-testid="reset-pass"
        disabled={resetting || !!locked} onClick={() => setConfirming(true)}>
        {resetting ? t.sec_reset_running : t.reset_pass}
      </Btn>

      <Confirm open={confirming} icon="shield" title={t.sec_reset_confirm_h} body={t.sec_reset_confirm_p}
        confirmLabel={t.reset_pass} cancelLabel={t.cancel}
        onConfirm={doReset} onCancel={() => setConfirming(false)} />

      <Confirm open={!!fresh} icon="shield" variant="primary" title={t.sec_new_pass_h} body={t.sec_new_pass_p}
        confirmLabel={t.copy} cancelLabel={t.close}
        onConfirm={copyFresh} onCancel={() => setFresh(null)}>
        <div style={{ margin: "10px 0" }}>
          <Copyable text={fresh ? fresh.password : ""} t={t} onCopy={onToast} />
        </div>
      </Confirm>
    </div>
  );
}

// Optional network restriction on this deployment's PUBLIC routes.
function IpAllowListPanel({ d, lang, onToast, locked }) {
  const t = STR[lang];
  const [text, setText] = useState((d.ip_allow_list || []).join("\n"));
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const saved = (d.ip_allow_list || []).join("\n");
  const dirty = text.trim() !== saved.trim();

  async function save() {
    setBusy(true); setErr("");
    try {
      const cidrs = text.split(/[\s,]+/).map(x => x.trim()).filter(Boolean);
      await API.setIpAllowList(d.id, cidrs);
      onToast(cidrs.length ? t.ipallow_saved : t.ipallow_cleared);
    } catch (e) {
      setErr(e.message || String(e));
    } finally {
      setBusy(false);
    }
  }

  const open = (d.ip_allow_list || []).length === 0;
  return (
    <div className="card pad" style={{ marginBottom: 16 }}>
      <h3 className="section-title"><Icon name="globe" size={14} /> {t.ipallow_h}</h3>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap", marginBottom: 10 }}>
        <span className={`badge ${open ? "" : "green"}`}>{open ? t.ipallow_state_open : fmt(t.ipallow_state_limited, (d.ip_allow_list || []).length)}</span>
        <span className="hint">{t.ipallow_desc}</span>
      </div>
      <Field label={t.ipallow_field} hint={t.ipallow_hint} htmlFor="ip-allow">
        <textarea id="ip-allow" className="textarea mono" rows={4} value={text} spellCheck={false}
          placeholder={"203.0.113.0/24\n198.51.100.7"}
          onChange={e => setText(e.target.value)} />
      </Field>
      <Btn variant="primary" icon="check" disabled={busy || !dirty || !!locked} onClick={save}>
        {busy ? t.saving : t.save}
      </Btn>
      {err && <div className="field-err">{err}</div>}
    </div>
  );
}

// Who can get in and from where — one tab. Credentials and the network rule
// are short; the SSO form is long and sits last with its own heading.
function AccessTab({ d, lang, onReset, onToast, locked }) {
  const t = STR[lang];
  return (
    <div className="view-enter" style={{ maxWidth: 820 }}>
      <CredentialsPanel d={d} lang={lang} onToast={onToast} onReset={onReset} locked={locked} />
      <IpAllowListPanel d={d} lang={lang} onToast={onToast} locked={locked} />
      <div className="card pad" data-testid="sso-panel">
        <h3 className="section-title"><Icon name="user" size={14} /> {t.access_h_sso}</h3>
        <AuthProviderTab d={d} lang={lang} onToast={onToast} locked={locked} />
      </div>
    </div>
  );
}

/* ============================================================
   Activity
   ============================================================ */
function ActivityTab({ d, lang, onToast }) {
  const t = STR[lang];
  const a = d.activity || { kind: "idle" };
  return (
    <div className="view-enter">
      <p className="lead" style={{ marginBottom: 16 }}>{t.act_tab_lead}</p>
      {a.kind === "idle" && <Banner tone="ok" icon="check" testid="activity-idle">{t.ov_no_activity}</Banner>}
      <ActivityPanel d={d} lang={lang} />
      <UpgradeFailure u={d.upgrade} lang={lang} />
      <div className="card pad">
        <h3 className="section-title">{t.act_log_h}</h3>
        <p className="hint" style={{ marginBottom: 6 }}>{t.act_log_p}</p>
        <ActivityDetails id={d.id} lang={lang} defaultOpen />
      </div>
    </div>
  );
}

/* ============================================================
   The view
   ============================================================ */
function DeploymentView({ d, lang, hostNodes = [], tab, onTab, onToggleStack, onSaveEdit, onResetPass, onToast, onDelete, openUpgrade }) {
  const t = STR[lang];
  const [confirm, setConfirm] = useState(false);
  // Computed once from the server's verdict (ADR-050 invariant 3).
  const locked = lockReason(d, t);
  const act = d.activity || { kind: "idle" };
  const busy = act.kind !== "idle";
  const tabs = [
    { key: "overview", label: t.tab_overview },
    { key: "data", label: t.tab_data, count: (d.monitors || []).length + (d.otel_stack ? 1 : 0) || null },
    { key: "backup", label: t.tab_snapshot },
    { key: "access", label: t.tab_access },
    { key: "config", label: t.tab_config },
    { key: "activity", label: t.tab_activity },
  ];
  const snap = d.snapshot || { configured: false };
  const sizeMetaStr = fmt(t.dep_size_meta, sizeLabel(d.size, t), d.node_count || 3, fmtQty(d.mem, lang));

  return (
    <div className="view-enter">
      <PageHeader title={d.id} mono
        pill={<StatusPill health={d.health} t={t} testid="dep-status"
          busy={busy ? `${t[`act_badge_${act.kind}`] || t.upg_state_upgrading} · ${act.percent}%` : null} />}
        meta={<>
          <span>{t[`p_${d.purpose === "observability" ? "obs" : d.purpose === "security" ? "sec" : "search"}_t`]}</span>
          <span className="sep">·</span>
          {d.version && <><span>OpenSearch <span className="mono">{d.version}</span></span><span className="sep">·</span></>}
          <span>{sizeMetaStr}</span>
          <span className="sep">·</span>
          <span className={`badge ${snap.configured && snap.policy_state !== "ERROR" ? "green" : ""}`} data-testid="backup-chip">
            <Icon name="db" size={11} />
            {!snap.configured ? t.snap_chip_off
              : snap.policy_state === "ERROR" ? t.snap_chip_err
                : snap.policy_state === "PENDING" ? t.snap_chip_pending
                  : `${t.snap_chip_on}${snap.schedule ? ` ${snap.schedule}` : ""}`}
          </span>
          {d.created_at && <><span className="sep">·</span><span>{fmt(t.dep_created_ago, relAge(d.created_at, t))}</span></>}
        </>}
        actions={<>
          <UpgradeControl d={d} lang={lang} onToast={onToast} openUpgrade={openUpgrade} />
          {d.dashboard_url && (
            <a className="btn btn-primary" href={d.dashboard_url} target="_blank" rel="noreferrer" data-testid="open-dashboards">
              {t.dep_open_dash}<Icon name="ext" size={14} />
            </a>
          )}
          <Menu label={t.more_actions} testid="dep-menu" items={[
            { label: t.tab_config, icon: "sliders", onClick: () => onTab("config") },
            { label: t.tab_snapshot, icon: "db", onClick: () => onTab("backup") },
            { sep: true },
            { label: t.delete_dep, icon: "trash", danger: true, testid: "menu-delete", onClick: () => setConfirm(true) },
          ]} />
        </>} />

      <SingleCopyBanner d={d} lang={lang} hostNodes={hostNodes} onTab={onTab} />
      {/* #47: above the tabs, on every tab. */}
      <ProvisioningBanner d={d} lang={lang} onToast={onToast} />

      <Tabs items={tabs} value={tab} onChange={onTab} label={t.dep_tabs_label} testidPrefix="dep-tab" />

      {tab === "overview" && <OverviewTab d={d} lang={lang} onToast={onToast} hostNodes={hostNodes} onTab={onTab} />}
      {tab === "data" && <DataTab d={d} lang={lang} onToggleStack={onToggleStack} onToast={onToast} locked={locked} />}
      {tab === "backup" && <SnapshotTab d={d} lang={lang} onToast={onToast} locked={locked} />}
      {tab === "access" && <AccessTab d={d} lang={lang} onReset={onResetPass} onToast={onToast} locked={locked} />}
      {tab === "config" && <ConfigTab d={d} lang={lang} onSave={onSaveEdit} onToast={onToast} locked={locked} onDeleteRequest={() => setConfirm(true)} />}
      {tab === "activity" && <ActivityTab d={d} lang={lang} onToast={onToast} />}

      {/* Type-to-confirm: naming the target is what separates "yes, this one"
          from "yes, whatever was open". The dialog also says what the person
          would be giving up: the backup state decides how bad this is. */}
      <Confirm open={confirm} title={t.delete_dep} body={t.delete_confirm}
        confirmLabel={t.delete} cancelLabel={t.cancel}
        requireText={d.id} requireLabel={fmt(t.delete_type_name, d.id)}
        confirmTestid="delete-confirm" cancelTestid="delete-cancel"
        onCancel={() => setConfirm(false)}
        onConfirm={() => { setConfirm(false); onDelete(d.id); }}>
        <p className="hint" style={{ margin: "0 0 10px", color: snap.configured ? undefined : "var(--warn-text)" }}>
          {snap.configured ? (snap.schedule ? fmt(t.single_copy_bak_sched, snap.schedule) : t.snap_state_ok) : t.del_no_backup}
        </p>
      </Confirm>
    </div>
  );
}

export { DeploymentView, healthTone };
