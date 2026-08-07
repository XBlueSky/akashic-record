//! Integration tests for `PgOauthConsentRepo` (Task 5: CIMD validation +
//! consent screen replace DCR, spec §4 — MCP refactor 2026-08-07).
//!
//! Requires a live Postgres reachable via `DATABASE_URL` (falls back to
//! `akashic_test_support::test_pg_pool`'s :5433 default, which is a dead
//! port in this environment — export DATABASE_URL explicitly, e.g.
//! `postgres://akashic:${PG_PASSWORD}@localhost:5432/akashic`).
//!
//! CIMD fetch/validate and the actual authorization-code mint on approval
//! happen at the HTTP handler layer (`akashic_http::auth::mcp_oauth`) — this
//! repo only issues/redeems the pending-consent row and its bound
//! client/PKCE/state metadata, so there is no "repo validates the client
//! document" case to test here.

use akashic_domain::ports::OauthConsentRepo;
use akashic_domain::types::PendingConsentInput;
use akashic_store_pg::repos::PgOauthConsentRepo;
use akashic_test_support::test_pg_pool;
use sqlx::PgPool;

async fn setup() -> PgPool {
    let pool = test_pg_pool().await;
    akashic_store_pg::init_auth_schema(&pool)
        .await
        .expect("init_auth_schema");
    pool
}

fn sample_meta(client_id: &str) -> PendingConsentInput {
    PendingConsentInput {
        client_id: client_id.to_string(),
        client_name: Some("Test Client".to_string()),
        redirect_uri: "http://127.0.0.1:33418/callback".to_string(),
        oauth_state: "state-abc".to_string(),
        code_challenge: "challenge-abc".to_string(),
    }
}

async fn cleanup(pool: &PgPool, consent_id: uuid::Uuid) {
    sqlx::query("DELETE FROM mcp_oauth_pending_consents WHERE consent_id = $1")
        .bind(consent_id)
        .execute(pool)
        .await
        .ok();
}

#[tokio::test]
async fn issue_then_redeem_round_trips_bound_fields() {
    let pool = setup().await;
    let repo = PgOauthConsentRepo::new(pool.clone());
    let meta = sample_meta("https://client.example/metadata.json");

    let consent_id = repo
        .issue_pending(424_242, "test_t5_user", &meta)
        .await
        .expect("issue_pending");

    let redeemed = repo
        .redeem_pending(consent_id, 424_242)
        .await
        .expect("redeem_pending ok")
        .expect("redeem_pending some");

    assert_eq!(redeemed.client_id, meta.client_id);
    assert_eq!(redeemed.client_name, meta.client_name);
    assert_eq!(redeemed.redirect_uri, meta.redirect_uri);
    assert_eq!(redeemed.oauth_state, meta.oauth_state);
    assert_eq!(redeemed.code_challenge, meta.code_challenge);
    assert_eq!(redeemed.user_id, 424_242);
    assert_eq!(redeemed.user_login, "test_t5_user");

    cleanup(&pool, consent_id).await;
}

#[tokio::test]
async fn redeem_pending_is_one_time_use() {
    let pool = setup().await;
    let repo = PgOauthConsentRepo::new(pool.clone());
    let meta = sample_meta("https://client.example/one-time.json");

    let consent_id = repo
        .issue_pending(424_243, "test_t5_one_time", &meta)
        .await
        .expect("issue_pending");

    let first = repo
        .redeem_pending(consent_id, 424_243)
        .await
        .expect("first redeem ok");
    assert!(first.is_some(), "first redeem must succeed");

    let second = repo
        .redeem_pending(consent_id, 424_243)
        .await
        .expect("second redeem ok");
    assert!(
        second.is_none(),
        "replaying an already-used consent_id must return None"
    );

    cleanup(&pool, consent_id).await;
}

#[tokio::test]
async fn redeem_pending_rejects_wrong_user_id() {
    let pool = setup().await;
    let repo = PgOauthConsentRepo::new(pool.clone());
    let meta = sample_meta("https://client.example/wrong-user.json");

    let consent_id = repo
        .issue_pending(424_244, "test_t5_owner", &meta)
        .await
        .expect("issue_pending");

    // CSRF defense: a different user_id (e.g. a forged cross-session POST)
    // must not be able to redeem someone else's pending consent, even with
    // the correct consent_id.
    let wrong_user = repo
        .redeem_pending(consent_id, 999_999)
        .await
        .expect("redeem ok");
    assert!(
        wrong_user.is_none(),
        "a different user_id must not redeem another user's consent"
    );

    // The row must still be unredeemed for the actual owner afterwards.
    let owner = repo
        .redeem_pending(consent_id, 424_244)
        .await
        .expect("redeem ok")
        .expect("owner redeem must still succeed");
    assert_eq!(owner.user_id, 424_244);

    cleanup(&pool, consent_id).await;
}

#[tokio::test]
async fn redeem_pending_rejects_expired_consent() {
    let pool = setup().await;

    // Seed an already-expired row directly (bypassing issue_pending's fixed
    // 10-minute TTL) to exercise the expiry branch of the atomic CAS.
    let consent_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO mcp_oauth_pending_consents \
            (user_id, user_login, client_id, client_name, redirect_uri, \
             oauth_state, code_challenge, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, now() - INTERVAL '1 second') \
         RETURNING consent_id",
    )
    .bind(424_245_i64)
    .bind("test_t5_expired_user")
    .bind("https://client.example/expired.json")
    .bind(Some("Test Client"))
    .bind("http://127.0.0.1:33418/callback")
    .bind("state-expired")
    .bind("challenge-expired")
    .fetch_one(&pool)
    .await
    .expect("seed expired consent");

    let repo = PgOauthConsentRepo::new(pool.clone());
    let redeemed = repo
        .redeem_pending(consent_id, 424_245)
        .await
        .expect("redeem ok");
    assert!(redeemed.is_none(), "expired consent must not be redeemable");

    cleanup(&pool, consent_id).await;
}

#[tokio::test]
async fn redeem_pending_returns_none_for_unknown_consent_id() {
    let pool = setup().await;
    let repo = PgOauthConsentRepo::new(pool.clone());
    let redeemed = repo
        .redeem_pending(uuid::Uuid::new_v4(), 1)
        .await
        .expect("redeem ok");
    assert!(redeemed.is_none());
}
