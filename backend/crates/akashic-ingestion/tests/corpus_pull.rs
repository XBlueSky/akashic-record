//! Integration tests for Task 9 (B4) pull bootstrap: `.akashic/docs.toml`
//! detection → `maybe_ingest_repo_docs` → the shared `CorpusIngestService`
//! core (Task 5) via `CorpusSource::Pull` — no untar, no CI packer involved.
//!
//! Requires a live Postgres reachable via `DATABASE_URL` (falls back to
//! `akashic_test_support::test_pg_pool`'s :5433 default, which is a dead
//! port in this environment — always export
//! `DATABASE_URL=postgres://akashic:${PG_PASSWORD}@localhost:5432/akashic`
//! when running these), since `ingest_tree` persists for real.

use std::path::Path;
use std::sync::Arc;

use akashic_ingestion::ingestion::corpus::{CorpusIngestService, NoopCorpusDeriveSpawner};
use akashic_ingestion::ingestion::corpus_pull::maybe_ingest_repo_docs;
use akashic_store_pg::repos::PgCorpusStore;
use akashic_test_support::test_pg_pool;
use sqlx::PgPool;
use tempfile::TempDir;

fn service(pool: PgPool) -> CorpusIngestService {
    CorpusIngestService::new(
        Arc::new(PgCorpusStore::new(pool)),
        Arc::new(NoopCorpusDeriveSpawner),
    )
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

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Build a fixture repo checkout tree: `.akashic/docs.toml` (with an
/// unconsumed `[snippets]`/`[check]` section the parser must tolerate) + a
/// C version header + a `docs/` tree including a `docs/superpowers/`
/// subtree the contract's `[corpus].exclude` must drop.
fn build_fixture_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        &root.join(".akashic/docs.toml"),
        r#"
version_header = "include/foo/version.hpp"
docs_root = "docs"
index = "docs/README.md"
languages = ["en"]

[corpus]
exclude = ["superpowers/**"]

[snippets]
enabled = true

[check]
strict = true
"#,
    );

    write(
        &root.join("include/foo/version.hpp"),
        "\
#define FOO_VERSION_MAJOR 1
#define FOO_VERSION_MINOR 2
#define FOO_VERSION_PATCH 3
",
    );

    write(
        &root.join("docs/README.md"),
        "\
# Foo Docs

## All pages

### Guide

- [Setup](guide/setup.md) — Install and configure Foo.
",
    );

    write(
        &root.join("docs/guide/setup.md"),
        "\
# Setup

Install and configure Foo.
",
    );

    write(
        &root.join("docs/superpowers/secret.md"),
        "\
# Secret

Must never be ingested.
",
    );

    dir
}

#[tokio::test]
async fn maybe_ingest_repo_docs_happy_path_persists_pull_version_and_drops_excluded_files() {
    let pool = test_pg_pool().await;
    let repo = "corpus-pull-test-happy";
    clean(&pool, repo).await;
    let svc = service(pool.clone());

    let fixture = build_fixture_repo();

    let accepted = maybe_ingest_repo_docs(&svc, fixture.path(), repo, "deadbeef01")
        .await
        .expect("fixture with a valid .akashic/docs.toml must be ingested");

    assert_eq!(accepted.repo, repo);
    assert_eq!(
        accepted.version, "1.2.3",
        "version must come from the version header fixture"
    );
    assert_eq!(accepted.sha, "deadbeef01");
    assert_eq!(accepted.pages, 2, "docs/README.md + docs/guide/setup.md");
    assert!(
        accepted.warnings.is_empty(),
        "docs_root-prefixed corpus keys (docs/README.md, docs/guide/setup.md) must resolve \
         against the nav's relative links, not produce false orphan_page warnings: {:?}",
        accepted.warnings
    );

    let row: (String, String) = sqlx::query_as(
        "SELECT source, version FROM corpus_versions WHERE repo_name = $1 AND sha = $2",
    )
    .bind(repo)
    .bind("deadbeef01")
    .fetch_one(&pool)
    .await
    .expect("corpus_versions row must exist with source='pull'");
    assert_eq!(row.0, "pull");
    assert_eq!(row.1, "1.2.3");

    let paths: Vec<String> = sqlx::query_scalar(
        "SELECT cf.path FROM corpus_files cf \
         JOIN corpus_versions cv ON cv.id = cf.version_id \
         WHERE cv.repo_name = $1 AND cv.sha = $2",
    )
    .bind(repo)
    .bind("deadbeef01")
    .fetch_all(&pool)
    .await
    .unwrap();

    assert!(paths.contains(&"docs/README.md".to_string()));
    assert!(paths.contains(&"docs/guide/setup.md".to_string()));
    assert!(
        !paths.iter().any(|p| p.contains("superpowers")),
        "excluded docs/superpowers/** files must not be persisted: {paths:?}"
    );

    clean(&pool, repo).await;
}

/// Task 11 E2E finding (real acme docs): a markdown page that embeds a
/// real, existing corpus asset (`![Logo](logo.png)`) must NOT be flagged
/// `broken_link` — `check_links`'s content map used to only ever contain
/// markdown files, so any link to a non-markdown asset (an image, in the
/// common case) was unconditionally treated as unresolved.
#[tokio::test]
async fn maybe_ingest_repo_docs_accepts_markdown_links_to_real_image_assets() {
    let pool = test_pg_pool().await;
    let repo = "corpus-pull-test-image-link";
    clean(&pool, repo).await;
    let svc = service(pool.clone());

    let fixture = build_fixture_repo();
    write(
        &fixture.path().join("docs/guide/setup.md"),
        "\
# Setup

![Logo](../logo.png)

Install and configure Foo.
",
    );
    // A real binary-ish asset (PNG magic bytes) — not valid UTF-8, so it
    // could never have landed in a markdown-only content map anyway.
    std::fs::write(
        fixture.path().join("docs/logo.png"),
        [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
    )
    .unwrap();

    let accepted = maybe_ingest_repo_docs(&svc, fixture.path(), repo, "deadbeef03")
        .await
        .expect(
            "a markdown link to a real, existing corpus image asset must not be rejected \
             as a broken_link contract error",
        );
    assert_eq!(accepted.pages, 2);
    assert_eq!(accepted.assets, 1, "docs/logo.png");

    clean(&pool, repo).await;
}

#[tokio::test]
async fn maybe_ingest_repo_docs_returns_none_without_docs_toml() {
    let pool = test_pg_pool().await;
    let repo = "corpus-pull-test-no-contract";
    clean(&pool, repo).await;
    let svc = service(pool.clone());

    let dir = tempfile::tempdir().unwrap();
    // no .akashic/docs.toml written at all

    let result = maybe_ingest_repo_docs(&svc, dir.path(), repo, "cafebabe02").await;
    assert!(result.is_none());

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM corpus_versions WHERE repo_name = $1")
            .bind(repo)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        count, 0,
        "no .akashic/docs.toml must mean no corpus_versions row"
    );
}
