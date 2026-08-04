//! D2 §5.4 — search via deterministic embedder.
//!
//! Two notes seeded with distinct `TestEmbedder` vectors → `GET
//! /api/v1/search?q=...&layer=note` → assert pgvector cosine ranks the
//! matching note first.
//!
//! Why this proves the wiring works: `TestEmbedder::embed(text)` is
//! blake3(text) → first 32 dims unique, remaining 1504 dims zero. Two
//! distinct seed contents therefore live at distinct points in vector
//! space, and a query whose text exactly matches one seed's content
//! produces an *identical* embedding to that seed → cosine = 1.0 — a
//! perfect hit, ranked above the other.
//!
//! Route notes:
//! - `GET /api/v1/search?q=&repo=&layer=&limit=` (public, no auth)
//! - The handler does `state.embedder.embed(&q)` then `ORDER BY
//!   embedding <=> $1`. If `TestEmbedder` weren't injected into
//!   `state.embedder` by the bench, the embed call would either hit the
//!   network (it won't — base_url is `http://127.0.0.1:1`) or produce a
//!   non-deterministic vector. The assertion below is the proof.
//! - `layer=note` restricts the SQL to the `notes` table, sparing us
//!   from seeding `chunks` and `sections`.
//! - Response is a top-level JSON array of `SearchResultItem`, not a
//!   `{results: [...]}` envelope.

mod common;

use akashic_embed::EmbeddingProvider;
use pgvector::Vector;

#[tokio::test]
#[serial_test::serial]
async fn test_search_via_deterministic_embedder() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();

    let repo = "test-repo";
    let a_id = uuid::Uuid::new_v4();
    let b_id = uuid::Uuid::new_v4();
    let a_content = "alpha alpha alpha";
    let b_content = "beta beta beta";

    // Seed two notes with distinct TestEmbedder-derived embeddings.
    // `notes` NOT NULL columns without defaults: repo_name, category,
    // content, embedding (per Task 6 findings). Embedding dim must
    // match the migrated schema (1536) and the bench's TestEmbedder.
    for (id, title, content) in [(a_id, "A", a_content), (b_id, "B", b_content)] {
        let emb = common::TestEmbedder
            .embed(content)
            .await
            .expect("TestEmbedder.embed(seed content)");
        sqlx::query(
            "INSERT INTO notes (id, repo_name, category, title, content, embedding) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(repo)
        .bind("ARCHITECTURE")
        .bind(title)
        .bind(content)
        .bind(Vector::from(emb.vector))
        .execute(env.pg_pool())
        .await
        .expect("seed PG notes row");
    }

    // ── Query 1: text identical to note A's content ──────────────────
    // TestEmbedder is deterministic → server-side embed(q) == seed
    // embedding for A → cosine distance 0 → A ranks first.
    let resp = client
        .get(format!("http://{}/api/v1/search", env.app_addr))
        .query(&[("q", a_content), ("layer", "note"), ("limit", "5")])
        .send()
        .await
        .expect("GET /api/v1/search (alpha)");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "search must return 200 (alpha)",
    );
    let results: serde_json::Value = resp.json().await.expect("search response 1 JSON");
    let results = results
        .as_array()
        .expect("response is a top-level array of SearchResultItem");
    assert!(
        !results.is_empty(),
        "search must return at least one hit for the seeded note (alpha)",
    );
    let first_id = results[0]
        .get("id")
        .and_then(|v| v.as_str())
        .expect("hit must have an `id` field");
    assert_eq!(
        first_id,
        a_id.to_string(),
        "expected note A ranked first; full response: {results:#?}",
    );

    // ── Query 2: text identical to note B's content ──────────────────
    // Same proof on the other side — confirms the ranking is driven by
    // the embedding match, not by insertion order or repo_name etc.
    let resp2 = client
        .get(format!("http://{}/api/v1/search", env.app_addr))
        .query(&[("q", b_content), ("layer", "note"), ("limit", "5")])
        .send()
        .await
        .expect("GET /api/v1/search (beta)");

    assert_eq!(
        resp2.status().as_u16(),
        200,
        "search must return 200 (beta)",
    );
    let results2: serde_json::Value = resp2.json().await.expect("search response 2 JSON");
    let results2 = results2
        .as_array()
        .expect("response 2 is a top-level array of SearchResultItem");
    assert!(
        !results2.is_empty(),
        "search must return at least one hit for the seeded note (beta)",
    );
    let first_id2 = results2[0]
        .get("id")
        .and_then(|v| v.as_str())
        .expect("hit 2 must have an `id` field");
    assert_eq!(
        first_id2,
        b_id.to_string(),
        "expected note B ranked first; full response: {results2:#?}",
    );
}
