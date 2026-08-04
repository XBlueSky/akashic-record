//! MCP streamable-http server (rmcp 1.x) served directly from axum.
//!
//! Replaces the rmcp 0.1 SSE `SseServer` + loopback proxy (Slice E). The
//! `StreamableHttpService` is a tower `Service` nested into an axum `Router` at
//! `/mcp`; the cross-cutting concerns the old proxy applied are now plain
//! axum/tower layers:
//!
//!   1. rate-limit (`apply_mcp_rate_limit`) — outermost
//!   2. [`crate::mcp_middleware::mcp_auth`] — validates the Bearer token,
//!      inserts `Arc<dyn Authenticated>` into request extensions, AND enters
//!      `CURRENT_ACTOR.scope(...)` so tool handlers are quota-counted (closes
//!      the MCP-bypasses-quota gap rmcp 0.1 could not address).
//!   3. trace
//!   4. `StreamableHttpService(AkashicMcp)` — in-process tool dispatch.
//!
//! Write-tool gating (anonymous → error) is enforced in-handler via
//! `require_actor` (no body-peek); B2 audit is recorded in-handler.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::info;

use akashic_platform::middleware::rate_limit::apply_mcp_rate_limit;
use akashic_store_neo4j::Neo4jPool;

use super::tools::AkashicMcp;

/// Build the axum `Router` serving the MCP streamable-http endpoint at `/mcp`,
/// with rate-limit + auth + trace layers. `pg`/`db` are taken explicitly
/// because `AppState` no longer carries the pools (A2a). Public so the
/// `mcp_contract` bench can mount it on an ephemeral port.
pub fn build_mcp_router(app_state: akashic_context::AppState, pg: PgPool, db: Neo4jPool) -> Router {
    let cfg = app_state.config.clone();

    let mcp = AkashicMcp::new(
        db,
        pg,
        app_state.graph_service.clone(),
        app_state.search_service.clone(),
        app_state.navigation_service.clone(),
        app_state.curation_service.clone(),
        app_state.corpus_store.clone(),
    );

    let service = StreamableHttpService::new(
        move || Ok(mcp.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let router = Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            crate::mcp_middleware::mcp_auth,
        ))
        .layer(TraceLayer::new_for_http());

    apply_mcp_rate_limit(router, &cfg)
}

/// Start the public MCP listener on `cfg.mcp_sse_host:cfg.mcp_sse_port`,
/// serving the streamable-http router. Replaces `proxy::start` + the loopback
/// `SseServer`.
pub async fn start(
    app_state: akashic_context::AppState,
    pg: PgPool,
    db: Neo4jPool,
    shutdown: CancellationToken,
) -> Result<()> {
    let cfg = app_state.config.clone();
    let public_addr = format!("{}:{}", cfg.mcp_sse_host, cfg.mcp_sse_port);
    let app = build_mcp_router(app_state, pg, db);

    let listener = TcpListener::bind(&public_addr)
        .await
        .with_context(|| format!("MCP server: failed to bind {public_addr}"))?;
    info!(public = %public_addr, "MCP streamable-http server listening");

    let serve_shutdown = shutdown.clone();
    let serve_signal = shutdown.clone();
    tokio::spawn(async move {
        let serve_result = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { serve_signal.cancelled().await })
        .await;

        if serve_shutdown.is_cancelled() {
            tracing::info!(event = "mcp_server_serve_drained");
            return;
        }
        match serve_result {
            Ok(()) => tracing::error!(
                "MCP server axum::serve exited unexpectedly with Ok(()) — aborting so the orchestrator restarts it"
            ),
            Err(e) => tracing::error!(error = %e, "MCP server axum::serve failed — aborting"),
        }
        serve_shutdown.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::process::exit(70);
    });

    Ok(())
}
