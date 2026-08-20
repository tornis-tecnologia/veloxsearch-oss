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
import "./styles.css";

createRoot(document.getElementById("root")).render(<App />);
