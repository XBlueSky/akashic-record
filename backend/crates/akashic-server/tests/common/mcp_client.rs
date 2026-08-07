//! MCP test client (Slice E). Drives `tools/list` and `tools/call` against a
//! live MCP server over the rmcp 3.1 **streamable-http** transport, single
//! sessionless `/mcp` port (`mcp::http`), using the 2026-07-28 **Discover**
//! lifecycle (`server/discover` + self-contained per-request metadata,
//! rather than the legacy `initialize`/`notifications/initialized`
//! handshake — see [`ClientLifecycleMode::Discover`]). The server still
//! accepts the legacy `initialize` path for other callers (unchanged by
//! this task); this client just pins itself to Discover so the contract
//! suite exercises — and locks in — the target 2026-07-28 protocol.
//!
//! `connect()` is anonymous. `connect_with_bearer()` sets the `Authorization:
//! Bearer <token>` header (via the transport's `auth_header` config, applied
//! to every request the transport sends — including the `server/discover`
//! call itself) so the server's `mcp_auth` layer validates it and the
//! in-handler write-tool gate sees an authenticated actor. `mcp_auth` is
//! method-agnostic (Task 3): it gates on the HTTP `Authorization` header
//! alone, before any JSON-RPC method (including `server/discover`) is even
//! parsed, so Discover-lifecycle connects need a bearer exactly as
//! Initialize-lifecycle ones did.

use anyhow::{Result, anyhow};
use rmcp::{
    model::{CallToolRequestParams, ClientInfo, ContentBlock, Implementation, ProtocolVersion},
    service::{ClientLifecycleMode, ClientServiceExt, RoleClient, RunningService},
    transport::streamable_http_client::{
        StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
    },
};
use serde_json::Value;

const CLIENT_NAME: &str = "akashic-record-contract-test";
const CLIENT_VERSION: &str = "0.1.0";

pub struct McpClient {
    service: RunningService<RoleClient, ClientInfo>,
}

/// `<base_url>/mcp` — the streamable-http endpoint mounted by `mcp::http`.
fn mcp_uri(base_url: &str) -> String {
    format!("{}/mcp", base_url.trim_end_matches('/'))
}

impl McpClient {
    /// Connect anonymously.
    pub async fn connect(base_url: &str) -> Result<Self> {
        let config = StreamableHttpClientTransportConfig::with_uri(mcp_uri(base_url));
        Self::connect_inner(config).await
    }

    /// Connect with `Authorization: Bearer <token>` (the transport prepends
    /// `Bearer `). Use for write-tool tests: `mcp_auth` validates the `ak_*` /
    /// `glpat-*` prefix and the in-handler gate requires the resulting actor.
    pub async fn connect_with_bearer(base_url: &str, token: &str) -> Result<Self> {
        let config =
            StreamableHttpClientTransportConfig::with_uri(mcp_uri(base_url)).auth_header(token);
        Self::connect_inner(config).await
    }

    async fn connect_inner(config: StreamableHttpClientTransportConfig) -> Result<Self> {
        // `from_config` builds rmcp's own (reqwest-backed) client internally —
        // avoids a type mismatch with the workspace's separate reqwest version.
        let transport = StreamableHttpClientTransport::from_config(config);
        // ClientInfo (InitializeRequestParams) is #[non_exhaustive] in rmcp 3.x —
        // start from Default and override only the implementation identity.
        let mut client_info = ClientInfo::default();
        client_info.client_info = Implementation::new(CLIENT_NAME, CLIENT_VERSION);
        // Discover lifecycle (2026-07-28 spec §6): `ClientServiceExt::
        // serve_with_lifecycle` sends `server/discover` instead of
        // `initialize`, negotiating a version from `preferred_versions`
        // against the server's advertised `supported_versions`
        // (`select_protocol_version` in rmcp's `service/client.rs`; our
        // server's `discover` override — `akashic-mcp/src/mcp/tools/mod.rs`
        // — always advertises 2026-07-28, so a single preferred version is
        // sufficient, no legacy fallback needed here).
        let service = client_info
            .serve_with_lifecycle(
                transport,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            )
            .await
            .map_err(|e| anyhow!("serve as client (discover lifecycle): {e}"))?;
        Ok(Self { service })
    }

    /// The MCP protocol version negotiated during the Discover handshake.
    /// Backed by `Peer::peer_info()` (`Option<Arc<ServerPeerInfo>>` for
    /// `RoleClient`), which `serve_with_lifecycle`'s discover path
    /// populates via `ServerPeerInfo::from_discover_result` before this
    /// `McpClient` is ever constructed — so `.expect()` here is safe: a
    /// successfully-returned `connect`/`connect_with_bearer` guarantees it.
    pub fn protocol_version(&self) -> ProtocolVersion {
        self.service
            .peer()
            .peer_info()
            .expect("peer info is set by a successful discover handshake")
            .protocol_version
            .clone()
    }

    /// Return the names of all tools.
    pub async fn tools_list(&self) -> Result<Vec<String>> {
        let resp = self
            .service
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| anyhow!("list_all_tools: {e}"))?;
        Ok(resp.into_iter().map(|t| t.name.to_string()).collect())
    }

    /// Call a tool and return the response payload as a JSON `Value`. The
    /// stitched text-content blocks are parsed as JSON; on parse failure the raw
    /// text is returned as `Value::String`. Empty content collapses to `Null`.
    pub async fn tools_call(&self, name: &str, args: Value) -> Result<Value> {
        let mut param = CallToolRequestParams::new(name.to_string());
        if let Some(obj) = args.as_object().cloned() {
            param = param.with_arguments(obj);
        }
        let resp = self
            .service
            .peer()
            .call_tool(param)
            .await
            .map_err(|e| anyhow!("call_tool {name}: {e}"))?;
        if resp.is_error.unwrap_or(false) {
            return Err(anyhow!(
                "tool {name} returned is_error=true: {:?}",
                resp.content
            ));
        }
        let text = resp
            .content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}
