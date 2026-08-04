//! Community detection algorithms — pure, DB-free.
//!
//! Moved from `akashic-ingestion::community::mod` (A1 Task 2).
//! The async `run_leiden_bounded` (which uses tokio timeout) stays in the
//! ingestion crate — only the synchronous, CPU-bound kernels live here.

use std::collections::HashMap;

/// Attempt to run Leiden community detection using the `fa-leiden-cd` crate.
///
/// Returns `None` if Leiden panics or returns an unexpected result. Callers
/// should fall back to `union_find_components` on `None`.
pub fn try_leiden(
    node_count: usize,
    edges: &[(usize, usize)],
    weights: &[f64],
    resolution: f64,
) -> Option<Vec<usize>> {
    use fa_leiden_cd::Graph;

    std::panic::catch_unwind(|| {
        let mut graph = Graph::new();

        for _ in 0..node_count {
            graph.add_node(());
        }

        for (i, &(src, tgt)) in edges.iter().enumerate() {
            let base_w = weights.get(i).copied().unwrap_or(1.0);
            let w = (base_w * resolution) as f32;
            graph.add_edge(src, tgt, (), w);
        }

        let mut optimizer = BoundedModularityOptimizer {
            parallel_scale: 1000,
            tol: 1e-8,
            sweeps: 0,
            max_sweeps: 300,
        };
        const MAX_LEIDEN_PASSES: usize = 32;
        let result_graph = graph.leiden(Some(MAX_LEIDEN_PASSES), &mut optimizer);

        let assignments = std::cell::RefCell::new(vec![0usize; node_count]);
        for (community_id, community) in result_graph.node_data_slice().iter().enumerate() {
            community.collect_nodes(&|node_idx| {
                if node_idx < node_count {
                    assignments.borrow_mut()[node_idx] = community_id;
                }
            });
        }
        assignments.into_inner()
    })
    .ok()
}

/// Run Leiden community detection, falling back to union-find on failure.
pub fn run_leiden(node_count: usize, edges: &[(usize, usize, f64)], resolution: f64) -> Vec<usize> {
    let edge_pairs: Vec<(usize, usize)> = edges.iter().map(|(s, t, _)| (*s, *t)).collect();
    let weights: Vec<f64> = edges.iter().map(|(_, _, w)| *w).collect();

    match try_leiden(node_count, &edge_pairs, &weights, resolution) {
        Some(communities) if communities.len() == node_count => communities,
        _ => {
            tracing::warn!(
                node_count,
                resolution,
                "Leiden returned unexpected result, falling back to union-find components"
            );
            union_find_components(node_count, &edge_pairs)
        }
    }
}

/// Greedy union-find connected-components fallback partitioning.
///
/// Each connected component becomes its own community. Single-node components
/// are all assigned community 0 to avoid degenerate cases.
pub fn union_find_components(node_count: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..node_count).collect();

    fn find(parent: &mut Vec<usize>, x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }

    for &(u, v) in edges {
        if u < node_count && v < node_count {
            let pu = find(&mut parent, u);
            let pv = find(&mut parent, v);
            if pu != pv {
                parent[pv] = pu;
            }
        }
    }

    let mut root_to_community: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    let mut result = vec![0usize; node_count];
    for (i, item) in result.iter_mut().enumerate().take(node_count) {
        let root = find(&mut parent, i);
        let cid = *root_to_community.entry(root).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });
        *item = cid;
    }
    result
}

/// Sweep-capped modularity optimizer for Leiden.
///
/// Bounds the inner local-move loops to prevent the never-converging oscillation
/// found on some real graphs. Healthy graphs converge well within `max_sweeps`.
struct BoundedModularityOptimizer {
    parallel_scale: usize,
    tol: f32,
    sweeps: usize,
    max_sweeps: usize,
}

impl fa_leiden_cd::ModularityOptimizer for BoundedModularityOptimizer {
    fn is_converged(&mut self, previous: f32, current: f32) -> bool {
        self.sweeps += 1;
        if self.sweeps >= self.max_sweeps {
            return true;
        }
        previous - current < self.tol
    }

    fn get_parallel_threshold(&self) -> usize {
        self.parallel_scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_find_two_components() {
        // 4 nodes: 0-1 connected, 2-3 connected → 2 communities
        let edges = vec![(0, 1), (2, 3)];
        let result = union_find_components(4, &edges);
        assert_eq!(result.len(), 4);
        // 0 and 1 share a community
        assert_eq!(result[0], result[1]);
        // 2 and 3 share a community
        assert_eq!(result[2], result[3]);
        // The two groups are distinct
        assert_ne!(result[0], result[2]);
    }

    #[test]
    fn union_find_fully_connected() {
        let edges = vec![(0, 1), (1, 2), (2, 3)];
        let result = union_find_components(4, &edges);
        assert_eq!(result.len(), 4);
        // All in the same community
        assert_eq!(result[0], result[1]);
        assert_eq!(result[1], result[2]);
        assert_eq!(result[2], result[3]);
    }

    #[test]
    fn union_find_no_edges() {
        let result = union_find_components(3, &[]);
        assert_eq!(result.len(), 3);
        // Each node is its own component
        assert_ne!(result[0], result[1]);
        assert_ne!(result[1], result[2]);
        assert_ne!(result[0], result[2]);
    }

    #[test]
    fn union_find_out_of_bounds_edges_ignored() {
        // Edge (5, 6) is out of bounds for node_count=4 — must not panic
        let edges = vec![(0, 1), (5, 6)];
        let result = union_find_components(4, &edges);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], result[1]);
    }
}
