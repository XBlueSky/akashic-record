use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use akashic_context::AppState;

use super::super::error::AppError;
use super::super::extractors;
use super::{extract_user_token, require_gitlab_access, resolve_username};

/// Resolve the effective source type for a repo, handler-side, so the per-repo
/// GitLab authorization check (`require_gitlab_access`) can run BEFORE the
/// idempotency reserve and the service call.
///
/// Mirrors the source-type resolution the pre-thinning handlers performed
/// inline (commit HEAD~1): look up the `sources` table; if absent, infer from
/// the repo name (`'/'`-containing names default to gitlab). `resume_ingest`
/// used the same `sources.source_type` value (it has no name-based fallback),
/// so callers can ignore the inferred fallback by checking the `from_db` flag.
async fn resolve_source_type(state: &AppState, repo: &str) -> Result<(String, bool), AppError> {
    state
        .ingest_service
        .resolve_source_type(repo.to_string())
        .await
        .map_err(AppError::from)
}

// ── Request / Response types ────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct IngestRequestBody {
    git_ref: String,
    #[serde(default = "default_source")]
    source: String,
    path: Option<String>,
}

fn default_source() -> String {
    "gitlab".into()
}

#[derive(Serialize, Deserialize)]
struct IngestResponse {
    job_id: String,
    status: String,
}

#[derive(Deserialize)]
pub(super) struct AddSourceRequest {
    source_type: String, // "website"
    url: String,
    crawl_depth: Option<u8>,
    url_pattern: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct AddSourceResponse {
    job_id: String,
    repo_name: String,
    status: String,
}

#[derive(Serialize)]
struct JobStatus {
    id: String,
    repo_name: String,
    git_ref: Option<String>,
    status: String,
    total_files: Option<i32>,
    processed_files: Option<i32>,
    total_chunks: Option<i32>,
    error_message: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

#[derive(Serialize)]
struct ActiveJob {
    repo_name: String,
    status: String,
    processed_files: Option<i32>,
    total_files: Option<i32>,
}

#[derive(Deserialize)]
pub(super) struct ReingestQuery {
    branch: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GitLabBranchesQuery {
    repo: String,
}

#[derive(Serialize)]
struct GitLabBranchItem {
    name: String,
    default: bool,
}

#[derive(Serialize)]
struct ActiveJobInfo {
    job_id: String,
    status: String,
    processed_files: Option<i32>,
    total_files: Option<i32>,
}

#[derive(Serialize)]
struct SourceOverview {
    name: String,
    source_type: String,
    status: String,
    branch: Option<String>,
    chunk_count: i64,
    module_count: i64,
    note_count: i64,
    section_count: i64,
    last_synced_at: Option<String>,
    active_job: Option<ActiveJobInfo>,
    last_error: Option<String>,
    can_resume: bool,
}

#[derive(Serialize)]
struct SourcesOverviewResponse {
    sources: Vec<SourceOverview>,
    total: usize,
    healthy: usize,
    stale: usize,
    failed: usize,
    ingesting: usize,
}

// ── Idempotency step helpers ──────────────────────────────────────────────────
//
// The four ingestion handlers share the same choreography — fast-path check,
// atomic reserve, then finalize-on-success / release-on-error — around their own
// (differently-ordered) auth + prep steps. These helpers factor out the
// repetitive per-step bodies WITHOUT moving the ordering: each handler still
// calls fast-path → reserve → service → finalize/release in sequence, so the
// NON-NEGOTIABLE ordering (commit c151ea4) stays visible at the call site. A
// `None` key makes every helper a no-op (idempotency is opt-in per request).

/// Fast-path: return `Some(prior result)` (decoded, `fallback` on decode error)
/// when this key already completed, so the caller can short-circuit; `None` =
/// proceed with the work.
pub(crate) async fn idem_fast_path<T: serde::de::DeserializeOwned>(
    state: &AppState,
    idem_key: Option<&String>,
    fallback: T,
) -> Result<Option<T>, AppError> {
    if let Some(key) = idem_key
        && let Some(cached) = extractors::check_idempotency(state, key).await?
    {
        return Ok(Some(
            serde_json::from_value::<T>(cached).unwrap_or(fallback),
        ));
    }
    Ok(None)
}

/// Atomically reserve the key. `Ok(None)` = reserved, proceed; `Ok(Some(result))`
/// = a prior request already completed this key, short-circuit with it. Always
/// performs the reserve when the key is present (its INSERT-on-conflict is the
/// double-work TOCTOU guard).
pub(crate) async fn idem_reserve<T: serde::de::DeserializeOwned>(
    state: &AppState,
    idem_key: Option<&String>,
    saga_type: &str,
    repo_name: &str,
    fallback: T,
) -> Result<Option<T>, AppError> {
    if let Some(key) = idem_key
        && let extractors::IdempotencyReservation::Cached(cached) =
            extractors::reserve_idempotency(state, key, saga_type, repo_name).await?
    {
        return Ok(Some(
            serde_json::from_value::<T>(cached).unwrap_or(fallback),
        ));
    }
    Ok(None)
}

/// Cache the successful `response` under the key (no-op when the key is absent).
pub(crate) async fn idem_finalize<T: serde::Serialize>(
    state: &AppState,
    idem_key: Option<&String>,
    response: &T,
) -> Result<(), AppError> {
    if let Some(key) = idem_key {
        let result_json = serde_json::to_value(response)?;
        extractors::finalize_idempotency(state, key, &result_json).await?;
    }
    Ok(())
}

/// Best-effort release after the work failed to start (no-op when key absent).
pub(crate) async fn idem_release(state: &AppState, idem_key: Option<&String>) {
    if let Some(key) = idem_key {
        extractors::release_idempotency(state, key).await;
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// POST /api/v1/repos/:name/ingest — trigger ingestion pipeline
///
/// Idempotency ordering (NON-NEGOTIABLE, commit c151ea4):
///   1. check_idempotency  (fast-path cache hit)
///   2. reserve_idempotency  (atomic INSERT-on-conflict ← stays handler-side)
///   3. state.ingest_service.trigger_ingest()  (has_active_job-check + spawn)
///   4. finalize_idempotency / release_idempotency
pub(super) async fn trigger_ingest(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<IngestRequestBody>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;

    // Triggering a GitLab ingest writes the repo's data; require Developer+
    // access. (Non-GitLab sources have no GitLab owner to resolve against and
    // rely on require_auth.)
    if body.source == "gitlab" {
        require_gitlab_access(&state, &headers, &name, 30).await?;
    }

    // ── Idempotency: fast-path cache check ─────────────────────────────────
    let idem_key = extractors::extract_idempotency_key(&headers)?;
    let cached_fallback = || IngestResponse {
        job_id: String::new(),
        status: "completed".into(),
    };
    if let Some(cached) = idem_fast_path(&state, idem_key.as_ref(), cached_fallback()).await? {
        return Ok(Json(cached));
    }

    let user_token = extract_user_token(&state, &headers).await;

    // ── Idempotency: atomic reserve (before has_active_job-check + spawn) ──
    if let Some(cached) = idem_reserve(
        &state,
        idem_key.as_ref(),
        "trigger_ingest",
        &name,
        cached_fallback(),
    )
    .await?
    {
        return Ok(Json(cached));
    }

    // ── Service call (has_active_job-check + spawn + source upsert) ────────
    let job_id = match state
        .ingest_service
        .trigger_ingest(
            name.clone(),
            body.git_ref,
            body.source,
            body.path,
            user_token,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            idem_release(&state, idem_key.as_ref()).await;
            return Err(AppError::from(e));
        }
    };

    let response = IngestResponse {
        job_id: job_id.to_string(),
        status: "pending".into(),
    };

    // ── Idempotency: finalize with the response ─────────────────────────────
    idem_finalize(&state, idem_key.as_ref(), &response).await?;

    let _ = state
        .event_tx
        .send(super::super::events::AppEvent::ReposChanged);

    Ok(Json(response))
}

/// POST /api/v1/sources/add — add a new source (website crawl).
///
/// Idempotency ordering preserved (same pattern as trigger_ingest):
///   reserve → service.add_source() → finalize/release
pub(super) async fn add_source(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AddSourceRequest>,
) -> Result<impl IntoResponse, AppError> {
    let idem_key = extractors::extract_idempotency_key(&headers)?;

    // Fast-path cache hit (before repo_name is known — key is URL-based here)
    if let Some(cached) = idem_fast_path(
        &state,
        idem_key.as_ref(),
        AddSourceResponse {
            job_id: String::new(),
            repo_name: String::new(),
            status: String::new(),
        },
    )
    .await?
    {
        return Ok(Json(cached));
    }

    // Derive repo_name from URL for the idempotency reserve key. NOTE: kept
    // inline (not shared with the service's derivation) on purpose — this side
    // is infallible best-effort (a bad URL falls back to the raw string as the
    // key), whereas the service validates and rejects with two distinct 400s
    // and also needs the https-prefixed URL for probe/seed.
    let url_for_reserve = if !body.url.starts_with("http://") && !body.url.starts_with("https://") {
        format!("https://{}", body.url)
    } else {
        body.url.clone()
    };
    let reserve_context = reqwest::Url::parse(&url_for_reserve)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_string();
            let seg = u
                .path_segments()
                .and_then(|mut s| s.find(|p| !p.is_empty()))
                .map(String::from);
            Some(match seg {
                Some(s) => format!("{host}/{s}"),
                None => host,
            })
        })
        .unwrap_or_else(|| body.url.clone());

    // ── Atomic reserve ──────────────────────────────────────────────────────
    if let Some(cached) = idem_reserve(
        &state,
        idem_key.as_ref(),
        "add_source",
        &reserve_context,
        AddSourceResponse {
            job_id: String::new(),
            repo_name: reserve_context.clone(),
            status: String::new(),
        },
    )
    .await?
    {
        return Ok(Json(cached));
    }

    // Resolve submitter (unauthenticated callers get an empty string; the gate
    // in Task 3 will hold the source in pending_review until an admin approves).
    let submitter = resolve_username(&state, &headers).await.unwrap_or_default();

    // ── Service call ────────────────────────────────────────────────────────
    let result = match state
        .ingest_service
        .add_source(
            body.source_type,
            body.url,
            body.crawl_depth,
            body.url_pattern,
            submitter,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            idem_release(&state, idem_key.as_ref()).await;
            return Err(AppError::from(e));
        }
    };

    let response = AddSourceResponse {
        job_id: result.job_id,
        repo_name: result.repo_name,
        status: result.status,
    };

    let _ = state
        .event_tx
        .send(super::super::events::AppEvent::ReposChanged);

    idem_finalize(&state, idem_key.as_ref(), &response).await?;

    Ok(Json(response))
}

/// GET /api/v1/repos/:name/ingest/status — latest job status
pub(super) async fn ingest_status(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let status = state
        .ingest_service
        .ingest_status(name)
        .await
        .map_err(AppError::from)?;

    Ok(Json(JobStatus {
        id: status.job_id,
        repo_name: status.repo_name,
        git_ref: status.git_ref,
        status: status.status,
        total_files: status.total_files,
        processed_files: status.processed_files,
        total_chunks: status.total_chunks,
        error_message: status.error_message,
        started_at: status.started_at,
        completed_at: status.completed_at,
    }))
}

/// POST /api/v1/repos/{name}/reingest — re-ingest using stored config
///
/// Idempotency ordering preserved:
///   check → reserve → service.reingest() (has_active_job + spawn) → finalize/release
pub(super) async fn reingest(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ReingestQuery>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;

    // Fast-path cache check
    let idem_key = extractors::extract_idempotency_key(&headers)?;
    let cached_fallback = || IngestResponse {
        job_id: String::new(),
        status: "completed".into(),
    };
    if let Some(cached) = idem_fast_path(&state, idem_key.as_ref(), cached_fallback()).await? {
        return Ok(Json(cached));
    }

    // Per-repo authorization (handler-side guard, BEFORE reserve + service call).
    // Reingesting a GitLab-sourced repo overwrites its content via clean_old_data,
    // so require Developer+ (level 30) access to that specific repo. Website/local
    // repos have no GitLab owner and rely on require_auth (authentication).
    // Mirrors the dropped check from the pre-thinning handler (HEAD~1).
    let (source_type, _from_db) = resolve_source_type(&state, &name).await?;
    if source_type == "gitlab" {
        require_gitlab_access(&state, &headers, &name, 30).await?;
    }

    let user_token = extract_user_token(&state, &headers).await;

    // Atomic reserve (before service call — preserves reserve→check→spawn order)
    if let Some(cached) = idem_reserve(
        &state,
        idem_key.as_ref(),
        "reingest",
        &name,
        cached_fallback(),
    )
    .await?
    {
        return Ok(Json(cached));
    }

    let job_id = match state
        .ingest_service
        .reingest(name.clone(), q.branch, user_token)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            idem_release(&state, idem_key.as_ref()).await;
            return Err(AppError::from(e));
        }
    };

    let response = IngestResponse {
        job_id: job_id.to_string(),
        status: "pending".into(),
    };
    idem_finalize(&state, idem_key.as_ref(), &response).await?;

    Ok(Json(response))
}

/// POST /api/v1/repos/{name}/resume — resume most recent failed job with checkpoint
///
/// Idempotency ordering preserved:
///   check → reserve → service.resume_ingest() → finalize/release
pub(super) async fn resume_ingest(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;

    // Fast-path cache check
    let idem_key = extractors::extract_idempotency_key(&headers)?;
    let cached_fallback = || IngestResponse {
        job_id: String::new(),
        status: "completed".into(),
    };
    if let Some(cached) = idem_fast_path(&state, idem_key.as_ref(), cached_fallback()).await? {
        return Ok(Json(cached));
    }

    // Per-repo authorization (handler-side guard, BEFORE reserve + service call).
    // Resuming a GitLab-sourced ingest re-runs writes, so require Developer+
    // (level 30) access to that specific repo. The pre-thinning handler (HEAD~1)
    // gated on `sources.source_type == Some("gitlab")` with NO name-based
    // fallback (the source_type came straight from the sources subquery), so we
    // only gate when the sources row exists in the DB and is gitlab.
    let (source_type, from_db) = resolve_source_type(&state, &name).await?;
    if from_db && source_type == "gitlab" {
        require_gitlab_access(&state, &headers, &name, 30).await?;
    }

    let user_token = extract_user_token(&state, &headers).await;

    // Atomic reserve (before service call — preserves reserve→spawn order)
    if let Some(cached) = idem_reserve(
        &state,
        idem_key.as_ref(),
        "resume_ingest",
        &name,
        cached_fallback(),
    )
    .await?
    {
        return Ok(Json(cached));
    }

    let job_id = match state
        .ingest_service
        .resume_ingest(name.clone(), user_token)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            idem_release(&state, idem_key.as_ref()).await;
            return Err(AppError::from(e));
        }
    };

    let response = IngestResponse {
        job_id: job_id.to_string(),
        status: "pending".into(),
    };
    idem_finalize(&state, idem_key.as_ref(), &response).await?;

    Ok(Json(response))
}

/// GET /api/v1/jobs/active — repos with in-progress ingestion
pub(super) async fn active_jobs(
    State(state): State<AppState>,
    Query(page): Query<extractors::PaginationParams>,
) -> Result<impl IntoResponse, AppError> {
    let limit = page.clamped_limit(100);
    // Finding (ingestion.rs:600): offset is wired from PaginationParams cursor
    // so paging past page 1 works (was previously ignored).
    let offset = page.offset();
    let items = state.ingest_service.active_jobs(limit, offset).await?;

    let jobs: Vec<ActiveJob> = items
        .into_iter()
        .map(|item| ActiveJob {
            repo_name: item.repo_name,
            status: item.status,
            processed_files: item.processed_files,
            total_files: item.total_files,
        })
        .collect();

    Ok(Json(jobs))
}

/// GET /api/v1/sources/overview — dashboard overview of all sources
pub(super) async fn sources_overview(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let overview = state.ingest_service.sources_overview().await?;

    let sources: Vec<SourceOverview> = overview
        .sources
        .into_iter()
        .map(|item| SourceOverview {
            name: item.name,
            source_type: item.source_type,
            status: item.status,
            branch: item.branch,
            chunk_count: item.chunk_count,
            module_count: item.module_count,
            note_count: item.note_count,
            section_count: item.section_count,
            last_synced_at: item.last_synced_at,
            active_job: item.active_job.map(|aj| ActiveJobInfo {
                job_id: String::new(), // not available in ActiveJobItem (was always empty)
                status: aj.status,
                processed_files: aj.processed_files,
                total_files: aj.total_files,
            }),
            last_error: item.last_error,
            can_resume: item.can_resume,
        })
        .collect();

    Ok(Json(SourcesOverviewResponse {
        total: overview.total,
        healthy: overview.healthy,
        stale: overview.stale,
        failed: overview.failed,
        ingesting: overview.ingesting,
        sources,
    }))
}

/// GET /api/v1/sources/pending — list sources awaiting admin review (admin only)
pub(super) async fn pending_sources(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    super::require_admin(&state, &headers).await?;
    let items = state.ingest_service.list_pending_sources().await?;
    Ok(Json(items))
}

/// GET /api/v1/sources/demand — list adapter-demand backlog (admin only)
pub(super) async fn demand_sources(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    super::require_admin(&state, &headers).await?;
    let items = state.ingest_service.list_demand().await?;
    Ok(Json(items))
}

/// POST /api/v1/sources/{id}/approve — approve a pending source (admin only)
pub(super) async fn approve_source(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let admin = super::require_admin(&state, &headers).await?;
    let res = state.ingest_service.approve_source(id, admin).await?;
    let _ = state
        .event_tx
        .send(super::super::events::AppEvent::ReposChanged);
    Ok(Json(AddSourceResponse {
        job_id: res.job_id,
        repo_name: res.repo_name,
        status: res.status,
    }))
}

/// POST /api/v1/sources/{id}/reject — reject a pending source (admin only)
pub(super) async fn reject_source(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let admin = super::require_admin(&state, &headers).await?;
    state.ingest_service.reject_source(id, admin).await?;
    let _ = state
        .event_tx
        .send(super::super::events::AppEvent::ReposChanged);
    Ok(Json(serde_json::json!({ "status": "rejected" })))
}

/// GET /api/v1/gitlab/branches?repo=acme/widget-factory
/// Proxy to GitLab API using the user's OAuth token from session.
pub(super) async fn gitlab_branches(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<GitLabBranchesQuery>,
) -> Result<impl IntoResponse, AppError> {
    let gitlab_token = extract_user_token(&state, &headers)
        .await
        .ok_or(AppError::Unauthorized)?;

    let branches = state
        .repo_service
        .gitlab_branches(params.repo, gitlab_token)
        .await?;

    // Map domain GitLabBranch → local GitLabBranchItem for response serialization
    let items: Vec<GitLabBranchItem> = branches
        .into_iter()
        .map(|b| GitLabBranchItem {
            name: b.name,
            default: b.default,
        })
        .collect();

    Ok(Json(items))
}

#[cfg(test)]
mod tests {
    use super::*;
    // The REAL implementation the production reingest path uses (it used to be
    // asserted via a local #[cfg(test)] copy that could drift).
    use akashic_domain::algos::resolve_reingest_git_ref;

    // Finding #39: reingest must not default git_ref to "" for gitlab repos.
    #[test]
    fn query_branch_takes_priority() {
        let r = resolve_reingest_git_ref(Some("main".into()), Some("dev".into()));
        assert_eq!(r, "main");
    }

    #[test]
    fn falls_back_to_last_job_ref_when_query_absent() {
        let r = resolve_reingest_git_ref(None, Some("dev".into()));
        assert_eq!(r, "dev");
    }

    #[test]
    fn blank_query_branch_is_treated_as_absent() {
        // Frontend sends "" when the user doesn't pick a branch; it must not
        // shadow the recovered prior-job ref (would make builder.branch("") fail).
        assert_eq!(
            resolve_reingest_git_ref(Some(String::new()), Some("release-1.0".into())),
            "release-1.0"
        );
        assert_eq!(
            resolve_reingest_git_ref(Some("   ".into()), Some("release-1.0".into())),
            "release-1.0"
        );
    }

    #[test]
    fn blank_last_job_ref_does_not_win() {
        assert_eq!(
            resolve_reingest_git_ref(None, Some("  ".into())),
            String::new()
        );
    }

    #[test]
    fn empty_when_no_ref_available_for_non_clone_sources() {
        // Website/local sources never clone, so an empty ref is acceptable here.
        assert_eq!(resolve_reingest_git_ref(None, None), String::new());
    }

    // Finding (ingestion.rs:600): active_jobs now binds page.offset() to OFFSET,
    // so paging past page 1 must actually shift the window. These assertions lock
    // in the pagination contract the SQL binding relies on: an absent cursor must
    // yield offset 0 (page 1, unchanged behaviour) and a real cursor must decode
    // to its encoded offset (so OFFSET $2 advances). Pure logic — no DB needed.
    #[test]
    fn active_jobs_offset_is_zero_without_cursor() {
        let p = extractors::PaginationParams {
            limit: 100,
            cursor: None,
        };
        assert_eq!(p.offset(), 0);
        // clamped_limit also bounds the page size that maps to LIMIT $1.
        assert_eq!(p.clamped_limit(100), 100);
        assert_eq!(p.clamped_limit(50), 50);
    }

    #[test]
    fn active_jobs_offset_advances_with_cursor() {
        // A cursor encodes {"offset":N} url-encoded, exactly as Paginated emits.
        let encoded = urlencoding::encode(r#"{"offset":100}"#).into_owned();
        let p = extractors::PaginationParams {
            limit: 100,
            cursor: Some(encoded),
        };
        // Without the new OFFSET binding this value was computed but never used,
        // silently pinning every request to the first page.
        assert_eq!(p.offset(), 100);
    }
}
