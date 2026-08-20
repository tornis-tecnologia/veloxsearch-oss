// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The Axum backend to proxy /api/* to during `npm run dev` (issue #68).
// Defaults to the dev bind docs/DEVELOPMENT.md documents
// (`VELOX_SITE_ADDR=127.0.0.1:3000`); override with VITE_API_TARGET.
const apiTarget = process.env.VITE_API_TARGET || "http://127.0.0.1:3000";

// Builds the React SPA (issue #31). Output goes to frontend/build, which
// deploy/build-image.sh stages as the container's /app/dist. base "/" so the
// hashed assets resolve from the server root (Axum serves them under /assets/,
// see src/auth.rs::is_asset). The repo-root public/ (favicon.ico) is copied
// verbatim into the build root.
export default defineConfig({
  plugins: [react()],
  base: "/",
  publicDir: "../public",
  build: {
    outDir: "build",
    emptyOutDir: true,
  },
  // Dev-only. `vite build` ignores `server`, so the production build path
  // (frontend/build → image) is untouched. The SPA calls the API at the
  // relative path /api/* (api.jsx: API_BASE = "/api"); without this proxy the
  // Vite dev server answers those itself and every call 404s (issue #68).
  server: {
    proxy: {
      "/api": {
        target: apiTarget,
        changeOrigin: true,
        // The /api/events SSE stream must stream, not buffer. Vite's proxy
        // (node-http-proxy) pipes responses through, so the stream flows by
        // default — but assert no compressing/transforming middlebox buffers
        // it: ask upstream for identity encoding and forbid transforms/caching
        // downstream (x-accel-buffering: no defuses a reverse proxy in front).
        configure: (proxy) => {
          proxy.on("proxyReq", (proxyReq, req) => {
            if (req.url && req.url.startsWith("/api/events")) {
              proxyReq.setHeader("Accept-Encoding", "identity");
            }
          });
          proxy.on("proxyRes", (proxyRes, req) => {
            if (req.url && req.url.startsWith("/api/events")) {
              proxyRes.headers["cache-control"] = "no-cache, no-transform";
              proxyRes.headers["x-accel-buffering"] = "no";
            }
          });
        },
      },
    },
  },
});
