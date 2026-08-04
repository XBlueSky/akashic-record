pub mod summarize;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tracing::info;
use uuid::Uuid;

use akashic_domain::algos::community::{run_leiden, union_find_components};
use akashic_domain::ports::{CommunityGraphRepo, CommunityRepo};

/// Detected community with its members.
#[derive(Debug, Clone)]
pub struct Community {
    pub level: u8,
    pub members: Vec<CommunityMember>,
}

#[derive(Debug, Clone)]
pub struct CommunityMember {
    pub pg_id: Uuid,
    pub name: String,
    pub member_type: String, // "chunk" or "module"
}

/// Run community detection for a repository.
///
/// Adaptive levels: <200 chunks → 2 levels, ≥200 → 3 levels.
/// Stores results in PostgreSQL communities + community_members tables,
/// and creates Neo4j Community nodes with HAS_MEMBER edges.
pub async fn detect_communities(
    community_graph: &Arc<dyn CommunityGraphRepo>,
    community: &Arc<dyn CommunityRepo>,
    repo_name: &str,
) -> Result<Vec<Vec<Community>>> {
    // 1. Load nodes (chunks + modules) and edges from Neo4j
    let (node_ids, node_meta, edges) = community_graph.load_graph_for_detection(repo_name).await?;

    if node_ids.is_empty() {
        info!(
            repo = repo_name,
            "No nodes found, skipping community detection"
        );
        return Ok(vec![]);
    }

    let node_count = node_ids.len();
    info!(
        repo = repo_name,
        nodes = node_count,
        edges = edges.len(),
        "Loaded graph for Leiden"
    );

    // 2. Determine resolutions based on repo size
    let resolutions: Vec<f64> = if node_count >= 200 {
        vec![1.0, 0.5, 0.1] // 3 levels
    } else {
        vec![1.0, 0.3] // 2 levels
    };

    // 3. Build index mappings
    let id_to_idx: HashMap<Uuid, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();

    // 4. Convert edges to (usize, usize, f64) for Leiden
    let weighted_edges: Vec<(usize, usize, f64)> = edges
        .iter()
        .filter_map(|(src, tgt, weight)| {
            let src_idx = id_to_idx.get(src)?;
            let tgt_idx = id_to_idx.get(tgt)?;
            Some((*src_idx, *tgt_idx, *weight))
        })
        .collect();

    // 5. Run Leiden at each resolution.
    // Precompute the union-find fallback partition once (deterministic, cheap):
    // used when Leiden times out, and directly for every remaining resolution
    // after the first timeout — the same graph hangs identically, so there's no
    // point paying another timeout + leaking another thread.
    let uf_edge_pairs: Vec<(usize, usize)> =
        weighted_edges.iter().map(|(s, t, _)| (*s, *t)).collect();
    let uf_fallback = union_find_components(node_count, &uf_edge_pairs);
    let mut leiden_hung = false;

    let mut all_levels = Vec::new();

    for (level, resolution) in resolutions.iter().enumerate() {
        let communities = if leiden_hung {
            uf_fallback.clone()
        } else {
            match run_leiden_bounded(node_count, weighted_edges.clone(), *resolution).await {
                Some(c) => c,
                None => {
                    leiden_hung = true;
                    tracing::warn!(
                        node_count,
                        resolution,
                        timeout_secs = LEIDEN_TIMEOUT_SECS,
                        "Leiden timed out (fa-leiden-cd inner-loop pathology); using union-find \
                         for this and remaining resolutions"
                    );
                    uf_fallback.clone()
                }
            }
        };

        // Group nodes by community
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for (node_idx, &community_id) in communities.iter().enumerate() {
            groups.entry(community_id).or_default().push(node_idx);
        }

        let level_communities: Vec<Community> = groups
            .into_values()
            .filter(|members| !members.is_empty())
            .map(|member_indices| Community {
                level: level as u8,
                members: member_indices
                    .into_iter()
                    .map(|idx| {
                        let id = node_ids[idx];
                        let (name, mtype) = node_meta.get(&id).cloned().unwrap_or_default();
                        CommunityMember {
                            pg_id: id,
                            name,
                            member_type: mtype,
                        }
                    })
                    .collect(),
            })
            .collect();

        info!(
            repo = repo_name,
            level,
            resolution,
            communities = level_communities.len(),
            "Leiden level complete"
        );

        all_levels.push(level_communities);
    }

    // 6. Store in PostgreSQL + Neo4j
    store_communities(community_graph, community, repo_name, &all_levels).await?;

    Ok(all_levels)
}

/// Seconds a single Leiden run may take before the watchdog abandons it and
/// falls back to union-find. Healthy graphs converge in well under a second;
/// this only fires on the `fa-leiden-cd` inner-loop non-termination pathology.
const LEIDEN_TIMEOUT_SECS: u64 = 30;

/// Run Leiden with a watchdog timeout. `fa-leiden-cd`'s inner local-move loops
/// can spin indefinitely on some real graphs (no inner iteration cap + a
/// random-move pitfall escape), which otherwise hangs Stage-9 forever. Runs the
/// (synchronous, CPU-bound) computation on a dedicated thread and awaits it with a
/// timeout: `Some(communities)` on success, `None` on timeout. The caller handles
/// `None` (union-find fallback + skips Leiden for the remaining resolutions). The
/// abandoned thread is detached — Rust can't interrupt a CPU-bound thread, so it
/// spins until process exit; an accepted cost versus an unbounded hang. Proper
/// long-term fix: patch/replace fa-leiden-cd to cap its inner loops.
async fn run_leiden_bounded(
    node_count: usize,
    edges: Vec<(usize, usize, f64)>,
    resolution: f64,
) -> Option<Vec<usize>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = run_leiden(node_count, &edges, resolution);
        let _ = tx.send(result); // receiver dropped on timeout → ignore
    });
    match tokio::time::timeout(std::time::Duration::from_secs(LEIDEN_TIMEOUT_SECS), rx).await {
        Ok(Ok(communities)) => Some(communities),
        _ => None,
    }
}

// run_leiden and union_find_components are provided by akashic_domain::algos::community
// (imported at the top of this file). BoundedModularityOptimizer is encapsulated
// inside the domain crate's run_leiden/try_leiden; this crate only keeps the
// async watchdog wrapper (run_leiden_bounded) that uses tokio.

/// Store communities in PostgreSQL and Neo4j via port traits.
async fn store_communities(
    community_graph: &Arc<dyn CommunityGraphRepo>,
    community: &Arc<dyn CommunityRepo>,
    repo_name: &str,
    levels: &[Vec<Community>],
) -> Result<()> {
    // Clean old data
    community.clean_communities(repo_name).await?;
    community_graph
        .delete_communities_by_repo(repo_name)
        .await?;

    // Insert new communities
    for level_communities in levels {
        for comm in level_communities {
            let cid = community
                .insert_community(repo_name, comm.level as i16, comm.members.len() as i32)
                .await?;

            // Insert members
            for member in &comm.members {
                if member.member_type == "chunk" {
                    community
                        .insert_community_chunk_member(cid, member.pg_id)
                        .await?;
                } else {
                    community
                        .insert_community_module_member(cid, member.pg_id)
                        .await?;
                }
            }

            // Neo4j Community node
            community_graph
                .create_community_node(cid, comm.level, repo_name, comm.members.len())
                .await?;

            // HAS_MEMBER edges
            for member in &comm.members {
                let label = if member.member_type == "chunk" {
                    "Chunk"
                } else {
                    "Module"
                };
                community_graph
                    .create_has_member_edge(cid, member.pg_id, label)
                    .await?;
            }
        }
    }

    info!(
        repo = repo_name,
        levels = levels.len(),
        total_communities = levels.iter().map(|l| l.len()).sum::<usize>(),
        "Stored communities"
    );

    Ok(())
}
