//! Memory stack — build text representations of repo context at different
//! detail levels for LLM prompt construction.
//!
//! - **L0 (Repo Identity)**: ~100 tokens — languages, module/chunk/note/saga counts.
//! - **L1 (Essential Notes)**: ~200 tokens — top notes by access count.
//! - **L2 (Saga Context)**: ~300-500 tokens — full saga timeline with notes.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use akashic_domain::ports::{ChunkRepo, ModuleRepo, NoteHealthRepo, SagaRepo};

// Pure formatting kernel moved to akashic-domain (A1 Task 2).
// Re-export so build_l1 and the unit tests (via `use super::*`) continue
// to resolve the symbol.
pub(crate) use akashic_domain::algos::memory_format::format_top_note_line;

// ── L0: Repo Identity ──────────────────────────────────────────────

/// Build a compact repo identity block (~100 tokens).
///
/// Includes language distribution, module/chunk counts, note counts by
/// category, saga counts, and last ingestion date.
///
/// All DB access goes through port traits (A1 T3/T4/T5/T6):
/// - `chunk_repo` → language distribution, chunk count, last ingested
/// - `module_repo` → module count
/// - `note_health` → note category counts
/// - `saga_repo` → saga counts
pub async fn build_l0(
    chunk_repo: &Arc<dyn ChunkRepo>,
    module_repo: &Arc<dyn ModuleRepo>,
    note_health: &Arc<dyn NoteHealthRepo>,
    saga_repo: &Arc<dyn SagaRepo>,
    repo_name: &str,
) -> Result<String> {
    // Language distribution (top 3) — via ChunkRepo port (A1 T6)
    let lang_rows = chunk_repo.get_language_distribution(repo_name).await?;

    let lang_total: f64 = lang_rows.iter().map(|r| r.count as f64).sum();
    let lang_str = if lang_rows.is_empty() {
        "unknown".to_string()
    } else {
        lang_rows
            .iter()
            .map(|r| {
                let pct = if lang_total > 0.0 {
                    (r.count as f64 / lang_total * 100.0).round() as i64
                } else {
                    0
                };
                format!("{} ({pct}%)", r.language)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };

    // Module count — via ModuleRepo port (A1 T6)
    let module_count = module_repo.count_modules(repo_name).await?;

    // Chunk count — via ChunkRepo port (A1 T6)
    let chunk_count = chunk_repo.count_chunks(repo_name).await?;

    // Note counts by category — via NoteHealthRepo port (A1 T4)
    let cat_rows = note_health.get_notes_by_category(repo_name).await?;

    let total_notes: i64 = cat_rows.iter().map(|(_, c)| *c).sum();
    let cat_str = if cat_rows.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = cat_rows
            .iter()
            .map(|(cat, cnt)| format!("{cnt} {cat}"))
            .collect();
        format!(" ({})", parts.join(", "))
    };

    // Saga counts — via SagaRepo port (A1 T5)
    let active_sagas = saga_repo.count_active_sagas(repo_name).await?;
    let resolved_sagas = saga_repo.count_resolved_sagas(repo_name).await?;

    // Last ingested date (most recent chunk) — via ChunkRepo port (A1 T6)
    let last_str = chunk_repo
        .get_max_ingested_at(repo_name)
        .await?
        .map_or_else(
            || "never".to_string(),
            |ts| ts.format("%Y-%m-%d").to_string(),
        );

    Ok(format!(
        "[REPO] {repo_name}\n\
         [LANG] {lang_str}\n\
         [MODULES] {module_count} modules, {chunk_count} chunks\n\
         [NOTES] {total_notes} notes{cat_str}\n\
         [SAGAS] {active_sagas} active, {resolved_sagas} resolved\n\
         [LAST INGESTED] {last_str}"
    ))
}

// ── L1: Essential Notes ────────────────────────────────────────────

/// Build a list of top notes by access count (~200 tokens).
pub async fn build_l1(
    note_health: &Arc<dyn NoteHealthRepo>,
    repo_name: &str,
    limit: i64,
) -> Result<String> {
    let rows = note_health
        .get_top_notes_by_access(repo_name, limit)
        .await?;

    if rows.is_empty() {
        return Ok("[TOP NOTES] (none yet)".to_string());
    }

    let mut lines = vec!["[TOP NOTES]".to_string()];
    for (i, row) in rows.iter().enumerate() {
        lines.push(format_top_note_line(
            i,
            row.title.as_deref(),
            &row.category,
            row.access_count,
        ));
    }

    Ok(lines.join("\n"))
}

// ── L2: Saga Context ──────────────────────────────────────────────

/// Build a saga timeline with all its notes (~300-500 tokens).
///
/// Saga info (name, status) is fetched via SagaRepo (A1 T5).
/// Note queries are served via NoteHealthRepo (A1 T4).
pub async fn build_l2(
    note_health: &Arc<dyn NoteHealthRepo>,
    saga_repo: &Arc<dyn SagaRepo>,
    saga_id: Uuid,
) -> Result<String> {
    // Saga info — via SagaRepo port (A1 T5)
    let (saga_name, saga_status) = match saga_repo.get_saga_info(saga_id).await? {
        Some((name, status)) => (name.unwrap_or_else(|| "unnamed".to_string()), status),
        None => anyhow::bail!("saga {saga_id} not found"),
    };

    // Note count — via NoteHealthRepo port (A1 T4)
    let note_count = note_health.count_notes_for_saga(saga_id).await?;

    // Notes timeline — via NoteHealthRepo port (A1 T4)
    let notes = note_health.get_saga_notes_timeline(saga_id).await?;

    let mut lines = vec![format!(
        "[SAGA] {} ({}, {} notes)",
        saga_name, saga_status, note_count,
    )];

    for row in &notes {
        let t = row.title.as_deref().unwrap_or("untitled");
        let date = row.ts.format("%Y-%m-%d").to_string();
        let marker = if row.is_superseded { "  " } else { "\u{2605} " };
        let status_label = if row.is_superseded {
            " \u{2014} superseded"
        } else {
            " \u{2014} valid"
        };
        lines.push(format!(
            "  {date} {marker}\"{t}\" ({category}){status_label}",
            category = row.category
        ));
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_note_line_uses_1_based_index() {
        // idx is 0-based internally; rendered position must start at 1.
        let line = format_top_note_line(0, Some("Cache layer"), "ARCHITECTURE", Some(42));
        assert!(line.starts_with("1. "), "got: {line}");
        assert!(line.contains("\"Cache layer\""));
        assert!(line.contains("(ARCHITECTURE)"));
        assert!(line.ends_with("42 hits"));
    }

    #[test]
    fn top_note_line_renders_null_access_count_as_zero() {
        // A SQL NULL access_count (None) must render as "0 hits" rather than
        // panicking or being treated as the most-accessed. This mirrors why
        // build_l1's ORDER BY needs NULLS LAST: NULL is the *least* accessed,
        // so it must both sort last and display as zero.
        let line = format_top_note_line(2, Some("Untouched note"), "DECISION", None);
        assert!(line.starts_with("3. "), "got: {line}");
        assert!(line.ends_with("0 hits"), "got: {line}");
    }

    #[test]
    fn top_note_line_falls_back_to_untitled() {
        // A NULL title (None) should not produce an empty quoted string.
        let line = format_top_note_line(0, None, "BUG_FIX", Some(7));
        assert!(line.contains("\"untitled\""), "got: {line}");
        assert!(line.ends_with("7 hits"));
    }
}
