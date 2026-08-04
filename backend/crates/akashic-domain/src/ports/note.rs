//! Port traits for note persistence (PostgreSQL side) and note graph (Neo4j side).
//!
//! `NoteRepo` covers the full note lifecycle: CRUD, dedup, staleness scan, and
//! search.  `NoteHealthRepo` covers the health / analytics read surface.
//! `NoteGraphRepo` covers the lightweight Neo4j note-node writes: create, update
//! properties, and detach-delete.
//!
//! All traits are infra-free: they use only domain types from
//! `akashic_domain::types`.  PG adapters live in `akashic-store-pg`;
//! the Neo4j adapter lives in `akashic-store-neo4j`.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{
    DedupCandidate, NoteBriefRow, NoteContentRow, NoteDetailRow, NoteFullRow, NoteSearchRow,
    NoteWithSymbols, SagaNoteDbRow, ScoutNoteRow, StaleNoteDbRow, TopNoteDbRow,
};

// ── NoteInsertRow — input bundle for NoteRepo::insert_note ───────────────────

/// Input bundle for [`NoteRepo::insert_note`].
///
/// Consolidates all scalar columns so the trait method has a single argument.
/// `embedding` carries the pre-computed pgvector vector as raw `f32` values;
/// the PG adapter converts it to `pgvector::Vector`.
#[derive(Debug, Clone)]
pub struct NoteInsertRow {
    pub repo_name: String,
    pub branch: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub facts: Vec<String>,
    pub related_symbols: Vec<String>,
    pub related_files: Vec<String>,
    pub tags: Vec<String>,
    pub saga_id: Option<Uuid>,
    pub embedding: Vec<f32>,
}

// ── NotePatchRow — input bundle for NoteRepo::update_note ────────────────────

/// Partial patch input for [`NoteRepo::update_note`].
///
/// `None` means "leave the column unchanged".  `embedding` is `None` when the
/// embedder was unavailable during update and we skip re-embedding.
#[derive(Debug, Clone, Default)]
pub struct NotePatchRow {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub embedding: Option<Vec<f32>>,
}

// ── NoteCrudRow — result row for NoteRepo::fetch_note_by_id ──────────────────

/// Full note CRUD row returned by [`NoteRepo::fetch_note_by_id`].
#[derive(Debug, Clone)]
pub struct NoteCrudRow {
    pub id: Uuid,
    pub repo_name: String,
    pub branch: Option<String>,
    pub category: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content: String,
    pub facts: Option<Vec<String>>,
    pub related_symbols: Option<Vec<String>>,
    pub related_files: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub saga_id: Option<Uuid>,
    pub supersedes: Option<Uuid>,
    pub is_superseded: bool,
    pub access_count: Option<i32>,
    pub created_at: String,
    pub updated_at: Option<String>,
}

// ── NoteListCrudRow — result row for NoteRepo::list_notes_paginated ──────────

/// Lightweight note list row returned by [`NoteRepo::list_notes_paginated`].
#[derive(Debug, Clone)]
pub struct NoteListCrudRow {
    pub id: Uuid,
    pub title: Option<String>,
    pub category: String,
    pub summary: Option<String>,
    pub tags: Option<Vec<String>>,
    pub saga_id: Option<Uuid>,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub access_count: Option<i32>,
}

// ── NoteRepo ─────────────────────────────────────────────────────────────────

/// Repository contract for note lifecycle + similarity search.
///
/// All methods return `anyhow::Result` — error-type unification is deferred to
/// a later slice.  Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn NoteRepo>`.
#[async_trait]
pub trait NoteRepo: Send + Sync {
    /// Find the most similar note in the same repo by embedding cosine distance.
    ///
    /// Returns the top-1 candidate (if any valid note exists), including its
    /// similarity score and saga membership — used by the dedup check.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::dedup::check_dedup`.
    async fn find_similar_by_embedding(
        &self,
        repo_name: &str,
        embedding: &[f32],
    ) -> anyhow::Result<Option<DedupCandidate>>;

    /// Fetch all non-archived notes with their related-symbol lists.
    ///
    /// Used by `detect_staleness` to iterate and check each note's symbols.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::detect_staleness`.
    async fn get_all_with_symbols(&self, repo_name: &str) -> anyhow::Result<Vec<NoteWithSymbols>>;

    /// Check whether a symbol name exists in the chunks table for a repo.
    ///
    /// Returns the count (0 = deleted, >0 = still present).
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::detect_staleness`.
    async fn check_symbol_existence(
        &self,
        repo_name: &str,
        symbol_name: &str,
    ) -> anyhow::Result<i64>;

    /// Compute the average cosine divergence between a note's embedding and the
    /// embeddings of its related chunks.
    ///
    /// Returns `None` when no chunks match or embeddings are NULL.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::detect_staleness`.
    async fn compute_embedding_divergence(
        &self,
        note_id: Uuid,
        repo_name: &str,
        symbol_names: &[String],
    ) -> anyhow::Result<Option<f64>>;

    /// Vector (cosine) search over note embeddings.
    ///
    /// SQL moved verbatim from `akashic-retrieval::graphrag::mod.rs`
    /// `search_human_space` (A1 Task 7).
    async fn search_notes_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<NoteSearchRow>>;

    /// Scout vector search returning the display columns (category, title,
    /// summary, per-row repo name) the unified `GET /api/v1/search` endpoint
    /// needs.
    ///
    /// SQL moved verbatim from `akashic-server::api::routes::search::unified_search`
    /// (note layer). Unlike [`search_notes_by_vector`] it does NOT exclude
    /// archived notes — preserving the Scout endpoint's behaviour.
    async fn scout_search_notes_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ScoutNoteRow>>;

    /// BM25 full-text search over notes.
    ///
    /// SQL moved verbatim from `akashic-retrieval::graphrag::mod.rs`
    /// `search_note_bm25` (A1 Task 7).
    async fn search_notes_by_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<NoteSearchRow>>;

    /// Fetch (content, author, category) for a single note by id.
    ///
    /// SQL moved verbatim from `akashic-retrieval::graphrag::mod.rs`
    /// `fetch_full_node` Human branch (A1 Task 7).
    async fn fetch_note_content(&self, note_id: Uuid) -> anyhow::Result<Option<NoteContentRow>>;

    /// Batch-fetch (id, title, category, summary) for a set of note ids.
    ///
    /// SQL: `SELECT id, title, category, summary FROM notes WHERE id = ANY($1)`.
    /// Used by GraphService::get_module_detail to batch-load note metadata for
    /// a module without N+1 per-note queries.
    async fn fetch_notes_detail_batch(
        &self,
        note_ids: &[Uuid],
    ) -> anyhow::Result<Vec<NoteDetailRow>>;

    /// Batch-fetch (id, title, category) for a set of note ids.
    ///
    /// SQL: `SELECT id, title, category FROM notes WHERE id = ANY($1)`.
    /// Used by GraphService chunk-note detail (simpler shape than fetch_notes_detail_batch).
    async fn fetch_notes_brief_batch(
        &self,
        note_ids: &[Uuid],
    ) -> anyhow::Result<Vec<NoteDetailRow>>;

    /// Batch-fetch full note rows for the `get_details` endpoint.
    ///
    /// SQL (verbatim from `GET /api/v1/details` handler):
    /// `SELECT id, title, summary, facts, content, category,
    ///         related_symbols, related_files, branch
    ///  FROM notes WHERE id = ANY($1)`.
    async fn fetch_notes_full_batch(&self, note_ids: &[Uuid]) -> anyhow::Result<Vec<NoteFullRow>>;

    // ── CRUD methods (A2a) ────────────────────────────────────────────────────

    /// Insert a new note and return the generated UUID.
    ///
    /// SQL (verbatim from MCP `save_note`):
    /// `INSERT INTO notes (repo_name, branch, category, title, summary, content, facts,
    ///  author, embedding, related_symbols, related_files, tags, saga_id)
    ///  VALUES ($1, $2, $3, $4, $5, $6, $7, 'ai', $8, $9, $10, $11, $12)
    ///  RETURNING id`.
    async fn insert_note(&self, row: NoteInsertRow) -> anyhow::Result<Uuid>;

    /// List notes with optional branch/category filter, paginated.
    ///
    /// Returns `(total_count, rows)`.
    ///
    /// SQL (verbatim from `GET /api/v1/repos/:name/notes` handler):
    /// `SELECT count(*) …` + `SELECT id, title, … ORDER BY created_at DESC OFFSET $4 LIMIT $5`.
    async fn list_notes_paginated(
        &self,
        repo_name: &str,
        branch: Option<&str>,
        category: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> anyhow::Result<(i64, Vec<NoteListCrudRow>)>;

    /// Fetch a single note by (id, repo_name).
    ///
    /// Returns `None` when the note does not exist or belongs to a different repo.
    ///
    /// SQL (verbatim from `GET /api/v1/repos/:name/notes/:uuid` handler):
    /// `SELECT id, title, summary, content, category, branch, repo_name, author,
    ///  to_char(created_at, …), tags, facts, related_symbols, related_files
    ///  FROM notes WHERE id = $1 AND repo_name = $2`.
    async fn fetch_note_by_id(
        &self,
        note_id: Uuid,
        repo_name: &str,
    ) -> anyhow::Result<Option<NoteCrudRow>>;

    /// Apply a partial patch to a note (content columns + optional embedding).
    ///
    /// Returns the number of rows affected (0 = not found).
    ///
    /// SQL (verbatim from `PUT /api/v1/repos/:name/notes/:uuid` handler,
    /// two variants: with embedding and without):
    /// `UPDATE notes SET title=$1, summary=$2, content=$3, category=$4, tags=$5
    ///  [, embedding=$6] , updated_at=now() WHERE id=$N AND repo_name=$M`.
    async fn update_note(
        &self,
        note_id: Uuid,
        repo_name: &str,
        patch: NotePatchRow,
    ) -> anyhow::Result<u64>;

    /// Hard-delete a note by (id, repo_name).
    ///
    /// Returns the number of rows affected (0 = not found).
    ///
    /// SQL (verbatim from `DELETE /api/v1/repos/:name/notes/:uuid` handler):
    /// `DELETE FROM notes WHERE id = $1::uuid AND repo_name = $2`.
    async fn delete_note(&self, note_id: Uuid, repo_name: &str) -> anyhow::Result<u64>;

    /// Check whether a note exists by UUID (repo-agnostic).
    ///
    /// SQL (verbatim from MCP `supersede_note`):
    /// `SELECT EXISTS(SELECT 1 FROM notes WHERE id = $1)`.
    async fn note_exists(&self, note_id: Uuid) -> anyhow::Result<bool>;

    /// Mark `old_id` as superseded by `new_id`.
    ///
    /// Returns the number of rows affected.
    ///
    /// SQL (verbatim from MCP `supersede_note` + inline save_note supersedes path):
    /// `UPDATE notes SET invalid_at = now(), superseded_by = $1 WHERE id = $2`.
    async fn mark_superseded(&self, old_id: Uuid, new_id: Uuid) -> anyhow::Result<u64>;

    /// Undo [`mark_superseded`] for `old_id` — the compensating write used by
    /// `CurationService::supersede_note` when the Neo4j `SUPERSEDES` mirror
    /// write fails.
    ///
    /// Returns the number of rows affected.
    ///
    /// SQL (exact inverse of `mark_superseded`):
    /// `UPDATE notes SET invalid_at = NULL, superseded_by = NULL WHERE id = $1`.
    async fn unmark_superseded(&self, old_id: Uuid) -> anyhow::Result<u64>;
}

// ── NoteGraphRepo ─────────────────────────────────────────────────────────────

/// Repository contract for note graph writes (Neo4j side).
///
/// Covers the four lightweight Note-node mutations that were previously inline
/// in `akashic-server::mcp::tools` and `akashic-server::api::routes::notes`
/// (plus `create_supersedes_edge`, added for the ADR-as-graph `SUPERSEDES`
/// mirror).
///
/// `CurationService::save_note` uses `create_note_node` + `detach_delete_note`
/// (the compensating rollback path).  `update_note` uses `update_note_node`.
/// `delete_note` uses `detach_delete_note`. `supersede_note` uses
/// `create_supersedes_edge`, compensating via `NoteRepo::unmark_superseded` on
/// failure (the same both-or-neither guarantee).
///
/// All Cypher is moved verbatim from the original call-sites.
#[async_trait]
pub trait NoteGraphRepo: Send + Sync {
    /// Create a Note node in Neo4j and link it to its Branch, Repository, and
    /// Category nodes.
    ///
    /// Cypher (verbatim from MCP `save_note`):
    /// ```cypher
    /// MERGE (r:Repository {name: $repo_name})
    /// MERGE (r)-[:HAS_BRANCH]->(b:Branch {name: $branch_name})
    /// CREATE (c:Note {pg_id: $pg_id, repo_name: $repo_name})
    /// CREATE (c)-[:LINKED_TO]->(b)
    /// CREATE (c)-[:BELONGS_TO]->(r)
    /// WITH c
    /// MATCH (cat:Category {name: $category})
    /// CREATE (c)-[:TAGGED_AS]->(cat)
    /// ```
    ///
    /// Returns `Err` on Neo4j failure — the service layer compensates by
    /// deleting the PG row.
    async fn create_note_node(
        &self,
        pg_id: Uuid,
        repo_name: &str,
        branch_name: &str,
        category: &str,
    ) -> anyhow::Result<()>;

    /// Update mutable properties on a Note node.
    ///
    /// Cypher (verbatim from REST `update_note`):
    /// `MATCH (n:Note {pg_id: $uuid}) SET n.title = $title, n.category = $category`.
    ///
    /// Non-fatal if the node is absent (may have been cleaned up independently).
    async fn update_note_node(
        &self,
        pg_id: Uuid,
        title: &str,
        category: &str,
    ) -> anyhow::Result<()>;

    /// Detach-delete a Note node and all its relationships.
    ///
    /// Cypher (verbatim from REST `delete_note`):
    /// `MATCH (n:Note {pg_id: $uuid}) DETACH DELETE n`.
    ///
    /// Non-fatal if the node is absent.
    async fn detach_delete_note(&self, pg_id: Uuid) -> anyhow::Result<()>;

    /// Mirror `notes.superseded_by` as a `SUPERSEDES` edge: the NEWER note
    /// points at the OLDER one (`(new)-[:SUPERSEDES]->(old)` reads "new
    /// supersedes old"), matching the Postgres semantics
    /// (`old.superseded_by = new.id`). Idempotent (MERGE).
    ///
    /// Cypher:
    /// ```cypher
    /// MATCH (old:Note {pg_id: $old_id})
    /// MATCH (new:Note {pg_id: $new_id})
    /// MERGE (new)-[:SUPERSEDES]->(old)
    /// ```
    ///
    /// Returns `Err` on Neo4j failure — `CurationService::supersede_note`
    /// compensates by calling `NoteRepo::unmark_superseded`.
    async fn create_supersedes_edge(&self, old_id: Uuid, new_id: Uuid) -> anyhow::Result<()>;
}

// ── NoteHealthRepo ────────────────────────────────────────────────────────────

/// Repository contract for note health / analytics writes and reads.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn NoteHealthRepo>`.
#[async_trait]
pub trait NoteHealthRepo: Send + Sync {
    // ── Write ────────────────────────────────────────────────────────────────

    /// Update the staleness score and reasons for a note.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::detect_staleness`.
    async fn update_staleness(
        &self,
        note_id: Uuid,
        staleness_score: f64,
        reasons: &[String],
    ) -> anyhow::Result<()>;

    /// Auto-archive notes with staleness > 0.9, zero access, and age > 90 days.
    ///
    /// Returns the number of rows archived.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::detect_staleness`.
    async fn auto_archive_stale(&self, repo_name: &str) -> anyhow::Result<i64>;

    // ── Count reads ──────────────────────────────────────────────────────────

    /// Count all notes in a repo (including archived).
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::get_health_summary`.
    async fn count_total_notes(&self, repo_name: &str) -> anyhow::Result<i64>;

    /// Count archived notes in a repo.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::get_health_summary`.
    async fn count_archived_notes(&self, repo_name: &str) -> anyhow::Result<i64>;

    /// Count notes with staleness_score < 0.3 (healthy).
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::get_health_summary`.
    async fn count_healthy_notes(&self, repo_name: &str) -> anyhow::Result<i64>;

    /// Count notes with staleness_score in [0.3, 0.7) (needs review).
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::get_health_summary`.
    async fn count_needs_review_notes(&self, repo_name: &str) -> anyhow::Result<i64>;

    /// Count notes with staleness_score >= 0.7 (likely stale).
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::get_health_summary`.
    async fn count_stale_notes(&self, repo_name: &str) -> anyhow::Result<i64>;

    // ── Fetch reads ──────────────────────────────────────────────────────────

    /// Fetch notes with staleness_score >= `min_staleness`, ordered by score DESC.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::health::get_health_summary`.
    async fn get_stale_notes_above_threshold(
        &self,
        repo_name: &str,
        min_staleness: f64,
    ) -> anyhow::Result<Vec<StaleNoteDbRow>>;

    /// Notes counts grouped by category (valid, non-archived).
    ///
    /// SQL moved verbatim from `akashic-curation::notes::memory_stack::build_l0`.
    async fn get_notes_by_category(&self, repo_name: &str) -> anyhow::Result<Vec<(String, i64)>>;

    /// Top notes by access count (valid, non-archived, with title), up to `limit`.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::memory_stack::build_l1`.
    async fn get_top_notes_by_access(
        &self,
        repo_name: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<TopNoteDbRow>>;

    /// Count notes belonging to a saga.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::memory_stack::build_l2`.
    async fn count_notes_for_saga(&self, saga_id: Uuid) -> anyhow::Result<i64>;

    /// Notes in a saga ordered by COALESCE(valid_at, created_at) ASC.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::memory_stack::build_l2`.
    async fn get_saga_notes_timeline(&self, saga_id: Uuid) -> anyhow::Result<Vec<SagaNoteDbRow>>;

    /// Latest N notes for a repo with optional branch filter.
    ///
    /// SQL (verbatim from get_project_summary MCP tool):
    /// `SELECT id, category, branch, to_char(created_at, ...), left(content, 120)
    ///  FROM notes WHERE repo_name = $1 [AND branch = $2]
    ///  ORDER BY created_at DESC LIMIT $n`.
    async fn get_latest_notes(
        &self,
        repo_name: &str,
        branch: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<NoteBriefRow>>;

    /// Increment `access_count` and set `last_accessed_at = now()` for a note.
    ///
    /// SQL (verbatim from MCP search_knowledge access-tracking UPDATE):
    /// `UPDATE notes SET access_count = coalesce(access_count, 0) + 1,
    ///  last_accessed_at = now() WHERE id = $1`.
    ///
    /// Used by `SearchService::graphrag_query` (and formerly by the MCP
    /// `search_knowledge` tool) when a Human-space note appears in results.
    async fn increment_access_count(&self, note_id: Uuid) -> anyhow::Result<()>;
}
