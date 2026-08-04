use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use akashic_domain::ports::services::NotePatch;

use akashic_context::AppState;

use super::super::error::AppError;
use super::super::extractors;
use super::{extract_user_token, resolve_gitlab_access};

// ── Request / Response types ────────────────────────────────────────────────

#[derive(Serialize)]
struct NoteItem {
    uuid: String,
    title: Option<String>,
    summary: Option<String>,
    content: String,
    category: String,
    // FIX(audit notes.rs:56): frontend Note expects `repo_name` (required) and
    // `branch_name`, not `branch`. Emit `repo_name` and rename the branch key so
    // NoteCard's `{#if note.branch_name}` chip and any `note.repo_name` reader work.
    repo_name: String,
    #[serde(rename = "branch_name")]
    branch: Option<String>,
    author: Option<String>,
    created_at: String,
    tags: Option<Vec<String>>,
    facts: Option<Vec<String>>,
    related_files: Option<Vec<String>>,
}

#[derive(Serialize)]
struct NoteDetailResp {
    uuid: String,
    title: Option<String>,
    summary: Option<String>,
    content: String,
    category: String,
    branch: Option<String>,
    repo: String,
    author: Option<String>,
    created_at: String,
    tags: Option<Vec<String>>,
    facts: Option<Vec<String>>,
    related_symbols: Option<Vec<String>>,
    related_files: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(super) struct NotesQuery {
    branch: Option<String>,
    category: Option<String>,
    #[serde(default = "default_page")]
    page: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_page() -> i64 {
    1
}

fn default_limit() -> i64 {
    20
}

/// Maximum page size accepted from clients.
const MAX_LIMIT: i64 = 200;

/// Clamp attacker-controlled `page`/`limit` to sane bounds and compute the
/// SQL OFFSET with saturating arithmetic.
fn paginate(page: i64, limit: i64) -> (i64, i64, i64) {
    let page = page.max(1);
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = page.saturating_sub(1).saturating_mul(limit);
    (page, limit, offset)
}

#[derive(Serialize)]
struct NotesResponse {
    items: Vec<NoteItem>,
    total: i64,
    page: i64,
    limit: i64,
}

#[derive(Deserialize)]
pub(super) struct UpdateNoteBody {
    title: String,
    summary: String,
    content: String,
    category: String,
    tags: Vec<String>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/repos/:name/notes?branch=&category=&page=&limit=
/// Thinned (A2a): delegates to CurationService::list_notes, which returns the
/// total count alongside the page of items. The handler still owns input
/// clamping (paginate) and the wire projection onto `NoteItem`.
pub(super) async fn list_notes(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<NotesQuery>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;
    if let Some(ref b) = params.branch {
        extractors::validate_branch(b)?;
    }
    let (page, limit, offset) = paginate(params.page, params.limit);

    let (total, rows) = state
        .curation_service
        .list_notes(name.clone(), params.branch, params.category, offset, limit)
        .await?;

    let items: Vec<NoteItem> = rows
        .into_iter()
        .map(|r| NoteItem {
            uuid: r.id.to_string(),
            title: r.title,
            summary: r.summary,
            content: String::new(), // content not in list view
            category: r.category,
            repo_name: name.clone(),
            branch: None, // branch not returned by list_notes_paginated
            author: None,
            created_at: r.created_at,
            tags: r.tags,
            facts: None,
            related_files: None,
        })
        .collect();

    Ok(Json(NotesResponse {
        items,
        total,
        page,
        limit,
    }))
}

/// GET /api/v1/repos/:name/notes/:uuid
/// Thinned (A2a-T10): delegates to CurationService::get_note.
pub(super) async fn get_note(
    State(state): State<AppState>,
    Path((name, uuid_str)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let id: uuid::Uuid = uuid_str.parse().map_err(|_| AppError::NotFound)?;

    let note = state.curation_service.get_note(name.clone(), id).await?;

    Ok(Json(NoteDetailResp {
        uuid: note.id.to_string(),
        title: note.title,
        summary: note.summary,
        content: note.content,
        category: note.category,
        branch: if note.branch.is_empty() {
            None
        } else {
            Some(note.branch)
        },
        repo: note.repo,
        author: None,
        created_at: note.created_at,
        tags: if note.tags.is_empty() {
            None
        } else {
            Some(note.tags)
        },
        facts: if note.facts.is_empty() {
            None
        } else {
            Some(note.facts)
        },
        related_symbols: if note.related_symbols.is_empty() {
            None
        } else {
            Some(note.related_symbols)
        },
        related_files: if note.related_files.is_empty() {
            None
        } else {
            Some(note.related_files)
        },
    }))
}

/// PUT /api/v1/repos/:name/notes/:uuid — update a note (Developer+).
/// Thinned (A2a-T10): delegates to CurationService::update_note.
/// Auth + access check remain in handler. Embedding quota-ordering is
/// preserved inside CurationService::update_note (embed before persist).
pub(super) async fn update_note(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((repo_name, note_uuid)): Path<(String, String)>,
    Json(body): Json<UpdateNoteBody>,
) -> Result<StatusCode, AppError> {
    // Verify auth + Developer+ permission
    let gitlab_token = extract_user_token(&state, &headers)
        .await
        .ok_or(AppError::Unauthorized)?;
    let access_level = resolve_gitlab_access(&state, &gitlab_token, &repo_name).await?;
    if access_level < 30 {
        return Err(AppError::Forbidden(
            "Developer+ access required to edit notes".into(),
        ));
    }

    // Validate category
    if !akashic_domain::types::CATEGORIES.contains(&body.category.as_str()) {
        return Err(AppError::BadRequest(format!(
            "Invalid category: {}",
            body.category
        )));
    }

    let note_id: uuid::Uuid = note_uuid
        .parse()
        .map_err(|_| AppError::BadRequest("Invalid note UUID".into()))?;

    // Delegate to CurationService (embedding quota-ordering preserved: embed
    // BEFORE persist, inside the service).
    state
        .curation_service
        .update_note(
            repo_name.clone(),
            note_id,
            NotePatch {
                title: Some(body.title),
                summary: Some(body.summary),
                content: Some(body.content),
                category: Some(body.category),
                tags: Some(body.tags),
                ..Default::default()
            },
        )
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /api/v1/repos/:name/notes/:uuid — delete a note (Maintainer+).
/// Thinned (A2a-T10): delegates to CurationService::delete_note.
/// Auth + access check remain in handler. PG-first-then-Neo4j ordering
/// is preserved inside CurationService::delete_note.
pub(super) async fn delete_note(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((repo_name, note_uuid)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    // Verify auth + Maintainer+ permission
    let gitlab_token = extract_user_token(&state, &headers)
        .await
        .ok_or(AppError::Unauthorized)?;
    let access_level = resolve_gitlab_access(&state, &gitlab_token, &repo_name).await?;
    if access_level < 40 {
        return Err(AppError::Forbidden(
            "Maintainer+ access required to delete notes".into(),
        ));
    }

    let note_id: uuid::Uuid = note_uuid
        .parse()
        .map_err(|_| AppError::BadRequest("Invalid note UUID".into()))?;

    // Delegate to CurationService (PG delete then Neo4j detach-delete,
    // consistent with save_note's dual-write ordering).
    state
        .curation_service
        .delete_note(repo_name.clone(), note_id)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/repos/:name/notes/health?min_staleness=0.3
pub(super) async fn note_health(
    Path(name): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let min_staleness: f64 = params
        .get("min_staleness")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.3);

    match state
        .curation_service
        .get_note_health(name, min_staleness)
        .await
    {
        Ok(summary) => Json(serde_json::json!(summary)).into_response(),
        // FIX(audit notes.rs:406): do not leak raw DB/SQL error detail to the
        // client. Log the real error server-side and return the same sanitized
        // generic body every AppError::Internal response uses.
        Err(e) => {
            tracing::warn!(%e, "Internal error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Internal server error"})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paginate_normal_case() {
        // page 3, limit 20 -> offset 40, values unchanged
        assert_eq!(paginate(3, 20), (3, 20, 40));
        // page 1 starts at offset 0
        assert_eq!(paginate(1, 20), (1, 20, 0));
    }

    #[test]
    fn paginate_clamps_zero_and_negative_page() {
        // page 0 and negative pages clamp up to 1 -> offset 0 (no negative OFFSET)
        assert_eq!(paginate(0, 20), (1, 20, 0));
        assert_eq!(paginate(-5, 20), (1, 20, 0));
        assert_eq!(paginate(i64::MIN, 20), (1, 20, 0));
    }

    #[test]
    fn paginate_clamps_zero_negative_and_huge_limit() {
        // limit 0 / negative clamp up to 1 (no negative LIMIT); page is preserved
        assert_eq!(paginate(2, 0), (2, 1, 1));
        assert_eq!(paginate(2, -100), (2, 1, 1));
        // negative page AND negative limit both clamp; offset stays 0
        assert_eq!(paginate(i64::MIN, i64::MIN), (1, 1, 0));
        // limit above the cap clamps down to MAX_LIMIT
        assert_eq!(paginate(2, 10_000), (2, MAX_LIMIT, MAX_LIMIT));
    }

    #[test]
    fn paginate_saturates_on_overflow() {
        // (page - 1) * limit would overflow i64; saturate instead of panicking.
        let (page, limit, offset) = paginate(i64::MAX, MAX_LIMIT);
        assert_eq!(page, i64::MAX);
        assert_eq!(limit, MAX_LIMIT);
        assert_eq!(offset, i64::MAX);
    }

    /// Regression for audit notes.rs:56 — the list-endpoint JSON must carry
    /// `repo_name` and `branch_name` (the keys the frontend Note type consumes),
    /// and must NOT emit the old `branch` key.
    #[test]
    fn note_item_serializes_repo_and_branch_name_keys() {
        let item = NoteItem {
            uuid: "11111111-1111-1111-1111-111111111111".into(),
            title: Some("t".into()),
            summary: None,
            content: "c".into(),
            category: "BUG_FIX".into(),
            repo_name: "my-repo".into(),
            branch: Some("main".into()),
            author: None,
            created_at: "2026-06-02T00:00:00Z".into(),
            tags: None,
            facts: None,
            related_files: None,
        };

        let v = serde_json::to_value(&item).expect("NoteItem serializes");

        // repo_name is present and matches the path repo.
        assert_eq!(v.get("repo_name").and_then(|x| x.as_str()), Some("my-repo"));
        // branch is exposed under the `branch_name` key the frontend reads.
        assert_eq!(v.get("branch_name").and_then(|x| x.as_str()), Some("main"));
        // the old `branch` key must be gone so the rename is real.
        assert!(v.get("branch").is_none());
    }

    /// A branch-less note must still serialize `branch_name` as JSON null (the
    /// frontend type is `string | null`), not omit it.
    #[test]
    fn note_item_branchless_serializes_null_branch_name() {
        let item = NoteItem {
            uuid: "22222222-2222-2222-2222-222222222222".into(),
            title: None,
            summary: None,
            content: "c".into(),
            category: "DECISION".into(),
            repo_name: "my-repo".into(),
            branch: None,
            author: None,
            created_at: "2026-06-02T00:00:00Z".into(),
            tags: None,
            facts: None,
            related_files: None,
        };

        let v = serde_json::to_value(&item).expect("NoteItem serializes");
        assert!(v.get("branch_name").map(|x| x.is_null()).unwrap_or(false));
        assert!(v.get("branch").is_none());
    }
}
