//! GitBook shelf adapter: the AngularJS shelf exposes `/api/book/list`, and
//! each book is static GitBook output at `<origin>/<name>/`. discover enumerates
//! the books and reuses the gitbook-static `search_index.json` parser per book
//! (with per-book failure isolation); extract is the gitbook-static passthrough.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Url;
use tracing::warn;

use super::gitbook_static::parse_search_index;
use super::version_coordinate::infer_version_coordinate;
use super::{CrawlLimits, ExtractedPage, PageRef, ProbeContext, SiteAdapter, SourceSpec};
use crate::ingestion::crawler;

pub struct GitbookShelfAdapter {
    client: reqwest::Client,
}

impl GitbookShelfAdapter {
    pub fn new() -> Self {
        Self {
            client: crawler::build_crawl_client(15),
        }
    }
}

impl Default for GitbookShelfAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(serde::Deserialize)]
struct ShelfBook {
    name: String,
}

/// Parse the shelf's `/api/book/list` JSON (`[{type,title,name,description?}]`)
/// into the list of book `name` slugs.
fn parse_book_list(json: &str) -> Result<Vec<String>> {
    let books: Vec<ShelfBook> =
        serde_json::from_str(json).context("invalid /api/book/list JSON")?;
    Ok(books.into_iter().map(|b| b.name).collect())
}

#[async_trait]
impl SiteAdapter for GitbookShelfAdapter {
    fn id(&self) -> &'static str {
        "gitbook-shelf"
    }

    async fn detect(&self, ctx: &ProbeContext) -> f32 {
        // The shelf root carries the "Gitbook Shelf" title.
        if ctx.root_html.to_lowercase().contains("gitbook shelf") {
            0.9
        } else {
            0.0
        }
    }

    async fn discover(&self, src: &SourceSpec, limits: &CrawlLimits) -> Result<Vec<PageRef>> {
        let origin = Url::parse(&src.seed_url).context("invalid seed_url for gitbook-shelf")?;
        let list_url = origin
            .join("/api/book/list")
            .context("build /api/book/list URL")?;
        let json = crawler::fetch_text(&self.client, list_url.as_str()).await?;
        let names = parse_book_list(&json)?;

        let mut pages = Vec::new();
        for name in names {
            // Each book is static GitBook at `<origin>/<name>/`.
            let book_base = match origin.join(&format!("/{name}/")) {
                Ok(u) => u,
                Err(e) => {
                    warn!(book = %name, err = %e, "bad book name; skipping");
                    continue;
                }
            };
            let idx_url = match book_base.join("search_index.json") {
                Ok(u) => u,
                Err(e) => {
                    warn!(book = %name, err = %e, "build search_index URL failed; skipping");
                    continue;
                }
            };
            // Per-book isolation: a failed book never sinks the whole shelf.
            match crawler::fetch_text(&self.client, idx_url.as_str()).await {
                Ok(idx_json) => match parse_search_index(&idx_json, &book_base) {
                    Ok(book_pages) => pages.extend(book_pages),
                    Err(e) => {
                        warn!(book = %name, err = %e, "parse search_index failed; skipping book")
                    }
                },
                Err(e) => warn!(book = %name, err = %e, "fetch search_index failed; skipping book"),
            }
        }

        // Deterministic across the multi-book aggregate.
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

    #[test]
    fn parse_book_list_extracts_name_slugs() {
        let json = r#"[
          {"type":"basic","title":"Disk Diag","name":"disk-diag"},
          {"type":"basic","title":"APM","name":"apm-docs","description":"d"}
        ]"#;
        assert_eq!(
            parse_book_list(json).unwrap(),
            vec!["disk-diag".to_string(), "apm-docs".to_string()]
        );
    }

    #[test]
    fn parse_book_list_empty_is_empty() {
        assert_eq!(parse_book_list("[]").unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn extract_passes_body_and_infers_version() {
        let a = GitbookShelfAdapter::new();
        let page = PageRef {
            url: "http://gitbook.example.com/v7/x".into(),
            title: "X".into(),
            inline_markdown: Some("body".into()),
        };
        let ex = a.extract(&page).await.unwrap();
        assert_eq!(ex.markdown, "body");
        assert_eq!(
            ex.version_coordinate.unwrap().version_range.as_deref(),
            Some("7")
        );
        assert_eq!(a.id(), "gitbook-shelf");
    }

    #[tokio::test]
    async fn detect_scores_shelf_high() {
        let a = GitbookShelfAdapter::new();
        let hit = ProbeContext {
            seed_url: "http://gitbook.example.com/".into(),
            root_html: "<title>Gitbook Shelf</title>".into(),
        };
        let miss = ProbeContext {
            seed_url: "http://gitbook.example.com/".into(),
            root_html: "<html></html>".into(),
        };
        assert!(a.detect(&hit).await >= 0.9);
        assert_eq!(a.detect(&miss).await, 0.0);
    }
}
