//! AuditPort — a dyn-safe async trait for recording write-tool invocations.
//!
//! Hoisted here (with `actor`) to break the import cycle between `mcp` and
//! `{auth, llm, embedding}`. Provider decorators (`QuotaLlm`, `QuotaEmbedding`)
//! depend on this trait rather than calling `mcp::audit::record_write` directly,
//! removing the back-edge from those crates into `mcp`.
//!
//! The production implementation (`McpAudit`) lives in `mcp::audit` and
//! delegates to the existing `record_write` free function.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::actor::Authenticated;

/// Port for fire-and-forget audit writes.
///
/// The signature mirrors `mcp::audit::record_write` exactly (minus the
/// `pg: PgPool` that the implementation captures in its struct).
#[async_trait]
pub trait AuditPort: Send + Sync {
    async fn record_write(
        &self,
        auth: Arc<dyn Authenticated>,
        tool_name: String,
        args: Value,
        response_body: Vec<u8>,
        success: bool,
        peer_ip: Option<std::net::IpAddr>,
    );
}

/// No-op implementation of `AuditPort`. Useful in unit tests that need to
/// construct `QuotaLlm` / `QuotaEmbedding` without a live database connection.
#[derive(Debug, Default)]
pub struct NoopAudit;

#[async_trait]
impl AuditPort for NoopAudit {
    async fn record_write(
        &self,
        _auth: Arc<dyn Authenticated>,
        _tool_name: String,
        _args: Value,
        _response_body: Vec<u8>,
        _success: bool,
        _peer_ip: Option<std::net::IpAddr>,
    ) {
        // intentionally does nothing
    }
}
