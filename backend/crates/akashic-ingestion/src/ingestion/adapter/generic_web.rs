//! GenericWebAdapter: same-domain link-BFS + HTML→markdown, returning pages in
//! memory. This is the fallback adapter and a behavioral twin of the original
//! `crawler::crawl_website` (which wrote `.md` to disk); here we collect
//! `PageRef`s instead so the pipeline ingests them without a disk round-trip.

use std::collections::{HashSet, VecDeque};

use anyhow::Result;
use async_trait::async_trait;
use tracing::{info, warn};

use super::{CrawlLimits, ExtractedPage, PageRef, ProbeContext, SiteAdapter, SourceSpec};
use crate::ingestion::crawler;

pub struct GenericWebAdapter {
    client: reqwest::Client,
}

impl GenericWebAdapter {
    pub fn new() -> Self {
        Self {
            // A timeout keeps a stalled host from hanging discover/extract.
            client: crawler::build_crawl_client(15),
        }
    }
}

impl Default for GenericWebAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive a document title from a URL: the last non-empty path segment, or
/// "index" for a root path. Mirrors the file-stem title the old disk path used.
pub(crate) fn url_to_title(url: &str) -> String {
    let path = reqwest::Url::parse(url).ok();
    let seg = path
        .as_ref()
        .and_then(|u| u.path_segments())
        .and_then(|mut segs| segs.rfind(|s| !s.is_empty()).map(str::to_string));
    seg.unwrap_or_else(|| "index".to_string())
}

#[async_trait]
impl SiteAdapter for GenericWebAdapter {
    fn id(&self) -> &'static str {
        "generic-web"
    }

    /// Generic is the explicit fallback; it never wins auto-sniff.
    async fn detect(&self, _ctx: &ProbeContext) -> f32 {
        0.1
    }

    async fn discover(&self, src: &SourceSpec, limits: &CrawlLimits) -> Result<Vec<PageRef>> {
        let seed = reqwest::Url::parse(&src.seed_url)?;
        let domain = seed
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("Seed URL has no host"))?
            .to_string();

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, u8)> = VecDeque::new();
        let mut pages: Vec<PageRef> = Vec::new();
        // Bound the crawl by HTTP requests ISSUED, not pages SAVED. Counting
        // saved pages let a site of mostly empty / non-HTML pages fetch far more
        // URLs than the operator's cap (each empty page still fetched and still
        // enqueued ~links), turning `ingest_crawl_max_pages` into a floor on
        // saves rather than a ceiling on requests.
        let mut fetches = 0usize;

        queue.push_back((src.seed_url.clone(), 0));
        visited.insert(crawler::normalize_url(&src.seed_url));

        while let Some((url, depth)) = queue.pop_front() {
            if fetches >= limits.max_pages {
                info!(
                    fetches,
                    saved = pages.len(),
                    max_pages = limits.max_pages,
                    "Reached max pages limit"
                );
                break;
            }

            let fetched = crawler::fetch_page(&self.client, &url).await;
            fetches += 1;

            // Polite delay after EVERY fetch attempt — including failures and
            // empty pages — so a run of failing/non-HTML responses cannot become
            // an undelayed request flood against the target host.
            if limits.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(limits.delay_ms)).await;
            }

            let html = match fetched {
                Ok(h) => h,
                Err(e) => {
                    warn!(url, err = %e, "Failed to fetch page, skipping");
                    continue;
                }
            };

            let markdown = crawler::html_to_markdown(&html);
            if !markdown.trim().is_empty() {
                pages.push(PageRef {
                    title: url_to_title(&url),
                    url: url.clone(),
                    inline_markdown: Some(markdown),
                });
                info!(url, depth, saved = pages.len(), "Crawled page");
            }

            if depth < limits.depth {
                let links =
                    crawler::extract_links(&html, &url, &domain, limits.url_pattern.as_deref());
                for link in links {
                    let normalized = crawler::normalize_url(&link);
                    if !visited.contains(&normalized) {
                        visited.insert(normalized);
                        queue.push_back((link, depth + 1));
                    }
                }
            }
        }

        info!(
            saved = pages.len(),
            fetches, domain, "Generic crawl complete"
        );
        Ok(pages)
    }

    async fn extract(&self, page: &PageRef) -> Result<ExtractedPage> {
        Ok(ExtractedPage {
            url: page.url.clone(),
            title: page.title.clone(),
            markdown: page.inline_markdown.clone().unwrap_or_default(),
            version_coordinate: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingestion::adapter::{PageRef, SiteAdapter};

    #[test]
    fn title_from_url_uses_last_path_segment() {
        assert_eq!(url_to_title("https://x.com/guide/intro"), "intro");
        assert_eq!(url_to_title("https://x.com/"), "index");
        assert_eq!(url_to_title("https://x.com"), "index");
        assert_eq!(url_to_title("https://x.com/a/b/"), "b");
    }

    #[tokio::test]
    async fn extract_passes_through_inline_markdown() {
        let a = GenericWebAdapter::new();
        let page = PageRef {
            url: "https://x.com/p".into(),
            title: "p".into(),
            inline_markdown: Some("# P\n\nbody".into()),
        };
        let ex = a.extract(&page).await.unwrap();
        assert_eq!(ex.markdown, "# P\n\nbody");
        assert_eq!(ex.title, "p");
        assert_eq!(a.id(), "generic-web");
    }
}
