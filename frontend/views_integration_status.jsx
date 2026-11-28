// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   views_integration_status.jsx — the install contract on screen (ADR-075).

   An installed integration says where it stands, never just "Installed":
     working           — documents arrive and every panel's fields resolve
     waiting_for_data  — installed correctly; WHAT is expected and FROM WHERE
     blocked           — WHY it cannot work and the exact remediation

   And before the user commits, the preflight lists what the core can already
   tell would stand in the way. Both come from the backend verbatim; nothing
   here decides a state.
   ============================================================ */
import { useEffect, useState } from "react";
import { STR } from "./i18n.jsx";
import { API } from "./api.jsx";
import { Banner } from "./ui.jsx";

// Poll fast while the install settles, slowly once it works.
const FAST_MS = 15000;
const SLOW_MS = 60000;

function IntegrationStatus({ d, id, lang, initial }) {
  const t = STR[lang];
  const [s, setS] = useState(initial || null);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let alive = true;
    let timer = null;
    const tick = async () => {
      let next = FAST_MS;
      try {
        const r = await API.integrationStatus(d.id, id);
        if (!alive) return;
        setS(r);
        if (r.state === "working") next = SLOW_MS;
      } catch { /* keep the last state; the next poll retries */ }
      if (alive) timer = setTimeout(tick, next);
    };
    tick();
    return () => { alive = false; if (timer) clearTimeout(timer); };
  }, [d.id, id]);

  if (!s) return <p className="hint" data-testid={`integ-status-${id}`} data-state="checking">{t.integ_status_checking}</p>;
  const badge = {
    working: ["green", t.integ_status_working],
    waiting_for_data: ["yellow", t.integ_status_waiting],
    blocked: ["red", t.integ_status_blocked],
  }[s.state] || ["", s.state];
  const failed = (s.checks || []).filter(c => !c.ok);
  return (
    <div data-testid={`integ-status-${id}`} data-state={s.state} style={{ marginTop: 8 }}>
      <span className={`badge ${badge[0]}`}>{badge[1]}</span>
      {s.state === "working" && <span className="hint" style={{ marginLeft: 8 }}>{t.integ_status_docs.replace("{n}", s.docs)}</span>}
      {s.state === "waiting_for_data" && (
        <p className="hint" style={{ margin: "6px 0 0" }}>
          {t.integ_status_expected}: {s.expected}. {t.integ_status_from}: {s.source}.
        </p>
      )}
      {s.state === "blocked" && (
        <div style={{ marginTop: 6 }}>
          <p className="field-err" style={{ margin: 0 }}>{s.reason}</p>
          <pre className="mono hint" style={{ whiteSpace: "pre-wrap", margin: "6px 0 0" }} data-testid={`integ-remediation-${id}`}>{s.remediation}</pre>
        </div>
      )}
      {failed.length > 0 && (
        <button type="button" className="btn-link hint" onClick={() => setOpen(o => !o)}>
          {open ? t.integ_status_hide : t.integ_status_details.replace("{n}", failed.length)}
        </button>
      )}
      {open && (
        <ul className="hint" style={{ margin: "4px 0 0", paddingLeft: 18 }}>
          {failed.map((c, i) => <li key={i}><span className="mono">{c.name}</span> — {c.detail}</li>)}
        </ul>
      )}
    </div>
  );
}

function PreflightFindings({ d, id, lang }) {
  const t = STR[lang];
  const [p, setP] = useState(null);
  const [err, setErr] = useState("");
  useEffect(() => {
    let alive = true;
    setP(null); setErr("");
    API.integrationPreflight(d.id, id)
      .then(r => { if (alive) setP(r); })
      .catch(e => { if (alive) setErr(e.message); });
    return () => { alive = false; };
  }, [d.id, id]);

  if (err) return <p className="hint" data-testid="preflight" data-state="error">{t.integ_preflight_err}: {err}</p>;
  if (!p) return <p className="hint" data-testid="preflight" data-state="checking">{t.integ_preflight_checking}</p>;
  if (!p.findings.length) return <p className="hint" data-testid="preflight" data-state="clear">{t.integ_preflight_clear}</p>;
  return (
    <div data-testid="preflight" data-state={p.findings.some(f => f.blocker) ? "blocked" : "notice"}>
      {p.findings.map((f, i) => (
        <Banner key={i} tone={f.blocker ? "danger" : "warn"} style={{ marginBottom: 8 }}
          title={f.blocker ? t.integ_preflight_blocker : t.integ_preflight_notice}>
          <p style={{ margin: "0 0 6px" }}>{f.reason}</p>
          <pre className="mono" style={{ whiteSpace: "pre-wrap", margin: 0 }}>{f.remediation}</pre>
        </Banner>
      ))}
    </div>
  );
}

export { IntegrationStatus, PreflightFindings };
