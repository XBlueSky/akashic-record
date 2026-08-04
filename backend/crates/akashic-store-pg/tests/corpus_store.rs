//! Integration tests for `PgCorpusStore` (Task 4, C2).
//!
//! Requires a live Postgres reachable via `DATABASE_URL` (falls back to
//! `akashic_test_support::test_pg_pool`'s :5433 default, which is a dead
//! port in this environment — always export DATABASE_URL explicitly when
//! running these).

use akashic_domain::ports::corpus::{CorpusStore, InsertOutcome, NewCorpusFile, NewCorpusVersion};
use akashic_domain::types::corpus::{CorpusManifest, CorpusSource, DeriveStatus, NavTree};
use akashic_store_pg::repos::PgCorpusStore;
use akashic_test_support::test_pg_pool;
use sqlx::PgPool;
use uuid::Uuid;

async fn clean(pool: &PgPool, repo_name: &str) {
    // Every test enters through here, so this is the one place that has to
    // bootstrap the corpus tables — CI runs against a bare Postgres that has
    // never seen a migration.
    akashic_store_pg::init_corpus_schema(pool)
        .await
        .expect("init_corpus_schema");
    sqlx::query("DELETE FROM corpus_versions WHERE repo_name = $1")
        .bind(repo_name)
        .execute(pool)
        .await
        .unwrap();
}

fn manifest(repo: &str, version: &str, sha: &str) -> CorpusManifest {
    CorpusManifest {
        repo: repo.to_string(),
        version: version.to_string(),
        sha: sha.to_string(),
        index: "index.md".to_string(),
        languages: vec!["en".to_string()],
        tool_version: "1.0.0".to_string(),
    }
}

fn nav() -> NavTree {
    NavTree {
        description: "test corpus".to_string(),
        groups: vec![],
    }
}

fn new_version(
    repo: &str,
    version: &str,
    sha: &str,
    files: Vec<NewCorpusFile>,
) -> NewCorpusVersion {
    NewCorpusVersion {
        manifest: manifest(repo, version, sha),
        nav: nav(),
        source: CorpusSource::Push,
        is_tagged: false,
        files,
    }
}

#[tokio::test]
async fn insert_version_roundtrips_bytes_exactly_including_non_utf8() {
    let pool = test_pg_pool().await;
    let repo = "corpus-store-test-bytes";
    clean(&pool, repo).await;
    let store = PgCorpusStore::new(pool.clone());

    // Non-UTF-8 asset bytes (0xFF is invalid as a UTF-8 lead byte).
    let asset_bytes = vec![0xFFu8, 0x00, 0x89, 0x50, 0x4E, 0x47, 0xFF, 0xFE];
    let files = vec![
        NewCorpusFile {
            path: "index.md".to_string(),
            content: b"# Hello\n".to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "assets/logo.png".to_string(),
            content: asset_bytes.clone(),
            is_markdown: false,
        },
    ];

    let outcome = store
        .insert_version(new_version(repo, "1.0.0", "aaaaaaa1", files))
        .await
        .unwrap();
    let meta = match outcome {
        InsertOutcome::Inserted(m) => m,
        InsertOutcome::Existing(_) => panic!("expected Inserted on first insert"),
    };

    let file = store
        .get_file(meta.id, "assets/logo.png")
        .await
        .unwrap()
        .expect("asset file must exist");
    assert_eq!(
        file.content, asset_bytes,
        "byte content must round-trip exactly"
    );
    assert_eq!(file.size_bytes, asset_bytes.len() as i64);
    assert!(!file.is_markdown);

    let md_file = store
        .get_file(meta.id, "index.md")
        .await
        .unwrap()
        .expect("markdown file must exist");
    assert_eq!(md_file.content, b"# Hello\n".to_vec());
    assert!(md_file.is_markdown);
}

#[tokio::test]
async fn insert_version_duplicate_repo_sha_returns_existing_idempotently() {
    let pool = test_pg_pool().await;
    let repo = "corpus-store-test-dup";
    clean(&pool, repo).await;
    let store = PgCorpusStore::new(pool.clone());

    let files = vec![NewCorpusFile {
        path: "index.md".to_string(),
        content: b"one".to_vec(),
        is_markdown: true,
    }];

    let first = store
        .insert_version(new_version(repo, "1.0.0", "bbbbbbb2", files.clone()))
        .await
        .unwrap();
    let first_meta = match first {
        InsertOutcome::Inserted(m) => m,
        InsertOutcome::Existing(_) => panic!("expected Inserted on first insert"),
    };

    // Same (repo, sha) replayed — must NOT error, must return Existing with
    // the same version id, and must not create a second corpus_files row set.
    let second = store
        .insert_version(new_version(repo, "1.0.0", "bbbbbbb2", files))
        .await
        .unwrap();
    match second {
        InsertOutcome::Existing(m) => assert_eq!(m.id, first_meta.id),
        InsertOutcome::Inserted(_) => panic!("expected Existing on duplicate (repo, sha) replay"),
    }

    let versions = store.list_versions(repo).await.unwrap();
    assert_eq!(
        versions.len(),
        1,
        "duplicate replay must not create a new row"
    );
}

#[tokio::test]
async fn insert_version_flips_latest_atomically_across_two_versions() {
    let pool = test_pg_pool().await;
    let repo = "corpus-store-test-latest";
    clean(&pool, repo).await;
    let store = PgCorpusStore::new(pool.clone());

    let files = |tag: &str| {
        vec![NewCorpusFile {
            path: "index.md".to_string(),
            content: tag.as_bytes().to_vec(),
            is_markdown: true,
        }]
    };

    let v1 = store
        .insert_version(new_version(repo, "1.0.0", "ccccccc1", files("v1")))
        .await
        .unwrap();
    let v1_id = match v1 {
        InsertOutcome::Inserted(m) => {
            assert!(m.is_latest);
            m.id
        }
        InsertOutcome::Existing(_) => panic!("expected Inserted"),
    };

    let v2 = store
        .insert_version(new_version(repo, "1.1.0", "ccccccc2", files("v2")))
        .await
        .unwrap();
    let v2_id = match v2 {
        InsertOutcome::Inserted(m) => {
            assert!(m.is_latest);
            m.id
        }
        InsertOutcome::Existing(_) => panic!("expected Inserted"),
    };

    let versions = store.list_versions(repo).await.unwrap();
    assert_eq!(versions.len(), 2);
    let latest_flags: Vec<bool> = versions.iter().map(|v| v.is_latest).collect();
    assert_eq!(
        latest_flags.iter().filter(|&&b| b).count(),
        1,
        "exactly one version must be is_latest after two inserts"
    );
    let latest_row = versions.iter().find(|v| v.is_latest).unwrap();
    assert_eq!(
        latest_row.id, v2_id,
        "the second (newer) insert must be latest"
    );
    assert_ne!(v1_id, v2_id);
}

#[tokio::test]
async fn resolve_version_handles_latest_exact_and_sha_prefix() {
    let pool = test_pg_pool().await;
    let repo = "corpus-store-test-resolve";
    clean(&pool, repo).await;
    let store = PgCorpusStore::new(pool.clone());

    let files = vec![NewCorpusFile {
        path: "index.md".to_string(),
        content: b"x".to_vec(),
        is_markdown: true,
    }];

    let v1 = store
        .insert_version(new_version(repo, "1.0.0", "deadbeef0000", files.clone()))
        .await
        .unwrap();
    let v1_id = match v1 {
        InsertOutcome::Inserted(m) => m.id,
        _ => panic!("expected Inserted"),
    };
    let v2 = store
        .insert_version(new_version(repo, "2.0.0", "feedface0000", files))
        .await
        .unwrap();
    let v2_id = match v2 {
        InsertOutcome::Inserted(m) => m.id,
        _ => panic!("expected Inserted"),
    };

    // "latest" keyword resolves to the most recently inserted version.
    let latest = store
        .resolve_version(repo, "latest")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, v2_id);

    // Exact version string.
    let exact = store.resolve_version(repo, "1.0.0").await.unwrap().unwrap();
    assert_eq!(exact.id, v1_id);

    // sha prefix (>= 7 chars).
    let by_sha = store
        .resolve_version(repo, "deadbee")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_sha.id, v1_id);

    // No match at all.
    let none = store.resolve_version(repo, "nope").await.unwrap();
    assert!(none.is_none());
}

#[tokio::test]
async fn list_repos_returns_summary_with_latest_and_count() {
    let pool = test_pg_pool().await;
    let repo = "corpus-store-test-summary";
    clean(&pool, repo).await;
    let store = PgCorpusStore::new(pool.clone());

    let files = vec![NewCorpusFile {
        path: "index.md".to_string(),
        content: b"x".to_vec(),
        is_markdown: true,
    }];
    store
        .insert_version(new_version(repo, "1.0.0", "1111111a", files.clone()))
        .await
        .unwrap();
    store
        .insert_version(new_version(repo, "2.0.0", "2222222b", files))
        .await
        .unwrap();

    let repos = store.list_repos().await.unwrap();
    let summary = repos
        .iter()
        .find(|r| r.repo_name == repo)
        .expect("repo must appear in list_repos");
    assert_eq!(summary.version_count, 2);
    assert_eq!(summary.latest.version, "2.0.0");
}

#[tokio::test]
async fn list_md_paths_returns_only_markdown_files() {
    let pool = test_pg_pool().await;
    let repo = "corpus-store-test-mdpaths";
    clean(&pool, repo).await;
    let store = PgCorpusStore::new(pool.clone());

    let files = vec![
        NewCorpusFile {
            path: "index.md".to_string(),
            content: b"a".to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "guide.md".to_string(),
            content: b"b".to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "assets/logo.png".to_string(),
            content: vec![0xFF, 0xD8],
            is_markdown: false,
        },
    ];

    let meta = match store
        .insert_version(new_version(repo, "1.0.0", "3333333c", files))
        .await
        .unwrap()
    {
        InsertOutcome::Inserted(m) => m,
        _ => panic!("expected Inserted"),
    };

    let mut md_paths = store.list_md_paths(meta.id).await.unwrap();
    md_paths.sort();
    assert_eq!(
        md_paths,
        vec!["guide.md".to_string(), "index.md".to_string()]
    );
}

#[tokio::test]
async fn set_derive_status_updates_status_error_and_job_id() {
    let pool = test_pg_pool().await;
    let repo = "corpus-store-test-derive";
    clean(&pool, repo).await;
    let store = PgCorpusStore::new(pool.clone());

    let files = vec![NewCorpusFile {
        path: "index.md".to_string(),
        content: b"x".to_vec(),
        is_markdown: true,
    }];
    let meta = match store
        .insert_version(new_version(repo, "1.0.0", "4444444d", files))
        .await
        .unwrap()
    {
        InsertOutcome::Inserted(m) => m,
        _ => panic!("expected Inserted"),
    };
    assert_eq!(meta.derive_status, DeriveStatus::Pending);

    let job_id = Uuid::new_v4();
    store
        .set_derive_status(meta.id, DeriveStatus::Running, None, Some(job_id))
        .await
        .unwrap();
    let after_running = store.resolve_version(repo, "1.0.0").await.unwrap().unwrap();
    assert_eq!(after_running.derive_status, DeriveStatus::Running);

    store
        .set_derive_status(meta.id, DeriveStatus::Failed, Some("boom"), Some(job_id))
        .await
        .unwrap();
    let after_failed = store.resolve_version(repo, "1.0.0").await.unwrap().unwrap();
    assert_eq!(after_failed.derive_status, DeriveStatus::Failed);
}
