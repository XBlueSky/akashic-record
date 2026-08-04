//! Plan-3 Task 5 (E1/E2/E4/E7): AI-surface text endpoints generated on the
//! fly from the raw corpus — per-repo `llms.txt` / `llms-full.txt` /
//! `skill.md` plus the global `/llms.txt` repo index. Nothing is persisted, so
//! the output can never drift from the corpus.
//!
//! Always **latest** semantics: the repo-scoped endpoints resolve the
//! `latest` version, carry `ETag = "{sha}"` and `Cache-Control: no-cache`
//! (D5's `latest` strategy), and honor `If-None-Match` with a 304. Exact
//! versions travel inside the body stamp, not the URL.
//!
//! `llms-full.txt` is assembled as one in-memory `String` rather than a
//! streamed body — a deliberate simplification of E2's "streaming" note:
//! corpus size is already bounded by the A2 ingest cap (artifact ≤ 64 MiB,
//! markdown subset far smaller), so buffering is fine and keeps the
//! ETag/304 path trivial.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};

use akashic_context::AppState;
use akashic_domain::algos::llms::{
    LlmsPage, order_pages, render_global_llms_txt, render_llms_full, render_llms_txt,
    render_skill_md,
};

use super::super::error::AppError;
use super::super::extractors;
use super::docs_read::{
    M_DOCS_READ_TOTAL, apply_cache_headers, etag_value, if_none_match_satisfied,
    not_modified_response, resolve_version_or_404,
};

/// GET /docs/:repo/llms.txt
pub(super) async fn repo_llms_txt(
    State(state): State<AppState>,
    Path(repo): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "llms_txt").increment(1);
    extractors::validate_repo_name(&repo)?;

    let meta = resolve_version_or_404(&state, &repo, "latest").await?;
    let etag = etag_value(&meta.sha);
    if if_none_match_satisfied(&headers, &etag) {
        return Ok(not_modified_response(&etag, "latest"));
    }

    let nav = state
        .corpus_store
        .get_nav(meta.id)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;
    let body = render_llms_txt(
        &repo,
        &meta.version,
        &meta.sha,
        &nav,
        &meta.index_path,
        &state.config.public_base_url,
    );
    Ok(text_response(body, "text/plain; charset=utf-8", &etag))
}

/// GET /docs/:repo/llms-full.txt
pub(super) async fn repo_llms_full(
    State(state): State<AppState>,
    Path(repo): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "llms_full").increment(1);
    extractors::validate_repo_name(&repo)?;

    let meta = resolve_version_or_404(&state, &repo, "latest").await?;
    let etag = etag_value(&meta.sha);
    if if_none_match_satisfied(&headers, &etag) {
        return Ok(not_modified_response(&etag, "latest"));
    }

    let nav = state
        .corpus_store
        .get_nav(meta.id)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;
    let md_paths = state
        .corpus_store
        .list_md_paths(meta.id)
        .await
        .map_err(AppError::Internal)?;

    let ordered = order_pages(&nav, &md_paths, &meta.index_path);
    let mut pages = Vec::with_capacity(ordered.len());
    for path in ordered {
        let file = state
            .corpus_store
            .get_file(meta.id, &path)
            .await
            .map_err(AppError::Internal)?
            .ok_or(AppError::NotFound)?;
        let markdown = String::from_utf8(file.content).map_err(|e| AppError::Internal(e.into()))?;
        pages.push(LlmsPage { path, markdown });
    }

    let body = render_llms_full(
        &repo,
        &meta.version,
        &meta.sha,
        &pages,
        &state.config.public_base_url,
    );
    Ok(text_response(body, "text/plain; charset=utf-8", &etag))
}

/// GET /docs/:repo/skill.md
pub(super) async fn repo_skill_md(
    State(state): State<AppState>,
    Path(repo): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "skill_md").increment(1);
    extractors::validate_repo_name(&repo)?;

    let meta = resolve_version_or_404(&state, &repo, "latest").await?;
    let etag = etag_value(&meta.sha);
    if if_none_match_satisfied(&headers, &etag) {
        return Ok(not_modified_response(&etag, "latest"));
    }

    let nav = state
        .corpus_store
        .get_nav(meta.id)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;
    let body = render_skill_md(
        &repo,
        &meta.version,
        &meta.sha,
        &nav,
        &meta.index_path,
        &state.config.public_base_url,
    );
    Ok(text_response(body, "text/markdown; charset=utf-8", &etag))
}

/// GET /llms.txt — global index of every published repo.
///
/// No ETag: there is no single corpus sha covering the whole list, and the
/// body is tiny — `no-cache` alone is the whole caching story here.
pub(super) async fn global_llms_txt(State(state): State<AppState>) -> Result<Response, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "llms_global").increment(1);

    let summaries = state
        .corpus_store
        .list_repos()
        .await
        .map_err(AppError::Internal)?;
    let mut repos = Vec::with_capacity(summaries.len());
    for s in summaries {
        let description = state
            .corpus_store
            .get_nav(s.latest.id)
            .await
            .map_err(AppError::Internal)?
            .map(|n| n.description)
            .unwrap_or_default();
        repos.push((s.repo_name, description));
    }

    let body = render_global_llms_txt(&repos, &state.config.public_base_url);
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(resp)
}

/// Text body + content-type + the D5 `latest` cache headers (ETag,
/// `no-cache`).
fn text_response(body: String, content_type: &'static str, etag: &str) -> Response {
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    apply_cache_headers(h, etag, "latest");
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_response_sets_content_type_etag_and_no_cache() {
        let resp = text_response("hi".into(), "text/plain; charset=utf-8", "\"abc\"");
        assert_eq!(resp.headers()["content-type"], "text/plain; charset=utf-8");
        assert_eq!(resp.headers()["etag"], "\"abc\"");
        assert_eq!(resp.headers()["cache-control"], "no-cache");
    }
}
