// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Velox Mesh — the controller. Skeleton: it boots, states its entitlement
//! and answers health. The OpAMP endpoint exists so an agent can be pointed
//! at it today and get an honest "not implemented" instead of a 404.
//!
//! Shape to grow into (veloxagent/docs/ARCHITECTURE.md §4): stateless replicas,
//! Postgres for state (the core's `VELOX_PG_*` conventions, own migrations
//! under `veloxagent/migrations/`), agents long-poll `/v1/opamp`, the console talks to
//! `/api/mesh/*` with a service token minted by the core. Same licence as the
//! core (AGPL-3.0-only), but a separate workspace, binary and image
//! (ADR-068): the Mesh scales and releases on its own cadence, and a core
//! upgrade never restarts the fleet's controller.

use axum::{http::StatusCode, routing::get, routing::post, Json, Router};
use velox_license::{entitlement, Tier};

/// Where to listen. Spec default port for OpAMP is 4320.
const ADDR_VAR: &str = "VELOX_MESH_ADDR";
const DEFAULT_ADDR: &str = "127.0.0.1:4320";

#[tokio::main]
async fn main() {
    let addr = std::env::var(ADDR_VAR).unwrap_or_else(|_| DEFAULT_ADDR.to_string());

    // No licence store yet: the free tier, as any fresh install.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ent = entitlement(None, now);
    let tier = match ent.tier {
        Tier::Free => "free",
        Tier::Licensed => "licensed",
        Tier::Grace { .. } => "grace",
        Tier::ReadOnly { .. } => "read-only",
    };
    eprintln!(
        "velox-mesh {} — tier {tier}, {} seats",
        env!("CARGO_PKG_VERSION"),
        ent.seats
    );

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(velox_opamp::DEFAULT_PATH, post(opamp_not_yet));

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("velox-mesh: cannot listen on {addr}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("velox-mesh: listening on http://{addr}");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("velox-mesh: server error: {e}");
        std::process::exit(1);
    }
}

/// The agent gets a structured refusal it can log, in the shape it will
/// later parse for real.
async fn opamp_not_yet() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(
            serde_json::json!({ "error": "velox-mesh: OpAMP endpoint not implemented yet (veloxagent/docs/PLAN.md fase 1)" }),
        ),
    )
}
