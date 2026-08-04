//! Integration tests for `PgOauthClientRepo` (Task 3: MCP OAuth dynamic
//! client registration, RFC 7591).
//!
//! Requires a live Postgres reachable via `DATABASE_URL` (falls back to
//! `akashic_test_support::test_pg_pool`'s :5433 default, which is a dead
//! port in this environment — always export DATABASE_URL explicitly when
//! running these, e.g. `postgres://akashic:${PG_PASSWORD}@localhost:5432/akashic`).
//!
//! Redirect-URI format validation (loopback-http only) happens at the HTTP
//! handler layer (`akashic_http::auth::mcp_oauth::is_allowed_redirect_uri`,
//! a pure function unit-tested there directly) — this repo persists whatever
//! `redirect_uris` it is given, so there is no "repo rejects a bad URI" case
//! to test here.

use akashic_domain::ports::OauthClientRepo;
use akashic_store_pg::repos::PgOauthClientRepo;
use akashic_test_support::test_pg_pool;
use sqlx::PgPool;

async fn setup() -> PgPool {
    let pool = test_pg_pool().await;
    akashic_store_pg::init_auth_schema(&pool)
        .await
        .expect("init_auth_schema");
    pool
}

async fn cleanup_by_name(pool: &PgPool, client_name: &str) {
    sqlx::query("DELETE FROM mcp_oauth_clients WHERE client_name = $1")
        .bind(client_name)
        .execute(pool)
        .await
        .ok();
}

#[tokio::test]
async fn register_then_get_round_trips_redirect_uris_and_name() {
    const NAME: &str = "test_t3_register_get";
    let pool = setup().await;
    cleanup_by_name(&pool, NAME).await;
    let repo = PgOauthClientRepo::new(pool.clone());

    let redirect_uris = vec![
        "http://127.0.0.1:33418/callback".to_string(),
        "http://localhost:33418/callback".to_string(),
    ];

    let registered = repo
        .register_client(redirect_uris.clone(), Some(NAME.to_string()))
        .await
        .expect("register");

    assert_eq!(registered.client_name.as_deref(), Some(NAME));
    assert_eq!(registered.redirect_uris, redirect_uris);

    let fetched = repo
        .get_client(registered.client_id)
        .await
        .expect("get ok")
        .expect("some");

    assert_eq!(fetched.client_id, registered.client_id);
    assert_eq!(fetched.client_name.as_deref(), Some(NAME));
    assert_eq!(fetched.redirect_uris, redirect_uris);

    cleanup_by_name(&pool, NAME).await;
}

#[tokio::test]
async fn register_without_client_name_persists_none() {
    let pool = setup().await;
    let repo = PgOauthClientRepo::new(pool.clone());

    let redirect_uris = vec!["https://example.com/cb".to_string()];
    let registered = repo
        .register_client(redirect_uris.clone(), None)
        .await
        .expect("register");

    assert_eq!(registered.client_name, None);

    let fetched = repo
        .get_client(registered.client_id)
        .await
        .expect("get ok")
        .expect("some");
    assert_eq!(fetched.client_name, None);
    assert_eq!(fetched.redirect_uris, redirect_uris);

    sqlx::query("DELETE FROM mcp_oauth_clients WHERE client_id = $1")
        .bind(registered.client_id)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
async fn get_client_returns_none_for_unknown_id() {
    let pool = setup().await;
    let repo = PgOauthClientRepo::new(pool.clone());

    let unknown = repo.get_client(uuid::Uuid::new_v4()).await.expect("get ok");
    assert!(unknown.is_none());
}
