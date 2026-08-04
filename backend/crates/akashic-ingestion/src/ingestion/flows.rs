use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::entry_points::DetectedEntryPoint;

const MAX_DEPTH: u32 = 8;
const MAX_NODES: u32 = 50;

/// A complete execution flow from an entry point.
#[derive(Debug, Clone)]
pub struct ExecutionFlow {
    pub entry_point: DetectedEntryPoint,
    pub steps: Vec<FlowStep>,
    pub truncated: bool,
}

/// A single step in an execution flow.
#[derive(Debug, Clone)]
pub struct FlowStep {
    pub chunk_id: Uuid,
    pub chunk_name: String,
    pub module_path: String,
    pub position: u32,
    pub depth: u32,
}

/// In-memory equivalent of the old Neo4j-backed `load_calls_adjacency` (Roadmap F) — builds the
/// same `HashMap<Uuid, Vec<(Uuid, String, String)>>` shape `build_single_flow`
/// expects, but from THIS RUN's accumulated CALLS edges + chunk list instead
/// of a Neo4j query, since under RAM-first nothing has been written to Neo4j
/// yet when this stage runs.
///
/// `chunk_by_id` is sourced from `acc.chunk_index_source`, NOT `acc.pg.chunks`
/// (Roadmap F, Task 7.5 fix): a CALLS edge whose TARGET is a byte-matched/
/// reused chunk could not be resolved into this adjacency map at all when
/// this only looked at the delta-only `acc.pg.chunks` — the same visibility
/// bug Stage 6's `ChunkIndex` had. See `ChunkIndexEntry`'s doc comment for
/// the full rationale.
pub(crate) fn calls_adjacency_from_accumulator(
    acc: &crate::ingestion::accumulator::IngestAccumulator,
) -> HashMap<Uuid, Vec<(Uuid, String, String)>> {
    let chunk_by_id: HashMap<Uuid, &crate::ingestion::accumulator::ChunkIndexEntry> =
        acc.chunk_index_source.iter().map(|c| (c.id, c)).collect();

    let mut adj: HashMap<Uuid, Vec<(Uuid, String, String)>> = HashMap::new();
    for edge in &acc.graph.calls_edges {
        if let Some(tgt) = chunk_by_id.get(&edge.tgt_chunk_id) {
            adj.entry(edge.src_chunk_id).or_default().push((
                edge.tgt_chunk_id,
                tgt.name.clone(),
                tgt.module_path.clone(),
            ));
        }
    }
    adj
}

/// Build one entry point's execution flow by DFS-traversing `adjacency`.
///
/// `pub(crate)` (Roadmap F, Task 6): widened from private so `stages.rs` can
/// call it directly for the resolve-only Stage 7 path (formerly only
/// `build_flows`, in this same module, called it). No behavior change; the
/// function itself is untouched — only its visibility.
pub(crate) fn build_single_flow(
    ep: &DetectedEntryPoint,
    adjacency: &HashMap<Uuid, Vec<(Uuid, String, String)>>,
) -> ExecutionFlow {
    let mut steps = Vec::new();
    let mut visited = HashSet::new();
    let mut position: u32 = 0;
    let mut truncated = false;

    dfs(
        ep.chunk_id,
        &ep.chunk_name,
        &ep.module_path,
        0,
        adjacency,
        &mut visited,
        &mut steps,
        &mut position,
        &mut truncated,
    );

    ExecutionFlow {
        entry_point: ep.clone(),
        steps,
        truncated,
    }
}

#[allow(clippy::too_many_arguments)]
fn dfs(
    chunk_id: Uuid,
    chunk_name: &str,
    module_path: &str,
    depth: u32,
    adjacency: &HashMap<Uuid, Vec<(Uuid, String, String)>>,
    visited: &mut HashSet<Uuid>,
    steps: &mut Vec<FlowStep>,
    position: &mut u32,
    truncated: &mut bool,
) {
    if *truncated || depth > MAX_DEPTH || !visited.insert(chunk_id) {
        return;
    }

    if *position >= MAX_NODES {
        *truncated = true;
        return;
    }

    steps.push(FlowStep {
        chunk_id,
        chunk_name: chunk_name.to_string(),
        module_path: module_path.to_string(),
        position: *position,
        depth,
    });
    *position += 1;

    if let Some(callees) = adjacency.get(&chunk_id) {
        let mut sorted_callees = callees.clone();
        sorted_callees.sort_by(|a, b| a.1.cmp(&b.1));

        for (callee_id, callee_name, callee_module) in sorted_callees {
            dfs(
                callee_id,
                &callee_name,
                &callee_module,
                depth + 1,
                adjacency,
                visited,
                steps,
                position,
                truncated,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingestion::entry_points::DetectedEntryPoint;

    fn make_ep(id_byte: u8, name: &str) -> DetectedEntryPoint {
        DetectedEntryPoint {
            chunk_id: Uuid::from_bytes([id_byte, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            chunk_name: name.to_string(),
            module_path: "test/module".to_string(),
            entry_type: "api_handler".to_string(),
            display_name: format!("GET /{name}"),
        }
    }

    fn uuid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }

    #[test]
    fn test_build_linear_flow() {
        let mut adj = HashMap::new();
        adj.insert(uuid(1), vec![(uuid(2), "B".into(), "mod".into())]);
        adj.insert(uuid(2), vec![(uuid(3), "C".into(), "mod".into())]);

        let ep = make_ep(1, "A");
        let flow = build_single_flow(&ep, &adj);

        assert_eq!(flow.steps.len(), 3);
        assert_eq!(flow.steps[0].chunk_name, "A");
        assert_eq!(flow.steps[0].position, 0);
        assert_eq!(flow.steps[0].depth, 0);
        assert_eq!(flow.steps[1].chunk_name, "B");
        assert_eq!(flow.steps[1].position, 1);
        assert_eq!(flow.steps[1].depth, 1);
        assert_eq!(flow.steps[2].chunk_name, "C");
        assert_eq!(flow.steps[2].position, 2);
        assert_eq!(flow.steps[2].depth, 2);
        assert!(!flow.truncated);
    }

    #[test]
    fn test_build_branching_flow() {
        let mut adj = HashMap::new();
        adj.insert(
            uuid(1),
            vec![
                (uuid(3), "C".into(), "mod".into()),
                (uuid(2), "B".into(), "mod".into()),
            ],
        );

        let ep = make_ep(1, "A");
        let flow = build_single_flow(&ep, &adj);

        assert_eq!(flow.steps.len(), 3);
        assert_eq!(flow.steps[0].chunk_name, "A");
        assert_eq!(flow.steps[1].chunk_name, "B");
        assert_eq!(flow.steps[2].chunk_name, "C");
    }

    #[test]
    fn test_cycle_prevention() {
        let mut adj = HashMap::new();
        adj.insert(uuid(1), vec![(uuid(2), "B".into(), "mod".into())]);
        adj.insert(uuid(2), vec![(uuid(1), "A".into(), "mod".into())]);

        let ep = make_ep(1, "A");
        let flow = build_single_flow(&ep, &adj);

        assert_eq!(flow.steps.len(), 2);
        assert!(!flow.truncated);
    }

    #[test]
    fn test_no_callees() {
        let adj = HashMap::new();
        let ep = make_ep(1, "isolated");
        let flow = build_single_flow(&ep, &adj);

        assert_eq!(flow.steps.len(), 1);
    }
}
