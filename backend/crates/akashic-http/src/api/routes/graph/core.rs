//! Thinned graph/core.rs handlers — delegates to GraphService (A2a T4).
//!
//! `get_graph` and `get_god_nodes` were previously 300+ lines of inline Neo4j +
//! PG queries. They now extract path params, call the service, and map domain
//! types to the wire-JSON shapes (preserving the `"type"` serde rename).
//!
//! Handler-local response types are kept here so the wire format (including the
//! `#[serde(rename = "type")]` on graph node/edge fields) is unchanged.
//! The domain types (`GraphView`, `GodNodesResult`) use `node_type`/`edge_type`
//! as field names (no rename); the mapping step in each handler converts them.

use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};
use serde::Serialize;

use akashic_context::AppState;

use super::super::super::error::AppError;
use super::super::super::extractors;
use super::{GraphEdge, GraphNode, GraphResponse};

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct GodNode {
    id: String,
    name: String,
    path: String,
    degree: i64,
    node_type: String,
    /// For modules: import count; for chunks: incoming call count
    connectivity: i64,
    /// For modules: chunk count; for chunks: outgoing call count
    composition: i64,
}

#[derive(Serialize)]
struct GodNodeResponse {
    god_nodes: Vec<GodNode>,
    total_nodes: i64,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/graph/:repo_name — lightweight graph data for visualization.
pub(in crate::api::routes) async fn get_graph(
    State(state): State<AppState>,
    Path(repo_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&repo_name)?;

    let view = state.graph_service.get_graph(repo_name).await?;

    // Map domain GraphView → handler GraphResponse (preserves "type" serde rename).
    let resp = GraphResponse {
        nodes: view
            .nodes
            .into_iter()
            .map(|n| GraphNode {
                id: n.id,
                label: n.label,
                node_type: n.node_type,
            })
            .collect(),
        edges: view
            .edges
            .into_iter()
            .map(|e| GraphEdge {
                source: e.source,
                target: e.target,
                edge_type: e.edge_type,
            })
            .collect(),
    };
    Ok(Json(resp))
}

/// GET /api/v1/god-nodes/:repo_name — top-N highest-degree entities.
///
/// Returns the most connected modules and chunks, excluding the repo hub node.
pub(in crate::api::routes) async fn get_god_nodes(
    State(state): State<AppState>,
    Path(repo_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&repo_name)?;

    let result = state.graph_service.get_god_nodes(repo_name).await?;

    let god_nodes: Vec<GodNode> = result
        .god_nodes
        .into_iter()
        .map(|n| GodNode {
            id: n.id,
            name: n.name,
            path: n.path,
            degree: n.degree,
            node_type: n.node_type,
            connectivity: n.connectivity,
            composition: n.composition,
        })
        .collect();

    Ok(Json(GodNodeResponse {
        god_nodes,
        total_nodes: result.total_nodes,
    }))
}
