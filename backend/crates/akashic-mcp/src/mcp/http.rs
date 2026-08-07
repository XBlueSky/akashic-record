//! MCP streamable-http server (rmcp 3.x) served directly from axum.
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
//!
//! Transport is configured sessionless (`legacy_session_mode: false`,
//! `NeverSessionManager`): the 2026-07-28 spec removes sessions entirely (all
//! requests are served statelessly regardless of this flag for that protocol
//! version), and this deploy never negotiates an older, session-carrying
//! version. `json_response: true` prefers a plain JSON response body over SSE
//! for simple request/response tool calls (falls back to `text/event-stream`
//! only if the handler emits a notification/request before the final
//! response).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::info;

use akashic_platform::middleware::rate_limit::apply_mcp_rate_limit;
use akashic_store_neo4j::Neo4jPool;

use super::tools::AkashicMcp;

/// Hosts rmcp's DNS-rebinding guard accepts for the `Host` header. Loopback
/// (dev/tests) plus the public host from `PUBLIC_BASE_URL` (prod behind
/// nginx, which forwards the original `Host` header). Entries carry no port,
/// so `host_is_allowed` matches any port on that host (rmcp 3.1's
/// `NormalizedAuthority` comparison treats a port-less allowed entry as a
/// wildcard on port) — this is what lets tests connect to
/// `127.0.0.1:<ephemeral>` without enumerating ports.
fn allowed_hosts_for(cfg: &akashic_config::Config) -> Vec<String> {
    let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
        .into_iter()
        .map(String::from)
        .collect();
    if let Ok(url) = url::Url::parse(&cfg.public_base_url)
        && let Some(h) = url.host_str()
    {
        let h = h.trim_start_matches('[').trim_end_matches(']').to_string();
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    hosts
}

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

    let http_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false) // 1.7's `stateful_mode` renamed; 2026-07-28 is always stateless
        .with_json_response(true)
        .with_allowed_hosts(allowed_hosts_for(&cfg));

    let service = StreamableHttpService::new(
        move || Ok(mcp.clone()),
        Arc::new(NeverSessionManager::default()),
        http_config,
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

#[cfg(test)]
mod tests {
    #[test]
    fn allowed_hosts_includes_loopback_and_public_host() {
        let mut cfg = akashic_test_support::test_config_minimal();
        cfg.public_base_url = "https://akashic.example.com".into();
        let hosts = super::allowed_hosts_for(&cfg);
        assert!(hosts.contains(&"localhost".to_string()));
        assert!(hosts.contains(&"127.0.0.1".to_string()));
        assert!(hosts.contains(&"akashic.example.com".to_string()));
    }
}
