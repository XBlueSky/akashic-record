//! Reciprocal Rank Fusion — pure, DB-free scoring.
//!
//! Moved from `akashic-retrieval::graphrag::rrf` (A1 Task 2).

use uuid::Uuid;

use crate::types::Space;

/// A single item from a ranked search result list.
#[derive(Debug, Clone)]
pub struct RankedItem {
    pub id: Uuid,
    pub rank: usize,
    pub metadata: RankedItemMeta,
}

/// Metadata carried through RRF fusion.
#[derive(Debug, Clone)]
pub struct RankedItemMeta {
    pub space: Space,
    pub entity_type: String,
    pub name: String,
}

/// Result of RRF fusion for one item.
#[derive(Debug, Clone)]
pub struct FusedItem {
    pub id: Uuid,
    pub rrf_score: f64,
    pub metadata: RankedItemMeta,
}

/// Fuse two ranked lists using weighted Reciprocal Rank Fusion.
///
/// `low_weight` and `high_weight` control how much each list contributes.
/// `k` is the RRF constant (standard: 60).
///
/// Returns items sorted descending by fused RRF score.
pub fn weighted_rrf(
    low_level: &[RankedItem],
    high_level: &[RankedItem],
    low_weight: f64,
    high_weight: f64,
    k: f64,
) -> Vec<FusedItem> {
    use std::collections::HashMap;

    let mut scores: HashMap<Uuid, (f64, RankedItemMeta)> = HashMap::new();

    for item in low_level {
        let rrf = low_weight / (k + item.rank as f64 + 1.0);
        scores
            .entry(item.id)
            .and_modify(|(s, _)| *s += rrf)
            .or_insert((rrf, item.metadata.clone()));
    }

    for item in high_level {
        let rrf = high_weight / (k + item.rank as f64 + 1.0);
        scores
            .entry(item.id)
            .and_modify(|(s, _)| *s += rrf)
            .or_insert((rrf, item.metadata.clone()));
    }

    let mut fused: Vec<FusedItem> = scores
        .into_iter()
        .map(|(id, (rrf_score, metadata))| FusedItem {
            id,
            rrf_score,
            metadata,
        })
        .collect();

    fused.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    fused
}

/// Merge multiple per-space FusedItem lists into a single cross-space list.
///
/// Optional `prefer_space` multiplies that space's scores by `prefer_boost`.
pub fn cross_space_merge(
    space_results: Vec<Vec<FusedItem>>,
    prefer_space: Option<Space>,
    prefer_boost: f64,
) -> Vec<FusedItem> {
    let mut all: Vec<FusedItem> = space_results.into_iter().flatten().collect();

    if let Some(preferred) = prefer_space {
        for item in &mut all {
            if item.metadata.space == preferred {
                item.rrf_score *= prefer_boost;
            }
        }
    }

    all.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    all
}

/// Fuse multiple ranked lists with different weights into a single ranked list.
/// Each source is a (ranked_list, weight) pair.
pub fn multi_source_rrf(sources: &[(Vec<RankedItem>, f64)], k: f64) -> Vec<FusedItem> {
    use std::collections::HashMap;

    let mut scores: HashMap<Uuid, (f64, RankedItemMeta)> = HashMap::new();

    for (items, weight) in sources {
        for item in items {
            let rrf = weight / (k + item.rank as f64 + 1.0);
            scores
                .entry(item.id)
                .and_modify(|(s, _)| *s += rrf)
                .or_insert((rrf, item.metadata.clone()));
        }
    }

    let mut fused: Vec<FusedItem> = scores
        .into_iter()
        .map(|(id, (rrf_score, metadata))| FusedItem {
            id,
            rrf_score,
            metadata,
        })
        .collect();

    fused.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Space;

    fn make_item(id_byte: u8, rank: usize, space: Space) -> RankedItem {
        RankedItem {
            id: Uuid::from_bytes([id_byte, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            rank,
            metadata: RankedItemMeta {
                space,
                entity_type: "chunk".into(),
                name: format!("item_{id_byte}"),
            },
        }
    }

    #[test]
    fn equal_weight_rrf_prefers_items_in_both_lists() {
        let low = vec![
            make_item(0xA, 0, Space::Code),
            make_item(0xC, 1, Space::Code),
        ];
        let high = vec![
            make_item(0xB, 0, Space::Code),
            make_item(0xD, 1, Space::Code),
            make_item(0xA, 2, Space::Code),
        ];

        let fused = weighted_rrf(&low, &high, 0.5, 0.5, 60.0);
        assert_eq!(fused[0].id.as_bytes()[0], 0xA);
        assert!((fused[0].rrf_score - 0.01614).abs() < 0.001);
    }

    #[test]
    fn weighted_rrf_respects_weights() {
        let low = vec![make_item(0xA, 0, Space::Code)];
        let high = vec![make_item(0xB, 0, Space::Code)];

        let fused = weighted_rrf(&low, &high, 0.8, 0.2, 60.0);
        assert_eq!(fused[0].id.as_bytes()[0], 0xA);
        assert!(fused[0].rrf_score > fused[1].rrf_score);
    }

    #[test]
    fn cross_space_merge_with_preference() {
        let code_items = vec![FusedItem {
            id: Uuid::from_bytes([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            rrf_score: 0.01,
            metadata: RankedItemMeta {
                space: Space::Code,
                entity_type: "chunk".into(),
                name: "code_item".into(),
            },
        }];
        let human_items = vec![FusedItem {
            id: Uuid::from_bytes([2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            rrf_score: 0.01,
            metadata: RankedItemMeta {
                space: Space::Human,
                entity_type: "note".into(),
                name: "note_item".into(),
            },
        }];

        let merged = cross_space_merge(vec![code_items, human_items], Some(Space::Human), 1.5);
        assert_eq!(merged[0].metadata.space, Space::Human);
        assert!((merged[0].rrf_score - 0.015).abs() < 0.001);
    }

    #[test]
    fn empty_lists_return_empty() {
        let fused = weighted_rrf(&[], &[], 0.5, 0.5, 60.0);
        assert!(fused.is_empty());

        let merged = cross_space_merge(vec![], None, 1.5);
        assert!(merged.is_empty());
    }
}
