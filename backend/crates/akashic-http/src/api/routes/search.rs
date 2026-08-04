use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use akashic_context::AppState;
use akashic_domain::types::{GraphRagQuery, KnowledgeSearchRequest};

use super::super::error::AppError;
use super::super::extractors;

// ── Request / Response types ────────────────────────────────────────────────

fn default_limit() -> i64 {
    20
}

/// Max number of ids accepted by the /details endpoint in a single request.
///
/// This endpoint is public and unauthenticated; without a cap a single
/// request could carry hundreds of thousands of comma-separated ids and
/// fan out into a matching number of serialized DB round-trips (DoS).
/// We cap the parsed/deduped ids at this count and ignore the rest.
const MAX_DETAIL_IDS: usize = 100;

/// Parse a comma-separated `ids` string into ordered, deduplicated UUIDs,
/// capped at [`MAX_DETAIL_IDS`].
///
/// Returns the parsed UUIDs in first-seen input order (so the response can be
/// emitted in request order) together with the original string for each id so
/// the not-found fallback can echo what the caller actually sent. Blank
/// segments (e.g. trailing commas) are skipped. Once the cap is reached the
/// remaining segments are dropped to bound the DB fan-out.
///
/// Pure (no DB / IO) so it is unit-testable without a live database.
fn parse_detail_ids(ids: &str) -> Result<Vec<(uuid::Uuid, String)>, String> {
    let mut out: Vec<(uuid::Uuid, String)> = Vec::new();
    for raw in ids.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let id: uuid::Uuid = trimmed
            .parse()
            .map_err(|_| format!("Invalid UUID: {trimmed}"))?;
        // Dedup so repeated ids do not waste a slot under the cap.
        if out.iter().any(|(seen, _)| *seen == id) {
            continue;
        }
        out.push((id, trimmed.to_string()));
        if out.len() >= MAX_DETAIL_IDS {
            break;
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    q: String,
    repo: Option<String>,
    layer: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}

#[derive(Serialize)]
struct SearchDocsRef {
    repo: String,
    path: String,
    anchor: String,
}

#[derive(Serialize)]
struct SearchResultItem {
    id: String,
    layer: String,
    #[serde(rename = "type")]
    result_type: String,
    name: String,
    repo_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    docs: Option<SearchDocsRef>,
}

#[derive(Deserialize)]
pub(super) struct DetailsQuery {
    #[serde(default)]
    ids: Option<String>,
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Serialize)]
struct RelinkResponse {
    status: String,
    sections_processed: usize,
    edges_created_exact: usize,
    edges_created_llm: usize,
    target_repos: Vec<String>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/search?q=&repo=&layer=&limit= — compact search endpoint (Scout format)
pub(super) async fn unified_search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_search_query(&params.q)?;
    if let Some(ref repo) = params.repo {
        extractors::validate_repo_name(repo)?;
    }

    // The dual-level vector search (embed → chunks/notes/sections → merge →
    // sort → truncate, with the public-endpoint limit clamp) lives in
    // `SearchService::search_knowledge`. The handler only maps the domain hits
    // onto the public Scout wire shape (`type` rename + omit null
    // module/summary, preserved by `SearchResultItem`).
    let items = state
        .search_service
        .search_knowledge(KnowledgeSearchRequest {
            query: params.q,
            repo: params.repo,
            layer: params.layer,
            limit: params.limit,
        })
        .await?;

    let results: Vec<SearchResultItem> = items
        .into_iter()
        .map(|i| SearchResultItem {
            id: i.id,
            layer: i.layer,
            result_type: i.result_type,
            name: i.name,
            repo_name: i.repo_name,
            module: i.module,
            summary: i.summary,
            score: i.score,
            docs: i.docs.map(|d| SearchDocsRef {
                repo: d.repo,
                path: d.path,
                anchor: d.anchor,
            }),
        })
        .collect();

    Ok(Json(results))
}

/// POST /api/v1/graphrag/query — multi-space graph-aware retrieval
pub(super) async fn graphrag_query(
    State(state): State<AppState>,
    Json(req): Json<GraphRagQuery>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_search_query(&req.query)?;
    if let Some(ref repo) = req.repo_name {
        extractors::validate_repo_name(repo)?;
    }
    let response = state.search_service.graphrag_query(req).await?;
    Ok(Json(response))
}

/// GET /api/v1/details?ids=uuid1,uuid2 or ?repo=name — Sniper endpoint
pub(super) async fn details(
    State(state): State<AppState>,
    Query(params): Query<DetailsQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Mode 1: module-list mode — repo present, no ids.
    if params.ids.is_none()
        || params
            .ids
            .as_ref()
            .is_some_and(std::string::String::is_empty)
    {
        // A missing required query param is a client error (caller fault), not a
        // server fault. Build it as AppError::BadRequest (HTTP 400) instead of an
        // anyhow error, which the blanket `From<E>` impl would coerce into
        // AppError::Internal (HTTP 500).
        let repo = params.repo.clone().ok_or_else(|| {
            AppError::BadRequest("Either 'ids' or 'repo' query parameter required".into())
        })?;
        let modules = state
            .search_service
            .get_details(Vec::new(), Some(repo))
            .await?;
        return Ok(Json(serde_json::to_value(modules)?));
    }

    // Mode 2: fetch details for specific IDs.
    //
    // BUGFIX (misc-High DoS): the previous implementation split `ids` with no
    // cap and issued up to three sequential DB round-trips per id in a serial
    // loop, so one public request could fan out into hundreds of thousands of
    // serialized queries. We now cap the id count (MAX_DETAIL_IDS) and batch
    // each table lookup with `WHERE id = ANY($1)` — at most 3 queries total,
    // independent of N.
    // Mode 1 above returns early when `ids` is None/empty, so this is reached
    // only with `ids` present — but resolve defensively (BadRequest, not panic)
    // so a future refactor of the guard can't turn this into a 500/DoS.
    let ids_str = params
        .ids
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("'ids' query parameter required".into()))?;
    let ids = parse_detail_ids(ids_str).map_err(|e| anyhow::anyhow!(e))?;
    let id_vec: Vec<uuid::Uuid> = ids.iter().map(|(id, _)| *id).collect();

    // Delegate to service (collapses REST + MCP dup).
    let details = state
        .search_service
        .get_details(id_vec, params.repo)
        .await?;

    Ok(Json(serde_json::to_value(details)?))
}

/// POST /api/v1/repos/:name/relink-explains — re-run cross-repo EXPLAINS linking.
pub(super) async fn relink_explains(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;

    // Delegate EXPLAINS linking + target-repo collection to the search service,
    // then the section re-clustering to the ingest service (whose pipeline owns
    // pg/db/llm). Splitting across the two services keeps the handler off
    // AppState's pools while still avoiding a retrieval→ingestion crate cycle.
    let result = state
        .search_service
        .relink_explains(name.clone(), None)
        .await?;

    let cluster_count = match state
        .ingest_service
        .recluster_doc_sections(name.clone())
        .await
    {
        Ok(count) => count,
        Err(e) => {
            tracing::error!(err = %e, "Doc clustering failed");
            0
        }
    };

    tracing::info!(clusters = cluster_count, "Re-clustering complete");

    Ok(Json(RelinkResponse {
        status: "completed".into(),
        sections_processed: result.sections_processed,
        edges_created_exact: result.edges_created_exact,
        edges_created_llm: result.edges_created_llm,
        target_repos: result.target_repos,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_item_docs_field_skipped_when_none() {
        let item = SearchResultItem {
            id: "x".into(),
            layer: "DOC".into(),
            result_type: "section".into(),
            name: "Setup".into(),
            repo_name: "acme".into(),
            module: None,
            summary: None,
            score: 0.9,
            docs: None,
        };
        let json = serde_json::to_value(&item).unwrap();
        assert!(json.get("docs").is_none());
        let with = SearchResultItem {
            docs: Some(SearchDocsRef {
                repo: "acme".into(),
                path: "guide/setup.md".into(),
                anchor: "quick-start".into(),
            }),
            ..item
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["docs"]["path"], "guide/setup.md");
    }

    fn uuid_str(n: u128) -> String {
        uuid::Uuid::from_u128(n).to_string()
    }

    #[test]
    fn parses_and_preserves_order() {
        let a = uuid_str(1);
        let b = uuid_str(2);
        let input = format!("{a},{b}");
        let out = parse_detail_ids(&input).expect("valid uuids");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, uuid::Uuid::from_u128(1));
        assert_eq!(out[0].1, a);
        assert_eq!(out[1].0, uuid::Uuid::from_u128(2));
        assert_eq!(out[1].1, b);
    }

    #[test]
    fn trims_whitespace_and_skips_blank_segments() {
        let a = uuid_str(1);
        let b = uuid_str(2);
        // Leading/trailing spaces around ids and a trailing comma.
        let input = format!("  {a} , {b} ,");
        let out = parse_detail_ids(&input).expect("valid uuids");
        assert_eq!(out.len(), 2);
        // The echoed string is the trimmed form (no surrounding spaces).
        assert_eq!(out[0].1, a);
        assert_eq!(out[1].1, b);
    }

    #[test]
    fn deduplicates_repeated_ids_preserving_first_occurrence() {
        let a = uuid_str(1);
        let b = uuid_str(2);
        let input = format!("{a},{b},{a}");
        let out = parse_detail_ids(&input).expect("valid uuids");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, uuid::Uuid::from_u128(1));
        assert_eq!(out[1].0, uuid::Uuid::from_u128(2));
    }

    #[test]
    fn caps_at_max_detail_ids_and_drops_the_rest() {
        // Build MAX_DETAIL_IDS + 50 distinct uuids.
        let overflow = MAX_DETAIL_IDS + 50;
        let input = (1..=overflow as u128)
            .map(uuid_str)
            .collect::<Vec<_>>()
            .join(",");
        let out = parse_detail_ids(&input).expect("valid uuids");
        assert_eq!(out.len(), MAX_DETAIL_IDS);
        // The first MAX_DETAIL_IDS distinct ids are kept, in order.
        assert_eq!(out[0].0, uuid::Uuid::from_u128(1));
        assert_eq!(
            out[MAX_DETAIL_IDS - 1].0,
            uuid::Uuid::from_u128(MAX_DETAIL_IDS as u128)
        );
    }

    #[test]
    fn cap_counts_distinct_ids_not_raw_segments() {
        // A flood of the SAME id must not exhaust the cap; it dedups to one.
        let a = uuid_str(1);
        let input = std::iter::repeat_n(a.clone(), MAX_DETAIL_IDS * 10)
            .collect::<Vec<_>>()
            .join(",");
        let out = parse_detail_ids(&input).expect("valid uuids");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, uuid::Uuid::from_u128(1));
    }

    #[test]
    fn rejects_invalid_uuid() {
        let err = parse_detail_ids("not-a-uuid").unwrap_err();
        assert!(err.contains("Invalid UUID"));
        assert!(err.contains("not-a-uuid"));
    }

    #[test]
    fn empty_input_yields_no_ids() {
        assert!(parse_detail_ids("").expect("ok").is_empty());
        assert!(parse_detail_ids("   ").expect("ok").is_empty());
        assert!(parse_detail_ids(",,,").expect("ok").is_empty());
    }

    // Regression guard for the details mode-1 missing-required-param finding:
    // a missing 'ids'/'repo' param is a client error and must surface as
    // HTTP 400 (BadRequest), not HTTP 500 (Internal). This asserts the
    // AppError variant chosen by the handler maps to the correct status —
    // pure logic, no DB/IO.
    #[test]
    fn missing_required_param_maps_to_400_not_500() {
        use axum::http::StatusCode;

        // The variant the handler now builds for the missing-param case.
        let resp = AppError::BadRequest("Either 'ids' or 'repo' query parameter required".into())
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Guard against regressing to the old behaviour (anyhow -> Internal -> 500).
        let regressed = AppError::Internal(anyhow::anyhow!("boom")).into_response();
        assert_eq!(regressed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
