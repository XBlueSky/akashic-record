use std::sync::Arc;

use anyhow::Result;
use regex::Regex;
use uuid::Uuid;

use akashic_domain::ports::{EdgeRepo, SymbolRepo};
use akashic_llm::LlmProvider;

// ── Tier 1 ──────────────────────────────────────────────────────────────

/// Tier 1: Extract explicit code references from section content and create EXPLAINS edges.
/// Looks for inline code (`symbol_name`), code blocks, and filepath-like strings.
/// Returns the number of edges created.
///
/// All SQL/Cypher moved verbatim to `SymbolRepo` (PG lookups) and
/// `EdgeRepo` (Neo4j EXPLAINS writes) — retrieval is now infra-free.
pub async fn create_deterministic_explains_edges(
    symbol_repo: &Arc<dyn SymbolRepo>,
    edge_repo: &Arc<dyn EdgeRepo>,
    section_pg_id: Uuid,
    section_content: &str,
    _repo_name: &str,
) -> Result<usize> {
    // Extract inline code references (backtick-delimited identifiers)
    let code_re = Regex::new(r"`([a-zA-Z_][\w:./\-]*)`").unwrap();
    let refs: Vec<String> = code_re
        .captures_iter(section_content)
        .map(|c| c[1].to_string())
        .collect();

    if refs.is_empty() {
        return Ok(0);
    }

    let mut edges_created = 0;

    for reference in &refs {
        // Try matching against chunk names across all repos (min length 5 to avoid false positives)
        // SQL moved verbatim to SymbolRepo::find_chunk_ids_by_name.
        let chunk_matches = symbol_repo.find_chunk_ids_by_name(reference, 3).await?;

        if !chunk_matches.is_empty() {
            for row in &chunk_matches {
                // Cypher moved verbatim to EdgeRepo::merge_explains_to_chunk.
                edge_repo
                    .merge_explains_to_chunk(section_pg_id, row.id, 1.0, "exact", &row.repo_name)
                    .await?;
                edges_created += 1;
            }
            continue;
        }

        // Try matching against module paths across all repos (min length 5)
        // SQL moved verbatim to SymbolRepo::find_module_ids_by_path_fragment.
        let module_matches = symbol_repo
            .find_module_ids_by_path_fragment(reference, 3)
            .await?;

        for row in &module_matches {
            // Cypher moved verbatim to EdgeRepo::merge_explains_to_module.
            edge_repo
                .merge_explains_to_module(section_pg_id, row.id, 1.0, "exact", &row.repo_name)
                .await?;
            edges_created += 1;
        }
    }

    Ok(edges_created)
}

/// Tier 2: Hybrid embedding similarity linking.
///
/// - similarity > 0.50 → auto-create EXPLAINS (confidence=0.85, method='embedding')
/// - 0.40 < similarity ≤ 0.50 → LLM verification (confidence=0.70, method='llm')
/// - similarity ≤ 0.40 → skip
///
/// Max 3 edges per section to avoid noise.
///
/// All SQL/Cypher moved verbatim to `SymbolRepo` (PG candidate fetch) and
/// `EdgeRepo` (Neo4j EXPLAINS writes) — retrieval is now infra-free.
#[allow(clippy::too_many_arguments)]
pub async fn create_llm_verified_explains_edges(
    symbol_repo: &Arc<dyn SymbolRepo>,
    edge_repo: &Arc<dyn EdgeRepo>,
    section_pg_id: Uuid,
    section_content: &str,
    section_heading: &str,
    section_embedding: &[f32],
    _repo_name: &str,
    llm: &dyn LlmProvider,
) -> Result<usize> {
    // SQL moved verbatim to SymbolRepo::find_chunks_for_explains_linking.
    let candidates = symbol_repo
        .find_chunks_for_explains_linking(section_embedding, 8)
        .await?;

    if candidates.is_empty() {
        return Ok(0);
    }

    let mut edges_created = 0;
    let max_edges = 3;

    // Tier 2a: High similarity → auto-link (no LLM needed)
    for candidate in &candidates {
        if edges_created >= max_edges {
            break;
        }
        if candidate.similarity > 0.50 {
            // Cypher moved verbatim to EdgeRepo::merge_explains_to_chunk.
            edge_repo
                .merge_explains_to_chunk(
                    section_pg_id,
                    candidate.id,
                    0.85,
                    "embedding",
                    &candidate.repo_name,
                )
                .await?;
            edges_created += 1;
        }
    }

    // Tier 2b: Medium similarity → LLM verification
    let medium_candidates: Vec<&akashic_domain::ports::ExplainsLinkCandidate> = candidates
        .iter()
        .filter(|c| c.similarity > 0.40 && c.similarity <= 0.50)
        .collect();

    if edges_created < max_edges && !medium_candidates.is_empty() {
        let candidate_list: String = medium_candidates
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{}. {} '{}' in module '{}' (repo: {}, sim: {:.2})",
                    i + 1,
                    c.chunk_type,
                    c.name,
                    c.module_path,
                    c.repo_name,
                    c.similarity
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let content_truncated: String = section_content.chars().take(1000).collect();
        let prompt = format!(
            "You are an architect mapping documentation to a codebase.\n\n\
             Wiki Section: \"{section_heading}\"\n\
             Content: {content_truncated}\n\n\
             Candidate code elements:\n{candidate_list}\n\n\
             Does this wiki section explicitly explain or document any of these code elements?\n\
             Return ONLY a JSON array of the exact names that match. If none match, return [].\n\
             Example: [\"validate_token\", \"AuthModule\"]"
        );

        if let Ok(r) = llm.generate_json(&prompt).await {
            let cleaned = akashic_llm::extract_json(&r.text);
            let matched_names: Vec<String> = serde_json::from_str(cleaned).unwrap_or_default();

            for candidate in &medium_candidates {
                if edges_created >= max_edges {
                    break;
                }
                if matched_names
                    .iter()
                    .any(|m| m.eq_ignore_ascii_case(&candidate.name))
                {
                    // Cypher moved verbatim to EdgeRepo::merge_explains_to_chunk.
                    edge_repo
                        .merge_explains_to_chunk(
                            section_pg_id,
                            candidate.id,
                            0.70,
                            "llm",
                            &candidate.repo_name,
                        )
                        .await?;
                    edges_created += 1;
                }
            }
        }
    }

    Ok(edges_created)
}
