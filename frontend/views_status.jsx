// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Deployments — the list, the first-run empty state, and the "getting
   started" checklist.

   A fleet summary leads: the first question an operator asks the list is
   "does anything need me?", so the counts come before any row and each one
   is a filter. The rows are a table, ordered attention-first; on a phone the
   same table reads as cards (styles.css .dtable).
   ============================================================ */
import { useMemo, useState } from "react";
import { STR } from "./i18n.jsx";
import { Icon, Btn, StatusPill, healthTone, EmptyState, PageHeader, fmtInt } from "./ui.jsx";
import { fmtDur, stageLabel } from "./views_activity.jsx";

function fmt(s, ...args) { return args.reduce((acc, v, i) => acc.replaceAll(`{${i}}`, v), s || ""); }

function PurposeBadge({ purpose, lang }) {
  const t = STR[lang];
  const map = {
    observability: { icon: "activity", label: t.p_obs_t },
    security: { icon: "shield", label: t.p_sec_t },
    search: { icon: "search", label: t.p_search_t },
  };
  const m = map[purpose] || map.observability;
  return (
    <span className="badge">
      <Icon name={m.icon} size={12} />{m.label}
    </span>
  );
}

function ClusterArt() {
  return (
    <svg className="art" width="132" height="100" viewBox="0 0 132 100" fill="none" aria-hidden="true">
      <path d="M66 26L30 62M66 26l36 36M30 62h72" stroke="var(--border-strong)" strokeWidth="1.5" />
      <circle cx="66" cy="26" r="13" fill="var(--accent-soft)" stroke="var(--accent)" strokeWidth="1.5" />
      <circle cx="30" cy="62" r="13" fill="var(--surface-2)" stroke="var(--border-strong)" strokeWidth="1.5" />
      <circle cx="102" cy="62" r="13" fill="var(--surface-2)" stroke="var(--border-strong)" strokeWidth="1.5" />
      <circle cx="66" cy="26" r="4" fill="var(--accent)" />
      <circle cx="30" cy="62" r="4" fill="var(--text-3)" />
      <circle cx="102" cy="62" r="4" fill="var(--text-3)" />
    </svg>
  );
}

// "há 3h" from an RFC 3339 timestamp. Coarse on purpose: a list wants "is this
// new or old", not a clock.
function relAge(iso, t) {
  const ms = Date.now() - new Date(iso).getTime();
  if (!Number.isFinite(ms) || ms < 0) return "";
  const min = Math.floor(ms / 60000);
  if (min < 60) return fmt(t.ago_m, min);
  if (min < 1440) return fmt(t.ago_h, Math.floor(min / 60));
  return fmt(t.ago_d, Math.floor(min / 1440));
}

// What a row says about its backup: the schedule when there is one, the
// policy state when it is not fine, and "no backup" otherwise.
function backupText(d, t) {
  const s = d.snapshot || {};
  if (!s.configured) return { text: t.list_backup_none, tone: "muted" };
  if (s.policy_state === "ERROR") return { text: t.snap_chip_err, tone: "bad" };
  if (s.policy_state === "PENDING") return { text: t.snap_chip_pending, tone: "muted" };
  return { text: s.schedule ? `${t.snap_chip_on} ${s.schedule}` : t.snap_chip_on, tone: "ok" };
}

// Attention-first ordering: red, then yellow, then busy, then green.
function rank(d) {
  const busy = d.activity && d.activity.kind !== "idle";
  if (d.health === "red") return 0;
  if (d.health === "yellow") return 1;
  if (busy) return 2;
  if (d.health === "green") return 4;
  return 3;
}

function lsGet(k) { try { return localStorage.getItem(k); } catch (e) { return null; } }
function lsSet(k, v) { try { localStorage.setItem(k, v); } catch (e) {} }

/* --- Getting started: the three things a new installation does before it
   has shown any value, derived from live state — never a stored progress.
   Dismissal is a UI preference and the one thing kept in localStorage. --- */
function GettingStarted({ deployments, t, onOpen, onCreate }) {
  const [dismissed, setDismissed] = useState(() => lsGet("velox-gs-dismissed") === "1");
  const hasDep = deployments.length > 0;
  const hasInteg = deployments.some(d => (d.monitors || []).length > 0 || d.otel_stack);
  const hasBackup = deployments.some(d => d.snapshot && d.snapshot.configured);
  const first = deployments[0];
  const steps = [
    { key: "create", label: t.gs_create, done: hasDep, go: onCreate },
    { key: "integ", label: t.gs_integ, done: hasInteg, go: () => first && onOpen(first.id, "data") },
    { key: "backup", label: t.gs_backup, done: hasBackup, go: () => first && onOpen(first.id, "backup") },
  ];
  const done = steps.filter(s => s.done).length;
  if (dismissed || done === steps.length) return null;
  const c = 2 * Math.PI * 17;
  return (
    <div className="card checklist" data-testid="getting-started">
      <svg className="ring" viewBox="0 0 42 42" aria-hidden="true">
        <circle cx="21" cy="21" r="17" fill="none" stroke="var(--surface-3)" strokeWidth="5" />
        <circle cx="21" cy="21" r="17" fill="none" stroke="var(--accent)" strokeWidth="5" strokeLinecap="round"
          strokeDasharray={`${c * done / steps.length} ${c}`} transform="rotate(-90 21 21)" />
        <text x="21" y="25" textAnchor="middle" fill="var(--text)" fontSize="11" fontFamily="var(--font-mono)">{done}/{steps.length}</text>
      </svg>
      <div>
        <h3>{t.gs_h}</h3>
        <div className="steps">
          {steps.map(s => (
            <button key={s.key} type="button" className={s.done ? "done" : "todo"} onClick={s.done ? undefined : s.go}
              disabled={!s.done && !hasDep && s.key !== "create"}>
              {s.done && <Icon name="check" size={12} />}{s.label}
            </button>
          ))}
        </div>
      </div>
      <Btn variant="outline" size="sm" icon="x" className="dismiss" onClick={() => { lsSet("velox-gs-dismissed", "1"); setDismissed(true); }}>{t.gs_dismiss}</Btn>
    </div>
  );
}

function StatusView({ deployments, loaded, lang, onOpen, onUpgrade, onCreate }) {
  const t = STR[lang];
  const [q, setQ] = useState("");
  const [purpose, setPurpose] = useState("");
  const [only, setOnly] = useState(""); // "" | ok | warn | busy | upg | nobak
  const [sort, setSort] = useState("attention");

  const counts = useMemo(() => {
    const c = { ok: 0, warn: 0, busy: 0, upg: 0, nobak: 0 };
    deployments.forEach(d => {
      const busy = d.activity && d.activity.kind !== "idle";
      if (busy) c.busy++;
      else if (d.health === "green") c.ok++;
      else c.warn++;
      if (d.suggested_version && !busy) c.upg++;
      if (!(d.snapshot && d.snapshot.configured)) c.nobak++;
    });
    return c;
  }, [deployments]);

  const rows = useMemo(() => {
    const needle = q.trim().toLowerCase();
    let r = deployments.filter(d => {
      const busy = d.activity && d.activity.kind !== "idle";
      if (needle && !d.id.toLowerCase().includes(needle)) return false;
      if (purpose && d.purpose !== purpose) return false;
      if (only === "ok" && (busy || d.health !== "green")) return false;
      if (only === "warn" && (busy || d.health === "green")) return false;
      if (only === "busy" && !busy) return false;
      if (only === "upg" && (!d.suggested_version || busy)) return false;
      if (only === "nobak" && d.snapshot && d.snapshot.configured) return false;
      return true;
    });
    if (sort === "name") r = [...r].sort((a, b) => a.id.localeCompare(b.id));
    else if (sort === "newest") r = [...r].sort((a, b) => new Date(b.created_at || 0) - new Date(a.created_at || 0));
    else r = [...r].sort((a, b) => rank(a) - rank(b) || a.id.localeCompare(b.id));
    return r;
  }, [deployments, q, purpose, only, sort]);

  if (loaded && deployments.length === 0) {
    const steps = [t.status_empty_step1, t.status_empty_step2, t.status_empty_step3];
    return (
      <div className="view-enter">
        <PageHeader title={t.nav_deployments} />
        <div className="card">
          <EmptyState art={<ClusterArt />} title={t.status_empty_h} body={t.status_empty_p}
            action={<Btn variant="primary" size="lg" icon="plus" onClick={onCreate} data-testid="create-first">{t.status_empty_cta}</Btn>}>
            <div style={{ display: "grid", gap: 10, maxWidth: 420, margin: "30px auto 0", textAlign: "left" }}>
              {steps.map((s, i) => (
                <div key={i} style={{ display: "flex", alignItems: "center", gap: 12 }}>
                  <span style={{
                    width: 24, height: 24, borderRadius: "50%", flexShrink: 0,
                    display: "grid", placeItems: "center", fontFamily: "var(--font-mono)",
                    fontSize: 12, color: "var(--accent)", background: "var(--accent-soft)",
                    border: "1px solid var(--accent-border)",
                  }}>{i + 1}</span>
                  <span style={{ fontSize: 14, color: "var(--text-2)" }}>{s}</span>
                </div>
              ))}
            </div>
          </EmptyState>
        </div>
      </div>
    );
  }

  const nodesReady = deployments.reduce((s, d) => s + (d.nodes_ready || 0), 0);
  const nodesTotal = deployments.reduce((s, d) => s + (d.node_count || 0), 0);
  const sumBtn = (key, label, n, dotVar, icon) => (
    <button type="button" aria-pressed={only === key} onClick={() => setOnly(o => o === key ? "" : key)} data-testid={`sum-${key}`}>
      <span className="k">{dotVar ? <i style={{ background: dotVar }} /> : <Icon name={icon} size={12} />}{label}</span>
      <span className="v">{fmtInt(n, lang)}</span>
    </button>
  );

  return (
    <div className="view-enter">
      <PageHeader title={t.nav_deployments}
        meta={loaded ? <>
          <span>{deployments.length === 1 ? t.list_count_one : fmt(t.list_count, deployments.length)}</span>
          <span className="sep">·</span>
          <span>{fmt(t.list_nodes, nodesReady, nodesTotal)}</span>
        </> : null}
        actions={<Btn variant="primary" icon="plus" onClick={onCreate} data-testid="create-deployment">{t.new_cluster}</Btn>} />

      <div className="card summary-strip" data-testid="fleet-summary">
        {sumBtn("ok", t.list_sum_ok, counts.ok, "var(--accent)")}
        {sumBtn("warn", t.list_sum_warn, counts.warn, "var(--warn)")}
        {sumBtn("busy", t.list_sum_busy, counts.busy, "var(--info)")}
        {sumBtn("upg", t.list_sum_upg, counts.upg, null, "arrowUp")}
        {sumBtn("nobak", t.list_sum_nobak, counts.nobak, null, "db")}
      </div>

      <GettingStarted deployments={deployments} t={t} onOpen={onOpen} onCreate={onCreate} />

      <div className="filters">
        <label className="search">
          <Icon name="search" size={14} className="ic" />
          <input className="input" value={q} onChange={e => setQ(e.target.value)} placeholder={t.list_filter_ph}
            aria-label={t.list_filter_ph} data-testid="list-filter" />
        </label>
        <button type="button" className="chip" aria-pressed={purpose === ""} onClick={() => setPurpose("")}>{t.list_all}</button>
        <button type="button" className="chip" aria-pressed={purpose === "observability"} onClick={() => setPurpose(p => p === "observability" ? "" : "observability")}>{t.p_obs_t}</button>
        <button type="button" className="chip" aria-pressed={purpose === "security"} onClick={() => setPurpose(p => p === "security" ? "" : "security")}>{t.p_sec_t}</button>
        <button type="button" className="chip" aria-pressed={purpose === "search"} onClick={() => setPurpose(p => p === "search" ? "" : "search")}>{t.p_search_t}</button>
        <label className="right" style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 13, color: "var(--text-3)" }}>
          {t.sort_by}
          <select className="select" style={{ width: "auto", padding: "4px 30px 4px 10px", fontSize: 12.5 }} value={sort} onChange={e => setSort(e.target.value)} aria-label={t.sort_by}>
            <option value="attention">{t.list_sort_attention}</option>
            <option value="name">{t.list_sort_name}</option>
            <option value="newest">{t.list_sort_newest}</option>
          </select>
        </label>
      </div>

      <div className="card">
        {rows.length === 0 ? (
          <div className="empty" style={{ padding: "32px 16px" }}>
            <p style={{ margin: 0 }}>{t.list_no_match}</p>
            <Btn variant="outline" size="sm" style={{ marginTop: 12 }} onClick={() => { setQ(""); setPurpose(""); setOnly(""); }}>{t.list_clear_filter}</Btn>
          </div>
        ) : (
          <table className="dtable" data-testid="deployments-table">
            <thead>
              <tr>
                <th>{t.col_name}</th><th>{t.col_status}</th><th>{t.col_purpose}</th><th>{t.col_version}</th>
                <th>{t.col_nodes}</th><th>{t.col_backup}</th><th>{t.col_age}</th><th aria-label={t.more_actions} />
              </tr>
            </thead>
            <tbody>
              {rows.map(d => {
                const act = d.activity || { kind: "idle" };
                const busy = act.kind !== "idle";
                const bak = backupText(d, t);
                return (
                  <tr key={d.id} className="clickable" data-testid={`dep-row-${d.id}`} data-health={healthTone(d.health)}
                    onClick={() => onOpen(d.id)}>
                    <td className="nm" data-label={t.col_name}><span className={`statusdot ${busy ? "" : healthTone(d.health)}`} style={{ marginRight: 8 }} />{d.id}</td>
                    <td data-label={t.col_status}>
                      <StatusPill health={d.health} t={t} size="sm" testid="card-state"
                        busy={busy ? `${t[`act_badge_${act.kind}`] || t.upg_state_upgrading} · ${act.percent}%` : null} />
                      {busy && act.stalled && (
                        <span className="muted" style={{ marginLeft: 8, color: "var(--warn-text)", fontSize: 12 }}>
                          {t.act_badge_stalled} {fmtDur(act.since_secs)}
                        </span>
                      )}
                      {busy && !act.stalled && (
                        <span className="muted" style={{ marginLeft: 8, fontSize: 12 }}>{stageLabel(t, act.stage, act.kind)}</span>
                      )}
                    </td>
                    <td data-label={t.col_purpose}><PurposeBadge purpose={d.purpose} lang={lang} /></td>
                    <td className="num nowrap" data-label={t.col_version}>
                      {d.version || "—"}
                      {d.suggested_version && !busy && (
                        <button type="button" className="badge green" style={{ marginLeft: 8 }}
                          title={t.upg_btn} onClick={e => { e.stopPropagation(); onUpgrade(d.id); }}
                          data-testid={`upgrade-tag-${d.id}`}>
                          <Icon name="arrowUp" size={11} />{fmt(t.list_upg_avail, d.suggested_version)}
                        </button>
                      )}
                    </td>
                    <td className="num" data-label={t.col_nodes}>{d.nodes_ready}/{d.node_count}</td>
                    <td data-label={t.col_backup} className={bak.tone === "muted" ? "muted" : ""}
                      style={bak.tone === "bad" ? { color: "var(--danger-text)" } : bak.tone === "ok" ? { fontFamily: "var(--font-mono)", fontSize: 12.5 } : undefined}>
                      {bak.text}
                    </td>
                    <td className="muted nowrap" data-label={t.col_age}>{d.created_at ? relAge(d.created_at, t) : "—"}</td>
                    <td className="actions">
                      {d.dashboard_url && !busy && (
                        <a className="btn btn-outline btn-sm" href={d.dashboard_url} target="_blank" rel="noreferrer"
                          onClick={e => e.stopPropagation()}>Dashboards<Icon name="ext" size={12} /></a>
                      )}
                      <Btn variant="ghost" size="sm" iconR="chevR" data-testid={`dep-open-${d.id}`} className="dep-open"
                        onClick={e => { e.stopPropagation(); onOpen(d.id); }}>
                        {busy ? t.list_follow : t.list_open}
                      </Btn>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

export { StatusView, PurposeBadge, relAge };
