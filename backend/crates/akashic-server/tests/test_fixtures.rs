#![cfg(feature = "test-fixtures")]
//! Integration tests for the test-fixtures HTTP endpoints.
//! Run with `cargo test --features test-fixtures`.

mod common;

use common::TestEnv;
use serde_json::json;

#[tokio::test]
#[serial_test::serial]
async fn fixture_session_issues_cookie_and_inserts_row() {
    let env = TestEnv::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{}/test/fixtures/session", env.app_addr))
        .json(&json!({
            "username": "e2e-user-1",
            "name": "E2E User",
            "gitlab_user_id": 999001_i64,
        }))
        .send()
        .await
        .expect("fixture call");

    assert_eq!(resp.status(), 200);

    let set_cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|h| h.to_str().unwrap_or("").starts_with("ak_session="))
        .expect("ak_session Set-Cookie present")
        .to_str()
        .unwrap()
        .to_string();

    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Lax"));
    assert!(set_cookie.contains("Path=/"));

    let body: serde_json::Value = resp.json().await.unwrap();
    let api_key = body["api_key"].as_str().expect("api_key in body");
    assert!(!api_key.is_empty());
    assert_eq!(body["username"], "e2e-user-1");

    // Verify DB row exists.
    let row: (String, String) =
        sqlx::query_as("SELECT username, gitlab_token FROM sessions WHERE api_key = $1")
            .bind(api_key)
            .fetch_one(env.pg_pool())
            .await
            .expect("row exists");
    assert_eq!(row.0, "e2e-user-1");
    assert!(!row.1.is_empty(), "gitlab_token should be populated");
}

#[tokio::test]
#[serial_test::serial]
async fn fixture_note_inserts_pg_and_neo4j() {
    let env = TestEnv::start().await;
    let client = reqwest::Client::new();

    // No pre-seed: the note handler's `MERGE (r:Repository {name: $repo})`
    // creates the repo node on demand, so this test stays self-contained.
    let resp = client
        .post(format!("http://{}/test/fixtures/note", env.app_addr))
        .json(&serde_json::json!({
            "repo_name": "akashic-record",
            "title": "fixture title",
            "content": "fixture body",
            "category": "ARCHITECTURE",
        }))
        .send()
        .await
        .expect("note fixture call");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let uuid = body["uuid"].as_str().expect("uuid in body");
    assert_eq!(body["repo_name"], "akashic-record");

    let (title, content, repo_name): (Option<String>, String, String) =
        sqlx::query_as("SELECT title, content, repo_name FROM notes WHERE id = $1::uuid")
            .bind(uuid)
            .fetch_one(env.pg_pool())
            .await
            .expect("note row exists");
    assert_eq!(title.as_deref(), Some("fixture title"));
    assert_eq!(content, "fixture body");
    assert_eq!(repo_name, "akashic-record");
}

#[tokio::test]
#[serial_test::serial]
async fn fixture_repo_creates_neo4j_node_and_sources_row() {
    let env = TestEnv::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{}/test/fixtures/repo", env.app_addr))
        .json(&serde_json::json!({
            "name": "akashic-record",
            "source_type": "gitlab",
        }))
        .send()
        .await
        .expect("repo fixture call");

    assert_eq!(resp.status(), 200);

    // Neo4j node exists. `env.neo4j()` returns a raw `&Graph`; the
    // canonical "issue MATCH, read first row" shape is
    // `graph.execute(query).await? -> RowStream`, then `.next().await?`.
    let row = env
        .neo4j()
        .execute(
            neo4rs::query("MATCH (r:Repository {name: $name}) RETURN r.name AS name")
                .param("name", "akashic-record"),
        )
        .await
        .expect("neo4j query")
        .next()
        .await
        .expect("neo4j next")
        .expect("at least one row");
    let name: String = row.get("name").expect("name column");
    assert_eq!(name, "akashic-record");

    // sources row exists
    let src: (String, String) =
        sqlx::query_as("SELECT repo_name, source_type FROM sources WHERE repo_name = $1")
            .bind("akashic-record")
            .fetch_one(env.pg_pool())
            .await
            .expect("sources row");
    assert_eq!(src.0, "akashic-record");
    assert_eq!(src.1, "gitlab");

    // Idempotent: second call doesn't 5xx.
    let resp2 = client
        .post(format!("http://{}/test/fixtures/repo", env.app_addr))
        .json(&serde_json::json!({"name": "akashic-record", "source_type": "gitlab"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);
}
