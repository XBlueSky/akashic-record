//! D2 §5.3 (reframed 2026-05-11) — note GET after direct DB seed.
//!
//! Seed a note row directly into PG (bypassing the MCP-only write path)
//! → GET via REST as the logged-in actor → assert 200 + response body
//! matches the seed.
//!
//! This validates the REST GET note path end-to-end: session cookie →
//! `require_auth` middleware → handler → PG read → JSON serialization.
//! The production write path lives behind MCP (`save_note` / `supersede_note`)
//! and is exercised by D4's golden-path test; this test covers the read
//! surface in isolation.
//!
//! Note: `get_note` in `backend/src/api/routes/notes.rs` reads ONLY from
//! PostgreSQL (no Neo4j MATCH on the request path), so this test seeds
//! only PG. The Neo4j `:Note` node the production save path creates is
//! incidental to the GET response.

mod common;

use pgvector::Vector;

#[tokio::test]
#[serial_test::serial]
async fn test_get_note_after_db_seed() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();

    let note_id = uuid::Uuid::new_v4();
    let repo = "test-repo";

    // Seed PG `notes` row. NOT NULL columns without defaults are
    // `repo_name`, `category`, `content`, and `embedding`; everything
    // else is nullable or defaulted (id, created_at, updated_at).
    //
    // Embedding dimensions must match the bench's TestEmbedder (1536) —
    // that's what `init_schema` provisions when the bench boots with
    // `EmbeddingPrecision::Float32`.
    let zero_embedding = Vector::from(vec![0.0_f32; 1536]);
    sqlx::query(
        "INSERT INTO notes (id, repo_name, category, title, summary, content, embedding) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(note_id)
    .bind(repo)
    .bind("ARCHITECTURE")
    .bind("Seeded title")
    .bind("Seeded summary")
    .bind("Seeded content")
    .bind(zero_embedding)
    .execute(env.pg_pool())
    .await
    .expect("seed PG notes row");

    let resp = client
        .get(format!(
            "http://{}/api/v1/repos/{}/notes/{}",
            env.app_addr, repo, note_id
        ))
        .header("Cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET note by id");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "expected 200 for a seeded note"
    );

    let body: serde_json::Value = resp.json().await.expect("note JSON body");

    // Response shape is `NoteDetail` in api/routes/notes.rs:
    // { uuid, title, summary, content, category, branch, repo, author,
    //   created_at, tags, facts, related_symbols, related_files }
    assert_eq!(
        body.get("uuid").and_then(|v| v.as_str()),
        Some(note_id.to_string().as_str()),
        "uuid should round-trip"
    );
    assert_eq!(
        body.get("title").and_then(|v| v.as_str()),
        Some("Seeded title"),
    );
    assert_eq!(
        body.get("summary").and_then(|v| v.as_str()),
        Some("Seeded summary"),
    );
    assert_eq!(
        body.get("content").and_then(|v| v.as_str()),
        Some("Seeded content"),
    );
    assert_eq!(
        body.get("category").and_then(|v| v.as_str()),
        Some("ARCHITECTURE"),
    );
    assert_eq!(
        body.get("repo").and_then(|v| v.as_str()),
        Some(repo),
        "repo field should echo the path segment",
    );
}

#[tokio::test]
#[serial_test::serial]
async fn test_get_note_missing_returns_404() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();

    let unseeded_id = uuid::Uuid::new_v4();
    let resp = client
        .get(format!(
            "http://{}/api/v1/repos/test-repo/notes/{}",
            env.app_addr, unseeded_id
        ))
        .header("Cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET note by id");

    assert_eq!(
        resp.status().as_u16(),
        404,
        "expected 404 for an id with no row",
    );
}
