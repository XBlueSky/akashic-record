mod admins;
mod docs_derive;
pub mod docs_publish;
mod docs_read;
mod graph;
mod health;
pub mod health_oauth;
mod ingestion;
mod llms;
pub mod metrics;
mod modules;
mod notes;
mod publish_tokens;
pub mod repos;
mod sagas;
mod search;

#[cfg(feature = "test-fixtures")]
pub mod test_fixtures;

use axum::{
    Router,
    routing::{delete, get, post, put},
};

use akashic_context::AppState;

use super::error::AppError;

/// Return the health and readiness routes only. These are exempt from
/// rate limiting per A6 sub-spec AC-3.
pub fn health_router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .merge(health_oauth::router())
}

/// Build the REST API router for the Svelte frontend viewer.
/// Public read-only routes — no authentication required.
pub fn public_router() -> Router<AppState> {
    Router::new()
        // repos
        .route("/api/v1/repos", get(repos::list_repos))
        .route("/api/v1/repos/{name}/branches", get(repos::list_branches))
        .route(
            "/api/v1/repos/{name}/permissions",
            get(repos::get_permissions),
        )
        .route("/api/v1/repos/{name}/detail", get(repos::repo_detail))
        // notes
        .route("/api/v1/repos/{name}/notes", get(notes::list_notes))
        .route("/api/v1/repos/{name}/notes/health", get(notes::note_health))
        .route("/api/v1/repos/{name}/notes/{uuid}", get(notes::get_note))
        // graph — core
        .route("/api/v1/graph/{repo_name}", get(graph::core::get_graph))
        .route(
            "/api/v1/god-nodes/{repo_name}",
            get(graph::core::get_god_nodes),
        )
        // graph — module
        .route(
            "/api/v1/module-graph/{repo_name}",
            get(graph::module::get_module_graph),
        )
        .route(
            "/api/v1/modules/{module_id}/chunks",
            get(graph::module::get_module_detail),
        )
        .route(
            "/api/v1/modules/{module_id}/call-graph",
            get(graph::module::get_module_call_graph),
        )
        // graph — doc
        .route(
            "/api/v1/doc-graph/{repo_name}",
            get(graph::doc::get_doc_graph),
        )
        .route(
            "/api/v1/documents/{doc_id}/sections",
            get(graph::doc::get_document_detail),
        )
        .route(
            "/api/v1/clusters/{cluster_id}/sections",
            get(graph::doc::get_cluster_detail),
        )
        // modules
        .route("/api/v1/repos/{name}/modules", get(modules::list_modules))
        .route(
            "/api/v1/repos/{name}/modules/{path}/chunks",
            get(modules::module_chunks),
        )
        .route(
            "/api/v1/repos/{name}/chunks/{id}",
            get(modules::chunk_detail),
        )
        // ingestion
        .route(
            "/api/v1/repos/{name}/ingest/status",
            get(ingestion::ingest_status),
        )
        .route("/api/v1/jobs/active", get(ingestion::active_jobs))
        .route("/api/v1/sources/overview", get(ingestion::sources_overview))
        // search
        .route("/api/v1/search", get(search::unified_search))
        .route("/api/v1/graphrag/query", post(search::graphrag_query))
        .route("/api/v1/details", get(search::details))
        // sagas
        .route("/api/v1/repos/{name}/sagas", get(sagas::list_sagas_handler))
        .route(
            "/api/v1/repos/{name}/sagas/{saga_id}",
            get(sagas::get_saga_handler),
        )
        // docs-corpus reads (Task 10, C3) — public, unauthenticated, mirrors
        // repos/graph::doc above. `derive/retry` (protected, POST) lives in
        // docs_derive under protected_router below.
        .route("/api/v1/docs", get(docs_read::list_docs_repos))
        .route("/api/v1/docs/{repo}", get(docs_read::list_docs_versions))
        .route("/api/v1/docs/{repo}/{version}/nav", get(docs_read::get_nav))
        .route(
            "/api/v1/docs/{repo}/{version}/page/{*path}",
            get(docs_read::get_page),
        )
        .route(
            "/api/v1/docs/{repo}/{version}/raw/{*path}",
            get(docs_read::get_raw),
        )
        // Plan-3 E-part AI surface (always-latest text endpoints)
        .route("/llms.txt", get(llms::global_llms_txt))
        .route("/docs/{repo}/llms.txt", get(llms::repo_llms_txt))
        .route("/docs/{repo}/llms-full.txt", get(llms::repo_llms_full))
        .route("/docs/{repo}/skill.md", get(llms::repo_skill_md))
        // events
        .route("/api/v1/events", get(super::events::event_stream))
}

/// Protected routes — require authenticated session (GitLab token needed).
pub fn protected_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/repos/{name}/ingest",
            post(ingestion::trigger_ingest),
        )
        .route("/api/v1/repos/{name}/reingest", post(ingestion::reingest))
        .route(
            "/api/v1/repos/{name}/resume",
            post(ingestion::resume_ingest),
        )
        .route("/api/v1/sources/add", post(ingestion::add_source))
        .route("/api/v1/sources/pending", get(ingestion::pending_sources))
        .route("/api/v1/sources/demand", get(ingestion::demand_sources))
        .route(
            "/api/v1/sources/{id}/approve",
            post(ingestion::approve_source),
        )
        .route(
            "/api/v1/sources/{id}/reject",
            post(ingestion::reject_source),
        )
        .route(
            "/api/v1/repos/{name}/notes/{uuid}",
            put(notes::update_note).delete(notes::delete_note),
        )
        .route("/api/v1/repos/{name}", delete(repos::delete_repo))
        // relink-explains writes Neo4j EXPLAINS edges and invokes the LLM per
        // section, so it must be authenticated + actor-quota-scoped, not public.
        .route(
            "/api/v1/relink-explains/{name}",
            post(search::relink_explains),
        )
        .route("/api/v1/gitlab/branches", get(ingestion::gitlab_branches))
        // Admin trust-chain (A2d-3)
        .route("/api/v1/admins", get(admins::list_admins))
        .route("/api/v1/admins/grant", post(admins::grant_admin))
        .route("/api/v1/admins/revoke", post(admins::revoke_admin))
        // Docs-publish tokens (Task 6, B1) — repo-scoped `akp_` bearers that
        // authorize the docs-publish endpoint.
        .route(
            "/api/v1/docs-tokens",
            post(publish_tokens::issue_token).get(publish_tokens::list_tokens),
        )
        .route(
            "/api/v1/docs-tokens/{id}/revoke",
            post(publish_tokens::revoke_token),
        )
        // Docs-corpus derive retry (Task 8, B3).
        .route(
            "/api/v1/docs/{repo}/derive/retry",
            post(docs_derive::retry_derive),
        )
}

// ── Shared helpers ──────────────────────────────────────────────────────────

/// Map a `QuotaExceededError` (if present in the error chain) to an HTTP 429
/// response. Returns `Some(response)` on match, `None` otherwise so the
/// caller can fall through to its normal error handling.
pub fn quota_exceeded_response(err: &anyhow::Error) -> Option<axum::response::Response> {
    use axum::{Json, http::StatusCode, response::IntoResponse};
    err.downcast_ref::<akashic_quota::QuotaExceededError>()
        .map(|q| {
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "quota_exceeded",
                    "used": q.used,
                    "cap": q.cap,
                    "window_secs": q.window_secs,
                })),
            )
                .into_response()
        })
}

/// Extract the session key from the `Authorization: Bearer` header, falling
/// back to the `ak_session` cookie. Shared by extract_user_token /
/// resolve_username so the auth-critical extraction lives in one place.
fn extract_session_key(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(String::from)
        .or_else(|| {
            let cookie_header = headers.get("cookie")?.to_str().ok()?;
            for part in cookie_header.split(';') {
                let part = part.trim();
                if let Some(val) = part.strip_prefix("ak_session=") {
                    let val = val.trim();
                    if !val.is_empty() {
                        return Some(val.to_string());
                    }
                }
            }
            None
        })
}

/// Extract the user's GitLab token from session (bearer or cookie).
pub(super) async fn extract_user_token(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    let api_key = extract_session_key(headers)?;
    let (_, gitlab_token, _gitlab_user_id) = state.auth_store.validate_session(&api_key).await?;
    Some(gitlab_token)
}

/// Resolve GitLab access_level for a user+repo via the RepoService gateway.
///
/// Thinned (Phase2-GW): the inline `/api/v4/projects` lookup + the local
/// `select_gitlab_project` moved into `akashic-gitlab`; this delegates to
/// `RepoService::get_repo_permissions` and returns the same `i64` level the
/// callers compare against their thresholds.
pub(super) async fn resolve_gitlab_access(
    state: &AppState,
    gitlab_token: &str,
    repo_name: &str,
) -> Result<i64, AppError> {
    let perms = state
        .repo_service
        .get_repo_permissions(repo_name.to_string(), Some(gitlab_token.to_string()))
        .await?;
    Ok(perms.access_level as i64)
}

/// Pure membership check: is `username` an admin? Empty allowlist denies all.
pub(super) fn username_is_admin(username: &str, admins: &[String]) -> bool {
    !admins.is_empty() && admins.iter().any(|a| a == username)
}

/// Resolve the authenticated caller's GitLab username from the session.
pub(super) async fn resolve_username(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    let api_key = extract_session_key(headers)?;
    let (user_info, _gitlab_token, _uid) = state.auth_store.validate_session(&api_key).await?;
    Some(user_info.username)
}

/// Require the caller be an admin.
///
/// Admin = env root (short-circuit, no DB) OR reachable from a root via
/// non-revoked `admin_grants` edges (recursive CTE).  Fail-closed: empty
/// `AKASHIC_ADMIN_USERS` ⇒ nobody is admin.
pub(super) async fn require_admin(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<String, AppError> {
    let username = resolve_username(state, headers)
        .await
        .ok_or(AppError::Unauthorized)?;

    // Fast path: env roots short-circuit without a DB call.
    if username_is_admin(&username, &state.config.admin_users) {
        return Ok(username);
    }

    // Slow path: recursive CTE over admin_grants.
    let is_admin = state
        .admin_grant_repo
        .is_admin(&state.config.admin_users, &username)
        .await
        .map_err(AppError::Internal)?;

    if is_admin {
        return Ok(username);
    }
    Err(AppError::Forbidden(format!(
        "admin privileges required for '{username}'"
    )))
}

/// Verify the caller holds at least `min_level` GitLab access to `repo_name`.
/// Returns `Unauthorized` when no session token is present, `Forbidden` when
/// the resolved access level is below `min_level`.
pub(super) async fn require_gitlab_access(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    repo_name: &str,
    min_level: i64,
) -> Result<(), AppError> {
    let token = extract_user_token(state, headers)
        .await
        .ok_or(AppError::Unauthorized)?;
    let level = resolve_gitlab_access(state, &token, repo_name).await?;
    if level < min_level {
        return Err(AppError::Forbidden(format!(
            "GitLab access level {min_level}+ required for '{repo_name}'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::username_is_admin;

    #[test]
    fn admin_predicate() {
        let admins = vec!["tonyhu".to_string(), "alice".to_string()];
        assert!(username_is_admin("tonyhu", &admins));
        assert!(!username_is_admin("mallory", &admins));
        assert!(
            !username_is_admin("tonyhu", &[]),
            "empty allowlist denies all"
        );
    }
}
