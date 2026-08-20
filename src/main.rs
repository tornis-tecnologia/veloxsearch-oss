// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use tower_http::services::{ServeDir, ServeFile};

    // kube-rs uses rustls; with both aws-lc-rs and ring in the tree, rustls
    // can't auto-pick a provider — install one explicitly before any TLS use.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Control-plane datastore bring-up (ADR-041, #92), gated by
    // VELOX_PG_ENABLED (default off). Enabled: migrations must reach head
    // BEFORE the app serves — a DB outage at startup fails closed rather than
    // silently degrading (K8s restarts us; the loop converges once the DB pod
    // is up). Disabled: behave exactly as before, but still best-effort ensure
    // the credentials Secret so the bundled Postgres StatefulSet can start
    // (its pod env references the Secret this app generates).
    if veloxsearch::db::pg_enabled() {
        if let Err(e) = veloxsearch::db::bring_up().await {
            tracing::error!("control-plane postgres bring-up failed, refusing to serve: {e:#}");
            std::process::exit(1);
        }
    } else {
        tracing::info!("control-plane postgres disabled (VELOX_PG_ENABLED unset; ADR-041 bring-up flag)");
        tokio::spawn(async {
            if let Err(e) = veloxsearch::db::ensure_pg_secret_best_effort().await {
                tracing::warn!("could not ensure postgres credentials Secret (harmless off-cluster): {e:#}");
            }
        });
    }

    // The React SPA bundle. Any path that is not a real file falls back to
    // index.html so the client-side router owns navigation (/login, /setup,
    // /d/:name, …). Built by the frontend; the dir is overridable for deploys.
    let static_dir = std::env::var("VELOX_STATIC_DIR").unwrap_or_else(|_| "dist".to_string());
    let spa = ServeDir::new(&static_dir).fallback(ServeFile::new(format!("{static_dir}/index.html")));

    let app = Router::new()
        .nest("/api", veloxsearch::api::routes())
        .fallback_service(spa)
        // Gate every route (incl. the dangerous /api/create_cluster) behind a
        // session. The guard lets through the login + setup flows, /api/auth_state,
        // and static assets; first-run mode funnels everything to /setup.
        .layer(axum::middleware::from_fn(veloxsearch::auth::auth_guard));

    // Background cluster-health sampler (#9): appends one aggregate sample per
    // deployment per interval into the bounded `velox-metrics-*` series the
    // Overview's time-series view reads. Best-effort; never blocks serving.
    tokio::spawn(veloxsearch::metrics::run_sampler());

    // Hourly upstream version check (ADR-048 rev. 2): discovers the newest
    // OpenSearch release so a deployment can show an "Upgrade v3.8.0" tag.
    // Suggestion only — it writes nothing and every upgrade still goes through
    // the same pre-flight. VELOX_VERSION_CHECK_SECS=0 turns it off.
    tokio::spawn(veloxsearch::version_feed::run_poller());

    let addr = std::env::var("VELOX_SITE_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
    tracing::info!("listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

#[cfg(not(feature = "ssr"))]
pub fn main() {
    // no client-side main function — see lib.rs hydrate()
}
