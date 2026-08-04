pub(super) mod core;
pub(super) mod doc;
pub(super) mod module;

use serde::Serialize;

// ── Shared graph types ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct GraphNode {
    pub(super) id: String,
    pub(super) label: String,
    #[serde(rename = "type")]
    pub(super) node_type: String,
}

#[derive(Serialize)]
pub(super) struct GraphEdge {
    pub(super) source: String,
    pub(super) target: String,
    #[serde(rename = "type")]
    pub(super) edge_type: String,
}

#[derive(Serialize)]
pub(super) struct GraphResponse {
    pub(super) nodes: Vec<GraphNode>,
    pub(super) edges: Vec<GraphEdge>,
}
