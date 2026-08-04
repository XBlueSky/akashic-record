//! Decision-lineage merge + formatting — pure, DB-free.
//!
//! Given the `SUPERSEDES`-chain members walked from one or more DECISION notes
//! (each tagged with its direction + hop distance from the seed) plus a
//! note-id→title map resolved from Postgres, produce a deduplicated,
//! distance-ordered decision timeline and render it. Mirrors `super::dead_code`
//! (pure algo + DTOs + formatter).

use std::collections::HashMap;

use uuid::Uuid;

/// One member of a note's `SUPERSEDES` chain, relative to the seed note it was
/// walked from. `direction` is "self" (the seed, hop 0), "ancestor" (an older
/// note the seed supersedes), or "descendant" (a newer note that supersedes the
/// seed).
#[derive(Debug, Clone)]
pub struct ChainMember {
    pub note_id: Uuid,
    pub direction: String,
    pub hop_distance: i64,
}

/// A chunk a decision note is directly `ATTACHED_TO` (for get_decision_lineage).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AttachedChunk {
    pub name: String,
    pub fqn: Option<String>,
    pub module_path: Option<String>,
    pub chunk_type: Option<String>,
}

/// One entry in a merged decision timeline.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DecisionTimelineEntry {
    pub note_id: Uuid,
    pub title: String,
    pub direction: String,
    pub hop_distance: i64,
}

/// Full lineage of one decision: its merged supersede timeline + the chunks the
/// TARGET note itself is attached to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DecisionLineageReport {
    pub note_id: Uuid,
    pub timeline: Vec<DecisionTimelineEntry>,
    pub attached_chunks: Vec<AttachedChunk>,
}

/// Tie-break rank for `direction` when two members share a hop distance: prefer
/// the seed ("self") over ancestor/descendant labels.
fn direction_rank(direction: &str) -> u8 {
    match direction {
        "self" => 0,
        "ancestor" => 1,
        _ => 2,
    }
}

/// Deduplicate chain members by note id (keeping the smallest hop distance, then
/// the strongest direction), resolve each note's title from `titles` (falling
/// back to the UUID string when absent), and order by (hop_distance, title).
pub fn merge_decision_timeline(
    members: &[ChainMember],
    titles: &HashMap<Uuid, String>,
) -> Vec<DecisionTimelineEntry> {
    let mut best: HashMap<Uuid, &ChainMember> = HashMap::new();
    for m in members {
        match best.get(&m.note_id) {
            Some(cur) => {
                let better = m.hop_distance < cur.hop_distance
                    || (m.hop_distance == cur.hop_distance
                        && direction_rank(&m.direction) < direction_rank(&cur.direction));
                if better {
                    best.insert(m.note_id, m);
                }
            }
            None => {
                best.insert(m.note_id, m);
            }
        }
    }

    let mut out: Vec<DecisionTimelineEntry> = best
        .values()
        .map(|m| DecisionTimelineEntry {
            note_id: m.note_id,
            title: titles
                .get(&m.note_id)
                .cloned()
                .unwrap_or_else(|| m.note_id.to_string()),
            direction: m.direction.clone(),
            hop_distance: m.hop_distance,
        })
        .collect();

    out.sort_by(|a, b| {
        a.hop_distance
            .cmp(&b.hop_distance)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.note_id.cmp(&b.note_id))
    });
    out
}

/// Render `trace_decision_history` output: the merged timeline for a symbol.
pub fn format_decision_history(
    repo: &str,
    symbol: &str,
    timeline: &[DecisionTimelineEntry],
) -> String {
    if timeline.is_empty() {
        return format!(
            "No DECISION notes are attached to code matching '{symbol}' in '{repo}'.\n"
        );
    }
    let mut out = format!(
        "Decision history for '{symbol}' in '{repo}' ({} decision(s), nearest-first):\n\n",
        timeline.len()
    );
    for e in timeline {
        out.push_str(&format!(
            "[{}] {} (hop {})\n",
            e.direction, e.title, e.hop_distance
        ));
    }
    out
}

/// Render `get_decision_lineage` output: one decision's chain + its own chunks.
pub fn format_decision_lineage(report: &DecisionLineageReport) -> String {
    let mut out = format!(
        "Decision lineage for note {} ({} node(s) in chain):\n\n",
        report.note_id,
        report.timeline.len()
    );
    if report.timeline.is_empty() {
        out.push_str("No supersede chain for this decision.\n");
    } else {
        for e in &report.timeline {
            out.push_str(&format!(
                "[{}] {} (hop {})\n",
                e.direction, e.title, e.hop_distance
            ));
        }
    }
    out.push_str("\nAttached code:\n");
    if report.attached_chunks.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for c in &report.attached_chunks {
            match c.fqn.as_deref().filter(|s| !s.is_empty()) {
                Some(f) => out.push_str(&format!("  - {} ({})\n", c.name, f)),
                None => out.push_str(&format!("  - {}\n", c.name)),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: Uuid, dir: &str, hop: i64) -> ChainMember {
        ChainMember {
            note_id: id,
            direction: dir.into(),
            hop_distance: hop,
        }
    }

    #[test]
    fn merge_dedups_and_orders_by_hop_then_title() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        // Two seeds' chains sharing member `b`: b at hop 1 (from a) and hop 2
        // (from c). Dedup must keep the smaller hop (1).
        let members = vec![
            m(a, "self", 0),
            m(b, "descendant", 1),
            m(c, "descendant", 2),
            m(b, "ancestor", 2), // duplicate of b at a larger hop → dropped
        ];
        let mut titles = HashMap::new();
        titles.insert(a, "Decision A".to_string());
        titles.insert(b, "Decision B".to_string());
        titles.insert(c, "Decision C".to_string());

        let out = merge_decision_timeline(&members, &titles);
        let ordered: Vec<(&str, i64)> = out
            .iter()
            .map(|e| (e.title.as_str(), e.hop_distance))
            .collect();
        assert_eq!(
            ordered,
            vec![("Decision A", 0), ("Decision B", 1), ("Decision C", 2)]
        );
    }

    #[test]
    fn merge_equal_hop_tiebreak_prefers_self_over_ancestor_or_descendant() {
        let a = Uuid::from_u128(4);
        // Same note, same hop distance (0), reported once as "descendant" and
        // once as "self" — the tiebreak (direction_rank) must keep "self"
        // regardless of insertion order, since a smaller hop never applies here
        // (both entries share hop 0).
        let members_desc_then_self = vec![m(a, "descendant", 0), m(a, "self", 0)];
        let out = merge_decision_timeline(&members_desc_then_self, &HashMap::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].direction, "self");

        let members_self_then_desc = vec![m(a, "self", 0), m(a, "descendant", 0)];
        let out = merge_decision_timeline(&members_self_then_desc, &HashMap::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].direction, "self");
    }

    #[test]
    fn merge_missing_title_falls_back_to_uuid() {
        let a = Uuid::from_u128(7);
        let out = merge_decision_timeline(&[m(a, "self", 0)], &HashMap::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, a.to_string());
    }

    #[test]
    fn merge_empty_is_empty() {
        assert!(merge_decision_timeline(&[], &HashMap::new()).is_empty());
    }

    #[test]
    fn history_format_has_titles_and_directions() {
        let a = Uuid::from_u128(1);
        let timeline = vec![DecisionTimelineEntry {
            note_id: a,
            title: "Decision A".into(),
            direction: "self".into(),
            hop_distance: 0,
        }];
        let s = format_decision_history("acme", "handle_login", &timeline);
        assert!(s.contains("Decision history"));
        assert!(s.contains("Decision A"));
        assert!(s.contains("[self]"));
    }

    #[test]
    fn history_format_empty_message() {
        let s = format_decision_history("acme", "nope", &[]);
        assert!(s.contains("No DECISION notes"));
    }

    #[test]
    fn lineage_format_shows_chain_and_chunks() {
        let a = Uuid::from_u128(1);
        let report = DecisionLineageReport {
            note_id: a,
            timeline: vec![DecisionTimelineEntry {
                note_id: a,
                title: "Decision A".into(),
                direction: "self".into(),
                hop_distance: 0,
            }],
            attached_chunks: vec![AttachedChunk {
                name: "handle_login".into(),
                fqn: Some("auth::handle_login".into()),
                module_path: Some("src/auth.rs".into()),
                chunk_type: Some("function".into()),
            }],
        };
        let s = format_decision_lineage(&report);
        assert!(s.contains("Decision lineage"));
        assert!(s.contains("Decision A"));
        assert!(s.contains("Attached code"));
        assert!(s.contains("handle_login"));
        assert!(s.contains("auth::handle_login"));
    }

    #[test]
    fn lineage_format_empty_chain_and_no_chunks() {
        let a = Uuid::from_u128(9);
        let report = DecisionLineageReport {
            note_id: a,
            timeline: vec![],
            attached_chunks: vec![],
        };
        let s = format_decision_lineage(&report);
        assert!(s.contains("No supersede chain"));
        assert!(s.contains("(none)"));
    }
}
