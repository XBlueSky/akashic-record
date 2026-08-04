//! Integration test for Task 11 (docs-kit Plan 2): the `dev_issue_publish_token`
//! dev-ops CLI (`src/bin/dev_issue_publish_token.rs`).
//!
//! The bin is the interim ops path for minting a repo's publish token before
//! the G-part admin UI exists — it wraps `AuthStore::issue_publish_token`
//! directly against `DATABASE_URL` and prints the plaintext `akp_...` token.
//! This test runs the compiled binary as a subprocess (rather than calling
//! the library function directly) so it also exercises the bin's
//! arg-parsing and `DATABASE_URL` wiring, then confirms the printed token
//! round-trips through `PgPublishTokenRepo::validate_publish_token`.
//!
//! Deliberately lighter-weight than `docs_publish.rs`/`common::TestEnv`: no
//! full router, no Neo4j, no `reset_state` TRUNCATE. Follows the same
//! pattern as `akashic-store-pg/tests/publish_token.rs` — `test_pg_pool()` +
//! `init_auth_schema` (idempotent `CREATE IF NOT EXISTS`, never destructive)
//! — so it is safe to run against a live shared dev stack: it only touches
//! rows scoped to its own `REPO`/`created_by` and cleans them up after.

use akashic_domain::ports::PublishTokenRepo;
use akashic_store_pg::repos::PgPublishTokenRepo;
use akashic_test_support::test_pg_pool;
use sqlx::PgPool;
use std::process::Command;

const REPO: &str = "dev-cli-test-repo";

async fn setup() -> PgPool {
    let pool = test_pg_pool().await;
    akashic_store_pg::init_auth_schema(&pool)
        .await
        .expect("init_auth_schema");
    pool
}

async fn cleanup(pool: &PgPool) {
    sqlx::query("DELETE FROM publish_tokens WHERE repo_name = $1")
        .bind(REPO)
        .execute(pool)
        .await
        .ok();
}

fn run_bin(args: &[&str], database_url: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dev_issue_publish_token"))
        .args(args)
        .env("DATABASE_URL", database_url)
        .output()
        .expect("spawn dev_issue_publish_token")
}

#[tokio::test]
async fn issues_a_token_that_validates_for_the_given_repo() {
    let pool = setup().await;
    cleanup(&pool).await;
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set (see docs_publish.rs)");

    let output = run_bin(&[REPO], &database_url);
    assert!(
        output.status.success(),
        "bin exited non-zero: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let token = String::from_utf8(output.stdout)
        .expect("utf8 stdout")
        .trim()
        .to_string();
    assert!(
        token.starts_with("akp_"),
        "expected akp_-prefixed token, got {token:?}"
    );

    let repo = PgPublishTokenRepo::new(pool.clone());
    let validated = repo
        .validate_publish_token(&token)
        .await
        .expect("validate ok")
        .expect("freshly issued token must validate");
    assert_eq!(validated.repo_name, REPO);

    cleanup(&pool).await;
}

#[tokio::test]
async fn requires_a_repo_argument() {
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set (see docs_publish.rs)");

    let output = run_bin(&[], &database_url);
    assert!(
        !output.status.success(),
        "bin must exit non-zero without a repo argument"
    );
}
