// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   App root — boot/auth gate, API + SSE state, routing, the shell.

   Boot sequence keyed off /api/auth_state:
     auth_state.first_run        → "setup"      (AuthView mode=setup)
     !auth_state.authenticated   → "login"      (AuthView mode=login)
     authenticated + !ready      → "bootstrap"  (BootstrapView)
     authenticated + ready       → the shell

   The shell is a sidebar of PLACES (deployments, capacity, settings) and a
   top bar with the breadcrumb. Actions (create, upgrade, delete) are buttons
   on the screens they act on, never navigation. The account menu holds
   theme, language and sign-out; the build version lives in Settings › About.

   Every screen has a URL (router.jsx), so the browser's Back button and F5
   keep their meaning and a link in an alert opens the right screen.

   Deployments come from the SSE stream (/api/events); localStorage holds
   ONLY UI prefs (theme, lang, dismissed checklist). No mock data, no
   persisted server state.
   ============================================================ */
import { useState, useEffect, useRef, useCallback, lazy, Suspense } from "react";
import { STR } from "./i18n.jsx";
import { API, adaptDeployment } from "./api.jsx";
import { Logo, Icon, Toast, ErrorBoundary, Menu } from "./ui.jsx";
import { useRouter, nextAfterLogin } from "./router.jsx";
import { AuthView } from "./views_auth.jsx";
import { BootstrapView, OperatorDriftNotice, TenantIsolationNotice } from "./views_bootstrap.jsx";
import { StatusView } from "./views_status.jsx";
import { CreateView } from "./views_create.jsx";
import { CapacityView } from "./views_capacity.jsx";
import { SettingsView } from "./views_settings.jsx";
import { DeploymentView } from "./views_deployment.jsx";

// The design-tweaks panel is a prototyping tool: it only ships in dev.
const DevTweaks = import.meta.env.DEV ? lazy(() => import("./tweaks-dev.jsx")) : null;

const DOCS_URL = "https://get.veloxsearch.ai/docs/en";

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

// A sidebar entry: a real link (middle-click works) that the router handles.
function NavLink({ href, active, icon, label, count, onGo, testid }) {
  return (
    <a href={href} aria-current={active ? "page" : undefined} data-testid={testid}
      onClick={e => { if (e.metaKey || e.ctrlKey || e.button === 1) return; e.preventDefault(); onGo(href); }}>
      <span className="ic"><Icon name={icon} size={16} /></span>
      <span>{label}</span>
      {count != null && <span className="cnt">{count}</span>}
    </a>
  );
}

function App() {
  const [route, go] = useRouter();
  const [lang, setLang] = useState(lsGet("velox-lang") || "pt");
  const [theme, setTheme] = useState(lsGet("velox-theme") || "dark");
  const [boot, setBoot] = useState("loading"); // loading | setup | login | bootstrap | ready
  const [deployments, setDeployments] = useState([]);
  const [loaded, setLoaded] = useState(false); // a deployment snapshot has arrived
  // Host-cluster nodes, fetched once (#26/#32 UI honesty): feeds the
  // single-copy warning (fewer than 3 nodes → one Longhorn replica) and the
  // known kernel-incompatibility warning in the wizard. Empty = unknown →
  // both warnings stay silent rather than guessing.
  const [hostNodes, setHostNodes] = useState([]);
  const [capacity, setCapacity] = useState(null);
  const [sseDead, setSseDead] = useState(false);
  const [bootStatus, setBootStatus] = useState(null);
  const [username, setUsername] = useState("");
  const [isTenant, setIsTenant] = useState(false);
  const [buildInfo, setBuildInfo] = useState(null);
  const [toast, setToast] = useState({ msg: "", show: false, kind: "ok" });
  const [navOpen, setNavOpen] = useState(false);
  const toastTimer = useRef(null);

  const tr = STR[lang];

  // ── persist UI prefs only ──────────────────────────────────────
  useEffect(() => { lsSet("velox-lang", lang); }, [lang]);
  useEffect(() => { lsSet("velox-theme", theme); }, [theme]);
  useEffect(() => { document.documentElement.setAttribute("data-theme", theme); }, [theme]);

  // ── boot probe: auth_state → bootstrap gate → ready ────────────
  const probeBoot = useCallback(async () => {
    setBoot("loading");
    try {
      const a = await API.authState();
      if (a && a.first_run) { setBoot("setup"); return; }
      if (!a || !a.authenticated) { setBoot("login"); return; }
      setIsTenant(!!a.tenant);
      setUsername(a.username || "");
      // Authenticated → conformity gate (ADR-014). Probe errors fall through
      // to the main UI, which surfaces its own problems.
      try {
        const bs = await API.bootstrapStatus();
        setBootStatus(bs);
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

  // The URL the server redirected from (`/login?next=…`) is honoured once the
  // session exists; /login and /setup are never screens of their own.
  useEffect(() => {
    if (boot !== "ready") return;
    if (route.name === "auth") go(nextAfterLogin(), { replace: true });
  }, [boot, route.name, go]);

  // ── the tab title follows the screen ───────────────────────────
  useEffect(() => {
    const parts = [];
    if (route.name === "deployment") {
      parts.push(route.id);
      parts.push(tr[`tab_${route.tab}`] || tr.tab_overview);
    } else if (route.name === "create") parts.push(tr.nav_create);
    else if (route.name === "capacity") parts.push(tr.nav_capacity);
    else if (route.name === "settings") parts.push(tr.nav_settings);
    else parts.push(tr.nav_deployments);
    document.title = `${parts.join(" · ")} · VeloxSearch`;
  }, [route, tr]);

  // Close the mobile drawer on navigation.
  useEffect(() => { setNavOpen(false); }, [route]);

  // ── SSE deployment stream (only once authenticated + ready) ────
  useEffect(() => {
    if (boot !== "ready") return;
    let alive = true;

    API.listDeployments()
      .then(list => { if (alive && Array.isArray(list)) { setDeployments(list.map(adaptDeployment)); setLoaded(true); } })
      .catch(() => {});

    API.clusterCapacity()
      .then(c => { if (alive && c) { setCapacity(c); if (Array.isArray(c.nodes)) setHostNodes(c.nodes); } })
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

  // ── build identity: fixed for the life of this pod, so fetch once ──
  useEffect(() => {
    if (boot !== "ready" || isTenant) return;
    let alive = true;
    API.buildInfo()
      .then(b => { if (alive && b) setBuildInfo(b); })
      .catch(() => {});
    return () => { alive = false; };
  }, [boot, isTenant]);

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
  // `ok` is true (success), false (error) or a kind name. A success fades on
  // its own; an error stays until closed, because the reason a save failed is
  // not read in two seconds — and never wears a check mark (#141).
  const showToast = useCallback((msg, ok = true) => {
    const kind = typeof ok === "string" ? ok : ok ? "ok" : "error";
    setToast({ msg, show: true, kind });
    clearTimeout(toastTimer.current);
    if (kind !== "error") {
      toastTimer.current = setTimeout(() => setToast(s => ({ ...s, show: false })), kind === "ok" ? 2600 : 5000);
    }
  }, []);
  const closeToast = () => setToast(s => ({ ...s, show: false }));

  // ── API-backed handlers ────────────────────────────────────────
  // Toast AND rethrow (same contract as toggleOtelStack): the wizard keeps its
  // submit button disabled until this settles and must re-arm it on failure.
  async function createCluster({ name, purpose, size, sources, extra, memory, disk, version, snapshot, retentionDays }) {
    const monitors = Object.keys(sources || {}).filter(k => sources[k]).join(",");
    try {
      // Heads-up signal (ADR-031): a node-local/absent default means this create
      // auto-installs Longhorn server-side. Note it BEFORE so the success toast
      // can report the install. Best-effort; failure just shows the plain toast.
      let installedLonghorn = false;
      try { const ss = await API.storageStatus(); installedLonghorn = !!(ss && ss.needs_longhorn); }
      catch (e) { /* no cluster / pre-auth: plain toast */ }
      const finalName = await API.createCluster({
        // nodes stays "" — the 3-node count is a backend invariant (ADR-016).
        name: name || "logs", size, purpose,
        nodes: "", memory: memory || "", disk: disk || "", config: extra || "", monitors,
        // Create-only: a later save never moves a live deployment's version
        // (ADR-048 rev. 2) nor touches its snapshot slice (ADR-049).
        version: version || "",
        snapshot: snapshot || null,
        // ADR-062: null = the installation default for the purpose.
        retention_days: retentionDays || null,
      });
      showToast(installedLonghorn ? tr.storage_installed_toast : tr.created_h);
      go({ name: "deployment", id: finalName, tab: "overview" });
    } catch (e) {
      showToast(e.message, false);
      throw e;
    }
  }

  async function saveEdit(payload) {
    try { await API.saveCluster(payload); showToast(tr.saved); }
    catch (e) { showToast(e.message, false); throw e; }
  }

  // ADR-053. Toast AND rethrow, so the panel can pin the reason.
  async function toggleOtelStack(name, install, deleteIndices = false) {
    try {
      if (install) await API.installOtelStack(name);
      else await API.uninstallOtelStack(name, deleteIndices);
      showToast(install ? tr.otel_install_toast : tr.otel_uninstall_toast);
    } catch (e) {
      showToast(e.message, false);
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
      // 409: refused before anything changed — the deployment is busy (#115).
      showToast(e.status === 409 ? tr.sec_reset_busy : e.message, false);
      throw e;
    }
  }

  async function deleteCluster(name) {
    try { await API.deleteCluster(name); showToast(tr.dep_deleted); }
    catch (e) { showToast(e.message, false); }
    go({ name: "deployments" });
  }

  async function doLogout() {
    try { await API.logout(); } catch (e) {}
    setDeployments([]);
    setLoaded(false);
    setBuildInfo(null); // the next session may be a tenant's
    go("/login", { replace: true });
    probeBoot();
  }

  // ── deployment-not-found: bounce to the list once data has loaded ─
  const current = route.name === "deployment" ? deployments.find(d => d.id === route.id) : null;
  useEffect(() => {
    if (route.name !== "deployment" || current || !loaded) return;
    const h = setTimeout(() => { if (!deployments.find(d => d.id === route.id)) go({ name: "deployments" }, { replace: true }); }, 6000);
    return () => clearTimeout(h);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [route.name, route.id, current, loaded]);

  // ── pre-ready screens (login / setup / bootstrap) ──────────────
  if (boot === "loading") {
    return <BootShell><p className="hint">{tr.loading}</p></BootShell>;
  }
  if (boot === "login" || boot === "setup") {
    return <AuthView mode={boot} lang={lang} setLang={setLang} theme={theme} setTheme={setTheme} onAuthed={probeBoot} />;
  }
  if (boot === "bootstrap") {
    return <BootstrapView lang={lang} setLang={setLang} theme={theme} setTheme={setTheme} onReady={() => setBoot("ready")} />;
  }

  // ── main shell ─────────────────────────────────────────────────
  const section = route.name === "deployment" || route.name === "create" ? "deployments" : route.name;
  const initial = (username || "?").slice(0, 1).toUpperCase();
  const accountItems = [
    { label: tr.menu_theme, icon: theme === "dark" ? "sun" : "moon", right: theme === "dark" ? tr.theme_dark : tr.theme_light,
      title: theme === "dark" ? tr.theme_to_light : tr.theme_to_dark,
      onClick: () => setTheme(th => th === "dark" ? "light" : "dark"), testid: "theme-toggle" },
    { label: tr.menu_lang, icon: "globe", right: lang.toUpperCase(),
      onClick: () => setLang(l => l === "pt" ? "en" : l === "en" ? "es" : "pt"), testid: "lang-toggle" },
    { sep: true },
    { label: tr.logout, icon: "logout", onClick: doLogout, testid: "logout" },
  ];

  const crumb = route.name === "deployment"
    ? (<div className="crumb">
        <button type="button" className="lnk" onClick={() => go({ name: "deployments" })}>{tr.nav_deployments}</button>
        <span className="sep">/</span>
        <span className="cur">{route.id}</span>
      </div>)
    : route.name === "create"
      ? (<div className="crumb">
          <button type="button" className="lnk" onClick={() => go({ name: "deployments" })}>{tr.nav_deployments}</button>
          <span className="sep">/</span>
          <span style={{ color: "var(--text)" }}>{tr.nav_create}</span>
        </div>)
      : <div className="crumb"><span style={{ color: "var(--text)" }}>{tr[`nav_${section}`] || ""}</span></div>;

  return (
    <div className={`shell${navOpen ? " nav-open" : ""}`}>
      {navOpen && <div className="scrim" onClick={() => setNavOpen(false)} />}
      <aside className="sidebar" aria-label={tr.nav_main}>
        <a href="/deployments" className="brand" onClick={e => { e.preventDefault(); go({ name: "deployments" }); }}>
          <span className="logo"><Logo size={20} /></span>
          VeloxSearch
        </a>
        <nav className="snav" aria-label={tr.nav_main}>
          <NavLink href="/deployments" active={section === "deployments"} icon="grid" label={tr.nav_deployments}
            count={loaded ? deployments.length : null} onGo={go} testid="nav-deployments" />
          <NavLink href="/capacity" active={section === "capacity"} icon="gauge" label={tr.nav_capacity} onGo={go} testid="nav-capacity" />
          <NavLink href="/settings" active={section === "settings"} icon="gear" label={tr.nav_settings} onGo={go} testid="nav-settings" />
        </nav>
        <div className="foot">
          <nav className="snav">
            <a href={DOCS_URL} target="_blank" rel="noreferrer"><span className="ic"><Icon name="book" size={16} /></span>{tr.nav_help}</a>
          </nav>
          <Menu items={accountItems} align="up" label={tr.menu_account}
            renderTrigger={({ toggle, ...aria }) => (
              <button type="button" className="user-btn" onClick={toggle} data-testid="account-menu" {...aria}>
                <span className="av">{initial}</span>
                <span className="who"><b>{username || "—"}</b><small>{isTenant ? tr.role_tenant : tr.role_admin}</small></span>
                <Icon name="chevD" size={14} />
              </button>
            )} />
        </div>
      </aside>

      <div className="main">
        <header className="topbar">
          <button type="button" className="iconbtn hamburger" aria-label={navOpen ? tr.close_menu : tr.open_menu}
            aria-expanded={navOpen} onClick={() => setNavOpen(o => !o)}>
            <Icon name={navOpen ? "x" : "menu"} size={18} />
          </button>
          {crumb}
          <span className="spacer" />
          {/* The live stream is down and the app is on its 5s fallback poll. */}
          {sseDead && (
            <span className="badge yellow" title={tr.conn_lost_tip}>
              <Icon name="clock" size={12} />{tr.conn_lost}
            </span>
          )}
        </header>

        <main className="page">
          <OperatorDriftNotice status={bootStatus} t={tr} style={{ marginBottom: 16 }} />
          <TenantIsolationNotice status={bootStatus} t={tr} style={{ marginBottom: 16 }} />

          {/* A view that throws while rendering takes only itself down (#141):
              the shell stays, and navigating away resets it. */}
          <ErrorBoundary t={tr} resetKey={`${route.name}/${route.id || ""}/${route.tab || ""}`}>
            {route.name === "deployments" && (
              <StatusView deployments={deployments} loaded={loaded} lang={lang} capacity={capacity}
                onOpen={(id, tab) => go({ name: "deployment", id, tab: tab || "overview" })}
                onUpgrade={id => go({ name: "deployment", id, tab: "overview", upgrade: true })}
                onCreate={() => go({ name: "create" })} />
            )}
            {route.name === "create" && (
              <CreateView lang={lang} hostNodes={hostNodes} capacity={capacity} onCreate={createCluster}
                onCancel={() => go({ name: "deployments" })} />
            )}
            {route.name === "capacity" && (
              <CapacityView lang={lang} />
            )}
            {route.name === "settings" && (
              <SettingsView lang={lang} onToast={showToast} buildInfo={buildInfo} isTenant={isTenant} />
            )}
            {route.name === "deployment" && (current
              ? <DeploymentView d={current} lang={lang} hostNodes={hostNodes} tab={route.tab || "overview"}
                  openUpgrade={!!route.upgrade}
                  onTab={tab => go({ name: "deployment", id: route.id, tab }, { keepScroll: true })}
                  onToggleStack={toggleOtelStack}
                  onSaveEdit={saveEdit}
                  onResetPass={resetPass}
                  onToast={showToast}
                  onDelete={deleteCluster} />
              : <div className="view-enter"><p className="hint">{tr.loading_dep}</p></div>)}
          </ErrorBoundary>
        </main>
      </div>

      <Toast msg={toast.msg} show={toast.show} kind={toast.kind} onClose={closeToast} t={tr} />

      {DevTweaks && <Suspense fallback={null}><DevTweaks lang={lang} /></Suspense>}
    </div>
  );
}

export { App };
