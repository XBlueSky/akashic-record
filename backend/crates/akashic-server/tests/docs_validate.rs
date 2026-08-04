//! Integration tests for Task 1 (docs-kit Plan 2, B2 addendum): `POST
//! /api/v1/ingest/validate` — the dry-run twin of `POST /api/v1/ingest/publish`
//! (`docs_publish.rs`, Task 7). Same auth, same untar/contract-validation
//! pipeline, same reject envelope — but never inserts a `corpus_versions` row,
//! never spawns a derive job, and never touches the `docs_publish:*`
//! idempotency key a real publish of the same artifact would use.
//!
//! Fixture builders are copied from `docs_publish.rs` (Task 7) rather than
//! shared via a `pub` test-util crate — see that file's doc comment for why
//! the malicious fixtures bypass `tar::Header::set_path`'s own safety net (not
//! exercised by this file, but the builders are copied verbatim to stay in
//! sync).
//!
//! Requires the live Postgres bench (`common::TestEnv::start()`); export
//! `DATABASE_URL`/`TEST_DATABASE_URL` at :5432 (the akashic_test_support
//! `test_pg_pool()` :5433 default is a dead port in this environment).

mod common;

use std::io::Write;

use akashic_domain::types::corpus::CorpusManifest;
use bytes::Bytes;
use serde_json::Value;

// ── fixture builders (copied from docs_publish.rs) ──────────────────────────

fn build_tar_gz(entries: &[(&str, &[u8])]) -> Bytes {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *content).unwrap();
        }
        builder.finish().unwrap();
    }
    gzip(&tar_bytes)
}

fn gzip(raw: &[u8]) -> Bytes {
    let mut gz = Vec::new();
    {
        let mut encoder = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
        encoder.write_all(raw).unwrap();
        encoder.finish().unwrap();
    }
    Bytes::from(gz)
}

fn manifest_json(repo: &str, version: &str, sha: &str, index: &str) -> Vec<u8> {
    serde_json::to_vec(&CorpusManifest {
        repo: repo.to_string(),
        version: version.to_string(),
        sha: sha.to_string(),
        index: index.to_string(),
        languages: vec!["en".to_string()],
        tool_version: "1.0.0".to_string(),
    })
    .unwrap()
}

const GOOD_INDEX: &str = include_str!("../../akashic-domain/tests/fixtures/corpus/good/index.md");
const GOOD_SETUP: &str =
    include_str!("../../akashic-domain/tests/fixtures/corpus/good/guide/setup.md");
const GOOD_USAGE: &str =
    include_str!("../../akashic-domain/tests/fixtures/corpus/good/guide/usage.md");
const BROKEN_LINK_INDEX: &str =
    include_str!("../../akashic-domain/tests/fixtures/corpus/bad_broken_link/index.md");
const BROKEN_LINK_SETUP: &str =
    include_str!("../../akashic-domain/tests/fixtures/corpus/bad_broken_link/guide/setup.md");

fn good_corpus_tar_gz(repo: &str, version: &str, sha: &str) -> Bytes {
    let manifest = manifest_json(repo, version, sha, "index.md");
    build_tar_gz(&[
        ("manifest.json", &manifest),
        ("index.md", GOOD_INDEX.as_bytes()),
        ("guide/setup.md", GOOD_SETUP.as_bytes()),
        ("guide/usage.md", GOOD_USAGE.as_bytes()),
        ("assets/logo.png", &[0x89, 0x50, 0x4E, 0x47, 0xFF, 0xFE]),
    ])
}

fn broken_link_corpus_tar_gz(repo: &str, version: &str, sha: &str) -> Bytes {
    let manifest = manifest_json(repo, version, sha, "index.md");
    build_tar_gz(&[
        ("manifest.json", &manifest),
        ("index.md", BROKEN_LINK_INDEX.as_bytes()),
        ("guide/setup.md", BROKEN_LINK_SETUP.as_bytes()),
    ])
}

// ── helpers ───────────────────────────────────────────────────────────────

async fn issue_token(env: &common::TestEnv, repo: &str) -> String {
    let (_id, token) = env
        .state
        .auth_store
        .issue_publish_token(repo, "test-user")
        .await
        .expect("issue publish token");
    token
}

async fn corpus_version_row_exists(pg: &sqlx::PgPool, repo: &str, sha: &str) -> bool {
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM corpus_versions WHERE repo_name = $1 AND sha = $2")
            .bind(repo)
            .bind(sha)
            .fetch_one(pg)
            .await
            .expect("count corpus_versions");
    n > 0
}

async fn corpus_versions_count_for_repo(pg: &sqlx::PgPool, repo: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM corpus_versions WHERE repo_name = $1")
        .bind(repo)
        .fetch_one(pg)
        .await
        .expect("count corpus_versions");
    n
}

fn validate_url(env: &common::TestEnv) -> String {
    format!("http://{}/api/v1/ingest/validate", env.app_addr)
}

fn publish_url(env: &common::TestEnv) -> String {
    format!("http://{}/api/v1/ingest/publish", env.app_addr)
}

// ── tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial_test::serial]
async fn validate_happy_path_returns_200_and_does_not_persist_version() {
    let env = common::TestEnv::start().await;
    let repo = "docs-validate-test-happy";
    let token = issue_token(&env, repo).await;

    let count_before = corpus_versions_count_for_repo(env.pg_pool(), repo).await;

    let gz = good_corpus_tar_gz(repo, "1.0.0", "validatehappysha1");
    let client = reqwest::Client::new();
    let resp = client
        .post(validate_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST validate");

    assert_eq!(resp.status(), 200, "happy path must return 200");
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["repo"], repo);
    assert_eq!(body["version"], "1.0.0");
    assert_eq!(body["sha"], "validatehappysha1");
    assert_eq!(body["pages"], 3);
    assert_eq!(body["assets"], 1);
    assert!(
        body.get("derive_job_id").is_none(),
        "validate response must not carry publish-only fields, got {body}"
    );
    assert!(
        body.get("replayed").is_none(),
        "validate response must not carry publish-only fields, got {body}"
    );

    let count_after = corpus_versions_count_for_repo(env.pg_pool(), repo).await;
    assert_eq!(
        count_after, count_before,
        "a validate call must never insert a corpus_versions row"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn validate_broken_link_returns_422_with_finding() {
    let env = common::TestEnv::start().await;
    let repo = "docs-validate-test-brokenlink";
    let token = issue_token(&env, repo).await;

    let gz = broken_link_corpus_tar_gz(repo, "1.0.0", "validatebrokensha1");
    let client = reqwest::Client::new();
    let resp = client
        .post(validate_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST validate");

    assert_eq!(resp.status(), 422, "a contract error must reject with 422");
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["findings"][0]["rule"], "broken_link",
        "expected a broken_link finding in the error body, got {body}"
    );
    assert!(!corpus_version_row_exists(env.pg_pool(), repo, "validatebrokensha1").await);
}

#[tokio::test]
#[serial_test::serial]
async fn validate_without_token_returns_401() {
    let env = common::TestEnv::start().await;
    let repo = "docs-validate-test-noauth";

    let gz = good_corpus_tar_gz(repo, "1.0.0", "novalidateauthsha");
    let client = reqwest::Client::new();
    let resp = client
        .post(validate_url(&env))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST validate");

    assert_eq!(resp.status(), 401, "missing bearer must be rejected");
    assert!(
        !corpus_version_row_exists(env.pg_pool(), repo, "novalidateauthsha").await,
        "no version must be persisted on an unauthenticated request"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn validate_repo_mismatch_returns_403() {
    let env = common::TestEnv::start().await;
    let token_repo = "docs-validate-test-token-repo";
    let manifest_repo = "docs-validate-test-manifest-repo";
    let token = issue_token(&env, token_repo).await;

    // Token authorizes `token_repo`; the manifest inside the artifact claims a
    // DIFFERENT repo — must be rejected before anything is persisted.
    let gz = good_corpus_tar_gz(manifest_repo, "1.0.0", "validatemismatchsha");
    let client = reqwest::Client::new();
    let resp = client
        .post(validate_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST validate");

    assert_eq!(
        resp.status(),
        403,
        "token repo != manifest.repo must be rejected"
    );
    assert!(!corpus_version_row_exists(env.pg_pool(), manifest_repo, "validatemismatchsha").await);
}

/// The key dry-run invariant (Task 1's brief): validating an artifact must
/// leave no trace that would interfere with a REAL publish of the exact same
/// artifact afterward — neither a persisted `corpus_versions` row nor a
/// `docs_publish:{repo}:{sha}` idempotency reservation.
#[tokio::test]
#[serial_test::serial]
async fn validate_dry_run_does_not_poison_a_subsequent_publish() {
    let env = common::TestEnv::start().await;
    let repo = "docs-validate-test-no-poison";
    let token = issue_token(&env, repo).await;

    let gz = good_corpus_tar_gz(repo, "1.0.0", "nopoisonsha1");
    let client = reqwest::Client::new();

    let validate_resp = client
        .post(validate_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST validate");
    assert_eq!(
        validate_resp.status(),
        200,
        "validate must return 200 for a good artifact"
    );

    let idem_key = format!("docs_publish:{repo}:nopoisonsha1");
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sagas WHERE idempotency_key = $1")
        .bind(&idem_key)
        .fetch_one(env.pg_pool())
        .await
        .expect("count sagas");
    assert_eq!(
        n, 0,
        "validate must never reserve a docs_publish idempotency key"
    );
    assert!(
        !corpus_version_row_exists(env.pg_pool(), repo, "nopoisonsha1").await,
        "validate must never persist a corpus_versions row"
    );

    let publish_resp = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish");
    assert_eq!(
        publish_resp.status(),
        200,
        "a real publish of the same artifact after a validate dry-run must still succeed"
    );
    let publish_body: Value = publish_resp.json().await.expect("json body");
    assert_eq!(
        publish_body["replayed"], false,
        "publish must be a fresh insert, not a replay, got {publish_body}"
    );

    assert!(
        corpus_version_row_exists(env.pg_pool(), repo, "nopoisonsha1").await,
        "the real publish must persist a corpus_versions row"
    );
}
