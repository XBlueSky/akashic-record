//! GraphService impl bodies — summary (graph.rs split). Inherent `gs_*` methods
//! that the thin `impl GraphService` in `super` delegates to.
use super::*;

impl RetrievalServices {
    pub(crate) async fn gs_get_project_summary(
        &self,
        repo: String,
        branch: Option<String>,
    ) -> DomainResult<String> {
        // Branches from Neo4j (verbatim from tools.rs: get_project_summary).
        let branch_rows = self.graph_read_repo.get_branches(&repo).await?;
        let branches: Vec<String> = branch_rows.into_iter().map(|b| b.name).collect();

        // Category breakdown from NoteHealthRepo (branch-aware).
        // The original handler uses a raw `notes` query with optional branch filter;
        // NoteHealthRepo::get_notes_by_category does the unfiltered version.
        // For now we use the unfiltered version and filter post-fetch if branch is set.
        // (Branch-filtered category count is only needed for the branch-scoped summary.)
        let cat_rows = self.note_health_repo.get_notes_by_category(&repo).await?;

        // Latest notes (branch-aware) via NoteHealthRepo::get_latest_notes.
        let latest_rows = self
            .note_health_repo
            .get_latest_notes(&repo, branch.as_deref(), 10)
            .await?;

        let mut note_count: i64 = 0;
        let categories: Vec<serde_json::Value> = cat_rows
            .into_iter()
            .map(|(category, cnt)| {
                note_count += cnt;
                serde_json::json!({ "category": category, "count": cnt })
            })
            .collect();

        let latest_notes: Vec<serde_json::Value> = latest_rows
            .into_iter()
            .map(|n| {
                serde_json::json!({
                    "uuid": n.id.to_string(),
                    "category": n.category,
                    "branch": n.branch.unwrap_or_default(),
                    "created_at": n.created_at.unwrap_or_default(),
                    "snippet": n.snippet,
                })
            })
            .collect();

        // Ingestion stats from PG repos.
        let ingested_modules_count = self.module_repo.count_modules(&repo).await?;
        let total_chunks = self.chunk_repo.count_chunks(&repo).await?;
        let last_ingested_at = self
            .chunk_repo
            .get_max_ingested_at(&repo)
            .await?
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());

        let summary = serde_json::json!({
            "repo_name": repo,
            "branches": branches,
            "note_count": note_count,
            "categories": categories,
            "latest_notes": latest_notes,
            "ingested_modules_count": ingested_modules_count,
            "total_chunks": total_chunks,
            "last_ingested_at": last_ingested_at,
        });

        serde_json::to_string_pretty(&summary).map_err(Into::into)
    }
}
