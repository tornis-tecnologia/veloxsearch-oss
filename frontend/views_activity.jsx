// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Activity panel — what a deployment is doing, and why its controls are
   locked (ADR-050).

   One surface for the three moments that all ask the same question: a cluster
   being created, a version upgrade, and a restart caused by a configuration
   that touches the pod spec (ADR-049 credentials). They differ only in the
   label.

   The rule lives on the server. This file renders `d.activity` and never
   recomputes readiness from `health` — that belief is exactly the bug the ADR
   exists for: during a rolling restart the operator brings nodes back one at a
   time, so between two restarts every pod is up and the cluster reports green
   while the roll still has nodes to go. The panel used to vanish right there.
   ============================================================ */
import React, { useState, useEffect, useRef } from "react";
import { STR } from "./i18n.jsx";
import { Icon, Btn } from "./ui.jsx";
import { API } from "./api.jsx";

// The ladder, in the server's order. Kept as ids: the server sends the id and
// the label is translated here, never prose over the wire (ADR-048 rule).
const STAGES = ["storage", "accepted", "volumes", "nodes", "security", "dashboards", "settling"];

// A duration in words. Hours matter here: the stall this panel exists to
// report ran for sixteen of them, and "960:00" is not a number anyone reads.
// The unit letters are the same in both locales, so they are not translated.
function fmtDur(secs) {
  const s = Math.max(0, Math.floor(Number(secs) || 0));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}min ${s % 60}s`;
  const h = Math.floor(s / 3600);
  return `${h}h ${Math.floor((s % 3600) / 60)}min`;
}

/* --- How long the current activity has been going, in seconds.
   The server measures it against the cluster (`activity.since_secs` — the age
   of the newest node pod, i.e. the last time anything actually advanced), and
   this only keeps the display ticking between SSE frames.

   The old version started a clock at component mount and held it in a ref keyed
   on `activity.kind`. Two things reset it: switching tabs, which remounts the
   panel, and any flip in `kind`. During the 16-hour stall of issue #131 the
   observed clock read 0:20, then 0:16 — a timer measuring the age of a React
   component while claiming to measure the age of an outage. Nothing local
   anchors it now. --- */
function useSinceSecs(a) {
  const server = Math.max(0, Number(a.since_secs) || 0);
  const anchor = useRef({ server: -1, at: 0 });
  if (anchor.current.server !== server) anchor.current = { server, at: Date.now() };
  const [, tick] = useState(0);
  useEffect(() => {
    if (a.kind === "idle") return undefined;
    const h = setInterval(() => tick(x => x + 1), 1000);
    return () => clearInterval(h);
  }, [a.kind]);
  return server + Math.floor((Date.now() - anchor.current.at) / 1000);
}

function fmt(s, ...args) {
  return args.reduce((acc, v, i) => acc.replaceAll(`{${i}}`, v), s || "");
}

/* --- The `nodes` rung answers two different questions depending on what the
   deployment is doing: bringing nodes up (a create) or replacing them (a
   restart / upgrade). Labelling both "Subindo os nós" is what let the panel say
   `Starting the nodes · 0/3` next to a NODES tile reading `3/3 — all ready`.
   Both numbers were true; only one of them was about starting nodes. --- */
function stageLabel(t, stage, kind) {
  if (stage === "nodes") return t[`act_stage_nodes_${kind}`] || t.act_stage_nodes;
  return t[`act_stage_${stage}`] || stage;
}

/* --- …and the counter says which of the two it is counting. --- */
function detailUnit(t, stage, kind) {
  if (stage === "volumes") return t.act_unit_volumes;
  if (stage === "nodes") return t[`act_unit_nodes_${kind}`] || t.act_unit_nodes_creating;
  return "";
}

/* --- Why a stalled activity is stalled, said where it happens rather than in a
   toast (ADR-050 UI rule 5). Composed from the server's structured facts, so
   the sentence is in the user's language; the only strings that crossed the
   wire are the cluster's OWN words — a health colour, an operator component, a
   recovery stage, an index name — reproduced verbatim, the treatment
   `upgrade.reason` and `snapshot.last_error` already get.

   Every line is independently conditional. A cluster that answered health but
   not `_recovery` still gets its shard count; one that answered nothing falls
   through to `act_stall_unknown`, which points at the Details accordion instead
   of inventing a cause. `unassigned_shards` is -1 for "we do not know" and is
   never printed as "0 unassigned shards". --- */
function StallNotice({ a, lang }) {
  const t = STR[lang];
  const b = a.blocked || {};
  const lines = [];
  if (b.health && b.health !== "green") lines.push(fmt(t.act_stall_health, b.health));
  if (b.unassigned_shards > 0) lines.push(fmt(t.act_stall_unassigned, b.unassigned_shards));
  if (b.recovery_index) {
    lines.push(fmt(t.act_stall_recovery, b.recovery_index, b.recovery_stage || "?", fmtDur(b.recovery_secs)));
  }
  // #27: what the app already DID about it — stated as a fact, only after
  // the bounce happened (the backend sets the field on success).
  if (b.remediated_node) lines.push(fmt(t.act_stall_remediated, b.remediated_node));
  if (b.component && b.component_status) {
    lines.push(fmt(t.act_stall_component, b.component, b.component_status));
  }
  if (lines.length === 0) lines.push(t.act_stall_unknown);

  return (
    <div data-testid="activity-stall" style={{
      marginTop: 14, padding: 14, borderRadius: "var(--radius)",
      background: "var(--surface-2)", border: "1px solid var(--warn-soft, var(--border))",
    }}>
      <p style={{ margin: 0, fontSize: 13, color: "var(--text-2)" }}>{t.act_stall_p}</p>
      <ul style={{ margin: "8px 0 0", paddingLeft: 18, fontSize: 13.5, color: "var(--text)" }}>
        {lines.map((l, i) => <li key={i} style={{ padding: "2px 0" }}>{l}</li>)}
      </ul>
      {/* The other half of the truth, and the half the old screen could not
          tell: for sixteen hours this deployment was BOTH stuck and working.
          Reading "0/3 — 25%" alone, an operator concludes the cluster is down
          and starts doing damage to a cluster that was serving fine. */}
      {a.serving && (
        <p style={{ margin: "10px 0 0", fontSize: 13, color: "var(--text-2)" }}>{t.act_stall_serving}</p>
      )}
    </div>
  );
}

function StageRow({ id, current, reached, label, detail }) {
  const done = reached && !current;
  const glyph = done ? "✓" : current ? "▸" : "·";
  const color = done ? "var(--accent)" : current ? "var(--text)" : "var(--text-3)";
  return (
    <li style={{ display: "flex", alignItems: "center", gap: 10, padding: "7px 0" }}>
      <span aria-hidden="true" style={{ color, width: 16, textAlign: "center", fontFamily: "var(--font-mono)" }}>{glyph}</span>
      <span style={{ flex: 1, color: current ? "var(--text)" : "var(--text-2)", fontWeight: current ? 600 : 400 }}>
        {label}
      </span>
      {current && detail && (
        <span className="tnum" style={{ color: "var(--text-3)", fontFamily: "var(--font-mono)", fontSize: 12.5 }}>
          {detail}
        </span>
      )}
    </li>
  );
}

/* --- The "Detalhes" accordion: Kubernetes Events, pod container state and the
   operator's componentsStatus. Fetches on open and polls ONLY while open, so a
   closed accordion costs nothing. Not container logs — a pod stuck before
   start has none, and reading them would need cluster-wide `pods/log`. --- */
function ActivityDetails({ id, lang }) {
  const t = STR[lang];
  const [open, setOpen] = useState(false);
  const [lines, setLines] = useState(null);
  const [err, setErr] = useState("");

  useEffect(() => {
    if (!open) return undefined;
    let alive = true;
    const tick = () => {
      API.deploymentActivityLog(id)
        .then((l) => { if (alive) { setLines(l || []); setErr(""); } })
        .catch((e) => { if (alive) setErr(e.message); });
    };
    tick();
    const h = setInterval(tick, 5000);
    return () => { alive = false; clearInterval(h); };
  }, [open, id]);

  const tone = (s) => (s === "error" ? "var(--danger)" : s === "warn" ? "var(--warn)" : "var(--text-3)");

  return (
    <details open={open} style={{ marginTop: 16 }} data-testid="activity-details">
      <summary
        style={{ cursor: "pointer", fontSize: 13, color: "var(--text-2)" }}
        onClick={(e) => { e.preventDefault(); setOpen((o) => !o); }}
      >
        <Icon name={open ? "chevD" : "chevR"} size={12} /> {t.act_details}
      </summary>
      {open && (
        <div style={{ marginTop: 12 }}>
          {err && <p className="hint" style={{ color: "var(--danger)" }}>{err}</p>}
          {!err && lines === null && <p className="hint">{t.act_details_loading}</p>}
          {!err && lines !== null && lines.length === 0 && <p className="hint">{t.act_details_empty}</p>}
          {lines && lines.length > 0 && (
            <div style={{ display: "grid", gap: 8, maxHeight: 340, overflowY: "auto" }}>
              {lines.map((l, i) => (
                <div key={i} style={{
                  padding: "8px 10px", borderRadius: "var(--radius)",
                  background: "var(--surface-2)", border: "1px solid var(--border)",
                }}>
                  <div style={{ display: "flex", gap: 8, alignItems: "baseline", flexWrap: "wrap", fontSize: 12.5 }}>
                    <span style={{ color: tone(l.severity), fontFamily: "var(--font-mono)" }}>{l.title || l.source}</span>
                    <span style={{ color: "var(--text-3)", fontFamily: "var(--font-mono)" }}>{l.object}</span>
                    <span className="spacer" style={{ flex: 1 }} />
                    <span style={{ color: "var(--text-3)", fontSize: 11.5 }}>{l.at ? l.at.replace("T", " ").slice(0, 19) : t.act_src_state}</span>
                  </div>
                  {l.detail && (
                    <pre style={{
                      margin: "6px 0 0", whiteSpace: "pre-wrap", wordBreak: "break-word",
                      fontSize: 12, color: "var(--text-2)", fontFamily: "var(--font-mono)",
                    }}>{l.detail}</pre>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </details>
  );
}

/* --- The panel. Renders whenever the server says this deployment is not idle;
   hides only on `settled`. Absorbs the old ProvisioningPanel and
   UpgradeProgress, which asked the same question in two places. --- */
function ActivityPanel({ d, lang }) {
  const t = STR[lang];
  const a = d.activity || { kind: "idle" };
  const elapsed = useSinceSecs(a);

  if (a.kind === "idle") return null;

  // The server owns the threshold now (`activity.stalled`, ADR-050 invariant 3
  // — the UI renders the verdict and never re-runs the rule). It used to be a
  // client-side constant compared against a clock the client started itself,
  // which is how a 16-hour stall kept presenting as a fresh 20-second wait.
  const slow = !!a.stalled;
  const reachedIdx = STAGES.indexOf(a.stage);
  const heading = t[`act_h_${a.kind}`] || t.act_h_creating;
  const unit = detailUnit(t, a.stage, a.kind);

  return (
    <div className="card pad" data-testid="activity-panel" data-activity-kind={a.kind}
      data-activity-stalled={slow ? "true" : "false"}
      style={{ marginTop: 16, borderColor: slow ? "var(--warn)" : undefined }}>
      <h3 className="section-title" style={{ marginTop: 0, color: slow ? "var(--warn)" : undefined }}>
        <Icon name={slow ? "bolt" : "clock"} size={15} />{" "}
        {slow ? fmt(t.act_stall_h, fmtDur(elapsed)) : heading}
      </h3>
      <p className="hint">{slow ? t.act_slow_p : t[`act_p_${a.kind}`] || ""}</p>

      {/* Why it is stuck, immediately under the heading that says it is stuck.
          Before anything else on the panel, because it is the only part the
          user can act on. */}
      {slow && <StallNotice a={a} lang={lang} />}

      {/* Overall progress. The words carry the meaning; the number is the
          secondary signal, which is why the stage label leads. The counter
          names what it counts — "0/3 nodes replaced" is a different fact from
          the Overview's "3/3 nodes ready", and both can be true at once. */}
      <div style={{ display: "flex", alignItems: "baseline", gap: 10, marginTop: 14 }}>
        <span style={{ flex: 1, fontSize: 14 }}>
          {stageLabel(t, a.stage, a.kind)}
          {a.detail && (
            <span style={{ color: "var(--text-3)" }} data-testid="activity-detail">
              {" "}· {a.detail}{unit ? ` ${unit}` : ""}
            </span>
          )}
        </span>
        <span className="tnum" style={{ fontFamily: "var(--font-mono)", fontSize: 14 }} data-testid="activity-percent">
          {a.percent}%
        </span>
      </div>
      <div className={`bar${slow ? " warn" : ""}`} style={{ marginTop: 8 }}>
        <span style={{ width: `${Math.max(2, a.percent)}%` }} />
      </div>

      {/* The node pair comes off the activity verdict, which is the same pair
          the Overview tile renders — one source, so they cannot disagree. */}
      <div className="meta" style={{ marginTop: 12 }}>
        <span className={`badge ${d.health === "green" ? "green" : d.health === "red" ? "" : "yellow"}`}>
          <span className="dot" />{d.health}
        </span>
        <span data-testid="activity-nodes">
          {a.nodes_ready ?? d.nodes_ready}/{(a.nodes_total || d.node_count) || "?"} {t.nodes} {t.act_meta_ready}
        </span>
        <span className="sep">·</span>
        <span className="tnum" data-testid="activity-elapsed">{fmtDur(elapsed)}</span>
      </div>

      {/* The ladder. A create walks all seven; an upgrade or restart starts at
          `nodes`, so the earlier rungs render as already passed. */}
      <ul style={{ listStyle: "none", margin: "14px 0 0", padding: 0 }}>
        {STAGES.map((s, i) => (
          <StageRow key={s} id={s} label={stageLabel(t, s, a.kind)}
            current={s === a.stage} reached={i <= reachedIdx}
            detail={a.detail} />
        ))}
      </ul>

      {slow && <p className="hint" style={{ marginTop: 14 }}>{t.act_slow_hint}</p>}

      <ActivityDetails id={d.id} lang={lang} />
    </div>
  );
}

/* --- Why this deployment's mutating controls refuse right now, or null.
   Reads the server's verdict; never re-derives it (ADR-050 invariant 3). --- */
function lockReason(d, t) {
  const a = (d && d.activity) || null;
  if (!a || !a.locks_edits) return null;
  return t[`act_lock_${a.kind}`] || t.act_lock_restarting;
}

/* --- The banner a locked tab shows above its fields. A blocked state explains
   itself where it is, never as a dead button (ADR-045 UI rule 1). --- */
function LockNotice({ reason }) {
  if (!reason) return null;
  return (
    <div data-testid="lock-notice" style={{
      display: "flex", gap: 12, padding: 14, marginBottom: 16, borderRadius: "var(--radius)",
      background: "var(--info-soft)", border: "1px solid var(--border)", color: "var(--text-2)", fontSize: 14,
    }}>
      <span style={{ color: "var(--info)", flexShrink: 0 }}><Icon name="clock" size={18} /></span>
      <span>{reason}</span>
    </div>
  );
}

export { ActivityPanel, ActivityDetails, lockReason, LockNotice, STAGES, fmtDur, stageLabel };
