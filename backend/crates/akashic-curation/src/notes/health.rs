use std::sync::Arc;

use anyhow::Result;
use tracing::info;

use akashic_domain::ports::{NoteHealthRepo, NoteRepo};

// Pure type definitions live in akashic-domain; re-export so all existing
// call sites (notes/mod.rs, MCP tools, etc.) continue to compile unchanged.
pub use akashic_domain::types::{NoteHealthEntry, NoteHealthSummary};

/// Process one note's staleness: symbol-deleted check + content-divergence,
/// then persist via `update_staleness`. Isolated so a per-note failure can be
/// logged-and-skipped by the caller rather than aborting the whole repo's run.
async fn detect_one_note(
    notes: &Arc<dyn NoteRepo>,
    note_health: &Arc<dyn NoteHealthRepo>,
    repo_name: &str,
    note_entry: &akashic_domain::types::NoteWithSymbols,
) -> Result<()> {
    let mut reasons: Vec<String> = Vec::new();
    let mut score = 0.0_f64;

    for symbol in &note_entry.symbols {
        let count = notes.check_symbol_existence(repo_name, symbol).await?;
        if count == 0 {
            reasons.push(format!("symbol_deleted:{symbol}"));
            score += 0.5;
        }
    }

    if !note_entry.symbols.is_empty() {
        let divergence = notes
            .compute_embedding_divergence(note_entry.id, repo_name, &note_entry.symbols)
            .await?;
        if let Some(div) = divergence
            && div > 0.3
        {
            reasons.push(format!("content_diverged:similarity={:.2}", 1.0 - div));
            score += div;
        }
    }

    let score = score.min(1.0);
    note_health
        .update_staleness(note_entry.id, score, &reasons)
        .await
}

/// Run staleness detection for all notes in a repository.
///
/// Detection types:
/// 1. Symbol Deleted — related_symbols no longer exist in chunks table
/// 2. Content Diverged — note embedding vs current chunk embeddings diverged
pub async fn detect_staleness(
    notes: &Arc<dyn NoteRepo>,
    note_health: &Arc<dyn NoteHealthRepo>,
    repo_name: &str,
) -> Result<()> {
    // Get all non-archived notes with related_symbols
    let notes_with_symbols = notes.get_all_with_symbols(repo_name).await?;

    let mut skipped = 0usize;
    for note_entry in &notes_with_symbols {
        if let Err(e) = detect_one_note(notes, note_health, repo_name, note_entry).await {
            tracing::warn!(
                repo = repo_name,
                note_id = %note_entry.id,
                error = %e,
                "staleness detection skipped for note"
            );
            skipped += 1;
        }
    }

    // Auto-archive: staleness > 0.9 + access_count == 0 + age > 90 days
    let archived_count = note_health.auto_archive_stale(repo_name).await?;

    if archived_count > 0 {
        info!(
            repo = repo_name,
            archived = archived_count,
            "Auto-archived stale notes"
        );
    }

    info!(
        repo = repo_name,
        notes = notes_with_symbols.len(),
        skipped_notes = skipped,
        "Staleness detection complete"
    );
    Ok(())
}

/// Get note health summary for a repository.
pub async fn get_health_summary(
    note_health: &Arc<dyn NoteHealthRepo>,
    repo_name: &str,
    min_staleness: f64,
) -> Result<NoteHealthSummary> {
    let total = note_health.count_total_notes(repo_name).await?;
    let archived = note_health.count_archived_notes(repo_name).await?;
    let healthy = note_health.count_healthy_notes(repo_name).await?;
    let needs_review = note_health.count_needs_review_notes(repo_name).await?;
    let likely_stale = note_health.count_stale_notes(repo_name).await?;

    let stale_rows = note_health
        .get_stale_notes_above_threshold(repo_name, min_staleness)
        .await?;

    let stale_notes: Vec<NoteHealthEntry> = stale_rows
        .into_iter()
        .map(|row| {
            let deleted_count = row
                .staleness_reasons
                .iter()
                .filter(|r| r.starts_with("symbol_deleted"))
                .count();
            let suggestion = if deleted_count > 0 {
                format!("Review: {deleted_count} referenced symbol(s) no longer exist")
            } else if row.staleness_score > 0.5 {
                "Review: content has diverged from referenced code".into()
            } else {
                "Minor staleness detected".into()
            };

            NoteHealthEntry {
                id: row.id,
                title: row.title.unwrap_or_else(|| "Untitled".into()),
                staleness_score: row.staleness_score,
                staleness_reasons: row.staleness_reasons,
                access_count: row.access_count.unwrap_or(0),
                last_accessed: row.last_accessed_at.map(|t| t.to_rfc3339()),
                suggestion,
            }
        })
        .collect();

    Ok(NoteHealthSummary {
        total_notes: total,
        healthy,
        needs_review,
        likely_stale,
        archived,
        stale_notes,
    })
}
