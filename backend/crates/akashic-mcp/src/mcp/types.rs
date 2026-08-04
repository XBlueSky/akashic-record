use serde::{Deserialize, Serialize};

// ── Default helpers ──────────────────────────────────────────────────

fn default_limit_10() -> u32 {
    10
}

// ── search_knowledge (The Scout) ────────────────────────────────────

/// Arguments for the `search_knowledge` MCP tool (replaces search_context, search_code, search).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchKnowledgeArgs {
    /// Natural-language search query
    pub query: String,
    /// Filter by repository name (optional)
    #[serde(default)]
    pub repo: Option<String>,
    /// Max results (default 10)
    #[serde(default = "default_limit_10")]
    pub limit: u32,
    /// Optional: boost results from a specific space ("code", "doc", "human")
    #[serde(default)]
    pub prefer_space: Option<String>,
    /// Search mode preset: "code", "notes", "saga", "explore", or null for default RRF
    #[serde(default)]
    pub mode: Option<String>,
}

// ── get_details (The Sniper) ────────────────────────────────────────

/// Arguments for the `get_details` MCP tool (replaces get_module_surface, list_modules).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDetailsArgs {
    /// UUIDs from search_knowledge results to fetch full content for
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// If ids omitted, list modules for this repo
    #[serde(default)]
    pub repo: Option<String>,
}

// ── save_note (Enhanced) ────────────────────────────────────────────

/// Arguments for the `save_note` MCP tool.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SaveNoteArgs {
    /// Repository name (e.g., "backend/payment-service")
    pub repo_name: String,
    /// Branch name (e.g., "feat/oauth-login"); defaults to "main" if unsure
    pub branch_name: String,
    /// Must be one of: ARCHITECTURE, BUG_FIX, CONFIG, ONBOARDING, DECISION
    pub category: String,
    /// Short descriptive title for this knowledge entry
    pub title: String,
    /// Actionable one-liner, max 100 characters
    pub summary: String,
    /// Full knowledge content (WHAT/WHY/IMPACT/ACTION format)
    pub content: String,
    /// Machine-readable key facts, each max 200 characters
    #[serde(default)]
    pub facts: Vec<String>,
    /// Optional tags for improved search (e.g., ["nginx", "oauth"])
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional related code symbols to auto-link (e.g., ["AppState", "config::Config"])
    #[serde(default)]
    pub related_symbols: Vec<String>,
    /// Exact file paths this context relates to
    #[serde(default)]
    pub related_files: Vec<String>,
    /// Optional Workplus issue reference (e.g., "PROJ-123456") — auto-creates/joins a saga
    #[serde(default)]
    pub issue_ref: Option<String>,
    /// Optional manual topic name — auto-creates/joins a manual saga
    #[serde(default)]
    pub topic: Option<String>,
    /// UUID of an existing note this one supersedes (marks old note as invalid)
    #[serde(default)]
    pub supersedes: Option<String>,
    /// Skip dedup check (set true after receiving a WARN to force save)
    #[serde(default)]
    pub skip_dedup: bool,
}

// ── get_project_summary (Unchanged) ─────────────────────────────────

/// Arguments for the `get_project_summary` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetProjectSummaryArgs {
    /// Repository name
    pub repo_name: String,
    /// Optional branch filter
    #[serde(default)]
    pub branch_name: Option<String>,
}

/// Category count entry for project summary.
#[derive(Debug, Serialize)]
pub struct CategoryCount {
    pub category: String,
    pub count: i64,
}

/// Brief note info for summaries.
#[derive(Debug, Serialize)]
pub struct NoteBrief {
    pub uuid: String,
    pub category: String,
    pub branch: String,
    pub created_at: String,
    pub snippet: String,
}

/// Project summary returned by `get_project_summary`.
#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    pub repo_name: String,
    pub branches: Vec<String>,
    pub note_count: i64,
    pub categories: Vec<CategoryCount>,
    pub latest_notes: Vec<NoteBrief>,
    pub ingested_modules_count: i64,
    pub total_chunks: i64,
    pub last_ingested_at: Option<String>,
}

// ── traverse_code_calls ────────────────────────────────────────────

fn default_direction() -> String {
    "both".into()
}
fn default_depth_3() -> u32 {
    3
}
fn default_confidence() -> f32 {
    0.6
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TraverseCallsArgs {
    /// Function/method name to trace (supports partial match via CONTAINS)
    pub symbol: String,
    /// Restrict to repository
    #[serde(default)]
    pub repo: Option<String>,
    /// Direction: "downstream" (callees), "upstream" (callers), or "both"
    #[serde(default = "default_direction")]
    pub direction: String,
    /// Maximum traversal depth (1-4)
    #[serde(default = "default_depth_3")]
    pub max_depth: u32,
    /// Minimum CALLS edge confidence to follow (0.6-1.0). The default (0.6)
    /// excludes `trait_default` (0.5, D3's trait/generic-dispatch approximation
    /// tier) — pass a lower value to include those edges.
    #[serde(default = "default_confidence")]
    pub min_confidence: f32,
}

// ── goto_definition ───────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GotoDefinitionArgs {
    /// Exact symbol name or fully-qualified name (e.g. "resolve_import_target"
    /// or "ingestion.pipeline.resolve_import_target"). Matched exactly.
    pub name: String,
    /// Restrict to repository
    #[serde(default)]
    pub repo: Option<String>,
    /// Narrow by module path substring (forgiving hint)
    #[serde(default)]
    pub module_hint: Option<String>,
    /// Narrow by chunk_type (e.g. "function", "struct", "trait")
    #[serde(default)]
    pub kind_hint: Option<String>,
}

// ── find_references ───────────────────────────────────────────────

fn default_ref_confidence() -> f32 {
    0.7
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindReferencesArgs {
    /// Exact fully-qualified name of the DEFINITION to find references to.
    /// Get it from goto_definition first.
    pub fqn: String,
    /// Restrict to repository
    #[serde(default)]
    pub repo: Option<String>,
    /// Minimum CALLS/REFERENCES confidence (default 0.7 excludes heuristic 0.6
    /// guesses and trait_default 0.5 approximations)
    #[serde(default = "default_ref_confidence")]
    pub min_confidence: f32,
    /// Restrict to these reference kinds ("call", "type"); empty = all.
    #[serde(default)]
    pub include_kinds: Vec<String>,
}

// ── find_implementations ─────────────────────────────────────────

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindImplementationsArgs {
    /// Exact fqn of the trait/interface/class.
    pub fqn: String,
    /// Restrict to repository
    #[serde(default)]
    pub repo: Option<String>,
    /// true (default) = who implements/extends fqn; false = what fqn implements/extends.
    #[serde(default = "default_true")]
    pub implementors: bool,
}

// ── trace_execution_flow ──────────────────────────────────────────

fn default_flow_depth() -> u32 {
    8
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TraceFlowArgs {
    /// Symbol name to find flows containing it (optional if list_all=true)
    #[serde(default)]
    pub symbol: Option<String>,
    /// Restrict to repository
    #[serde(default)]
    pub repo: Option<String>,
    /// Filter by entry type: "api_handler", "main", "test", "event_listener", "module_entry"
    #[serde(default)]
    pub entry_type: Option<String>,
    /// Maximum flow depth to display (default 8)
    #[serde(default = "default_flow_depth")]
    pub max_depth: u32,
    /// List all flows in the repo instead of searching by symbol
    #[serde(default)]
    pub list_all: bool,
}

// ── get_note_health ───────────────────────────────────────────────

fn default_min_staleness() -> f64 {
    0.3
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NoteHealthArgs {
    /// Repository name
    pub repo: String,
    /// Minimum staleness score to show (default 0.3)
    #[serde(default = "default_min_staleness")]
    pub min_staleness: f64,
}

// ── global_query ──────────────────────────────────────────────────

fn default_max_communities() -> u32 {
    5
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GlobalQueryArgs {
    /// Natural-language query about overall architecture/themes
    pub query: String,
    /// Repository name (required)
    pub repo: String,
    /// Target community level: 0 (fine), 1 (architectural), 2 (strategic). Auto-selects if omitted.
    #[serde(default)]
    pub level: Option<u8>,
    /// Include representative code snippets (default false)
    #[serde(default)]
    pub include_examples: bool,
    /// Maximum communities to return (default 5)
    #[serde(default = "default_max_communities")]
    pub max_communities: u32,
}

// ── analyze_impact ─────────────────────────────────────────────────

fn default_depth_2() -> u32 {
    2
}
fn default_detect_max_communities() -> u32 {
    25
}
fn default_detect_max_members() -> u32 {
    15
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AnalyzeImpactArgs {
    /// Symbol name or file/module path to analyze
    pub target: String,
    /// Restrict to repository
    #[serde(default)]
    pub repo: Option<String>,
    /// Maximum depth for CALLS traversal (1-4)
    #[serde(default = "default_depth_2")]
    pub max_depth: u32,
    /// Minimum impact score to include in results (default 0.0)
    #[serde(default)]
    pub min_score: f64,
}

// ── analyze_change_impact ──────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AnalyzeChangeImpactArgs {
    /// The set of changed symbol names (from your diff/PR) to analyze together.
    /// Each entry is matched by name SUBSTRING (`CONTAINS`), inherited from
    /// `analyze_impact`'s target matching — a symbol may match more than
    /// intended. Self-exclusion of a changed symbol from its own impact is
    /// exact-name-only, not substring-based.
    pub changed: Vec<String>,
    /// Restrict to repository
    #[serde(default)]
    pub repo: Option<String>,
    /// Maximum depth for CALLS traversal (1-4, default 2)
    #[serde(default = "default_depth_2")]
    pub max_depth: u32,
    /// Minimum impact score to include in results (default 0.0)
    #[serde(default)]
    pub min_score: f64,
}

// ── supersede_note ───────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupersedeNoteArgs {
    /// UUID of the old note to mark as superseded
    pub old_note_id: String,
    /// UUID of the new note that replaces it
    pub new_note_id: String,
}

// ── list_sagas ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListSagasArgs {
    /// Repository name
    pub repo_name: String,
    /// Optional status filter: "active", "resolved", "archived"
    #[serde(default)]
    pub status: Option<String>,
}

// ── get_saga_timeline ────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetSagaTimelineArgs {
    /// Saga UUID
    pub saga_id: String,
}

// ── detect_code_communities ───────────────────────────────────────

/// Arguments for the `detect_code_communities` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DetectCodeCommunitiesArgs {
    /// Repository name
    pub repo: String,
    /// Minimum CALLS edge confidence to include (default 0.0)
    #[serde(default)]
    pub min_confidence: f64,
    /// Max communities to show (default 25)
    #[serde(default = "default_detect_max_communities")]
    pub max_communities: u32,
    /// Max members per community to show (default 15)
    #[serde(default = "default_detect_max_members")]
    pub max_members: u32,
}

// ── detect_dead_code ──────────────────────────────────────────────

fn default_dead_code_max_candidates() -> u32 {
    50
}

/// Arguments for the `detect_dead_code` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DetectDeadCodeArgs {
    /// Repository name
    pub repo: String,
    /// Max candidates to list (default 50)
    #[serde(default = "default_dead_code_max_candidates")]
    pub max_candidates: u32,
}

// ── link_cross_service_calls ──────────────────────────────────────

fn default_include_intra_repo() -> bool {
    true
}

/// Arguments for the `link_cross_service_calls` MCP tool. No repo scope — the
/// tool rebuilds ALL cross-service `HTTP_CALLS` edges across the whole graph.
#[derive(Debug, Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct LinkCrossServiceCallsArgs {
    /// Include intra-repo links in the report (default true). When false, the
    /// report shows only cross-repo links (the edges are persisted either way).
    #[serde(default = "default_include_intra_repo")]
    pub include_intra_repo: bool,
}

// ── trace_decision_history ────────────────────────────────────────

/// Arguments for the `trace_decision_history` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TraceDecisionHistoryArgs {
    /// Repository name
    pub repo: String,
    /// Code symbol or fully-qualified name to trace decisions for. Matched as a
    /// forgiving substring (CONTAINS) against chunk name/fqn.
    pub symbol: String,
}

// ── get_decision_lineage ──────────────────────────────────────────

/// Arguments for the `get_decision_lineage` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDecisionLineageArgs {
    /// UUID of the DECISION note to fetch lineage for
    pub note_id: String,
}

// ── docs contract tools (E5/A6) ─────────────────────────────────────
//
// Read-only, anonymous-callable tools exposing the docs-kit corpus
// contract to the kit's `akashic` plugin: the same canonical schemas
// (`akashic_domain::types::corpus`) and validator
// (`akashic_domain::algos::corpus_contract`) the platform's own ingest
// path uses, plus a sectioned authoring guide. See
// `mcp/tools/docs_contract.rs`.

/// Arguments for the `get_docs_schema` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDocsSchemaArgs {
    /// Which canonical JSON Schema to return: "manifest" (CI packer's corpus
    /// manifest) or "docs-toml" (the `.akashic/docs.toml` pull-bootstrap
    /// contract)
    pub which: String,
}

/// Arguments for the `list_authoring_sections` MCP tool. Takes no parameters
/// — the guide's sections are fixed content, not scoped by caller input.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListAuthoringSectionsArgs {}

/// Arguments for the `get_authoring_guide` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetAuthoringGuideArgs {
    /// Section id from `list_authoring_sections` (e.g. "page-writing")
    pub section: String,
}

/// Arguments for the `check_docs_coverage` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CheckDocsCoverageArgs {
    /// Every file in the corpus tree to validate, keyed by its corpus-relative
    /// path (the same convention `manifest.index`/nav links use)
    pub files: std::collections::BTreeMap<String, String>,
    /// Path (a key of `files`) of the index page
    pub index: String,
}

/// Arguments for the `list_docs` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListDocsArgs {
    /// Repo to inspect. Omitted: list every repo that has published docs,
    /// each with its latest version stamp. Provided: that repo's full nav
    /// tree at the latest version (page paths feed `get_docs_page`).
    pub repo: Option<String>,
}

/// Arguments for the `get_docs_page` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDocsPageArgs {
    /// Repo whose published docs corpus to read
    pub repo: String,
    /// Corpus-relative page path, exactly as listed by `list_docs`
    pub path: String,
    /// Version selector: "latest" (default), an exact version string, or a
    /// sha prefix (>= 7 hex chars)
    pub version: Option<String>,
}
