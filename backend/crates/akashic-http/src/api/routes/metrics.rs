//! C5: GET /api/v1/metrics — Prometheus exposition format.
//!
//! No auth, no rate-limit (same posture as /health and /ready). External
//! exposure gated by C7 reverse proxy.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;

use akashic_context::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/metrics", get(render))
}

async fn render(State(state): State<AppState>) -> impl IntoResponse {
    let body = state.metrics_handle.render();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    (StatusCode::OK, headers, body)
}
