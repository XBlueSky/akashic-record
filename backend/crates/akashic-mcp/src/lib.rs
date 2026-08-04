//! MCP (Model Context Protocol) integration for Akashic Record.
//!
//! Carved out of `akashic-server` (A2b). Slice E (rmcp 1.x) replaced the SSE
//! `server` + loopback `proxy` with the streamable-http [`mcp::http`] router.

pub mod mcp {
    pub mod http;
    pub mod tools;
    pub mod types;
}

pub mod mcp_middleware;
