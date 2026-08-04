use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};
use serde::Serialize;

use akashic_context::AppState;

use super::super::super::error::AppError;
use super::super::super::extractors;

// NOTE (A2a): get_module_graph, get_module_detail, and get_module_call_graph
// all delegate to GraphService — no inline SQL/Cypher remains in this handler.

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ModuleGraphResponse {
    nodes: Vec<ModuleNode>,
    edges: Vec<ImportEdge>,
    calls_count: i64,
    saga_groups: Vec<SagaGroup>,
}

#[derive(Serialize)]
struct SagaGroup {
    saga_id: String,
    name: String,
    status: String,
    module_ids: Vec<String>,
}

#[derive(Serialize)]
struct ModuleNode {
    id: String,
    label: String,
    path: String,
    chunk_count: i64,
    note_count: i64,
    is_virtual: bool,
    language: String,
}

#[derive(Serialize)]
struct ImportEdge {
    source: String,
    target: String,
}

// NOTE: CallGraph* response types and resolve_saga_status helper removed (A2a-T4).
// The get_module_call_graph handler now delegates to GraphService and returns
// a serde_json::Value without needing local structs.

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/graph/:repo_name/modules — module-only graph for D3.js visualization.
///
/// Thinned (A2a-T4): delegates to `GraphService::get_module_graph` and maps
/// the domain `ModuleGraphView` back to the handler-local wire response types.
pub(in crate::api::routes) async fn get_module_graph(
    State(state): State<AppState>,
    Path(repo_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&repo_name)?;

    let view = state.graph_service.get_module_graph(repo_name).await?;

    // Map domain types → handler-local wire types (ImportEdge has no edge_type field).
    let nodes: Vec<ModuleNode> = view
        .nodes
        .into_iter()
        .map(|n| ModuleNode {
            id: n.id,
            label: n.label,
            path: n.path,
            chunk_count: n.chunk_count,
            note_count: n.note_count,
            is_virtual: n.is_virtual,
            language: n.language,
        })
        .collect();

    let edges: Vec<ImportEdge> = view
        .edges
        .into_iter()
        .map(|e| ImportEdge {
            source: e.source,
            target: e.target,
        })
        .collect();

    let saga_groups: Vec<SagaGroup> = view
        .saga_groups
        .into_iter()
        .map(|sg| SagaGroup {
            saga_id: sg.saga_id,
            name: sg.name,
            status: sg.status,
            module_ids: sg.module_ids,
        })
        .collect();

    Ok(Json(ModuleGraphResponse {
        nodes,
        edges,
        calls_count: view.calls_count,
        saga_groups,
    }))
}

/// GET /api/v1/modules/:module_id/detail — chunk drill-down for sidebar.
///
/// Thinned (A2a): delegates to `GraphService::get_module_detail`, which returns
/// the combined module/chunks/notes view as a JSON string (or `None` for an
/// unknown module → 404). Mirrors `get_module_call_graph`.
pub(in crate::api::routes) async fn get_module_detail(
    State(state): State<AppState>,
    Path(module_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let id: uuid::Uuid = module_id.parse().map_err(|_| AppError::NotFound)?;

    let json_str = state
        .graph_service
        .get_module_detail(id)
        .await?
        .ok_or(AppError::NotFound)?;
    let value: serde_json::Value = serde_json::from_str(&json_str).map_err(anyhow::Error::from)?;
    Ok(Json(value))
}

/// GET /api/v1/modules/:module_id/call-graph — chunk-level CALLS graph for drill-down.
///
/// Thinned (A2a-T4): delegates to `GraphService::get_module_call_graph`.
/// The service returns a JSON string; we parse it and re-serialise via `Json`.
pub(in crate::api::routes) async fn get_module_call_graph(
    State(state): State<AppState>,
    Path(module_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let id: uuid::Uuid = module_id.parse().map_err(|_| AppError::NotFound)?;

    let json_str = state.graph_service.get_module_call_graph(id).await?;
    let value: serde_json::Value = serde_json::from_str(&json_str).map_err(anyhow::Error::from)?;
    Ok(Json(value))
}
