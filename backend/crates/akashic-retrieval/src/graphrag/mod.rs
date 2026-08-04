pub mod impact;
pub mod query_analyzer;
pub mod rrf;
pub mod symbol_resolution;
pub mod types;

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use akashic_domain::ports::{ChunkRepo, DocumentRepo, GraphTraversalRepo, ModuleRepo, NoteRepo};
use akashic_domain::types::{GraphNeighbour, Space};
use akashic_embed::EmbeddingProvider;
use types::*;

/// Convert SeedNodes to RankedItems for RRF fusion (rank = position in the vec).
fn to_ranked(seeds: &[SeedNode]) -> Vec<rrf::RankedItem> {
    seeds
        .iter()
        .enumerate()
        .map(|(rank, s)| rrf::RankedItem {
            id: s.pg_id,
            rank,
            metadata: rrf::RankedItemMeta {
                space: s.space,
                entity_type: s.entity_type.clone(),
                name: s.name.clone(),
            },
        })
        .collect()
}

pub struct GraphRagService {
    chunk_repo: Arc<dyn ChunkRepo>,
    doc_repo: Arc<dyn DocumentRepo>,
    note_repo: Arc<dyn NoteRepo>,
    module_repo: Arc<dyn ModuleRepo>,
    traversal_repo: Arc<dyn GraphTraversalRepo>,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl GraphRagService {
    pub fn new(
        chunk_repo: Arc<dyn ChunkRepo>,
        doc_repo: Arc<dyn DocumentRepo>,
        note_repo: Arc<dyn NoteRepo>,
        module_repo: Arc<dyn ModuleRepo>,
        traversal_repo: Arc<dyn GraphTraversalRepo>,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            chunk_repo,
            doc_repo,
            note_repo,
            module_repo,
            traversal_repo,
            embedder,
        }
    }

    // ── Full pipeline: scatter → expand → rank → assemble ──────────────

    pub async fn query(&self, req: &GraphRagQuery) -> Result<GraphRagResponse> {
        let k = req.k_per_space.unwrap_or(3);
        let budget = req.token_budget.unwrap_or(64_000);

        // Stage 1
        let query_embedding = self.embedder.embed(&req.query).await?.vector;
        let seeds = self
            .scatter_search(
                &req.query,
                &query_embedding,
                req.repo_name.as_deref(),
                k,
                req.prefer_space,
            )
            .await?;

        // Stage 2
        let expanded = self.graph_expand(&seeds).await?;

        // Stage 3
        let ranked = Self::rank_and_prune(expanded, budget);

        // Stage 4
        let total_tokens = ranked.iter().map(|n| n.token_count).sum();
        let context = Self::assemble_context(&ranked);

        Ok(GraphRagResponse {
            context,
            nodes_used: ranked,
            total_tokens,
        })
    }

    // ── Stage 1: Dual-level scatter search with RRF across 3 spaces ────

    async fn scatter_search(
        &self,
        query: &str,
        embedding: &[f32],
        repo: Option<&str>,
        k: usize,
        prefer_space: Option<PreferSpace>,
    ) -> Result<Vec<SeedNode>> {
        let analysis = query_analyzer::analyze(query, embedding);

        // 8 parallel queries: 2 levels × 3 spaces + signature + large chunks
        let (code_bm25, code_vec, code_sig, large_chunks, doc_bm25, doc_vec, note_bm25, note_vec) =
            tokio::try_join!(
                self.search_code_bm25(query, repo, k),
                self.search_code_space(embedding, repo, k),
                self.search_code_signature(embedding, repo, k),
                self.search_large_chunks(embedding, repo, k),
                self.search_doc_bm25(query, repo, k),
                self.search_doc_space(embedding, repo, k),
                self.search_note_bm25(query, repo, k),
                self.search_human_space(embedding, repo, k),
            )?;

        let low_w = analysis.low_level_weight;
        let high_w = analysis.high_level_weight;
        let k_rrf = 60.0;

        // Per-space RRF fusion
        let code_fused = rrf::multi_source_rrf(
            &[
                (to_ranked(&code_bm25), low_w),
                (to_ranked(&code_vec), high_w),
                (code_sig, high_w * 1.2),
                (large_chunks, high_w * 0.8),
            ],
            k_rrf,
        );
        let doc_fused = rrf::weighted_rrf(
            &to_ranked(&doc_bm25),
            &to_ranked(&doc_vec),
            low_w,
            high_w,
            k_rrf,
        );
        let note_fused = rrf::weighted_rrf(
            &to_ranked(&note_bm25),
            &to_ranked(&note_vec),
            low_w,
            high_w,
            k_rrf,
        );

        // Cross-space merge with optional preference
        let prefer = prefer_space.map(akashic_domain::types::PreferSpace::to_space);
        let merged = rrf::cross_space_merge(vec![code_fused, doc_fused, note_fused], prefer, 1.5);

        // Convert back to SeedNode, take top k*3
        let seeds: Vec<SeedNode> = merged
            .into_iter()
            .take(k * 3)
            .map(|f| SeedNode {
                pg_id: f.id,
                space: f.metadata.space,
                entity_type: f.metadata.entity_type,
                name: f.metadata.name,
                score: f.rrf_score as f32,
            })
            .collect();

        Ok(seeds)
    }

    async fn search_code_space(
        &self,
        embedding: &[f32],
        repo: Option<&str>,
        k: usize,
    ) -> Result<Vec<SeedNode>> {
        // SQL moved to ChunkRepo::search_chunks_by_vector (via PgChunkRepo).
        // SQL verbatim from original search_code_space.
        let rows = self
            .chunk_repo
            .search_chunks_by_vector(embedding, repo, k as i64)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| SeedNode {
                pg_id: r.id,
                space: Space::Code,
                entity_type: r.chunk_type,
                name: r.name,
                score: r.score,
            })
            .collect())
    }

    async fn search_doc_space(
        &self,
        embedding: &[f32],
        repo: Option<&str>,
        k: usize,
    ) -> Result<Vec<SeedNode>> {
        // SQL moved to DocumentRepo::search_sections_by_vector (via PgDocumentRepo).
        let rows = self
            .doc_repo
            .search_sections_by_vector(embedding, repo, k as i64)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| SeedNode {
                pg_id: r.id,
                space: Space::Doc,
                entity_type: "section".into(),
                name: r.heading,
                score: r.score,
            })
            .collect())
    }

    async fn search_human_space(
        &self,
        embedding: &[f32],
        repo: Option<&str>,
        k: usize,
    ) -> Result<Vec<SeedNode>> {
        // SQL moved to NoteRepo::search_notes_by_vector (via PgNoteRepo).
        let rows = self
            .note_repo
            .search_notes_by_vector(embedding, repo, k as i64)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| SeedNode {
                pg_id: r.id,
                space: Space::Human,
                entity_type: "note".into(),
                name: r.title,
                score: r.score,
            })
            .collect())
    }

    // ── Signature & large chunk vector search ─────────────────────────

    async fn search_code_signature(
        &self,
        embedding: &[f32],
        repo: Option<&str>,
        k: usize,
    ) -> Result<Vec<rrf::RankedItem>> {
        // SQL moved to ChunkRepo::search_chunks_by_signature (via PgChunkRepo).
        let rows = self
            .chunk_repo
            .search_chunks_by_signature(embedding, repo, k as i64)
            .await?;
        Ok(rows
            .into_iter()
            .enumerate()
            .map(|(rank, (id, name, _score))| rrf::RankedItem {
                id,
                rank,
                metadata: rrf::RankedItemMeta {
                    name,
                    space: Space::Code,
                    entity_type: "chunk".into(),
                },
            })
            .collect())
    }

    async fn search_large_chunks(
        &self,
        embedding: &[f32],
        repo: Option<&str>,
        k: usize,
    ) -> Result<Vec<rrf::RankedItem>> {
        // SQL moved to ChunkRepo::search_large_chunks_by_vector (via PgChunkRepo).
        let rows = self
            .chunk_repo
            .search_large_chunks_by_vector(embedding, repo, k as i64)
            .await?;
        Ok(rows
            .into_iter()
            .enumerate()
            .map(|(rank, r)| rrf::RankedItem {
                id: r.id,
                rank,
                metadata: rrf::RankedItemMeta {
                    name: r.name,
                    space: Space::Code,
                    entity_type: "large_chunk".into(),
                },
            })
            .collect())
    }

    // ── BM25 keyword search methods (for dual-level scatter) ─────────

    async fn search_code_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        k: usize,
    ) -> Result<Vec<SeedNode>> {
        // SQL moved to ChunkRepo::search_chunks_by_bm25 (via PgChunkRepo).
        let rows = self
            .chunk_repo
            .search_chunks_by_bm25(query, repo, k as i64)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| SeedNode {
                pg_id: r.id,
                space: Space::Code,
                entity_type: r.chunk_type,
                name: r.name,
                score: 0.0,
            })
            .collect())
    }

    async fn search_doc_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        k: usize,
    ) -> Result<Vec<SeedNode>> {
        // SQL moved to DocumentRepo::search_sections_by_bm25 (via PgDocumentRepo).
        let rows = self
            .doc_repo
            .search_sections_by_bm25(query, repo, k as i64)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| SeedNode {
                pg_id: r.id,
                space: Space::Doc,
                entity_type: "section".into(),
                name: r.heading,
                score: 0.0,
            })
            .collect())
    }

    async fn search_note_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        k: usize,
    ) -> Result<Vec<SeedNode>> {
        // SQL moved to NoteRepo::search_notes_by_bm25 (via PgNoteRepo).
        let rows = self
            .note_repo
            .search_notes_by_bm25(query, repo, k as i64)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| SeedNode {
                pg_id: r.id,
                space: Space::Human,
                entity_type: "note".into(),
                name: r.title,
                score: 0.0,
            })
            .collect())
    }

    // ── Stage 2: Graph expand — traverse Neo4j neighbours ──────────────

    async fn graph_expand(&self, seeds: &[SeedNode]) -> Result<Vec<ExpandedNode>> {
        let mut expanded: Vec<ExpandedNode> = Vec::new();
        let mut seen: HashSet<Uuid> = HashSet::new();

        // Add seeds as hop=0
        for seed in seeds {
            seen.insert(seed.pg_id);
            if let Some(node) = self
                .fetch_full_node(
                    seed.pg_id,
                    &seed.space,
                    &seed.entity_type,
                    &seed.name,
                    Some(seed.score),
                    0,
                    None,
                )
                .await?
            {
                expanded.push(node);
            }
        }

        // Hop 1: Expand each seed via Neo4j relationships
        for seed in seeds {
            let neighbor_ids = match seed.space {
                Space::Code => self.expand_chunk(seed.pg_id).await?,
                Space::Doc => self.traversal_repo.expand_section(seed.pg_id).await?,
                Space::Human => self.traversal_repo.expand_note(seed.pg_id).await?,
            };

            for GraphNeighbour {
                pg_id,
                space,
                entity_type,
                name,
                hop,
            } in neighbor_ids
            {
                if seen.insert(pg_id)
                    && let Some(node) = self
                        .fetch_full_node(
                            pg_id,
                            &space,
                            &entity_type,
                            &name,
                            None,
                            hop,
                            Some(seed.score),
                        )
                        .await?
                {
                    expanded.push(node);
                }
            }
        }

        // Hop 2: Cross-space expansion from hop-1 nodes
        // Only expand hop-1 nodes whose space differs from their parent seed's space
        let hop1_nodes: Vec<(Uuid, Space, f32)> = expanded
            .iter()
            .filter(|n| n.hop_distance == 1)
            .map(|n| (n.pg_id, n.space, n.parent_score.unwrap_or(0.0)))
            .collect();

        let max_expanded = 30;
        for (pg_id, space, ps) in hop1_nodes {
            if expanded.len() >= max_expanded {
                break;
            }
            let neighbors = match space {
                Space::Code => self.expand_chunk(pg_id).await?,
                Space::Doc => self.traversal_repo.expand_section(pg_id).await?,
                Space::Human => self.traversal_repo.expand_note(pg_id).await?,
            };
            for GraphNeighbour {
                pg_id: nb_id,
                space: nb_space,
                entity_type: nb_etype,
                name: nb_name,
                ..
            } in neighbors
            {
                if expanded.len() >= max_expanded {
                    break;
                }
                if seen.insert(nb_id)
                    && let Some(node) = self
                        .fetch_full_node(
                            nb_id,
                            &nb_space,
                            &nb_etype,
                            &nb_name,
                            None,
                            2,
                            Some(ps * 0.7), // further decay
                        )
                        .await?
                {
                    expanded.push(node);
                }
            }
        }

        Ok(expanded)
    }

    /// Expand a Chunk seed: wraps traversal_repo.expand_chunk and handles
    /// the flow entry-point sub-expansion inline.
    async fn expand_chunk(&self, pg_id: Uuid) -> Result<Vec<GraphNeighbour>> {
        let raw = self.traversal_repo.expand_chunk(pg_id).await?;
        let mut neighbors: Vec<GraphNeighbour> = Vec::new();

        for nb in raw {
            if nb.entity_type == "__flow__" {
                // Resolve flow entry-point chunks
                let ep_ids = self
                    .traversal_repo
                    .find_flow_entry_points(nb.pg_id)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "Flow entry point query failed");
                        vec![]
                    });
                for ep_uuid in ep_ids {
                    neighbors.push(GraphNeighbour {
                        pg_id: ep_uuid,
                        space: Space::Code,
                        entity_type: "chunk".into(),
                        name: String::new(),
                        hop: 1,
                    });
                }
            } else {
                neighbors.push(nb);
            }
        }

        Ok(neighbors)
    }

    /// Fetch full content for a node from PostgreSQL via repo ports.
    #[allow(clippy::too_many_arguments)]
    async fn fetch_full_node(
        &self,
        pg_id: Uuid,
        space: &Space,
        entity_type: &str,
        name: &str,
        vector_score: Option<f32>,
        hop: u8,
        parent_score: Option<f32>,
    ) -> Result<Option<ExpandedNode>> {
        match space {
            Space::Code => {
                if entity_type == "module" {
                    // SQL moved to ModuleRepo::fetch_module_summary (via PgModuleRepo).
                    let row = self.module_repo.fetch_module_summary(pg_id).await?;
                    Ok(row.map(|s| ExpandedNode {
                        pg_id,
                        space: Space::Code,
                        entity_type: "module".into(),
                        name: s.path.clone(),
                        vector_score,
                        parent_score,
                        hop_distance: hop,
                        content: s.summary.unwrap_or_default(),
                        module_path: Some(s.path),
                        language: s.language,
                        document_title: None,
                        heading: None,
                        author: None,
                        category: None,
                    }))
                } else if entity_type == "large_chunk" {
                    // SQL moved to ChunkRepo::fetch_large_chunk_content (via PgChunkRepo).
                    let row = self.chunk_repo.fetch_large_chunk_content(pg_id).await?;
                    Ok(row.map(|r| ExpandedNode {
                        pg_id,
                        space: Space::Code,
                        entity_type: "large_chunk".into(),
                        name: name.into(),
                        vector_score,
                        parent_score,
                        hop_distance: hop,
                        content: r.content,
                        module_path: Some(r.module_path),
                        language: None,
                        document_title: None,
                        heading: None,
                        author: None,
                        category: None,
                    }))
                } else {
                    // SQL moved to ChunkRepo::fetch_chunk_content (via PgChunkRepo).
                    let row = self.chunk_repo.fetch_chunk_content(pg_id).await?;
                    Ok(row.map(|r| ExpandedNode {
                        pg_id,
                        space: Space::Code,
                        entity_type: entity_type.into(),
                        name: name.into(),
                        vector_score,
                        parent_score,
                        hop_distance: hop,
                        content: r.content,
                        module_path: Some(r.module_path),
                        language: r.language,
                        document_title: None,
                        heading: None,
                        author: None,
                        category: None,
                    }))
                }
            }
            Space::Doc => {
                // SQL moved to DocumentRepo::fetch_section_content (via PgDocumentRepo).
                let row = self.doc_repo.fetch_section_content(pg_id).await?;
                Ok(row.map(|r| ExpandedNode {
                    pg_id,
                    space: Space::Doc,
                    entity_type: "section".into(),
                    name: name.into(),
                    vector_score,
                    parent_score,
                    hop_distance: hop,
                    content: r.content,
                    module_path: None,
                    language: None,
                    document_title: Some(r.doc_title),
                    heading: Some(name.into()),
                    author: None,
                    category: None,
                }))
            }
            Space::Human => {
                // SQL moved to NoteRepo::fetch_note_content (via PgNoteRepo).
                let row = self.note_repo.fetch_note_content(pg_id).await?;
                Ok(row.map(|r| ExpandedNode {
                    pg_id,
                    space: Space::Human,
                    entity_type: "note".into(),
                    name: name.into(),
                    vector_score,
                    parent_score,
                    hop_distance: hop,
                    content: r.content,
                    module_path: None,
                    language: None,
                    document_title: None,
                    heading: None,
                    author: r.author,
                    category: r.category,
                }))
            }
        }
    }

    // ── Stage 3: Score and prune to token budget ───────────────────────

    fn rank_and_prune(nodes: Vec<ExpandedNode>, token_budget: usize) -> Vec<ScoredNode> {
        let mut scored: Vec<ScoredNode> = nodes
            .into_iter()
            .map(|n| {
                // Neighbors inherit a decayed version of their parent seed's score
                let base = n
                    .vector_score
                    .or(n.parent_score.map(|ps| ps * 0.7))
                    .unwrap_or(0.0);
                let edge_bonus = match n.hop_distance {
                    0 => 0.0,
                    1 => 0.15,
                    2 => 0.05,
                    _ => 0.0,
                };
                let space_weight = match n.space {
                    Space::Human => 1.2,
                    Space::Code => 1.0,
                    Space::Doc => 0.9,
                };
                let final_score = (base + edge_bonus) * space_weight;
                let token_count = estimate_tokens(&n.content) + 50;

                ScoredNode {
                    pg_id: n.pg_id,
                    space: n.space,
                    entity_type: n.entity_type,
                    name: n.name,
                    final_score,
                    hop_distance: n.hop_distance,
                    content: n.content,
                    token_count,
                    module_path: n.module_path,
                    language: n.language,
                    document_title: n.document_title,
                    heading: n.heading,
                    author: n.author,
                    category: n.category,
                }
            })
            .collect();

        scored.sort_by(|a, b| {
            b.final_score
                .partial_cmp(&a.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // FIX(graphrag/mod.rs ~849): pack the token budget instead of stopping
        // at the first overflow. `take_while` terminated inclusion of ALL
        // lower-ranked nodes the moment one node exceeded the remaining budget,
        // dropping smaller nodes that still fit. Use filter to skip the too-big
        // node and keep packing the rest in rank order.
        let mut budget = token_budget;
        scored
            .into_iter()
            .filter(|n| {
                if n.token_count <= budget {
                    budget -= n.token_count;
                    true
                } else {
                    false
                }
            })
            .collect()
    }

    // ── Stage 4: Format ranked nodes into structured XML context ───────

    fn assemble_context(nodes: &[ScoredNode]) -> String {
        let mut ctx = String::with_capacity(nodes.iter().map(|n| n.content.len() + 100).sum());
        for node in nodes {
            match node.space {
                Space::Code => {
                    ctx.push_str(&format!(
                        "<code module=\"{}\" name=\"{}\" lang=\"{}\" type=\"{}\">\n{}\n</code>\n\n",
                        node.module_path.as_deref().unwrap_or("unknown"),
                        node.name,
                        node.language.as_deref().unwrap_or("unknown"),
                        node.entity_type,
                        node.content,
                    ));
                }
                Space::Doc => {
                    ctx.push_str(&format!(
                        "<doc source=\"{}\" section=\"{}\">\n{}\n</doc>\n\n",
                        node.document_title.as_deref().unwrap_or("unknown"),
                        node.heading.as_deref().unwrap_or(&node.name),
                        node.content,
                    ));
                }
                Space::Human => {
                    ctx.push_str(&format!(
                        "<note author=\"{}\" category=\"{}\" title=\"{}\">\n{}\n</note>\n\n",
                        node.author.as_deref().unwrap_or("unknown"),
                        node.category.as_deref().unwrap_or("GENERAL"),
                        node.name,
                        node.content,
                    ));
                }
            }
        }
        ctx
    }
}

/// Estimate token count for text, accounting for CJK characters.
/// ASCII: ~4 chars per token. CJK/non-ASCII: ~1-2 chars per token.
fn estimate_tokens(text: &str) -> usize {
    let ascii_count = text.bytes().filter(u8::is_ascii).count();
    let total_chars = text.chars().count();
    let non_ascii_chars = total_chars.saturating_sub(ascii_count);
    ascii_count / 4 + non_ascii_chars * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a hop-0 Code ExpandedNode with a given vector score and an
    /// all-ASCII content body of `content_len` chars. For hop-0 Code nodes
    /// rank_and_prune computes final_score == vector_score (edge_bonus 0,
    /// space_weight 1.0) and token_count == content_len / 4 + 50, which lets
    /// these tests control ranking and budget packing precisely.
    fn code_node(score: f32, content_len: usize) -> ExpandedNode {
        ExpandedNode {
            pg_id: Uuid::new_v4(),
            space: Space::Code,
            entity_type: "chunk".into(),
            name: "n".into(),
            vector_score: Some(score),
            parent_score: None,
            hop_distance: 0,
            content: "a".repeat(content_len),
            module_path: Some("m".into()),
            language: Some("rust".into()),
            document_title: None,
            heading: None,
            author: None,
            category: None,
        }
    }

    /// FIX(graphrag/mod.rs ~849) regression: an oversized node must NOT
    /// terminate inclusion of lower-ranked nodes that still fit. Ranks are
    /// big(0.9) > huge(0.8) > small(0.7). With the old take_while, hitting
    /// `huge` (which overflows the remaining budget) dropped `small` too;
    /// with filter-based packing, `small` is kept.
    #[test]
    fn rank_and_prune_skips_oversized_keeps_smaller() {
        // token_count = content_len/4 + 50.
        let big = code_node(0.9, 400); // 100 + 50 = 150 tokens
        let huge = code_node(0.8, 4000); // 1000 + 50 = 1050 tokens
        let small = code_node(0.7, 40); // 10 + 50 = 60 tokens

        let big_id = big.pg_id;
        let huge_id = huge.pg_id;
        let small_id = small.pg_id;

        // Budget fits big (150) + small (60) = 210, but not huge (1050).
        let budget = 250;
        let kept = GraphRagService::rank_and_prune(vec![small, huge, big], budget);

        let ids: Vec<Uuid> = kept.iter().map(|n| n.pg_id).collect();
        // big is ranked first and fits; huge is skipped; small still fits.
        assert_eq!(ids, vec![big_id, small_id]);
        assert!(!ids.contains(&huge_id));
        // Packed total stays within budget.
        let total: usize = kept.iter().map(|n| n.token_count).sum();
        assert!(total <= budget, "packed {total} exceeds budget {budget}");
    }

    /// A node larger than the entire budget is simply excluded; packing does
    /// not panic on underflow and lower-ranked fitting nodes survive.
    #[test]
    fn rank_and_prune_excludes_node_larger_than_budget() {
        let oversized = code_node(0.9, 4000); // 1050 tokens
        let fits = code_node(0.5, 40); // 60 tokens
        let fits_id = fits.pg_id;

        let kept = GraphRagService::rank_and_prune(vec![oversized, fits], 100);

        let ids: Vec<Uuid> = kept.iter().map(|n| n.pg_id).collect();
        assert_eq!(ids, vec![fits_id]);
    }

    /// Nodes are emitted in descending final_score order before packing.
    #[test]
    fn rank_and_prune_orders_by_score_descending() {
        let lo = code_node(0.1, 4);
        let hi = code_node(0.9, 4);
        let mid = code_node(0.5, 4);
        let (lo_id, hi_id, mid_id) = (lo.pg_id, hi.pg_id, mid.pg_id);

        let kept = GraphRagService::rank_and_prune(vec![lo, hi, mid], 64_000);
        let ids: Vec<Uuid> = kept.iter().map(|n| n.pg_id).collect();
        assert_eq!(ids, vec![hi_id, mid_id, lo_id]);
    }
}
