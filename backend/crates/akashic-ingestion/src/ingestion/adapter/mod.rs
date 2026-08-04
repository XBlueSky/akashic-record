//! Pluggable per-source ingestion adapters. A `SiteAdapter` is one cohesive
//! unit that knows how to (1) enumerate the pages of one source and (2) extract
//! clean markdown from each.

pub mod generic_web;
pub mod gitbook_shelf;
pub mod gitbook_static;
pub mod mediawiki;
pub mod registry;
pub mod resolver;
pub mod version_coordinate;
pub mod vitepress_llms;

use anyhow::Result;
use async_trait::async_trait;

use version_coordinate::VersionCoordinate;

/// Limits applied to discovery/crawl, carried from `IngestRequest` + `Config`.
#[derive(Clone, Debug)]
pub struct CrawlLimits {
    pub depth: u8,
    pub max_pages: usize,
    pub delay_ms: u64,
    pub url_pattern: Option<String>,
}

/// One registered source the adapter is asked to ingest.
#[derive(Clone, Debug)]
pub struct SourceSpec {
    pub repo_name: String,
    pub seed_url: String,
}

/// A discovered page. Bulk adapters fill `inline_markdown` during `discover()`
/// (whole corpus in one pass); per-page adapters leave it `None` and fetch in
/// `extract()`. The pipeline calls `discover()`→`extract()` uniformly for both.
#[derive(Clone, Debug)]
pub struct PageRef {
    pub url: String,
    pub title: String,
    pub inline_markdown: Option<String>,
}

/// Clean, ingest-ready content for one page.
#[derive(Clone, Debug)]
pub struct ExtractedPage {
    pub url: String,
    pub title: String,
    pub markdown: String,
    pub version_coordinate: Option<VersionCoordinate>,
}

/// Context for the auto-sniff path: seed URL + already-fetched root HTML.
#[derive(Clone, Debug)]
pub struct ProbeContext {
    pub seed_url: String,
    pub root_html: String,
}

/// A site adapter: discovery + extraction for one kind of source.
#[async_trait]
pub trait SiteAdapter: Send + Sync {
    /// Stable id, e.g. "generic-web" / "mediawiki" / "gitbook-static".
    fn id(&self) -> &'static str;

    /// Cheap probe for the auto-sniff path. Returns confidence in [0,1].
    /// Preset sources are pinned and bypass this.
    async fn detect(&self, ctx: &ProbeContext) -> f32;

    /// Enumerate the pages to ingest. May use a bulk source or link-BFS.
    async fn discover(&self, src: &SourceSpec, limits: &CrawlLimits) -> Result<Vec<PageRef>>;

    /// Produce clean markdown for one page. Bulk adapters pass through the
    /// content attached during `discover()`; per-page adapters fetch here.
    async fn extract(&self, page: &PageRef) -> Result<ExtractedPage>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyAdapter;

    #[async_trait::async_trait]
    impl SiteAdapter for DummyAdapter {
        fn id(&self) -> &'static str {
            "dummy"
        }
        async fn detect(&self, _ctx: &ProbeContext) -> f32 {
            1.0
        }
        async fn discover(
            &self,
            src: &SourceSpec,
            _limits: &CrawlLimits,
        ) -> anyhow::Result<Vec<PageRef>> {
            Ok(vec![PageRef {
                url: src.seed_url.clone(),
                title: "Home".to_string(),
                inline_markdown: Some("# Home\n\nhi".to_string()),
            }])
        }
        async fn extract(&self, page: &PageRef) -> anyhow::Result<ExtractedPage> {
            Ok(ExtractedPage {
                url: page.url.clone(),
                title: page.title.clone(),
                markdown: page.inline_markdown.clone().unwrap_or_default(),
                version_coordinate: None,
            })
        }
    }

    #[tokio::test]
    async fn adapter_discover_extract_roundtrip() {
        let a = DummyAdapter;
        let src = SourceSpec {
            repo_name: "r".into(),
            seed_url: "http://x/".into(),
        };
        let limits = CrawlLimits {
            depth: 1,
            max_pages: 10,
            delay_ms: 0,
            url_pattern: None,
        };
        let pages = a.discover(&src, &limits).await.unwrap();
        assert_eq!(pages.len(), 1);
        let ex = a.extract(&pages[0]).await.unwrap();
        assert_eq!(ex.markdown, "# Home\n\nhi");
        assert_eq!(a.id(), "dummy");
    }
}
