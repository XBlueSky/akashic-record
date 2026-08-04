//! Integration tests for Task 7 (B2, docs-corpus): `POST
//! /api/v1/ingest/publish` — the docs-kit CI packer's upload endpoint.
//!
//! Fixture builders are copied from `akashic-ingestion/tests/corpus_ingest.rs`
//! (Task 5) rather than shared via a `pub` test-util crate — see that file's
//! doc comment for why the malicious fixtures bypass `tar::Header::set_path`'s
//! own safety net.
//!
//! Requires the live Postgres bench (`common::TestEnv::start()`); export
//! `DATABASE_URL`/`TEST_DATABASE_URL` at :5432 (the akashic_test_support
//! `test_pg_pool()` :5433 default is a dead port in this environment).

mod common;

use std::io::Write;

use akashic_domain::types::corpus::CorpusManifest;
use bytes::Bytes;
use serde_json::Value;

// ── fixture builders (copied from akashic-ingestion/tests/corpus_ingest.rs) ─

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

fn publish_url(env: &common::TestEnv) -> String {
    format!("http://{}/api/v1/ingest/publish", env.app_addr)
}

async fn audit_log_ingest_corpus_count(pg: &sqlx::PgPool, repo: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'ingest_corpus' AND target_id = $1",
    )
    .bind(repo)
    .fetch_one(pg)
    .await
    .expect("count audit_log ingest_corpus rows");
    n
}

/// Task 11: parse a Prometheus counter's current value for a specific label
/// set out of `PrometheusHandle::render()`'s text-exposition output. Returns
/// 0.0 when the metric/label combination hasn't been emitted yet. Callers
/// compare deltas, not absolute values — the recorder is a process-global
/// static shared across every test in this binary, so other tests accumulate
/// into the same counter.
fn counter_value(rendered: &str, name: &str, label_fragment: &str) -> f64 {
    let prefix = format!("{name}{{");
    rendered
        .lines()
        .find(|l| l.starts_with(&prefix) && l.contains(label_fragment))
        .and_then(|l| l.split_whitespace().last())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

// ── tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial_test::serial]
async fn publish_happy_path_returns_200_and_persists_version() {
    let env = common::TestEnv::start().await;
    let repo = "docs-publish-test-happy";
    let token = issue_token(&env, repo).await;

    let gz = good_corpus_tar_gz(repo, "1.0.0", "happysha1");
    let client = reqwest::Client::new();
    let resp = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish");

    assert_eq!(resp.status(), 200, "happy path must return 200");
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["repo"], repo);
    assert_eq!(body["version"], "1.0.0");
    assert_eq!(body["sha"], "happysha1");
    assert_eq!(body["pages"], 3);
    assert_eq!(body["assets"], 1);

    assert!(
        corpus_version_row_exists(env.pg_pool(), repo, "happysha1").await,
        "corpus_versions row must exist after a successful publish"
    );
}

/// Task 11 (E2E/C5 wiring): one successful publish writes exactly one more
/// `audit_log(action='ingest_corpus')` row (the best-effort audit call in
/// `publish`'s step 1, mirroring `publish_tokens::issue_token`/`revoke_token`)
/// and increments `akashic_docs_publish_total{outcome="accepted"}` by
/// exactly 1 — proving the counter registration + emission path is wired,
/// not just that the code compiles.
#[tokio::test]
#[serial_test::serial]
async fn publish_records_one_audit_row_and_increments_accepted_counter() {
    let env = common::TestEnv::start().await;
    let repo = "docs-publish-test-metrics-audit";
    let token = issue_token(&env, repo).await;

    let audit_before = audit_log_ingest_corpus_count(env.pg_pool(), repo).await;
    let rendered_before = env.state.metrics_handle.render();
    let accepted_before = counter_value(
        &rendered_before,
        "akashic_docs_publish_total",
        r#"outcome="accepted""#,
    );

    let gz = good_corpus_tar_gz(repo, "1.0.0", "metricsaudit1");
    let client = reqwest::Client::new();
    let resp = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish");
    assert_eq!(resp.status(), 200, "happy path must return 200");

    let audit_after = audit_log_ingest_corpus_count(env.pg_pool(), repo).await;
    assert_eq!(
        audit_after,
        audit_before + 1,
        "a single publish must write exactly one more audit_log(action='ingest_corpus') row"
    );

    let rendered_after = env.state.metrics_handle.render();
    let accepted_after = counter_value(
        &rendered_after,
        "akashic_docs_publish_total",
        r#"outcome="accepted""#,
    );
    assert_eq!(
        accepted_after,
        accepted_before + 1.0,
        "a successful, non-replayed publish must increment \
         akashic_docs_publish_total{{outcome=\"accepted\"}} by exactly 1, rendered=\n{rendered_after}"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn publish_without_token_returns_401() {
    let env = common::TestEnv::start().await;
    let repo = "docs-publish-test-noauth";

    let gz = good_corpus_tar_gz(repo, "1.0.0", "noauthsha");
    let client = reqwest::Client::new();
    let resp = client
        .post(publish_url(&env))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish");

    assert_eq!(resp.status(), 401, "missing bearer must be rejected");
    assert!(
        !corpus_version_row_exists(env.pg_pool(), repo, "noauthsha").await,
        "no version must be persisted on an unauthenticated request"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn publish_repo_mismatch_returns_403() {
    let env = common::TestEnv::start().await;
    let token_repo = "docs-publish-test-token-repo";
    let manifest_repo = "docs-publish-test-manifest-repo";
    let token = issue_token(&env, token_repo).await;

    // Token authorizes `token_repo`; the manifest inside the artifact claims
    // a DIFFERENT repo — must be rejected before anything is persisted.
    let gz = good_corpus_tar_gz(manifest_repo, "1.0.0", "mismatchsha");
    let client = reqwest::Client::new();
    let resp = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish");

    assert_eq!(
        resp.status(),
        403,
        "token repo != manifest.repo must be rejected"
    );
    assert!(!corpus_version_row_exists(env.pg_pool(), manifest_repo, "mismatchsha").await);

    let idem_key = format!("docs_publish:{manifest_repo}:mismatchsha");
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sagas WHERE idempotency_key = $1")
        .bind(&idem_key)
        .fetch_one(env.pg_pool())
        .await
        .expect("count sagas");
    assert_eq!(
        n, 0,
        "a repo-mismatch request must never leave (or even transiently touch) an \
         idempotency reservation behind"
    );
}

/// Deterministic (non-racy) proof that the repo-mismatch check runs BEFORE
/// the idempotency reserve, not after: a caller holding a valid token for
/// SOME repo could otherwise submit a manifest claiming ANY OTHER repo, and
/// if that repo's idempotency key happens to already be reserved by a real
/// concurrent publish, the mismatched request would surface as a 409
/// (from `idem_reserve` seeing an in-flight reservation) instead of the
/// correct 403 — and would have raced/interfered with an unrelated repo's
/// legitimate publisher's idempotency key in the process. Simulating the
/// "already in flight" state with a direct seed row makes this deterministic
/// instead of depending on true concurrency to reproduce.
#[tokio::test]
#[serial_test::serial]
async fn publish_repo_mismatch_returns_403_even_when_manifest_repo_key_is_in_flight() {
    let env = common::TestEnv::start().await;
    let token_repo = "docs-publish-test-early-gate-token-repo";
    let manifest_repo = "docs-publish-test-early-gate-manifest-repo";
    let token = issue_token(&env, token_repo).await;
    let sha = "earlygatesha1";

    // Simulate a legitimate publish for `manifest_repo` currently in flight
    // (e.g. a real concurrent request that reserved the key first).
    let idem_key = format!("docs_publish:{manifest_repo}:{sha}");
    sqlx::query(
        "INSERT INTO sagas (id, saga_type, idempotency_key, repo_name, status) \
         VALUES (gen_random_uuid(), 'docs_publish', $1, $2, 'running')",
    )
    .bind(&idem_key)
    .bind(manifest_repo)
    .execute(env.pg_pool())
    .await
    .expect("seed in-flight reservation");

    // Token authorizes `token_repo`; manifest claims `manifest_repo` (the
    // repo whose key is already "in flight" above) — must be rejected as a
    // repo mismatch WITHOUT ever calling idempotency_reserve on that
    // unrelated key.
    let gz = good_corpus_tar_gz(manifest_repo, "1.0.0", sha);
    let client = reqwest::Client::new();
    let resp = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish");

    assert_eq!(
        resp.status(),
        403,
        "repo mismatch must be rejected before the idempotency reserve even runs, \
         not surfaced as a 409 from an unrelated in-flight reservation"
    );

    let (status,): (String,) =
        sqlx::query_as("SELECT status FROM sagas WHERE idempotency_key = $1")
            .bind(&idem_key)
            .fetch_one(env.pg_pool())
            .await
            .expect("fetch seeded saga row");
    assert_eq!(
        status, "running",
        "the mismatched request must not touch the pre-existing (unrelated) reservation"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn publish_broken_link_returns_422_with_finding() {
    let env = common::TestEnv::start().await;
    let repo = "docs-publish-test-brokenlink";
    let token = issue_token(&env, repo).await;

    let gz = broken_link_corpus_tar_gz(repo, "1.0.0", "brokensha1");
    let client = reqwest::Client::new();
    let resp = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish");

    assert_eq!(resp.status(), 422, "a contract error must reject with 422");
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["findings"][0]["rule"], "broken_link",
        "expected a broken_link finding in the error body, got {body}"
    );
    assert!(!corpus_version_row_exists(env.pg_pool(), repo, "brokensha1").await);
}

#[tokio::test]
#[serial_test::serial]
async fn publish_concurrent_same_sha_reserve_collision_returns_409() {
    let env = common::TestEnv::start().await;
    let repo = "docs-publish-test-concurrent";
    let token = issue_token(&env, repo).await;

    let gz = good_corpus_tar_gz(repo, "1.0.0", "concurrentsha");
    let client = reqwest::Client::new();

    let req_a = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send();
    let req_b = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send();

    let (resp_a, resp_b) = tokio::join!(req_a, req_b);
    let status_a = resp_a.expect("POST publish A").status();
    let status_b = resp_b.expect("POST publish B").status();

    let statuses = [status_a.as_u16(), status_b.as_u16()];
    assert!(
        statuses.contains(&200) && statuses.contains(&409),
        "exactly one concurrent same-sha publish must reserve (200) and the other \
         must be turned away (409), got {statuses:?}"
    );

    assert!(corpus_version_row_exists(env.pg_pool(), repo, "concurrentsha").await);
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM corpus_versions WHERE repo_name = $1 AND sha = $2")
            .bind(repo)
            .bind("concurrentsha")
            .fetch_one(env.pg_pool())
            .await
            .expect("count corpus_versions");
    assert_eq!(n, 1, "exactly one version row must be inserted, not two");
}

#[tokio::test]
#[serial_test::serial]
async fn publish_replay_same_sha_after_key_expiry_reports_replayed_true() {
    let env = common::TestEnv::start().await;
    let repo = "docs-publish-test-replay";
    let token = issue_token(&env, repo).await;

    let gz = good_corpus_tar_gz(repo, "1.0.0", "replaysha1");
    let client = reqwest::Client::new();

    let first = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish (first)");
    assert_eq!(first.status(), 200);
    let first_body: Value = first.json().await.expect("json body");
    assert_eq!(first_body["replayed"], false);

    // Simulate the idempotency-key cache having expired/been cleared (e.g. a
    // long time later) while the corpus_versions row for this (repo, sha)
    // still exists — the SECOND publish must not error, and the store's own
    // idempotent-insert (InsertOutcome::Existing) must report replayed=true.
    sqlx::query("DELETE FROM sagas WHERE idempotency_key = $1")
        .bind(format!("docs_publish:{repo}:replaysha1"))
        .execute(env.pg_pool())
        .await
        .expect("clear idempotency row");

    let second = client
        .post(publish_url(&env))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/gzip")
        .body(gz.to_vec())
        .send()
        .await
        .expect("POST publish (second)");
    assert_eq!(
        second.status(),
        200,
        "resubmitting the same sha must not error"
    );
    let second_body: Value = second.json().await.expect("json body");
    assert_eq!(second_body["sha"], "replaysha1");
    assert_eq!(
        second_body["replayed"], true,
        "the store-level idempotent-insert must be surfaced once the HTTP idem \
         cache no longer short-circuits, got {second_body}"
    );

    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM corpus_versions WHERE repo_name = $1 AND sha = $2")
            .bind(repo)
            .bind("replaysha1")
            .fetch_one(env.pg_pool())
            .await
            .expect("count corpus_versions");
    assert_eq!(n, 1, "a replay must not insert a second row");
}
