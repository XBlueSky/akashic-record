//! E-part AI-surface text generation (llms.txt / llms-full.txt / skill.md).
//!
//! Pure functions over the stored corpus types — no I/O. The HTTP handlers
//! (`akashic-http::api::routes::llms`) and MCP tools
//! (`akashic-mcp::mcp::tools::docs_read`) are thin shells over these, so the
//! exact output formats are locked HERE by unit tests. The platform-wide
//! stamp literal is `documents {repo} {version} @ {sha7}` (E3).

use crate::algos::corpus_contract::resolve_nav_path;
use crate::types::corpus::NavTree;

/// The platform-wide version stamp (E3): `documents {repo} {version} @ {sha7}`.
///
/// `sha` is publisher-supplied and only typed as `String` by the manifest
/// schema — nothing upstream constrains it to ASCII hex — so the 7-character
/// truncation counts CHARACTERS, not bytes. Byte-slicing would panic on a
/// multi-byte codepoint straddling offset 7, and this stamp is on every
/// llms/skill/MCP response path.
#[must_use]
pub fn stamp(repo: &str, version: &str, sha: &str) -> String {
    let sha7: String = sha.chars().take(7).collect();
    format!("documents {repo} {version} @ {sha7}")
}

/// Percent-encodes a corpus path (or repo name) for use inside a URL path,
/// preserving `/` as the segment separator.
///
/// Corpus paths are only checked for absolute/`..` shapes at ingest — spaces,
/// `#`, `?`, `%` and non-ASCII are all legal — and every URL emitted here
/// lands inside a markdown link destination, where an unencoded space ends
/// the destination and `#`/`?` start a fragment/query. Encodes everything
/// outside RFC 3986 `unreserved`, byte-wise (so UTF-8 is handled correctly).
fn encode_path(path: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(char::from(b));
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// True iff `path` lies in a zh-TW mirror subtree (any path segment exactly
/// `zh-TW` — the A-spec mirror convention). zh-TW pages are for human
/// rendering and stay out of `llms-full.txt` (E2).
#[must_use]
pub fn is_zh_tw_path(path: &str) -> bool {
    path.split('/').any(|seg| seg == "zh-TW")
}

/// Title of the nav entry for the full corpus key `path`, if any.
///
/// `index_path` is `manifest.index`: nav paths are stored relative to the
/// index's own directory, so they must be resolved before comparing against a
/// full corpus key (see [`resolve_nav_path`]).
#[must_use]
pub fn nav_page_title(nav: &NavTree, path: &str, index_path: &str) -> Option<String> {
    nav.groups
        .iter()
        .flat_map(|g| g.pages.iter())
        .find(|p| resolve_nav_path(index_path, &p.path) == path)
        .map(|p| p.title.clone())
}

/// Filename stem fallback title: last path segment, `.md` stripped.
#[must_use]
pub fn path_stem(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
        .to_string()
}

/// `llms-full.txt` page order (E2): nav order first, then every remaining
/// markdown path (index + orphans) sorted lexicographically; zh-TW mirror
/// pages excluded throughout. Nav entries that don't exist in `md_paths` are
/// skipped; duplicates are emitted once.
///
/// `md_paths` are full corpus keys, so nav paths are resolved against
/// `index_path` (`manifest.index`) before matching — without that, a corpus
/// whose index isn't at the artifact root matches nothing and loses its
/// entire nav ordering to the lexicographic fallback below.
#[must_use]
pub fn order_pages(nav: &NavTree, md_paths: &[String], index_path: &str) -> Vec<String> {
    let known: std::collections::HashSet<&str> = md_paths.iter().map(String::as_str).collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(md_paths.len());

    for page in nav.groups.iter().flat_map(|g| g.pages.iter()) {
        let resolved = resolve_nav_path(index_path, &page.path);
        if known.contains(resolved.as_str())
            && !is_zh_tw_path(&resolved)
            && !seen.contains(&resolved)
        {
            seen.insert(resolved.clone());
            out.push(resolved);
        }
    }

    let mut rest: Vec<&str> = md_paths
        .iter()
        .map(String::as_str)
        .filter(|p| !seen.contains(*p) && !is_zh_tw_path(p))
        .collect();
    rest.sort_unstable();
    out.extend(rest.into_iter().map(str::to_string));
    out
}

/// Per-repo `llms.txt` (E1, llmstxt.org convention): H1 repo name, lede
/// blockquote, stamp comment, one `##` per nav group with links to LATEST
/// raw-md URLs (stable; exact version travels in the stamp), and an
/// `## Optional` section pointing at `llms-full.txt`.
#[must_use]
pub fn render_llms_txt(
    repo: &str,
    version: &str,
    sha: &str,
    nav: &NavTree,
    index_path: &str,
    base_url: &str,
) -> String {
    use std::fmt::Write as _;
    let base = base_url.trim_end_matches('/');
    let repo_url = encode_path(repo);
    let mut out = format!("# {repo}\n");
    if !nav.description.is_empty() {
        let _ = writeln!(out, "> {}", nav.description);
    }
    let _ = writeln!(out, "<!-- {} -->", stamp(repo, version, sha));
    for g in &nav.groups {
        let _ = writeln!(out, "\n## {}", g.title);
        for p in &g.pages {
            let _ = writeln!(
                out,
                "- [{}]({base}/api/v1/docs/{repo_url}/latest/raw/{}): {}",
                p.title,
                encode_path(&resolve_nav_path(index_path, &p.path)),
                p.description
            );
        }
    }
    let _ = writeln!(
        out,
        "\n## Optional\n- [llms-full.txt]({base}/docs/{repo_url}/llms-full.txt): full EN corpus, one fetch"
    );
    out
}

/// Global `/llms.txt` (E1): the single-URL entry point listing every
/// published repo → its per-repo `llms.txt`. `repos` is `(name, description)`.
#[must_use]
pub fn render_global_llms_txt(repos: &[(String, String)], base_url: &str) -> String {
    use std::fmt::Write as _;
    let base = base_url.trim_end_matches('/');
    let mut out = String::from(
        "# Akashic Record — documentation index\n\
         > Docs corpora published to this Akashic Record instance. \
         Fetch a repo's llms.txt for its page index.\n\n## Repos\n",
    );
    for (repo, description) in repos {
        let repo_url = encode_path(repo);
        if description.is_empty() {
            let _ = writeln!(out, "- [{repo}]({base}/docs/{repo_url}/llms.txt)");
        } else {
            let _ = writeln!(
                out,
                "- [{repo}]({base}/docs/{repo_url}/llms.txt): {description}"
            );
        }
    }
    out
}

/// One page of `llms-full.txt` input: full corpus key (as emitted by
/// [`order_pages`], i.e. already resolved against `manifest.index`) + raw
/// markdown.
#[derive(Debug, Clone)]
pub struct LlmsPage {
    pub path: String,
    pub markdown: String,
}

/// `llms-full.txt` (E2): stamp comment header, then every page as a YAML
/// frontmatter block (`url:` = that page's latest raw URL) followed by the
/// raw markdown. The shape deliberately mirrors what
/// `VitepressLlmsAdapter::parse_llms_full` consumes — akashic can re-ingest
/// its own output (E8 round-trip dogfood). Page bodies are emitted
/// `trim_end()`ed + one trailing newline so the parser's trailing-blank
/// trimming round-trips byte-stable.
#[must_use]
pub fn render_llms_full(
    repo: &str,
    version: &str,
    sha: &str,
    pages: &[LlmsPage],
    base_url: &str,
) -> String {
    use std::fmt::Write as _;
    let base = base_url.trim_end_matches('/');
    let repo_url = encode_path(repo);
    let mut out = format!("<!-- {} -->\n", stamp(repo, version, sha));
    for p in pages {
        let _ = write!(
            out,
            "\n---\nurl: {base}/api/v1/docs/{repo_url}/latest/raw/{}\n---\n\n{}\n",
            encode_path(&p.path),
            p.markdown.trim_end()
        );
    }
    out
}

/// Skill name for a repo's generated SKILL.md: lowercase, non-alphanumerics
/// collapsed to single `-`, capped so `{name}-docs` stays ≤ 64 chars
/// (Claude Code skill frontmatter convention).
#[must_use]
pub fn skill_name(repo: &str) -> String {
    let mut base = String::with_capacity(repo.len());
    let mut prev_dash = true; // suppress leading '-'
    for c in repo.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            base.push('-');
            prev_dash = true;
        }
    }
    let mut base = base.trim_end_matches('-').to_string();
    let max_base = 64 - "-docs".len();
    if base.len() > max_base {
        let mut cut = max_base;
        while !base.is_char_boundary(cut) {
            cut -= 1;
        }
        base.truncate(cut);
    }
    while base.ends_with('-') {
        base.pop();
    }
    format!("{base}-docs")
}

/// When-to-use description for the skill frontmatter: repo positioning from
/// the index lede plus nav group/page titles as natural trigger keywords.
/// Deliberately uncapped here — the frontmatter's 1024-char cap has to apply
/// to the *escaped* value (`render_skill_md` escapes afterwards, and
/// escaping only grows the string), so the cap lives there instead.
fn skill_description(repo: &str, nav: &NavTree) -> String {
    let mut topics: Vec<&str> = Vec::new();
    for g in &nav.groups {
        topics.push(g.title.as_str());
        for p in &g.pages {
            topics.push(p.title.as_str());
        }
    }
    format!(
        "Use when working with the {repo} repository or its APIs. {} Topics: {}.",
        nav.description,
        topics.join(", ")
    )
}

/// Caps an already-escaped frontmatter description to ≤ 1024 chars
/// (frontmatter convention). Truncates then trims back further if that cut
/// lands mid-escape-sequence (a dangling trailing `\` would otherwise
/// swallow the closing quote), so the result never ends on an odd run of
/// consecutive `\` characters.
fn cap_escaped(escaped: String) -> String {
    if escaped.chars().count() <= 1024 {
        return escaped;
    }
    let mut truncated: String = escaped.chars().take(1021).collect();
    while truncated.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1 {
        truncated.pop();
    }
    truncated.push_str("...");
    truncated
}

/// First page whose path/title mentions "getting started", else the first
/// page across the whole nav tree — the skill's "quick start" entry link.
fn quickstart_page(nav: &NavTree) -> Option<&crate::types::corpus::NavPage> {
    nav.groups
        .iter()
        .flat_map(|g| g.pages.iter())
        .find(|p| {
            p.path.contains("getting-started") || p.title.to_lowercase().contains("getting started")
        })
        .or_else(|| nav.groups.iter().flat_map(|g| g.pages.iter()).next())
}

/// `skill.md` (E4): deterministic v1 assembly (no LLM). Frontmatter
/// `name`/`description`, then: positioning line → quick start link →
/// all-pages index (nav) → `search_knowledge` deep-dive pointer → stamp.
/// The description is double-quoted with `"`/`\` escaped so nav text can't
/// break the YAML.
#[must_use]
pub fn render_skill_md(
    repo: &str,
    version: &str,
    sha: &str,
    nav: &NavTree,
    index_path: &str,
    base_url: &str,
) -> String {
    use std::fmt::Write as _;
    let base = base_url.trim_end_matches('/');
    let repo_url = encode_path(repo);
    let desc = cap_escaped(
        skill_description(repo, nav)
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " "),
    );
    let mut out = format!(
        "---\nname: {}\ndescription: \"{desc}\"\n---\n\n# {repo} docs\n\n{}\n",
        skill_name(repo),
        nav.description
    );
    if let Some(qs) = quickstart_page(nav) {
        let _ = writeln!(
            out,
            "\nQuick start: [{}]({base}/api/v1/docs/{repo_url}/latest/raw/{})",
            qs.title,
            encode_path(&resolve_nav_path(index_path, &qs.path))
        );
    }
    out.push_str("\n## All pages\n");
    for g in &nav.groups {
        let _ = writeln!(out, "\n### {}", g.title);
        for p in &g.pages {
            let _ = writeln!(
                out,
                "- [{}]({base}/api/v1/docs/{repo_url}/latest/raw/{}): {}",
                p.title,
                encode_path(&resolve_nav_path(index_path, &p.path)),
                p.description
            );
        }
    }
    let _ = writeln!(
        out,
        "\nFor deeper or cross-page questions, use the akashic MCP server's \
         `search_knowledge` tool with `repo=\"{repo}\"`, `prefer_space=\"doc\"`."
    );
    let _ = writeln!(out, "\n<!-- {} -->", stamp(repo, version, sha));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::corpus::{NavGroup, NavPage};

    fn nav() -> NavTree {
        NavTree {
            description: "Test corpus.".into(),
            groups: vec![NavGroup {
                title: "Guide".into(),
                pages: vec![
                    NavPage {
                        title: "Setup".into(),
                        path: "guide/setup.md".into(),
                        description: "Install.".into(),
                    },
                    NavPage {
                        title: "Usage".into(),
                        path: "guide/usage.md".into(),
                        description: "Use it.".into(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn stamp_truncates_sha_to_seven() {
        assert_eq!(
            stamp("acme", "1.0.0", "0123456789abcdef"),
            "documents acme 1.0.0 @ 0123456"
        );
        // shorter-than-7 sha must not panic
        assert_eq!(stamp("r", "1", "abc"), "documents r 1 @ abc");
    }

    /// `manifest.sha` is only typed as `String` — nothing upstream enforces
    /// ASCII hex — so truncation must count characters. Byte-slicing at 7
    /// would land inside `é`'s UTF-8 encoding here and panic.
    #[test]
    fn stamp_truncates_multibyte_sha_on_char_boundary() {
        assert_eq!(stamp("r", "1", "abcdefé"), "documents r 1 @ abcdefé");
        assert_eq!(stamp("r", "1", "ééééééééé"), "documents r 1 @ ééééééé");
    }

    #[test]
    fn encode_path_preserves_separators_and_escapes_url_syntax() {
        assert_eq!(encode_path("guide/setup.md"), "guide/setup.md");
        assert_eq!(encode_path("guide/a-b_c.d~e"), "guide/a-b_c.d~e");
        assert_eq!(
            encode_path("guide/intro to API.md"),
            "guide/intro%20to%20API.md"
        );
        assert_eq!(encode_path("guide/C++ #1.md"), "guide/C%2B%2B%20%231.md");
        assert_eq!(encode_path("guide/q?x.md"), "guide/q%3Fx.md");
        // multi-byte UTF-8 is encoded byte-wise
        assert_eq!(encode_path("設定.md"), "%E8%A8%AD%E5%AE%9A.md");
    }

    #[test]
    fn is_zh_tw_path_matches_segment_only() {
        assert!(is_zh_tw_path("zh-TW/guide/setup.md"));
        assert!(is_zh_tw_path("docs/zh-TW/setup.md"));
        assert!(!is_zh_tw_path("guide/zh-TWfoo/setup.md"));
        assert!(!is_zh_tw_path("guide/setup.md"));
    }

    #[test]
    fn nav_page_title_finds_exact_path() {
        assert_eq!(
            nav_page_title(&nav(), "guide/setup.md", "index.md"),
            Some("Setup".into())
        );
        assert_eq!(nav_page_title(&nav(), "guide/missing.md", "index.md"), None);
    }

    /// Nested index (`docs/README.md`, the pull-bootstrap shape): nav paths are
    /// index-relative, the lookup key is a full corpus key.
    #[test]
    fn nav_page_title_resolves_nav_against_nested_index() {
        assert_eq!(
            nav_page_title(&nav(), "docs/guide/setup.md", "docs/README.md"),
            Some("Setup".into())
        );
        assert_eq!(
            nav_page_title(&nav(), "guide/setup.md", "docs/README.md"),
            None
        );
    }

    #[test]
    fn path_stem_strips_dirs_and_md() {
        assert_eq!(path_stem("guide/setup.md"), "setup");
        assert_eq!(path_stem("index.md"), "index");
    }

    #[test]
    fn order_pages_nav_first_then_sorted_rest_excluding_zh_tw() {
        let md_paths: Vec<String> = [
            "index.md",
            "guide/usage.md",
            "guide/setup.md",
            "guide/orphan-b.md",
            "guide/orphan-a.md",
            "zh-TW/guide/setup.md",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        assert_eq!(
            order_pages(&nav(), &md_paths, "index.md"),
            vec![
                "guide/setup.md".to_string(), // nav order, not path order
                "guide/usage.md".to_string(),
                "guide/orphan-a.md".to_string(), // rest sorted
                "guide/orphan-b.md".to_string(),
                "index.md".to_string(),
            ]
        );
    }

    #[test]
    fn order_pages_skips_nav_entries_absent_from_corpus_and_dedups() {
        let md_paths = vec!["guide/setup.md".to_string()];
        // usage.md is in nav but not in the corpus paths — must not appear
        assert_eq!(
            order_pages(&nav(), &md_paths, "index.md"),
            vec!["guide/setup.md".to_string()]
        );
    }

    /// Pull-bootstrapped corpus: `manifest.index = docs/README.md`, file keys
    /// carry the `docs_root/` prefix, nav paths stay index-relative. Nav
    /// ORDER must survive — before nav paths were resolved, nothing matched
    /// and every page fell through to the lexicographic tail.
    #[test]
    fn order_pages_resolves_nav_against_nested_index() {
        let md_paths: Vec<String> = [
            "docs/README.md",
            "docs/guide/usage.md",
            "docs/guide/setup.md",
            "docs/guide/orphan.md",
            "docs/zh-TW/guide/setup.md",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        assert_eq!(
            order_pages(&nav(), &md_paths, "docs/README.md"),
            vec![
                "docs/guide/setup.md".to_string(), // nav order preserved
                "docs/guide/usage.md".to_string(),
                "docs/README.md".to_string(), // rest sorted
                "docs/guide/orphan.md".to_string(),
            ]
        );
    }

    #[test]
    fn render_llms_txt_matches_e1_shape() {
        let out = render_llms_txt(
            "acme",
            "1.0.0",
            "0123456789abcdef",
            &nav(),
            "index.md",
            "https://akashic.example.com/",
        );
        let expected = "\
# acme
> Test corpus.
<!-- documents acme 1.0.0 @ 0123456 -->

## Guide
- [Setup](https://akashic.example.com/api/v1/docs/acme/latest/raw/guide/setup.md): Install.
- [Usage](https://akashic.example.com/api/v1/docs/acme/latest/raw/guide/usage.md): Use it.

## Optional
- [llms-full.txt](https://akashic.example.com/docs/acme/llms-full.txt): full EN corpus, one fetch
";
        assert_eq!(out, expected);
    }

    #[test]
    fn render_llms_txt_omits_blockquote_when_description_empty() {
        let mut n = nav();
        n.description = String::new();
        let out = render_llms_txt("r", "1", "abcdefgh", &n, "index.md", "http://x");
        assert!(out.starts_with("# r\n<!-- documents r 1 @ abcdefg -->\n"));
        assert!(!out.contains("> "));
    }

    /// The raw URLs must address the FULL corpus key, not the index-relative
    /// nav path — otherwise every generated link 404s for a corpus whose index
    /// isn't at the artifact root.
    #[test]
    fn render_llms_txt_nested_index_emits_full_corpus_keys() {
        let out = render_llms_txt(
            "acme",
            "1.0.0",
            "0123456789abcdef",
            &nav(),
            "docs/README.md",
            "https://akashic.example.com",
        );
        assert!(out.contains(
            "- [Setup](https://akashic.example.com/api/v1/docs/acme/latest/raw/docs/guide/setup.md): Install.\n"
        ));
        assert!(!out.contains("/raw/guide/setup.md"));
    }

    /// Corpus paths only get absolute/`..` checks at ingest, so a legal path
    /// can carry URL-significant characters; unencoded, the space would end the
    /// markdown link destination and `#` would start a fragment.
    #[test]
    fn render_llms_txt_percent_encodes_path_and_repo() {
        let n = NavTree {
            description: String::new(),
            groups: vec![NavGroup {
                title: "Guide".into(),
                pages: vec![NavPage {
                    title: "Intro".into(),
                    path: "guide/intro to C++ #1.md".into(),
                    description: "Start.".into(),
                }],
            }],
        };
        let out = render_llms_txt("my repo", "1", "abcdefgh", &n, "index.md", "http://x");
        assert!(out.contains(
            "- [Intro](http://x/api/v1/docs/my%20repo/latest/raw/guide/intro%20to%20C%2B%2B%20%231.md): Start.\n"
        ));
        // the H1 keeps the human-readable repo name
        assert!(out.starts_with("# my repo\n"));
    }

    #[test]
    fn render_global_llms_txt_lists_each_repo() {
        let repos = vec![
            ("acme".to_string(), "Codegen toolkit.".to_string()),
            ("kaer-morhen".to_string(), String::new()),
        ];
        let out = render_global_llms_txt(&repos, "https://akashic.example.com");
        assert!(out.starts_with("# Akashic Record — documentation index\n"));
        assert!(out.contains(
            "- [acme](https://akashic.example.com/docs/acme/llms.txt): Codegen toolkit.\n"
        ));
        // empty description → no trailing ": "
        assert!(
            out.contains(
                "- [kaer-morhen](https://akashic.example.com/docs/kaer-morhen/llms.txt)\n"
            )
        );
    }

    #[test]
    fn render_llms_full_stamp_header_then_frontmatter_pages() {
        let pages = vec![
            LlmsPage {
                path: "guide/setup.md".into(),
                markdown: "# Setup\n\nInstall steps.\n".into(),
            },
            LlmsPage {
                path: "index.md".into(),
                markdown: "# Index\n\nWelcome.".into(),
            },
        ];
        let out = render_llms_full(
            "acme",
            "1.0.0",
            "0123456789abcdef",
            &pages,
            "http://a.example",
        );
        let expected = "\
<!-- documents acme 1.0.0 @ 0123456 -->

---
url: http://a.example/api/v1/docs/acme/latest/raw/guide/setup.md
---

# Setup

Install steps.

---
url: http://a.example/api/v1/docs/acme/latest/raw/index.md
---

# Index

Welcome.
";
        assert_eq!(out, expected);
    }

    #[test]
    fn render_llms_full_empty_pages_is_just_stamp() {
        let out = render_llms_full("r", "1", "abcdefgh", &[], "http://x");
        assert_eq!(out, "<!-- documents r 1 @ abcdefg -->\n");
    }

    #[test]
    fn skill_name_sanitizes_and_caps() {
        assert_eq!(skill_name("acme"), "acme-docs");
        assert_eq!(skill_name("ds.base/juniper"), "ds-base-juniper-docs");
        assert_eq!(skill_name("A__Weird..Repo"), "a-weird-repo-docs");
        let long = skill_name(&"x".repeat(200));
        assert!(long.len() <= 64);
        assert!(long.ends_with("-docs"));
    }

    #[test]
    fn render_skill_md_frontmatter_and_sections() {
        let out = render_skill_md(
            "acme",
            "1.0.0",
            "0123456789abcdef",
            &nav(),
            "index.md",
            "https://akashic.example.com",
        );
        // frontmatter block
        assert!(out.starts_with("---\nname: acme-docs\ndescription: \""));
        let close = out.find("\n---\n\n").expect("frontmatter closes");
        let fm = &out[..close];
        let desc_line = fm.lines().find(|l| l.starts_with("description: ")).unwrap();
        assert!(desc_line.len() <= "description: ".len() + 1026); // 1024 + 2 quotes
        assert!(desc_line.contains("acme"));
        assert!(desc_line.contains("Setup")); // nav topics feed the trigger text
        // body sections
        assert!(out.contains("Quick start: [Setup](https://akashic.example.com/api/v1/docs/acme/latest/raw/guide/setup.md)"));
        assert!(out.contains("\n## All pages\n"));
        assert!(out.contains("### Guide\n"));
        assert!(out.contains("- [Usage](https://akashic.example.com/api/v1/docs/acme/latest/raw/guide/usage.md): Use it."));
        assert!(out.contains("`search_knowledge`"));
        assert!(out.contains("repo=\"acme\""));
        assert!(out.contains("prefer_space=\"doc\""));
        assert!(
            out.trim_end()
                .ends_with("<!-- documents acme 1.0.0 @ 0123456 -->")
        );
    }

    #[test]
    fn render_skill_md_escapes_quotes_in_description() {
        let mut n = nav();
        n.description = "Has \"quotes\" and: colons.".into();
        let out = render_skill_md("r", "1", "abcdefgh", &n, "index.md", "http://x");
        assert!(out.contains(r#"Has \"quotes\" and: colons."#));
    }

    #[test]
    fn render_skill_md_description_cap_applies_post_escape() {
        // 700 quotes escape to 1400 chars; the raw (pre-escape) description
        // is well under 1024, so a pre-escape cap would let this through.
        let mut n = nav();
        n.description = "\"".repeat(700);
        let out = render_skill_md("r", "1", "abcdefgh", &n, "index.md", "http://x");
        let close = out.find("\n---\n\n").expect("frontmatter closes");
        let fm = &out[..close];
        let desc_line = fm.lines().find(|l| l.starts_with("description: ")).unwrap();
        assert!(desc_line.starts_with("description: \"") && desc_line.ends_with('"'));
        let value = &desc_line["description: \"".len()..desc_line.len() - 1];
        assert!(
            value.chars().count() <= 1024,
            "escaped description value must be capped at 1024 chars, got {}",
            value.chars().count()
        );
        let trailing_backslashes = value.chars().rev().take_while(|&c| c == '\\').count();
        assert_eq!(
            trailing_backslashes % 2,
            0,
            "must not end on a dangling escape that would swallow the closing quote"
        );
    }

    #[test]
    fn render_skill_md_quickstart_falls_back_across_whole_nav_when_first_group_empty() {
        let n = NavTree {
            description: "Test corpus.".into(),
            groups: vec![
                NavGroup {
                    title: "Empty".into(),
                    pages: vec![],
                },
                NavGroup {
                    title: "Guide".into(),
                    pages: vec![NavPage {
                        title: "Setup".into(),
                        path: "guide/setup.md".into(),
                        description: "Install.".into(),
                    }],
                },
            ],
        };
        let out = render_skill_md("r", "1", "abcdefgh", &n, "index.md", "http://x");
        assert!(
            out.contains("Quick start: [Setup](http://x/api/v1/docs/r/latest/raw/guide/setup.md)")
        );
    }
}
