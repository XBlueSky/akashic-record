use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::Serialize;

use akashic_context::AppState;

use super::super::error::AppError;
use super::super::extractors;

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ModuleItem {
    id: String,
    path: String,
    language: Option<String>,
    summary: Option<String>,
    exports_count: Option<i32>,
    file_count: Option<i32>,
}

#[derive(Serialize)]
struct ChunkItem {
    id: String,
    name: String,
    chunk_type: String,
    signature: Option<String>,
    content: String,
    language: Option<String>,
}

#[derive(Serialize)]
struct ChunkDetail {
    id: String,
    repo_name: String,
    module_path: String,
    name: String,
    chunk_type: String,
    signature: Option<String>,
    content: String,
    language: Option<String>,
    git_ref: Option<String>,
    ingested_at: Option<String>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/repos/:name/modules — list ingested modules
pub(super) async fn list_modules(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(page): Query<extractors::PaginationParams>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;
    let limit = page.clamped_limit(500);
    let offset = page.offset();

    let (total, rows) = state
        .graph_service
        .list_modules(name, limit, offset)
        .await?;

    let modules: Vec<ModuleItem> = rows
        .into_iter()
        .map(|m| ModuleItem {
            id: m.id.to_string(),
            path: m.path,
            language: m.language,
            summary: m.summary,
            exports_count: m.exports_count,
            file_count: m.file_count,
        })
        .collect();

    Ok(Json(extractors::Paginated::from_offset(
        modules,
        offset,
        limit,
        Some(total),
    )))
}

/// GET /api/v1/repos/:name/modules/:path/chunks — chunks within a module
pub(super) async fn module_chunks(
    State(state): State<AppState>,
    Path((name, module_path)): Path<(String, String)>,
    Query(page): Query<extractors::PaginationParams>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;
    extractors::validate_module_path(&module_path)?;
    let limit = page.clamped_limit(200);
    let offset = page.offset();

    let rows = state
        .graph_service
        .list_module_chunks(name, module_path, limit, offset)
        .await?;

    let chunks: Vec<ChunkItem> = rows
        .into_iter()
        .map(|c| ChunkItem {
            id: c.id.to_string(),
            name: c.name,
            chunk_type: c.chunk_type,
            signature: c.signature,
            content: c.content,
            language: c.language,
        })
        .collect();

    Ok(Json(extractors::Paginated::from_offset(
        chunks, offset, limit, None,
    )))
}

/// GET /api/v1/repos/:name/chunks/:id — single chunk detail
pub(super) async fn chunk_detail(
    State(state): State<AppState>,
    Path((name, chunk_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let id: uuid::Uuid = chunk_id.parse().map_err(|_| AppError::NotFound)?;

    let row = state
        .graph_service
        .get_chunk_detail(name, id)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(ChunkDetail {
        id: row.id.to_string(),
        repo_name: row.repo_name,
        module_path: row.module_path,
        name: row.name,
        chunk_type: row.chunk_type,
        signature: row.signature,
        content: row.content,
        language: row.language,
        git_ref: row.git_ref,
        ingested_at: row.ingested_at,
    }))
}
