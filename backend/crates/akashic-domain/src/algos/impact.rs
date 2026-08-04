//! Impact aggregation — pure, DB-free scoring.
//!
//! Moved from `akashic-retrieval::graphrag::impact` (A1 Task 2).

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::types::{
    AffectedFlow, ChangeImpactReport, ImpactNode, ImpactPath, ImpactPathType, SuggestedTest,
};

const DECAY: f64 = 0.6;

/// Raw impact edge collected from Neo4j traversal.
#[derive(Debug, Clone)]
pub struct RawImpactEdge {
    pub node_id: Uuid,
    pub node_name: String,
    pub module_path: String,
    pub entity_type: String,
    pub path_type: ImpactPathType,
    pub hops: u32,
    pub confidence: f64,
}

/// Compute impact score for a single path.
pub fn path_score(path_type: ImpactPathType, hops: u32, confidence: f64) -> f64 {
    path_type.weight() * DECAY.powi(hops as i32) * confidence
}

/// Aggregate raw impact edges into scored ImpactNodes.
///
/// Multi-path aggregation: max(path_scores) + 0.1 * (path_count - 1)
pub fn aggregate_impacts(edges: Vec<RawImpactEdge>) -> Vec<ImpactNode> {
    let mut by_node: HashMap<Uuid, (String, String, String, Vec<ImpactPath>)> = HashMap::new();

    for edge in edges {
        let score = path_score(edge.path_type, edge.hops, edge.confidence);
        let path = ImpactPath {
            path_type: edge.path_type,
            hops: edge.hops,
            confidence: edge.confidence,
            score,
        };

        by_node
            .entry(edge.node_id)
            .and_modify(|(_, _, _, paths)| paths.push(path.clone()))
            .or_insert((
                edge.node_name,
                edge.module_path,
                edge.entity_type,
                vec![path],
            ));
    }

    let mut nodes: Vec<ImpactNode> = by_node
        .into_iter()
        .map(|(id, (name, module_path, entity_type, paths))| {
            let max_score = paths.iter().map(|p| p.score).fold(0.0_f64, f64::max);
            let path_count = paths.len();
            let impact_score = max_score + 0.1 * (path_count as f64 - 1.0).max(0.0);

            ImpactNode {
                id,
                name,
                module_path,
                entity_type,
                impact_score,
                paths,
            }
        })
        .collect();

    nodes.sort_by(|a, b| {
        b.impact_score
            .partial_cmp(&a.impact_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    nodes
}

/// Determine risk level from impact nodes.
pub fn risk_level(nodes: &[ImpactNode]) -> &'static str {
    let total: f64 = nodes.iter().map(|n| n.impact_score).sum();
    let max_single = nodes.iter().map(|n| n.impact_score).fold(0.0_f64, f64::max);

    if total > 3.0 || max_single > 0.8 {
        "HIGH"
    } else if total > 1.0 {
        "MEDIUM"
    } else {
        "LOW"
    }
}

/// Format impact nodes into human-readable output.
pub fn format_impact(
    target: &str,
    nodes: &[ImpactNode],
    risk: &str,
    affected_flows: &[AffectedFlow],
    suggested_tests: &[SuggestedTest],
) -> String {
    let mut out = format!("=== IMPACT ANALYSIS: '{target}' ===\n\n");

    // High impact
    let high: Vec<&ImpactNode> = nodes.iter().filter(|n| n.impact_score > 0.5).collect();
    if !high.is_empty() {
        out.push_str("── HIGH IMPACT (score > 0.5) ──\n");
        for n in &high {
            out.push_str(&format!(
                "  [{:.2}] {} ({}) — {}\n",
                n.impact_score, n.name, n.entity_type, n.module_path
            ));
            for p in &n.paths {
                out.push_str(&format!(
                    "         via: {} (d{}, conf={:.2})\n",
                    p.path_type.label(),
                    p.hops,
                    p.confidence
                ));
            }
        }
        out.push('\n');
    }

    // Medium impact
    let med: Vec<&ImpactNode> = nodes
        .iter()
        .filter(|n| n.impact_score > 0.2 && n.impact_score <= 0.5)
        .collect();
    if !med.is_empty() {
        out.push_str("── MEDIUM IMPACT (0.2 - 0.5) ──\n");
        for n in &med {
            out.push_str(&format!(
                "  [{:.2}] {} ({}) — {}\n",
                n.impact_score, n.name, n.entity_type, n.module_path
            ));
            for p in &n.paths {
                out.push_str(&format!(
                    "         via: {} (d{}, conf={:.2})\n",
                    p.path_type.label(),
                    p.hops,
                    p.confidence
                ));
            }
        }
        out.push('\n');
    }

    // Low impact
    let low: Vec<&ImpactNode> = nodes.iter().filter(|n| n.impact_score <= 0.2).collect();
    if !low.is_empty() {
        out.push_str("── LOW IMPACT (< 0.2) ──\n");
        for n in &low {
            let path_labels: Vec<String> = n
                .paths
                .iter()
                .map(|p| p.path_type.label().to_string())
                .collect();
            out.push_str(&format!(
                "  [{:.2}] {} ({}) — {} [{}]\n",
                n.impact_score,
                n.name,
                n.entity_type,
                n.module_path,
                path_labels.join(", ")
            ));
        }
        out.push('\n');
    }

    // Affected flows
    if !affected_flows.is_empty() {
        out.push_str("── AFFECTED FLOWS ──\n");
        for f in affected_flows {
            out.push_str(&format!(
                "  [{}] {} — step {}/{}\n",
                f.entry_type, f.flow_name, f.step_position, f.total_steps
            ));
        }
        out.push('\n');
    }

    // Suggested tests
    if !suggested_tests.is_empty() {
        out.push_str("── SUGGESTED TESTS ──\n");
        for t in suggested_tests {
            out.push_str(&format!(
                "  {} ({}) — {}\n",
                t.entry_point, t.entry_type, t.reason
            ));
        }
        out.push('\n');
    }

    // Summary
    let total_score: f64 = nodes.iter().map(|n| n.impact_score).sum();
    out.push_str(&format!(
        "── SUMMARY ──\n\
         Risk level: {risk}\n\
         Total impact score: {total_score:.2}\n\
         High-impact nodes: {}\n\
         Medium-impact nodes: {}\n\
         Low-impact nodes: {}\n\
         Affected flows: {}\n\
         Suggested tests: {}\n",
        high.len(),
        med.len(),
        low.len(),
        affected_flows.len(),
        suggested_tests.len(),
    ));

    out
}

/// Combine per-seed impact collections into a `ChangeImpactReport` (pure, DB-free).
///
/// `changed` and `per_seed_edges` are index-aligned: `per_seed_edges[i]` holds
/// the raw blast-radius edges produced by `changed[i]`. `flows` / `tests` are the
/// concatenation of every seed's affected flows / suggested tests.
///
/// Steps: partition `changed` into with/without-impact by raw-edge count;
/// aggregate all edges once (dedups by node_id); filter by `min_score`;
/// self-exclude affected nodes whose `name` equals a changed symbol; truncate
/// to `max_nodes` (self-exclusion MUST happen before truncation — otherwise a
/// high-scoring changed seed can occupy a top slot and cause a genuinely
/// impacted node to be silently dropped); union+dedup flows (by `flow_name`)
/// and tests (by `entry_point`+`entry_type`+`flow_name`); compute `risk_level`
/// over the result.
pub fn aggregate_change_impacts(
    changed: Vec<String>,
    per_seed_edges: Vec<Vec<RawImpactEdge>>,
    flows: Vec<AffectedFlow>,
    tests: Vec<SuggestedTest>,
    min_score: f64,
    max_nodes: usize,
) -> ChangeImpactReport {
    debug_assert_eq!(
        changed.len(),
        per_seed_edges.len(),
        "changed and per_seed_edges must be index-aligned"
    );

    // Partition by whether the seed produced any raw edge.
    let mut with_impact: Vec<String> = Vec::new();
    let mut without_impact: Vec<String> = Vec::new();
    for (sym, edges) in changed.iter().zip(per_seed_edges.iter()) {
        if edges.is_empty() {
            without_impact.push(sym.clone());
        } else {
            with_impact.push(sym.clone());
        }
    }

    // Merge all seeds' edges and aggregate once (dedups by node_id).
    let all_edges: Vec<RawImpactEdge> = per_seed_edges.into_iter().flatten().collect();
    let mut nodes = aggregate_impacts(all_edges);
    if min_score > 0.0 {
        nodes.retain(|n| n.impact_score >= min_score);
    }

    // Self-exclude: a changed symbol is a cause, not an effect. (ImpactNode has
    // no `fqn`, so this matches on `name` only.) This MUST run before
    // truncation: otherwise a self-excluded node occupying a top slot wastes
    // it, and a genuinely impacted node that would have made the cut is
    // silently dropped.
    let changed_set: HashSet<&str> = changed.iter().map(String::as_str).collect();
    nodes.retain(|n| !changed_set.contains(n.name.as_str()));

    nodes.truncate(max_nodes);

    // Union + dedup flows and tests.
    let mut flows = flows;
    let mut flow_seen: HashSet<String> = HashSet::new();
    flows.retain(|f| flow_seen.insert(f.flow_name.clone()));

    let mut tests = tests;
    let mut test_seen: HashSet<(String, String, String)> = HashSet::new();
    tests.retain(|t| {
        test_seen.insert((
            t.entry_point.clone(),
            t.entry_type.clone(),
            t.flow_name.clone(),
        ))
    });

    let risk = risk_level(&nodes).to_string();

    ChangeImpactReport {
        changed,
        with_impact,
        without_impact,
        affected: nodes,
        flows,
        suggested_tests: tests,
        risk,
    }
}

/// Format a `ChangeImpactReport` into human-readable output (mirror of `format_impact`).
pub fn format_change_impact(report: &ChangeImpactReport) -> String {
    let mut out = String::from("=== CHANGE IMPACT ANALYSIS ===\n\n");

    // Summary line.
    out.push_str(&format!(
        "{} changed · {} with-impact · {} without-impact · {} affected · risk {}\n\n",
        report.changed.len(),
        report.with_impact.len(),
        report.without_impact.len(),
        report.affected.len(),
        report.risk,
    ));

    // Ranked affected nodes (already score-sorted by aggregate_impacts).
    if report.affected.is_empty() {
        out.push_str("── AFFECTED NODES ──\n  (none)\n\n");
    } else {
        out.push_str("── AFFECTED NODES (ranked) ──\n");
        for n in &report.affected {
            let path_labels: Vec<&str> = n.paths.iter().map(|p| p.path_type.label()).collect();
            out.push_str(&format!(
                "  [{:.2}] {} ({}) — {} [{}]\n",
                n.impact_score,
                n.name,
                n.entity_type,
                n.module_path,
                path_labels.join(", "),
            ));
        }
        out.push('\n');
    }

    // Affected flows.
    if !report.flows.is_empty() {
        out.push_str("── AFFECTED FLOWS ──\n");
        for f in &report.flows {
            out.push_str(&format!(
                "  [{}] {} — step {}/{}\n",
                f.entry_type, f.flow_name, f.step_position, f.total_steps,
            ));
        }
        out.push('\n');
    }

    // Suggested tests.
    if !report.suggested_tests.is_empty() {
        out.push_str("── SUGGESTED TESTS ──\n");
        for t in &report.suggested_tests {
            out.push_str(&format!(
                "  {} ({}) — {}\n",
                t.entry_point, t.entry_type, t.reason,
            ));
        }
        out.push('\n');
    }

    // Without-impact caveat line (only when non-empty).
    if !report.without_impact.is_empty() {
        out.push_str(&format!(
            "── WITHOUT IMPACT (not indexed, or no dependents) ──\n  {}\n",
            report.without_impact.join(", "),
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }

    #[test]
    fn path_score_direct_call() {
        // CalledBy, hop=1, confidence=1.0 → 1.0 * 0.6 * 1.0 = 0.6
        let score = path_score(ImpactPathType::CalledBy, 1, 1.0);
        assert!((score - 0.6).abs() < 1e-10);
    }

    #[test]
    fn path_score_indirect_call() {
        // CalledBy, hop=2, confidence=1.0 → 1.0 * 0.36 * 1.0 = 0.36
        let score = path_score(ImpactPathType::CalledBy, 2, 1.0);
        assert!((score - 0.36).abs() < 1e-10);
    }

    #[test]
    fn path_score_with_low_confidence() {
        // CalledBy, hop=1, confidence=0.6 → 1.0 * 0.6 * 0.6 = 0.36
        let score = path_score(ImpactPathType::CalledBy, 1, 0.6);
        assert!((score - 0.36).abs() < 1e-10);
    }

    #[test]
    fn aggregate_multi_path() {
        let edges = vec![
            RawImpactEdge {
                node_id: uuid(1),
                node_name: "func_a".into(),
                module_path: "mod/a".into(),
                entity_type: "function".into(),
                path_type: ImpactPathType::CalledBy,
                hops: 1,
                confidence: 1.0,
            },
            RawImpactEdge {
                node_id: uuid(1), // same node, different path
                node_name: "func_a".into(),
                module_path: "mod/a".into(),
                entity_type: "function".into(),
                path_type: ImpactPathType::ImportedBy,
                hops: 1,
                confidence: 1.0,
            },
        ];

        let nodes = aggregate_impacts(edges);
        assert_eq!(nodes.len(), 1);
        // max(0.6, 0.42) + 0.1 * (2-1) = 0.6 + 0.1 = 0.7
        assert!((nodes[0].impact_score - 0.7).abs() < 1e-10);
        assert_eq!(nodes[0].paths.len(), 2);
    }

    #[test]
    fn aggregate_sorted_by_score() {
        let edges = vec![
            RawImpactEdge {
                node_id: uuid(1),
                node_name: "low".into(),
                module_path: "m".into(),
                entity_type: "function".into(),
                path_type: ImpactPathType::NotedBy,
                hops: 1,
                confidence: 1.0,
            },
            RawImpactEdge {
                node_id: uuid(2),
                node_name: "high".into(),
                module_path: "m".into(),
                entity_type: "function".into(),
                path_type: ImpactPathType::CalledBy,
                hops: 1,
                confidence: 1.0,
            },
        ];

        let nodes = aggregate_impacts(edges);
        assert_eq!(nodes[0].name, "high");
        assert_eq!(nodes[1].name, "low");
    }

    #[test]
    fn risk_level_high() {
        let nodes = vec![ImpactNode {
            id: uuid(1),
            name: "x".into(),
            module_path: "m".into(),
            entity_type: "f".into(),
            impact_score: 0.9,
            paths: vec![],
        }];
        assert_eq!(risk_level(&nodes), "HIGH");
    }

    #[test]
    fn risk_level_medium() {
        let nodes = vec![
            ImpactNode {
                id: uuid(1),
                name: "a".into(),
                module_path: "m".into(),
                entity_type: "f".into(),
                impact_score: 0.6,
                paths: vec![],
            },
            ImpactNode {
                id: uuid(2),
                name: "b".into(),
                module_path: "m".into(),
                entity_type: "f".into(),
                impact_score: 0.5,
                paths: vec![],
            },
        ];
        assert_eq!(risk_level(&nodes), "MEDIUM");
    }

    #[test]
    fn risk_level_low() {
        let nodes = vec![ImpactNode {
            id: uuid(1),
            name: "x".into(),
            module_path: "m".into(),
            entity_type: "f".into(),
            impact_score: 0.3,
            paths: vec![],
        }];
        assert_eq!(risk_level(&nodes), "LOW");
    }

    #[test]
    fn change_impact_merges_and_dedups_by_node_id() {
        // Same node_id reached from two different seeds → one merged node.
        let edges_a = vec![RawImpactEdge {
            node_id: uuid(9),
            node_name: "shared".into(),
            module_path: "m".into(),
            entity_type: "function".into(),
            path_type: ImpactPathType::CalledBy,
            hops: 1,
            confidence: 1.0,
        }];
        let edges_b = vec![RawImpactEdge {
            node_id: uuid(9),
            node_name: "shared".into(),
            module_path: "m".into(),
            entity_type: "function".into(),
            path_type: ImpactPathType::ImportedBy,
            hops: 1,
            confidence: 1.0,
        }];
        let report = aggregate_change_impacts(
            vec!["a".into(), "b".into()],
            vec![edges_a, edges_b],
            vec![],
            vec![],
            0.0,
            100,
        );
        assert_eq!(report.affected.len(), 1, "same node_id from 2 seeds merges");
        assert_eq!(report.affected[0].paths.len(), 2);
        assert_eq!(report.with_impact, vec!["a".to_string(), "b".to_string()]);
        assert!(report.without_impact.is_empty());
    }

    #[test]
    fn change_impact_self_excludes_changed_symbols() {
        // Seed "a" reaches an affected node named "b" — but "b" is itself a
        // changed symbol (a cause, not an effect), so it must be dropped.
        let edges_a = vec![RawImpactEdge {
            node_id: uuid(2),
            node_name: "b".into(),
            module_path: "m".into(),
            entity_type: "function".into(),
            path_type: ImpactPathType::CalledBy,
            hops: 1,
            confidence: 1.0,
        }];
        let report = aggregate_change_impacts(
            vec!["a".into(), "b".into()],
            vec![edges_a, vec![]], // "b" produced nothing
            vec![],
            vec![],
            0.0,
            100,
        );
        assert!(
            report.affected.iter().all(|n| n.name != "b"),
            "changed symbol 'b' must be self-excluded"
        );
        assert_eq!(report.with_impact, vec!["a".to_string()]);
        assert_eq!(report.without_impact, vec!["b".to_string()]);
    }

    #[test]
    fn change_impact_self_exclude_before_truncate() {
        // 3 candidate nodes survive aggregate_impacts: "b" (itself a changed
        // seed, scores HIGHEST), "real1", "real2". With max_nodes=2, truncation
        // must not bind until AFTER self-exclusion — otherwise "b" occupies a
        // top slot, truncate(2) keeps [b, real1], and "real2" (which would have
        // made the cut) is silently dropped once "b" is removed afterward.
        let edges_a = vec![
            RawImpactEdge {
                node_id: uuid(2),
                node_name: "b".into(), // "b" is itself a changed seed below
                module_path: "m".into(),
                entity_type: "function".into(),
                path_type: ImpactPathType::CalledBy, // weight 1.0 -> score 0.6 (highest)
                hops: 1,
                confidence: 1.0,
            },
            RawImpactEdge {
                node_id: uuid(3),
                node_name: "real1".into(),
                module_path: "m".into(),
                entity_type: "function".into(),
                path_type: ImpactPathType::InFlow, // weight 0.9 -> score 0.54
                hops: 1,
                confidence: 1.0,
            },
            RawImpactEdge {
                node_id: uuid(4),
                node_name: "real2".into(),
                module_path: "m".into(),
                entity_type: "function".into(),
                path_type: ImpactPathType::ImportedBy, // weight 0.7 -> score 0.42
                hops: 1,
                confidence: 1.0,
            },
        ];

        let report = aggregate_change_impacts(
            vec!["a".into(), "b".into()],
            vec![edges_a, vec![]], // seed "b" itself produces nothing
            vec![],
            vec![],
            0.0,
            2, // max_nodes: small enough that truncation actually binds
        );

        let names: Vec<&str> = report.affected.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            report.affected.len(),
            2,
            "self-exclude must happen before truncate, else a wasted slot silently \
             drops a real impacted node; got {names:?}"
        );
        assert!(
            !names.contains(&"b"),
            "changed seed 'b' must never appear in affected: {names:?}"
        );
        assert!(names.contains(&"real1"), "expected real1 in {names:?}");
        assert!(names.contains(&"real2"), "expected real2 in {names:?}");
    }

    #[test]
    fn change_impact_unions_and_dedups_flows_and_tests() {
        let flow = AffectedFlow {
            flow_name: "F".into(),
            entry_point: "F".into(),
            entry_type: "api_handler".into(),
            step_position: 1,
            total_steps: 3,
        };
        let test = SuggestedTest {
            entry_point: "ep".into(),
            entry_type: "api_handler".into(),
            flow_name: "F".into(),
            reason: "r".into(),
        };
        let report = aggregate_change_impacts(
            vec!["a".into(), "b".into()],
            vec![vec![], vec![]],
            vec![flow.clone(), flow.clone()], // duplicate flow from 2 seeds
            vec![test.clone(), test.clone()], // duplicate test from 2 seeds
            0.0,
            100,
        );
        assert_eq!(report.flows.len(), 1, "duplicate flows dedup by flow_name");
        assert_eq!(report.suggested_tests.len(), 1, "duplicate tests dedup");
    }

    #[test]
    fn format_change_impact_includes_summary_and_caveat() {
        let report = ChangeImpactReport {
            changed: vec!["a".into(), "b".into(), "c".into()],
            with_impact: vec!["a".into(), "b".into()],
            without_impact: vec!["c".into()],
            affected: vec![ImpactNode {
                id: uuid(1),
                name: "caller".into(),
                module_path: "m".into(),
                entity_type: "function".into(),
                impact_score: 0.6,
                paths: vec![ImpactPath {
                    path_type: ImpactPathType::CalledBy,
                    hops: 1,
                    confidence: 1.0,
                    score: 0.6,
                }],
            }],
            flows: vec![],
            suggested_tests: vec![],
            risk: "MEDIUM".into(),
        };
        let out = format_change_impact(&report);
        assert!(
            out.contains("3 changed · 2 with-impact · 1 without-impact · 1 affected · risk MEDIUM"),
            "summary line missing/wrong:\n{out}"
        );
        assert!(out.contains("caller"), "affected node not rendered:\n{out}");
        assert!(
            out.contains("WITHOUT IMPACT"),
            "caveat line missing:\n{out}"
        );
    }
}
