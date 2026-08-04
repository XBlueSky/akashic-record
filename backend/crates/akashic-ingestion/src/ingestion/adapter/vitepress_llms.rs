//! VitePress llms adapter: one fetch of `<origin>/llms-full.txt` (the whole
//! corpus) split into pages by YAML frontmatter blocks. A page boundary is a
//! `---`…`---` block whose interior has a `url:` key; the page body is the
//! markdown until the next such block, with the trailing end-marker `---`/blank
//! lines trimmed. Output is in document order (so truncate is deterministic).

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Url;

use super::version_coordinate::infer_version_coordinate;
use super::{CrawlLimits, ExtractedPage, PageRef, ProbeContext, SiteAdapter, SourceSpec};
use crate::ingestion::crawler;

pub struct VitepressLlmsAdapter {
    client: reqwest::Client,
}

impl VitepressLlmsAdapter {
    pub fn new() -> Self {
        Self {
            client: crawler::build_crawl_client(15),
        }
    }
}

impl Default for VitepressLlmsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Split an `llms-full.txt` corpus into pages. `origin` is the site URL the
/// frontmatter `url:` paths (which are origin-absolute, e.g. `/a/intro.md`) are
/// resolved against.
fn parse_llms_full(text: &str, origin: &Url) -> Vec<PageRef> {
    let lines: Vec<&str> = text.lines().collect();

    // 1. Locate frontmatter blocks: a `---`…`---` whose interior has a `url:` line.
    //    Each found block starts a page; (close_index, url) is what we need.
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "---"
            && let Some(close) = (i + 1..lines.len()).find(|&j| lines[j].trim() == "---")
        {
            let url = lines[i + 1..close]
                .iter()
                .find_map(|l| l.trim().strip_prefix("url:").map(|v| v.trim().to_string()));
            if let Some(url) = url {
                blocks.push((close, url));
                i = close + 1;
                continue;
            }
        }
        i += 1;
    }

    // 2. One page per block. Body = lines (this block's close .. next block's
    //    open). We don't track each block's open separately, so recompute the
    //    next boundary by scanning forward for the next recorded close's frontmatter.
    let mut pages = Vec::with_capacity(blocks.len());
    for (k, (close, url)) in blocks.iter().enumerate() {
        // The next page's body starts after the next frontmatter's close; its
        // body region ends just before that frontmatter opens. Since we only
        // stored closes, the body for page k runs from `close+1` up to the line
        // before the next page's frontmatter opening. We reconstruct the next
        // opening as: walk back from the next close to its matching open `---`.
        let body_end = if let Some((next_close, _)) = blocks.get(k + 1) {
            // next frontmatter opening = the `---` line that starts the block
            // ending at next_close: scan backward from next_close-1 for the
            // nearest `---`.
            (0..*next_close)
                .rfind(|&j| lines[j].trim() == "---")
                .unwrap_or(*next_close)
        } else {
            lines.len()
        };

        let mut body = &lines[close + 1..body_end];
        // trim leading blanks
        while body.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
            body = &body[1..];
        }
        // trim trailing blanks / lone `---` end-markers
        while body
            .last()
            .map(|l| l.trim().is_empty() || l.trim() == "---")
            .unwrap_or(false)
        {
            body = &body[..body.len() - 1];
        }

        let markdown = body.join("\n");
        let title = body
            .iter()
            .find_map(|l| l.trim().strip_prefix("# ").map(|t| t.trim().to_string()))
            .unwrap_or_else(|| url.clone());
        let resolved = origin
            .join(url)
            .unwrap_or_else(|_| origin.clone())
            .to_string();

        pages.push(PageRef {
            url: resolved,
            title,
            inline_markdown: Some(markdown),
        });
    }

    pages
}

#[async_trait]
impl SiteAdapter for VitepressLlmsAdapter {
    fn id(&self) -> &'static str {
        "vitepress-llms"
    }

    async fn detect(&self, ctx: &ProbeContext) -> f32 {
        if ctx.root_html.to_lowercase().contains("vitepress") {
            0.9
        } else {
            0.0
        }
    }

    async fn discover(&self, src: &SourceSpec, limits: &CrawlLimits) -> Result<Vec<PageRef>> {
        let base = Url::parse(&src.seed_url).context("invalid seed_url for vitepress-llms")?;
        // llms-full.txt lives at the site root; an absolute path joins against origin.
        let llms_url = base
            .join("/llms-full.txt")
            .context("could not build llms-full.txt URL")?;
        let text = crawler::fetch_text(&self.client, llms_url.as_str()).await?;
        let mut pages = parse_llms_full(&text, &base);
        pages.truncate(limits.max_pages); // document order ⇒ deterministic
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

    // Two pages separated by an end-marker `---` then the next frontmatter,
    // mirroring the real llms-full.txt structure.
    const FIXTURE: &str = "---\nurl: /a/intro.md\ndescription: First.\n---\n\n# Intro\n\nIntro body.\n\n---\n\n---\nurl: /b/setup.md\ndescription: Second.\n---\n\n# Setup\n\nSetup body.\n";

    fn origin() -> reqwest::Url {
        reqwest::Url::parse("https://docs.example.com/").unwrap()
    }

    #[test]
    fn splits_pages_by_frontmatter_and_resolves_urls() {
        let pages = parse_llms_full(FIXTURE, &origin());
        assert_eq!(pages.len(), 2, "expected 2 pages, got {}", pages.len());

        assert_eq!(pages[0].url, "https://docs.example.com/a/intro.md");
        assert_eq!(pages[0].title, "Intro");
        // body keeps the H1, drops the trailing end-marker `---`.
        assert_eq!(
            pages[0].inline_markdown.as_deref(),
            Some("# Intro\n\nIntro body.")
        );

        assert_eq!(pages[1].url, "https://docs.example.com/b/setup.md");
        assert_eq!(pages[1].title, "Setup");
        assert_eq!(
            pages[1].inline_markdown.as_deref(),
            Some("# Setup\n\nSetup body.")
        );
    }

    #[test]
    fn content_triple_dash_without_url_is_not_a_page_boundary() {
        // A `---` horizontal rule inside body (no `url:` in the block) must not
        // split a page.
        let text = "---\nurl: /x.md\n---\n\n# X\n\nbefore\n\n---\n\nafter\n";
        let pages = parse_llms_full(text, &origin());
        assert_eq!(pages.len(), 1);
        assert!(
            pages[0]
                .inline_markdown
                .as_deref()
                .unwrap()
                .contains("before")
        );
        assert!(
            pages[0]
                .inline_markdown
                .as_deref()
                .unwrap()
                .contains("after")
        );
    }

    #[tokio::test]
    async fn extract_passes_body_and_infers_version() {
        let a = VitepressLlmsAdapter::new();
        let page = PageRef {
            url: "https://x/v7/page".into(),
            title: "P".into(),
            inline_markdown: Some("# P\n\nbody".into()),
        };
        let ex = a.extract(&page).await.unwrap();
        assert_eq!(ex.markdown, "# P\n\nbody");
        assert_eq!(
            ex.version_coordinate.unwrap().version_range.as_deref(),
            Some("7")
        );
        assert_eq!(a.id(), "vitepress-llms");
    }

    #[tokio::test]
    async fn detect_scores_vitepress_high() {
        let a = VitepressLlmsAdapter::new();
        let hit = ProbeContext {
            seed_url: "https://x/".into(),
            root_html: r#"<meta name="generator" content="VitePress v2.0.0">"#.into(),
        };
        let miss = ProbeContext {
            seed_url: "https://x/".into(),
            root_html: "<html></html>".into(),
        };
        assert!(a.detect(&hit).await >= 0.9);
        assert_eq!(a.detect(&miss).await, 0.0);
    }

    /// E8 round-trip dogfood: the platform's own `llms-full.txt` output
    /// (akashic-domain renderer) must parse back through THIS adapter into
    /// the exact page set + content it was generated from — akashic can
    /// re-ingest itself.
    #[test]
    fn round_trip_own_llms_full_output() {
        use akashic_domain::algos::llms::{LlmsPage, render_llms_full};

        let pages = vec![
            LlmsPage {
                path: "index.md".into(),
                markdown: "# Acme\n\nOverview text.\n".into(),
            },
            LlmsPage {
                path: "guide/setup.md".into(),
                markdown: "# Setup\n\nInstall steps.\n\n## Deeper\n\nMore detail.".into(),
            },
        ];
        let text = render_llms_full(
            "acme",
            "1.0.0",
            "0123456789abcdef",
            &pages,
            "https://akashic.example.com",
        );
        let origin = Url::parse("https://akashic.example.com").expect("origin");

        let parsed = parse_llms_full(&text, &origin);

        assert_eq!(parsed.len(), pages.len(), "page set must round-trip");
        for (got, want) in parsed.iter().zip(&pages) {
            assert_eq!(
                got.url,
                format!(
                    "https://akashic.example.com/api/v1/docs/acme/latest/raw/{}",
                    want.path
                )
            );
            assert_eq!(
                got.inline_markdown.as_deref().map(str::trim_end),
                Some(want.markdown.trim_end()),
                "content must round-trip modulo trailing whitespace ({})",
                want.path
            );
        }
        // titles come from the first `# ` heading
        assert_eq!(parsed[0].title, "Acme");
        assert_eq!(parsed[1].title, "Setup");
    }
}
