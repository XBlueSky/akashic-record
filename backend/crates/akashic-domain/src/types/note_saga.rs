// ── Note health value types ───────────────────────────────────────────────────

/// Staleness data for one note, produced by the health-check pipeline.
///
/// Moved from `akashic-curation::notes::health` (A1 Task 2).
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteHealthEntry {
    pub id: uuid::Uuid,
    pub title: String,
    pub staleness_score: f64,
    pub staleness_reasons: Vec<String>,
    pub access_count: i32,
    pub last_accessed: Option<String>,
    pub suggestion: String,
}

/// Repository-level note health summary.
///
/// Moved from `akashic-curation::notes::health` (A1 Task 2).
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteHealthSummary {
    pub total_notes: i64,
    pub healthy: i64,
    pub needs_review: i64,
    pub likely_stale: i64,
    pub archived: i64,
    pub stale_notes: Vec<NoteHealthEntry>,
}

// ── Note repo row types (raw DB rows returned by NoteRepo/NoteHealthRepo) ────

/// A candidate note returned by the dedup similarity query.
///
/// Moved from `akashic-curation::notes::dedup` (A1 Task 4).
#[derive(Debug, Clone)]
pub struct DedupCandidate {
    pub note_id: uuid::Uuid,
    pub title: Option<String>,
    pub category: String,
    pub similarity: f64,
    pub saga_id: Option<uuid::Uuid>,
}

/// A note with its related-symbol list, used by staleness detection.
///
/// Moved from `akashic-curation::notes::health` (A1 Task 4).
#[derive(Debug, Clone)]
pub struct NoteWithSymbols {
    pub id: uuid::Uuid,
    pub symbols: Vec<String>,
}

/// Raw staleness row from the DB (before mapping to `NoteHealthEntry`).
///
/// Moved from `akashic-curation::notes::health` (A1 Task 4).
#[derive(Debug, Clone)]
pub struct StaleNoteDbRow {
    pub id: uuid::Uuid,
    pub title: Option<String>,
    pub staleness_score: f64,
    pub staleness_reasons: Vec<String>,
    pub access_count: Option<i32>,
    pub last_accessed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A note row for the L1 "top notes by access" memory-stack query.
///
/// Moved from `akashic-curation::notes::memory_stack` (A1 Task 4).
#[derive(Debug, Clone)]
pub struct TopNoteDbRow {
    pub title: Option<String>,
    pub category: String,
    pub summary: Option<String>,
    pub access_count: Option<i32>,
}

/// A note row in a saga's chronological timeline.
///
/// Moved from `akashic-curation::notes::memory_stack` (A1 Task 4).
#[derive(Debug, Clone)]
pub struct SagaNoteDbRow {
    pub title: Option<String>,
    pub category: String,
    pub summary: Option<String>,
    pub ts: chrono::DateTime<chrono::Utc>,
    pub is_superseded: bool,
}

// ── Saga domain row types ─────────────────────────────────────────────────────

/// A knowledge saga row returned from the `sagas` table.
///
/// Moved from `akashic-curation::notes::saga` (A1 Task 5).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SagaDbRow {
    pub id: uuid::Uuid,
    pub repo_name: String,
    pub name: Option<String>,
    pub source_type: Option<String>,
    pub source_ref: Option<String>,
    pub summary: Option<String>,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A saga with its note count.
///
/// Moved from `akashic-curation::notes::saga` (A1 Task 5).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SagaWithCount {
    #[serde(flatten)]
    pub saga: SagaDbRow,
    pub note_count: i64,
}

/// A timeline entry (note row) within a saga.
///
/// Moved from `akashic-curation::notes::saga` (A1 Task 5).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SagaTimelineEntry {
    pub uuid: String,
    pub title: Option<String>,
    pub category: String,
    pub summary: Option<String>,
    pub valid_at: Option<chrono::DateTime<chrono::Utc>>,
    pub invalid_at: Option<chrono::DateTime<chrono::Utc>>,
    pub superseded_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Result of an idempotency lookup in the executor saga table.
///
/// Used by `SagaExecutorRepo::find_idempotent_saga` (A1 Task 5).
#[derive(Debug, Clone)]
pub struct SagaIdempotencyRecord {
    pub id: uuid::Uuid,
    pub status: String,
    pub result_json: Option<serde_json::Value>,
}
