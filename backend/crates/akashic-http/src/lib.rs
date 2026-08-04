//! Akashic Record HTTP interface crate.
//!
//! A2b carve #3 (2026-06-04): the HTTP interface — the `api`, `auth`, and
//! `gitlab` module trees plus `build_router` — was carved out of
//! `akashic-server` into this crate. `akashic-server` re-exports
//! `akashic_http::{api, auth, build_router, gitlab}` so every existing
//! `crate::api::…` / `crate::auth::…` / `crate::gitlab::…` / `build_router`
//! reference in `main.rs`, `embedding/`, `llm/`, and the integration tests
//! keeps resolving unchanged.

pub mod api;
pub mod auth;
pub mod gitlab;

// AppState is defined in akashic-context and re-exported here so that all
// `use crate::AppState` / `crate::AppState` references in handler modules
// continue to resolve without churn.
pub use akashic_context::AppState;

use axum::Router;
use axum::http::{
    HeaderValue, Method,
    header::{AUTHORIZATION, CONTENT_TYPE},
};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

/// Build the production axum router from a fully-constructed `AppState`.
///
/// D2: extracted from `main.rs` so integration tests under `backend/tests/`
/// can serve the exact production router on an ephemeral port without
/// duplicating the route wiring and middleware-layer order.
pub fn build_router(state: AppState) -> Router {
    let cfg = state.config.clone();

    use akashic_platform::middleware::rate_limit::{
        apply_auth_rate_limit, apply_ingest_rate_limit, apply_rate_limits,
    };

    // Auth router: stricter quota. Both the unauthenticated OAuth flow and
    // the protected session-management endpoints (/auth/me, /auth/logout)
    // get the auth quota — those are session-keepalive calls, not ingest
    // writes, so the 5/min ingest bucket would 429 normal browsing.
    let auth_public = crate::auth::router();
    let auth_session = crate::auth::protected_auth_router().route_layer(
        axum::middleware::from_fn_with_state(state.clone(), crate::auth::require_auth),
    );
    let auth_routes = apply_auth_rate_limit(auth_public.merge(auth_session), &cfg);

    // Protected/ingest router (Path A from A6 plan Step 5.0): the
    // protected branch hosts the expensive write routes (ingest /
    // reingest / resume / sources/add) plus a few others (note edits,
    // repo delete) — all accept the 5/min ingest quota.
    let protected = Router::new()
        .merge(crate::api::routes::protected_router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_auth,
        ));
    let protected = apply_ingest_rate_limit(protected, &cfg);

    // GitLab webhook authenticates via the X-Gitlab-Token header, not a session
    // cookie, so it must NOT sit behind require_auth — that rejected every real
    // delivery with 401, making the feature dead. Keep it ingest-rate-limited.
    let webhook = apply_ingest_rate_limit(crate::gitlab::webhook::router(), &cfg);

    // Docs-corpus publish (Task 7, B2) authenticates via a repo-scoped `akp_`
    // bearer token (Task 6), not a session cookie/token — same reasoning as
    // the webhook branch above, `require_auth` would reject every real CI
    // packer delivery with 401. Keep it ingest-rate-limited (IP-keyed, same
    // as every other write route here); the route's own body-limit layer is
    // set inside `docs_publish::router()`.
    let docs_publish = apply_ingest_rate_limit(crate::api::routes::docs_publish::router(), &cfg);

    // Public REST routes (excluding /health, /ready which are split out)
    // get the general /api/ 60/min quota via apply_rate_limits.
    let public_api = apply_rate_limits(crate::api::routes::public_router(), &cfg);

    // /health, /ready: NOT layered. Sub-spec AC-3.
    let health_routes = crate::api::routes::health_router();

    let app = Router::new()
        .merge(auth_routes)
        .merge(health_routes)
        .merge(public_api)
        .merge(protected)
        .merge(webhook)
        .merge(docs_publish)
        .merge(crate::api::routes::metrics::router());

    // D3: test-fixture endpoints. Gated by the `test-fixtures` cargo
    // feature — production release builds NEVER see these routes
    // (verified by AC-2 release-build symbol-grep). Nested under
    // `/test/fixtures` so the Playwright bench can mint sessions / seed
    // notes / seed repos against the real router.
    #[cfg(feature = "test-fixtures")]
    let app = app.nest(
        "/test/fixtures",
        crate::api::routes::test_fixtures::router(),
    );

    app.layer(TraceLayer::new_for_http())
        .layer({
            let mut origins: Vec<HeaderValue> = vec![
                cfg.frontend_url
                    .parse()
                    .expect("FRONTEND_URL must be a valid origin"),
            ];
            for extra in &cfg.cors_extra_origins {
                if let Ok(val) = extra.parse() {
                    origins.push(val);
                }
            }
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers([CONTENT_TYPE, AUTHORIZATION])
                .allow_credentials(true)
        })
        // C5: outermost layers — RequestIdLayer attaches X-Request-Id span
        // field BEFORE any other layer logs; MetricsLayer captures the
        // request even when downstream layers reject it (e.g. CORS preflight
        // or rate-limit 429). Tower applies layers in REVERSE declaration
        // order, so RequestIdLayer (last .layer call) is the outermost.
        .layer(akashic_platform::middleware::metrics::MetricsLayer)
        .layer(akashic_platform::middleware::request_id::RequestIdLayer)
        .with_state(state)
}
