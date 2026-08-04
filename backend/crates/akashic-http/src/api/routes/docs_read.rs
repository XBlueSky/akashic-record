//! Task 10 (C3, docs-corpus): public read-only HTTP API over the stored
//! corpus — `GET /api/v1/docs/*`. This is what the frontend docs area (and,
//! later, `llms.txt`) reads. All five endpoints are unauthenticated
//! (registered in `public_router`, mirroring `repos`/`graph::doc`).
//!
//! ## Caching (D5)
//!
//! `nav`, `page`, and `raw` all carry an `ETag` (the resolved version's
//! `sha`, quoted) and honor `If-None-Match` with a `304`. `Cache-Control` is
//! `no-cache` when the caller's `{version}` selector was the literal
//! `"latest"` (it can move), or `public, max-age=31536000, immutable` when
//! it was an exact version or sha (pinned — that resolution can never
//! change).
//!
//! ## Ambiguous sha-prefix selectors
//!
//! `CorpusStore::resolve_version` resolves a sha-prefix selector that
//! matches multiple versions deterministically (earliest-ingested wins) — see
//! its doc comment. This module does not add a `409 Ambiguous` on top of
//! that; the deterministic pick is used as-is.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uuid::Uuid;

use akashic_context::AppState;
use akashic_domain::types::corpus::{CorpusVersionMeta, NavTree};

use super::super::error::AppError;
use super::super::extractors;

/// Task 11 (C5): per-endpoint read counter, labeled `endpoint` (one of
/// `list`/`versions`/`nav`/`page`/`raw`). `akashic_`-prefixed to match every
/// other custom counter in this codebase — see `docs_publish::M_DOCS_PUBLISH_TOTAL`'s
/// doc comment for the same rationale.
pub(super) const M_DOCS_READ_TOTAL: &str = "akashic_docs_read_total";

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct DocsRepoEntry {
    repo: String,
    version: String,
    sha: String,
    page_count: Option<i32>,
    ingested_at: chrono::DateTime<chrono::Utc>,
    derive_status: String,
    description: String,
    index: String,
}

#[derive(Serialize)]
struct DocsRepoListResponse {
    repos: Vec<DocsRepoEntry>,
}

#[derive(Serialize)]
struct DocsVersionEntry {
    version: String,
    sha: String,
    is_tagged: bool,
    is_latest: bool,
    ingested_at: chrono::DateTime<chrono::Utc>,
    derive_status: String,
    index: String,
}

#[derive(Serialize)]
struct PageResponse {
    markdown: String,
    title: String,
    stamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_id: Option<Uuid>,
    /// Per-page EXPLAINS summary. Deliberately left absent for now: wiring it
    /// would mean either reducing `GraphService::get_document_detail`'s
    /// per-section explains into a flat page-level list, or a new batched
    /// query — deferred past Task 10 (see the Task 10 report). `document_id`
    /// above already works once derive completes; a frontend that wants
    /// EXPLAINS detail can follow up with
    /// `GET /api/v1/documents/:document_id/sections`.
    #[serde(skip_serializing_if = "Option::is_none")]
    explains: Option<Vec<serde_json::Value>>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/docs
pub(super) async fn list_docs_repos(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "list").increment(1);
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
        repos.push(DocsRepoEntry {
            repo: s.repo_name,
            version: s.latest.version,
            sha: s.latest.sha,
            page_count: s.latest.page_count,
            ingested_at: s.latest.ingested_at,
            derive_status: s.latest.derive_status.as_str().to_string(),
            description,
            index: s.latest.index_path.clone(),
        });
    }

    Ok(Json(DocsRepoListResponse { repos }))
}

/// GET /api/v1/docs/:repo
pub(super) async fn list_docs_versions(
    State(state): State<AppState>,
    Path(repo): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "versions").increment(1);
    extractors::validate_repo_name(&repo)?;

    let versions = state
        .corpus_store
        .list_versions(&repo)
        .await
        .map_err(AppError::Internal)?;

    let out: Vec<DocsVersionEntry> = versions
        .into_iter()
        .map(|v| DocsVersionEntry {
            version: v.version,
            sha: v.sha,
            is_tagged: v.is_tagged,
            is_latest: v.is_latest,
            ingested_at: v.ingested_at,
            derive_status: v.derive_status.as_str().to_string(),
            index: v.index_path,
        })
        .collect();

    Ok(Json(out))
}

/// GET /api/v1/docs/:repo/:version/nav
pub(super) async fn get_nav(
    State(state): State<AppState>,
    Path((repo, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "nav").increment(1);
    extractors::validate_repo_name(&repo)?;

    let meta = resolve_version_or_404(&state, &repo, &version).await?;
    let etag = etag_value(&meta.sha);
    if if_none_match_satisfied(&headers, &etag) {
        return Ok(not_modified_response(&etag, &version));
    }

    let nav = state
        .corpus_store
        .get_nav(meta.id)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;

    let mut resp = Json(nav).into_response();
    apply_cache_headers(resp.headers_mut(), &etag, &version);
    Ok(resp)
}

/// GET /api/v1/docs/:repo/:version/page/*path
pub(super) async fn get_page(
    State(state): State<AppState>,
    Path((repo, version, path)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "page").increment(1);
    extractors::validate_repo_name(&repo)?;
    validate_corpus_path(&path)?;

    let meta = resolve_version_or_404(&state, &repo, &version).await?;
    let etag = etag_value(&meta.sha);
    if if_none_match_satisfied(&headers, &etag) {
        return Ok(not_modified_response(&etag, &version));
    }

    let file = state
        .corpus_store
        .get_file(meta.id, &path)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;
    let markdown = String::from_utf8(file.content).map_err(|e| AppError::Internal(e.into()))?;

    let nav = state
        .corpus_store
        .get_nav(meta.id)
        .await
        .map_err(AppError::Internal)?;
    let title = nav
        .as_ref()
        .and_then(|n| find_nav_page_title(n, &path, &meta.index_path))
        .unwrap_or_else(|| path_stem(&path));

    let stamp = stamp_for(&repo, &meta);

    // document_id resolves once Task 8/9's derive job has created the
    // `doc_type = "corpus"` document for this (repo, path) — see
    // `GraphService::find_corpus_document_id`'s doc comment. `Ok(None)`
    // (derive pending/running/failed, or this page not yet derived) simply
    // omits the field, per the endpoint's `document_id?` contract.
    let document_id = state
        .graph_service
        .find_corpus_document_id(repo.clone(), path.clone())
        .await
        .map_err(AppError::from)?;

    let mut resp = Json(PageResponse {
        markdown,
        title,
        stamp,
        document_id,
        explains: None,
    })
    .into_response();
    apply_cache_headers(resp.headers_mut(), &etag, &version);
    Ok(resp)
}

/// GET /api/v1/docs/:repo/:version/raw/*path
pub(super) async fn get_raw(
    State(state): State<AppState>,
    Path((repo, version, path)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    metrics::counter!(M_DOCS_READ_TOTAL, "endpoint" => "raw").increment(1);
    extractors::validate_repo_name(&repo)?;
    validate_corpus_path(&path)?;

    let meta = resolve_version_or_404(&state, &repo, &version).await?;
    let etag = etag_value(&meta.sha);
    if if_none_match_satisfied(&headers, &etag) {
        return Ok(not_modified_response(&etag, &version));
    }

    let file = state
        .corpus_store
        .get_file(meta.id, &path)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;
    let stamp = stamp_for(&repo, &meta);

    let mut resp = file.content.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for(&path)),
    );
    h.insert(
        "x-akashic-docs-stamp",
        HeaderValue::from_str(&stamp).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    apply_cache_headers(h, &etag, &version);
    Ok(resp)
}

// ── Shared helpers ──────────────────────────────────────────────────────────

pub(super) async fn resolve_version_or_404(
    state: &AppState,
    repo: &str,
    selector: &str,
) -> Result<CorpusVersionMeta, AppError> {
    state
        .corpus_store
        .resolve_version(repo, selector)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)
}

/// Reject a `{path...}` segment that could be misread as a filesystem escape.
/// `CorpusStore::get_file` is a DB key lookup (`WHERE path = $2`, no
/// filesystem access), so `..` can't actually traverse anything here — this
/// is defense in depth against a client-side / proxy-cache confusion, not a
/// path-traversal fix.
fn validate_corpus_path(path: &str) -> Result<(), AppError> {
    if path.contains("..") || path.starts_with('/') {
        return Err(AppError::BadRequest(
            "path must not contain '..' or start with '/'".into(),
        ));
    }
    Ok(())
}

/// `path` is a full corpus key; `index_path` is `manifest.index`, which nav's
/// index-relative page paths are resolved against before matching.
fn find_nav_page_title(nav: &NavTree, path: &str, index_path: &str) -> Option<String> {
    akashic_domain::algos::llms::nav_page_title(nav, path, index_path)
}

fn path_stem(path: &str) -> String {
    akashic_domain::algos::llms::path_stem(path)
}

fn stamp_for(repo: &str, meta: &CorpusVersionMeta) -> String {
    akashic_domain::algos::llms::stamp(repo, &meta.version, &meta.sha)
}

fn content_type_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "md" => "text/markdown; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

pub(super) fn etag_value(sha: &str) -> String {
    format!("\"{sha}\"")
}

pub(super) fn if_none_match_satisfied(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| value.split(',').any(|part| part.trim() == etag))
}

/// `"latest"` moves, so it must always be revalidated; an exact version or
/// sha selector is a pinned, immutable resolution.
fn cache_control_for(selector: &str) -> &'static str {
    if selector == "latest" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    }
}

pub(super) fn apply_cache_headers(headers: &mut HeaderMap, etag: &str, selector: &str) {
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).unwrap_or_else(|_| HeaderValue::from_static("\"\"")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for(selector)),
    );
}

pub(super) fn not_modified_response(etag: &str, selector: &str) -> Response {
    let mut resp = Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .body(axum::body::Body::empty())
        .expect("a 304 with only header fields set is always a valid response");
    apply_cache_headers(resp.headers_mut(), etag, selector);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use akashic_domain::types::corpus::{NavGroup, NavPage};

    fn sample_nav() -> NavTree {
        NavTree {
            description: "desc".into(),
            groups: vec![NavGroup {
                title: "Guide".into(),
                pages: vec![NavPage {
                    title: "Setup".into(),
                    path: "guide/setup.md".into(),
                    description: "Install".into(),
                }],
            }],
        }
    }

    #[test]
    fn find_nav_page_title_matches_exact_path() {
        let nav = sample_nav();
        assert_eq!(
            find_nav_page_title(&nav, "guide/setup.md", "index.md"),
            Some("Setup".to_string())
        );
        assert_eq!(
            find_nav_page_title(&nav, "guide/missing.md", "index.md"),
            None
        );
    }

    /// Pull-bootstrapped corpus: file keys carry the `docs_root/` prefix while
    /// nav paths stay index-relative, so the title lookup only matches once
    /// the nav path is resolved against `manifest.index`.
    #[test]
    fn find_nav_page_title_matches_nested_index_corpus() {
        let nav = sample_nav();
        assert_eq!(
            find_nav_page_title(&nav, "docs/guide/setup.md", "docs/README.md"),
            Some("Setup".to_string())
        );
        assert_eq!(
            find_nav_page_title(&nav, "guide/setup.md", "docs/README.md"),
            None
        );
    }

    #[test]
    fn path_stem_strips_dir_and_extension() {
        assert_eq!(path_stem("guide/setup.md"), "setup");
        assert_eq!(path_stem("index.md"), "index");
    }

    #[test]
    fn content_type_for_maps_known_extensions() {
        assert_eq!(content_type_for("a.md"), "text/markdown; charset=utf-8");
        assert_eq!(content_type_for("a.png"), "image/png");
        assert_eq!(content_type_for("a.SVG"), "image/svg+xml");
        assert_eq!(content_type_for("a.jpeg"), "image/jpeg");
        assert_eq!(content_type_for("a.gif"), "image/gif");
        assert_eq!(content_type_for("a.bin"), "application/octet-stream");
        assert_eq!(content_type_for("noext"), "application/octet-stream");
    }

    #[test]
    fn validate_corpus_path_rejects_dotdot_and_leading_slash() {
        assert!(validate_corpus_path("guide/setup.md").is_ok());
        assert!(validate_corpus_path("../etc/passwd").is_err());
        assert!(validate_corpus_path("/etc/passwd").is_err());
    }

    #[test]
    fn cache_control_differs_for_latest_vs_pinned() {
        assert_eq!(cache_control_for("latest"), "no-cache");
        assert_eq!(
            cache_control_for("1.0.0"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control_for("deadbeef"),
            "public, max-age=31536000, immutable"
        );
    }

    #[test]
    fn stamp_for_truncates_sha_to_seven_chars() {
        let meta = CorpusVersionMeta {
            id: Uuid::nil(),
            repo_name: "acme".into(),
            version: "1.0.0".into(),
            sha: "0123456789abcdef".into(),
            is_latest: true,
            is_tagged: false,
            derive_status: akashic_domain::types::corpus::DeriveStatus::Complete,
            ingested_at: chrono::Utc::now(),
            page_count: Some(1),
            asset_count: Some(0),
            index_path: "index.md".into(),
        };
        assert_eq!(stamp_for("acme", &meta), "documents acme 1.0.0 @ 0123456");
    }
}
