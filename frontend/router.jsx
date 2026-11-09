// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   router.jsx — URL ↔ route, on the History API, with no library.

   Every screen and every deployment tab has a path, so F5 keeps the place,
   the browser's Back button works, and a link in an alert or a support
   ticket opens the right screen:

     /                          → /deployments
     /deployments               the list
     /deployments/new           the create wizard
     /deployments/:id           → /deployments/:id/overview
     /deployments/:id/:tab      overview | data | backup | access | config | activity
     /capacity
     /mesh                      → /mesh/overview
     /mesh/:tab                 overview | agents | policies (ADR-068)
     /settings
     /login, /setup             owned by the boot gate (app.jsx), never rendered
                                as screens by this router

   `?upgrade=1` on a deployment opens the upgrade dialog (the list's "Upgrade
   vX" tag) — a one-shot intent, stripped once consumed.

   The old tab names (edit, integrations, snapshot, security, auth) are
   accepted as aliases so a bookmark from before the regrouping still lands.
   Deployment names are RFC 1123 labels (views_create.jsx NAME_RE), so a path
   segment never needs escaping — and never contains a dot, which is what the
   server's asset check keys on (auth.rs is_asset).
   ============================================================ */
import { useState, useEffect, useCallback } from "react";

const TABS = ["overview", "data", "backup", "access", "config", "activity"];
// Velox Mesh sub-screens (ADR-068 §4.5). Same rule as the deployment tabs:
// an unknown segment falls back rather than 404ing a bookmark.
const MESH_TABS = ["overview", "agents", "policies"];
const TAB_ALIASES = {
  edit: "config", integrations: "data", snapshot: "backup",
  security: "access", auth: "access",
};

function parse(pathname, search) {
  const q = new URLSearchParams(search || "");
  const seg = (pathname || "/").split("/").filter(Boolean);
  if (seg.length === 0) return { name: "deployments" };
  switch (seg[0]) {
    case "deployments": {
      if (seg.length === 1) return { name: "deployments" };
      if (seg[1] === "new") return { name: "create" };
      const id = seg[1];
      let tab = seg[2] || "overview";
      tab = TAB_ALIASES[tab] || tab;
      if (!TABS.includes(tab)) tab = "overview";
      return { name: "deployment", id, tab, upgrade: q.get("upgrade") === "1" };
    }
    case "capacity": return { name: "capacity" };
    case "mesh": {
      const tab = MESH_TABS.includes(seg[1]) ? seg[1] : "overview";
      return { name: "mesh", tab };
    }
    case "settings": return { name: "settings" };
    case "login": case "setup": return { name: "auth" };
    default: return { name: "deployments" };
  }
}

function pathFor(r) {
  switch (r.name) {
    case "create": return "/deployments/new";
    case "deployment": return `/deployments/${r.id}/${r.tab || "overview"}${r.upgrade ? "?upgrade=1" : ""}`;
    case "capacity": return "/capacity";
    case "mesh": return `/mesh/${r.tab || "overview"}`;
    case "settings": return "/settings";
    default: return "/deployments";
  }
}

// The current route and a `go(routeOrPath, { replace })`. `go` pushes a
// history entry (or replaces the current one) and scrolls to the top, the
// way a full navigation would.
function useRouter() {
  const read = () => parse(window.location.pathname, window.location.search);
  const [route, setRoute] = useState(read);

  useEffect(() => {
    const onPop = () => setRoute(read());
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const go = useCallback((r, opts = {}) => {
    const path = typeof r === "string" ? r : pathFor(r);
    const cur = window.location.pathname + window.location.search;
    if (path !== cur) {
      try {
        if (opts.replace) window.history.replaceState(null, "", path);
        else window.history.pushState(null, "", path);
      } catch (e) { /* file:// or a sandboxed frame: state still moves */ }
    }
    setRoute(parse(path.split("?")[0], path.includes("?") ? path.slice(path.indexOf("?")) : ""));
    if (!opts.keepScroll) window.scrollTo({ top: 0 });
  }, []);

  return [route, go];
}

// Where to send the user after a successful login: the `next` the server put
// on its redirect (auth.rs), if it is a plain in-app path. Same rule as
// auth.rs login_redirect, including the refusal of `//`: `//2130706433` is a
// scheme-relative URL to 127.0.0.1, not a path. history.replaceState would
// throw on it rather than leave the origin, but the query string is
// attacker-controlled and the check should not lean on that.
function nextAfterLogin() {
  const n = new URLSearchParams(window.location.search).get("next") || "";
  return /^\/(?!\/)[A-Za-z0-9/_-]*$/.test(n) && n !== "/login" && n !== "/setup" ? n : "/deployments";
}

export { useRouter, parse as parseRoute, pathFor, nextAfterLogin, TABS as DEPLOYMENT_TABS };
