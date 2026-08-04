pub mod explains;

use std::sync::Arc;

use anyhow::Result;

use akashic_domain::ports::{EdgeRepo, SymbolRepo};

/// Auto-link a Note to matching Chunks by symbol name.
///
/// Level 1: exact symbol match in the same repo.
/// Level 2: BELONGS_TO Repository (always created by save_note).
/// Level 3: semantic match (deferred — future phase).
///
/// All SQL/Cypher moved verbatim to `SymbolRepo` (PG lookups) and
/// `EdgeRepo` (Neo4j ATTACHED_TO writes) — retrieval is now infra-free.
pub async fn link_note_to_chunks(
    symbol_repo: &Arc<dyn SymbolRepo>,
    edge_repo: &Arc<dyn EdgeRepo>,
    context_pg_id: &str,
    repo_name: &str,
    related_symbols: &[String],
) -> Result<()> {
    if related_symbols.is_empty() {
        return Ok(());
    }

    // Level 1: Find chunks matching the symbol names in the same repo
    for symbol in related_symbols {
        // Try exact name match, or parent::child match
        let (parent, child) = if let Some((p, c)) = symbol.rsplit_once("::") {
            (Some(p.to_string()), c.to_string())
        } else {
            (None, symbol.clone())
        };

        let chunk_ids: Vec<uuid::Uuid> = if let Some(ref parent) = parent {
            // SQL moved verbatim to SymbolRepo::find_chunk_ids_by_name_and_module.
            symbol_repo
                .find_chunk_ids_by_name_and_module(repo_name, &child, parent, 10)
                .await?
        } else {
            // SQL moved verbatim to SymbolRepo::find_chunk_ids_by_name_in_repo.
            symbol_repo
                .find_chunk_ids_by_name_in_repo(repo_name, &child, 10)
                .await?
        };

        // Create ATTACHED_TO edges in Neo4j
        // Cypher moved verbatim to EdgeRepo::merge_attached_to_chunk.
        for chunk_id in &chunk_ids {
            edge_repo
                .merge_attached_to_chunk(context_pg_id, *chunk_id)
                .await?;
        }
    }

    Ok(())
}
