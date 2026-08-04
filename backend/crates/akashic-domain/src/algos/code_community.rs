//! Code call-graph community detection — pure, DB-free.
//!
//! Maps `CALLS` edges (with endpoint metadata) onto dense indices, runs the
//! shared Leiden kernel (`super::community::run_leiden`, which falls back to
//! union-find connected components), then groups, labels, and formats.

use std::collections::HashMap;

use super::community::run_leiden;

/// One `CALLS` edge with endpoint metadata, as fetched from the graph.
#[derive(Debug, Clone)]
pub struct CommunityEdge {
    pub source_id: String,
    pub source_name: String,
    pub source_module: String,
    pub target_id: String,
    pub target_name: String,
    pub target_module: String,
    pub weight: f64,
}

/// A member symbol of a community.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct CommunityMember {
    pub name: String,
    pub module: String,
}

/// A detected community, labeled by its dominant module path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Community {
    pub label: String,
    pub size: usize,
    pub members: Vec<CommunityMember>,
}

/// Detect communities from call edges via the shared Leiden kernel.
///
/// Returns communities sorted by size (desc). Members are sorted by
/// `(module, name)`. Nodes that never appear in an edge are absent by
/// construction, so there are no isolated singletons to special-case.
/// Note: the union-find fallback inside `run_leiden` is weight-blind — callers
/// wanting confidence-pruned clusters must set `min_confidence` > 0 upstream.
pub fn detect_communities(edges: &[CommunityEdge]) -> Vec<Community> {
    if edges.is_empty() {
        return Vec::new();
    }

    // Dense index map + per-index (name, module) metadata. String-keyed.
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut meta: Vec<CommunityMember> = Vec::new();
    let mut intern = |id: &str, name: &str, module: &str| -> usize {
        if let Some(&i) = index.get(id) {
            return i;
        }
        let i = meta.len();
        index.insert(id.to_string(), i);
        meta.push(CommunityMember {
            name: name.to_string(),
            module: module.to_string(),
        });
        i
    };

    let mut idx_edges: Vec<(usize, usize, f64)> = Vec::with_capacity(edges.len());
    for e in edges {
        let s = intern(&e.source_id, &e.source_name, &e.source_module);
        let t = intern(&e.target_id, &e.target_name, &e.target_module);
        idx_edges.push((s, t, e.weight));
    }
    let node_count = meta.len();

    let assignments = run_leiden(node_count, &idx_edges, 1.0);

    let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &cid) in assignments.iter().enumerate() {
        buckets.entry(cid).or_default().push(i);
    }

    let mut communities: Vec<Community> = buckets
        .into_values()
        .map(|node_idxs| {
            let mut members: Vec<CommunityMember> =
                node_idxs.iter().map(|&i| meta[i].clone()).collect();
            members.sort_by(|a, b| (&a.module, &a.name).cmp(&(&b.module, &b.name)));
            let label = dominant_module(&members);
            Community {
                label,
                size: members.len(),
                members,
            }
        })
        .collect();

    communities.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.members.cmp(&b.members))
    });
    communities
}

/// Most frequent module path among members (mode), lexicographic tie-break.
fn dominant_module(members: &[CommunityMember]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for m in members {
        *counts.entry(m.module.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(module, _)| module.to_string())
        .unwrap_or_default()
}

/// Render the community report as an agent-facing string.
pub fn format_communities(
    repo: &str,
    communities: &[Community],
    max_communities: usize,
    max_members: usize,
) -> String {
    if communities.is_empty() {
        return format!(
            "No communities found in '{repo}' (the call graph has no CALLS edges, \
             or none above the confidence floor)."
        );
    }
    let mut out = format!(
        "Call-graph communities in '{repo}' ({} total, Leiden clustering).\n\
         Symbols with no CALLS edge are not shown.\n\n",
        communities.len()
    );
    for (rank, c) in communities.iter().take(max_communities).enumerate() {
        out.push_str(&format!(
            "#{} · {} members · label: {}\n",
            rank + 1,
            c.size,
            c.label
        ));
        for m in c.members.iter().take(max_members) {
            out.push_str(&format!("   - {} · {}\n", m.name, m.module));
        }
        if c.members.len() > max_members {
            out.push_str(&format!(
                "   … (+{} more, truncated)\n",
                c.members.len() - max_members
            ));
        }
        out.push('\n');
    }
    if communities.len() > max_communities {
        out.push_str(&format!(
            "(+{} more communities, truncated — raise `max_communities`)\n",
            communities.len() - max_communities
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(sn: &str, sm: &str, tn: &str, tm: &str) -> CommunityEdge {
        CommunityEdge {
            source_id: sn.into(),
            source_name: sn.into(),
            source_module: sm.into(),
            target_id: tn.into(),
            target_name: tn.into(),
            target_module: tm.into(),
            weight: 1.0,
        }
    }

    #[test]
    fn empty_edges_yields_no_communities() {
        assert!(detect_communities(&[]).is_empty());
    }

    #[test]
    fn two_disconnected_pairs_group_separately() {
        let edges = vec![
            edge("a", "m1", "b", "m1"),
            edge("b", "m1", "a", "m1"),
            edge("c", "m2", "d", "m2"),
            edge("d", "m2", "c", "m2"),
        ];
        let comms = detect_communities(&edges);
        let group_of = |name: &str| -> usize {
            comms
                .iter()
                .position(|c| c.members.iter().any(|m| m.name == name))
                .unwrap()
        };
        assert_eq!(group_of("a"), group_of("b"));
        assert_eq!(group_of("c"), group_of("d"));
        assert_ne!(group_of("a"), group_of("c"));
    }

    #[test]
    fn community_containing_a_is_labeled_core() {
        // 3 nodes; module "core" is the mode. Robust to whether Leiden keeps
        // them as one community or splits {x} off: the community with `a` is core.
        let edges = vec![
            edge("a", "core", "b", "core"),
            edge("b", "core", "x", "util"),
        ];
        let comms = detect_communities(&edges);
        let a_comm = comms
            .iter()
            .find(|c| c.members.iter().any(|m| m.name == "a"))
            .unwrap();
        assert_eq!(a_comm.label, "core");
    }

    #[test]
    fn sorted_by_size_desc() {
        let edges = vec![
            edge("a", "m1", "b", "m1"),
            edge("b", "m1", "c", "m1"),
            edge("p", "m2", "q", "m2"),
        ];
        let comms = detect_communities(&edges);
        assert!(comms.len() >= 2);
        assert!(comms[0].size >= comms[1].size);
    }

    #[test]
    fn format_caps_and_notes() {
        let comms = vec![Community {
            label: "m1".into(),
            size: 2,
            members: vec![
                CommunityMember {
                    name: "a".into(),
                    module: "m1".into(),
                },
                CommunityMember {
                    name: "b".into(),
                    module: "m1".into(),
                },
            ],
        }];
        let out = format_communities("acme", &comms, 10, 1);
        assert!(out.contains("acme"));
        assert!(out.contains("m1"));
        assert!(out.to_lowercase().contains("truncat"));
    }

    #[test]
    fn format_empty() {
        let out = format_communities("acme", &[], 10, 10);
        assert!(out.to_lowercase().contains("no communit"));
    }

    #[test]
    fn ordering_is_deterministic_on_size_label_collision() {
        // Two disconnected same-module pairs → two communities with identical
        // (size, label). Output order must be stable across runs.
        let edges = vec![
            edge("a", "m1", "b", "m1"),
            edge("b", "m1", "a", "m1"),
            edge("c", "m1", "d", "m1"),
            edge("d", "m1", "c", "m1"),
        ];
        let first = detect_communities(&edges);
        let second = detect_communities(&edges);
        assert_eq!(first, second, "community ordering must be deterministic");
    }
}
