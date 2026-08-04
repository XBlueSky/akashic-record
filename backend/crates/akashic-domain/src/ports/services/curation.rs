use super::*;

// ── CurationService (14 methods) ──────────────────────────────────────────────

/// Use-case port for note curation, health monitoring, saga management, and
/// saga-pattern orchestration.
///
/// Covers: note CRUD (save / list / get / update / delete / supersede),
/// note health analysis (staleness detection, health summary), saga lifecycle
/// (find-or-create, list, timeline), and saga-pattern execution (run, cleanup).
///
/// **A1 note:** note-CRUD methods whose orchestration is still inline in
/// `akashic-server` handlers (embedding generation, graph writes, idempotency
/// reservation) are stubbed with `anyhow::bail!` labeled `(A2)`.  The
/// trait compiles and enforces the port contract; A2 will migrate each handler.
/// The six app-resident use cases (health, staleness, saga ops) have **full
/// impls** in `akashic-curation::services` delegating to existing functions.
#[async_trait]
pub trait CurationService: Send + Sync {
    // ── Note CRUD (A2 stubs — handlers remain in akashic-server) ──────────

    /// Validate, deduplicate, embed, persist, and graph-link a new note.
    ///
    /// Returns the new note id AND the saga it resolved to, so the caller can
    /// create the Neo4j `PART_OF` edge without re-resolving the saga (that edge
    /// write is deferred to the caller because it needs an `EdgeRepo` this
    /// service doesn't hold).
    ///
    /// MCP: `save_note`
    /// HTTP: `POST /api/v1/repos/:name/notes` (note create path)
    async fn save_note(&self, req: SaveNoteRequest) -> crate::DomainResult<SavedNote>;

    /// List notes for a repo with optional category filter (paginated).
    ///
    /// Returns `(total_count, page_of_items)`. `offset`/`limit` are the
    /// already-clamped pagination bounds computed by the caller (input
    /// clamping is an interface concern).
    ///
    /// HTTP: `GET /api/v1/repos/:name/notes`
    async fn list_notes(
        &self,
        repo: String,
        branch: Option<String>,
        category: Option<String>,
        offset: i64,
        limit: i64,
    ) -> crate::DomainResult<(i64, Vec<NoteListItem>)>;

    /// Fetch a single note by ID.
    ///
    /// HTTP: `GET /api/v1/repos/:name/notes/:uuid`
    async fn get_note(&self, repo: String, note_id: uuid::Uuid) -> crate::DomainResult<NoteDetail>;

    /// Apply a partial patch to a note (title, content, tags, etc.).
    ///
    /// HTTP: `PUT /api/v1/repos/:name/notes/:uuid`
    async fn update_note(
        &self,
        repo: String,
        note_id: uuid::Uuid,
        patch: NotePatch,
    ) -> crate::DomainResult<()>;

    /// Hard-delete a note and its graph node.
    ///
    /// HTTP: `DELETE /api/v1/repos/:name/notes/:uuid`
    async fn delete_note(&self, repo: String, note_id: uuid::Uuid) -> crate::DomainResult<()>;

    /// Mark `old_note_id` as superseded by `new_note_id`.
    ///
    /// MCP: `supersede_note`
    async fn supersede_note(
        &self,
        old_note_id: uuid::Uuid,
        new_note_id: uuid::Uuid,
    ) -> crate::DomainResult<()>;

    // ── Note health (full impls — app-resident in akashic-curation) ────────

    /// Compute staleness scores for all notes in a repo and auto-archive stale
    /// ones (staleness > 0.9 + access_count == 0 + age > 90 days).
    ///
    /// HTTP: internal / Stage-8 pipeline call.
    async fn detect_staleness(&self, repo: String) -> crate::DomainResult<()>;

    /// Return an aggregated health summary for a repo.
    ///
    /// HTTP: `GET /api/v1/repos/:name/notes/health`
    /// MCP: `get_note_health`
    async fn get_note_health(
        &self,
        repo: String,
        min_staleness: f64,
    ) -> crate::DomainResult<crate::types::NoteHealthSummary>;

    // ── Saga lifecycle (full impls — app-resident in akashic-curation) ─────

    /// Find or create the saga that corresponds to an issue ref, branch, or
    /// manual topic. Returns the saga UUID.
    ///
    /// Internal: used by `save_note` saga resolution.
    async fn find_or_create_saga(
        &self,
        repo: String,
        source_type: String,
        source_ref: Option<String>,
        name: Option<String>,
    ) -> crate::DomainResult<uuid::Uuid>;

    /// List knowledge sagas for a repo with optional status filter.
    ///
    /// HTTP: `GET /api/v1/repos/:name/sagas`
    /// MCP: `list_sagas`
    async fn list_sagas(
        &self,
        repo: String,
        status: Option<String>,
    ) -> crate::DomainResult<Vec<crate::types::SagaWithCount>>;

    /// Get a saga and its notes timeline.
    ///
    /// HTTP: `GET /api/v1/repos/:name/sagas/:id`
    /// MCP: `get_saga_timeline`
    async fn get_saga_timeline(
        &self,
        saga_id: uuid::Uuid,
    ) -> crate::DomainResult<(
        crate::types::SagaDbRow,
        Vec<crate::types::SagaTimelineEntry>,
    )>;

    // ── Saga-pattern orchestration (full impls) ────────────────────────────

    /// Delete saga records older than `stale_minutes`.
    ///
    /// Returns the number of rows deleted.
    ///
    /// Internal: startup + periodic cleanup timer.
    async fn cleanup_stale_sagas(&self, stale_minutes: i64) -> crate::DomainResult<i64>;

    /// Delete idempotency keys older than `ttl_hours`.
    ///
    /// Returns the number of rows deleted.
    ///
    /// Internal: periodic cache cleanup timer.
    async fn cleanup_expired_idempotency(&self, ttl_hours: i64) -> crate::DomainResult<i64>;
}

// ── CurationService domain types ──────────────────────────────────────────────

/// Input bundle for `CurationService::save_note`.
///
/// Consolidates the 14 scalar parameters so the trait method stays dyn-safe and
/// avoids the `clippy::too_many_arguments` limit (7).
#[derive(Debug, Clone)]
pub struct SaveNoteRequest {
    pub repo: String,
    pub branch: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub facts: Vec<String>,
    pub related_symbols: Vec<String>,
    pub related_files: Vec<String>,
    pub tags: Vec<String>,
    pub issue_ref: Option<String>,
    pub topic: Option<String>,
    pub supersedes: Option<uuid::Uuid>,
    pub skip_dedup: bool,
}

/// Outcome of [`CurationService::save_note`]: the persisted note plus the saga
/// it was resolved into (if any), so the caller can link the `PART_OF` edge
/// without re-running saga resolution.
#[derive(Debug, Clone, Copy)]
pub struct SavedNote {
    pub note_id: uuid::Uuid,
    pub saga_id: Option<uuid::Uuid>,
}

/// A note entry in the `list_notes` response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteListItem {
    pub id: uuid::Uuid,
    pub title: Option<String>,
    pub category: String,
    pub summary: Option<String>,
    /// `None` preserves a NULL `tags` column (distinct from an empty array).
    pub tags: Option<Vec<String>>,
    pub saga_id: Option<uuid::Uuid>,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub access_count: i32,
}

/// Full note detail returned by `get_note`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteDetail {
    pub id: uuid::Uuid,
    pub repo: String,
    pub branch: String,
    pub category: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content: String,
    pub facts: Vec<String>,
    pub related_symbols: Vec<String>,
    pub related_files: Vec<String>,
    pub tags: Vec<String>,
    pub saga_id: Option<uuid::Uuid>,
    pub supersedes: Option<uuid::Uuid>,
    pub is_superseded: bool,
    pub access_count: i32,
    pub created_at: String,
    pub updated_at: Option<String>,
}

/// Partial patch for `update_note` — all fields optional.
#[derive(Debug, Clone, Default)]
pub struct NotePatch {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub facts: Option<Vec<String>>,
    pub related_symbols: Option<Vec<String>>,
    pub related_files: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub category: Option<String>,
    pub supersedes: Option<uuid::Uuid>,
}
