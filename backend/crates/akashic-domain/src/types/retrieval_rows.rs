use super::*;

// ── Community domain types ────────────────────────────────────────────────────

/// A single member within a community (chunk or module).
///
/// Used by `CommunityRepo::store_communities` (A1 Task 6).
#[derive(Debug, Clone)]
pub struct CommunityMemberRow {
    pub pg_id: uuid::Uuid,
    pub name: String,
    pub member_type: String, // "chunk" or "module"
}

/// A single community level result.
///
/// Used by `CommunityRepo::store_communities` (A1 Task 6).
#[derive(Debug, Clone)]
pub struct CommunityLevelRow {
    pub level: u8,
    pub members: Vec<CommunityMemberRow>,
}

// ── Document / Section domain types ──────────────────────────────────────────

/// A section embedding row fetched from the DB for clustering.
///
/// Used by `DocClusterRepo::fetch_section_embeddings` (A1 Task 6).
#[derive(Debug, Clone)]
pub struct SectionEmbeddingRow {
    pub id: uuid::Uuid,
    pub heading: String,
    pub embedding: Vec<f32>,
}

/// A section row with content + optional embedding for the relink-explains pass.
///
/// SQL (verbatim from `POST /api/v1/repos/:name/relink-explains` handler):
/// `SELECT s.id, s.heading, s.content, s.embedding`
/// `FROM sections s JOIN documents d ON s.doc_id = d.id`
/// `WHERE d.repo_name = $1`
///
/// Used by `DocumentRepo::fetch_sections_for_relink`.
#[derive(Debug, Clone)]
pub struct RelinkSectionRow {
    pub id: uuid::Uuid,
    pub heading: String,
    pub content: String,
    /// `None` when the section was ingested without an embedding (e.g. very old
    /// data or a skipped embedding pass).
    pub embedding: Option<Vec<f32>>,
}

// ── Search / fetch row types for retrieval (graphrag / symbol_resolution) ─────

/// A section search hit (vector or BM25) from the `sections` PG table.
///
/// Used by `DocumentRepo::search_sections_by_vector` and
/// `DocumentRepo::search_sections_by_bm25` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct SectionSearchRow {
    pub id: uuid::Uuid,
    pub heading: String,
    pub score: f32,
}

/// A note search hit (vector or BM25) from the `notes` PG table.
///
/// Used by `NoteRepo::search_notes_by_vector` and
/// `NoteRepo::search_notes_by_bm25` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct NoteSearchRow {
    pub id: uuid::Uuid,
    pub title: String,
    pub score: f32,
}

// ── Scout (unified compact-search) rows ───────────────────────────────────────
//
// These back the `GET /api/v1/search` "Scout" endpoint (`SearchService::
// search_knowledge`), which is a richer surface than the GraphRAG retrieval
// search rows above: it returns display columns (module path, per-row repo
// name, note category/summary, section document title) and — unlike the
// GraphRAG note search — does NOT filter out archived notes. They are kept
// distinct from `ChunkSearchRow`/`NoteSearchRow`/`SectionSearchRow` so the
// GraphRAG RRF path (vector + BM25 + large variants) stays byte-for-byte
// unchanged.

/// A Scout chunk hit (`ChunkRepo::scout_search_chunks_by_vector`).
#[derive(Debug, Clone)]
pub struct ScoutChunkRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub chunk_type: String,
    pub module_path: String,
    pub repo_name: String,
    pub score: f64,
}

/// A Scout note hit (`NoteRepo::scout_search_notes_by_vector`).
#[derive(Debug, Clone)]
pub struct ScoutNoteRow {
    pub id: uuid::Uuid,
    pub category: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub repo_name: String,
    pub score: f64,
}

/// A Scout section hit (`DocumentRepo::scout_search_sections_by_vector`).
#[derive(Debug, Clone)]
pub struct ScoutSectionRow {
    pub id: uuid::Uuid,
    pub heading: String,
    pub doc_title: String,
    pub repo_name: String,
    pub score: f64,
    /// `documents.doc_type` — `"corpus"` for a DOC-layer hit whose backing
    /// document is a docs-corpus page (as opposed to an EXPLAINS-derived
    /// summary or other doc source); used to decide whether to attach a
    /// `KnowledgeDocsRef` deep link.
    pub doc_type: String,
    /// `documents.source_url` — for a `doc_type == "corpus"` document, the
    /// full corpus key (e.g. `guide/setup.md`); `None` for non-corpus
    /// documents or ones ingested without a source URL.
    pub source_url: Option<String>,
}

/// Full content row for a single chunk (for graphrag context assembly).
///
/// Used by `ChunkRepo::fetch_chunk_content` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct ChunkContentRow {
    pub content: String,
    pub module_path: String,
    pub language: Option<String>,
}

/// Full content row for a large_chunk (for graphrag context assembly).
///
/// Used by `ChunkRepo::fetch_large_chunk_content` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct LargeChunkContentRow {
    pub content: String,
    pub module_path: String,
}

/// Full content row for a section + its document title (for graphrag context).
///
/// Used by `DocumentRepo::fetch_section_content` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct SectionContentRow {
    pub content: String,
    pub doc_title: String,
}

/// Full content row for a note (for graphrag context assembly).
///
/// Used by `NoteRepo::fetch_note_content` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct NoteContentRow {
    pub content: String,
    pub author: Option<String>,
    pub category: Option<String>,
}

/// A symbol candidate definition from the `chunks` PG table.
///
/// Used by `SymbolRepo::resolve_symbol_candidates` (A1 Task 7).
/// Mirrors `akashic_retrieval::graphrag::symbol_resolution::SymbolCandidate`
/// without the `sqlx::FromRow` derive.
#[derive(Debug, Clone)]
pub struct SymbolCandidateRow {
    pub fqn: Option<String>,
    pub name: String,
    pub chunk_type: String,
    pub module_path: String,
    pub visibility: Option<String>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub signature: Option<String>,
}

/// Chunk metadata row used for enriching symbol reference lookups.
///
/// Used by `SymbolRepo::enrich_chunk_meta_by_fqns` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct ChunkMetaRow {
    pub fqn: String,
    pub module_path: String,
    pub chunk_type: String,
}

/// A chunk-id lookup result used by linking (explains + attached_to).
///
/// Used by `SymbolRepo::find_chunk_ids_by_name` and
/// `SymbolRepo::find_chunk_ids_by_name_and_module` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct ChunkIdRow {
    pub id: uuid::Uuid,
    pub repo_name: String,
}

/// A module-id lookup result used by linking (explains edges to modules).
///
/// Used by `SymbolRepo::find_module_ids_by_path_fragment` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct ModuleIdRow {
    pub id: uuid::Uuid,
    pub repo_name: String,
}

/// A graph neighbour node returned from a Neo4j traversal.
///
/// Used by `GraphTraversalRepo` expand methods (A1 Task 7).
#[derive(Debug, Clone)]
pub struct GraphNeighbour {
    pub pg_id: uuid::Uuid,
    pub space: Space,
    pub entity_type: String,
    pub name: String,
    pub hop: u8,
}

// ── Dedup verdict types ───────────────────────────────────────────────────────

/// A matching note found during deduplication check.
///
/// Moved from `akashic-curation::notes::dedup` (A1 Task 2).
#[derive(Debug)]
pub struct DedupMatch {
    pub note_id: uuid::Uuid,
    pub title: Option<String>,
    pub category: String,
    pub similarity: f64,
}

/// Verdict from the deduplication check.
///
/// Moved from `akashic-curation::notes::dedup` (A1 Task 2).
pub enum DedupVerdict {
    /// Similarity >= block threshold — nearly identical note exists.
    Block(DedupMatch),
    /// Similarity in warn range — similar note exists but not identical.
    Warn(DedupMatch),
    /// Similarity below warn threshold — safe to save.
    Pass,
}

// ── Chunk query row types for ingestion pipeline (A1 Task 8) ─────────────────

/// A chunk row loaded for CALLS-edge resolution in Stage 6.
///
/// Fields: `(id, name, module_path, chunk_type, fqn, parent_fqn, signature,
/// content)`
///
/// Used by `ChunkRepo::fetch_chunks_for_call_resolution` (A1 Task 8).
/// `signature` (D1c-1) is threaded into `ChunkIndex::build_from` to populate
/// `by_return_type` via `parse_return_type`. `content` (D1c-2) is threaded in
/// the same way to populate `by_field_type` via `parse_struct_fields` for
/// STRUCT chunks (`content` is `NOT NULL` in the `chunks` table — no
/// `Option` wrapper).
#[derive(Debug, Clone)]
pub struct ChunkCallResolutionRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub module_path: String,
    pub chunk_type: String,
    pub fqn: Option<String>,
    pub parent_fqn: Option<String>,
    pub signature: Option<String>,
    pub content: String,
}

/// A chunk row for Go structural interface-satisfaction detection in Stage 6.
///
/// Fields: `(id, name, chunk_type, content, module_path)`
///
/// Used by `ChunkRepo::fetch_go_chunks` (A1 Task 8).
#[derive(Debug, Clone)]
pub struct GoChunkRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub chunk_type: String,
    pub content: String,
    pub module_path: String,
}

/// A chunk row loaded for entry-point detection in Stage 7.
///
/// Fields: `(id, name, module_path, content, language)`
///
/// Used by `ChunkRepo::fetch_chunks_for_entry_points` (A1 Task 8).
#[derive(Debug, Clone)]
pub struct EntryPointChunkRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub module_path: String,
    pub content: String,
    pub language: Option<String>,
}

// ── Ingest edge domain types (A1 Task 8) ─────────────────────────────────────

/// A resolved edge between two chunks, used for CALLS / REFERENCES /
/// IMPLEMENTS / ROUTES_TO Neo4j edges.
///
/// A domain-only type that mirrors `akashic_extraction::resolution::ResolvedEdge`
/// without the extraction-crate dependency in `akashic-domain`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestEdge {
    pub src_chunk_id: uuid::Uuid,
    pub tgt_chunk_id: uuid::Uuid,
    pub confidence: f32,
    pub method: String,
    pub ref_kind: Option<String>,
}

// ── Flow persistence domain types (A1 Task 8) ────────────────────────────────

/// A complete execution flow from an entry point, ready for Neo4j persistence.
///
/// Used by `FlowGraphRepo::store_flows` (A1 Task 8).
/// Mirrors `akashic-ingestion::ingestion::flows::ExecutionFlow` without the
/// extraction-crate dependency in `akashic-domain`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FlowRecord {
    pub flow_id: String,
    pub entry_chunk_id: uuid::Uuid,
    pub display_name: String,
    pub entry_type: String,
    pub repo_name: String,
    pub step_count: usize,
    pub truncated: bool,
    pub steps: Vec<FlowStepRecord>,
}

/// A single step in an execution flow, for Neo4j persistence.
///
/// Used by `FlowGraphRepo::store_flows` (A1 Task 8).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FlowStepRecord {
    pub chunk_id: uuid::Uuid,
    pub position: u32,
    pub depth: u32,
}
