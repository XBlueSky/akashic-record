//! Test-fixture HTTP endpoints. Compiled ONLY when the `test-fixtures`
//! cargo feature is enabled (the `mod` declaration in `routes/mod.rs`
//! is itself feature-gated). The production release binary NEVER
//! contains these symbols — verified by AC-2 (release build symbol-grep).

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};

use crate::auth::build_session_cookie;
use crate::auth::types::UserInfo;
use akashic_context::AppState;
use akashic_domain::ports::corpus::NewCorpusFile;
use akashic_domain::types::corpus::{CorpusManifest, CorpusSource};

use super::super::error::AppError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/session", post(create_session))
        .route("/note", post(create_note))
        .route("/repo", post(create_repo))
        .route("/docs-corpus", post(seed_docs_corpus))
}

#[derive(Deserialize, Default)]
struct SessionBody {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    gitlab_user_id: Option<i64>,
}

#[derive(Serialize)]
struct SessionResp {
    api_key: String,
    username: String,
}

async fn create_session(
    State(state): State<AppState>,
    Json(body): Json<SessionBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let api_key = Alphanumeric.sample_string(&mut rand::thread_rng(), 48);
    let gitlab_token = format!(
        "e2e-token-{}",
        Alphanumeric.sample_string(&mut rand::thread_rng(), 16)
    );
    let username = body.username.unwrap_or_else(|| "e2e-user".to_string());
    let name = body.name.unwrap_or_else(|| username.clone());

    let user_info = UserInfo {
        username: username.clone(),
        name: Some(name),
        avatar_url: None,
    };

    state
        .auth_store
        .insert_session(
            &api_key,
            &user_info,
            &gitlab_token,
            60 * 60 * 24,
            body.gitlab_user_id,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("insert_session: {e}"),
            )
        })?;

    let cookie = build_session_cookie(
        state.config.cookie_secure,
        &state.config.frontend_url,
        &api_key,
        60 * 60 * 24,
    );

    Ok((
        StatusCode::OK,
        [("set-cookie", cookie)],
        Json(SessionResp { api_key, username }),
    ))
}

#[derive(Deserialize)]
struct NoteBody {
    repo_name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Serialize)]
struct NoteResp {
    uuid: String,
    repo_name: String,
}

async fn create_note(
    State(state): State<AppState>,
    Json(body): Json<NoteBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let title = body.title.unwrap_or_else(|| "fixture note".to_string());
    let content = body.content.unwrap_or_else(|| "fixture body".to_string());
    let category = body.category.unwrap_or_else(|| "ARCHITECTURE".to_string());

    // Zero-vector 1536-dim embedding — deterministic, doesn't affect
    // Path A / Path B which don't exercise cosine ranking.
    let zero_embedding: Vec<f32> = vec![0.0_f32; 1536];
    let embedding = pgvector::Vector::from(zero_embedding);

    // AppState no longer carries DB pools (A2a); this feature-gated e2e seeding
    // endpoint builds its own from config.
    let pg = akashic_store_pg::connect(&state.config.database_url)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("pg connect: {e}"),
            )
        })?;

    let uuid: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO notes (repo_name, category, title, content, embedding) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&body.repo_name)
    .bind(&category)
    .bind(&title)
    .bind(&content)
    .bind(embedding)
    .fetch_one(&pg)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("notes insert: {e}"),
        )
    })?;

    // Best-effort Neo4j node — Path A renders graph, but its absence
    // doesn't break the test (the node may be created by ingestion in
    // production; here we wire it explicitly for completeness).
    //
    // Both `uuid` and `pg_id` are set to the generated UUID — production
    // graph queries `coalesce(n.uuid, n.pg_id)` so retain both properties.
    // `repo_name` is also denormalized for parity with production ingestion.
    match akashic_store_neo4j::Neo4jPool::connect(&state.config).await {
        Ok(db) => {
            if let Err(e) = db
                .query(
                    neo4rs::query(
                        "MERGE (r:Repository {name: $repo}) \
                         CREATE (n:Note {uuid: $uuid, pg_id: $uuid, repo_name: $repo, title: $title}) \
                         MERGE (n)-[:BELONGS_TO]->(r)",
                    )
                    .param("repo", body.repo_name.clone())
                    .param("uuid", uuid.to_string())
                    .param("title", title.clone()),
                )
                .await
            {
                tracing::warn!(error = %e, "test-fixtures: Neo4j note seed failed (non-fatal)");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "test-fixtures: Neo4j connect failed (non-fatal)");
        }
    }

    Ok((
        StatusCode::OK,
        Json(NoteResp {
            uuid: uuid.to_string(),
            repo_name: body.repo_name,
        }),
    ))
}

#[derive(Deserialize)]
struct RepoBody {
    name: String,
    #[serde(default)]
    source_type: Option<String>,
}

async fn create_repo(
    State(state): State<AppState>,
    Json(body): Json<RepoBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // AppState no longer carries DB pools (A2a); build them from config.
    let db = akashic_store_neo4j::Neo4jPool::connect(&state.config)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("neo4j connect: {e}"),
            )
        })?;

    // MERGE — idempotent on second call with the same name.
    db.query(neo4rs::query("MERGE (r:Repository {name: $name})").param("name", body.name.clone()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("neo4j: {e}")))?;

    let pg = akashic_store_pg::connect(&state.config.database_url)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("pg connect: {e}"),
            )
        })?;

    // `sources.repo_name` carries a UNIQUE constraint (see db::pg
    // init_schema); ON CONFLICT DO NOTHING makes the PG side idempotent
    // as well.
    let source_type = body.source_type.unwrap_or_else(|| "gitlab".to_string());
    sqlx::query(
        "INSERT INTO sources (repo_name, source_type) VALUES ($1, $2) \
         ON CONFLICT (repo_name) DO NOTHING",
    )
    .bind(&body.name)
    .bind(&source_type)
    .execute(&pg)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("sources insert: {e}"),
        )
    })?;

    Ok((StatusCode::OK, Json(serde_json::json!({"name": body.name}))))
}

#[derive(Deserialize)]
pub(crate) struct DocsCorpusFixtureReq {
    repo: Option<String>,
}

/// POST /test/fixtures/docs-corpus — ingest a tiny deterministic corpus
/// through the SAME `CorpusIngestService` path real publishes use.
/// Idempotent via the (repo, sha) natural key (`InsertOutcome::Existing`).
pub(crate) async fn seed_docs_corpus(
    State(state): State<AppState>,
    Json(req): Json<DocsCorpusFixtureReq>,
) -> Result<impl IntoResponse, AppError> {
    let repo = req.repo.unwrap_or_else(|| "e2e-docs".to_string());
    let sha = "0123456789abcdef0123456789abcdef012345e2";
    let manifest = CorpusManifest {
        repo: repo.clone(),
        version: "v0.0.1-e2e".to_string(),
        sha: sha.to_string(),
        index: "index.md".to_string(),
        languages: vec!["en".to_string(), "zh-TW".to_string()],
        tool_version: "e2e-fixture".to_string(),
    };
    state
        .corpus_ingest_service
        .ingest_tree(manifest, e2e_corpus_files(), CorpusSource::Push)
        .await
        .map_err(|rej| AppError::BadRequest(format!("fixture corpus rejected: {rej:?}")))?;
    Ok(Json(serde_json::json!({ "repo": repo, "sha": sha })))
}

fn e2e_corpus_files() -> Vec<NewCorpusFile> {
    fn md(path: &str, content: &str) -> NewCorpusFile {
        NewCorpusFile {
            path: path.to_string(),
            content: content.as_bytes().to_vec(),
            is_markdown: true,
        }
    }
    vec![
        md("index.md", INDEX_MD),
        md("guide/setup.md", SETUP_MD),
        md("guide/advanced.md", ADVANCED_MD),
        md("about.md", ABOUT_MD),
        md("zh-TW/index.md", ZH_INDEX_MD),
        NewCorpusFile {
            path: "guide/images/dot.png".to_string(),
            content: DOT_PNG.to_vec(),
            is_markdown: false,
        },
    ]
}

const INDEX_MD: &str = r#"# E2E Docs Corpus

A tiny deterministic corpus for e2e journeys.

**Language:** English | [繁體中文](zh-TW/index.md)

## All pages

### Guide

- [Setup](guide/setup.md)
- [Advanced](guide/advanced.md)

### Meta

- [About](about.md)
- [中文首頁](zh-TW/index.md)
"#;

const SETUP_MD: &str = r#"# Setup

**Language:** English | [繁體中文](../zh-TW/index.md)

> [!NOTE]
> This corpus exists only for e2e tests.

## Install

```cpp
int main() { return 0; }
```

![dot](images/dot.png)

## Quick Start

See [Advanced](advanced.md#tuning) or go [home](../index.md).
"#;

const ADVANCED_MD: &str = r#"# Advanced

## Tuning

Details with a footnote.[^1]

```mermaid
graph TD;
  A-->B;
```

[^1]: tuning footnote body.
"#;

const ABOUT_MD: &str = r#"# About

Plain page. Back to [Setup](guide/setup.md#install).
"#;

const ZH_INDEX_MD: &str = r#"# E2E 文件

**Language:** [English](../index.md) | 繁體中文

中文內容頁,含全形標點:「快速」開始——測試。
"#;

// 1×1 PNG: iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==
const DOT_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x50,
    0x0f, 0x00, 0x04, 0x85, 0x01, 0x88, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
