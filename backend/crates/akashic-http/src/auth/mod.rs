//! Phase-1 shim: core identity types (AuthStore, quota, types, oauth_runtime,
//! session_cookie_value) live in `akashic-identity`. Handler files that couple
//! to `AppState` stay here. `mcp_middleware` was carved into `akashic-mcp`
//! (A2b) since it couples to mcp internals and is only used by `mcp/proxy`.

// Re-export everything from akashic-identity so `crate::auth::X` keeps working.
pub use akashic_identity::*;

// Handler modules (Router<AppState> coupling — cannot move to identity).
pub mod account;
mod exchange;
pub mod mcp_oauth;
pub mod middleware;
mod oauth;
pub mod oauth_device;
mod web;

// Re-exports from local handler modules needed by lib.rs and other callers.
pub use middleware::require_auth;

// D3: re-export the session-cookie builder so the test-fixtures router can
// mint cookies byte-identical to real-login output (see Task 2). The `web`
// module itself remains private — only this single helper is exposed.
#[cfg(feature = "test-fixtures")]
pub(crate) use web::build_session_cookie;

use akashic_context::AppState;
use axum::Router;

/// Build the public (unauthenticated) auth router.
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(oauth::router())
        .merge(exchange::router())
        .merge(web::public_router())
        .merge(oauth_device::public_router())
        .merge(mcp_oauth::public_router())
}

/// Build the protected auth router (behind auth middleware).
pub fn protected_auth_router() -> Router<AppState> {
    web::protected_router()
        .merge(oauth_device::protected_router())
        .merge(account::router())
}
