/// A single code-chunk row returned from the PG `chunks` table.
///
/// `fqn`/`parent_fqn` (Roadmap F, Task 7.5): added for `list_chunks_in_module_all`'s
/// ingestion-pipeline caller (Stage 4), which needs both to build the union
/// `ChunkIndexEntry` Stage 6/7 resolve from (see `IngestAccumulator::
/// chunk_index_source`'s doc comment for the full rationale). Low blast
/// radius: `ChunkRow` is also returned by `list_chunks_in_module` (the
/// paginated HTTP module-chunks listing), whose SELECT doesn't fetch either
/// column — that caller sets both to `None` rather than paying for an unused
/// SELECT. Every other consumer of `ChunkRow` (the HTTP `ChunkItem` DTO
/// builder, `gs_get_module_detail`, `gs_get_module_call_graph`) accesses
/// fields by name, not by exhaustive struct pattern, so adding fields here is
/// backward compatible for all of them.
#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub chunk_type: String,
    pub signature: Option<String>,
    pub content: String,
    pub language: Option<String>,
    pub fqn: Option<String>,
    pub parent_fqn: Option<String>,
}

/// Full detail for a single chunk (used by the chunk-detail endpoint).
#[derive(Debug, Clone)]
pub struct ChunkDetailRow {
    pub id: uuid::Uuid,
    pub repo_name: String,
    pub module_path: String,
    pub name: String,
    pub chunk_type: String,
    pub signature: Option<String>,
    pub content: String,
    pub language: Option<String>,
    pub git_ref: Option<String>,
    pub ingested_at: Option<String>,
}

/// A search hit from chunk vector or BM25 search.
#[derive(Debug, Clone)]
pub struct ChunkSearchRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub chunk_type: String,
    pub score: f32,
}

/// A module row (list view).
#[derive(Debug, Clone)]
pub struct ModuleRow {
    pub id: uuid::Uuid,
    pub path: String,
    pub language: Option<String>,
    pub summary: Option<String>,
    pub exports_count: Option<i32>,
    pub file_count: Option<i32>,
}

/// Module summary for graphrag context assembly.
#[derive(Debug, Clone)]
pub struct ModuleSummary {
    pub path: String,
    pub summary: Option<String>,
    pub language: Option<String>,
}

/// A chunk definition tuple (name, signature, fqn) for symbol resolution.
#[derive(Debug, Clone)]
pub struct ChunkDefinition {
    pub name: String,
    pub signature: Option<String>,
    pub fqn: Option<String>,
}

/// Language distribution entry.
#[derive(Debug, Clone)]
pub struct LanguageCount {
    pub language: String,
    pub count: i64,
}

// ── Ingestion row for bulk module insert ─────────────────────────────────────

/// A single module entry for bulk insert from the ingestion pipeline.
#[derive(Debug, Clone)]
pub struct ModuleIngestionRow {
    pub repo_name: String,
    pub path: String,
    pub language: Option<String>,
    pub summary: Option<String>,
    pub exports_count: Option<i32>,
    pub file_count: Option<i32>,
    pub is_virtual: bool,
    pub git_ref: Option<String>,
}

// ── Doc cluster row ───────────────────────────────────────────────────────────

/// A `doc_clusters` row (id, name, section_count, repo_name).
///
/// Used by GraphService::get_doc_graph / get_cluster_detail.
#[derive(Debug, Clone)]
pub struct DocClusterRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub section_count: i32,
    pub repo_name: Option<String>,
}

// ── Module graph PG row ───────────────────────────────────────────────────────

/// PG module metadata for the module graph (language, is_virtual).
///
/// Used by GraphService::get_module_graph to decorate Neo4j module nodes.
#[derive(Debug, Clone)]
pub struct ModuleMetaRow {
    pub id: uuid::Uuid,
    pub language: Option<String>,
    pub is_virtual: bool,
}

// ── Document section row ──────────────────────────────────────────────────────

/// A single section row for document/cluster detail display.
#[derive(Debug, Clone)]
pub struct SectionRow {
    pub id: uuid::Uuid,
    pub parent_id: Option<uuid::Uuid>,
    pub heading: String,
    pub content: String,
    pub depth: i16,
    pub tags: Option<Vec<String>>,
}

/// Document metadata for detail view.
#[derive(Debug, Clone)]
pub struct DocumentMetaRow {
    pub id: uuid::Uuid,
    pub title: String,
    pub doc_type: String,
    pub source_url: Option<String>,
}

// ── Note detail row for batch fetch ──────────────────────────────────────────

/// Note fields needed for module-detail / call-graph displays.
#[derive(Debug, Clone)]
pub struct NoteDetailRow {
    pub id: uuid::Uuid,
    pub title: Option<String>,
    pub category: String,
    pub summary: Option<String>,
}

// ── Chunk meta for ghost-node resolution ─────────────────────────────────────

/// Chunk type + module_path, used to resolve ghost node metadata in call-graph.
#[derive(Debug, Clone)]
pub struct ChunkModuleRow {
    pub id: uuid::Uuid,
    pub chunk_type: String,
    pub module_path: String,
}

// ── Latest note brief for project summary ────────────────────────────────────

/// A brief note entry used by get_project_summary.
#[derive(Debug, Clone)]
pub struct NoteBriefRow {
    pub id: uuid::Uuid,
    pub category: String,
    pub branch: Option<String>,
    pub created_at: Option<String>,
    pub snippet: String,
}
