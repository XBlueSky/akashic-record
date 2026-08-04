//! Integration tests for `PgOauthCodeRepo` (Task 4: MCP OAuth authorize +
//! token, RFC 6749 §4.1).
//!
//! Requires a live Postgres reachable via `DATABASE_URL` (falls back to
//! `akashic_test_support::test_pg_pool`'s :5433 default, which is a dead
//! port in this environment — export DATABASE_URL explicitly, e.g.
//! `postgres://akashic:${PG_PASSWORD}@localhost:5432/akashic`).
//!
//! PKCE verification and `client_id`/`redirect_uri` matching happen at the
//! HTTP handler layer (`akashic_http::auth::mcp_oauth`) — this repo only
//! persists/redeems the code and its bound `code_challenge`/`redirect_uri`,
//! so there is no "repo rejects a PKCE mismatch" case to test here.

use akashic_domain::ports::{OauthClientRepo, OauthCodeRepo};
use akashic_store_pg::repos::{PgOauthClientRepo, PgOauthCodeRepo};
use akashic_test_support::test_pg_pool;
use sqlx::PgPool;
use uuid::Uuid;

async fn setup() -> PgPool {
    let pool = test_pg_pool().await;
    akashic_store_pg::init_auth_schema(&pool)
        .await
        .expect("init_auth_schema");
    pool
}

/// Register a throwaway client so `mcp_oauth_codes.client_id`'s FK is
/// satisfied, returning its `client_id`.
async fn register_client(pool: &PgPool, name: &str) -> Uuid {
    let repo = PgOauthClientRepo::new(pool.clone());
    repo.register_client(
        vec!["http://127.0.0.1:33418/callback".to_string()],
        Some(name.to_string()),
    )
    .await
    .expect("register_client")
    .client_id
}

async fn cleanup(pool: &PgPool, client_id: Uuid) {
    sqlx::query("DELETE FROM mcp_oauth_codes WHERE client_id = $1")
        .bind(client_id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM mcp_oauth_clients WHERE client_id = $1")
        .bind(client_id)
        .execute(pool)
        .await
        .ok();
}

#[tokio::test]
async fn issue_then_consume_round_trips_bound_fields() {
    let pool = setup().await;
    let client_id = register_client(&pool, "test_t4_issue_consume").await;
    let repo = PgOauthCodeRepo::new(pool.clone());

    let code = repo
        .issue_code(
            client_id,
            424_242,
            "test_t4_user",
            "challenge-abc",
            "http://127.0.0.1:33418/callback",
        )
        .await
        .expect("issue_code");
    assert_eq!(code.len(), 64, "code should be 32 bytes hex-encoded");
    assert!(code.chars().all(|c| c.is_ascii_hexdigit()));

    let consumed = repo
        .consume_code(&code)
        .await
        .expect("consume_code ok")
        .expect("consume_code some");
    assert_eq!(consumed.client_id, client_id);
    assert_eq!(consumed.user_id, 424_242);
    assert_eq!(consumed.user_login, "test_t4_user");
    assert_eq!(consumed.code_challenge, "challenge-abc");
    assert_eq!(consumed.redirect_uri, "http://127.0.0.1:33418/callback");

    cleanup(&pool, client_id).await;
}

#[tokio::test]
async fn consume_code_is_one_time_use() {
    let pool = setup().await;
    let client_id = register_client(&pool, "test_t4_one_time").await;
    let repo = PgOauthCodeRepo::new(pool.clone());

    let code = repo
        .issue_code(
            client_id,
            424_243,
            "test_t4_user2",
            "challenge-xyz",
            "http://127.0.0.1:33418/callback",
        )
        .await
        .expect("issue_code");

    let first = repo.consume_code(&code).await.expect("first consume ok");
    assert!(first.is_some(), "first consume must succeed");

    let second = repo.consume_code(&code).await.expect("second consume ok");
    assert!(
        second.is_none(),
        "replaying an already-used code must return None (invalid_grant)"
    );

    cleanup(&pool, client_id).await;
}

#[tokio::test]
async fn consume_code_rejects_expired_code() {
    let pool = setup().await;
    let client_id = register_client(&pool, "test_t4_expired").await;

    // Seed an already-expired row directly (bypassing issue_code's fixed
    // 10-minute TTL) to exercise the expiry branch of the atomic CAS.
    use sha2::Digest;
    let plaintext = "deadbeef".repeat(8); // 64 hex chars, well-formed shape
    let hash = sha2::Sha256::digest(plaintext.as_bytes());
    sqlx::query(
        "INSERT INTO mcp_oauth_codes \
            (code_hash, client_id, user_id, user_login, code_challenge, redirect_uri, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, now() - INTERVAL '1 second')",
    )
    .bind(hash.as_slice())
    .bind(client_id)
    .bind(424_244_i64)
    .bind("test_t4_expired_user")
    .bind("challenge-expired")
    .bind("http://127.0.0.1:33418/callback")
    .execute(&pool)
    .await
    .expect("seed expired code");

    let repo = PgOauthCodeRepo::new(pool.clone());
    let consumed = repo.consume_code(&plaintext).await.expect("consume ok");
    assert!(consumed.is_none(), "expired code must not be redeemable");

    cleanup(&pool, client_id).await;
}

#[tokio::test]
async fn consume_code_returns_none_for_unknown_code() {
    let pool = setup().await;
    let repo = PgOauthCodeRepo::new(pool.clone());
    let consumed = repo
        .consume_code("0000000000000000000000000000000000000000000000000000000000000000")
        .await
        .expect("consume ok");
    assert!(consumed.is_none());
}
