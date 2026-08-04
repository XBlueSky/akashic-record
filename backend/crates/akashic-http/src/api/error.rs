use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub enum AppError {
    Internal(anyhow::Error),
    NotFound,
    BadRequest(String),
    Unauthorized,
    Forbidden(String),
    Conflict(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Internal(ref e) => {
                // Check for quota exceeded before falling through to 500.
                if let Some(resp) = crate::api::routes::quota_exceeded_response(e) {
                    return resp;
                }
                tracing::warn!(%e, "Internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "Internal server error"})),
                )
                    .into_response()
            }
            // Return the same JSON {"error": ...} envelope as every other variant
            // so clients can uniformly parse the body (a bare 404 had an empty body).
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Not Found"})),
            )
                .into_response(),
            AppError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": msg})),
            )
                .into_response(),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Unauthorized"})),
            )
                .into_response(),
            AppError::Forbidden(msg) => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": msg})),
            )
                .into_response(),
            AppError::Conflict(msg) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": msg})),
            )
                .into_response(),
        }
    }
}

// NOTE: a blanket `From<E: Into<anyhow::Error>>` cannot coexist with a typed
// `From<DomainError>` — a thiserror enum is itself `Into<anyhow::Error>`, so the
// two impls collide (coherence). We therefore provide explicit conversions: one
// for raw `anyhow::Error` (direct `?` in handlers) and one that maps the typed
// `DomainError` variants to HTTP statuses (replacing the old message-substring
// matching at the call sites).
impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e)
    }
}

// serde_json errors in handlers (response serialization) → 500, exactly as the
// removed blanket mapped them. Kept explicit since the blanket is gone.
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(e.into())
    }
}

impl From<akashic_domain::DomainError> for AppError {
    fn from(e: akashic_domain::DomainError) -> Self {
        use akashic_domain::DomainError as D;
        match e {
            D::NotFound(_) => AppError::NotFound,
            D::Conflict(m) => AppError::Conflict(m),
            D::Forbidden(m) => AppError::Forbidden(m),
            D::BadRequest(m) => AppError::BadRequest(m),
            D::Unauthorized => AppError::Unauthorized,
            // Quota-exceeded still rides the anyhow chain (the 429 downcast in
            // `quota_exceeded_response` runs on AppError::Internal).
            D::Internal(e) => AppError::Internal(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    // Regression: AppError::NotFound used to return a bare 404 with an empty
    // body. It must now carry the same JSON {"error": ...} envelope as every
    // other variant so clients can uniformly parse the response body.
    #[tokio::test]
    async fn not_found_returns_json_error_envelope() {
        let resp = AppError::NotFound.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body should be collectable");
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).expect("body should be valid JSON");
        assert_eq!(body, serde_json::json!({"error": "Not Found"}));
    }
}
