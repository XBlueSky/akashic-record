//! Thinned graph/doc.rs handlers — delegates to GraphService (A2a T4).
//!
//! `get_doc_graph`, `get_document_detail`, and `get_cluster_detail` were
//! previously 700+ lines of inline Neo4j + PG queries.  They now extract path
//! params, call the service, and return the pre-serialised JSON string as an
//! `application/json` response.
//!
//! The pure helper functions `group_explains_tuples` / `group_explains_rows`
//! and their unit tests are moved to the service layer inside
//! `akashic-retrieval::services` (logically: they belong with the UNWIND
//! batch-explains orchestration, not in the HTTP adapter layer).

use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};

use akashic_context::AppState;

use super::super::super::error::AppError;
use super::super::super::extractors;

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/doc-graph/:repo_name — document constellation graph for website repos.
///
/// Thinned (A2a-T4): delegates to `GraphService::get_doc_graph`.
pub(in crate::api::routes) async fn get_doc_graph(
    State(state): State<AppState>,
    Path(repo_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&repo_name)?;

    let json_str = state.graph_service.get_doc_graph(repo_name).await?;
    let value: serde_json::Value = serde_json::from_str(&json_str).map_err(anyhow::Error::from)?;
    Ok(Json(value))
}

/// GET /api/v1/documents/:doc_id/sections — section detail with EXPLAINS targets.
///
/// Thinned (A2a-T4): delegates to `GraphService::get_document_detail`.
pub(in crate::api::routes) async fn get_document_detail(
    State(state): State<AppState>,
    Path(doc_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let id: uuid::Uuid = doc_id.parse().map_err(|_| AppError::NotFound)?;

    let json_str = state.graph_service.get_document_detail(id).await?;
    let value: serde_json::Value = serde_json::from_str(&json_str).map_err(anyhow::Error::from)?;
    Ok(Json(value))
}

/// GET /api/v1/clusters/:cluster_id/sections — section detail for a topic cluster.
///
/// Thinned (A2a-T4): delegates to `GraphService::get_cluster_detail`.
pub(in crate::api::routes) async fn get_cluster_detail(
    State(state): State<AppState>,
    Path(cluster_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let id: uuid::Uuid = cluster_id.parse().map_err(|_| AppError::NotFound)?;

    let json_str = state.graph_service.get_cluster_detail(id).await?;
    let value: serde_json::Value = serde_json::from_str(&json_str).map_err(anyhow::Error::from)?;
    Ok(Json(value))
}
