//! Integration tests for `CorpusIngestService` (Task 5, B2 core).
//!
//! `ingest_artifact` is the untrusted-input entry point of the whole
//! docs-kit corpus feature: these tests build malicious/malformed
//! `corpus.tar.gz` fixtures BY HAND (bypassing `tar::Header::set_path`'s own
//! safety net where needed — see `set_raw_path` below) so the tar-safety
//! assertions actually exercise the service's own defenses, not the `tar`
//! crate's writer-side conveniences.
//!
//! Requires a live Postgres reachable via `DATABASE_URL` (falls back to
//! `akashic_test_support::test_pg_pool`'s :5433 default, which is a dead
//! port in this environment — always export DATABASE_URL explicitly when
//! running these), since `insert_version` computes counts/hashes for real.

use std::io::Write;

use akashic_domain::ports::corpus::{CorpusStore, NewCorpusFile};
use akashic_domain::types::corpus::{CorpusManifest, CorpusSource, Severity};
use akashic_ingestion::ingestion::corpus::{
    CorpusIngestService, NoopCorpusDeriveSpawner, RejectCode, RetryDeriveOutcome, extract_tar_gz,
};
use akashic_store_pg::repos::PgCorpusStore;
use akashic_test_support::test_pg_pool;
use bytes::Bytes;
use sqlx::PgPool;
use std::sync::Arc;

// ── fixture builders ─────────────────────────────────────────────────────

/// Build a well-formed `corpus.tar.gz` from `(path, content)` entries using
/// the `tar` crate's normal (path-validating) writer API.
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

/// Overwrite a GNU header's raw 100-byte `name` field directly, bypassing
/// `Header::set_path`'s own `..`/absolute-path rejection. A real attacker's
/// tar writer isn't `tar-rs` and has no reason to honor that safety net —
/// this is the only way to build a fixture that actually proves the
/// extraction code (not the writer library) rejects the path.
fn set_raw_path(header: &mut tar::Header, raw: &[u8]) {
    let gnu = header.as_gnu_mut().expect("gnu header");
    gnu.name = [0u8; 100];
    gnu.name[..raw.len()].copy_from_slice(raw);
}

/// A tar.gz with exactly one malicious/oversized entry appended via a raw
/// header (see [`set_raw_path`]), used for the path-traversal fixture.
fn build_tar_gz_with_raw_path(raw_path: &[u8], content: &[u8]) -> Bytes {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        set_raw_path(&mut header, raw_path);
        header.set_cksum();
        builder.append(&header, content).unwrap();
    }
    gzip(&tar_bytes)
}

/// A tar.gz with a single absolute-path entry, built via the `tar` crate's
/// own `set_path_absolute` (a legitimate public API — unlike `..`, absolute
/// paths ARE constructible without bypassing the writer's validation).
fn build_tar_gz_with_absolute_path(abs_path: &str, content: &[u8]) -> Bytes {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_path_absolute(abs_path).unwrap();
        header.set_cksum();
        builder.append(&header, content).unwrap();
    }
    gzip(&tar_bytes)
}

/// A tar.gz with a single symlink entry (own path safe, link target
/// irrelevant — the entry TYPE alone must be rejected).
fn build_tar_gz_with_symlink() -> Bytes {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_path("evil-link.md").unwrap();
        header.set_link_name("/etc/passwd").unwrap();
        header.set_cksum();
        builder.append(&header, &[][..]).unwrap();
    }
    gzip(&tar_bytes)
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
const ORPHAN_INDEX: &str =
    include_str!("../../akashic-domain/tests/fixtures/corpus/bad_orphan/index.md");
const ORPHAN_SETUP: &str =
    include_str!("../../akashic-domain/tests/fixtures/corpus/bad_orphan/guide/setup.md");
const ORPHAN_ORPHAN: &str =
    include_str!("../../akashic-domain/tests/fixtures/corpus/bad_orphan/guide/orphan.md");

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

async fn clean(pool: &PgPool, repo_name: &str) {
    // Bootstrap here: every test enters through clean(), and CI runs against a
    // bare Postgres that has never seen a migration.
    akashic_store_pg::init_corpus_schema(pool)
        .await
        .expect("init_corpus_schema");
    sqlx::query("DELETE FROM corpus_versions WHERE repo_name = $1")
        .bind(repo_name)
        .execute(pool)
        .await
        .unwrap();
}

fn service(pool: PgPool) -> CorpusIngestService {
    CorpusIngestService::new(
        Arc::new(PgCorpusStore::new(pool)),
        Arc::new(NoopCorpusDeriveSpawner),
    )
}

// ── happy path ────────────────────────────────────────────────────────────

#[tokio::test]
async fn ingest_artifact_happy_path_returns_accepted_with_correct_counts() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-happy";
    clean(&pool, repo).await;
    let svc = service(pool);

    let gz = good_corpus_tar_gz(repo, "1.0.0", "aaaaaaa1");
    let accepted = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect("happy-path artifact must be accepted");

    assert_eq!(accepted.repo, repo);
    assert_eq!(accepted.version, "1.0.0");
    assert_eq!(accepted.sha, "aaaaaaa1");
    assert_eq!(
        accepted.pages, 3,
        "index.md + guide/setup.md + guide/usage.md"
    );
    assert_eq!(accepted.assets, 1, "assets/logo.png");
    assert!(accepted.warnings.is_empty());
    assert!(!accepted.replayed);
}

// ── Task 7 seam: extract once, feed the manifest peek + the ingest ──────

/// Task 7's HTTP publish handler needs `manifest.repo`/`manifest.sha`
/// (to build the idempotency key and to peek before reserving) BEFORE
/// calling into the store. Rather than duplicating the tar-safety
/// extraction logic in the HTTP crate, it calls `extract_tar_gz` itself
/// (inside `spawn_blocking`) and hands the already-extracted artifact to
/// `ingest_extracted` — extraction runs exactly once. This must produce
/// the identical outcome as `ingest_artifact` (which is now just
/// `extract_tar_gz` + `ingest_extracted` composed together).
#[tokio::test]
async fn ingest_extracted_after_separate_extract_matches_ingest_artifact() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-extracted-seam";
    clean(&pool, repo).await;
    let svc = service(pool);

    let gz = good_corpus_tar_gz(repo, "1.0.0", "seedseed");
    let extracted = extract_tar_gz(&gz).expect("well-formed fixture must extract");
    assert_eq!(extracted.manifest.repo, repo);
    assert_eq!(extracted.manifest.sha, "seedseed");

    let accepted = svc
        .ingest_extracted(extracted, repo, false)
        .await
        .expect("pre-extracted well-formed artifact must be accepted");

    assert_eq!(accepted.repo, repo);
    assert_eq!(accepted.sha, "seedseed");
    assert_eq!(accepted.pages, 3);
    assert_eq!(accepted.assets, 1);
    assert!(!accepted.replayed);
}

/// The repo-mismatch check must still fire when going through the
/// extract-then-ingest_extracted seam (not just the all-in-one
/// `ingest_artifact` path) — `ingest_extracted` re-checks
/// `manifest.repo == expected_repo` itself rather than trusting the
/// caller to have already checked it.
#[tokio::test]
async fn ingest_extracted_rejects_repo_mismatch() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-extracted-mismatch";
    clean(&pool, repo).await;
    let svc = service(pool);

    let gz = good_corpus_tar_gz("some-other-repo", "1.0.0", "mismatch1");
    let extracted = extract_tar_gz(&gz).expect("well-formed fixture must extract");

    let rejection = svc
        .ingest_extracted(extracted, repo, false)
        .await
        .expect_err("manifest.repo != expected_repo must be rejected");

    assert_eq!(rejection.code, RejectCode::RepoMismatch);
}

#[tokio::test]
async fn ingest_artifact_replay_same_repo_sha_returns_existing_and_replayed_true() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-replay";
    clean(&pool, repo).await;
    let svc = service(pool);

    let gz1 = good_corpus_tar_gz(repo, "1.0.0", "bbbbbbb2");
    let first = svc.ingest_artifact(gz1, repo, false).await.unwrap();
    assert!(!first.replayed);

    let gz2 = good_corpus_tar_gz(repo, "1.0.0", "bbbbbbb2");
    let second = svc.ingest_artifact(gz2, repo, false).await.unwrap();
    assert!(
        second.replayed,
        "re-submitting the same (repo, sha) must not error"
    );
    assert_eq!(second.sha, "bbbbbbb2");
}

// ── tar-safety rejections ────────────────────────────────────────────────

#[tokio::test]
async fn ingest_artifact_rejects_symlink_entry() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-symlink";
    clean(&pool, repo).await;
    let svc = service(pool);

    let gz = build_tar_gz_with_symlink();
    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("symlink entries must be rejected");

    assert_eq!(rejection.code, RejectCode::Contract);
    assert!(
        rejection.findings.iter().any(|f| f.rule == "unsafe_entry"),
        "expected an unsafe_entry finding, got {:?}",
        rejection.findings
    );
}

#[tokio::test]
async fn ingest_artifact_rejects_parent_dir_traversal_entry() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-traversal";
    clean(&pool, repo).await;
    let svc = service(pool);

    let gz = build_tar_gz_with_raw_path(b"../../etc/evil.md", b"# evil\n");
    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("\"..\" traversal entries must be rejected");

    assert_eq!(rejection.code, RejectCode::Contract);
    assert!(
        rejection.findings.iter().any(|f| f.rule == "unsafe_entry"),
        "expected an unsafe_entry finding, got {:?}",
        rejection.findings
    );
}

#[tokio::test]
async fn ingest_artifact_rejects_nested_parent_dir_traversal_entry() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-traversal-nested";
    clean(&pool, repo).await;
    let svc = service(pool);

    // ".." not in the leading position — must still be caught (every path
    // COMPONENT is checked, not just a string-prefix check).
    let gz = build_tar_gz_with_raw_path(b"assets/../../secret.md", b"# evil\n");
    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("nested \"..\" traversal entries must be rejected");

    assert_eq!(rejection.code, RejectCode::Contract);
    assert!(rejection.findings.iter().any(|f| f.rule == "unsafe_entry"));
}

#[tokio::test]
async fn ingest_artifact_rejects_absolute_path_entry() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-absolute";
    clean(&pool, repo).await;
    let svc = service(pool);

    let gz = build_tar_gz_with_absolute_path("/etc/passwd", b"root:x:0:0\n");
    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("absolute-path entries must be rejected");

    assert_eq!(rejection.code, RejectCode::Contract);
    assert!(
        rejection.findings.iter().any(|f| f.rule == "unsafe_entry"),
        "expected an unsafe_entry finding, got {:?}",
        rejection.findings
    );
}

#[tokio::test]
async fn ingest_artifact_rejects_single_file_over_cap() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-single-file-cap";
    clean(&pool, repo).await;
    let svc = service(pool);

    // 8 MiB cap + 1 byte, highly compressible so the .tar.gz itself stays small.
    let big = vec![b'a'; 8 * 1024 * 1024 + 1];
    let gz = build_tar_gz(&[("assets/big.bin", &big)]);
    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("a single file over the 8 MiB cap must be rejected");

    assert_eq!(rejection.code, RejectCode::TooLarge);
    assert!(rejection.findings.iter().any(|f| f.rule == "too_large"));
}

#[tokio::test]
async fn ingest_artifact_rejects_too_many_entries() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-entry-count";
    clean(&pool, repo).await;
    let svc = service(pool);

    let owned: Vec<(String, Vec<u8>)> = (0..2001)
        .map(|i| (format!("f{i:05}.md"), b"x".to_vec()))
        .collect();
    let entries: Vec<(&str, &[u8])> = owned
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let gz = build_tar_gz(&entries);

    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("more than 2000 entries must be rejected");

    assert_eq!(rejection.code, RejectCode::TooLarge);
    assert!(rejection.findings.iter().any(|f| f.rule == "too_large"));
}

#[tokio::test]
async fn ingest_artifact_rejects_total_size_over_cap() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-total-cap";
    clean(&pool, repo).await;
    let svc = service(pool);

    // 10 files x 7 MiB (each under the 8 MiB single-file cap) = 70 MiB,
    // over the 64 MiB total cap — a decompression-bomb SHAPE (many
    // compressible files) that only the cumulative check can catch.
    let chunk = vec![b'z'; 7 * 1024 * 1024];
    let owned: Vec<(String, Vec<u8>)> = (0..10)
        .map(|i| (format!("big{i}.bin"), chunk.clone()))
        .collect();
    let entries: Vec<(&str, &[u8])> = owned
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let gz = build_tar_gz(&entries);

    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("total uncompressed size over the 64 MiB cap must be rejected");

    assert_eq!(rejection.code, RejectCode::TooLarge);
    assert!(rejection.findings.iter().any(|f| f.rule == "too_large"));
}

// ── contract validation ──────────────────────────────────────────────────

#[tokio::test]
async fn ingest_artifact_rejects_non_utf8_markdown() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-non-utf8";
    clean(&pool, repo).await;
    let svc = service(pool);

    let manifest = manifest_json(repo, "1.0.0", "cccccccc", "index.md");
    let bad_bytes: &[u8] = &[b'#', b' ', 0xFF, 0xFE, b'\n'];
    let gz = build_tar_gz(&[("manifest.json", &manifest), ("index.md", bad_bytes)]);

    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("non-UTF-8 markdown content must be rejected");

    assert_eq!(rejection.code, RejectCode::Contract);
    assert!(
        rejection.findings.iter().any(|f| f.rule == "invalid_utf8"),
        "expected an invalid_utf8 finding, got {:?}",
        rejection.findings
    );
}

#[tokio::test]
async fn ingest_artifact_rejects_repo_mismatch() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-repo-mismatch";
    clean(&pool, repo).await;
    let svc = service(pool);

    let gz = good_corpus_tar_gz("some-other-repo", "1.0.0", "dddddddd");
    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("manifest.repo != expected_repo must be rejected");

    assert_eq!(rejection.code, RejectCode::RepoMismatch);
}

#[tokio::test]
async fn ingest_artifact_rejects_manifest_missing_required_field() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-manifest-schema";
    clean(&pool, repo).await;
    let svc = service(pool);

    // Missing `sha` (required, deny_unknown_fields) — a hand-built JSON
    // object rather than CorpusManifest, to genuinely exercise the schema
    // check rather than being unable to construct the omission via the
    // typed struct.
    let bad_manifest = serde_json::json!({
        "repo": repo,
        "version": "1.0.0",
        "index": "index.md",
        "languages": ["en"],
        "tool_version": "1.0.0",
    });
    let manifest_bytes = serde_json::to_vec(&bad_manifest).unwrap();
    let gz = build_tar_gz(&[
        ("manifest.json", &manifest_bytes),
        ("index.md", GOOD_INDEX.as_bytes()),
    ]);

    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("a manifest missing a required field must be rejected");

    assert_eq!(rejection.code, RejectCode::Schema);
}

#[tokio::test]
async fn ingest_artifact_rejects_broken_link_contract_error() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-broken-link";
    clean(&pool, repo).await;
    let svc = service(pool);

    let manifest = manifest_json(repo, "1.0.0", "eeeeeeee", "index.md");
    let gz = build_tar_gz(&[
        ("manifest.json", &manifest),
        ("index.md", BROKEN_LINK_INDEX.as_bytes()),
        ("guide/setup.md", BROKEN_LINK_SETUP.as_bytes()),
    ]);

    let rejection = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect_err("a Severity::Error link finding must reject the whole artifact");

    assert_eq!(rejection.code, RejectCode::Contract);
    assert!(
        rejection
            .findings
            .iter()
            .any(|f| f.rule == "broken_link" && f.severity == Severity::Error),
        "expected a broken_link error finding, got {:?}",
        rejection.findings
    );
}

#[tokio::test]
async fn ingest_artifact_accepts_with_warnings_for_orphan_page() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-orphan-warn";
    clean(&pool, repo).await;
    let svc = service(pool);

    let manifest = manifest_json(repo, "1.0.0", "ffffffff", "index.md");
    let gz = build_tar_gz(&[
        ("manifest.json", &manifest),
        ("index.md", ORPHAN_INDEX.as_bytes()),
        ("guide/setup.md", ORPHAN_SETUP.as_bytes()),
        ("guide/orphan.md", ORPHAN_ORPHAN.as_bytes()),
    ]);

    let accepted = svc
        .ingest_artifact(gz, repo, false)
        .await
        .expect("a Severity::Warn-only finding must still be accepted");

    assert_eq!(accepted.pages, 3);
    assert!(
        accepted
            .warnings
            .iter()
            .any(|f| f.rule == "orphan_page" && f.severity == Severity::Warn),
        "expected an orphan_page warning, got {:?}",
        accepted.warnings
    );
}

// ── ingest_tree (pull path) ──────────────────────────────────────────────

#[tokio::test]
async fn ingest_tree_happy_path_persists_without_untar() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-tree-happy";
    clean(&pool, repo).await;
    let svc = service(pool);

    let manifest = CorpusManifest {
        repo: repo.to_string(),
        version: "1.0.0".to_string(),
        sha: "0000000a".to_string(),
        index: "index.md".to_string(),
        languages: vec!["en".to_string()],
        tool_version: "1.0.0".to_string(),
    };
    let files = vec![
        NewCorpusFile {
            path: "index.md".to_string(),
            content: GOOD_INDEX.as_bytes().to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "guide/setup.md".to_string(),
            content: GOOD_SETUP.as_bytes().to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "guide/usage.md".to_string(),
            content: GOOD_USAGE.as_bytes().to_vec(),
            is_markdown: true,
        },
    ];

    let accepted = svc
        .ingest_tree(manifest, files, CorpusSource::Pull)
        .await
        .expect("ingest_tree happy path must be accepted");

    assert_eq!(accepted.repo, repo);
    assert_eq!(accepted.pages, 3);
    assert_eq!(accepted.assets, 0);
    assert!(!accepted.replayed);
}

#[tokio::test]
async fn ingest_tree_rejects_broken_link_contract_error() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-tree-broken-link";
    clean(&pool, repo).await;
    let svc = service(pool);

    let manifest = CorpusManifest {
        repo: repo.to_string(),
        version: "1.0.0".to_string(),
        sha: "0000000b".to_string(),
        index: "index.md".to_string(),
        languages: vec!["en".to_string()],
        tool_version: "1.0.0".to_string(),
    };
    let files = vec![
        NewCorpusFile {
            path: "index.md".to_string(),
            content: BROKEN_LINK_INDEX.as_bytes().to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "guide/setup.md".to_string(),
            content: BROKEN_LINK_SETUP.as_bytes().to_vec(),
            is_markdown: true,
        },
    ];

    let rejection = svc
        .ingest_tree(manifest, files, CorpusSource::Pull)
        .await
        .expect_err("ingest_tree must also enforce the link contract");

    assert_eq!(rejection.code, RejectCode::Contract);
}

// ── Task 8 (B3): retry_derive seam ────────────────────────────────────────

#[tokio::test]
async fn retry_derive_returns_not_found_for_a_repo_with_no_ingested_version() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-retry-not-found";
    clean(&pool, repo).await;
    let svc = service(pool);

    let outcome = svc
        .retry_derive(repo)
        .await
        .expect("retry_derive must not error for an unknown repo");

    assert!(matches!(outcome, RetryDeriveOutcome::NotFound));
}

#[tokio::test]
async fn retry_derive_spawns_against_the_latest_version_when_repo_exists() {
    let pool = test_pg_pool().await;
    let repo = "corpus-ingest-test-retry-spawns";
    clean(&pool, repo).await;
    let store = PgCorpusStore::new(pool.clone());
    let svc = service(pool);

    svc.ingest_artifact(good_corpus_tar_gz(repo, "1.0.0", "retrysha1"), repo, false)
        .await
        .expect("seed artifact must be accepted");

    let expected_latest = store
        .resolve_version(repo, "latest")
        .await
        .expect("resolve_version must not error")
        .expect("a version was just ingested");

    let outcome = svc
        .retry_derive(repo)
        .await
        .expect("retry_derive must not error for an existing repo");

    match outcome {
        RetryDeriveOutcome::Spawned {
            version_id,
            derive_job_id,
        } => {
            assert_eq!(
                version_id, expected_latest.id,
                "retry_derive must target the repo's latest version"
            );
            // NoopCorpusDeriveSpawner (this test's service) always declines.
            assert!(derive_job_id.is_none());
        }
        RetryDeriveOutcome::NotFound => {
            panic!("expected Spawned for a repo with an ingested version")
        }
    }
}
