//! Akashic Record backend library crate.
//!
//! D2 (2026-05-09): split out from `main.rs` so integration tests under
//! `backend/tests/` can construct `AppState`, call `migrate::run`, build
//! the production router, and pre-populate auth sessions.
//!
//! Modules are re-declared here with `pub` visibility. `main.rs` keeps
//! `fn main()` and CLI dispatch and `use akashic_record::*`s everything
//! it needs from this crate.

// A2b carve #3 (2026-06-04): the HTTP interface (api/* + auth/* + gitlab/* +
// build_router) was carved into the `akashic-http` crate. Re-export them here
// so every existing `crate::api::…` / `crate::auth::…` / `crate::gitlab::…` /
// `akashic_record::build_router` reference in `main.rs` and the integration
// tests keeps resolving unchanged.
pub use akashic_http::{api, auth, build_router, gitlab};

// A2b (2026-06-04): the MCP integration (mcp/* + the mcp_middleware that pairs
// with mcp/proxy) was carved into the `akashic-mcp` crate. Re-export the `mcp`
// module here so every existing `crate::mcp::proxy::start`,
// `crate::mcp::audit::McpAudit`, etc. reference in `main.rs` keeps resolving
// unchanged.
pub use akashic_mcp::mcp;

// A2b (2026-06-04): the platform/infrastructure modules (alerts, middleware,
// migrate, observability, readiness, shutdown) were carved into the
// `akashic-platform` leaf crate. Re-export them here so every existing
// `crate::middleware::…` / `crate::migrate::…` / `akashic_record::readiness::…`
// reference in `build_router`, `main.rs`, and the integration tests keeps
// resolving unchanged.
pub use akashic_platform::{alerts, middleware, migrate, observability, readiness, shutdown};

#[cfg(test)]
pub mod verification;

// AppState is now defined in akashic-context and re-exported here so that
// all `use crate::AppState` and `crate::AppState` references in handler modules
// continue to resolve without any changes to those files.
pub use akashic_context::AppState;
