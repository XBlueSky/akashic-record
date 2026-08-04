//! Integration tests for Plan-3 Task 5 (E1/E2/E4): the on-the-fly AI-surface
//! text endpoints — /docs/{repo}/llms.txt, llms-full.txt, skill.md and the
//! global /llms.txt.
//!
//! Requires the live Postgres bench (`common::TestEnv::start()`) — CI-run
//! only; on the dev host these are compile-checked, never executed (the
//! persistent :5432 stack must not be TestEnv's target).

mod common;

use akashic_domain::ports::corpus::{InsertOutcome, NewCorpusFile, NewCorpusVersion};
use akashic_domain::types::corpus::{
    CorpusManifest, CorpusSource, CorpusVersionMeta, NavGroup, NavPage, NavTree,
};

fn sample_nav() -> NavTree {
    NavTree {
        description: "LLMS surface test corpus.".to_string(),
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

const INDEX_MD: &str = "# Index\n\nWelcome.\n\n## All pages\n\n- [Setup Page](guide/setup.md)\n";
const SETUP_MD: &str = "# Setup\n\nInstall steps here.\n";
const ORPHAN_MD: &str = "# Orphan\n\nNot in nav.\n";
const ZHTW_MD: &str = "# \u{5b89}\u{88dd}\n\nzh-TW mirror page.\n";

/// Seed one corpus version with a nav page, an orphan page, and a zh-TW
/// mirror page. sha = "llmssurfacesha1" → sha7 stamp fragment "llmssur".
async fn seed_corpus(env: &common::TestEnv, repo: &str) -> CorpusVersionMeta {
    let manifest = CorpusManifest {
        repo: repo.to_string(),
        version: "1.0.0".to_string(),
        sha: "llmssurfacesha1".to_string(),
        index: "index.md".to_string(),
        languages: vec!["en".to_string(), "zh-TW".to_string()],
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
            path: "guide/orphan.md".to_string(),
            content: ORPHAN_MD.as_bytes().to_vec(),
            is_markdown: true,
        },
        NewCorpusFile {
            path: "zh-TW/guide/setup.md".to_string(),
            content: ZHTW_MD.as_bytes().to_vec(),
            is_markdown: true,
        },
    ];
    let outcome = env
        .state
        .corpus_store
        .insert_version(NewCorpusVersion {
            manifest,
            nav: sample_nav(),
            source: CorpusSource::Push,
            is_tagged: false,
            files,
        })
        .await
        .expect("insert_version");
    match outcome {
        InsertOutcome::Inserted(meta) | InsertOutcome::Existing(meta) => meta,
    }
}

fn url(env: &common::TestEnv, suffix: &str) -> String {
    format!("http://{}{suffix}", env.app_addr)
}

#[tokio::test]
#[serial_test::serial]
async fn repo_llms_txt_renders_nav_with_stamp_etag_no_cache_and_304() {
    let env = common::TestEnv::start().await;
    let repo = "llms-surface-txt";
    seed_corpus(&env, repo).await;

    let resp = reqwest::get(url(&env, &format!("/docs/{repo}/llms.txt")))
        .await
        .expect("GET llms.txt");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/plain; charset=utf-8");
    assert_eq!(resp.headers()["cache-control"], "no-cache");
    let etag = resp.headers()["etag"].to_str().unwrap().to_string();
    assert_eq!(etag, "\"llmssurfacesha1\"");
    let body = resp.text().await.unwrap();
    assert!(body.starts_with(&format!("# {repo}\n")));
    assert!(body.contains("<!-- documents llms-surface-txt 1.0.0 @ llmssur -->"));
    assert!(body.contains("## Guide"));
    assert!(body.contains(&format!(
        "/api/v1/docs/{repo}/latest/raw/guide/setup.md): Install and configure."
    )));
    assert!(body.contains(&format!("/docs/{repo}/llms-full.txt")));

    let client = reqwest::Client::new();
    let resp2 = client
        .get(url(&env, &format!("/docs/{repo}/llms.txt")))
        .header("if-none-match", etag)
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 304);
}

#[tokio::test]
#[serial_test::serial]
async fn repo_llms_full_nav_first_orphans_appended_zh_tw_excluded() {
    let env = common::TestEnv::start().await;
    let repo = "llms-surface-full";
    seed_corpus(&env, repo).await;

    let resp = reqwest::get(url(&env, &format!("/docs/{repo}/llms-full.txt")))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/plain; charset=utf-8");
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("<!-- documents llms-surface-full 1.0.0 @ llmssur -->"));
    let setup_pos = body.find("raw/guide/setup.md").expect("nav page present");
    let orphan_pos = body.find("raw/guide/orphan.md").expect("orphan appended");
    let index_pos = body
        .find("raw/index.md")
        .expect("index appended (not in nav)");
    assert!(
        setup_pos < orphan_pos && setup_pos < index_pos,
        "nav pages come before appended rest"
    );
    assert!(!body.contains("zh-TW/"), "zh-TW subtree excluded");
    assert!(body.contains("Install steps here."));
    assert!(body.contains("Not in nav."));
}

#[tokio::test]
#[serial_test::serial]
async fn repo_skill_md_valid_frontmatter_and_stamp() {
    let env = common::TestEnv::start().await;
    let repo = "llms-surface-skill";
    seed_corpus(&env, repo).await;

    let resp = reqwest::get(url(&env, &format!("/docs/{repo}/skill.md")))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-type"],
        "text/markdown; charset=utf-8"
    );
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("---\nname: llms-surface-skill-docs\ndescription: \""));
    assert!(body.contains("\n## All pages\n"));
    assert!(body.contains("`search_knowledge`"));
    assert!(body.contains("<!-- documents llms-surface-skill 1.0.0 @ llmssur -->"));
}

#[tokio::test]
#[serial_test::serial]
async fn global_llms_txt_lists_repos_no_cache() {
    let env = common::TestEnv::start().await;
    let repo = "llms-surface-global";
    seed_corpus(&env, repo).await;

    let resp = reqwest::get(url(&env, "/llms.txt")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["cache-control"], "no-cache");
    let body = resp.text().await.unwrap();
    assert!(body.contains(&format!(
        "/docs/{repo}/llms.txt): LLMS surface test corpus."
    )));
}

#[tokio::test]
#[serial_test::serial]
async fn llms_endpoints_404_unknown_repo() {
    let env = common::TestEnv::start().await;
    for suffix in [
        "/docs/no-such-repo/llms.txt",
        "/docs/no-such-repo/llms-full.txt",
        "/docs/no-such-repo/skill.md",
    ] {
        let resp = reqwest::get(url(&env, suffix)).await.unwrap();
        assert_eq!(resp.status(), 404, "{suffix}");
    }
}
