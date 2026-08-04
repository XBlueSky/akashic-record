//! Memory-stack format helpers — pure, DB-free.
//!
//! Moved from `akashic-curation::notes::memory_stack` (A1 Task 2).

/// Render a single `[TOP NOTES]` line.
///
/// `idx` is the 0-based row position; `None` title falls back to `"untitled"`,
/// `None` access count falls back to `0 hits`.
pub fn format_top_note_line(
    idx: usize,
    title: Option<&str>,
    category: &str,
    access_count: Option<i32>,
) -> String {
    let t = title.unwrap_or("untitled");
    let hits = access_count.unwrap_or(0);
    format!(
        "{}. \u{2605} \"{}\" ({}) \u{2014} {} hits",
        idx + 1,
        t,
        category,
        hits,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_top_note_line_with_values() {
        let line = format_top_note_line(0, Some("Auth Overview"), "ARCHITECTURE", Some(42));
        assert_eq!(line, "1. ★ \"Auth Overview\" (ARCHITECTURE) — 42 hits");
    }

    #[test]
    fn format_top_note_line_null_title() {
        let line = format_top_note_line(2, None, "BUG_FIX", Some(7));
        assert_eq!(line, "3. ★ \"untitled\" (BUG_FIX) — 7 hits");
    }

    #[test]
    fn format_top_note_line_null_access_count() {
        let line = format_top_note_line(0, Some("My Note"), "ONBOARDING", None);
        assert_eq!(line, "1. ★ \"My Note\" (ONBOARDING) — 0 hits");
    }

    #[test]
    fn format_top_note_line_one_based_numbering() {
        // Ensure the index is 1-based (idx=4 → "5.")
        let line = format_top_note_line(4, Some("Fifth"), "DECISION", Some(1));
        assert!(line.starts_with("5. "));
    }
}
