// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
/* ============================================================
   Vite entry — mounts the React SPA (issue #31)

   Replaces the old @babel/standalone in-browser pipeline: this single
   module is the only <script> in index.html. It pulls in the global
   stylesheet (so Vite bundles + hashes it) and renders <App /> into #root.
   ============================================================ */
import { createRoot } from "react-dom/client";
import { App } from "./app.jsx";
import { STR } from "./i18n.jsx";
import { ErrorBoundary } from "./ui.jsx";
import "./styles.css";

// The top-level boundary sits outside App, so it cannot see App's language
// state; it reads the persisted choice (app.jsx writes it) when it renders.
function strings() {
  let lang = null;
  try { lang = localStorage.getItem("velox-lang"); } catch (e) { /* storage blocked */ }
  return STR[lang] || STR.pt;
}

// Anything App itself throws while rendering lands here instead of leaving a
// blank page (#141); each view also has its own boundary inside App.
createRoot(document.getElementById("root")).render(
  <ErrorBoundary t={strings}>
    <App />
  </ErrorBoundary>
);
