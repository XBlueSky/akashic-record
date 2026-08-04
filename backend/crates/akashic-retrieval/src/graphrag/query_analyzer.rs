//! Query analysis facade (A1 Task 2).
//!
//! The pure analysis kernel (`analyze`, helpers) moved to
//! `akashic-domain::algos::ranking`. This file keeps the IO-aware
//! `init_archetypes` (needs the async embedding provider) and re-exports
//! the pure parts so all existing call sites compile unchanged.

// Re-export the pure analysis functions.
pub use akashic_domain::algos::ranking::{
    CONCEPT_ARCHETYPE_TEXT, SYMBOL_ARCHETYPE_TEXT, analyze, set_archetypes,
};
pub use akashic_domain::types::QueryAnalysis;

/// Initialize archetype embeddings. Call once at startup with the embedding provider.
///
/// This wrapper stays in the retrieval crate because it requires an async
/// `EmbeddingProvider` — IO-aware and not domain-pure.
pub async fn init_archetypes(
    embedder: &dyn akashic_embed::EmbeddingProvider,
) -> anyhow::Result<()> {
    let sym = embedder.embed(SYMBOL_ARCHETYPE_TEXT).await?;
    let con = embedder.embed(CONCEPT_ARCHETYPE_TEXT).await?;
    set_archetypes(sym.vector, con.vector);
    Ok(())
}
