use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use akashic_context::AppState;

/// GET /health — liveness probe. Always returns 200.
pub(super) async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

/// GET /ready — readiness probe.
///
/// Returns 200 if ALL of these hold:
///   - shutdown is NOT in progress (`AppState.shutdown` not cancelled)
///   - every probe in `AppState.readiness` has succeeded within the last
///     30 seconds
///
/// Otherwise returns 503 with a JSON body describing per-probe state.
/// The probes themselves are run by the background poller spawned in
/// main(); this handler is a pure read.
pub(super) async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let summary = akashic_platform::readiness::check_ready(
        &state.readiness,
        &state.shutdown,
        std::time::Duration::from_secs(30),
    )
    .await;
    let status = if summary.all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(summary)).into_response()
}
