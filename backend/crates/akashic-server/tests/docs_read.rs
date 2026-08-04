//! Integration tests for Task 10 (C3, docs-corpus): `GET /api/v1/docs/*` —
//! the public, unauthenticated read API over the stored corpus.
//!
//! Requires the live Postgres bench (`common::TestEnv::start()`); export
//! `DATABASE_URL`/`TEST_DATABASE_URL` at :5432 (the akashic_test_support
//! `test_pg_pool()` :5433 default is a dead port in this environment).

mod common;

use akashic_domain::ports::corpus::{InsertOutcome, NewCorpusFile, NewCorpusVersion};
use akashic_domain::types::corpus::{
    CorpusManifest, CorpusSource, CorpusVersionMeta, NavGroup, NavPage, NavTree,
};
use serde_json::Value;

// ── fixture builder ──────────────────────────────────────────────────────────

fn sample_nav() -> NavTree {
    NavTree {
        description: "Sample corpus for Task 10 read-endpoint tests.".to_string(),
        groups: vec![NavGroup {
            title: "Guide".to_string(),
            pages: vec![NavPage {
                title: "Setup Page".to_string(),
                path: "guide/setup.md".to_string(),
                description: "Install and configure.".to_string(),
            }],
        }],
    }
}

const INDEX_MD: &str = "# Index\n\nWelcome.\n";
const SETUP_MD: &str = "# Setup\n\nInstall steps here.\n";

async fn seed_corpus(
    env: &common::TestEnv,
    repo: &str,
    version: &str,
    sha: &str,
    is_tagged: bool,
) -> CorpusVersionMeta {
    let manifest = CorpusManifest {
        repo: repo.to_string(),
        version: version.to_string(),
        sha: sha.to_string(),
        index: "index.md".to_string(),
        languages: vec!["en".to_string()],
        tool_version: "1.0.0".to_string(),
    };
    let files = vec![
        NewCorpusFile {
            path: "index.md".to_string(),
            content: INDEX_MD.as_bytes().to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "guide/setup.md".to_string(),
            content: SETUP_MD.as_bytes().to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "assets/logo.png".to_string(),
            content: vec![0x89, 0x50, 0x4E, 0x47, 0xFF, 0xFE],
            is_markdown: false,
        },
    ];

    let outcome = env
        .state
        .corpus_store
        .insert_version(NewCorpusVersion {
            manifest,
            nav: sample_nav(),
            source: CorpusSource::Push,
            is_tagged,
            files,
        })
        .await
        .expect("insert_version");

    match outcome {
        InsertOutcome::Inserted(meta) | InsertOutcome::Existing(meta) => meta,
    }
}

/// Simulate a completed derive: manually insert the `doc_type = "corpus"`
/// document row the Task 8/9 derive job would have created for one page,
/// keyed exactly the way `corpus_derive::run_derive_inner` keys it
/// (`repo_name`, `doc_type = "corpus"`, `source_url = <path>`).
async fn seed_derived_document(
    pg: &sqlx::PgPool,
    repo: &str,
    path: &str,
    title: &str,
) -> uuid::Uuid {
    let (id,): (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO documents (repo_name, source_url, title, doc_type) \
         VALUES ($1, $2, $3, 'corpus') RETURNING id",
    )
    .bind(repo)
    .bind(path)
    .bind(title)
    .fetch_one(pg)
    .await
    .expect("insert corpus document row");
    id
}

fn docs_url(env: &common::TestEnv, suffix: &str) -> String {
    format!("http://{}/api/v1/docs{suffix}", env.app_addr)
}

// ── tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial_test::serial]
async fn list_docs_repos_returns_repo_with_description() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-list-repos";
    let meta = seed_corpus(&env, repo, "1.0.0", "listreposha1", false).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(&env, ""))
        .send()
        .await
        .expect("GET /api/v1/docs");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json body");
    let repos = body["repos"].as_array().expect("repos array");
    let entry = repos
        .iter()
        .find(|r| r["repo"] == repo)
        .unwrap_or_else(|| panic!("repo {repo} not found in {body}"));
    assert_eq!(entry["version"], "1.0.0");
    assert_eq!(entry["sha"], meta.sha);
    assert_eq!(entry["page_count"], 2);
    assert_eq!(entry["derive_status"], "pending");
    assert_eq!(
        entry["description"],
        "Sample corpus for Task 10 read-endpoint tests."
    );
}

#[tokio::test]
#[serial_test::serial]
async fn list_docs_versions_returns_version_rows() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-list-versions";
    seed_corpus(&env, repo, "1.0.0", "listversha1", true).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(&env, &format!("/{repo}")))
        .send()
        .await
        .expect("GET /api/v1/docs/:repo");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json body");
    let versions = body.as_array().expect("versions array");
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["version"], "1.0.0");
    assert_eq!(versions[0]["sha"], "listversha1");
    assert_eq!(versions[0]["is_tagged"], true);
    assert_eq!(versions[0]["derive_status"], "pending");
}

#[tokio::test]
#[serial_test::serial]
async fn nav_resolves_by_latest_version_and_sha_prefix() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-nav-resolve";
    let meta = seed_corpus(&env, repo, "2.0.0", "navresolvesha1", false).await;
    let client = reqwest::Client::new();

    for selector in ["latest", "2.0.0", &meta.sha[..7]] {
        let resp = client
            .get(docs_url(&env, &format!("/{repo}/{selector}/nav")))
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET nav via selector {selector}: {e}"));
        assert_eq!(
            resp.status(),
            200,
            "selector {selector} must resolve to 200"
        );
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body["description"], "Sample corpus for Task 10 read-endpoint tests.",
            "selector {selector}"
        );
        assert_eq!(body["groups"][0]["title"], "Guide", "selector {selector}");
    }
}

#[tokio::test]
#[serial_test::serial]
async fn nav_unknown_version_returns_404() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-nav-404";
    seed_corpus(&env, repo, "1.0.0", "nav404sha1", false).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(&env, &format!("/{repo}/9.9.9/nav")))
        .send()
        .await
        .expect("GET nav unknown version");
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
#[serial_test::serial]
async fn page_returns_markdown_title_stamp_and_no_document_id_before_derive() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-page";
    let meta = seed_corpus(&env, repo, "1.0.0", "pagesha1234567", false).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(
            &env,
            &format!("/{repo}/latest/page/guide/setup.md"),
        ))
        .send()
        .await
        .expect("GET page");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(
        body["markdown"], SETUP_MD,
        "markdown must be byte-identical"
    );
    assert_eq!(
        body["title"], "Setup Page",
        "title must come from the nav entry, not the path stem"
    );
    let sha7 = &meta.sha[..7];
    assert_eq!(body["stamp"], format!("documents {repo} 1.0.0 @ {sha7}"));
    assert!(
        body.get("document_id").is_none() || body["document_id"].is_null(),
        "document_id must be absent before derive has run, got {body}"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn page_attaches_document_id_once_derive_has_created_the_document() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-page-derived";
    seed_corpus(&env, repo, "1.0.0", "pagederivedsha1", false).await;
    let doc_id = seed_derived_document(env.pg_pool(), repo, "guide/setup.md", "Setup Page").await;

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(
            &env,
            &format!("/{repo}/latest/page/guide/setup.md"),
        ))
        .send()
        .await
        .expect("GET page");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["document_id"], doc_id.to_string());
}

#[tokio::test]
#[serial_test::serial]
async fn page_falls_back_to_path_stem_title_when_not_in_nav() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-page-no-nav-title";
    seed_corpus(&env, repo, "1.0.0", "pagenonavsha1", false).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(&env, &format!("/{repo}/latest/page/index.md")))
        .send()
        .await
        .expect("GET page index.md (not in sample nav)");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["title"], "index");
}

#[tokio::test]
#[serial_test::serial]
async fn page_missing_path_returns_404() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-page-404";
    seed_corpus(&env, repo, "1.0.0", "page404sha1", false).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(
            &env,
            &format!("/{repo}/latest/page/does/not/exist.md"),
        ))
        .send()
        .await
        .expect("GET page missing path");
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
#[serial_test::serial]
async fn page_dotdot_path_returns_400() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-page-dotdot";
    seed_corpus(&env, repo, "1.0.0", "pagedotdotsha1", false).await;

    // A literal "../" mid-URL gets dot-segment-normalized away by the HTTP
    // client itself before the request is even sent (RFC 3986 §5.2.4), so it
    // can't reach the handler's guard at all — that's not a meaningful test
    // of `validate_corpus_path`. `%2F` keeps the segment opaque to that
    // normalization (no literal '/' for it to act on) while still decoding
    // to a `..`-bearing path once axum's wildcard extractor unescapes it —
    // this is the shape a client that DOES reach the handler exercises.
    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(
            &env,
            &format!("/{repo}/latest/page/guide%2F..%2Fsecret.md"),
        ))
        .send()
        .await
        .expect("GET page with encoded ../ path");
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
#[serial_test::serial]
async fn raw_returns_bytes_content_type_and_stamp_header() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-raw";
    let meta = seed_corpus(&env, repo, "1.0.0", "rawsha1234567", false).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(
            &env,
            &format!("/{repo}/latest/raw/guide/setup.md"),
        ))
        .send()
        .await
        .expect("GET raw markdown");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/markdown; charset=utf-8"
    );
    let sha7 = &meta.sha[..7];
    assert_eq!(
        resp.headers().get("x-akashic-docs-stamp").unwrap(),
        &format!("documents {repo} 1.0.0 @ {sha7}")
    );
    let bytes = resp.bytes().await.expect("body bytes");
    assert_eq!(bytes.as_ref(), SETUP_MD.as_bytes());

    let resp_png = client
        .get(docs_url(
            &env,
            &format!("/{repo}/latest/raw/assets/logo.png"),
        ))
        .send()
        .await
        .expect("GET raw png");
    assert_eq!(resp_png.status(), 200);
    assert_eq!(resp_png.headers().get("content-type").unwrap(), "image/png");
}

#[tokio::test]
#[serial_test::serial]
async fn raw_and_page_etag_supports_if_none_match_304() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-etag";
    let meta = seed_corpus(&env, repo, "1.0.0", "etagsha1234567", false).await;
    let etag = format!("\"{}\"", meta.sha);

    let client = reqwest::Client::new();
    let resp = client
        .get(docs_url(
            &env,
            &format!("/{repo}/latest/raw/guide/setup.md"),
        ))
        .header("If-None-Match", &etag)
        .send()
        .await
        .expect("GET raw with If-None-Match");
    assert_eq!(resp.status(), 304);

    let resp_page = client
        .get(docs_url(
            &env,
            &format!("/{repo}/latest/page/guide/setup.md"),
        ))
        .header("If-None-Match", &etag)
        .send()
        .await
        .expect("GET page with If-None-Match");
    assert_eq!(resp_page.status(), 304);

    let resp_nav = client
        .get(docs_url(&env, &format!("/{repo}/latest/nav")))
        .header("If-None-Match", &etag)
        .send()
        .await
        .expect("GET nav with If-None-Match");
    assert_eq!(resp_nav.status(), 304);
}

#[tokio::test]
#[serial_test::serial]
async fn cache_control_differs_for_latest_vs_pinned_selector() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-cache-control";
    seed_corpus(&env, repo, "1.0.0", "cachectrlsha1", false).await;

    let client = reqwest::Client::new();
    let resp_latest = client
        .get(docs_url(&env, &format!("/{repo}/latest/nav")))
        .send()
        .await
        .expect("GET nav via latest");
    assert_eq!(
        resp_latest.headers().get("cache-control").unwrap(),
        "no-cache"
    );

    let resp_pinned = client
        .get(docs_url(&env, &format!("/{repo}/1.0.0/nav")))
        .send()
        .await
        .expect("GET nav via exact version");
    assert_eq!(
        resp_pinned.headers().get("cache-control").unwrap(),
        "public, max-age=31536000, immutable"
    );
}

/// Task 11: parse a Prometheus counter's current value for a specific label
/// set out of `PrometheusHandle::render()`'s text-exposition output. Returns
/// 0.0 when the metric/label combination hasn't been emitted yet. Callers
/// compare deltas, not absolute values — the recorder is a process-global
/// static shared across every test in this binary, so other tests hitting
/// the same endpoint accumulate into the same counter.
fn counter_value(rendered: &str, name: &str, label_fragment: &str) -> f64 {
    let prefix = format!("{name}{{");
    rendered
        .lines()
        .find(|l| l.starts_with(&prefix) && l.contains(label_fragment))
        .and_then(|l| l.split_whitespace().last())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

#[tokio::test]
#[serial_test::serial]
async fn list_and_nav_increment_their_own_endpoint_counter() {
    let env = common::TestEnv::start().await;
    let repo = "docs-read-test-metrics-endpoint";
    seed_corpus(&env, repo, "1.0.0", "metricsendpointsha1", false).await;

    let rendered_before = env.state.metrics_handle.render();
    let list_before = counter_value(
        &rendered_before,
        "akashic_docs_read_total",
        r#"endpoint="list""#,
    );
    let nav_before = counter_value(
        &rendered_before,
        "akashic_docs_read_total",
        r#"endpoint="nav""#,
    );

    let client = reqwest::Client::new();
    let resp_list = client
        .get(docs_url(&env, ""))
        .send()
        .await
        .expect("GET /api/v1/docs");
    assert_eq!(resp_list.status(), 200);

    let resp_nav = client
        .get(docs_url(&env, &format!("/{repo}/latest/nav")))
        .send()
        .await
        .expect("GET /api/v1/docs/:repo/:version/nav");
    assert_eq!(resp_nav.status(), 200);

    let rendered_after = env.state.metrics_handle.render();
    let list_after = counter_value(
        &rendered_after,
        "akashic_docs_read_total",
        r#"endpoint="list""#,
    );
    let nav_after = counter_value(
        &rendered_after,
        "akashic_docs_read_total",
        r#"endpoint="nav""#,
    );

    assert_eq!(
        list_after,
        list_before + 1.0,
        "GET /api/v1/docs must increment akashic_docs_read_total{{endpoint=\"list\"}} by \
         exactly 1, rendered=\n{rendered_after}"
    );
    assert_eq!(
        nav_after,
        nav_before + 1.0,
        "GET .../nav must increment akashic_docs_read_total{{endpoint=\"nav\"}} by exactly 1, \
         rendered=\n{rendered_after}"
    );
    // Each handler must only ever touch its OWN label — a `nav` hit must not
    // also bump `list` (and vice versa), i.e. the label is handler-specific,
    // not a shared "any docs read" counter.
    assert_eq!(
        list_after,
        list_before + 1.0,
        "the nav request must not have also incremented the list counter"
    );
}
