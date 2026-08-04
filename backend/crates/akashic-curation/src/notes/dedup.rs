use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use akashic_domain::ports::NoteRepo;

// Pure type definitions and formatting functions live in akashic-domain.
// Re-export them so all existing call sites continue to compile unchanged.
pub use akashic_domain::algos::dedup::{format_block, format_warn, verdict_message};
pub use akashic_domain::types::{DedupMatch, DedupVerdict};

/// Check for duplicate notes by embedding cosine similarity.
///
/// Queries the top-1 most similar valid note in the same repo and returns a
/// verdict based on configurable thresholds:
///
/// - **Same saga** (both notes share a `saga_id`): block at 0.95, warn at 0.65
/// - **Different saga or standalone**: block at 0.90, warn at 0.70
pub async fn check_dedup(
    notes: &Arc<dyn NoteRepo>,
    repo_name: &str,
    embedding: &[f32],
    saga_id: Option<Uuid>,
) -> Result<DedupVerdict> {
    let candidate = notes
        .find_similar_by_embedding(repo_name, embedding)
        .await?;

    let Some(cand) = candidate else {
        return Ok(DedupVerdict::Pass);
    };

    // Determine thresholds based on saga relationship.
    let same_saga = match (saga_id, cand.saga_id) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };

    let (block_thresh, warn_thresh) = if same_saga {
        (0.95, 0.65)
    } else {
        (0.90, 0.70)
    };

    let matched = DedupMatch {
        note_id: cand.note_id,
        title: cand.title,
        category: cand.category,
        similarity: cand.similarity,
    };

    if cand.similarity >= block_thresh {
        Ok(DedupVerdict::Block(matched))
    } else if cand.similarity >= warn_thresh {
        Ok(DedupVerdict::Warn(matched))
    } else {
        Ok(DedupVerdict::Pass)
    }
}
