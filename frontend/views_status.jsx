// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Status view — cluster list + first-run empty state
   ============================================================ */
import { STR, sizeMeta } from "./i18n.jsx";
import { Icon, Btn } from "./ui.jsx";
import { fmtDur, stageLabel } from "./views_activity.jsx";

function PurposeBadge({ purpose, lang }) {
  const t = STR[lang];
  const map = {
    observability: { icon: "activity", label: t.p_obs_t },
    security: { icon: "shield", label: t.p_sec_t },
    search: { icon: "search", label: t.p_search_t },
  };
  const m = map[purpose] || map.observability;
  return (
    <span className="badge" style={{ textTransform: "none" }}>
      <Icon name={m.icon} size={12} />{m.label}
    </span>
  );
}

function ClusterArt() {
  // simple node-cluster motif: circles + connecting lines (allowed simple shapes)
  return (
    <svg className="art" width="132" height="100" viewBox="0 0 132 100" fill="none">
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

// Health → the stripe's tone. Anything the operator has not classified yet
// (provisioning, unknown) stays neutral rather than pretending to be a state.
function healthTone(health) {
  return ["green", "yellow", "red"].includes(health) ? health : "unknown";
}

// "há 3h" / "3h ago" from an RFC 3339 timestamp. Coarse on purpose: an admin
// list wants "is this new or old", not a clock.
function relAge(iso, lang) {
  const ms = Date.now() - new Date(iso).getTime();
  if (!Number.isFinite(ms) || ms < 0) return "";
  const min = Math.floor(ms / 60000);
  const val = min < 60 ? `${min}m` : min < 1440 ? `${Math.floor(min / 60)}h` : `${Math.floor(min / 1440)}d`;
  return lang === "pt" ? `há ${val}` : lang === "es" ? `hace ${val}` : `${val} ago`;
}

function StatusView({ deployments, lang, onOpen, onUpgrade, onCreate }) {
  const t = STR[lang];

  if (deployments.length === 0) {
    const steps = [t.status_empty_step1, t.status_empty_step2, t.status_empty_step3];
    return (
      <div className="view-enter">
        <h1 className="page-title">{STR[lang].nav_status}</h1>
        <div className="card" style={{ marginTop: 18 }}>
          <div className="empty">
            <ClusterArt />
            <h2>{t.status_empty_h}</h2>
            <p>{t.status_empty_p}</p>
            <Btn variant="primary" size="lg" icon="plus" onClick={onCreate}>{t.status_empty_cta}</Btn>
            <div style={{
              display: "grid", gap: 10, maxWidth: 420, margin: "34px auto 0", textAlign: "left",
            }}>
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
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="view-enter">
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 16, flexWrap: "wrap" }}>
        <div>
          <h1 className="page-title">{t.nav_status}</h1>
          <p className="lead" style={{ marginBottom: 0 }}>{t.status_lead}</p>
        </div>
        <Btn variant="primary" icon="plus" onClick={onCreate}>{t.new_cluster}</Btn>
      </div>

      {/* Four zones per card, same order in every card so the eye learns the
          layout once: identity → what it is → what it has → what you can do.
          Health is a left stripe (scannable) AND a word (never color alone). */}
      <div className="cluster-grid">
        {deployments.map(d => {
          // ADR-050: while a deployment is busy the card names the stage and
          // the percent instead of a raw health word that is misleading
          // mid-roll (green with a node still to replace).
          const act = d.activity || { kind: "idle" };
          const busy = act.kind !== "idle";
          return (
            <div key={d.id} className={`cluster-card h-${healthTone(d.health)}`}>
              {/* 1 — identity */}
              <div style={{ minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 9, minWidth: 0 }}>
                  <span className={`statusdot ${healthTone(d.health)}`} style={{ flexShrink: 0 }} />
                  <span className="name" title={d.id}>{d.id}</span>
                </div>
                <div className="sub" style={{ marginTop: 3 }} data-testid="card-state">
                  {busy
                    ? <>
                        {stageLabel(t, act.stage, act.kind)} · {act.percent}%
                        {/* A stall has to be visible from the LIST too, not
                            only once you open the deployment — the list is
                            where somebody notices at all (issue #131). */}
                        {act.stalled && (
                          <span style={{ color: "var(--warn)" }}>
                            {" "}· {t.act_badge_stalled} {fmtDur(act.since_secs)}
                          </span>
                        )}
                      </>
                    : d.health}
                  {d.created_at && <> · {relAge(d.created_at, lang)}</>}
                </div>
              </div>

              {/* 2 — what it is: purpose, running version, and the one action
                  that is not "open it" (a pending upgrade). */}
              <div style={{ display: "flex", alignItems: "center", gap: 7, flexWrap: "wrap" }}>
                <PurposeBadge purpose={d.purpose} lang={lang} />
                {d.version && (
                  <span className="badge" style={{ textTransform: "none" }}>
                    <Icon name="layers" size={12} />OpenSearch {d.version}
                  </span>
                )}
                {/* Hourly upstream check found a newer release (ADR-048 rev. 2).
                    Clicking opens the deployment with the upgrade dialog — the
                    irreversible step still asks for confirmation. */}
                {d.suggested_version && !busy && (
                  <button
                    className="badge"
                    style={{ cursor: "pointer", textTransform: "none", color: "var(--accent)", borderColor: "var(--accent-border)" }}
                    title={t.upg_btn}
                    onClick={() => (onUpgrade ? onUpgrade(d.id) : onOpen(d.id))}>
                    <Icon name="arrowR" size={12} />Upgrade v{d.suggested_version}
                  </button>
                )}
                {busy && (
                  <span className="badge" style={{ textTransform: "none", color: "var(--warn)", borderColor: "var(--warn-soft)" }}>
                    <Icon name="clock" size={12} />{t[`act_badge_${act.kind}`] || t.upg_state_upgrading}
                  </span>
                )}
              </div>

              <div className="rule" />

              {/* 3 — what it has. Label/value pairs, tabular values, so the
                  same fact sits in the same spot on every card. */}
              <div className="spec-grid">
                <div><span className="k">{t.nodes}</span><span className="v">{d.nodes_ready}/{d.node_count}</span></div>
                <div><span className="k">{t.sz_mem}</span><span className="v">{d.mem || "—"}</span></div>
                <div><span className="k">{t.sz_disk}</span><span className="v">{d.disk || "—"}</span></div>
                <div><span className="k">{t.tab_integ}</span><span className="v">{(d.monitors || []).length}</span></div>
              </div>

              <div className="rule" />

              {/* 4 — actions: the primary path left, the escape hatch right. */}
              <div className="card-actions">
                <Btn variant="outline" iconR="chevR" onClick={() => onOpen(d.id)}>{t.details}</Btn>
                {d.dashboard_url && (
                  <a className="btn btn-ghost" href={d.dashboard_url} target="_blank" rel="noreferrer">
                    Dashboard<Icon name="arrowR" size={13} />
                  </a>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export { StatusView, PurposeBadge };
