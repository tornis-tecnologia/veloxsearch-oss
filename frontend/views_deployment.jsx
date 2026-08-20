// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Deployment detail — Overview / Edit / Integrations / Backup / Security / Auth
   Wired to the API (issue #27):
     Overview      → node_stats (poll) + dashboard_url
     Edit          → save_cluster
     Integrations  → catalog (runtime registry, #76) + monitoring_status
                     + catalog_install / catalog_uninstall
     Security      → dashboard_credentials (on-demand reveal) + reset_admin_password
   ============================================================ */
import { useState, useEffect, useRef, useCallback } from "react";
import { STR, SIZES, sizeMeta } from "./i18n.jsx";
import { API, adaptMetrics, adaptSeries } from "./api.jsx";
import { Icon, Copyable, Btn, Confirm, Field, MiniMeter, StatTile } from "./ui.jsx";
import { AuthProviderTab } from "./views_auth_provider.jsx";
import { SnapshotTab } from "./views_snapshot.jsx";
import { ActivityPanel, lockReason, LockNotice } from "./views_activity.jsx";

// Short unit suffix. The tiles put two of these side by side ("12.4 G / 50 G")
// inside a fixed-width card, and the full "GiB" spelling was the widest thing
// in the row — it wrapped before the number it was qualifying did.
function gib(n) { return n.toFixed(1) + " G"; }

// Per-node percentages, derived in one place so the row meters, the "hottest
// node" summary and the emphasis threshold can never disagree.
function heapPct(n) { return n.heapTotal ? (n.heapUsed / n.heapTotal) * 100 : 0; }
function nodeDiskPct(n) { return n.diskTotal ? (n.diskUsed / n.diskTotal) * 100 : 0; }

// Documents gained per minute across the sampled window. Answers "is it growing
// and how fast", which a total on its own cannot. `ts` is epoch milliseconds.
function perMinute(points, lang) {
  const first = points[0];
  const last = points[points.length - 1];
  const mins = (last.ts - first.ts) / 60000;
  if (!(mins > 0)) return "—";
  const delta = (last.docs - first.docs) / mins;
  if (delta <= 0) return lang === "pt" ? "estável" : "steady";
  const v = delta >= 1000 ? `${(delta / 1000).toFixed(1)}k` : delta.toFixed(0);
  return `↗ +${v}${lang === "pt" ? "/min" : "/min"}`;
}

// Parse a K8s storage quantity ("5Gi" / "512Mi" / "2G" / plain bytes) to bytes,
// for the resize guard's client-side shrink check. Returns null when blank or
// unparseable so the caller can skip the comparison (backend still enforces it).
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

// Minimal dependency-free sparkline: a normalized SVG polyline over a fixed
// 100×28 viewBox (stretched to fill its card). `max` pins the vertical scale
// for bounded metrics (cpu/heap → 100%); unbounded metrics auto-scale to their
// own peak. Reuses the accent color so it sits with the existing meters.
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
      style={{ width: "100%", height: 28, display: "block", marginTop: 6 }}>
      <polyline points={pts} fill="none" stroke={color || "var(--accent)"}
        strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round"
        vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

// The "second moment" view (#9): per-metric sparklines over the recent window.
// The series is fetched ONCE by the Overview and handed down, because the
// summary tiles read the same points — two components polling the same endpoint
// would double the load and could disagree with each other on screen.
function MetricsTimeSeries({ points, lang }) {
  const t = STR[lang];

  if (points.length < 2) {
    return (
      <div data-testid="metrics-timeseries" style={{ marginTop: 16 }}>
        <h3 className="section-title">{t.mt_h}</h3>
        <p className="hint">{t.mt_empty}</p>
      </div>
    );
  }

  const last = points[points.length - 1];
  const cards = [
    { label: t.cpu, val: `${last.cpu.toFixed(0)}%`, values: points.map(p => p.cpu), max: 100 },
    { label: t.heap, val: `${last.heap.toFixed(0)}%`, values: points.map(p => p.heap), max: 100 },
    { label: t.mt_disk, val: gib(last.diskUsed), values: points.map(p => p.diskUsed) },
    { label: t.mt_indexing, val: `${last.rate.toFixed(1)}${t.mt_per_sec}`, values: points.map(p => p.rate) },
  ];

  return (
    <div data-testid="metrics-timeseries" style={{ marginTop: 16 }}>
      <h3 className="section-title">{t.mt_h}</h3>
      <div className="node-grid">
        {cards.map(c => (
          <div className="node-card" key={c.label}>
            <div className="metric-head">
              <span className="lbl">{c.label}</span>
              <span className="val tnum">{c.val}</span>
            </div>
            <Spark values={c.values} max={c.max} />
          </div>
        ))}
      </div>
    </div>
  );
}

// ── version upgrade (ADR-048) ───────────────────────────────────────────────
// The running version is always visible; when a newer tested target exists the
// chip grows an "atualização disponível" affordance and the upgrade button. The
// button is never a bare icon and never hidden in a menu.
//
// Everything shown here is read from the CR (via the SSE-driven `d` and
// /api/upgrade_options): the rules (no downgrade, ≤1 major, green cluster) are
// the backend's, and a reload or a backend restart loses no progress.
function fmt(s, ...args) {
  return args.reduce((acc, v, i) => acc.replaceAll(`{${i}}`, v), s || "");
}

// A REFUSED upgrade only. The pending/upgrading progress moved to
// `ActivityPanel` (ADR-050), which covers creation, upgrade and restart with
// one surface — but a refusal is not an activity: the operator rejected the
// version, nothing is in flight, and the deployment is settled. So it would
// render nothing at all, and the operator's message is the only place that
// refusal exists (ADR-048 invariant 4). Hence this narrow survivor.
function UpgradeFailure({ u, lang }) {
  const t = STR[lang];
  if (!u || u.state !== "failed") return null;
  return (
    <div className="card pad" style={{ marginBottom: 16, borderColor: "var(--danger-border)" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap", fontSize: 13.5 }}>
        <Icon name="shield" size={15} style={{ color: "var(--danger)" }} />
        <b style={{ fontFamily: "var(--font-mono)" }}>{t.upg_state_failed}</b>
      </div>
      {/* The operator's own refusal, verbatim — it is the only place it exists. */}
      {u.reason && (
        <p style={{ margin: "8px 0 0", color: "var(--danger)", fontSize: 12.5, fontFamily: "var(--font-mono)" }}>{u.reason}</p>
      )}
    </div>
  );
}

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

  // Reload the options whenever the deployment's version or upgrade state
  // moves — the SSE stream drives `d`, so this follows the real cluster.
  useEffect(() => {
    let alive = true;
    API.upgradeOptions(d.id)
      .then(o => {
        if (!alive) return;
        setOpts(o);
        // The suggested version leads the list, so it is also the default
        // selection — the tag the user clicked and the preselected target are
        // the same thing.
        setTarget(prev => prev || o.suggested_version || (o.targets && o.targets[0] ? o.targets[0].version : ""));
      })
      .catch(() => { if (alive) setOpts(null); });
    return () => { alive = false; };
  }, [d.id, d.version, state, d.dashboards_version, d.suggested_version]);

  const version = d.version || (opts && opts.version) || "";
  const targets = (opts && opts.targets) || [];
  const dashBehind = !!(opts && opts.dashboards_behind);
  // The hourly upstream check's find for THIS deployment (ADR-048 rev. 2).
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
      // The pre-flight's refusal IS the message, and it renders under the
      // control rather than in a toast (nothing was written when it fails).
      setErr(e.message || String(e));
    } finally {
      setBusy(false);
    }
  }

  // The running version itself lives in the Overview's meta line (it is a fact
  // about the deployment, not about upgrading); this control is only the
  // "there is something newer" affordance.
  if (!version) return null;
  return (
    <>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap", marginBottom: canOffer && state === "idle" ? 16 : 0 }}>
        {canOffer && state === "idle" && (
          <>
            <span className="hint" style={{ margin: 0 }}>{t.upg_available}</span>
            {/* The hourly check's find is named on the button itself — the same
                "Upgrade v3.8.0" tag the deployment list shows. */}
            <Btn variant="primary" icon="arrowR"
              onClick={() => { setErr(""); if (suggested) setTarget(suggested); setOpen(true); }}>
              {dashBehind && !targets.length
                ? t.upg_dash_retry
                : suggested ? `Upgrade v${suggested}` : t.upg_btn}
            </Btn>
          </>
        )}
      </div>

      {open && (
        <div
          style={{
            position: "fixed", inset: 0, zIndex: 300,
            background: "rgb(0 0 0 / 0.55)", backdropFilter: "blur(3px)",
            display: "grid", placeItems: "center", padding: 20,
          }}
          onClick={() => !busy && setOpen(false)}
          onKeyDown={e => { if (e.key === "Escape" && !busy) setOpen(false); }}
        >
          <div className="card pad" role="dialog" aria-modal="true" aria-label={t.upg_h}
            style={{ maxWidth: 520, width: "100%", maxHeight: "85vh", overflowY: "auto" }}
            onClick={e => e.stopPropagation()}>
            <h3 style={{ margin: "0 0 14px", fontFamily: "var(--font-mono)", fontSize: 16 }}>{t.upg_h}</h3>

            {/* A blocked state explains itself here, not in a toast. */}
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
                    <select id="upg-target" className="input" value={target} disabled={advanced}
                      onChange={e => setTarget(e.target.value)}>
                      {targets.map(o => (
                        <option key={o.version} value={o.version}>
                          {o.version} · {t[`upg_note_${o.note}`] || o.note}
                        </option>
                      ))}
                    </select>
                  </Field>
                )}

                {/* Blast radius before the action, not after. */}
                <h4 className="section-title" style={{ marginTop: 18 }}>{t.upg_blast_h}</h4>
                <p className="hint">{t.upg_blast_p}</p>
                <h4 className="section-title" style={{ marginTop: 14, color: "var(--danger)" }}>{t.upg_noback_h}</h4>
                <p className="hint">{t.upg_noback_p}</p>
                {/* Not fine print. Since ADR-049 this is conditional: a
                    deployment with a scheduled policy has something to restore
                    from and is told where it is; one without still gets the
                    original warning. */}
                <p className="hint">
                  {d.snapshot && d.snapshot.configured
                    ? fmt(t.upg_snapshot_ok, d.snapshot.schedule || "")
                    : t.upg_snapshot_p}
                </p>

                <details style={{ marginTop: 12 }} open={advanced}>
                  <summary style={{ cursor: "pointer", fontSize: 13 }}
                    onClick={e => { e.preventDefault(); setAdvanced(a => !a); }}>{t.upg_advanced}</summary>
                  <div style={{ marginTop: 10 }}>
                    <Field label={t.upg_custom} hint={t.upg_custom_hint} htmlFor="upg-custom">
                      <input id="upg-custom" className="input" value={custom} placeholder="3.8.0"
                        onChange={e => setCustom(e.target.value)} />
                    </Field>
                    <label style={{ display: "flex", gap: 8, alignItems: "flex-start", fontSize: 12.5 }}>
                      <input type="checkbox" checked={ack} onChange={e => setAck(e.target.checked)} />
                      <span>{t.upg_unverified}</span>
                    </label>
                  </div>
                </details>
              </>
            )}

            {err && (
              <p style={{ color: "var(--danger)", fontSize: 12.5, marginTop: 12, fontFamily: "var(--font-mono)" }}>{err}</p>
            )}

            <div style={{ display: "flex", gap: 10, justifyContent: "flex-end", marginTop: 16 }}>
              <Btn variant="outline" onClick={() => setOpen(false)} disabled={busy}>{t.cancel}</Btn>
              <Btn variant="primary" icon="arrowR" disabled={busy || !!blocked || !chosen} onClick={submit}>
                {t.upg_confirm}
              </Btn>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

// A node is "hot" when any meter crosses the same threshold the bars already
// use for their warning tone — the row emphasis and the bar color agree.
const HOT_PCT = 75;

function OverviewTab({ d, lang, onToast, openUpgrade }) {
  const t = STR[lang];
  const [nodes, setNodes] = useState([]);
  const [totals, setTotals] = useState({ docs: 0 });
  const [points, setPoints] = useState([]);
  // When the numbers on screen were last confirmed. Both polls used to swallow
  // their errors, so a dead backend left stale numbers looking live.
  const [lastOk, setLastOk] = useState(0);
  const [tick, setTick] = useState(0);
  const url = d.dashboard_url;

  // Cluster admin credentials live on the Security tab now, fetched only on an
  // explicit reveal (see CredentialsPanel) — the Overview no longer pulls the
  // password eagerly into the page.

  // Live per-node stats. While the cluster is still provisioning OpenSearch
  // isn't answering — treat any error as "no stats yet" and stay quiet, but
  // remember WHEN the last good answer came in.
  useEffect(() => {
    let alive = true;
    const load = () => API.nodeStats(d.id)
      .then(m => {
        if (!alive) return;
        const a = adaptMetrics(m);
        setNodes(a.nodes);
        setTotals({ docs: a.total_docs });
        setLastOk(Date.now());
      })
      .catch(() => {});
    load();
    const h = setInterval(load, 5000);
    return () => { alive = false; clearInterval(h); };
  }, [d.id]);

  // The recent-window series: feeds both the ingestion tile and the trend
  // sparklines, fetched once (see MetricsTimeSeries).
  useEffect(() => {
    let alive = true;
    const load = () => API.metricsSeries(d.id)
      .then(s => { if (alive) setPoints(adaptSeries(s).points); })
      .catch(() => {});
    load();
    const h = setInterval(load, 10000);
    return () => { alive = false; clearInterval(h); };
  }, [d.id]);

  // Re-render once a second so "atualizado há Xs" actually counts.
  useEffect(() => {
    const h = setInterval(() => setTick(x => x + 1), 1000);
    return () => clearInterval(h);
  }, []);

  const last = points.length ? points[points.length - 1] : null;
  const rate = last ? last.rate : 0;
  const receiving = rate > 0;
  // Cluster-wide storage: the deployment's PVCs, not the host disks (ADR-031).
  const diskUsed = nodes.reduce((s, n) => s + (n.pvcBound ? n.diskUsed : 0), 0);
  const diskTotal = nodes.reduce((s, n) => s + (n.pvcBound ? n.diskTotal : 0), 0);
  const diskPct = diskTotal ? (diskUsed / diskTotal) * 100 : 0;
  // The NODES tile and the activity panel now divide the SAME pair (the server
  // agrees `nodes_ready`/`nodes_desired` with `activity`, ADR-050 / issue
  // #131), so the tile can no longer print a fraction the panel disagrees with.
  // What it must ALSO stop doing is presenting "all ready" as the whole answer:
  // for sixteen hours this tile read "3/3 — all ready" beside a roll that had
  // not moved since the previous day. Ready is a fact about pods; converged is
  // a fact about the deployment, and only the activity verdict knows it.
  const act = d.activity || { kind: "idle" };
  const nodesOk = d.nodes_ready === d.node_count && d.node_count > 0;
  const nodesSub = !nodesOk
    ? (d.phase || t.ov_nodes_degraded)
    : act.kind === "idle"
      ? t.ov_nodes_ok
      : `${t.ov_nodes_ok} · ${t[`act_badge_${act.kind}`] || ""}`;
  const hottest = nodes.reduce((m, n) => Math.max(m, n.cpu, heapPct(n), nodeDiskPct(n)), 0);
  const staleSecs = lastOk ? Math.floor((Date.now() - lastOk) / 1000) : null;
  const stale = staleSecs !== null && staleSecs > 30;
  const snap = d.snapshot || { configured: false };

  return (
    <div className="view-enter">
      {/* Context line: what this deployment IS. Deliberately quiet — the loud
          things are the numbers below it. */}
      <div className="meta" style={{ marginBottom: 14 }}>
        <span className={`badge ${d.health === "green" ? "green" : ""}`}><span className="dot" />{d.health}</span>
        {/* The version actually RUNNING (`status.version` from the operator,
            not the spec) — ADR-048. */}
        {d.version && (
          <span className="badge" style={{ textTransform: "none" }}>
            <Icon name="layers" size={12} />OpenSearch {d.version}
          </span>
        )}
        {/* Backup state (ADR-049). Absent is a valid state, so an unconfigured
            deployment says so plainly instead of showing nothing — this is the
            one place someone notices they have no backup. */}
        <span className={`badge${snap.configured && snap.policy_state !== "ERROR" ? " green" : ""}`}
          style={{ textTransform: "none" }} data-testid="backup-chip">
          <Icon name="db" size={12} />
          {!snap.configured ? t.snap_chip_off
            : snap.policy_state === "ERROR" ? t.snap_chip_err
              : snap.policy_state === "PENDING" ? t.snap_chip_pending
                : `${t.snap_chip_on}${snap.schedule ? ` ${snap.schedule}` : ""}`}
        </span>
        <span>{sizeMeta(d.size).label.toLowerCase()}</span>
        <span className="sep">·</span>
        <span>{d.mem} {t.per_node}</span>
        <span className="sep">·</span>
        <span>{d.disk} {t.sz_disk}</span>
      </div>

      {/* The four questions an operator actually opens this screen to answer. */}
      <div className="stat-row">
        <StatTile label={t.documents} value={totals.docs.toLocaleString()}
          sub={last && points.length > 1 ? perMinute(points, lang) : "—"} />
        <StatTile label={t.ov_ingestion}
          value={last ? `${rate.toFixed(1)}${t.mt_per_sec}` : "—"}
          sub={last ? (receiving ? t.ov_receiving : t.ov_no_data) : t.ov_awaiting}
          tone={last ? (receiving ? "ok" : "warn") : ""} />
        <StatTile label={t.ov_storage}
          value={diskTotal ? `${gib(diskUsed)} / ${gib(diskTotal)}` : "—"}
          sub={diskTotal ? `${diskPct.toFixed(0)}%` : t.ov_awaiting}
          tone={diskPct >= 85 ? "warn" : ""} />
        <StatTile label={t.nodes} value={`${d.nodes_ready}/${d.node_count}`}
          sub={nodesSub}
          tone={nodesOk && act.kind === "idle" ? "ok" : "warn"} />
      </div>

      {/* Where this deployment lives. Two addresses, both named: the UI and the
          API are different audiences and used to be one unlabelled button. */}
      {(url || d.opensearch_url) && (
        <div className="card pad" style={{ marginBottom: 16 }}>
          <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 8, display: "flex", alignItems: "center", gap: 6 }}>
            <Icon name="plug" size={14} />{t.ov_endpoints_h}
          </div>
          <div style={{ display: "grid", gap: 6 }}>
            <EndpointRow t={t} label={t.otel_ep_dashboards} value={url}
              href={url} onCopy={() => onToast(t.copied)} />
            <EndpointRow t={t} label={t.otel_ep_opensearch} value={d.opensearch_url}
              href={d.opensearch_url} onCopy={() => onToast(t.copied)} />
          </div>
        </div>
      )}

      {/* Actions, always in the same place, whatever the cluster's state. */}
      <div className="action-bar">
        {!url && d.dashboard_portforward && (
          <>
            <span style={{ color: "var(--text-2)", fontSize: 13, fontFamily: "var(--font-mono)" }}>{t.portfwd_access}</span>
            <Copyable text={d.dashboard_portforward} onCopy={() => onToast(t.copied)} />
          </>
        )}
        <UpgradeControl d={d} lang={lang} onToast={onToast} openUpgrade={openUpgrade} />
        <span className="spacer" />
        {/* Freshness, and honesty when it is not fresh. */}
        <span className="hint" style={{ margin: 0, color: stale ? "var(--warn)" : undefined }}>
          {staleSecs === null ? t.ov_connecting : fmt(t.ov_updated, `${staleSecs}s`)}
        </span>
      </div>

      {/* One panel for creation, upgrade and node-restarting operations —
          they are the same question asked at different times (ADR-050). It
          hides on the server's `settled`, not on `health === "green"`, which
          goes green mid-roll between two node restarts. It does not REPLACE
          the page: a cluster with 2 of 3 nodes up still has nodes worth
          looking at, and its dashboard link still works. */}
      <UpgradeFailure u={d.upgrade} lang={lang} />
      <ActivityPanel d={d} lang={lang} />

      <>
        <details open>
          <summary className="section-title" style={{ marginTop: 26, cursor: "pointer", listStyle: "revert" }}>
            {t.nodes_h}
            {nodes.length > 0 && (
              <span className="hint" style={{ margin: 0, marginLeft: 8, fontWeight: 400 }}>
                {nodes.length} · {fmt(t.ov_peak, hottest.toFixed(0))}
              </span>
            )}
          </summary>
          {nodes.length === 0 ? (
            <p className="hint">{t.ov_gathering}</p>
          ) : (
            <div className="node-rows" style={{ marginTop: 10 }}>
              {nodes.map(n => {
                const hp = heapPct(n);
                const dp = nodeDiskPct(n);
                const hot = Math.max(n.cpu, hp, dp) >= HOT_PCT;
                return (
                  <div className={`node-row ${hot ? "hot" : ""}`} key={n.name}>
                    <div className="nname">
                      <span className={`statusdot ${d.health === "green" ? "green" : ""}`} />
                      <span title={n.name}>{n.name}</span>
                    </div>
                    <MiniMeter label={t.cpu} value={`${n.cpu}%`} pct={n.cpu} />
                    <MiniMeter label={t.heap} value={`${hp.toFixed(0)}%`} pct={hp} />
                    {n.pvcBound
                      ? <MiniMeter label={t.sz_disk} value={`${dp.toFixed(0)}%`} pct={dp} />
                      : <MiniMeter label={t.sz_disk} value={n.pvcPhase || "Pending"} pct={0} />}
                  </div>
                );
              })}
            </div>
          )}
        </details>

        {/* Same metrics as the rows above, over time — placed last because a
            trend is the follow-up question, never the first one. */}
        <MetricsTimeSeries points={points} lang={lang} />
      </>
    </div>
  );
}

function EditTab({ d, lang, onSave, locked }) {
  const t = STR[lang];
  const preset = sizeMeta(d.size);
  const [size, setSize] = useState(SIZES[d.size] ? d.size : "small");
  const [mem, setMem] = useState(d.mem || preset.mem);
  const [disk, setDisk] = useState(d.disk || preset.disk);
  const [nodeCount, setNodeCount] = useState(String(d.node_count || preset.nodes));
  // Seeded with what the deployment ACTUALLY runs. This field used to start
  // empty: saving then sent an empty additionalConfig, and because the CR is
  // applied server-side by one field manager, omitting the key PRUNED it — any
  // custom opensearch.yml was silently erased by an unrelated edit. Round-trip
  // it instead, so clearing the box stays an explicit act.
  const [extra, setExtra] = useState(d.extra_config || "");
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const sz = sizeMeta(size);
  // The JVM heap is operator-managed (#55/ADR-035): the OpenSearch operator
  // derives it as half the node memory. Resolve the displayed value from the
  // backend (like the wizard's Advanced step) so the UI never computes or
  // hardcodes the rule; seed with the deployment's current heap.
  const [jvmHeap, setJvmHeap] = useState(d.heap || sz.heap);
  useEffect(() => {
    let live = true;
    API.customSizing(mem, "")
      .then((p) => { if (live && p && p.heap) setJvmHeap(p.heap); })
      .catch(() => { /* offline: keep the last resolved value, no console noise */ });
    return () => { live = false; };
  }, [mem]);

  // Picking a preset refreshes the derived fields; the user can still override.
  function pickSize(k) {
    setSize(k);
    const m = sizeMeta(k);
    setMem(m.mem);
    setDisk(m.disk);
    setNodeCount(String(m.nodes));
  }

  // Resize guard (#16): disk can only grow — Kubernetes/CSI can't shrink a PVC.
  // Compared against the deployment's current disk; the backend enforces the
  // same rule (plus the StorageClass-expansion check) as the source of truth.
  const curBytes = parseDisk(d.disk);
  const newBytes = parseDisk(disk);
  const diskShrink = curBytes != null && newBytes != null && newBytes < curBytes;

  // Nothing to save, nothing to discard — and no confirmation to ask for.
  const dirty = size !== (SIZES[d.size] ? d.size : "small")
    || mem !== (d.mem || preset.mem)
    || disk !== (d.disk || preset.disk)
    || extra !== (d.extra_config || "");

  function reset() {
    setSize(SIZES[d.size] ? d.size : "small");
    setMem(d.mem || preset.mem);
    setDisk(d.disk || preset.disk);
    setNodeCount(String(d.node_count || preset.nodes));
    setExtra(d.extra_config || "");
  }

  async function save() {
    if (diskShrink) return; // belt-and-suspenders; the button is disabled too
    setBusy(true);
    try {
      await onSave({
        name: d.id,
        size,
        purpose: d.purpose,
        nodes: nodeCount,
        memory: mem,
        disk,
        config: extra,
        monitors: (d.monitors || []).join(","),
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="view-enter" style={{ maxWidth: 520 }}>
      <LockNotice reason={locked} />
      <p className="lead">{t.edit_lead}</p>
      <Field label={t.size}>
        <select className="select" value={size} onChange={e => pickSize(e.target.value)}>
          {Object.entries(SIZES).map(([k, v]) => <option key={k} value={k}>{v.label}</option>)}
        </select>
      </Field>
      {/* Always 3 nodes (ADR-016) — the wizard has said so from the start; this
          form used to accept any number, so the two screens disagreed. */}
      <Field label={t.node_count} hint={t.node_count_fixed}>
        <input className="input" value={nodeCount} disabled />
      </Field>
      <Field label={t.node_mem} tip={t.node_mem_hint}><input className="input" value={mem} onChange={e => setMem(e.target.value)} /></Field>
      <Field label={t.jvm} tip={t.jvm_hint}><input className="input" value={jvmHeap} disabled /></Field>
      <Field label={t.disk} hint={t.disk_hint}>
        <input className={`input${diskShrink ? " invalid" : ""}`} value={disk} onChange={e => setDisk(e.target.value)} />
        {diskShrink && <div className="field-err">{t.disk_shrink_err.replace(/\{cur\}/g, d.disk)}</div>}
      </Field>
      <Field label={t.extra} hint={t.extra_hint}><textarea className="textarea" value={extra} onChange={e => setExtra(e.target.value)} /></Field>
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <Btn variant="primary" icon="check" disabled={busy || diskShrink || !dirty || !!locked}
          data-testid="edit-save" onClick={() => setConfirm(true)}>{busy ? t.saving : t.save}</Btn>
        {dirty && <Btn variant="outline" disabled={busy} onClick={reset}>{t.discard}</Btn>}
      </div>

      {/* A save restarts the nodes. The upgrade flow states its blast radius
          before acting; an edit that reboots the same nodes must too. */}
      <Confirm open={confirm} title={t.edit_confirm_h} body={t.edit_confirm_p}
        confirmLabel={t.save} cancelLabel={t.cancel} icon="check" variant="primary"
        onCancel={() => setConfirm(false)}
        onConfirm={() => { setConfirm(false); save(); }} />
    </div>
  );
}

// One doc-count refresh per this many ms. Each installed integration costs one query
// against the customer's OpenSearch, so the old 5s cadence meant 12 enabled
// integrations = 144 requests/minute for numbers nobody watches that closely.
const INTEG_POLL_MS = 15000;

// Integration id → an icon that already exists in ui.jsx. The catalog is
// RUNTIME data (#76): anything unlisted falls back to the generic glyph, so a
// newly published registry package renders with no frontend change at all.
const INTEG_ICON = {
  postgres: "db", mysql: "db", mongo: "db", redis: "db",
  nginx: "layers", traefik: "layers", kubernetes: "layers",
  "k8s-events": "activity", kafka: "activity", rabbitmq: "activity",
  "k8s-audit": "shield", ssh: "shield",
};

// One labelled block of addresses. The stack publishes eight of them across
// three purposes (send / look / alerts), and an undifferentiated list of
// <Copyable>s is how the first version of this panel managed to show a user
// five green components and still leave them asking where to point an exporter.
function EndpointGroup({ title, desc, icon, children }) {
  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, fontWeight: 600, color: "var(--text-1)" }}>
        <Icon name={icon} size={14} />{title}
      </div>
      {desc && <div style={{ fontSize: 12, color: "var(--text-3)", margin: "3px 0 8px" }}>{desc}</div>}
      <div style={{ display: "grid", gap: 6 }}>{children}</div>
    </div>
  );
}

// One address: what it is, whether it is reachable from outside, the value, and
// a way to open it when a browser can.
function EndpointRow({ t, label, value, href, tag, onCopy }) {
  if (!value) return null;
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", fontSize: 12.5 }}>
      <span style={{ color: "var(--text-2)", minWidth: 132 }}>{label}</span>
      {tag && (
        <span className="badge" style={{ padding: "1px 6px", fontSize: 10.5 }}>{tag}</span>
      )}
      <Copyable text={value} onCopy={() => onCopy && onCopy(t.copied)} />
      {href && (
        <a href={href} target="_blank" rel="noreferrer" className="btn-link" style={{ fontSize: 12 }}>
          {t.open}<Icon name="arrowR" size={12} />
        </a>
      )}
    </div>
  );
}

// The credential both published endpoints check. Behind a reveal, and fetched
// only when asked — same contract as the cluster admin credentials, for the
// same reason: a secret on a 15-second poll ends up in every log between here
// and the browser.
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

  const row = { display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", fontSize: 12.5 };
  const label = { color: "var(--text-2)", minWidth: 90 };
  return (
    <div>
      <div style={row}>
        <span style={label}>{t.cred_user}</span>
        <Copyable text={(creds && creds.username) || user} onCopy={() => onToast(t.copied)} />
      </div>
      <div style={{ ...row, marginTop: 6 }}>
        <span style={label}>{t.cred_pass}</span>
        <span className="copyfield">
          <code>{creds && show ? creds.password : "•".repeat(12)}</code>
          {creds && show && (
            <button title={t.copy}
              onClick={() => { navigator.clipboard?.writeText(creds.password); onToast(t.copied); }}>
              <Icon name="copy" size={13} />
            </button>
          )}
        </span>
        <Btn variant="outline" icon={show ? "eyeOff" : "eye"} disabled={busy} onClick={reveal}
          style={{ padding: "5px 10px" }}>
          {busy ? t.cred_loading : creds ? (show ? t.hide : t.show) : t.cred_reveal}
        </Btn>
        <Btn variant="outline" icon="shield" disabled={busy || !!locked}
          onClick={() => setConfirming(true)} style={{ padding: "5px 10px" }}>
          {t.otel_cred_reset}
        </Btn>
      </div>
      {err && <div style={{ marginTop: 6, color: "var(--danger)", fontSize: 12 }}>{err}</div>}

      {/* The consequence is not obvious from the button, so it is spelled out:
          the collector holds exactly one credential and has no grace period. */}
      <Confirm
        open={confirming}
        icon="shield"
        title={t.otel_cred_reset_h}
        body={t.otel_cred_reset_p}
        confirmLabel={t.otel_cred_reset}
        cancelLabel={t.cancel}
        onCancel={() => setConfirming(false)}
        onConfirm={reset}
      >
        <ul style={{ margin: "4px 0 10px 18px", fontSize: 12.5, color: "var(--text-2)", lineHeight: 1.7 }}>
          <li>{t.otel_cred_reset_b1}</li>
          <li>{t.otel_cred_reset_b2}</li>
          <li>{t.otel_cred_reset_b3}</li>
        </ul>
      </Confirm>
    </div>
  );
}

// The OTel observability stack (ADR-053) — the SECOND collection option, shown
// above the recipe grid because both answer the same question ("how does data
// get in here"), one with logs and one with traces + metrics.
//
// `locked` is the verdict `DeploymentView` computed once (ADR-048 invariant 3):
// this panel never re-derives readiness from `health`, it only obeys.
function ObservabilityStackPanel({ d, lang, locked, onToggle, onToast }) {
  const t = STR[lang];
  // Browser-reachable Dashboards origin, or null in port-forward mode — the
  // workspace deep link only makes sense when there is one.
  const dashUrl = d.dashboard_url || "";
  const [info, setInfo] = useState(null);
  const [st, setSt] = useState(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [dropData, setDropData] = useState(false);
  const installed = !!d.otel_stack;

  // Static description (components + cost): fetched once, never hardcoded here.
  useEffect(() => {
    let alive = true;
    API.otelStackInfo().then(i => alive && setInfo(i)).catch(() => {});
    return () => { alive = false; };
  }, []);

  // Live component readiness, on the SAME cadence as the recipe cards and with
  // the same hidden-tab guard — and only while installed, so a deployment
  // without the stack costs zero requests.
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
    try {
      await onToggle(d.id, install, deleteIndices);
    } catch (e) {
      // Pinned here rather than left to a toast that disappears in 3s.
      setErr(e.message || String(e));
    } finally {
      setBusy(false);
    }
  }

  const cost = info
    ? t.otel_cost
        .replace("{0}", (info.cpu_millis / 1000).toFixed(1))
        .replace("{1}", (info.mem_mib / 1024).toFixed(1) + " GiB")
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
    <div className="recipe" style={{ display: "block", marginBottom: 20 }}>
      <div className="rh" style={{ marginBottom: 6 }}>
        <span style={{ color: installed ? "var(--accent)" : "var(--text-3)" }}>
          <Icon name="activity" size={16} />
        </span>
        <span className="rt">{t.otel_h}</span>
        <span className={`badge ${installed ? "green" : ""}`} style={{ padding: "2px 8px", marginLeft: 8 }}>
          {installed ? t.otel_installed : t.otel_not_installed}
        </span>
      </div>
      <div className="rd" style={{ marginBottom: 12 }}>{t.otel_lead}</div>

      {!installed && (
        <>
          <div style={{ fontSize: 12.5, color: "var(--text-2)", marginBottom: 4 }}>{t.otel_adds}</div>
          <ul style={{ margin: "0 0 14px 18px", fontSize: 13, color: "var(--text-2)" }}>
            <li>{t.otel_add_traces}</li>
            <li>{t.otel_add_agents}</li>
            <li>{t.otel_add_metrics}</li>
            <li>{t.otel_add_alerts}</li>
          </ul>
          {info && (
            <>
              <div style={{ fontSize: 12.5, color: "var(--text-2)" }}>{t.otel_comp_h}</div>
              <ul style={{ margin: "0 0 14px 18px", fontSize: 12.5, color: "var(--text-3)" }}>
                {info.components.map(c => (
                  <li key={c.key}>
                    {compLabel[c.key] || c.key}{" "}
                    <span style={{ fontFamily: "var(--font-mono)", fontSize: 11.5 }}>{c.image}</span>
                  </li>
                ))}
              </ul>
              {/* Stated before the click, not after: this is a real bill. */}
              <div style={{ fontSize: 12.5, color: "var(--text-2)", marginBottom: 14 }}>
                {t.otel_cost_h}: <strong>{cost}</strong>
              </div>
            </>
          )}
        </>
      )}

      {installed && st && (
        <div style={{ display: "grid", gap: 20, marginBottom: 18 }}>
          {/* 1. Is it running. One row, because everything below is an address
              and the health of what serves them comes first. */}
          <div style={{ display: "flex", flexWrap: "wrap", gap: 8, alignItems: "center" }}>
            {(st.components || []).map(c => (
              <span key={c.name} className={`badge ${c.ready >= 1 ? "green" : ""}`} style={{ padding: "2px 8px" }}>
                <Icon name={c.ready >= 1 ? "check" : "clock"} size={11} />
                {compLabel[c.name] || c.name}
              </span>
            ))}
            <span className={`badge ${st.datasource ? "green" : ""}`} style={{ padding: "2px 8px" }}>
              <Icon name={st.datasource ? "check" : "clock"} size={11} />
              {st.datasource ? t.otel_ds_ok : t.otel_ds_pending}
            </span>
            <span className={`badge ${st.boards > 0 ? "green" : ""}`} style={{ padding: "2px 8px" }}>
              <Icon name={st.boards > 0 ? "check" : "clock"} size={11} />
              {st.boards > 0 ? fmt(t.otel_boards_ok, st.boards) : t.otel_boards_pending}
            </span>
          </div>
          <div className={`rstat ${st.span_docs > 0 ? "live" : "idle"}`}>
            {st.span_docs > 0
              ? <><Icon name="spark" size={14} />{st.span_docs.toLocaleString()} {t.otel_spans}</>
              : <><Icon name="clock" size={14} />{t.integ_waiting}</>}
          </div>

          {/* 2. Where to send. The question this panel exists to answer, so it
              gets the top slot, and only the three signal endpoints appear —
              the OpenSearch API and the Dashboards URL live on the Overview,
              where they belong to the deployment rather than to this feature. */}
          <EndpointGroup t={t} title={t.otel_send_h} desc={t.otel_send_p} icon="plug">
            <EndpointRow t={t} label={t.otel_ep_otlp_logs} value={st.otlp_logs_url} onCopy={onToast} />
            <EndpointRow t={t} label={t.otel_ep_otlp_metrics} value={st.otlp_metrics_url} onCopy={onToast} />
            <EndpointRow t={t} label={t.otel_ep_otlp_http} value={st.otlp_traces_url} onCopy={onToast} />
            <p className="hint" style={{ margin: "8px 0 0" }}>{t.otel_send_how}</p>
          </EndpointGroup>

          {/* 3. Who may send. Its own block: the credential is shared by the
              three endpoints and rotating it is a decision with consequences. */}
          <EndpointGroup t={t} title={t.otel_cred_h} desc={t.otel_cred_p} icon="shield">
            <OtelCredentials d={d} t={t} user={st.otlp_user} onToast={onToast} locked={locked} />
          </EndpointGroup>

          {/* 4. Where the data shows up. */}
          <EndpointGroup t={t} title={t.otel_screens_h} desc={t.otel_screens_p} icon="activity">
            {dashUrl && st.workspace_path && (
              <EndpointRow t={t} label={t.otel_ep_workspace} value={dashUrl + st.workspace_path}
                href={dashUrl + st.workspace_path} onCopy={onToast} />
            )}
            <ul style={{ margin: "10px 0 0 18px", fontSize: 12.5, color: "var(--text-2)", lineHeight: 1.7 }}>
              <li>{t.otel_screen_traces}</li>
              <li>{t.otel_screen_agent}</li>
              <li>{t.otel_screen_metrics}</li>
              <li>{t.otel_screen_alerts}</li>
              <li>{t.otel_screen_boards}</li>
            </ul>
          </EndpointGroup>
        </div>
      )}

      <div className="ractions">
        {installed ? (
          <Btn variant="danger" icon="trash" disabled={busy || !!locked}
            onClick={() => setConfirming(true)} style={{ padding: "7px 13px" }}>
            {t.otel_uninstall}
          </Btn>
        ) : (
          <Btn variant="primary" size="" icon="plus" disabled={busy || !!locked}
            onClick={() => act(true)} style={{ padding: "7px 13px" }}>
            {busy ? t.otel_installing : t.otel_install}
          </Btn>
        )}
      </div>

      {err && (
        <div style={{ marginTop: 8, color: "var(--danger)", fontSize: 12.5, fontFamily: "var(--font-mono)" }}>
          {err}
        </div>
      )}

      {/* Type-to-confirm, like delete: this removes 17 objects and a volume. */}
      {/* Type-to-confirm, and the destructive OPTION is inside the dialog
          rather than a toggle on the page: the choice only exists at the moment
          of the decision, and the copy says what "recoverable" means here. */}
      <Confirm
        open={confirming}
        title={t.otel_uninstall_h}
        body={t.otel_uninstall_p}
        confirmLabel={t.otel_uninstall}
        cancelLabel={t.cancel}
        requireText={d.id}
        requireLabel={fmt(t.otel_uninstall_type, d.id)}
        onCancel={() => { setConfirming(false); setDropData(false); }}
        onConfirm={() => { setConfirming(false); act(false, dropData); setDropData(false); }}
      >
        <label style={{ display: "flex", gap: 8, alignItems: "flex-start", margin: "4px 0 12px",
                        fontSize: 13, color: "var(--text-2)", cursor: "pointer" }}>
          <input type="checkbox" checked={dropData} style={{ marginTop: 3 }}
            onChange={e => setDropData(e.target.checked)} />
          <span>
            {t.otel_uninstall_drop}
            <span style={{ display: "block", color: dropData ? "var(--danger)" : "var(--text-3)", fontSize: 12, marginTop: 3 }}>
              {dropData ? t.otel_uninstall_drop_on : t.otel_uninstall_drop_off}
            </span>
          </span>
        </label>
      </Confirm>
    </div>
  );
}

// The catalog publishes title/summary as locale → text (en/es/pt today). Fall
// back through en to `alt` so a package shipping one locale still renders
// instead of showing "undefined".
function loc(map, lang, alt) {
  if (!map) return alt;
  return map[lang] || map.en || Object.values(map)[0] || alt;
}

// age_seconds → a short human string. Only ever decorative (it qualifies the
// catalog's freshness); never parsed.
function fmtAge(sec, t) {
  if (sec == null) return "—";
  if (sec < 5) return t.integ_age_now;
  if (sec < 90) return t.integ_age_s.replace("{n}", String(sec));
  if (sec < 5400) return t.integ_age_m.replace("{n}", String(Math.round(sec / 60)));
  return t.integ_age_h.replace("{n}", String(Math.round(sec / 3600)));
}

// The runtime catalog (ADR-039 / ADR-047, #76). Everything on screen comes from
// POST /api/catalog — no compiled-in integration list — so publishing a package
// to the registry is the whole release. Three per-row states: installed (with
// the recorded version), available, and update-available (the catalog offers a
// newer version than the one recorded on the deployment).
//
// The degraded-registry states are RENDERED, not hidden: source "cache" shows a
// stale banner with the registry's own error text, source "bootstrap" explains
// that only the built-in floor is reachable. The tab is never empty or broken
// because the registry is down.
function IntegrationsTab({ d, lang, onToggleStack, onToast, locked }) {
  const t = STR[lang];
  const [view, setView] = useState(null);   // CatalogView, null until loaded
  const [loadErr, setLoadErr] = useState("");
  const [counts, setCounts] = useState({});
  const [busy, setBusy] = useState({});     // id → "install" | "uninstall"
  const [itemErr, setItemErr] = useState({});

  const reload = useCallback(async () => {
    try {
      const v = await API.catalog(d.id);
      setView(v);
      setLoadErr("");
    } catch (e) {
      // Only a transport/auth failure lands here: a down REGISTRY is a 200
      // with source=cache|bootstrap, which renders as a banner below.
      setLoadErr(e.message);
    }
  }, [d.id]);

  useEffect(() => { reload(); }, [reload]);

  const items = view ? view.integrations || [] : [];
  // Installed = a recorded package version, OR the monitor annotation alone —
  // deployments provisioned before versions were recorded (#75) still have a
  // running agent, and must read "installed", not "available".
  const monitors = d.monitors || [];
  const isInstalled = (it) => it.installed_version != null || monitors.includes(it.id);
  const installedSig = items.filter(isInstalled).map(it => it.id).join(",");

  // Keep the live doc-count polling for what's installed.
  useEffect(() => {
    if (!installedSig) return undefined;
    let alive = true;
    const load = async () => {
      if (document.hidden) return;
      for (const k of installedSig.split(",")) {
        try {
          const s = await API.monitoringStatus(d.id, k);
          if (!alive) return;
          setCounts(c => ({ ...c, [k]: s.doc_count }));
        } catch {
          if (!alive) return;
          setCounts(c => ({ ...c, [k]: undefined }));
        }
      }
    };
    load();
    const h = setInterval(load, INTEG_POLL_MS);
    return () => { alive = false; clearInterval(h); };
  }, [d.id, installedSig]);

  // One busy/error lane per row: a failing install must not freeze the tab.
  async function act(id, kind, version) {
    setBusy(b => ({ ...b, [id]: kind }));
    setItemErr(e => ({ ...e, [id]: "" }));
    try {
      if (kind === "uninstall") await API.catalogUninstall(d.id, id);
      else await API.catalogInstall(d.id, id, version);
      onToast(kind === "uninstall" ? t.integ_uninstalled_toast : t.integ_installed_toast);
      await reload();
    } catch (e) {
      // #80: install/uninstall resolve the deployment before mutating, and an
      // unknown OR unowned name is one deliberate 404 carrying the same
      // anti-enumeration text either way. Reaching it from a tab we opened off
      // the (already scope-filtered) deployment list means the deployment went
      // away underneath us — say that, rather than echoing "deployment not
      // found" into a row about an integration.
      setItemErr(er => ({ ...er, [id]: e.status === 404 ? t.integ_dep_gone : e.message }));
    } finally {
      setBusy(b => ({ ...b, [id]: null }));
    }
  }

  if (loadErr) {
    return (
      <div className="view-enter" data-testid="integrations-tab">
        <p className="lead">{t.integ_lead}</p>
        <div className="card pad" data-testid="catalog-error">
          <h3 className="section-title" style={{ marginTop: 0, color: "var(--danger)" }}>
            <Icon name="bolt" size={15} /> {t.integ_load_err}
          </h3>
          <p className="hint" style={{ fontFamily: "var(--font-mono)" }}>{loadErr}</p>
          <Btn variant="outline" icon="clock" onClick={reload} style={{ marginTop: 12 }}>{t.integ_retry}</Btn>
        </div>
      </div>
    );
  }

  if (!view) {
    return (
      <div className="view-enter" data-testid="integrations-tab">
        <p className="lead">{t.integ_lead}</p>
        <p className="hint" data-testid="catalog-loading">{t.integ_loading}</p>
      </div>
    );
  }

  const age = fmtAge(view.age_seconds, t);
  const dashUrl = d.dashboard_url;

  return (
    <div className="view-enter" data-testid="integrations-tab">
      <LockNotice reason={locked} />
      {/* Both surfaces answer "how does data get in here" — traces and metrics
          in the stack panel, logs in the recipe grid — so they belong on one
          screen, not two (ADR-053). The lead introduces both, so it comes
          first rather than sitting between them. */}
      <p className="lead">{t.integ_lead}</p>

      <ObservabilityStackPanel d={d} lang={lang} locked={locked} onToggle={onToggleStack} onToast={onToast} />
      {view.source === "registry" ? (
        <p className="hint" data-testid="catalog-source" data-source="registry"
          style={{ marginTop: -6, marginBottom: 16, fontFamily: "var(--font-mono)", fontSize: 12.5 }}>
          {t.integ_src_registry.replace("{age}", age)}
        </p>
      ) : (
        <div className="card pad" data-testid="catalog-degraded" data-source={view.source}
          style={{ marginBottom: 18, borderColor: "var(--warn-soft)" }}>
          <h3 className="section-title" style={{ marginTop: 0, color: "var(--warn)" }}>
            <Icon name="bolt" size={15} />{" "}
            {view.source === "cache" ? t.integ_stale_h : t.integ_bootstrap_h}
          </h3>
          <p className="hint">
            {view.source === "cache"
              ? t.integ_stale_p.replace("{age}", age)
              : t.integ_bootstrap_p}
          </p>
          {/* The registry's own words, verbatim — the operator needs the reason,
              not a paraphrase of it (ADR-047). */}
          {view.error && (
            <p data-testid="catalog-error-reason"
              style={{ marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--text-3)", wordBreak: "break-word" }}>
              {view.error}
            </p>
          )}
        </div>
      )}

      {items.length === 0 ? (
        <p className="hint" data-testid="catalog-empty">{t.integ_empty}</p>
      ) : (
        <div style={{ display: "grid", gap: 12 }}>
          {items.map(it => {
            const installed = isInstalled(it);
            const state = !installed ? "available" : it.update_available ? "update" : "installed";
            const count = counts[it.id];
            const working = busy[it.id];
            const err = itemErr[it.id];
            return (
              <div className={`recipe ${installed ? "on" : ""}`} key={it.id}
                data-testid={`integration-${it.id}`} data-state={state}>
                <div className="rh">
                  <span style={{ color: installed ? "var(--accent)" : "var(--text-3)" }}>
                    <Icon name={INTEG_ICON[it.id] || "server"} size={16} />
                  </span>
                  <span className="rt">{loc(it.title, lang, it.id)}</span>
                  <span className="badge" style={{ padding: "2px 8px" }}>
                    {t.integ_version} {it.version}
                  </span>
                  {it.builtin && <span className="badge" style={{ padding: "2px 8px" }}>{t.integ_builtin}</span>}
                </div>
                <div className="rd">{loc(it.summary, lang, "")}</div>

                <div className={`rstat ${installed && count > 0 ? "live" : "idle"}`}>
                  {installed && count > 0
                    ? <><Icon name="spark" size={14} />{t.receiving} — {count.toLocaleString()} {t.docs}</>
                    : <>{t.no_data}</>}
                </div>

                <div className="ractions">
                  {installed && (
                    <span className="badge green" style={{ padding: "2px 8px" }} data-testid={`installed-${it.id}`}>
                      <Icon name="check" size={11} />
                      {t.integ_installed} {it.installed_version || `(${t.integ_version_unknown})`}
                    </span>
                  )}
                  {state === "update" && (
                    <span className="badge yellow" style={{ padding: "2px 8px" }} data-testid={`update-badge-${it.id}`}>
                      {t.integ_update_badge}
                    </span>
                  )}
                  {state === "update" && (
                    <Btn variant="primary" icon="arrowR" disabled={!!working || !!locked}
                      data-testid={`update-${it.id}`} style={{ padding: "7px 13px" }}
                      onClick={() => act(it.id, "install", it.version)}>
                      {working === "install" ? t.integ_installing : t.integ_update}
                    </Btn>
                  )}
                  {!installed && (
                    <Btn variant="primary" icon="plus" disabled={!!working || !!locked || !it.compatible}
                      data-testid={`install-${it.id}`} style={{ padding: "7px 13px" }}
                      onClick={() => act(it.id, "install", it.version)}>
                      {working === "install" ? t.integ_installing : t.integ_install}
                    </Btn>
                  )}
                  {installed && (
                    <button className="btn-link" style={{ color: "var(--text-3)" }} disabled={!!working || !!locked}
                      data-testid={`uninstall-${it.id}`} onClick={() => act(it.id, "uninstall")}>
                      {working === "uninstall" ? t.integ_uninstalling : t.integ_uninstall}
                    </button>
                  )}
                  {installed && dashUrl &&
                    <a href={`${dashUrl}/app/dashboards`} target="_blank" rel="noreferrer" className="btn-link">{t.open_dashboard}</a>}
                </div>

                {!it.compatible && (
                  <p style={{ marginTop: 10, marginBottom: 0, fontSize: 12.5, color: "var(--warn)", fontFamily: "var(--font-mono)" }}
                    data-testid={`incompatible-${it.id}`}>
                    {t.integ_incompatible.replace("{v}", it.min_core_version || "?")}
                  </p>
                )}
                {err && (
                  <p style={{ marginTop: 10, marginBottom: 0, fontSize: 12.5, color: "var(--danger)", fontFamily: "var(--font-mono)", wordBreak: "break-word" }}
                    data-testid={`error-${it.id}`}>
                    {err}
                  </p>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

// The per-cluster OpenSearch admin credentials (read from the
// `<cluster>-admin-credentials` Secret on the backend). Since MR !55 each
// cluster's password is CSPRNG-generated, so this reveal is the only way users
// recover it. The password is fetched ONLY on an explicit reveal click — never
// eagerly into the page — and masked again on hide.
// Reading the admin credential and replacing it are the same subject, so they
// are one box: splitting them put "here is your password" and "here is how to
// change it" in separate cards a scroll apart.
function CredentialsPanel({ d, lang, onToast, onReset, locked }) {
  const t = STR[lang];
  const [creds, setCreds] = useState(null); // { username, password }
  const [show, setShow] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [resetting, setResetting] = useState(false);
  // The generated value, shown once right after a reset — the one moment it is
  // visible without going through the reveal flow.
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
    if (creds) { setShow(s => !s); return; } // already fetched — just toggle
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

  function copy(v) {
    if (v) { navigator.clipboard?.writeText(v); onToast(t.copied); }
  }

  const label = busy ? t.cred_loading : creds ? (show ? t.hide : t.show) : t.cred_reveal;

  return (
    <div className="card pad" style={{ marginBottom: 20 }} data-testid="cluster-credentials">
      <h3 className="section-title" style={{ marginTop: 0 }}><Icon name="shield" size={15} /> {t.cred_h}</h3>
      <p className="hint" style={{ marginTop: 4 }}>{t.cred_desc}</p>

      <div style={{ display: "flex", alignItems: "center", gap: 10, marginTop: 14, flexWrap: "wrap", fontSize: 13 }}>
        <span style={{ color: "var(--text-2)", fontFamily: "var(--font-mono)", minWidth: 72 }}>{t.cred_user}</span>
        {creds
          ? <Copyable text={creds.username} onCopy={() => onToast(t.copied)} />
          : <code style={{ color: "var(--text-3)" }}>—</code>}
      </div>

      <div style={{ display: "flex", alignItems: "center", gap: 10, marginTop: 10, flexWrap: "wrap", fontSize: 13 }}>
        <span style={{ color: "var(--text-2)", fontFamily: "var(--font-mono)", minWidth: 72 }}>{t.cred_pass}</span>
        <span className="copyfield">
          <code>{creds && show ? creds.password : "•".repeat(12)}</code>
          {creds && show &&
            <button onClick={() => copy(creds.password)} title={t.copy}><Icon name="copy" size={13} /></button>}
        </span>
        <Btn variant="outline" icon={show ? "eyeOff" : "eye"} disabled={busy} onClick={reveal}
          data-testid="reveal-credentials" style={{ padding: "6px 12px" }}>{label}</Btn>
      </div>

      {err && <p style={{ color: "var(--danger)", fontSize: 12.5, marginTop: 12, fontFamily: "var(--font-mono)" }}>{t.cred_error}</p>}

      <div className="hr" style={{ margin: "18px 0 14px" }} />

      <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 4 }}>{t.sec_reset_h}</div>
      <p className="hint" style={{ marginTop: 0 }}>{t.sec_reset_desc}</p>
      <ul style={{ margin: "8px 0 14px 18px", fontSize: 12.5, color: "var(--text-2)", lineHeight: 1.7 }}>
        <li>{t.sec_reset_b1}</li>
        <li>{t.sec_reset_b2}</li>
        <li>{t.sec_reset_b3}</li>
      </ul>
      <Btn variant="danger" icon="shield" data-testid="reset-pass"
        disabled={resetting || !!locked} onClick={() => setConfirming(true)}>
        {resetting ? t.sec_reset_running : t.reset_pass}
      </Btn>
      <LockNotice reason={locked} />

      <Confirm
        open={confirming}
        icon="shield"
        title={t.sec_reset_confirm_h}
        body={t.sec_reset_confirm_p}
        confirmLabel={t.reset_pass}
        cancelLabel={t.cancel}
        onConfirm={doReset}
        onCancel={() => setConfirming(false)}
      />

      <Confirm
        open={!!fresh}
        icon="shield"
        variant="primary"
        title={t.sec_new_pass_h}
        body={t.sec_new_pass_p}
        confirmLabel={t.copy}
        cancelLabel={t.close}
        onConfirm={() => { copy(fresh.password); setFresh(null); }}
        onCancel={() => setFresh(null)}
      >
        <div style={{ margin: "10px 0" }}>
          <Copyable text={fresh ? fresh.password : ""} onCopy={() => onToast(t.copied)} />
        </div>
      </Confirm>
    </div>
  );
}

// Optional network restriction on this deployment's PUBLIC routes (OpenSearch
// API, Dashboards, OTLP). Default is open — a customer opts in to restriction,
// never out of it by accident — so an empty list is a valid, expected state and
// the copy says so rather than nagging.
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
    <div className="card pad" style={{ marginBottom: 20 }}>
      <h3 className="section-title" style={{ marginTop: 0 }}>
        <Icon name="shield" size={15} /> {t.ipallow_h}
      </h3>
      <p className="hint" style={{ marginTop: 4 }}>{t.ipallow_desc}</p>
      <div style={{ margin: "10px 0" }}>
        <span className={`badge ${open ? "" : "green"}`} style={{ padding: "2px 8px" }}>
          {open ? t.ipallow_state_open : fmt(t.ipallow_state_limited, (d.ip_allow_list || []).length)}
        </span>
      </div>
      <Field label={t.ipallow_field}>
        <textarea className="input" rows={4} value={text} spellCheck={false}
          placeholder={"203.0.113.0/24\n198.51.100.7"}
          style={{ fontFamily: "var(--font-mono)", fontSize: 12.5 }}
          onChange={e => setText(e.target.value)} />
      </Field>
      <p className="hint" style={{ marginTop: -6 }}>{t.ipallow_hint}</p>
      <Btn variant="primary" icon="check" disabled={busy || !dirty || !!locked} onClick={save}>
        {busy ? t.saving : t.save}
      </Btn>
      {err && <div style={{ marginTop: 8, color: "var(--danger)", fontSize: 12.5, fontFamily: "var(--font-mono)" }}>{err}</div>}
    </div>
  );
}

// The next-generation Dashboards UI: workspaces plus the new navigation.
//
// Deployment-level, and deliberately NOT part of the observability stack's
// panel. The stack requires it and turns it on, but a user can want the new UI
// without ever installing the stack — and one who chose it keeps it when the
// stack is uninstalled. `next_ui_chosen` is what says which of those a
// deployment is, and the button copy changes accordingly rather than hiding it.
function NextUiPanel({ d, lang, onToast, locked }) {
  const t = STR[lang];
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [confirm, setConfirm] = useState(false);
  const on = !!d.next_ui;
  // Turning it off under a running stack would take the Observability screens
  // with it — they only render inside a workspace.
  const heldByStack = on && !!d.otel_stack;

  async function apply() {
    setBusy(true); setErr(""); setConfirm(false);
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
    <div className="card pad" style={{ marginBottom: 20 }}>
      <h3 className="section-title" style={{ marginTop: 0 }}>
        <Icon name="layers" size={15} /> {t.nextui_h}
      </h3>
      <p className="hint" style={{ marginTop: 4 }}>{t.nextui_desc}</p>
      <div style={{ margin: "10px 0" }}>
        <span className={`badge ${on ? "green" : ""}`} style={{ padding: "2px 8px" }}>
          {on ? t.nextui_state_on : t.nextui_state_off}
        </span>
      </div>
      <p className="hint">{on ? t.nextui_scope_ws : t.nextui_scope_tenant}</p>
      {heldByStack && <p className="hint">{t.nextui_held}</p>}
      <Btn icon={on ? "chevL" : "check"} variant={on ? "" : "primary"}
        disabled={busy || !!locked || heldByStack}
        onClick={() => setConfirm(true)}>
        {busy ? t.saving : (on ? t.nextui_disable : t.nextui_enable)}
      </Btn>
      {err && <div style={{ marginTop: 8, color: "var(--danger)", fontSize: 12.5, fontFamily: "var(--font-mono)" }}>{err}</div>}
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

// One column, full width: each box owns a row. The tab holds two subjects —
// who can log in, and who can reach the deployment over the network — and both
// have content (a credential pair, a CIDR list) that reads badly in a narrow
// column.
function SecurityTab({ d, lang, onReset, onToast, locked }) {
  return (
    <div className="view-enter" style={{ maxWidth: 760 }}>
      <CredentialsPanel d={d} lang={lang} onToast={onToast} onReset={onReset} locked={locked} />
      <NextUiPanel d={d} lang={lang} onToast={onToast} locked={locked} />
      <IpAllowListPanel d={d} lang={lang} onToast={onToast} locked={locked} />
    </div>
  );
}

function DeploymentView({ d, lang, tab, onTab, onToggleStack, onSaveEdit, onResetPass, onToast, onDelete, openUpgrade }) {
  const t = STR[lang];
  const [confirm, setConfirm] = useState(false);
  // Computed once, here, from the server's verdict — the tabs receive the
  // answer and none of them re-derives readiness from `health` (ADR-050
  // invariant 3). Note it locks THIS deployment only: the list, the nav and
  // every other deployment stay usable while this one comes up.
  const locked = lockReason(d, t);
  const tabs = [
    { key: "overview", label: t.tab_overview, icon: "activity" },
    { key: "edit", label: t.tab_edit, icon: "sliders" },
    { key: "integrations", label: t.tab_integ, icon: "plug" },
    { key: "snapshot", label: t.tab_snapshot, icon: "layers" },
    { key: "security", label: t.tab_sec, icon: "shield" },
    { key: "auth", label: t.tab_auth, icon: "db" },
  ];
  return (
    <div className="view-enter">
      <h1 className="page-title" style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <span style={{ color: "var(--text-3)" }}>{d.id}</span>
      </h1>

      <nav className="tabs" style={{ marginTop: 18 }}>
        {tabs.map(tb => (
          <button key={tb.key} aria-selected={tab === tb.key} onClick={() => onTab(tb.key)}>
            {tb.label}
          </button>
        ))}
      </nav>

      {tab === "overview" && <OverviewTab d={d} lang={lang} onToast={onToast} openUpgrade={openUpgrade} />}
      {tab === "edit" && <EditTab d={d} lang={lang} onSave={onSaveEdit} locked={locked} />}
      {tab === "integrations" && <IntegrationsTab d={d} lang={lang} onToggleStack={onToggleStack} onToast={onToast} locked={locked} />}
      {tab === "snapshot" && <SnapshotTab d={d} lang={lang} onToast={onToast} locked={locked} />}
      {tab === "security" && <SecurityTab d={d} lang={lang} onReset={onResetPass} onToast={onToast} locked={locked} />}
      {tab === "auth" && <AuthProviderTab d={d} lang={lang} onToast={onToast} locked={locked} />}

      <div className="hr" />
      <div>
        <h3 className="section-title" style={{ color: "var(--danger)" }}>{t.danger_zone}</h3>
        {/* Never locked, by decision (ADR-050 invariant 4): a provision that
            never finishes is exactly when the user most needs to remove it, and
            locking this would leave a zombie only kubectl can clear. */}
        <Btn variant="danger" icon="trash" data-testid="delete-deployment" onClick={() => setConfirm(true)}>{t.delete_dep}</Btn>
        {locked && <p className="hint" style={{ marginTop: 8 }}>{t.act_delete_ok}</p>}
      </div>

      {/* Type-to-confirm: deleting a cluster destroys its data and cannot be
          undone, and the deployment screens all look alike — naming the target
          is what separates "yes, this one" from "yes, whatever was open". */}
      <Confirm open={confirm} title={t.delete_dep} body={t.delete_confirm}
        confirmLabel={t.delete} cancelLabel={t.cancel}
        requireText={d.id} requireLabel={fmt(t.delete_type_name, d.id)}
        confirmTestid="delete-confirm" cancelTestid="delete-cancel"
        onCancel={() => setConfirm(false)}
        onConfirm={() => { setConfirm(false); onDelete(d.id); }} />
    </div>
  );
}

export { DeploymentView };
