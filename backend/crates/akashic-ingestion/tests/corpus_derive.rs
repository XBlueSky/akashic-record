//! Integration tests for Task 8 (B3): the async derive job that turns a
//! stored raw corpus version into the searchable Doc space (documents +
//! sections + embeddings, Neo4j graph nodes, EXPLAINS anchoring).
//!
//! Requires live Postgres (`DATABASE_URL`, the :5432 dev instance — NOT
//! `test_pg_pool`'s dead :5433 default) and Neo4j (`NEO4J_URI`, defaults to
//! `bolt://localhost:7687`), since sections are stored (and embedded, and
//! EXPLAINS-linked) for real. Corpus versions are seeded directly via
//! `PgCorpusStore::insert_version` (Task 5's store, not the tar/contract
//! pipeline) — this test is about the derive job, not re-proving publish
//! validation `corpus_ingest.rs` already covers.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::IngestionJobRepo;
use akashic_domain::ports::corpus::{CorpusStore, InsertOutcome, NewCorpusFile, NewCorpusVersion};
use akashic_domain::types::corpus::{CorpusManifest, CorpusSource, NavGroup, NavPage, NavTree};
use akashic_embed::{EmbeddingProvider, EmbeddingResponse};
use akashic_ingestion::ingestion::corpus_derive::{DeriveDeps, run_derive};
use akashic_ingestion::ingestion::doc_store::DocStore;
use akashic_store_neo4j::Neo4jPool;
use akashic_store_pg::repos::{PgCorpusStore, PgIngestionJobRepo};
use akashic_test_support::test_pg_pool;

/// Matches the live `vector({DIM})` column width used across `chunks` /
/// `sections` in this dev DB (see `akashic-ingestion::ingestion::e2e_test`'s
/// identical constant + rationale).
const DIM: usize = 1536;

/// Deterministic fake embedder — same recipe as `e2e_test::FakeEmbedder`,
/// duplicated here because that one lives in a private `#[cfg(test)] mod`
/// inside the crate and isn't reachable from this external `tests/` binary.
struct FakeEmbedder;

#[async_trait]
impl EmbeddingProvider for FakeEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<EmbeddingResponse> {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let mut v = Vec::with_capacity(DIM);
        let mut state = h | 1;
        for _ in 0..DIM {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            v.push(((state % 1000) as f32) / 1000.0 + 0.001);
        }
        Ok(EmbeddingResponse {
            vector: v,
            tokens_used: 0,
            model: "fake".into(),
        })
    }
    fn dimensions(&self) -> usize {
        DIM
    }
}

/// Embedder that always errors — the failure-injection fixture for the
/// "derive failure preserves the raw layer" test. `store_sections` calls
/// `embed()` before any section row is written, so this fails the very
/// first section of the very first page.
struct FailingEmbedder;

#[async_trait]
impl EmbeddingProvider for FailingEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<EmbeddingResponse> {
        Err(anyhow::anyhow!("injected embedder failure"))
    }
    fn dimensions(&self) -> usize {
        DIM
    }
}

async fn neo4j_pool() -> Neo4jPool {
    let mut cfg = akashic_test_support::test_config_minimal();
    cfg.neo4j_uri =
        std::env::var("NEO4J_URI").unwrap_or_else(|_| "bolt://localhost:7687".to_string());
    Neo4jPool::connect(&cfg).await.expect("Neo4jPool::connect")
}

async fn clean(pg: &PgPool, neo4j: &Neo4jPool, repo: &str) {
    sqlx::query("DELETE FROM corpus_versions WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .unwrap();
    sqlx::query("DELETE FROM documents WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .unwrap();
    sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ingestion_jobs WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .unwrap();
    neo4j
        .execute(
            neo4rs::query(
                "MATCH (d:Document {repo_name: $repo}) \
                 OPTIONAL MATCH (d)-[:HAS_SECTION|HAS_SUBSECTION*]->(s) \
                 DETACH DELETE d, s",
            )
            .param("repo", repo),
        )
        .await
        .ok();
    neo4j
        .execute(
            neo4rs::query("MATCH (c:Chunk {repo_name: $repo}) DETACH DELETE c").param("repo", repo),
        )
        .await
        .ok();
}

fn manifest(repo: &str, sha: &str) -> CorpusManifest {
    CorpusManifest {
        repo: repo.to_string(),
        version: "1.0.0".to_string(),
        sha: sha.to_string(),
        index: "index.md".to_string(),
        languages: vec!["en".to_string()],
        tool_version: "1.0.0".to_string(),
    }
}

fn nav() -> NavTree {
    NavTree {
        description: "test corpus".to_string(),
        groups: vec![NavGroup {
            title: "Guide".to_string(),
            pages: vec![NavPage {
                title: "Index".to_string(),
                path: "index.md".to_string(),
                description: String::new(),
            }],
        }],
    }
}

fn md_file(path: &str, content: &str) -> NewCorpusFile {
    NewCorpusFile {
        path: path.to_string(),
        content: content.as_bytes().to_vec(),
        is_markdown: true,
    }
}

async fn seed_version(
    store: &PgCorpusStore,
    repo: &str,
    sha: &str,
    files: Vec<NewCorpusFile>,
) -> Uuid {
    let v = NewCorpusVersion {
        manifest: manifest(repo, sha),
        nav: nav(),
        source: CorpusSource::Push,
        is_tagged: false,
        files,
    };
    match store.insert_version(v).await.unwrap() {
        InsertOutcome::Inserted(meta) | InsertOutcome::Existing(meta) => meta.id,
    }
}

fn deps(pg: PgPool, neo4j: Neo4jPool, embedder: Arc<dyn EmbeddingProvider>) -> DeriveDeps {
    DeriveDeps {
        corpus_store: Arc::new(PgCorpusStore::new(pg.clone())),
        doc_store: Arc::new(DocStore::new(pg.clone(), neo4j, embedder, None)),
        ingestion_job_repo: Arc::new(PgIngestionJobRepo::new(pg)),
        job_id: None,
    }
}

// ── happy path ────────────────────────────────────────────────────────────

#[tokio::test]
async fn derive_creates_one_corpus_document_per_md_page_with_sections() {
    let pg = test_pg_pool().await;
    let neo4j = neo4j_pool().await;
    let repo = "corpus-derive-test-happy";
    clean(&pg, &neo4j, repo).await;

    let store = PgCorpusStore::new(pg.clone());
    let version_id = seed_version(
        &store,
        repo,
        "sha0001",
        vec![
            md_file(
                "index.md",
                "# Welcome\n\nIntro text.\n\n## Setup\n\nSetup text.",
            ),
            md_file("guide/usage.md", "# Usage\n\nHow to use it."),
        ],
    )
    .await;

    run_derive(
        deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder)),
        version_id,
    )
    .await
    .expect("derive should succeed");

    let docs: Vec<(String,)> =
        sqlx::query_as("SELECT doc_type FROM documents WHERE repo_name = $1 ORDER BY title")
            .bind(repo)
            .fetch_all(&pg)
            .await
            .unwrap();
    assert_eq!(docs.len(), 2, "one document per md page");
    assert!(
        docs.iter().all(|(doc_type,)| doc_type == "corpus"),
        "every corpus-derived document must be doc_type='corpus'"
    );

    let section_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sections s JOIN documents d ON s.doc_id = d.id WHERE d.repo_name = $1",
    )
    .bind(repo)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(section_count, 3, "Welcome + Setup (child) + Usage");

    let (status,): (String,) =
        sqlx::query_as("SELECT derive_status FROM corpus_versions WHERE id = $1")
            .bind(version_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(status, "complete");
}

#[tokio::test]
async fn second_derive_run_does_not_leave_stale_sections() {
    let pg = test_pg_pool().await;
    let neo4j = neo4j_pool().await;
    let repo = "corpus-derive-test-rerun";
    clean(&pg, &neo4j, repo).await;

    let store = PgCorpusStore::new(pg.clone());
    let version_id = seed_version(
        &store,
        repo,
        "sha0002",
        vec![md_file("index.md", "# Hello\n\nBody text.")],
    )
    .await;

    for _ in 0..2 {
        run_derive(
            deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder)),
            version_id,
        )
        .await
        .expect("derive should succeed");
    }

    let section_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sections s JOIN documents d ON s.doc_id = d.id WHERE d.repo_name = $1",
    )
    .bind(repo)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(
        section_count, 1,
        "second run must replace, not accumulate, sections"
    );
}

#[tokio::test]
async fn corpus_scoped_clean_does_not_touch_website_doc_for_same_repo() {
    let pg = test_pg_pool().await;
    let neo4j = neo4j_pool().await;
    let repo = "corpus-derive-test-isolation";
    clean(&pg, &neo4j, repo).await;

    let (website_doc_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO documents (repo_name, title, doc_type) VALUES ($1, 'Website Page', 'website') RETURNING id",
    )
    .bind(repo)
    .fetch_one(&pg)
    .await
    .unwrap();
    // Matching Neo4j Document node (same shape create_document_node writes)
    // — the isolation proof must hold on BOTH the PG and Neo4j sides of
    // clean_repo_corpus_docs, not just PG.
    neo4j
        .execute(
            neo4rs::query(
                "MERGE (r:Repository {name: $repo}) \
                 MERGE (d:Document {pg_id: $pg_id}) \
                 SET d.repo_name = $repo, d.title = 'Website Page', d.doc_type = 'website' \
                 MERGE (d)-[:BELONGS_TO]->(r)",
            )
            .param("repo", repo)
            .param("pg_id", website_doc_id.to_string().as_str()),
        )
        .await
        .unwrap();

    let store = PgCorpusStore::new(pg.clone());
    let version_id = seed_version(
        &store,
        repo,
        "sha0003",
        vec![md_file("index.md", "# Hi\n\nBody.")],
    )
    .await;

    run_derive(
        deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder)),
        version_id,
    )
    .await
    .expect("derive should succeed");

    let still_there: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM documents WHERE id = $1")
        .bind(website_doc_id)
        .fetch_optional(&pg)
        .await
        .unwrap();
    assert!(
        still_there.is_some(),
        "the corpus-scoped clean must not delete a website-ingested document for the same repo (PG side)"
    );

    let corpus_docs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM documents WHERE repo_name = $1 AND doc_type = 'corpus'",
    )
    .bind(repo)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(corpus_docs, 1);

    let neo4j_rows = neo4j
        .query(
            neo4rs::query("MATCH (d:Document {pg_id: $pg_id}) RETURN d.doc_type AS doc_type")
                .param("pg_id", website_doc_id.to_string().as_str()),
        )
        .await
        .unwrap();
    assert_eq!(
        neo4j_rows.len(),
        1,
        "the corpus-scoped clean must not delete a website-ingested Document node for the same repo (Neo4j side)"
    );
    let doc_type: String = neo4j_rows[0].get("doc_type").unwrap();
    assert_eq!(doc_type, "website");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_derive_runs_for_same_version_do_not_corrupt_the_doc_space() {
    let pg = test_pg_pool().await;
    let neo4j = neo4j_pool().await;
    let repo = "corpus-derive-test-race";
    clean(&pg, &neo4j, repo).await;

    let store = PgCorpusStore::new(pg.clone());
    let version_id = seed_version(
        &store,
        repo,
        "shaRACE",
        vec![
            md_file("index.md", "# A\n\nBody A."),
            md_file("guide/usage.md", "# B\n\nBody B."),
            md_file("guide/setup.md", "# C\n\nBody C."),
        ],
    )
    .await;

    // Two run_derive invocations for the SAME version_id, fired as close to
    // simultaneously as possible on a real multi-thread runtime (not just
    // cooperative single-thread interleaving) — this is what retry-vs-retry
    // or retry-vs-fresh-publish's auto-derive looks like in production.
    let mut deps_a = deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder));
    let job_a = Uuid::new_v4();
    deps_a.job_id = Some(job_a);
    let mut deps_b = deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder));
    let job_b = Uuid::new_v4();
    deps_b.job_id = Some(job_b);

    let handle_a = tokio::spawn(run_derive(deps_a, version_id));
    let handle_b = tokio::spawn(run_derive(deps_b, version_id));
    let (result_a, result_b) = tokio::join!(handle_a, handle_b);
    result_a.unwrap().expect("run A must not error");
    result_b.unwrap().expect("run B must not error");

    // Exactly one of the two runs may have actually claimed and executed the
    // rebuild — its job_id is the only one that can end up persisted (the
    // CAS claim in run_derive never writes derive_job_id for a run that
    // loses the claim, so the loser's job_id can never appear here).
    let (final_job_id,): (Option<Uuid>,) =
        sqlx::query_as("SELECT derive_job_id FROM corpus_versions WHERE id = $1")
            .bind(version_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    let final_job_id = final_job_id.expect("the winning run must have recorded its job_id");
    assert!(
        final_job_id == job_a || final_job_id == job_b,
        "final derive_job_id must belong to exactly one of the two concurrent runs, got {final_job_id}"
    );

    let (status,): (String,) =
        sqlx::query_as("SELECT derive_status FROM corpus_versions WHERE id = $1")
            .bind(version_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(status, "complete");

    // No corruption: exactly 3 documents (one per page, no duplicates, none
    // dropped) and all 3 sections present, regardless of interleaving.
    let docs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM documents WHERE repo_name = $1 AND doc_type = 'corpus'",
    )
    .bind(repo)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(
        docs, 3,
        "concurrent derive runs must not leave duplicate or missing documents"
    );

    let sections: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sections s JOIN documents d ON s.doc_id = d.id WHERE d.repo_name = $1",
    )
    .bind(repo)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(
        sections, 3,
        "concurrent derive runs must not leave duplicate or missing sections"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_derive_runs_for_different_versions_of_same_repo_do_not_corrupt_the_doc_space() {
    let pg = test_pg_pool().await;
    let neo4j = neo4j_pool().await;
    let repo = "corpus-derive-test-cross-version-race";
    clean(&pg, &neo4j, repo).await;

    let store = PgCorpusStore::new(pg.clone());

    // Two DIFFERENT versions of the SAME repo — this is what two publishes
    // racing within the derive window look like: `insert_version` gives each
    // a distinct `version_id`, so `claim_derive_running`'s per-version CAS
    // does NOT serialize them, yet `clean_repo_corpus_docs` inside
    // `run_derive_inner` is repo-scoped. Without a wider (per-repo) guard,
    // both runs' clean+rebuild interleave into duplicate or stale-version
    // documents.
    let version_a = seed_version(
        &store,
        repo,
        "shaCROSSA",
        vec![
            md_file("index.md", "# Version A Index\n\nBody A1."),
            md_file("guide/only-a.md", "# Only In A\n\nBody A2."),
            md_file("guide/shared.md", "# Shared Path A\n\nBody A3."),
        ],
    )
    .await;
    // `insert_version` flips the previous latest off — B is now the sole
    // `is_latest = true` row for this repo.
    let version_b = seed_version(
        &store,
        repo,
        "shaCROSSB",
        vec![
            md_file("index.md", "# Version B Index\n\nBody B1."),
            md_file("guide/only-b.md", "# Only In B\n\nBody B2."),
            md_file("guide/shared.md", "# Shared Path B\n\nBody B3."),
        ],
    )
    .await;
    assert_ne!(version_a, version_b);

    // Fired as close to simultaneously as possible on a real multi-thread
    // runtime (not just cooperative single-thread interleaving) — matching
    // production's "a fresh publish's auto-derive spawn races an older
    // still-running derive for the same repo" shape.
    let mut deps_a = deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder));
    let job_a = Uuid::new_v4();
    deps_a.job_id = Some(job_a);
    let mut deps_b = deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder));
    let job_b = Uuid::new_v4();
    deps_b.job_id = Some(job_b);

    let handle_a = tokio::spawn(run_derive(deps_a, version_a));
    let handle_b = tokio::spawn(run_derive(deps_b, version_b));
    let (result_a, result_b) = tokio::join!(handle_a, handle_b);
    result_a.unwrap().expect("run A must not error");
    result_b.unwrap().expect("run B must not error");

    // The load-bearing assertion: regardless of which run acquired the
    // per-repo lock first, the final Doc space must contain EXACTLY B's
    // (the latest version's) 3 documents — no duplicates from A, no stale
    // docs left behind if A's clean ran after B's rebuild.
    let titles: Vec<String> = sqlx::query_scalar(
        "SELECT title FROM documents WHERE repo_name = $1 AND doc_type = 'corpus' ORDER BY title",
    )
    .bind(repo)
    .fetch_all(&pg)
    .await
    .unwrap();
    let mut expected = vec![
        "Only In B".to_string(),
        "Shared Path B".to_string(),
        "Version B Index".to_string(),
    ];
    expected.sort();
    assert_eq!(
        titles, expected,
        "final Doc space must contain exactly the latest version's (B's) documents — \
         no duplicates and no stale docs from the superseded version A"
    );

    let sections: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sections s JOIN documents d ON s.doc_id = d.id \
         WHERE d.repo_name = $1",
    )
    .bind(repo)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(
        sections, 3,
        "exactly B's 3 sections must remain, no duplicates and no stale A sections"
    );
}

// ── failure path ──────────────────────────────────────────────────────────

#[tokio::test]
async fn derive_failure_sets_failed_status_and_preserves_raw_layer() {
    let pg = test_pg_pool().await;
    let neo4j = neo4j_pool().await;
    let repo = "corpus-derive-test-failure";
    clean(&pg, &neo4j, repo).await;

    let store = PgCorpusStore::new(pg.clone());
    let version_id = seed_version(
        &store,
        repo,
        "sha0004",
        vec![md_file("index.md", "# Boom\n\nBody.")],
    )
    .await;

    let before: (String, String, String, Option<i32>) = sqlx::query_as(
        "SELECT repo_name, version, sha, page_count FROM corpus_versions WHERE id = $1",
    )
    .bind(version_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    let files_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM corpus_files WHERE version_id = $1")
            .bind(version_id)
            .fetch_one(&pg)
            .await
            .unwrap();

    let result = run_derive(
        deps(pg.clone(), neo4j.clone(), Arc::new(FailingEmbedder)),
        version_id,
    )
    .await;
    assert!(
        result.is_err(),
        "a failing embedder must fail the derive job"
    );

    let (status, error): (String, Option<String>) =
        sqlx::query_as("SELECT derive_status, derive_error FROM corpus_versions WHERE id = $1")
            .bind(version_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(status, "failed");
    assert!(error.is_some(), "derive_error must be recorded on failure");

    let after: (String, String, String, Option<i32>) = sqlx::query_as(
        "SELECT repo_name, version, sha, page_count FROM corpus_versions WHERE id = $1",
    )
    .bind(version_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(
        before, after,
        "the raw corpus_versions row must be untouched by a derive failure"
    );

    let files_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM corpus_files WHERE version_id = $1")
            .bind(version_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(
        files_before, files_after,
        "corpus_files must be untouched by a derive failure"
    );
}

// ── EXPLAINS anchoring (B5: code_sha staleness signal) ───────────────────

#[tokio::test]
async fn derive_annotates_explains_edges_with_code_sha_when_code_previously_ingested() {
    let pg = test_pg_pool().await;
    let neo4j = neo4j_pool().await;
    let repo = "corpus-derive-test-explains";
    clean(&pg, &neo4j, repo).await;

    // Seed a chunk (PG + matching Neo4j node) that a corpus page will
    // reference by exact backtick name — `find_chunk_ids_by_name` requires
    // an exact `chunks.name` match, min length 5.
    let chunk_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO chunks (id, repo_name, module_path, chunk_type, name, content, embedding) \
         VALUES ($1, $2, 'src/auth.rs', 'function', 'handle_login', 'fn handle_login() {}', $3)",
    )
    .bind(chunk_id)
    .bind(repo)
    .bind(pgvector::Vector::from(vec![0.01f32; DIM]))
    .execute(&pg)
    .await
    .unwrap();
    neo4j
        .execute(
            neo4rs::query("MERGE (c:Chunk {pg_id: $id}) SET c.repo_name = $repo")
                .param("id", chunk_id.to_string().as_str())
                .param("repo", repo),
        )
        .await
        .unwrap();

    let job_repo = PgIngestionJobRepo::new(pg.clone());
    job_repo.create_job(repo, "codesha-current").await.unwrap();

    let store = PgCorpusStore::new(pg.clone());
    let version_id = seed_version(
        &store,
        repo,
        "corpusshaZZZ",
        vec![md_file(
            "index.md",
            "# Login\n\nSee `handle_login` for details.",
        )],
    )
    .await;

    run_derive(
        deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder)),
        version_id,
    )
    .await
    .expect("derive should succeed");

    let rows = neo4j
        .query(
            neo4rs::query("MATCH (:Section)-[r:EXPLAINS]->(:Chunk {pg_id: $cid}) RETURN r.code_sha AS code_sha")
                .param("cid", chunk_id.to_string().as_str()),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one EXPLAINS edge to the seeded chunk"
    );
    let code_sha: String = rows[0].get("code_sha").unwrap();
    assert_eq!(
        code_sha, "codesha-current",
        "EXPLAINS edge must carry the repo's current code snapshot git_ref as code_sha"
    );
}

#[tokio::test]
async fn derive_skips_explains_when_repo_never_code_ingested() {
    let pg = test_pg_pool().await;
    let neo4j = neo4j_pool().await;
    let repo = "corpus-derive-test-noexplains";
    clean(&pg, &neo4j, repo).await;

    let store = PgCorpusStore::new(pg.clone());
    let version_id = seed_version(
        &store,
        repo,
        "shaNOCODE",
        vec![md_file(
            "index.md",
            "# Login\n\nSee `zzz_no_such_symbol_xyz` for details.",
        )],
    )
    .await;

    run_derive(
        deps(pg.clone(), neo4j.clone(), Arc::new(FakeEmbedder)),
        version_id,
    )
    .await
    .expect("derive should succeed even with no code snapshot ingested");

    let (doc_id,): (Uuid,) = sqlx::query_as("SELECT id FROM documents WHERE repo_name = $1")
        .bind(repo)
        .fetch_one(&pg)
        .await
        .unwrap();
    let (section_id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM sections WHERE doc_id = $1 LIMIT 1")
            .bind(doc_id)
            .fetch_one(&pg)
            .await
            .unwrap();

    let rows = neo4j
        .query(
            neo4rs::query("MATCH (s:Section {pg_id: $sid})-[r:EXPLAINS]->() RETURN r")
                .param("sid", section_id.to_string().as_str()),
        )
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "no code ever ingested for this repo => no EXPLAINS edges at all"
    );
}
