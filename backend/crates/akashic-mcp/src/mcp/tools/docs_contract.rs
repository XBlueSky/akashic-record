//! docs contract MCP tools (E5/A6): read-only, anonymous-callable tools
//! exposing the SAME Rust validator (`akashic_domain::algos::corpus_contract`)
//! and canonical schemas (`akashic_domain::types::corpus`) the platform's own
//! ingest path uses to the docs-kit kit's `akashic` plugin, plus a sectioned
//! authoring guide. One of the composed `#[tool_router]` blocks; combined in
//! `super`'s `AkashicMcp::new`.
use std::collections::BTreeMap;

use akashic_domain::algos::corpus_contract::{check_links, parse_nav};
use akashic_domain::types::corpus::{
    ContractFinding, Severity, docs_toml_json_schema, manifest_json_schema,
};

use super::*;

/// The docs-authoring guide, sectioned with `<!-- section: id | when: ... -->`
/// markers. `include_str!` keeps the guide reviewable as plain markdown while
/// letting [`list_authoring_sections`]/[`get_authoring_guide`] serve it
/// piecemeal.
const AUTHORING_GUIDE: &str = include_str!("../../../resources/docs-authoring-guide.md");

/// One entry of `list_authoring_sections`'s JSON array.
#[derive(Debug, Clone, serde::Serialize)]
struct AuthoringSectionSummary {
    id: String,
    title: String,
    when: String,
}

/// A parsed `<!-- section: id | when: ... -->` block: its metadata plus the
/// byte range (`start..end`, `start` at the marker line, `end` at the next
/// marker or EOF) of its full body within the source guide text.
struct GuideSection {
    id: String,
    title: String,
    when: String,
    start: usize,
    end: usize,
}

/// Byte offset (of the marker line's own start) + parsed id/when for every
/// `<!-- section: id | when: ... -->` marker line in `guide`, in file order.
/// Manual line-scan (not regex — `akashic-mcp` has no `regex` dependency),
/// same style as `corpus_contract::find_line_start`'s byte-offset walk.
fn find_section_markers(guide: &str) -> Vec<(usize, String, String)> {
    let mut markers = Vec::new();
    let mut offset = 0usize;
    for line in guide.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim();
        if let Some(rest) = trimmed
            .strip_prefix("<!-- section:")
            .and_then(|r| r.strip_suffix("-->"))
            && let Some((id_part, when_part)) = rest.split_once('|')
        {
            let id = id_part.trim().to_string();
            let when = when_part
                .trim()
                .strip_prefix("when:")
                .unwrap_or(when_part.trim())
                .trim()
                .to_string();
            markers.push((offset, id, when));
        }
        offset += line.len();
    }
    markers
}

/// Parse every section of `guide` into its metadata + body byte range. Title
/// is the `## ` heading found on the first such line following the marker
/// (per the E5 interface contract: "title = the `## ` heading following the
/// marker").
fn parsed_sections(guide: &str) -> Vec<GuideSection> {
    let markers = find_section_markers(guide);
    markers
        .iter()
        .enumerate()
        .map(|(i, (start, id, when))| {
            let end = markers.get(i + 1).map_or(guide.len(), |(next, ..)| *next);
            let title = guide[*start..end]
                .lines()
                .find_map(|l| l.trim().strip_prefix("## ").map(|t| t.trim().to_string()))
                .unwrap_or_default();
            GuideSection {
                id: id.clone(),
                title,
                when: when.clone(),
                start: *start,
                end,
            }
        })
        .collect()
}

/// The canonical JSON Schema for `which` ("manifest" or "docs-toml") — the
/// same schemars-generated schemas the platform validator uses, no
/// hand-maintained copy.
fn docs_schema(which: &str) -> Result<serde_json::Value, String> {
    match which {
        "manifest" => Ok(manifest_json_schema()),
        "docs-toml" => Ok(docs_toml_json_schema()),
        other => Err(format!(
            "unknown schema \"{other}\": expected \"manifest\" or \"docs-toml\""
        )),
    }
}

/// `[{id, title, when}]` for every section of [`AUTHORING_GUIDE`], as a
/// pretty-printed JSON string (matching the other JSON-returning tools'
/// convention, e.g. `get_details`).
fn list_authoring_sections_json() -> String {
    let summaries: Vec<AuthoringSectionSummary> = parsed_sections(AUTHORING_GUIDE)
        .into_iter()
        .map(|s| AuthoringSectionSummary {
            id: s.id,
            title: s.title,
            when: s.when,
        })
        .collect();
    serde_json::to_string_pretty(&summaries)
        .expect("Vec<AuthoringSectionSummary> always serializes")
}

/// The raw markdown of one guide section (marker line through the next
/// marker/EOF), or an error naming the unknown `section` id.
fn get_authoring_guide_section(section: &str) -> Result<String, String> {
    parsed_sections(AUTHORING_GUIDE)
        .into_iter()
        .find(|s| s.id == section)
        .map(|s| AUTHORING_GUIDE[s.start..s.end].trim_end().to_string())
        .ok_or_else(|| format!("unknown authoring guide section \"{section}\""))
}

/// Validate `files` against the SAME contract the platform's ingest path
/// enforces: look up `index` in `files`, `parse_nav` it, then `check_links`.
/// A missing `index` key produces a `nav_missing` finding with the same rule
/// name `parse_nav` itself uses for "no `## All pages` section found" — both
/// mean "couldn't establish a nav tree to validate against" (E5 interface:
/// "nav_missing finding if absent").
fn docs_coverage(files: &BTreeMap<String, String>, index: &str) -> Vec<ContractFinding> {
    let Some(index_md) = files.get(index) else {
        return vec![ContractFinding {
            rule: "nav_missing".to_string(),
            file: index.to_string(),
            line: None,
            detail: format!("index file \"{index}\" not found in provided files"),
            severity: Severity::Error,
        }];
    };
    match parse_nav(index_md) {
        Ok(nav) => check_links(files, &nav, index),
        Err(finding) => vec![finding],
    }
}

#[tool_router(router = docs_tools, vis = "pub(crate)")]
impl AkashicMcp {
    // ══════════════════════════════════════════════════════════════════
    // get_docs_schema — canonical manifest / docs.toml JSON Schema
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "get_docs_schema",
        description = "Return the canonical JSON Schema (2020-12 dialect) for a docs-kit \
artifact contract. `which` is \"manifest\" (the CI packer's corpus manifest) or \"docs-toml\" \
(the `.akashic/docs.toml` pull-bootstrap contract). Same schemars-generated schema the platform \
validator uses — no hand-maintained copy to drift out of sync."
    )]
    #[allow(clippy::unused_self)] // pure lookup; no instance state needed
    async fn get_docs_schema(
        &self,
        Parameters(args): Parameters<GetDocsSchemaArgs>,
    ) -> Result<String, String> {
        let schema = docs_schema(&args.which)?;
        serde_json::to_string_pretty(&schema).map_err(|e| format!("serialization failed: {e}"))
    }

    // ══════════════════════════════════════════════════════════════════
    // list_authoring_sections — sectioned authoring guide, table of contents
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "list_authoring_sections",
        description = "List every section of the docs-authoring guide as [{id, title, when}]. \
Use `when` to pick which section(s) are relevant to what you're about to write or fix, then \
fetch the body with get_authoring_guide."
    )]
    #[allow(clippy::unused_self)] // pure lookup; no instance state needed
    async fn list_authoring_sections(
        &self,
        Parameters(_args): Parameters<ListAuthoringSectionsArgs>,
    ) -> Result<String, String> {
        Ok(list_authoring_sections_json())
    }

    // ══════════════════════════════════════════════════════════════════
    // get_authoring_guide — one section's raw markdown
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "get_authoring_guide",
        description = "Fetch one section of the docs-authoring guide by id (see \
list_authoring_sections). Returns that section's raw markdown."
    )]
    #[allow(clippy::unused_self)] // pure lookup; no instance state needed
    async fn get_authoring_guide(
        &self,
        Parameters(args): Parameters<GetAuthoringGuideArgs>,
    ) -> Result<String, String> {
        get_authoring_guide_section(&args.section)
    }

    // ══════════════════════════════════════════════════════════════════
    // check_docs_coverage — the SAME validator the platform's ingest runs
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "check_docs_coverage",
        description = "Validate a docs corpus tree against the SAME contract the platform's \
ingest path enforces (parse_nav + check_links): nav_missing / broken_link / broken_anchor / \
orphan_page findings, identical severities and messages to what publish would produce. `files` \
is {path: content} for the whole corpus tree (markdown and any linked assets), keyed by \
corpus-relative path matching manifest.index's own convention; `index` is the index page's path \
within `files`. Returns a JSON array of findings — empty means the tree passes."
    )]
    #[allow(clippy::unused_self)] // pure lookup; no instance state needed
    async fn check_docs_coverage(
        &self,
        Parameters(args): Parameters<CheckDocsCoverageArgs>,
    ) -> Result<String, String> {
        let findings = docs_coverage(&args.files, &args.index);
        serde_json::to_string_pretty(&findings).map_err(|e| format!("serialization failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    // ── docs_schema ──────────────────────────────────────────────────

    #[test]
    fn docs_schema_manifest_returns_manifest_schema() {
        let schema = docs_schema("manifest").expect("known schema");
        assert_eq!(schema["title"], "CorpusManifest");
    }

    #[test]
    fn docs_schema_docs_toml_returns_docs_toml_schema() {
        let schema = docs_schema("docs-toml").expect("known schema");
        assert_eq!(schema["title"], "DocsToml");
    }

    #[test]
    fn docs_schema_unknown_which_is_err() {
        assert!(docs_schema("bogus").is_err());
    }

    // ── authoring guide sections ────────────────────────────────────

    #[test]
    fn list_authoring_sections_returns_all_seven_in_order() {
        let sections = parsed_sections(AUTHORING_GUIDE);
        let ids: Vec<&str> = sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "overview",
                "page-writing",
                "bilingual",
                "tables-for-parity",
                "snippets",
                "nav-index",
                "assets",
            ]
        );
        for s in &sections {
            assert!(!s.title.is_empty(), "section {} has empty title", s.id);
            assert!(!s.when.is_empty(), "section {} has empty when", s.id);
        }
    }

    #[test]
    fn get_authoring_guide_returns_body_for_known_section() {
        let body = get_authoring_guide_section("page-writing").expect("known section");
        assert!(body.contains("## Writing self-contained pages"));
        assert!(
            !body.contains("<!-- section: bilingual"),
            "section body must not leak into the next section: {body}"
        );
    }

    #[test]
    fn get_authoring_guide_unknown_section_is_err() {
        assert!(get_authoring_guide_section("bogus").is_err());
    }

    // ── check_docs_coverage ─────────────────────────────────────────

    #[test]
    fn docs_coverage_missing_index_is_nav_missing_finding() {
        let files: BTreeMap<String, String> = BTreeMap::new();
        let findings = docs_coverage(&files, "index.md");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "nav_missing");
    }

    #[test]
    fn docs_coverage_good_tree_has_no_findings() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            "# Docs\n\n## All pages\n\n### Guide\n\n- [Setup](guide/setup.md) — Install.\n"
                .to_string(),
        );
        files.insert(
            "guide/setup.md".to_string(),
            "# Setup\n\nInstall steps.\n".to_string(),
        );
        let findings = docs_coverage(&files, "index.md");
        assert_eq!(findings, vec![]);
    }

    #[test]
    fn docs_coverage_broken_link_is_flagged() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.md".to_string(),
            "# Docs\n\n## All pages\n\n### Guide\n\n- [Setup](guide/missing.md) — Install.\n"
                .to_string(),
        );
        let findings = docs_coverage(&files, "index.md");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "broken_link");
    }
}
