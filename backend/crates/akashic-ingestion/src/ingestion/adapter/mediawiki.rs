//! MediaWiki adapter: discover via the `allpages` API (paginated), extract each
//! page's wikitext via `parse&prop=wikitext`, convert wikitext → markdown-ish
//! text. discover-bulk + extract-per-page; `allpages` is alphabetical so
//! truncate is deterministic.

use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use reqwest::Url;
use std::sync::LazyLock;

use super::version_coordinate::infer_version_coordinate;
use super::{CrawlLimits, ExtractedPage, PageRef, ProbeContext, SiteAdapter, SourceSpec};
use crate::ingestion::crawler;

pub struct MediaWikiAdapter {
    client: reqwest::Client,
}

impl MediaWikiAdapter {
    pub fn new() -> Self {
        Self {
            client: crawler::build_crawl_client(15),
        }
    }
}

impl Default for MediaWikiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// ── wikitext → text ──────────────────────────────────────────────────────────
static RE_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
static RE_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<ref[^>]*>.*?</ref>").unwrap());
static RE_REF_SELF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<ref[^>]*/>").unwrap());
static RE_TEMPLATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{[^{}]*\}\}").unwrap());
static RE_LINK_PIPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[[^|\]]+\|([^\]]+)\]\]").unwrap());
static RE_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\[([^\]]+)\]\]").unwrap());
static RE_EXTLINK_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[https?://\S+\s+([^\]]+)\]").unwrap());
static RE_EXTLINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(https?://\S+)\]").unwrap());
static RE_BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"'''(.+?)'''").unwrap());
static RE_ITALIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"''(.+?)''").unwrap());
static RE_BLANKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());

/// Convert MediaWiki wikitext to markdown-ish plain text. Best-effort.
fn wikitext_to_text(wt: &str) -> String {
    // 1. comments + refs
    let mut s = RE_COMMENT.replace_all(wt, "").into_owned();
    s = RE_REF.replace_all(&s, "").into_owned();
    s = RE_REF_SELF.replace_all(&s, "").into_owned();
    // 2. templates — iterate for nesting (each pass strips innermost {{...}})
    for _ in 0..5 {
        let next = RE_TEMPLATE.replace_all(&s, "").into_owned();
        if next == s {
            break;
        }
        s = next;
    }
    // 3. headers, line by line: `== H ==` -> `## H` (Rust regex has no
    //    backrefs, so match the `=` run lengths in code).
    let s = s
        .lines()
        .map(|line| {
            let t = line.trim();
            if t.len() >= 2 && t.starts_with('=') && t.ends_with('=') {
                let lead = t.chars().take_while(|&c| c == '=').count();
                let trail = t.chars().rev().take_while(|&c| c == '=').count();
                let level = lead.min(trail);
                if (1..=6).contains(&level) && t.len() > level * 2 {
                    let inner = t[level..t.len() - level].trim();
                    return format!("{} {}", "#".repeat(level), inner);
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    // 4. links (piped first, then plain; external with text then bare)
    let s = RE_LINK_PIPE.replace_all(&s, "$1").into_owned();
    let s = RE_LINK.replace_all(&s, "$1").into_owned();
    let s = RE_EXTLINK_TEXT.replace_all(&s, "$1").into_owned();
    let s = RE_EXTLINK.replace_all(&s, "$1").into_owned();
    // 5. bold before italic (''' contains '')
    let s = RE_BOLD.replace_all(&s, "**$1**").into_owned();
    let s = RE_ITALIC.replace_all(&s, "*$1*").into_owned();
    // 6. collapse 3+ newlines
    RE_BLANKS.replace_all(&s, "\n\n").trim().to_string()
}

// ── API response parsing ─────────────────────────────────────────────────────
#[derive(serde::Deserialize)]
struct AllPagesResp {
    #[serde(rename = "continue")]
    cont: Option<ContinueToken>,
    query: Option<AllPagesQuery>,
}
#[derive(serde::Deserialize)]
struct ContinueToken {
    apcontinue: Option<String>,
}
#[derive(serde::Deserialize)]
struct AllPagesQuery {
    #[serde(default)]
    allpages: Vec<AllPage>,
}
#[derive(serde::Deserialize)]
struct AllPage {
    title: String,
}

/// Parse one `allpages` API page: returns (titles, next apcontinue token).
fn parse_allpages_page(json: &str) -> Result<(Vec<String>, Option<String>)> {
    let resp: AllPagesResp = serde_json::from_str(json).context("invalid allpages JSON")?;
    let titles = resp
        .query
        .map(|q| q.allpages.into_iter().map(|p| p.title).collect())
        .unwrap_or_default();
    let cont = resp.cont.and_then(|c| c.apcontinue);
    Ok((titles, cont))
}

#[derive(serde::Deserialize)]
struct ParseResp {
    parse: Option<ParseInner>,
}
#[derive(serde::Deserialize)]
struct ParseInner {
    wikitext: WikitextField,
}
#[derive(serde::Deserialize)]
struct WikitextField {
    #[serde(rename = "*")]
    star: String,
}

/// Parse a `parse&prop=wikitext` response into the raw wikitext string.
fn parse_wikitext_response(json: &str) -> Result<String> {
    let resp: ParseResp = serde_json::from_str(json).context("invalid parse JSON")?;
    resp.parse
        .map(|p| p.wikitext.star)
        .context("parse response had no wikitext")
}

#[async_trait]
impl SiteAdapter for MediaWikiAdapter {
    fn id(&self) -> &'static str {
        "mediawiki"
    }

    async fn detect(&self, ctx: &ProbeContext) -> f32 {
        if ctx.root_html.to_lowercase().contains("mediawiki") {
            0.9
        } else {
            0.0
        }
    }

    async fn discover(&self, src: &SourceSpec, limits: &CrawlLimits) -> Result<Vec<PageRef>> {
        let base = Url::parse(&src.seed_url).context("invalid seed_url for mediawiki")?;
        let mut pages = Vec::new();
        let mut apcontinue: Option<String> = None;

        loop {
            let mut api = base.join("/api.php").context("build api.php URL")?;
            {
                let mut q = api.query_pairs_mut();
                q.append_pair("action", "query");
                q.append_pair("list", "allpages");
                q.append_pair("aplimit", "500");
                q.append_pair("format", "json");
                if let Some(c) = &apcontinue {
                    q.append_pair("apcontinue", c);
                }
            }
            let json = crawler::fetch_text(&self.client, api.as_str()).await?;
            let (titles, next) = parse_allpages_page(&json)?;
            if titles.is_empty() {
                break; // no progress this batch — stop regardless of apcontinue
            }

            for title in titles {
                let mut page_url = base.join("/index.php").context("build index.php URL")?;
                page_url.query_pairs_mut().append_pair("title", &title);
                pages.push(PageRef {
                    url: page_url.to_string(),
                    title,
                    inline_markdown: None,
                });
                if pages.len() >= limits.max_pages {
                    return Ok(pages);
                }
            }

            match next {
                Some(c) => apcontinue = Some(c),
                None => break,
            }
        }
        Ok(pages)
    }

    async fn extract(&self, page: &PageRef) -> Result<ExtractedPage> {
        // Reconstruct the API base from the page URL's origin.
        let base = Url::parse(&page.url).context("invalid page url")?;
        let mut api = base.join("/api.php").context("build api.php URL")?;
        {
            let mut q = api.query_pairs_mut();
            q.append_pair("action", "parse");
            q.append_pair("page", &page.title);
            q.append_pair("prop", "wikitext");
            q.append_pair("redirects", "1");
            q.append_pair("format", "json");
        }
        let json = crawler::fetch_text(&self.client, api.as_str()).await?;
        let wikitext = parse_wikitext_response(&json)?;
        Ok(ExtractedPage {
            url: page.url.clone(),
            title: page.title.clone(),
            markdown: wikitext_to_text(&wikitext),
            version_coordinate: infer_version_coordinate(&page.url),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingestion::adapter::{ProbeContext, SiteAdapter};

    #[test]
    fn wikitext_converts_headers_links_bold_and_strips_templates() {
        let wt = "{{Infobox|x=1}}\n== Category ==\n=== By Dept ===\n* [[:Category:RD|RD]] and [[Plain]] articles.\n'''bold''' and ''em''.\n<!-- c -->\n";
        let out = wikitext_to_text(wt);
        assert!(out.contains("## Category"), "got: {out}");
        assert!(out.contains("### By Dept"));
        assert!(out.contains("* RD and Plain articles."));
        assert!(out.contains("**bold** and *em*."));
        assert!(!out.contains("Infobox"), "template not stripped: {out}");
        assert!(!out.contains("<!--"));
    }

    #[test]
    fn parse_allpages_extracts_titles_and_continue() {
        let json = r#"{"batchcomplete":"","continue":{"apcontinue":"Gamma","continue":"-||"},"query":{"allpages":[{"pageid":1,"ns":0,"title":"Alpha"},{"pageid":2,"ns":0,"title":"Beta"}]}}"#;
        let (titles, cont) = parse_allpages_page(json).unwrap();
        assert_eq!(titles, vec!["Alpha".to_string(), "Beta".to_string()]);
        assert_eq!(cont.as_deref(), Some("Gamma"));
    }

    #[test]
    fn parse_allpages_no_continue_is_none() {
        let json =
            r#"{"batchcomplete":"","query":{"allpages":[{"pageid":3,"ns":0,"title":"Zeta"}]}}"#;
        let (titles, cont) = parse_allpages_page(json).unwrap();
        assert_eq!(titles, vec!["Zeta".to_string()]);
        assert!(cont.is_none());
    }

    #[test]
    fn parse_wikitext_response_extracts_star() {
        let json = r#"{"parse":{"title":"X","pageid":1,"wikitext":{"*":"== H ==\nbody"}}}"#;
        assert_eq!(parse_wikitext_response(json).unwrap(), "== H ==\nbody");
    }

    #[tokio::test]
    async fn detect_scores_mediawiki_high() {
        let a = MediaWikiAdapter::new();
        let hit = ProbeContext {
            seed_url: "https://x/".into(),
            root_html: r#"<meta name="generator" content="MediaWiki 1.37.1"/>"#.into(),
        };
        let miss = ProbeContext {
            seed_url: "https://x/".into(),
            root_html: "<html></html>".into(),
        };
        assert!(a.detect(&hit).await >= 0.9);
        assert_eq!(a.detect(&miss).await, 0.0);
        assert_eq!(a.id(), "mediawiki");
    }
}
