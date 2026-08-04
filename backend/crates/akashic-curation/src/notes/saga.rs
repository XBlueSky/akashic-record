//! Saga CRUD — find-or-create, resolve, list, and timeline queries.
//!
//! A "knowledge saga" groups related notes that belong to the same logical
//! effort (an issue, a branch, or a manual topic). This module provides the
//! data-access layer for sagas used by the MCP tools and REST API.
//!
//! All SQL has been moved to `PgSagaRepo` in `akashic-store-pg` (A1 Task 5).
//! Functions now accept `&Arc<dyn SagaRepo>` instead of `&PgPool`.

use std::sync::Arc;

use anyhow::Result;
use tracing::info;
use uuid::Uuid;

use akashic_domain::ports::SagaRepo;

// Pure branch-parsing kernel moved to akashic-domain (A1 Task 2).
pub use akashic_domain::algos::saga_status::extract_issue_from_branch;

// Re-export domain types so all existing call-sites in akashic-server
// (mcp/tools.rs, api/routes/sagas.rs) compile unchanged.
pub use akashic_domain::types::{SagaDbRow as Saga, SagaTimelineEntry, SagaWithCount};

// ── find_or_create_saga ─────────────────────────────────────────────

/// Find an existing saga matching (repo_name, source_type, source_ref) or
/// create a new one. Returns the saga UUID.
pub async fn find_or_create_saga(
    repo: &Arc<dyn SagaRepo>,
    repo_name: &str,
    source_type: &str,
    source_ref: Option<&str>,
    name: Option<&str>,
) -> Result<Uuid> {
    // Try to find existing saga with matching criteria
    let existing: Option<Uuid> = if let Some(sref) = source_ref {
        repo.find_by_source(repo_name, source_type, sref).await?
    } else {
        match name {
            Some(n) => repo.find_by_name(repo_name, source_type, n).await?,
            None => None,
        }
    };

    if let Some(id) = existing {
        return Ok(id);
    }

    // Create new saga
    let saga_id = Uuid::new_v4();
    let display_name = name
        .map(std::string::ToString::to_string)
        .or_else(|| source_ref.map(|r| format!("{source_type}:{r}")));

    let row = repo
        .create_saga(
            saga_id,
            "knowledge",
            repo_name,
            "open",
            display_name.as_deref(),
            source_type,
            source_ref,
        )
        .await?;

    if let Some(id) = row {
        info!(
            saga_id = %id,
            repo = repo_name,
            source_type,
            source_ref,
            "Created new knowledge saga"
        );
        Ok(id)
    } else {
        // Another concurrent request won the race — fetch the existing saga
        let winner_id = repo
            .get_saga_by_source(repo_name, source_type, source_ref.unwrap_or(""))
            .await?;
        Ok(winner_id)
    }
}

// ── resolve_saga_for_note ───────────────────────────────────────────

/// Determine which saga a note belongs to using this priority:
///
/// 1. If `issue_ref` provided (e.g. "PROJ-123456") → find_or_create with source_type="issue"
/// 2. If branch matches `wp/<project>/<id>` → extract issue ref, source_type="branch"
/// 3. If `topic` provided → find_or_create with source_type="manual"
/// 4. Otherwise → None
pub async fn resolve_saga_for_note(
    repo: &Arc<dyn SagaRepo>,
    repo_name: &str,
    issue_ref: Option<&str>,
    branch: &str,
    topic: Option<&str>,
) -> Result<Option<Uuid>> {
    // Priority 1: explicit issue reference
    if let Some(iref) = issue_ref
        && !iref.is_empty()
    {
        let id = find_or_create_saga(repo, repo_name, "issue", Some(iref), None).await?;
        return Ok(Some(id));
    }

    // Priority 2: branch pattern wp/<PROJECT>/<ID> → extract issue ref
    if let Some(extracted) = extract_issue_from_branch(branch) {
        let id = find_or_create_saga(repo, repo_name, "branch", Some(&extracted), None).await?;
        return Ok(Some(id));
    }

    // Priority 3: manual topic
    if let Some(t) = topic
        && !t.is_empty()
    {
        let id = find_or_create_saga(repo, repo_name, "manual", None, Some(t)).await?;
        return Ok(Some(id));
    }

    Ok(None)
}

// ── list_sagas ──────────────────────────────────────────────────────

/// List sagas for a repository with note counts, optional status filter.
///
/// Finding (saga.rs:211): only knowledge sagas are listed. Ingestion-control
/// rows (saga_type in trigger_ingest/reingest/resume_ingest/add_source, with
/// name = NULL) share the `sagas` table but must not leak into the knowledge-
/// saga UI, so both query arms filter on `saga_type = 'knowledge'`.
pub async fn list_sagas(
    repo: &Arc<dyn SagaRepo>,
    repo_name: &str,
    status_filter: Option<&str>,
) -> Result<Vec<SagaWithCount>> {
    if let Some(status) = status_filter {
        repo.list_sagas_with_status_filter(repo_name, status).await
    } else {
        repo.list_all_sagas(repo_name).await
    }
}

// ── get_saga_timeline ───────────────────────────────────────────────

/// Get a saga and its notes ordered by COALESCE(valid_at, created_at) ASC.
///
/// Finding (saga.rs:211): the lookup is constrained to `saga_type = 'knowledge'`
/// so an ingestion-control saga UUID does not get surfaced as a knowledge saga
/// (it yields a not-found error instead of a bogus control-row header).
pub async fn get_saga_timeline(
    repo: &Arc<dyn SagaRepo>,
    saga_id: Uuid,
) -> Result<(Saga, Vec<SagaTimelineEntry>)> {
    let saga = repo.get_saga_by_id(saga_id).await?;
    let entries = repo.get_saga_timeline_entries(saga_id).await?;
    Ok((saga, entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The finding fixes themselves (the ON-CONFLICT comment in
    // find_or_create_saga and the `saga_type = 'knowledge'` guards in
    // list_sagas / get_saga_timeline) live inside DB-backed async functions and
    // inline SQL, so they are exercised by integration tests, not here. The only
    // pure, DB-free logic in this module is `extract_issue_from_branch`, which
    // decides the source_type/source_ref a saga is keyed on — and therefore
    // whether the dedup-by-source_ref path (finding saga.rs:113) applies at all.

    #[test]
    fn extracts_issue_ref_from_wp_branch() {
        assert_eq!(
            extract_issue_from_branch("wp/PROJ/123456"),
            Some("PROJ-123456".to_string())
        );
        assert_eq!(
            extract_issue_from_branch("wp/SVC/42"),
            Some("SVC-42".to_string())
        );
    }

    #[test]
    fn non_wp_branches_have_no_issue_ref() {
        // These fall through to the manual-topic / None path, where the
        // source_ref-based unique index does NOT dedup (finding saga.rs:113).
        assert_eq!(extract_issue_from_branch("main"), None);
        assert_eq!(extract_issue_from_branch("feature/foo"), None);
        assert_eq!(extract_issue_from_branch("wp/PROJ/abc"), None); // id must be digits
        assert_eq!(extract_issue_from_branch("wp/proj/123"), None); // project must be uppercase
        assert_eq!(extract_issue_from_branch("wp/PROJ/123/extra"), None); // anchored, no trailing
        assert_eq!(extract_issue_from_branch(""), None);
    }
}
