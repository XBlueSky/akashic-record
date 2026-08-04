//! Deduplication verdict dispatch and MCP message formatting — pure, DB-free.
//!
//! Moved from `akashic-curation::notes::dedup` (A1 Task 2).
//! The `check_dedup` function (which queries PostgreSQL) stays in the curation
//! crate; only the pure verdict dispatch and formatting helpers live here.

use crate::types::{DedupMatch, DedupVerdict};

/// Format a BLOCK verdict as an MCP-friendly response string.
pub fn format_block(m: &DedupMatch) -> String {
    let title_display = m.title.as_deref().unwrap_or("(untitled)");
    format!(
        "⚠ DUPLICATE DETECTED — A nearly identical note already exists:\n  \
         [{id}] \"{title}\" ({cat}, score={score:.2})\n  \
         → Use get_details to review. If genuinely new, add distinguishing content.",
        id = m.note_id,
        title = title_display,
        cat = m.category,
        score = m.similarity,
    )
}

/// Format a WARN verdict as an MCP-friendly response string.
pub fn format_warn(m: &DedupMatch) -> String {
    let title_display = m.title.as_deref().unwrap_or("(untitled)");
    format!(
        "⚡ SIMILAR NOTE FOUND:\n  \
         [{id}] \"{title}\" ({cat}, score={score:.2})\n  \
         → Options:\n    \
           1. Save as new (different perspective)\n    \
           2. Supersede old note: call save_note with supersedes: \"{id}\"",
        id = m.note_id,
        title = title_display,
        cat = m.category,
        score = m.similarity,
    )
}

/// Dispatch a `DedupVerdict` to its string representation, or `None` on Pass.
pub fn verdict_message(verdict: &DedupVerdict) -> Option<String> {
    match verdict {
        DedupVerdict::Block(m) => Some(format_block(m)),
        DedupVerdict::Warn(m) => Some(format_warn(m)),
        DedupVerdict::Pass => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DedupMatch, DedupVerdict};
    use uuid::Uuid;

    fn make_match(title: Option<&str>, similarity: f64) -> DedupMatch {
        DedupMatch {
            note_id: Uuid::from_bytes([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            title: title.map(String::from),
            category: "ARCHITECTURE".to_string(),
            similarity,
        }
    }

    #[test]
    fn block_verdict_message_contains_score() {
        let m = make_match(Some("Auth Overview"), 0.97);
        let msg = format_block(&m);
        assert!(msg.contains("DUPLICATE DETECTED"));
        assert!(msg.contains("0.97"));
        assert!(msg.contains("Auth Overview"));
    }

    #[test]
    fn warn_verdict_message_contains_score() {
        let m = make_match(Some("Auth Flow"), 0.75);
        let msg = format_warn(&m);
        assert!(msg.contains("SIMILAR NOTE FOUND"));
        assert!(msg.contains("0.75"));
        assert!(msg.contains("Auth Flow"));
    }

    #[test]
    fn block_verdict_uses_untitled_fallback() {
        let m = make_match(None, 0.95);
        let msg = format_block(&m);
        assert!(msg.contains("(untitled)"));
    }

    #[test]
    fn pass_verdict_returns_none() {
        assert!(verdict_message(&DedupVerdict::Pass).is_none());
    }

    #[test]
    fn block_verdict_returns_some() {
        let m = make_match(Some("X"), 0.96);
        assert!(verdict_message(&DedupVerdict::Block(m)).is_some());
    }

    #[test]
    fn warn_verdict_returns_some() {
        let m = make_match(Some("X"), 0.72);
        assert!(verdict_message(&DedupVerdict::Warn(m)).is_some());
    }
}
