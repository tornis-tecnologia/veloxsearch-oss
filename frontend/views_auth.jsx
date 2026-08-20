// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Login / first-run Setup screens (issue #28)

   Rendered by app.jsx when /api/auth_state reports:
     first_run        → mode="setup"  (create the admin account)
     !authenticated   → mode="login"

   Setup mirrors auth::complete_setup (src/auth.rs):
     username trimmed, 1–64 chars; password ≥ 8 chars; confirm must match.
   On success the cookie is set server-side; we re-probe boot via onAuthed().
   ============================================================ */
import { useState } from "react";
import { API } from "./api.jsx";
import { STR } from "./i18n.jsx";
import { Icon, Logo, Field, Btn } from "./ui.jsx";

function AuthPrefs({ lang, setLang, theme, setTheme }) {
  return (
    <div className="prefs" style={{ position: "absolute", top: 16, right: 16, display: "flex", gap: 8, alignItems: "center" }}>
      <button className="iconbtn" title={theme === "dark" ? "Light" : "Dark"} onClick={() => setTheme(t => t === "dark" ? "light" : "dark")}>
        <Icon name={theme === "dark" ? "sun" : "moon"} size={16} />
      </button>
      <button className="iconbtn" style={{ width: "auto", padding: "0 12px", fontFamily: "var(--font-mono)", fontSize: 13 }}
        title="language" onClick={() => setLang(lang === "pt" ? "en" : "pt")}>
        {lang === "pt" ? "EN" : "PT"}
      </button>
    </div>
  );
}

function AuthView({ mode, lang, setLang, theme, setTheme, onAuthed }) {
  const isSetup = mode === "setup";
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  // One dictionary for the whole app. This screen used to carry its own copy of
  // every string, which drifts from STR the moment either side is edited.
  const t = STR[lang] || STR.pt;

  // Live field validation on setup: the rules are known before the click, so
  // showing them only after a failed submit is a self-inflicted round trip.
  const userErr = isSetup && username.trim() && username.trim().length > 64 ? t.auth_e_user : "";
  const passErr = isSetup && password && password.length < 8 ? t.auth_e_pass : "";
  const matchErr = isSetup && confirm && password !== confirm ? t.auth_e_match : "";
  const canSubmit = !busy && (!isSetup
    || (username.trim() && password.length >= 8 && password === confirm));

  async function submit(e) {
    e.preventDefault();
    setErr("");
    if (isSetup) {
      const u = username.trim();
      if (!u || u.length > 64) { setErr(t.auth_e_user); return; }
      if (password.length < 8) { setErr(t.auth_e_pass); return; }
      if (password !== confirm) { setErr(t.auth_e_match); return; }
    }
    setBusy(true);
    try {
      if (isSetup) await API.setupAdmin(username.trim(), password, confirm);
      else await API.login(username, password);
      onAuthed();
    } catch (ex) {
      setErr(ex.message || t.auth_e_generic);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="app">
      <AuthPrefs lang={lang} setLang={setLang} theme={theme} setTheme={setTheme} />
      <main className="page">
        <div style={{ maxWidth: 380, margin: "84px auto 0" }}>
          <div style={{ display: "flex", alignItems: "center", gap: 10, justifyContent: "center", marginBottom: 8 }}>
            <span style={{ color: "var(--accent)" }}><Logo size={26} /></span>
            <h1 className="page-title" style={{ margin: 0 }}>VeloxSearch</h1>
          </div>
          <p className="lead" style={{ textAlign: "center" }}>{isSetup ? t.setup_sub : t.login_sub}</p>

          <form className="card pad" onSubmit={submit} style={{ marginTop: 16 }}>
            <Field label={t.auth_user} htmlFor="auth-user" error={userErr}>
              <input id="auth-user" className={`input${userErr ? " invalid" : ""}`} type="text"
                name="username" autoComplete="username"
                value={username} onChange={e => setUsername(e.target.value)} required autoFocus />
            </Field>
            <Field label={t.auth_pass} hint={isSetup ? t.auth_hint8 : undefined}
              htmlFor="auth-pass" error={passErr}>
              <input id="auth-pass" className={`input${passErr ? " invalid" : ""}`} type="password"
                name="password"
                autoComplete={isSetup ? "new-password" : "current-password"}
                value={password} onChange={e => setPassword(e.target.value)} required
                minLength={isSetup ? 8 : undefined} />
            </Field>
            {isSetup && (
              <Field label={t.auth_confirm} htmlFor="auth-confirm" error={matchErr}>
                <input id="auth-confirm" className={`input${matchErr ? " invalid" : ""}`} type="password"
                  name="confirm" autoComplete="new-password"
                  value={confirm} onChange={e => setConfirm(e.target.value)} required minLength={8} />
              </Field>
            )}
            {/* role="alert" so a screen reader announces a rejected login
                instead of leaving the user waiting for nothing. */}
            {err && (
              <p className="err" role="alert"
                style={{ color: "var(--danger)", fontFamily: "var(--font-mono)", fontSize: 13, margin: "0 0 12px" }}>
                {err}
              </p>
            )}
            <Btn variant="primary" type="submit" disabled={!canSubmit} style={{ width: "100%", justifyContent: "center" }}>
              {busy ? t.auth_working : (isSetup ? t.auth_create_btn : t.auth_login_btn)}
            </Btn>
          </form>
        </div>
      </main>
    </div>
  );
}

export { AuthView };
