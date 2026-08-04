//! GitBook v3 static adapter: one fetch of `search_index.json` (a lunr index
//! `{index, store}`) yields every page from `store` (each `{url, title, body}`,
//! body already plain text). Fully bulk: discover fills inline_markdown, extract
//! passes it through and infers the version coordinate.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Url;
use std::collections::HashMap;

use super::version_coordinate::infer_version_coordinate;
use super::{CrawlLimits, ExtractedPage, PageRef, ProbeContext, SiteAdapter, SourceSpec};
use crate::ingestion::crawler;

pub struct GitbookStaticAdapter {
    client: reqwest::Client,
}

impl GitbookStaticAdapter {
    pub fn new() -> Self {
        Self {
            client: crawler::build_crawl_client(15),
        }
    }
}

impl Default for GitbookStaticAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(serde::Deserialize)]
struct SearchIndex {
    #[serde(default)]
    store: HashMap<String, StoreEntry>,
}

#[derive(serde::Deserialize)]
struct StoreEntry {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

/// Ensure a seed URL ends with `/` so `Url::join` treats it as the book's base
/// directory (so `join("guide/x.html")` appends rather than replacing the last
/// path segment).
fn dir_base(seed_url: &str) -> Result<Url> {
    let s = if seed_url.ends_with('/') {
        seed_url.to_string()
    } else {
        format!("{seed_url}/")
    };
    Url::parse(&s).context("invalid seed_url for gitbook-static")
}

/// Parse a GitBook v3 `search_index.json` into pages, resolving each `store`
/// entry's relative url against `book_base`.
pub(crate) fn parse_search_index(json: &str, book_base: &Url) -> Result<Vec<PageRef>> {
    let idx: SearchIndex = serde_json::from_str(json).context("invalid search_index.json")?;
    let mut pages = Vec::with_capacity(idx.store.len());
    for entry in idx.store.values() {
        let resolved = book_base
            .join(&entry.url)
            .unwrap_or_else(|_| book_base.clone())
            .to_string();
        let title = if entry.title.is_empty() {
            resolved.clone()
        } else {
            entry.title.clone()
        };
        pages.push(PageRef {
            url: resolved,
            title,
            inline_markdown: Some(entry.body.clone()),
        });
    }
    Ok(pages)
}

#[async_trait]
impl SiteAdapter for GitbookStaticAdapter {
    fn id(&self) -> &'static str {
        "gitbook-static"
    }

    async fn detect(&self, ctx: &ProbeContext) -> f32 {
        // GitBook v3 static output carries a `generator` meta whose content is
        // "GitBook 3.x" — that version string is specific enough on its own.
        if ctx.root_html.to_lowercase().contains("gitbook 3") {
            0.9
        } else {
            0.0
        }
    }

    async fn discover(&self, src: &SourceSpec, limits: &CrawlLimits) -> Result<Vec<PageRef>> {
        let base = dir_base(&src.seed_url)?;
        let index_url = base
            .join("search_index.json")
            .context("could not build search_index.json URL")?;
        let json = crawler::fetch_text(&self.client, index_url.as_str()).await?;
        let mut pages = parse_search_index(&json, &base)?;
        // store is HashMap-iterated (nondeterministic order); sort so truncation
        // keeps a stable subset when a book exceeds max_pages.
        pages.sort_by(|a, b| a.url.cmp(&b.url));
        pages.truncate(limits.max_pages);
        Ok(pages)
    }

    async fn extract(&self, page: &PageRef) -> Result<ExtractedPage> {
        Ok(ExtractedPage {
            url: page.url.clone(),
            title: page.title.clone(),
            markdown: page.inline_markdown.clone().unwrap_or_default(),
            version_coordinate: infer_version_coordinate(&page.url),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingestion::adapter::{PageRef, ProbeContext, SiteAdapter};

    const FIXTURE: &str = r#"{
      "index": {"ignored": true},
      "store": {
        "./": {"url": "./", "title": "Introduction", "keywords": "", "body": "Welcome to the book."},
        "guide/setup.html": {"url": "guide/setup.html", "title": "Setup", "keywords": "", "body": "Install steps here."}
      }
    }"#;

    fn base() -> reqwest::Url {
        reqwest::Url::parse("https://dit.pages.example.com/book/ci-guide/").unwrap()
    }

    #[test]
    fn parse_resolves_store_urls_and_keeps_body() {
        let mut pages = parse_search_index(FIXTURE, &base()).unwrap();
        pages.sort_by(|a, b| a.url.cmp(&b.url));
        assert_eq!(pages.len(), 2);
        // "./" resolves to the book base; "guide/setup.html" appends to it.
        assert!(
            pages
                .iter()
                .any(|p| p.url == "https://dit.pages.example.com/book/ci-guide/"
                    && p.title == "Introduction"
                    && p.inline_markdown.as_deref() == Some("Welcome to the book."))
        );
        assert!(pages.iter().any(|p| p.url
            == "https://dit.pages.example.com/book/ci-guide/guide/setup.html"
            && p.title == "Setup"
            && p.inline_markdown.as_deref() == Some("Install steps here.")));
    }

    #[test]
    fn dir_base_appends_trailing_slash() {
        assert_eq!(
            dir_base("https://x/book/a").unwrap().as_str(),
            "https://x/book/a/"
        );
        assert_eq!(
            dir_base("https://x/book/a/").unwrap().as_str(),
            "https://x/book/a/"
        );
    }

    #[tokio::test]
    async fn extract_passes_body_and_infers_version() {
        let a = GitbookStaticAdapter::new();
        let page = PageRef {
            url: "https://x/v7/setup".into(),
            title: "Setup".into(),
            inline_markdown: Some("body".into()),
        };
        let ex = a.extract(&page).await.unwrap();
        assert_eq!(ex.markdown, "body");
        assert_eq!(
            ex.version_coordinate.unwrap().version_range.as_deref(),
            Some("7")
        );
        assert_eq!(a.id(), "gitbook-static");
    }

    #[tokio::test]
    async fn detect_scores_gitbook_generator_high() {
        let a = GitbookStaticAdapter::new();
        let hit = ProbeContext {
            seed_url: "https://x/".into(),
            root_html: r#"<meta name="generator" content="GitBook 3.2.3">"#.into(),
        };
        let miss = ProbeContext {
            seed_url: "https://x/".into(),
            root_html: "<html></html>".into(),
        };
        assert!(a.detect(&hit).await >= 0.9);
        assert_eq!(a.detect(&miss).await, 0.0);
    }
}
