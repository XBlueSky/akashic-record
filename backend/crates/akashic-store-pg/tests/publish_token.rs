//! Integration tests for `PgPublishTokenRepo` (Task 6, B1: repo-scoped
//! docs-publish bearer tokens, `akp_<32hex>`).
//!
//! Requires a live Postgres reachable via `DATABASE_URL` (falls back to
//! `akashic_test_support::test_pg_pool`'s :5433 default, which is a dead
//! port in this environment — always export DATABASE_URL explicitly when
//! running these).

use akashic_domain::ports::PublishTokenRepo;
use akashic_store_pg::repos::PgPublishTokenRepo;
use akashic_test_support::test_pg_pool;
use sqlx::PgPool;

async fn setup() -> PgPool {
    let pool = test_pg_pool().await;
    akashic_store_pg::init_auth_schema(&pool)
        .await
        .expect("init_auth_schema");
    pool
}

async fn cleanup_repo(pool: &PgPool, repo_name: &str) {
    sqlx::query("DELETE FROM publish_tokens WHERE repo_name = $1")
        .bind(repo_name)
        .execute(pool)
        .await
        .ok();
}

async fn cleanup_creators(pool: &PgPool, creators: &[&str]) {
    for c in creators {
        sqlx::query("DELETE FROM publish_tokens WHERE created_by = $1")
            .bind(c)
            .execute(pool)
            .await
            .ok();
    }
}

#[tokio::test]
async fn issue_then_validate_returns_same_repo() {
    const REPO: &str = "test_t6_issue_validate";
    let pool = setup().await;
    cleanup_repo(&pool, REPO).await;
    let repo = PgPublishTokenRepo::new(pool.clone());

    let (id, plaintext) = repo
        .issue_publish_token(REPO, "test_user_t6")
        .await
        .expect("issue");

    assert!(plaintext.starts_with("akp_"), "got {plaintext}");
    assert_eq!(plaintext.len(), 36, "akp_ + 32 hex");

    let validated = repo
        .validate_publish_token(&plaintext)
        .await
        .expect("validate ok")
        .expect("some");
    assert_eq!(validated.token_id, id);
    assert_eq!(validated.repo_name, REPO);

    cleanup_repo(&pool, REPO).await;
}

#[tokio::test]
async fn validate_returns_none_for_wrong_prefix_or_malformed() {
    let pool = setup().await;
    let repo = PgPublishTokenRepo::new(pool.clone());

    // Wrong prefix (mcp's ak_, not akp_):
    assert!(
        repo.validate_publish_token("ak_deadbeefdeadbeefdeadbeefdeadbeef")
            .await
            .unwrap()
            .is_none()
    );
    // Empty:
    assert!(repo.validate_publish_token("").await.unwrap().is_none());
    // Right prefix, too short:
    assert!(
        repo.validate_publish_token("akp_short")
            .await
            .unwrap()
            .is_none()
    );
    // Right prefix, non-hex:
    assert!(
        repo.validate_publish_token("akp_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz")
            .await
            .unwrap()
            .is_none()
    );
    // Right prefix, right shape, unknown hash:
    assert!(
        repo.validate_publish_token("akp_deadbeefdeadbeefdeadbeefdeadbeef")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn validate_returns_none_after_revoke_and_revoke_is_idempotent() {
    const REPO: &str = "test_t6_revoked";
    let pool = setup().await;
    cleanup_repo(&pool, REPO).await;
    let repo = PgPublishTokenRepo::new(pool.clone());

    let (id, plaintext) = repo
        .issue_publish_token(REPO, "test_user_t6")
        .await
        .expect("issue");

    assert!(
        repo.validate_publish_token(&plaintext)
            .await
            .unwrap()
            .is_some(),
        "sanity: freshly issued token must validate"
    );

    repo.revoke_publish_token(id).await.expect("revoke 1");
    repo.revoke_publish_token(id)
        .await
        .expect("revoke 2 idempotent");

    assert!(
        repo.validate_publish_token(&plaintext)
            .await
            .unwrap()
            .is_none()
    );

    cleanup_repo(&pool, REPO).await;
}

#[tokio::test]
async fn validate_returns_none_for_expired_token() {
    const REPO: &str = "test_t6_expired";
    let pool = setup().await;
    cleanup_repo(&pool, REPO).await;
    let repo = PgPublishTokenRepo::new(pool.clone());

    let (id, plaintext) = repo
        .issue_publish_token(REPO, "test_user_t6")
        .await
        .expect("issue");

    // A freshly issued token's expires_at is 90 days out — force it into the
    // past directly to exercise the expired branch without waiting.
    sqlx::query("UPDATE publish_tokens SET expires_at = now() - INTERVAL '1 second' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("force expiry");

    assert!(
        repo.validate_publish_token(&plaintext)
            .await
            .unwrap()
            .is_none()
    );

    cleanup_repo(&pool, REPO).await;
}

#[tokio::test]
async fn list_publish_tokens_returns_only_creators_tokens() {
    const REPO_A: &str = "test_t6_list_a";
    const REPO_B: &str = "test_t6_list_b";
    let pool = setup().await;
    cleanup_repo(&pool, REPO_A).await;
    cleanup_repo(&pool, REPO_B).await;
    cleanup_creators(&pool, &["test_user_t6_lister", "test_user_t6_other"]).await;
    let repo = PgPublishTokenRepo::new(pool.clone());

    let _ = repo
        .issue_publish_token(REPO_A, "test_user_t6_lister")
        .await
        .unwrap();
    let _ = repo
        .issue_publish_token(REPO_B, "test_user_t6_lister")
        .await
        .unwrap();
    let _ = repo
        .issue_publish_token(REPO_A, "test_user_t6_other")
        .await
        .unwrap();

    let rows = repo
        .list_publish_tokens("test_user_t6_lister")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "lister should see exactly their 2 tokens");
    assert!(rows.iter().all(|r| r.revoked_at.is_none()));

    cleanup_repo(&pool, REPO_A).await;
    cleanup_repo(&pool, REPO_B).await;
}

#[tokio::test]
async fn issue_publish_token_sets_90_day_sliding_expiry() {
    const REPO: &str = "test_t6_issue_expiry";
    let pool = setup().await;
    cleanup_repo(&pool, REPO).await;
    let repo = PgPublishTokenRepo::new(pool.clone());

    let _ = repo
        .issue_publish_token(REPO, "test_user_t6_expiry")
        .await
        .expect("issue");

    let rows = repo
        .list_publish_tokens("test_user_t6_expiry")
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    let expires_at = rows[0]
        .expires_at
        .expect("expires_at must be set (90d sliding TTL), not NULL");

    let now = chrono::Utc::now();
    assert!(
        expires_at > now + chrono::Duration::days(89),
        "expected expires_at > now+89d, got {expires_at} (now={now})"
    );
    assert!(
        expires_at < now + chrono::Duration::days(91),
        "expected expires_at < now+91d, got {expires_at} (now={now})"
    );

    cleanup_repo(&pool, REPO).await;
}

#[tokio::test]
async fn validate_publish_token_slides_expiry_forward_on_use() {
    const REPO: &str = "test_t6_slide";
    let pool = setup().await;
    cleanup_repo(&pool, REPO).await;
    let repo = PgPublishTokenRepo::new(pool.clone());

    let (id, plaintext) = repo
        .issue_publish_token(REPO, "test_user_t6_slide")
        .await
        .expect("issue");

    // Force the row into "issued long ago, about to expire, not used
    // recently" shape so the debounce (>60s since last_used_at) allows the
    // background bump to fire, and the near-term expires_at makes the slide
    // observable.
    sqlx::query(
        "UPDATE publish_tokens \
         SET last_used_at = now() - INTERVAL '1 hour', \
             expires_at   = now() + INTERVAL '1 day' \
         WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect("seed old last_used_at / near-term expires_at");

    let validated = repo
        .validate_publish_token(&plaintext)
        .await
        .expect("validate ok")
        .expect("some");
    assert_eq!(validated.token_id, id);

    // The expires_at bump is fire-and-forget (tokio::spawn'd), so it may not
    // have committed by the time validate_publish_token returns. Poll for up
    // to ~3s, mirroring the audit_log polling pattern in
    // akashic-server/tests/mcp_contract.rs.
    let mut attempts = 0u32;
    let expires_at = loop {
        let row: (Option<chrono::DateTime<chrono::Utc>>,) =
            sqlx::query_as("SELECT expires_at FROM publish_tokens WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("select expires_at");
        let expires_at = row.0.expect("expires_at must not be NULL");
        if expires_at > chrono::Utc::now() + chrono::Duration::days(2) {
            break expires_at;
        }
        attempts += 1;
        if attempts >= 30 {
            panic!("expires_at never slid forward after ~3s of polling (still {expires_at})");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };

    let now = chrono::Utc::now();
    assert!(
        expires_at > now + chrono::Duration::days(89),
        "expected slid expires_at > now+89d, got {expires_at} (now={now})"
    );
    assert!(
        expires_at < now + chrono::Duration::days(91),
        "expected slid expires_at < now+91d, got {expires_at} (now={now})"
    );

    cleanup_repo(&pool, REPO).await;
}

#[tokio::test]
async fn check_publish_token_ownership_returns_creator_or_none() {
    const REPO: &str = "test_t6_ownership";
    let pool = setup().await;
    cleanup_repo(&pool, REPO).await;
    let repo = PgPublishTokenRepo::new(pool.clone());

    let (id, _) = repo
        .issue_publish_token(REPO, "test_user_t6_owner")
        .await
        .unwrap();

    let owner = repo.check_publish_token_ownership(id).await.unwrap();
    assert_eq!(owner.as_deref(), Some("test_user_t6_owner"));

    let unknown = repo
        .check_publish_token_ownership(uuid::Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(unknown, None);

    cleanup_repo(&pool, REPO).await;
}
