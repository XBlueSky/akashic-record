use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use tracing::warn;

use akashic_context::AppState;
use akashic_identity::types::{ExchangeRequest, ExchangeResponse};

pub fn router() -> Router<AppState> {
    Router::new().route("/auth/exchange", post(exchange))
}

/// POST /auth/exchange — submit the verification code, receive an API key.
async fn exchange(
    State(state): State<AppState>,
    Json(body): Json<ExchangeRequest>,
) -> impl IntoResponse {
    match state.auth_service.exchange_code(body.code).await {
        Ok(json_str) => {
            // exchange_code returns JSON: {"api_key":"…","username":"…"}
            let v: serde_json::Value = match serde_json::from_str(&json_str) {
                Ok(v) => v,
                Err(e) => {
                    warn!(event = "exchange_code_parse_failed", error = %e);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error": "internal error"})),
                    )
                        .into_response();
                }
            };
            let api_key = v["api_key"].as_str().unwrap_or("").to_string();
            let username = v["username"].as_str().unwrap_or("").to_string();
            Json(ExchangeResponse { api_key, username }).into_response()
        }
        Err(akashic_domain::DomainError::Unauthorized) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Invalid or expired code"})),
        )
            .into_response(),
        Err(e) => {
            warn!(event = "exchange_code_failed", error = %e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Failed to create session"})),
            )
                .into_response()
        }
    }
}
