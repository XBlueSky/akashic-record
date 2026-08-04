//! Read-only corpus loader: pulls doc-bearing chunk text from Postgres.

/// One eval item: query = `doc`, retrieval target = `content` (the code body,
/// which the chunker already emits WITHOUT the doc comment — see the plan's
/// docstring-stripping note).
///
/// **This is a load-bearing methodological invariant, not an incidental
/// detail.** The eval's validity depends on `doc` and `content` sharing no
/// literal text: if a future chunker change ever lets the docstring leak
/// into `content`, every provider's score would silently inflate (verbatim
/// keyword overlap between query and target trivially boosts cosine
/// similarity), producing numbers that look better without the retrieval
/// quality actually improving. There is no runtime assertion for this here —
/// it's verified via golden fixtures — so a future maintainer touching the
/// chunker must preserve it deliberately.
#[derive(Debug, Clone)]
pub struct EvalItem {
    pub fqn: String,
    pub doc: String,
    pub content: String,
    pub language: Option<String>,
}

/// Load all doc-bearing chunks for `repo_name`. Reads text only; ignores the
/// stored `embedding` column entirely (the harness re-embeds fresh).
pub async fn load_corpus(pool: &sqlx::PgPool, repo_name: &str) -> anyhow::Result<Vec<EvalItem>> {
    let rows: Vec<(Option<String>, Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT fqn, doc, content, language FROM chunks \
         WHERE repo_name = $1 AND doc IS NOT NULL AND doc <> ''",
    )
    .bind(repo_name)
    .fetch_all(pool)
    .await?;

    let items: Vec<EvalItem> = rows
        .into_iter()
        .filter_map(|(fqn, doc, content, language)| {
            let doc = doc?;
            if doc.trim().is_empty() || content.trim().is_empty() {
                return None;
            }
            Some(EvalItem {
                fqn: fqn.unwrap_or_default(),
                doc,
                content,
                language,
            })
        })
        .collect();

    if items.is_empty() {
        anyhow::bail!(
            "no doc-bearing chunks found for repo '{repo_name}' — a doc→code eval \
             needs chunks with doc comments; ingest such a repo first"
        );
    }
    Ok(items)
}
