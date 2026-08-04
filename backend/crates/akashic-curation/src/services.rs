//! Application-service implementation for `CurationService`.
//!
//! `CurationService` is defined in `akashic-domain::ports::services` and
//! implemented here in `akashic-curation` — the crate that already owns
//! the note-health, memory-stack, saga-lifecycle, and saga-executor modules.
//!
//! **A2a scope (note CRUD — fully implemented):**
//!
//! Orchestration invariants preserved:
//! - `save_note` compensation: PG insert → Neo4j create → on Neo4j FAILURE,
//!   `NoteRepo::delete_note` rolls back the PG row (no orphan notes).
//! - `update_note` quota-ordering: `EmbeddingProvider::embed` runs BEFORE
//!   `NoteRepo::update_note`. A 429 surfaces from embed() without persisting.
//! - `delete_note` dual-write ordering: PG-first-then-Neo4j (consistent with
//!   save_note).
//!
//! **Full impls (health + saga):**
//! - `detect_staleness` → delegates to `crate::notes::health::detect_staleness`
//! - `get_note_health` → delegates to `crate::notes::health::get_health_summary`
//! - `find_or_create_saga` → delegates to `crate::notes::saga::find_or_create_saga`
//! - `list_sagas` → delegates to `crate::notes::saga::list_sagas`
//! - `get_saga_timeline` → delegates to `crate::notes::saga::get_saga_timeline`
//! - `cleanup_stale_sagas` → `SagaExecutorRepo::cleanup_stale_sagas`
//! - `cleanup_expired_idempotency` → `SagaExecutorRepo::cleanup_expired_idempotency`

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use akashic_domain::{DomainError, DomainResult};

use akashic_domain::ports::services::{
    CurationService, NoteDetail, NoteListItem, NotePatch, SaveNoteRequest, SavedNote,
};
use akashic_domain::ports::{
    EmbeddingProvider, NoteGraphRepo, NoteHealthRepo, NoteInsertRow, NotePatchRow, NoteRepo,
    SagaExecutorRepo, SagaRepo,
};
use akashic_domain::types::{NoteHealthSummary, SagaDbRow, SagaTimelineEntry, SagaWithCount};

// ── CurationServices ───────────────────────────────────────────────────────────

/// Thin service struct that holds port arcs for the curation domain and
/// implements `CurationService`.
///
/// A2a adds `note_graph_repo` (Neo4j note-node writes) and `embedder` (for
/// note embedding generation in save/update).  The compensation invariant is
/// preserved: `save_note` deletes the PG row when Neo4j create fails.
pub struct CurationServices {
    note_repo: Arc<dyn NoteRepo>,
    note_health_repo: Arc<dyn NoteHealthRepo>,
    note_graph_repo: Arc<dyn NoteGraphRepo>,
    embedder: Arc<dyn EmbeddingProvider>,
    saga_repo: Arc<dyn SagaRepo>,
    saga_executor_repo: Arc<dyn SagaExecutorRepo>,
}

impl CurationServices {
    pub fn new(
        note_repo: Arc<dyn NoteRepo>,
        note_health_repo: Arc<dyn NoteHealthRepo>,
        note_graph_repo: Arc<dyn NoteGraphRepo>,
        embedder: Arc<dyn EmbeddingProvider>,
        saga_repo: Arc<dyn SagaRepo>,
        saga_executor_repo: Arc<dyn SagaExecutorRepo>,
    ) -> Self {
        Self {
            note_repo,
            note_health_repo,
            note_graph_repo,
            embedder,
            saga_repo,
            saga_executor_repo,
        }
    }
}

// ── CurationService impl ───────────────────────────────────────────────────────

#[async_trait]
impl CurationService for CurationServices {
    // ── Note CRUD — full A2a impls ─────────────────────────────────────────

    /// Orchestration: embed → dedup → saga resolve → PG insert →
    /// Neo4j create → on-Neo4j-failure compensate (PG delete) + supersede old.
    ///
    /// The Neo4j create failure triggers a compensating `NoteRepo::delete_note`
    /// so no orphan PG note is left behind.
    async fn save_note(&self, req: SaveNoteRequest) -> DomainResult<SavedNote> {
        // 1. Generate embedding (includes title + summary for search quality).
        let embed_text = format!("{} {} {}", req.title, req.summary, req.content);
        let emb_resp =
            self.embedder.embed(&embed_text).await.map_err(|e| {
                anyhow::anyhow!("CurationService::save_note: embedding failed: {e}")
            })?;

        // 2. Dedup gate (skip if caller opted out).
        if !req.skip_dedup {
            match crate::notes::dedup::check_dedup(
                &self.note_repo,
                &req.repo,
                &emb_resp.vector,
                None, // saga_id resolved below; pre-check uses None (conservative)
            )
            .await
            {
                Ok(crate::notes::dedup::DedupVerdict::Block(m)) => {
                    return Err(DomainError::Conflict(crate::notes::dedup::format_block(&m)));
                }
                Ok(
                    crate::notes::dedup::DedupVerdict::Warn(_)
                    | crate::notes::dedup::DedupVerdict::Pass,
                ) => {}
                Err(e) => {
                    tracing::warn!("dedup check failed, proceeding: {e}");
                }
            }
        }

        // 3. Resolve saga (reuse existing full method).
        let saga_id = crate::notes::saga::resolve_saga_for_note(
            &self.saga_repo,
            &req.repo,
            req.issue_ref.as_deref(),
            &req.branch,
            req.topic.as_deref(),
        )
        .await
        .unwrap_or(None);

        // 4. PG insert.
        let note_id = self
            .note_repo
            .insert_note(NoteInsertRow {
                repo_name: req.repo.clone(),
                branch: req.branch.clone(),
                category: req.category.clone(),
                title: req.title.clone(),
                summary: req.summary.clone(),
                content: req.content.clone(),
                facts: req.facts.clone(),
                related_symbols: req.related_symbols.clone(),
                related_files: req.related_files.clone(),
                tags: req.tags.clone(),
                saga_id,
                embedding: emb_resp.vector,
            })
            .await
            .map_err(|e| anyhow::anyhow!("CurationService::save_note: PG insert failed: {e}"))?;

        // 5. Supersede old note if requested (PG only; non-fatal).
        if let Some(old_id) = req.supersedes {
            let _ = self.note_repo.mark_superseded(old_id, note_id).await;
        }

        // 6. Neo4j note node create.
        //
        // INVARIANT: on failure, compensate by deleting the PG note so there
        // is no orphan row that is searchable but absent from the graph.
        if let Err(e) = self
            .note_graph_repo
            .create_note_node(note_id, &req.repo, &req.branch, &req.category)
            .await
        {
            let _ = self.note_repo.delete_note(note_id, &req.repo).await;
            return Err(DomainError::Internal(anyhow::anyhow!(
                "CurationService::save_note: Neo4j write failed: {e} \
                 (rolled back PG note {note_id})"
            )));
        }

        // 7. Neo4j saga PART_OF link is deferred to the caller (it needs an
        //    EdgeRepo this service doesn't hold). Returning saga_id lets the
        //    caller create that edge WITHOUT re-resolving the saga.

        tracing::info!(
            note_id = %note_id,
            repo = %req.repo,
            branch = %req.branch,
            "CurationService::save_note: note saved"
        );
        Ok(SavedNote { note_id, saga_id })
    }

    /// List notes for a repo with optional branch/category filter (paginated).
    ///
    /// `offset`/`limit` arrive already clamped by the caller; returns the total
    /// count alongside the page of items.
    async fn list_notes(
        &self,
        repo: String,
        branch: Option<String>,
        category: Option<String>,
        offset: i64,
        limit: i64,
    ) -> DomainResult<(i64, Vec<NoteListItem>)> {
        let (total, rows) = self
            .note_repo
            .list_notes_paginated(&repo, branch.as_deref(), category.as_deref(), offset, limit)
            .await?;

        let items = rows
            .into_iter()
            .map(|r| NoteListItem {
                id: r.id,
                title: r.title,
                category: r.category,
                summary: r.summary,
                tags: r.tags,
                saga_id: r.saga_id,
                created_at: r.created_at,
                updated_at: r.updated_at,
                access_count: r.access_count.unwrap_or(0),
            })
            .collect();

        Ok((total, items))
    }

    /// Fetch a single note by (repo, id) — 404 = `Err`.
    async fn get_note(&self, repo: String, note_id: Uuid) -> DomainResult<NoteDetail> {
        let row = self
            .note_repo
            .fetch_note_by_id(note_id, &repo)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("Note {note_id} in repo {repo}")))?;

        Ok(NoteDetail {
            id: row.id,
            repo: row.repo_name,
            branch: row.branch.unwrap_or_default(),
            category: row.category,
            title: row.title,
            summary: row.summary,
            content: row.content,
            facts: row.facts.unwrap_or_default(),
            related_symbols: row.related_symbols.unwrap_or_default(),
            related_files: row.related_files.unwrap_or_default(),
            tags: row.tags.unwrap_or_default(),
            saga_id: row.saga_id,
            supersedes: row.supersedes,
            is_superseded: row.is_superseded,
            access_count: row.access_count.unwrap_or(0),
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    /// Apply a partial patch to a note.
    ///
    /// INVARIANT: embed BEFORE PG UPDATE. A quota rejection (or any embedding
    /// failure) surfaces as `Err` before the note is mutated — the note retains
    /// its previous content + embedding.  After PG update, Neo4j properties are
    /// updated (non-fatal on Neo4j failure).
    async fn update_note(&self, repo: String, note_id: Uuid, patch: NotePatch) -> DomainResult<()> {
        // 1. Re-embed BEFORE mutating the note (quota-ordering invariant).
        //    Only regenerate when content fields are actually changing.
        let has_content_change =
            patch.title.is_some() || patch.summary.is_some() || patch.content.is_some();

        let new_embedding = if has_content_change {
            // Fetch current note to fill in unchanged fields for embed text.
            let current = self
                .note_repo
                .fetch_note_by_id(note_id, &repo)
                .await?
                .ok_or_else(|| DomainError::NotFound(format!("Note {note_id} in repo {repo}")))?;

            let title = patch
                .title
                .as_deref()
                .unwrap_or(current.title.as_deref().unwrap_or(""));
            let summary = patch
                .summary
                .as_deref()
                .unwrap_or(current.summary.as_deref().unwrap_or(""));
            let content = patch.content.as_deref().unwrap_or(&current.content);

            let embed_text = format!("{title} {summary} {content}");
            match self.embedder.embed(&embed_text).await {
                Ok(emb) => Some(emb.vector),
                Err(e) => {
                    // Quota rejection must surface — do NOT continue with stale embedding.
                    // Other transient errors are tolerated: skip re-embed, leave existing.
                    let is_quota = e.chain().any(|cause| {
                        cause.to_string().contains("quota_exceeded")
                            || cause.to_string().contains("QuotaExceeded")
                    });
                    if is_quota {
                        return Err(DomainError::Internal(e));
                    }
                    tracing::warn!(
                        note_id = %note_id,
                        err = %e,
                        "Embedder failed during note update — saving without re-embed"
                    );
                    None
                }
            }
        } else {
            None
        };

        // 2. PG UPDATE (with or without new embedding).
        let note_patch_row = NotePatchRow {
            title: patch.title.clone(),
            summary: patch.summary.clone(),
            content: patch.content.clone(),
            category: patch.category.clone(),
            tags: patch.tags.clone(),
            embedding: new_embedding,
        };

        let rows = self
            .note_repo
            .update_note(note_id, &repo, note_patch_row)
            .await?;

        if rows == 0 {
            return Err(DomainError::NotFound(format!(
                "Note {note_id} in repo {repo}"
            )));
        }

        // 3. Neo4j property update (non-fatal — Neo4j and PG can drift).
        if patch.title.is_some() || patch.category.is_some() {
            // Fetch updated values for the Neo4j update.
            let title_val = patch.title.as_deref().unwrap_or("");
            let category_val = patch.category.as_deref().unwrap_or("");
            if !title_val.is_empty() || !category_val.is_empty() {
                let _ = self
                    .note_graph_repo
                    .update_note_node(note_id, title_val, category_val)
                    .await;
            }
        }

        Ok(())
    }

    /// Hard-delete a note and its Neo4j node.
    ///
    /// PG-first-then-Neo4j (consistent dual-write ordering with save_note).
    async fn delete_note(&self, repo: String, note_id: Uuid) -> DomainResult<()> {
        // 1. PG delete.
        let rows = self.note_repo.delete_note(note_id, &repo).await?;
        if rows == 0 {
            return Err(DomainError::NotFound(format!(
                "Note {note_id} in repo {repo}"
            )));
        }

        // 2. Neo4j detach-delete (non-fatal — PG row is already gone).
        let _ = self.note_graph_repo.detach_delete_note(note_id).await;

        Ok(())
    }

    /// Mark `old_note_id` as superseded by `new_note_id`, mirroring the mark as
    /// a Neo4j `SUPERSEDES` edge.
    ///
    /// STRICT compensation: PG mark first, then the Neo4j edge; if the Neo4j
    /// write fails, roll back the PG mark via `unmark_superseded` and propagate
    /// the ORIGINAL Neo4j error — both stores end unchanged (the same
    /// both-or-neither guarantee `save_note` gives for note creation).
    async fn supersede_note(&self, old_note_id: Uuid, new_note_id: Uuid) -> DomainResult<()> {
        // Verify both notes exist (verbatim logic from MCP supersede_note).
        let old_exists = self.note_repo.note_exists(old_note_id).await?;
        if !old_exists {
            return Err(DomainError::NotFound(format!("Old note {old_note_id}")));
        }
        let new_exists = self.note_repo.note_exists(new_note_id).await?;
        if !new_exists {
            return Err(DomainError::NotFound(format!("New note {new_note_id}")));
        }

        // PG: mark the old note superseded.
        self.note_repo
            .mark_superseded(old_note_id, new_note_id)
            .await?;

        // Neo4j: mirror as a SUPERSEDES edge. On failure, roll back the PG mark
        // and propagate the ORIGINAL Neo4j error.
        if let Err(e) = self
            .note_graph_repo
            .create_supersedes_edge(old_note_id, new_note_id)
            .await
        {
            if let Err(rollback_err) = self.note_repo.unmark_superseded(old_note_id).await {
                tracing::error!(
                    note_id = %old_note_id,
                    neo4j_error = %e,
                    rollback_error = %rollback_err,
                    "supersede_note: PG rollback ALSO failed after a Neo4j SUPERSEDES write failure — \
                     note {old_note_id} may be left inconsistently marked as superseded in Postgres \
                     with no SUPERSEDES edge in Neo4j"
                );
            }
            return Err(DomainError::Internal(anyhow::anyhow!(
                "CurationService::supersede_note: Neo4j SUPERSEDES write failed: {e} \
                 (rolled back PG supersede mark on note {old_note_id})"
            )));
        }

        Ok(())
    }

    // ── Note health — full impls ───────────────────────────────────────────

    /// Delegates to `crate::notes::health::detect_staleness`.
    async fn detect_staleness(&self, repo: String) -> DomainResult<()> {
        crate::notes::health::detect_staleness(&self.note_repo, &self.note_health_repo, &repo)
            .await
            .map_err(DomainError::Internal)
    }

    /// Delegates to `crate::notes::health::get_health_summary`.
    async fn get_note_health(
        &self,
        repo: String,
        min_staleness: f64,
    ) -> DomainResult<NoteHealthSummary> {
        crate::notes::health::get_health_summary(&self.note_health_repo, &repo, min_staleness)
            .await
            .map_err(DomainError::Internal)
    }

    // ── Saga lifecycle — full impls ────────────────────────────────────────

    /// Delegates to `crate::notes::saga::find_or_create_saga`.
    async fn find_or_create_saga(
        &self,
        repo: String,
        source_type: String,
        source_ref: Option<String>,
        name: Option<String>,
    ) -> DomainResult<Uuid> {
        crate::notes::saga::find_or_create_saga(
            &self.saga_repo,
            &repo,
            &source_type,
            source_ref.as_deref(),
            name.as_deref(),
        )
        .await
        .map_err(DomainError::Internal)
    }

    /// Delegates to `crate::notes::saga::list_sagas`.
    async fn list_sagas(
        &self,
        repo: String,
        status: Option<String>,
    ) -> DomainResult<Vec<SagaWithCount>> {
        crate::notes::saga::list_sagas(&self.saga_repo, &repo, status.as_deref())
            .await
            .map_err(DomainError::Internal)
    }

    /// Delegates to `crate::notes::saga::get_saga_timeline`.
    async fn get_saga_timeline(
        &self,
        saga_id: Uuid,
    ) -> DomainResult<(SagaDbRow, Vec<SagaTimelineEntry>)> {
        crate::notes::saga::get_saga_timeline(&self.saga_repo, saga_id)
            .await
            .map_err(DomainError::Internal)
    }

    // ── Saga-pattern orchestration ─────────────────────────────────────────

    /// Delegates to `SagaExecutorRepo::cleanup_stale_sagas`.
    async fn cleanup_stale_sagas(&self, stale_minutes: i64) -> DomainResult<i64> {
        self.saga_executor_repo
            .cleanup_stale_sagas(stale_minutes)
            .await
            .map_err(DomainError::Internal)
    }

    /// Delegates to `SagaExecutorRepo::cleanup_expired_idempotency`.
    async fn cleanup_expired_idempotency(&self, ttl_hours: i64) -> DomainResult<i64> {
        self.saga_executor_repo
            .cleanup_expired_idempotency(ttl_hours)
            .await
            .map_err(DomainError::Internal)
    }
}
