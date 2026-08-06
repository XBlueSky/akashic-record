//! B5 /api/v1/health/oauth endpoint.
//!
//! Returns the latest cached ValidationReport with HTTP status reflecting
//! `overall` (200 for Ok/Warn, 503 for Fail). Cache TTL 60s.

use std::time::{Duration, Instant};

use axum::{
    Json, Router, extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse,
    routing::get,
};

use akashic_context::AppState;
use akashic_domain::types::CheckStatus;

const CACHE_TTL: Duration = Duration::from_mins(1);

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/health/oauth", get(health_oauth))
}

async fn health_oauth(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    // Require a valid session: this endpoint exposes GitLab-OAuth config health
    // and triggers backend->GitLab probes, so it must not be anonymous (the
    // unauthenticated liveness surface is /health and /ready).
    let authed = match crate::auth::session_cookie_value(&headers) {
        Some(key) => state.auth_store.validate_session(&key).await.is_some(),
        None => false,
    };
    if !authed {
        return (StatusCode::UNAUTHORIZED, "authentication required").into_response();
    }

    // Fast path: return cached if fresh.
    {
        let cache = state.oauth_health_cache.read().await;
        if let Some((cached_at, report)) = cache.as_ref()
            && cached_at.elapsed() < CACHE_TTL
        {
            let status = status_for(report.overall);
            return (status, Json(report.clone())).into_response();
        }
    }

    // Slow path: acquire the write lock first, then re-check (the first
    // waiter does the work; subsequent waiters re-read the now-fresh cache).
    let mut cache = state.oauth_health_cache.write().await;
    if let Some((cached_at, report)) = cache.as_ref()
        && cached_at.elapsed() < CACHE_TTL
    {
        let status = status_for(report.overall);
        return (status, Json(report.clone())).into_response();
    }

    tracing::debug!(event = "oauth_health_revalidate");
    let report = match state.auth_service.health_oauth().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%e, "oauth health check failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "oauth health check failed",
            )
                .into_response();
        }
    };
    let status = status_for(report.overall);

    // C5: emit per-check gauge after revalidation. 0=Ok, 1=Warn, 2=Fail —
    // Prometheus 'lower-is-better' convention.
    for check in &report.checks {
        let value: f64 = match check.status {
            CheckStatus::Ok => 0.0,
            CheckStatus::Warn => 1.0,
            CheckStatus::Fail => 2.0,
        };
        metrics::gauge!(
            "akashic_oauth_runtime_check_status",
            "check_name" => check.name.to_string(),
        )
        .set(value);
    }

    *cache = Some((Instant::now(), report.clone()));
    (status, Json(report)).into_response()
}

fn status_for(overall: CheckStatus) -> StatusCode {
    match overall {
        CheckStatus::Ok | CheckStatus::Warn => StatusCode::OK,
        CheckStatus::Fail => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_SESSION_KEY: &str = "test-oauth-health-session";

    async fn build_state_with_gitlab_mock(server_uri: String) -> AppState {
        let mut state = akashic_test_support::build_app_state(server_uri.clone()).await;
        let mut new_cfg = state.config.clone();
        new_cfg.gitlab_url = server_uri;
        state.config = new_cfg;
        // The endpoint now requires a valid session; seed one for the tests.
        let user_info = crate::auth::types::UserInfo {
            username: "test_oauth_health".into(),
            name: None,
            avatar_url: None,
        };
        state
            .auth_store
            .insert_session(
                TEST_SESSION_KEY,
                &user_info,
                "fake-gitlab-token",
                3600,
                Some(42),
            )
            .await
            .expect("insert_session");
        state
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn health_oauth_returns_503_when_fail() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v4/version"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let state = build_state_with_gitlab_mock(server.uri()).await;
        let app = router().with_state(state);
        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/health/oauth")
            .header("cookie", format!("ak_session={TEST_SESSION_KEY}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn health_oauth_requires_session() {
        let server = MockServer::start().await;
        let state = build_state_with_gitlab_mock(server.uri()).await;
        let app = router().with_state(state);
        // No ak_session cookie → 401, and no upstream GitLab call is made.
        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/health/oauth")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn health_oauth_caches_for_60_seconds() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v4/version"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "16.0.0"})),
            )
            .expect(1) // exactly one upstream call across two health requests
            .mount(&server)
            .await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token": "x", "error": "shadow"})),
            )
            .expect(2) // 2 calls total: 1 each from oauth_token_endpoint_reachable and client_credentials_valid
            .mount(&server)
            .await;

        let state = build_state_with_gitlab_mock(server.uri()).await;
        let app = router().with_state(state);

        let req1 = Request::builder()
            .method("GET")
            .uri("/api/v1/health/oauth")
            .header("cookie", format!("ak_session={TEST_SESSION_KEY}"))
            .body(Body::empty())
            .unwrap();
        let _ = app.clone().oneshot(req1).await.unwrap();

        let req2 = Request::builder()
            .method("GET")
            .uri("/api/v1/health/oauth")
            .header("cookie", format!("ak_session={TEST_SESSION_KEY}"))
            .body(Body::empty())
            .unwrap();
        let _ = app.oneshot(req2).await.unwrap();

        // wiremock asserts expect(1) and expect(2) on drop.
    }
}
