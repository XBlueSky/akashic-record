use super::*;

// ── Service-layer request/response types (A1 Task 10) ────────────────────────
//
// These are domain types used as inputs/outputs of the `SearchService`,
// `NavigationService`, and `GraphService` traits. No infra dependencies.

/// Optional space preference for cross-space RRF in GraphRAG queries.
///
/// Promoted from `akashic-retrieval::graphrag::types` (A1 Task 10) so
/// `SearchService` can reference it without depending on the retrieval crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferSpace {
    Code,
    Doc,
    Human,
}

impl PreferSpace {
    pub fn to_space(self) -> Space {
        match self {
            PreferSpace::Code => Space::Code,
            PreferSpace::Doc => Space::Doc,
            PreferSpace::Human => Space::Human,
        }
    }
}

/// Request to the GraphRAG multi-space retrieval pipeline.
///
/// Promoted from `akashic-retrieval::graphrag::types` (A1 Task 10).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GraphRagQuery {
    pub query: String,
    pub repo_name: Option<String>,
    pub k_per_space: Option<usize>,
    pub token_budget: Option<usize>,
    #[serde(default)]
    pub prefer_space: Option<PreferSpace>,
}

/// A scored, ranked node in the GraphRAG result set.
///
/// Promoted from `akashic-retrieval::graphrag::types` (A1 Task 10).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoredNode {
    pub pg_id: uuid::Uuid,
    pub space: Space,
    pub entity_type: String,
    pub name: String,
    pub final_score: f32,
    pub hop_distance: u8,
    pub content: String,
    pub token_count: usize,
    pub module_path: Option<String>,
    pub language: Option<String>,
    pub document_title: Option<String>,
    pub heading: Option<String>,
    pub author: Option<String>,
    pub category: Option<String>,
}

/// Response from the GraphRAG multi-space retrieval pipeline.
///
/// Promoted from `akashic-retrieval::graphrag::types` (A1 Task 10).
#[derive(Debug, serde::Serialize)]
pub struct GraphRagResponse {
    pub context: String,
    pub nodes_used: Vec<ScoredNode>,
    pub total_tokens: usize,
}

/// A candidate symbol definition returned by `NavigationService::goto_definition`.
///
/// Mirrors `akashic_retrieval::graphrag::symbol_resolution::SymbolCandidate`
/// as a domain type so the service trait stays infra-free (A1 Task 10).
#[derive(Debug, Clone)]
pub struct SymbolCandidate {
    pub fqn: Option<String>,
    pub name: String,
    pub chunk_type: String,
    pub module_path: String,
    pub visibility: Option<String>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub signature: Option<String>,
}

/// A single direct or indirect reference to a symbol.
///
/// Mirrors `akashic_retrieval::graphrag::symbol_resolution::ReferenceRow`
/// as a domain type so the service trait stays infra-free (A1 Task 10).
#[derive(Debug, Clone)]
pub struct ReferenceRow {
    pub caller_fqn: String,
    pub caller_name: String,
    pub module_path: String,
    pub chunk_type: String,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    /// Relationship kind: "call", "import", "implements", …
    pub ref_kind: String,
    pub confidence: f64,
    pub method: String,
}

/// Result of `NavigationService::analyze_impact`.
#[derive(Debug)]
pub struct ImpactReport {
    /// Score-sorted list of affected nodes.
    pub affected: Vec<ImpactNode>,
    /// Flows whose execution path passes through the target.
    pub flows: Vec<AffectedFlow>,
    /// Suggested test entry-points derived from affected flows.
    pub suggested_tests: Vec<SuggestedTest>,
}

/// Result of `NavigationService::analyze_change_impact` — the combined blast
/// radius for a SET of changed symbols.
#[derive(Debug)]
pub struct ChangeImpactReport {
    /// The de-duplicated set of changed symbols analyzed (input echo).
    pub changed: Vec<String>,
    /// Changed symbols that produced >= 1 raw impact edge.
    pub with_impact: Vec<String>,
    /// Changed symbols that produced no raw impact edge. NOT the same as
    /// "not found": a symbol yields zero edges either because it is not indexed
    /// OR because it is indexed but has no dependents.
    pub without_impact: Vec<String>,
    /// Merged, score-sorted affected nodes (changed symbols self-excluded).
    pub affected: Vec<ImpactNode>,
    /// Union of affected execution flows across all seeds (de-duplicated).
    pub flows: Vec<AffectedFlow>,
    /// Union of suggested tests across all seeds (de-duplicated).
    pub suggested_tests: Vec<SuggestedTest>,
    /// Overall risk level ("LOW" / "MEDIUM" / "HIGH") over the merged set.
    pub risk: String,
}

/// Request to `SearchService::search_knowledge` (unified dual-level search).
#[derive(Debug, Clone)]
pub struct KnowledgeSearchRequest {
    pub query: String,
    pub repo: Option<String>,
    /// Space filter: "map", "note", "doc", or "all" (default).
    pub layer: Option<String>,
    pub limit: i64,
}

/// Docs-corpus deep-link payload for a DOC-layer hit whose backing
/// document is a corpus page (`doc_type == "corpus"`).
#[derive(Debug, Clone)]
pub struct KnowledgeDocsRef {
    /// Corpus repo name.
    pub repo: String,
    /// Full corpus key (`documents.source_url`), NOT docs_root-relative.
    pub path: String,
    /// `github_slug(section heading)`; may be empty for a heading that
    /// slugs to nothing.
    pub anchor: String,
}

/// A single compact search hit from `SearchService::search_knowledge`.
#[derive(Debug, Clone)]
pub struct KnowledgeSearchItem {
    pub id: String,
    pub layer: String,
    pub result_type: String,
    pub name: String,
    pub repo_name: String,
    pub module: Option<String>,
    pub summary: Option<String>,
    pub score: f64,
    /// Present only for a DOC-layer hit backed by a docs-corpus page.
    pub docs: Option<KnowledgeDocsRef>,
}

/// Result of `SearchService::relink_explains`.
#[derive(Debug)]
pub struct RelinkResult {
    pub sections_processed: usize,
    pub edges_created_exact: usize,
    pub edges_created_llm: usize,
    pub target_repos: Vec<String>,
}

// ── get_details batch fetch types ────────────────────────────────────────────

/// Full chunk row returned by `ChunkRepo::fetch_chunks_full_batch`.
///
/// Columns: `id, name, chunk_type, module_path, signature, content, language`
/// (verbatim from the `GET /api/v1/details` handler).
#[derive(Debug, Clone)]
pub struct ChunkFullRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub chunk_type: String,
    pub module_path: String,
    pub signature: Option<String>,
    pub content: String,
    pub language: Option<String>,
}

/// Full note row returned by `NoteRepo::fetch_notes_full_batch`.
///
/// Columns: `id, title, summary, facts, content, category,
///           related_symbols, related_files, branch`
/// (verbatim from the `GET /api/v1/details` handler).
#[derive(Debug, Clone)]
pub struct NoteFullRow {
    pub id: uuid::Uuid,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub facts: Option<Vec<String>>,
    pub content: String,
    pub category: String,
    pub related_symbols: Option<Vec<String>>,
    pub related_files: Option<Vec<String>>,
    pub branch: Option<String>,
}

/// Section + document title row returned by
/// `DocumentRepo::fetch_sections_detail_batch`.
///
/// Columns: `s.id, s.heading, s.content, d.title, s.depth, s.tags`
/// (verbatim from the `GET /api/v1/details` handler).
#[derive(Debug, Clone)]
pub struct SectionDetailRow {
    pub id: uuid::Uuid,
    pub heading: String,
    pub content: String,
    pub document_title: String,
    pub depth: i32,
    pub tags: Option<Vec<String>>,
}

// ── global_query community types ─────────────────────────────────────────────

/// A community row from `CommunityRepo::select_communities_by_vector`.
#[derive(Debug, Clone)]
pub struct CommunitySummaryRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub summary: String,
    pub level: i16,
    pub member_count: i32,
    pub score: f64,
}
