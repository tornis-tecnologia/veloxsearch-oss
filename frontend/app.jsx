// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   App root — boot/auth gate, API + SSE state, routing, tweaks (#27)

   Boot sequence keyed off /api/auth_state (issue #28 fills the auth +
   bootstrap screens; this root only delegates to them when present):
     auth_state.first_run        → "setup"      (AuthView mode=setup)
     !auth_state.authenticated   → "login"      (AuthView mode=login)
     authenticated + !ready      → "bootstrap"  (BootstrapView)
     authenticated + ready       → main app

   Deployments come from the SSE stream (/api/events); localStorage holds
   ONLY UI prefs (theme, lang). No mock data, no persisted server state.
   ============================================================ */
import { useState, useEffect, useRef, useCallback } from "react";
import { STR } from "./i18n.jsx";
import { API, adaptDeployment } from "./api.jsx";
import { Logo, Icon, Toast } from "./ui.jsx";
import { AuthView } from "./views_auth.jsx";
import { BootstrapView } from "./views_bootstrap.jsx";
import { StatusView } from "./views_status.jsx";
import { CreateView } from "./views_create.jsx";
import { CapacityView } from "./views_capacity.jsx";
import { SettingsView } from "./views_settings.jsx";
import { DeploymentView } from "./views_deployment.jsx";
import { useTweaks, TweaksPanel, TweakSection, TweakColor, TweakRadio, TweakSelect } from "./tweaks-panel.jsx";

const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
  "accent": "#10b981",
  "density": "regular",
  "uiFont": "IBM Plex"
}/*EDITMODE-END*/;

const ACCENT_HUE = {
  "#10b981": 162, // green
  "#3b82f6": 254, // blue
  "#8b5cf6": 292, // violet
  "#06b6d4": 220, // cyan
  "#f97316": 55,  // orange
};

const FONT_STACKS = {
  "IBM Plex": { sans: '"IBM Plex Sans", system-ui, sans-serif', mono: '"IBM Plex Mono", ui-monospace, monospace' },
  "Geist": { sans: '"Space Grotesk", system-ui, sans-serif', mono: '"JetBrains Mono", ui-monospace, monospace' },
  "Mono only": { sans: '"JetBrains Mono", ui-monospace, monospace', mono: '"JetBrains Mono", ui-monospace, monospace' },
};

function lsGet(k) { try { return localStorage.getItem(k); } catch (e) { return null; } }
function lsSet(k, v) { try { localStorage.setItem(k, v); } catch (e) {} }

// Minimal centered shell for the loading / pre-auth states.
function BootShell({ children }) {
  return (
    <div className="app">
      <main className="page">
        <div style={{ maxWidth: 440, margin: "90px auto 0", textAlign: "center" }}>{children}</div>
      </main>
    </div>
  );
}

function App() {
  const [t, setTweak] = useTweaks(TWEAK_DEFAULTS);

  const [lang, setLang] = useState(lsGet("velox-lang") || "pt");
  const [theme, setTheme] = useState(lsGet("velox-theme") || "dark");
  const [boot, setBoot] = useState("loading"); // loading | setup | login | bootstrap | ready
  const [route, setRoute] = useState({ name: "status" });
  const [deployments, setDeployments] = useState([]);
  const [loaded, setLoaded] = useState(false); // a deployment snapshot has arrived
  // Host-cluster nodes, fetched once (#26/#32 UI honesty): feeds the
  // single-copy warning (fewer than 3 nodes → one Longhorn replica) and the
  // known kernel-incompatibility warning in the wizard. Empty = unknown →
  // both warnings stay silent rather than guessing.
  const [hostNodes, setHostNodes] = useState([]);
  const [sseDead, setSseDead] = useState(false);
  const [toast, setToast] = useState({ msg: "", show: false });
  const toastTimer = useRef(null);

  const tr = STR[lang];

  // ── persist UI prefs only ──────────────────────────────────────
  useEffect(() => { lsSet("velox-lang", lang); }, [lang]);
  useEffect(() => { lsSet("velox-theme", theme); }, [theme]);

  // ── apply tweaks + theme to the document ───────────────────────
  useEffect(() => {
    const r = document.documentElement;
    r.setAttribute("data-theme", theme);
    r.style.setProperty("--accent-h", ACCENT_HUE[t.accent] ?? 162);
    r.style.setProperty("--density", t.density === "compact" ? 0.8 : t.density === "comfy" ? 1.2 : 1);
    const f = FONT_STACKS[t.uiFont] || FONT_STACKS["IBM Plex"];
    r.style.setProperty("--font-sans", f.sans);
    r.style.setProperty("--font-mono", f.mono);
  }, [t.accent, t.density, t.uiFont, theme]);

  // ── boot probe: auth_state → bootstrap gate → ready ────────────
  const probeBoot = useCallback(async () => {
    setBoot("loading");
    try {
      const a = await API.authState();
      if (a && a.first_run) { setBoot("setup"); return; }
      if (!a || !a.authenticated) { setBoot("login"); return; }
      // Authenticated → conformity gate (ADR-014). Probe errors fall through
      // to the main UI, which surfaces its own problems.
      try {
        const bs = await API.bootstrapStatus();
        setBoot(bs && bs.ready ? "ready" : "bootstrap");
      } catch (e) {
        setBoot("ready");
      }
    } catch (e) {
      // auth_state unreachable: show login rather than hammer gated endpoints.
      setBoot("login");
    }
  }, []);

  useEffect(() => { probeBoot(); }, [probeBoot]);

  // ── SSE deployment stream (only once authenticated + ready) ────
  useEffect(() => {
    if (boot !== "ready") return;
    let alive = true;

    // One eager fetch so the list isn't empty before the first SSE frame.
    API.listDeployments()
      .then(list => { if (alive && Array.isArray(list)) { setDeployments(list.map(adaptDeployment)); setLoaded(true); } })
      .catch(() => {});

    API.clusterCapacity()
      .then(c => { if (alive && Array.isArray(c?.nodes)) setHostNodes(c.nodes); })
      .catch(() => {});

    let es;
    try {
      es = new EventSource(API.EVENTS_URL);
      es.onmessage = (ev) => {
        try {
          const list = JSON.parse(ev.data);
          if (alive && Array.isArray(list)) {
            setDeployments(list.map(adaptDeployment));
            setLoaded(true);
            setSseDead(false);
          }
        } catch (e) { /* comment/keepalive frame */ }
      };
      es.onerror = () => { setSseDead(true); }; // EventSource auto-reconnects
    } catch (e) {
      setSseDead(true);
    }
    return () => { alive = false; if (es) es.close(); };
  }, [boot]);

  // ── fallback poll: advances only while the SSE stream is dead ──
  useEffect(() => {
    if (boot !== "ready" || !sseDead) return;
    let alive = true;
    const h = setInterval(() => {
      API.listDeployments()
        .then(list => { if (alive && Array.isArray(list)) { setDeployments(list.map(adaptDeployment)); setLoaded(true); } })
        .catch(() => {});
    }, 5000);
    return () => { alive = false; clearInterval(h); };
  }, [boot, sseDead]);

  // ── toast ──────────────────────────────────────────────────────
  function showToast(msg) {
    setToast({ msg, show: true });
    clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(s => ({ ...s, show: false })), 2200);
  }

  function go(r) { setRoute(r); window.scrollTo({ top: 0 }); }

  // ── API-backed handlers ────────────────────────────────────────
  async function createCluster({ name, purpose, size, sources, extra, memory, disk, version, snapshot }) {
    const monitors = Object.keys(sources || {}).filter(k => sources[k]).join(",");
    try {
      // Heads-up signal (ADR-031): a node-local/absent default means this create
      // auto-installs Longhorn server-side. Note it BEFORE (after, storage reads
      // durable) so the success toast can report the install — install + inform,
      // the user is never surprised. Best-effort; failure just shows the plain toast.
      let installedLonghorn = false;
      try { const ss = await API.storageStatus(); installedLonghorn = !!(ss && ss.needs_longhorn); }
      catch (e) { /* no cluster / pre-auth: plain toast */ }
      const finalName = await API.createCluster({
        // nodes stays "" — the 3-node count is a backend invariant (ADR-016),
        // never client-overridable. memory/disk carry the custom-size profile.
        name: name || "logs", size, purpose,
        nodes: "", memory: memory || "", disk: disk || "", config: extra || "", monitors,
        // OpenSearch version chosen in the wizard (ADR-048 rev. 2). Empty =
        // the backend's default. Only the create path carries this — a later
        // save never does, so an edit cannot move a live deployment's version.
        version: version || "",
        // Optional snapshot repository (ADR-049), null when the step was
        // skipped. Same rule as `version`: create-only, because the slice has
        // its own write path and a save must never touch it.
        snapshot: snapshot || null,
      });
      showToast(installedLonghorn ? tr.storage_installed_toast : (tr.created_h + " ✓"));
      go({ name: "deployment", id: finalName, tab: "overview" });
    } catch (e) {
      showToast(e.message);
    }
  }

  async function saveEdit(payload) {
    try { await API.saveCluster(payload); showToast(tr.saved); }
    catch (e) { showToast(e.message); }
  }

  // ADR-053. Same contract as toggleIntegration: toast AND rethrow, so the
  // panel can pin the reason instead of losing it with the toast.
  async function toggleOtelStack(name, install, deleteIndices = false) {
    try {
      if (install) await API.installOtelStack(name);
      else await API.uninstallOtelStack(name, deleteIndices);
      showToast(install ? tr.otel_install_toast : tr.otel_uninstall_toast);
    } catch (e) {
      showToast(e.message);
      throw e;
    }
  }

  // Generated, never chosen: the handler hands the new value straight back to
  // the panel, which is the only place it is ever shown.
  async function resetPass(name) {
    try {
      const c = await API.resetAdminPasswordRandom(name);
      showToast(tr.pass_reset);
      return c;
    } catch (e) {
      showToast(e.message);
      throw e;
    }
  }

  async function deleteCluster(name) {
    try { await API.deleteCluster(name); showToast(lang === "pt" ? "Cluster excluído" : lang === "es" ? "Deployment eliminado" : "Deployment deleted"); }
    catch (e) { showToast(e.message); }
    go({ name: "status" });
  }

  async function doLogout() {
    try { await API.logout(); } catch (e) {}
    setDeployments([]);
    setLoaded(false);
    probeBoot();
  }

  // ── deployment-not-found: bounce to status once data has loaded ─
  const current = route.name === "deployment" ? deployments.find(d => d.id === route.id) : null;
  useEffect(() => {
    if (route.name !== "deployment" || current || !loaded) return;
    const h = setTimeout(() => { if (!deployments.find(d => d.id === route.id)) go({ name: "status" }); }, 6000);
    return () => clearTimeout(h);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [route.name, route.id, current, loaded]);

  // ── pre-ready screens (login / setup / bootstrap) ──────────────
  if (boot === "loading") {
    return <BootShell><p className="hint">{lang === "pt" ? "Carregando…" : lang === "es" ? "Cargando…" : "Loading…"}</p></BootShell>;
  }
  if (boot === "login" || boot === "setup") {
    return <AuthView mode={boot} lang={lang} setLang={setLang} theme={theme} setTheme={setTheme} onAuthed={probeBoot} />;
  }
  if (boot === "bootstrap") {
    return <BootstrapView lang={lang} setLang={setLang} theme={theme} setTheme={setTheme} onReady={() => setBoot("ready")} />;
  }

  // ── main app ───────────────────────────────────────────────────
  const navItems = [
    { key: "status", label: tr.nav_status },
    { key: "create", label: tr.nav_create },
    { key: "capacity", label: tr.nav_capacity },
    { key: "settings", label: tr.nav_settings },
  ];
  const activeNav = route.name === "deployment" ? "status" : route.name;

  return (
    <div className="app">
      <header className="topbar">
        <a href="#" className="brand" onClick={e => { e.preventDefault(); go({ name: "status" }); }}>
          <span className="logo"><Logo size={20} /></span>
          VeloxSearch
        </a>
        {current && (
          <div className="crumb">
            <span className="sep">/</span>
            <span className="cur">{current.id}</span>
          </div>
        )}
        <span className="spacer" />
        {/* The live stream is down and the app is on its 5s fallback poll. The
            user was never told; the list just went quiet. */}
        {sseDead && (
          <span className="badge" style={{ color: "var(--warn)", borderColor: "var(--warn-soft)", textTransform: "none" }}
            title={tr.conn_lost_tip}>
            <Icon name="clock" size={12} />{tr.conn_lost}
          </span>
        )}
        <div className="seg" data-testid="lang-toggle">
          <button aria-pressed={lang === "pt"} onClick={() => setLang("pt")}>PT</button>
          <button aria-pressed={lang === "en"} onClick={() => setLang("en")}>EN</button>
          <button aria-pressed={lang === "es"} onClick={() => setLang("es")}>ES</button>
        </div>
        <button className="iconbtn" data-testid="theme-toggle" title={theme === "dark" ? "Light" : "Dark"} onClick={() => setTheme(th => th === "dark" ? "light" : "dark")}>
          <Icon name={theme === "dark" ? "sun" : "moon"} size={16} />
        </button>
        <button className="btn-link" style={{ color: "var(--text-3)", marginLeft: 4 }} onClick={doLogout}>{tr.logout}</button>
      </header>

      <main className="page">
        {/* Global navigation stays put inside a deployment too: it used to
            vanish, leaving the breadcrumb as the only way back. */}
        <nav className="tabs">
          {navItems.map(n => (
            <button key={n.key} aria-selected={activeNav === n.key && !current}
              onClick={() => go({ name: n.key })}>{n.label}</button>
          ))}
        </nav>

        {route.name === "status" && (
          <StatusView deployments={deployments} lang={lang}
            onOpen={id => go({ name: "deployment", id, tab: "overview" })}
            // The "Upgrade vX" tag opens the deployment WITH the upgrade
            // dialog — the click starts the flow, but the irreversible step
            // still needs its confirmation (ADR-048).
            onUpgrade={id => go({ name: "deployment", id, tab: "overview", upgrade: true })}
            onCreate={() => go({ name: "create" })} />
        )}
        {route.name === "create" && (
          <CreateView lang={lang} hostNodes={hostNodes} onCreate={createCluster} onCancel={() => go({ name: "status" })} />
        )}
        {route.name === "capacity" && (
          <CapacityView lang={lang} />
        )}
        {route.name === "settings" && (
          <SettingsView lang={lang} onToast={showToast} />
        )}
        {route.name === "deployment" && (current
          ? <DeploymentView d={current} lang={lang} hostNodes={hostNodes} tab={route.tab || "overview"}
              openUpgrade={!!route.upgrade}
              onTab={tab => setRoute(r => ({ ...r, tab, upgrade: false }))}
              onToggleStack={toggleOtelStack}
              onSaveEdit={saveEdit}
              onResetPass={resetPass}
              onToast={showToast}
              onDelete={deleteCluster} />
          : <div className="view-enter"><p className="hint">{lang === "pt" ? "Carregando deployment…" : lang === "es" ? "Cargando deployment…" : "Loading deployment…"}</p></div>)}
      </main>

      <Toast msg={toast.msg} show={toast.show} />

      <TweaksPanel title="Tweaks">
        <TweakSection label={lang === "pt" ? "Aparência" : lang === "es" ? "Apariencia" : "Appearance"} />
        <TweakColor label={lang === "pt" ? "Cor de destaque" : lang === "es" ? "Color de acento" : "Accent"} value={t.accent}
          options={["#10b981", "#3b82f6", "#8b5cf6", "#06b6d4", "#f97316"]}
          onChange={v => setTweak("accent", v)} />
        <TweakRadio label={lang === "pt" ? "Densidade" : lang === "es" ? "Densidad" : "Density"} value={t.density}
          options={["compact", "regular", "comfy"]}
          onChange={v => setTweak("density", v)} />
        <TweakSelect label={lang === "pt" ? "Fonte" : lang === "es" ? "Fuente" : "Font"} value={t.uiFont}
          options={["IBM Plex", "Geist", "Mono only"]}
          onChange={v => setTweak("uiFont", v)} />
      </TweaksPanel>
    </div>
  );
}

export { App };
