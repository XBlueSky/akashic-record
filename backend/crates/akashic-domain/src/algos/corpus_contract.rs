//! docs-kit corpus contract-validation algorithms — pure, DB-free (A1/B2).
//!
//! These four functions are the shared implementation behind two downstream
//! consumers: the CI packer (a separate TypeScript repo, aligned via the
//! golden fixtures under `tests/fixtures/corpus/`) and the platform ingest
//! path (Task 5/7), which calls them directly. Keep behavior and the
//! `ContractFinding` wire shape stable — both consumers key off them.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use regex::Regex;

use crate::types::corpus::{ContractFinding, NavGroup, NavPage, NavTree, Severity};

static VERSION_DEFINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#define\s+\w+_VERSION_(MAJOR|MINOR|PATCH)\s+(\d+)").unwrap());

/// Parse `#define FOO_VERSION_{MAJOR,MINOR,PATCH} N` lines out of a C/C++
/// version header. Returns `None` unless all three components are present.
pub fn parse_version_header(text: &str) -> Option<(u32, u32, u32)> {
    let mut major = None;
    let mut minor = None;
    let mut patch = None;
    for caps in VERSION_DEFINE_RE.captures_iter(text) {
        let value: u32 = caps[2].parse().ok()?;
        match &caps[1] {
            "MAJOR" => major = Some(value),
            "MINOR" => minor = Some(value),
            "PATCH" => patch = Some(value),
            _ => unreachable!("regex only captures MAJOR|MINOR|PATCH"),
        }
    }
    Some((major?, minor?, patch?))
}

/// The punctuation set stripped from a heading before slugging. Matches the
/// `github-slugger` npm package (the frontend imports the same package as
/// `markdown-it-anchor`'s slugify option): `_` is a word character and is
/// kept, and so are letters/digits from ANY script (Unicode `Alphabetic` +
/// `Numeric`, not just ASCII — e.g. CJK Han, accented Latin). Everything
/// else is punctuation/symbol and is stripped, ASCII (`!"#$%&'()*+,./:;<=>?@
/// [\]^\`{|}~`) or not — real `github-slugger` strips the em dash (U+2014),
/// curly quotes, CJK full-width punctuation, etc. just as readily as ASCII
/// punctuation (Task 11 E2E finding: an ASCII-only strip let the em dash in
/// `## Call another operation — \`exec\` (local)` survive, producing an
/// anchor real acme doc authors' own cross-references didn't expect —
/// see `github_slug_strips_non_ascii_punctuation_like_em_dash`). Hyphens and
/// spaces are never in this set — they're handled by step (c), and existing
/// hyphens are never collapsed.
fn is_slug_stripped_punctuation(c: char) -> bool {
    c != ' ' && c != '-' && c != '_' && !c.is_alphanumeric()
}

/// github-slugger-canonical heading slug (single occurrence — no dedup
/// suffix). See [`SlugCounter`] for the duplicate-suffixing wrapper used
/// when slugging every heading of one document.
pub fn github_slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter(|c| !is_slug_stripped_punctuation(*c))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// Assigns github-slugger-compatible duplicate suffixes (`-1`, `-2`, ...) to
/// repeated headings within one document, first occurrence bare.
#[derive(Debug, Default)]
pub struct SlugCounter {
    seen: HashMap<String, u32>,
}

impl SlugCounter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn slug(&mut self, heading: &str) -> String {
        let base = github_slug(heading);
        let occurrence = self.seen.entry(base.clone()).or_insert(0);
        let result = if *occurrence == 0 {
            base
        } else {
            format!("{base}-{occurrence}")
        };
        *occurrence += 1;
        result
    }
}

const ALL_PAGES_HEADING: &str = "## All pages";

static NAV_PAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-\s*\[([^\]]+)\]\(([^)]+)\)(?:\s*(?:—|-)\s*(.*))?$").unwrap());

/// Byte offset of the start of the first line whose trimmed content exactly
/// equals `target`, or `None` if no such line exists.
fn find_line_start(text: &str, target: &str) -> Option<usize> {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if line.trim_end_matches('\n').trim() == target {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// The index page's lede: the first non-empty prose paragraph in `pre_text`
/// (everything before `## All pages`), excluding the H1 title line and any
/// `**Language:**` metadata line, with a leading `> ` blockquote marker
/// stripped if present. Empty string if no such paragraph exists.
fn extract_lede_description(pre_text: &str) -> String {
    let mut para_lines: Vec<&str> = Vec::new();
    let mut collecting = false;
    for line in pre_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if collecting {
                break;
            }
            continue;
        }
        if trimmed.starts_with("# ") || trimmed.starts_with("**Language:**") {
            continue;
        }
        collecting = true;
        para_lines.push(trimmed);
    }
    if para_lines.is_empty() {
        return String::new();
    }
    let joined = para_lines.join(" ");
    joined.strip_prefix("> ").unwrap_or(&joined).to_string()
}

/// Parse the `## All pages` section of a corpus index page into a
/// [`NavTree`], including the lede-paragraph `description`.
pub fn parse_nav(index_md: &str) -> Result<NavTree, ContractFinding> {
    let Some(marker_start) = find_line_start(index_md, ALL_PAGES_HEADING) else {
        return Err(ContractFinding {
            rule: "nav_missing".to_string(),
            file: "index.md".to_string(),
            line: None,
            detail: format!("no \"{ALL_PAGES_HEADING}\" section found in index"),
            severity: Severity::Error,
        });
    };

    let description = extract_lede_description(&index_md[..marker_start]);

    let mut section_lines = index_md[marker_start..].lines();
    section_lines.next(); // consume the "## All pages" heading line itself

    let mut groups: Vec<NavGroup> = Vec::new();
    let mut current: Option<NavGroup> = None;

    for line in section_lines {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            break; // the next top-level section ends "## All pages"
        }
        if let Some(title) = trimmed.strip_prefix("### ") {
            if let Some(group) = current.take() {
                groups.push(group);
            }
            current = Some(NavGroup {
                title: title.trim().to_string(),
                pages: Vec::new(),
            });
            continue;
        }
        if let Some(caps) = NAV_PAGE_RE.captures(trimmed) {
            if let Some(group) = current.as_mut() {
                group.pages.push(NavPage {
                    title: caps[1].to_string(),
                    path: caps[2].to_string(),
                    description: caps
                        .get(3)
                        .map_or_else(String::new, |m| m.as_str().trim().to_string()),
                });
            }
            // A page line outside any group is malformed nav shape — skip.
            continue;
        }
        // Stray prose or malformed list syntax — skip silently.
    }
    if let Some(group) = current.take() {
        groups.push(group);
    }

    Ok(NavTree {
        description,
        groups,
    })
}

static MD_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]]*\]\(([^)]+)\)").unwrap());
static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^#{1,6}\s+(.+?)\s*$").unwrap());
static INLINE_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`\n]+`").unwrap());

/// Byte ranges of `content` a real markdown renderer would NOT treat as
/// prose — fenced code blocks (```` ``` ```` / `~~~`) and inline `` `code` ``
/// spans — so the link scanner can skip `[...](...)`-shaped text that's
/// actually code (a C++ lambda capture `[](args)`, an example inside a
/// fence, etc.) rather than a real markdown link. Task 11 E2E finding: real
/// acme docs (`docs/guides/handler-types.md` and others) have MANY
/// `[](const Req& req, ...) -> ...` lambda captures inside ```cpp fences —
/// without this exclusion those get misdetected as broken links wholesale.
///
/// Fence detection is CommonMark-ish, not exhaustive: an opening fence line
/// (≤3 leading spaces, ≥3 of the same fence char, nothing else but the
/// fence char on the line) is closed by the next such line using the same
/// char with length ≥ the opener's. An unterminated fence treats the rest
/// of the file as code (matches how a real renderer degrades). Inline-code
/// ranges are computed over the whole (unmasked) content — a span that
/// happens to overlap a fenced range is redundant coverage, not a bug.
fn masked_code_ranges(content: &str) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    let mut fence_open: Option<(usize, char, usize)> = None; // (start_byte, fence_char, min_len)
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let marker = trimmed.trim_end_matches('\n').trim_end();
        if indent <= 3 && !marker.is_empty() {
            let fence_char = marker
                .chars()
                .next()
                .expect("non-empty implies a first char");
            if fence_char == '`' || fence_char == '~' {
                // `run_len` is a count of single-byte ASCII chars, so it's
                // also a valid byte index into `marker` for the slice below.
                let run_len = marker.chars().take_while(|&c| c == fence_char).count();
                if run_len >= 3 {
                    let rest_after_run = &marker[run_len..];
                    match fence_open {
                        // Opening fence: an info string after the run (e.g.
                        // "```cpp") is valid CommonMark and doesn't disqualify it.
                        None => fence_open = Some((offset, fence_char, run_len)),
                        Some((start, open_char, open_len))
                            if fence_char == open_char
                                && run_len >= open_len
                                && rest_after_run.trim().is_empty() =>
                        {
                            // Closing fence: same char, length >= opener's,
                            // nothing but (optional trailing whitespace) on
                            // the line — no info string allowed on a closer.
                            ranges.push((start, offset + line.len()));
                            fence_open = None;
                        }
                        Some(_) => {} // doesn't close the open fence — just content
                    }
                }
            }
        }
        offset += line.len();
    }
    if let Some((start, ..)) = fence_open {
        ranges.push((start, content.len())); // unterminated fence: rest of file is code
    }

    for m in INLINE_CODE_RE.find_iter(content) {
        ranges.push((m.start(), m.end()));
    }

    ranges.sort_unstable();
    ranges
}

/// Whether byte offset `pos` falls inside any of `ranges` (as produced by
/// [`masked_code_ranges`]).
fn in_masked_range(ranges: &[(usize, usize)], pos: usize) -> bool {
    ranges.iter().any(|&(start, end)| pos >= start && pos < end)
}

fn is_external_link(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://") || target.starts_with("mailto:")
}

/// 1-indexed line number containing `byte_offset`.
fn line_number_at(text: &str, byte_offset: usize) -> u32 {
    u32::try_from(text[..byte_offset].matches('\n').count()).unwrap_or(u32::MAX) + 1
}

/// Directory portion of a corpus-relative file path (`""` for root files).
fn file_dir(file_path: &str) -> &str {
    file_path.rfind('/').map_or("", |idx| &file_path[..idx])
}

/// Whether `path` lives under the directory `prefix`, as whole path
/// segments — a plain [`str::starts_with`] would let `"documentation/public"`
/// wrongly match `"documentation/publicity/x.md"`; this requires the byte
/// right after `prefix` to be a `/` segment boundary (or nothing, i.e. an
/// exact match). `prefix` empty always matches (every path is "under" an
/// empty root).
fn path_has_dir_prefix(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Resolve a markdown link's path component against the directory of the
/// linking file, collapsing `.` and `..` segments — no filesystem access,
/// `files` is the corpus's own path→content map. The second return value is
/// `true` when a `..` segment was applied with nothing left to pop — i.e.
/// the reference walks OUTSIDE the resolution's own starting directory tree
/// entirely, not just to a sibling that happens not to exist. (This alone
/// doesn't catch every corpus-root escape — see `check_links`'s
/// `corpus_root_prefix` check for the pull-bootstrap case, where the corpus
/// root sits one level below the resolution root.)
fn resolve_relative_path(base_dir: &str, target_path: &str) -> (String, bool) {
    let mut segments: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    let mut underflowed = false;
    for part in target_path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    underflowed = true;
                }
            }
            seg => segments.push(seg),
        }
    }
    (segments.join("/"), underflowed)
}

/// Resolves ONE nav page path into the full corpus key that `CorpusStore`
/// keys files by.
///
/// [`parse_nav`] stores each [`NavPage::path`] exactly as written in the
/// index markdown — i.e. **relative to the index file's own directory**, not
/// to the corpus root. The two coincide only when the index sits at the
/// artifact root (`index.md`, the push default), which is why every consumer
/// that skipped this step still worked for push corpora. For a
/// pull-bootstrapped corpus (`manifest.index = "docs/README.md"`, file keys
/// carrying the `docs_root/` prefix) they do NOT: nav `guide/setup.md` is
/// stored as `docs/guide/setup.md`.
///
/// [`check_links`] does this same resolution internally before comparing nav
/// against `files`; anything OUTSIDE this module that reads nav paths and
/// then touches the store (or builds a URL) must call this first.
///
/// `index_path` is `manifest.index`.
#[must_use]
pub fn resolve_nav_path(index_path: &str, nav_path: &str) -> String {
    resolve_relative_path(file_dir(index_path), nav_path).0
}

/// [`resolve_nav_path`] applied across a whole [`NavTree`], returning a copy
/// whose every page path is a full corpus key. Titles/descriptions and group
/// order are untouched.
#[must_use]
pub fn resolve_nav_tree(nav: &NavTree, index_path: &str) -> NavTree {
    NavTree {
        description: nav.description.clone(),
        groups: nav
            .groups
            .iter()
            .map(|g| NavGroup {
                title: g.title.clone(),
                pages: g
                    .pages
                    .iter()
                    .map(|p| NavPage {
                        title: p.title.clone(),
                        path: resolve_nav_path(index_path, &p.path),
                        description: p.description.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// All github-slugger slugs (with duplicate-suffixing) of a markdown
/// document's headings, in heading order.
fn heading_slugs(content: &str) -> std::collections::HashSet<String> {
    let mut counter = SlugCounter::new();
    HEADING_RE
        .captures_iter(content)
        .map(|caps| counter.slug(caps[1].trim()))
        .collect()
}

/// Validate relative links and anchors across a corpus's markdown files, and
/// flag pages unreachable from `nav`.
///
/// `index_path` is the corpus-relative path of the index page itself
/// (`manifest.index` — `"index.md"` for an untarred push artifact where the
/// docs root IS the artifact root, or a docs_root-prefixed key like
/// `"docs/README.md"` for a pull-bootstrapped corpus). It is used for two
/// things a hardcoded `"index.md"` got wrong once `manifest.index` could
/// carry a prefix: (1) nav `page.path` entries are written relative to the
/// index file, exactly like any other markdown link, so they must be
/// resolved against the index's own directory — via the same
/// `resolve_relative_path` body links use — into full corpus keys BEFORE
/// being compared against `files`' own full-key paths; comparing the literal
/// nav text against a prefixed key would falsely orphan every page. (2) the
/// index page itself is exempt from orphan detection (nothing in the nav
/// links back to itself) — that exemption must check the real index path,
/// not an assumed root-level `"index.md"`.
pub fn check_links(
    files: &BTreeMap<String, String>,
    nav: &NavTree,
    index_path: &str,
) -> Vec<ContractFinding> {
    let mut findings = Vec::new();

    let index_dir = file_dir(index_path);
    let reachable: std::collections::HashSet<String> = nav
        .groups
        .iter()
        .flat_map(|group| {
            group
                .pages
                .iter()
                .map(|page| resolve_relative_path(index_dir, &page.path).0)
        })
        .collect();

    // The corpus's shared root prefix, derived from the index page's own
    // FULL containing directory (not just its first path segment — review
    // fix): `""` for a push artifact (`index_path == "index.md"` — docs_root
    // IS the artifact root, so `files` has no shared prefix to escape) or
    // the index's own directory for a pull-bootstrapped corpus
    // (`index_path == "docs/README.md"` → `"docs"`, or
    // `index_path == "documentation/public/README.md"` → the full
    // `"documentation/public"` — every corpus key shares that prefix; see
    // `corpus_pull::collect_docs_files`). A first-segment-only prefix (the
    // old behavior) mis-classified a link escaping to a SIBLING of a
    // multi-segment docs_root — e.g. `documentation/other/...` when the real
    // docs_root is `documentation/public`, sharing only `"documentation"` —
    // as still in-root, turning a real escape into a spurious broken_link.
    let corpus_root_prefix: &str = index_dir;

    for (file, content) in files {
        let dir = file_dir(file);
        let masked = masked_code_ranges(content);
        for caps in MD_LINK_RE.captures_iter(content) {
            let whole_match = caps.get(0).expect("match 0 is always present");
            if in_masked_range(&masked, whole_match.start()) {
                continue; // inside a fenced block / inline code span — not a real link
            }
            let target = &caps[1];
            if is_external_link(target) {
                continue;
            }
            let (path_part, anchor) = match target.split_once('#') {
                Some((p, a)) => (p, Some(a)),
                None => (target, None),
            };
            let line = line_number_at(content, whole_match.start());

            let (resolved_path, underflowed) = if path_part.is_empty() {
                (file.clone(), false)
            } else {
                resolve_relative_path(dir, path_part)
            };

            // A relative link that resolves outside the corpus's own root
            // (e.g. `../../include/foo.hpp` from a docs page, pointing at
            // the containing repo's source tree — Task 11 E2E finding
            // against real acme docs) is an external reference from the
            // corpus's point of view, same treatment as an http(s)/mailto
            // link — not a same-corpus broken link. Resolving these against
            // the code graph instead of skipping outright is a reasonable
            // follow-up, not done here (see the Task 11 report).
            let escaped_corpus_root = if corpus_root_prefix.is_empty() {
                underflowed
            } else {
                !path_has_dir_prefix(&resolved_path, corpus_root_prefix)
            };
            if escaped_corpus_root {
                continue;
            }

            let Some(target_content) = files.get(&resolved_path) else {
                findings.push(ContractFinding {
                    rule: "broken_link".to_string(),
                    file: file.clone(),
                    line: Some(line),
                    detail: format!("link target \"{target}\" does not resolve to a known file"),
                    severity: Severity::Error,
                });
                continue;
            };

            if let Some(anchor) = anchor.filter(|a| !a.is_empty())
                && !heading_slugs(target_content).contains(anchor)
            {
                findings.push(ContractFinding {
                    rule: "broken_anchor".to_string(),
                    file: file.clone(),
                    line: Some(line),
                    detail: format!("anchor \"#{anchor}\" not found in \"{resolved_path}\""),
                    severity: Severity::Error,
                });
            }
        }
    }

    for file in files.keys() {
        if file == index_path || !file.ends_with(".md") || reachable.contains(file.as_str()) {
            continue;
        }
        findings.push(ContractFinding {
            rule: "orphan_page".to_string(),
            file: file.clone(),
            line: None,
            detail: "page is not reachable from any nav group".to_string(),
            severity: Severity::Warn,
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Root index (the push default): nav paths already ARE full corpus keys,
    /// so resolution is the identity — this is why consumers that skipped it
    /// still worked for push corpora.
    #[test]
    fn resolve_nav_path_is_identity_for_root_index() {
        assert_eq!(
            resolve_nav_path("index.md", "guide/setup.md"),
            "guide/setup.md"
        );
        assert_eq!(resolve_nav_path("README.md", "setup.md"), "setup.md");
    }

    /// Nested index (the pull-bootstrap shape): the index's own directory is
    /// prepended, matching the `docs_root/`-prefixed keys the store holds.
    #[test]
    fn resolve_nav_path_prefixes_nested_index_dir() {
        assert_eq!(
            resolve_nav_path("docs/README.md", "guide/setup.md"),
            "docs/guide/setup.md"
        );
        assert_eq!(
            resolve_nav_path("a/b/index.md", "../c/page.md"),
            "a/c/page.md"
        );
        assert_eq!(resolve_nav_path("docs/README.md", "./x.md"), "docs/x.md");
    }

    #[test]
    fn resolve_nav_tree_resolves_every_page_and_keeps_shape() {
        let nav = NavTree {
            description: "Lede.".into(),
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
                        path: "usage.md".into(),
                        description: "Use.".into(),
                    },
                ],
            }],
        };
        let resolved = resolve_nav_tree(&nav, "docs/README.md");
        assert_eq!(resolved.description, "Lede.");
        assert_eq!(resolved.groups[0].title, "Guide");
        assert_eq!(resolved.groups[0].pages[0].path, "docs/guide/setup.md");
        assert_eq!(resolved.groups[0].pages[1].path, "docs/usage.md");
        // titles/descriptions untouched
        assert_eq!(resolved.groups[0].pages[0].title, "Setup");
        assert_eq!(resolved.groups[0].pages[1].description, "Use.");
    }

    #[test]
    fn parse_version_header_acme_fixture() {
        let text = "\
#define ACME_VERSION_MAJOR 1
#define ACME_VERSION_MINOR 0
#define ACME_VERSION_PATCH 0
";
        assert_eq!(parse_version_header(text), Some((1, 0, 0)));
    }

    #[test]
    fn parse_version_header_ignores_surrounding_noise() {
        let text = "\
// Copyright Acme Corp.
#pragma once

#define ACME_VERSION_MAJOR 12
#define ACME_VERSION_MINOR 34
#define ACME_BUILD_NUMBER 999
#define ACME_VERSION_PATCH 56
";
        assert_eq!(parse_version_header(text), Some((12, 34, 56)));
    }

    #[test]
    fn parse_version_header_missing_component_is_none() {
        let text = "\
#define ACME_VERSION_MAJOR 1
#define ACME_VERSION_MINOR 2
";
        assert_eq!(parse_version_header(text), None);
    }

    #[test]
    fn parse_version_header_empty_text_is_none() {
        assert_eq!(parse_version_header(""), None);
    }

    #[test]
    fn github_slug_simple_heading() {
        assert_eq!(github_slug("Handler Types"), "handler-types");
    }

    /// Lock fixture (Plan 4 Task 1, D5/C3 supplements): the frontend (T3)
    /// reads the SAME `slug_golden.json` to assert its own anchor-generation
    /// stays aligned with this backend implementation. This test doesn't
    /// specify new behavior — it locks the existing `github_slug`/
    /// `SlugCounter` behavior for cross-end reference.
    #[test]
    fn slug_golden_fixture_alignment() {
        let raw = include_str!("../../tests/fixtures/corpus/slug_golden.json");
        let golden: serde_json::Value = serde_json::from_str(raw).expect("valid golden json");
        for case in golden["single"].as_array().expect("single array") {
            let input = case["input"].as_str().unwrap();
            let expected = case["expected"].as_str().unwrap();
            assert_eq!(github_slug(input), expected, "github_slug({input:?})");
        }
        for case in golden["sequence"].as_array().expect("sequence array") {
            let mut counter = SlugCounter::new();
            let inputs = case["inputs"].as_array().unwrap();
            let expected = case["expected"].as_array().unwrap();
            for (i, e) in inputs.iter().zip(expected) {
                assert_eq!(
                    counter.slug(i.as_str().unwrap()),
                    e.as_str().unwrap(),
                    "SlugCounter sequence {inputs:?}"
                );
            }
        }
    }

    /// Matches `github-slugger`: `` ` `` and `*` strip but hyphens are never
    /// touched or collapsed, so the surviving hyphen from `acme-*\`` sits
    /// directly next to the hyphen produced by the following space, yielding
    /// a double hyphen.
    #[test]
    fn github_slug_strips_markdown_code_span_and_emphasis_punctuation() {
        assert_eq!(
            github_slug("OpenAPI `x-acme-*` Extensions"),
            "openapi-x-acme--extensions"
        );
    }

    /// Matches `github-slugger`: underscores are word characters and are
    /// kept, while backticks, slashes, and comma strip.
    #[test]
    fn github_slug_keeps_underscores_strips_backticks_slashes_and_comma() {
        assert_eq!(github_slug("`/_health`, `/_ready`"), "_health-_ready");
    }

    #[test]
    fn github_slug_is_unicode_aware() {
        assert_eq!(github_slug("Café Déjà Vu"), "café-déjà-vu");
    }

    #[test]
    fn github_slug_preserves_existing_hyphens_without_collapsing() {
        assert_eq!(github_slug("multi -- hyphen"), "multi----hyphen");
    }

    /// Task 11 E2E finding (M3, confirmed against real acme content):
    /// `docs/guides/realtime-and-exec.md`'s real heading is `## Call another
    /// operation — \`exec\` (local)` (em dash U+2014), and
    /// `docs/architecture.md` links to it expecting the anchor
    /// `#call-another-operation--exec-local` — i.e. the em dash is gone
    /// entirely (collapsed into the double hyphen from its surrounding
    /// spaces), matching real `github-slugger`'s Unicode punctuation strip.
    /// The old ASCII-only `is_slug_stripped_punctuation` let the em dash
    /// survive, producing a different (wrong) anchor.
    #[test]
    fn github_slug_strips_non_ascii_punctuation_like_em_dash() {
        assert_eq!(
            github_slug("Call another operation — `exec` (local)"),
            "call-another-operation--exec-local"
        );
    }

    /// Real `github-slugger` also strips CJK/full-width punctuation and
    /// curly quotes — non-ASCII, but not letters/numbers either.
    #[test]
    fn github_slug_strips_cjk_and_curly_punctuation() {
        assert_eq!(github_slug("設定：「快速」開始"), "設定快速開始");
        assert_eq!(github_slug("“Quoted” term"), "quoted-term");
    }

    #[test]
    fn slug_counter_dedups_repeated_headings() {
        let mut counter = SlugCounter::new();
        let slugs: Vec<String> = ["Setup", "Setup"].iter().map(|h| counter.slug(h)).collect();
        assert_eq!(slugs, vec!["setup", "setup-1"]);
    }

    #[test]
    fn slug_counter_third_occurrence_increments_further() {
        let mut counter = SlugCounter::new();
        assert_eq!(counter.slug("Setup"), "setup");
        assert_eq!(counter.slug("Setup"), "setup-1");
        assert_eq!(counter.slug("Setup"), "setup-2");
    }

    #[test]
    fn slug_counter_distinct_headings_dont_collide() {
        let mut counter = SlugCounter::new();
        assert_eq!(counter.slug("Setup"), "setup");
        assert_eq!(counter.slug("Usage"), "usage");
        assert_eq!(counter.slug("Setup"), "setup-1");
    }

    #[test]
    fn parse_nav_happy_path_extracts_groups_pages_and_description() {
        let index_md = "\
# Acme Corpus

**Language:** en

> Acme Corpus is the canonical reference for the Acme platform.

## All pages

### Guide

- [Setup](guide/setup.md) — Install and configure Acme.
- [Usage](guide/usage.md) - Day-to-day usage patterns.
";
        let nav = parse_nav(index_md).expect("valid nav");
        assert_eq!(
            nav.description,
            "Acme Corpus is the canonical reference for the Acme platform."
        );
        assert_eq!(nav.groups.len(), 1);
        assert_eq!(nav.groups[0].title, "Guide");
        assert_eq!(nav.groups[0].pages.len(), 2);
        assert_eq!(nav.groups[0].pages[0].title, "Setup");
        assert_eq!(nav.groups[0].pages[0].path, "guide/setup.md");
        assert_eq!(
            nav.groups[0].pages[0].description,
            "Install and configure Acme."
        );
        assert_eq!(nav.groups[0].pages[1].title, "Usage");
        assert_eq!(nav.groups[0].pages[1].path, "guide/usage.md");
        assert_eq!(
            nav.groups[0].pages[1].description,
            "Day-to-day usage patterns."
        );
    }

    #[test]
    fn parse_nav_without_lede_paragraph_yields_empty_description() {
        let index_md = "\
# Acme Corpus

## All pages

### Guide

- [Setup](guide/setup.md) — Install and configure Acme.
";
        let nav = parse_nav(index_md).expect("valid nav");
        assert_eq!(nav.description, "");
    }

    #[test]
    fn parse_nav_missing_all_pages_section_is_nav_missing_finding() {
        let index_md = "# Acme Corpus\n\nNo nav section here.\n";
        let err = parse_nav(index_md).expect_err("missing section");
        assert_eq!(err.rule, "nav_missing");
        assert_eq!(err.severity, Severity::Error);
    }

    #[test]
    fn parse_nav_skips_malformed_lines_outside_and_inside_groups() {
        let index_md = "\
# Acme Corpus

## All pages

Some stray prose before any group.

### Guide

not a list item
- [Setup](guide/setup.md) — Install and configure Acme.
";
        let nav = parse_nav(index_md).expect("valid nav");
        assert_eq!(nav.groups.len(), 1);
        assert_eq!(nav.groups[0].pages.len(), 1);
    }

    #[test]
    fn parse_nav_page_without_description_is_empty_string() {
        let index_md = "\
# Acme Corpus

## All pages

### Guide

- [Setup](guide/setup.md)
";
        let nav = parse_nav(index_md).expect("valid nav");
        assert_eq!(nav.groups[0].pages[0].description, "");
    }

    #[test]
    fn check_links_skips_external_and_mailto_links() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            "# Docs\n\n## All pages\n\nSee [external](https://example.com) or [mail](mailto:a@b.com).\n"
                .to_string(),
        );
        let nav = NavTree {
            description: String::new(),
            groups: vec![],
        };
        assert_eq!(check_links(&files, &nav, "index.md"), vec![]);
    }

    #[test]
    fn check_links_good_fixture_has_no_findings() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            include_str!("../../tests/fixtures/corpus/good/index.md").to_string(),
        );
        files.insert(
            "guide/setup.md".to_string(),
            include_str!("../../tests/fixtures/corpus/good/guide/setup.md").to_string(),
        );
        files.insert(
            "guide/usage.md".to_string(),
            include_str!("../../tests/fixtures/corpus/good/guide/usage.md").to_string(),
        );
        let nav = parse_nav(files.get("index.md").unwrap()).expect("valid nav");
        assert_eq!(check_links(&files, &nav, "index.md"), vec![]);
    }

    #[test]
    fn check_links_bad_broken_link_fixture_flags_missing_target() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            include_str!("../../tests/fixtures/corpus/bad_broken_link/index.md").to_string(),
        );
        files.insert(
            "guide/setup.md".to_string(),
            include_str!("../../tests/fixtures/corpus/bad_broken_link/guide/setup.md").to_string(),
        );
        let nav = parse_nav(files.get("index.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "index.md"),
            vec![ContractFinding {
                rule: "broken_link".to_string(),
                file: "index.md".to_string(),
                line: Some(8),
                detail: "link target \"guide/missing.md\" does not resolve to a known file"
                    .to_string(),
                severity: Severity::Error,
            }]
        );
    }

    #[test]
    fn check_links_bad_broken_anchor_fixture_flags_missing_heading() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            include_str!("../../tests/fixtures/corpus/bad_broken_anchor/index.md").to_string(),
        );
        files.insert(
            "guide/setup.md".to_string(),
            include_str!("../../tests/fixtures/corpus/bad_broken_anchor/guide/setup.md")
                .to_string(),
        );
        let nav = parse_nav(files.get("index.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "index.md"),
            vec![ContractFinding {
                rule: "broken_anchor".to_string(),
                file: "guide/setup.md".to_string(),
                line: Some(7),
                detail: "anchor \"#faq\" not found in \"guide/setup.md\"".to_string(),
                severity: Severity::Error,
            }]
        );
    }

    #[test]
    fn check_links_bad_orphan_fixture_flags_unreachable_page() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            include_str!("../../tests/fixtures/corpus/bad_orphan/index.md").to_string(),
        );
        files.insert(
            "guide/setup.md".to_string(),
            include_str!("../../tests/fixtures/corpus/bad_orphan/guide/setup.md").to_string(),
        );
        files.insert(
            "guide/orphan.md".to_string(),
            include_str!("../../tests/fixtures/corpus/bad_orphan/guide/orphan.md").to_string(),
        );
        let nav = parse_nav(files.get("index.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "index.md"),
            vec![ContractFinding {
                rule: "orphan_page".to_string(),
                file: "guide/orphan.md".to_string(),
                line: None,
                detail: "page is not reachable from any nav group".to_string(),
                severity: Severity::Warn,
            }]
        );
    }

    /// Task 11 E2E finding: a C++ lambda capture `[](args) { ... }` inside a
    /// fenced ```cpp code block is syntactically identical to a markdown
    /// link `[label](target)` — real acme docs (`docs/guides/handler-types.md`)
    /// hit this repeatedly. The link scanner must not treat fenced code as
    /// prose.
    #[test]
    fn check_links_ignores_lambda_capture_inside_fenced_code_block() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            "\
# Docs

## All pages

### Guide

- [Setup](guide/setup.md) — Handler example.
"
            .to_string(),
        );
        files.insert(
            "guide/setup.md".to_string(),
            "\
# Setup

```cpp
app.route(\"POST\", \"/api/do_thing\")
   .handle<MyReq, MyResp>(
       [](const MyReq& req, acme::context& ctx) -> acme::task<MyResp> {
           co_return MyResp{};
       });
```
"
            .to_string(),
        );
        let nav = parse_nav(files.get("index.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "index.md"),
            vec![],
            "a lambda capture inside a fenced code block must not be scanned as a markdown link"
        );
    }

    /// Task 11 E2E finding: an inline `` `[x](y)` `` code span must also be
    /// skipped, not just fenced blocks.
    #[test]
    fn check_links_ignores_bracket_paren_shape_inside_inline_code_span() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            "\
# Docs

## All pages

### Guide

- [Setup](guide/setup.md) — Inline code example.
"
            .to_string(),
        );
        files.insert(
            "guide/setup.md".to_string(),
            "# Setup\n\nUse `[](args)` as the lambda capture syntax.\n".to_string(),
        );
        let nav = parse_nav(files.get("index.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "index.md"),
            vec![],
            "a `[x](y)`-shaped inline code span must not be scanned as a markdown link"
        );
    }

    /// Task 11 E2E finding: real C++ docs (acme) routinely link into
    /// their own repo's source tree (`../../include/foo.hpp` from a
    /// `docs/reference/` page) — a relative link that resolves OUTSIDE the
    /// corpus's own root is an external reference from the corpus's point
    /// of view (same treatment as an http(s) link), not a same-corpus
    /// broken link.
    #[test]
    fn check_links_skips_relative_links_that_resolve_outside_docs_root() {
        let mut files = BTreeMap::new();
        files.insert(
            "docs/README.md".to_string(),
            "\
# Foo Docs

## All pages

### Reference

- [Configuration](reference/configuration.md) — App config.
"
            .to_string(),
        );
        files.insert(
            "docs/reference/configuration.md".to_string(),
            "# Configuration\n\nSee [app.hpp](../../include/foo/app.hpp) for the source of truth.\n"
                .to_string(),
        );
        let nav = parse_nav(files.get("docs/README.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "docs/README.md"),
            vec![],
            "a link resolving outside docs/ (into the repo's own source tree) must be \
             skipped as external, not flagged broken_link"
        );
    }

    /// Regression guard (review fix): the corpus-root-escape check must
    /// compare against the FULL index directory, not just its first path
    /// segment. `docs_root = "docs"` (a single segment, every other test in
    /// this file) can't distinguish the two — this fixture uses a
    /// multi-segment `docs_root = "documentation/public"`, where a link
    /// resolving to `"documentation/other/..."` shares only the first
    /// segment ("documentation") with the real root. A first-segment-only
    /// comparison would wrongly treat that as in-root and flag it
    /// `broken_link`; the correct behavior is the same skip-as-escape
    /// treatment as `check_links_skips_relative_links_that_resolve_outside_docs_root`.
    #[test]
    fn check_links_skips_relative_links_that_resolve_outside_a_multi_segment_docs_root() {
        let mut files = BTreeMap::new();
        files.insert(
            "documentation/public/README.md".to_string(),
            "\
# Foo Docs

## All pages

### Guide

- [Setup](guide/setup.md) — Install and configure Foo.
"
            .to_string(),
        );
        files.insert(
            "documentation/public/guide/setup.md".to_string(),
            "# Setup\n\nSee [leaked](../../other/leaked.md) for an internal design doc.\n"
                .to_string(),
        );
        let nav =
            parse_nav(files.get("documentation/public/README.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "documentation/public/README.md"),
            vec![],
            "a link resolving to \"documentation/other/leaked.md\" — a SIBLING of the \
             docs_root \"documentation/public\" that merely shares its first path segment \
             — must be skipped as an escape, not misclassified as an in-root broken_link"
        );
    }

    /// A relative link that stays WITHIN the corpus root but still points
    /// at nothing must still be flagged — the fix-3 skip is narrowly for
    /// links that escape the root, not a blanket relaxation.
    #[test]
    fn check_links_still_flags_broken_link_within_docs_root() {
        let mut files = BTreeMap::new();
        files.insert(
            "docs/README.md".to_string(),
            "\
# Foo Docs

## All pages

### Reference

- [Configuration](reference/configuration.md) — App config.
"
            .to_string(),
        );
        files.insert(
            "docs/reference/configuration.md".to_string(),
            "# Configuration\n\nSee [missing](../guide/missing.md) for details.\n".to_string(),
        );
        let nav = parse_nav(files.get("docs/README.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "docs/README.md"),
            vec![ContractFinding {
                rule: "broken_link".to_string(),
                file: "docs/reference/configuration.md".to_string(),
                line: Some(3),
                detail: "link target \"../guide/missing.md\" does not resolve to a known file"
                    .to_string(),
                severity: Severity::Error,
            }],
            "a same-corpus (docs/-prefixed) broken link must still be flagged"
        );
    }

    /// Regression guard (Task 9 review finding): when the index page lives
    /// under a docs_root prefix (the pull-bootstrap convention — corpus keys
    /// are repo-root-relative like `docs/guide/setup.md`, but nav links are
    /// still written relative to the index file, e.g. `guide/setup.md`),
    /// `reachable` must resolve nav paths against the index's own directory
    /// before comparing against `files`' full keys — otherwise every page
    /// is a false orphan_page, and the index page itself (here
    /// `docs/README.md`, not the hardcoded `index.md`) must still be exempt.
    #[test]
    fn check_links_docs_root_prefixed_keys_produce_no_false_orphans() {
        let mut files = BTreeMap::new();
        files.insert(
            "docs/README.md".to_string(),
            "\
# Foo Docs

## All pages

### Guide

- [Setup](guide/setup.md) — Install and configure Foo.
"
            .to_string(),
        );
        files.insert(
            "docs/guide/setup.md".to_string(),
            "# Setup\n\nInstall and configure Foo.\n".to_string(),
        );
        let nav = parse_nav(files.get("docs/README.md").unwrap()).expect("valid nav");
        assert_eq!(
            check_links(&files, &nav, "docs/README.md"),
            vec![],
            "docs_root-prefixed keys must not produce false orphan_page findings, \
             and the index page itself must be exempt via the real index_path"
        );
    }
}
