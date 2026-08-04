use axum::{routing::get, routing::post, Router};

pub fn app() -> Router {
    Router::new()
        .route("/users", get(list_users))
        .route("/users", post(create_user))
        .route("/health", get(health::check))
}

async fn list_users() -> &'static str {
    "users"
}

async fn create_user() -> &'static str {
    "created"
}
