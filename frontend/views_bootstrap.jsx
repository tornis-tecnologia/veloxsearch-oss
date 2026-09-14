// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Bootstrap / conformity screen (issue #28)

   Shown after login when the cluster isn't ready for VeloxSearch
   deployments (bootstrap_status.ready === false). It:
     - renders the REQUIREMENTS.md report (ADR-026) as ul.req-list
     - if the cluster is unsupported (a hard fail), refuses informatively
       and does NOT start an install (no ul.setup-list)
     - otherwise installs the missing prerequisites (cert-manager /
       operator) via /api/bootstrap_ensure and polls until ready, then
       hands control back to the main app via onReady().
   ============================================================ */
import { useState, useEffect, useRef } from "react";
import { API } from "./api.jsx";
import { STR } from "./i18n.jsx";
import { Icon, Btn } from "./ui.jsx";

function reqGlyph(s) { return s === "pass" ? "✓" : s === "warn" ? "⚠" : "✗"; }
function reqColor(s) { return s === "pass" ? "var(--accent)" : s === "warn" ? "var(--warn)" : "var(--danger)"; }
// The glyph is decorative; the word is the signal. A colored ✓ alone is
// invisible to a screen reader and ambiguous in grayscale.
function reqWord(s, t) { return s === "pass" ? t.boot_req_pass : s === "warn" ? t.boot_req_warn : t.boot_req_fail; }

// ADR-057: a running operator that differs from the one this release vendors
// is reported, never changed. The text is built here from the structured
// fields (ADR-019 — the backend's R9 detail is only the English fallback), and
// exported so the main shell can show it too: a conformant cluster never sees
// this screen, and drift is exactly what an upgraded, conformant cluster has.
function OperatorDriftNotice({ status, t, style }) {
  if (!status || !status.operator_drift) return null;
  return (
    <div className="card pad" role="status" data-testid="operator-drift"
      style={{ borderColor: "var(--warn-soft)", ...style }}>
      <div style={{ color: "var(--warn)", fontWeight: 600, marginBottom: 4 }}>
        <span aria-hidden="true">⚠ </span>{t.boot_drift_title}
      </div>
      <div style={{ color: "var(--text-2)", fontSize: 13 }}>
        {t.boot_drift
          .replace("{0}", status.operator_image_running || "?")
          .replace("{1}", status.operator_image_vendored || "?")}
      </div>
    </div>
  );
}

function BootPrefs({ lang, setLang, theme, setTheme }) {
  return (
    <div className="prefs" style={{ position: "absolute", top: 16, right: 16, display: "flex", gap: 8, alignItems: "center" }}>
      <button className="iconbtn" title={theme === "dark" ? "Light" : "Dark"} onClick={() => setTheme(t => t === "dark" ? "light" : "dark")}>
        <Icon name={theme === "dark" ? "sun" : "moon"} size={16} />
      </button>
      <button className="iconbtn" style={{ width: "auto", padding: "0 12px", fontFamily: "var(--font-mono)", fontSize: 13 }}
        title="language" onClick={() => setLang(lang === "pt" ? "en" : lang === "en" ? "es" : "pt")}>
        {lang === "pt" ? "EN" : lang === "en" ? "ES" : "PT"}
      </button>
    </div>
  );
}

function StepRow({ label, installed, ready, t }) {
  const state = ready ? t.boot_st_ready : installed ? t.boot_st_installing : t.boot_st_pending;
  const glyph = ready ? "✓" : "·";
  const color = ready ? "var(--accent)" : "var(--text-3)";
  return (
    <li style={{ display: "flex", alignItems: "center", gap: 10, padding: "8px 0" }}>
      <span aria-hidden="true" style={{ color, width: 16, textAlign: "center", fontFamily: "var(--font-mono)" }}>{glyph}</span>
      <span style={{ flex: 1 }}>{label}</span>
      <span style={{ color: ready ? "var(--accent)" : "var(--text-3)", fontFamily: "var(--font-mono)", fontSize: 12.5 }}>{state}</span>
    </li>
  );
}

function fmtElapsed(ms) {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function BootstrapView({ lang, setLang, theme, setTheme, onReady }) {
  const [status, setStatus] = useState(null);
  const [err, setErr] = useState("");
  const ensured = useRef(false);

  // One dictionary for the whole app (this screen carried its own copy).
  const t = STR[lang] || STR.pt;
  const startRef = useRef(Date.now());
  const [elapsed, setElapsed] = useState(0);
  const [retrying, setRetrying] = useState(false);

  useEffect(() => {
    const h = setInterval(() => setElapsed(Date.now() - startRef.current), 1000);
    return () => clearInterval(h);
  }, []);

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const s = await API.bootstrapStatus();
        if (!alive) return;
        setStatus(s);
        setErr(s.error || "");
        if (s.ready) { onReady(); return; }
        // Supported but not ready → install the missing pieces, once.
        if (!s.unsupported && !ensured.current && !s.installing) {
          ensured.current = true;
          API.bootstrapEnsure()
            .then(s2 => { if (alive && s2) { setStatus(s2); if (s2.ready) onReady(); } })
            .catch(e => { if (alive) setErr(e.message); });
        }
      } catch (e) {
        if (alive) setErr(e.message);
      }
    };
    tick();
    const h = setInterval(tick, 3000);
    return () => { alive = false; clearInterval(h); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The install runs once (`ensured`), so a failure used to leave the screen
  // polling forever with no way to act. This re-arms it.
  async function retry() {
    setRetrying(true);
    setErr("");
    startRef.current = Date.now();
    setElapsed(0);
    try {
      const s = await API.bootstrapEnsure();
      if (s) { setStatus(s); if (s.ready) onReady(); }
    } catch (e) {
      setErr(e.message);
    } finally {
      setRetrying(false);
    }
  }

  const reqs = status?.requirements || [];
  const unsupported = !!status?.unsupported;
  const installing = !!status?.installing;

  return (
    <div className="app">
      <BootPrefs lang={lang} setLang={setLang} theme={theme} setTheme={setTheme} />
      <main className="page">
        <div style={{ maxWidth: 560, margin: "60px auto 0" }}>
          <h1 className="page-title">{t.boot_title}</h1>
          <p className="lead">{t.boot_sub}</p>

          {!status ? (
            <p className="hint">{t.boot_checking}</p>
          ) : (
            <div className="card pad" style={{ marginTop: 8 }}>
              <h3 className="section-title">{t.boot_req_title}</h3>
              <ul className="req-list" style={{ listStyle: "none", padding: 0, margin: "0 0 8px" }}>
                {reqs.map(r => (
                  <li key={r.id} style={{ display: "flex", gap: 10, padding: "7px 0", alignItems: "flex-start" }}>
                    <span aria-hidden="true" style={{ color: reqColor(r.status), width: 16, textAlign: "center", flexShrink: 0, fontFamily: "var(--font-mono)" }}>{reqGlyph(r.status)}</span>
                    <span style={{ flex: 1 }}>
                      <b style={{ fontFamily: "var(--font-mono)", fontWeight: 600 }}>{r.id}</b>
                      {/* The status as a word, not only as a colored glyph. */}
                      <span style={{ color: reqColor(r.status), fontSize: 12, fontFamily: "var(--font-mono)" }}> [{reqWord(r.status, t)}]</span>
                      <span style={{ color: "var(--text-2)" }}> — {r.id === "r9" && r.status === "warn" && status.operator_drift
                        ? t.boot_drift.replace("{0}", status.operator_image_running || "?").replace("{1}", status.operator_image_vendored || "?")
                        : r.detail}</span>
                    </span>
                  </li>
                ))}
              </ul>

              {unsupported ? (
                <p style={{ color: "var(--danger)", fontSize: 14, marginTop: 8 }} role="alert">{t.boot_unsupported}</p>
              ) : (
                <>
                  <div className="hr" />
                  <h3 className="section-title">{t.boot_inst_title}</h3>
                  <ul className="setup-list" style={{ listStyle: "none", padding: 0, margin: 0 }}>
                    <StepRow label="cert-manager" t={t} installed={status.cert_manager_installed} ready={status.cert_manager_ready} />
                    <StepRow label="OpenSearch operator" t={t} installed={status.operator_installed} ready={status.operator_ready} />
                  </ul>
                  {/* Installing two operators takes minutes; a screen with no
                      clock reads as a hang. */}
                  <p className="hint" style={{ marginTop: 10 }}>
                    {/* "wait:<component>" = installed but not Ready; bootstrap
                        waits on it and applies nothing (ADR-057). */}
                    {installing && (status.installing.startsWith("wait:")
                      ? <>{t.boot_waiting} <span style={{ color: "var(--text-2)" }}>{status.installing.slice(5)}</span> · </>
                      : <>{t.boot_installing} <span style={{ color: "var(--text-2)" }}>{status.installing}</span> · </>)}
                    <span className="tnum">{t.boot_elapsed.replace("{0}", fmtElapsed(elapsed))}</span>
                  </p>
                </>
              )}

              {err && (
                <div style={{ marginTop: 12 }}>
                  <p className="err" role="alert" style={{ color: "var(--danger)", fontFamily: "var(--font-mono)", fontSize: 13, margin: 0 }}>{err}</p>
                  {!unsupported && (
                    <Btn variant="outline" icon="clock" disabled={retrying} onClick={retry} style={{ marginTop: 10 }}>
                      {t.boot_retry}
                    </Btn>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </main>
    </div>
  );
}

export { BootstrapView, OperatorDriftNotice };
