//! docs-corpus READ MCP tools (E5): `list_docs` + `get_docs_page`.
//! DB-backed (`CorpusStore`), read-only, anonymous-callable — the MCP face
//! of the same raw layer the `/api/v1/docs` HTTP endpoints serve, with the
//! shared stamp literal from `akashic_domain::algos::llms`. One of the
//! composed `#[tool_router]` blocks; combined in `super`'s `AkashicMcp::new`.

use akashic_domain::algos::corpus_contract::resolve_nav_tree;
use akashic_domain::algos::llms;
use akashic_domain::types::corpus::CorpusVersionMeta;

use super::*;

impl AkashicMcp {
    /// Resolve `(repo, selector)` to version metadata, mapping the store
    /// error and the not-found case to tool-error strings.
    async fn resolve_corpus_version(
        &self,
        repo: &str,
        selector: &str,
    ) -> Result<CorpusVersionMeta, String> {
        self.corpus_store
            .resolve_version(repo, selector)
            .await
            .map_err(|e| format!("CorpusStore::resolve_version failed: {e}"))?
            .ok_or_else(|| {
                format!(
                    "no docs corpus found for repo \"{repo}\" selector \"{selector}\" — \
                     call list_docs (no args) to see published repos"
                )
            })
    }
}

#[tool_router(router = docs_read_tools, vis = "pub(crate)")]
impl AkashicMcp {
    #[tool(
        name = "list_docs",
        description = "List published docs corpora. Without `repo`: every repo that has docs, \
with its latest version stamp. With `repo`: that repo's full nav tree (groups -> pages with \
path/title/description) at the latest version — page paths feed get_docs_page."
    )]
    async fn list_docs(
        &self,
        Parameters(args): Parameters<ListDocsArgs>,
    ) -> Result<String, String> {
        match args.repo {
            None => {
                let repos = self
                    .corpus_store
                    .list_repos()
                    .await
                    .map_err(|e| format!("CorpusStore::list_repos failed: {e}"))?;
                let entries: Vec<serde_json::Value> = repos
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "repo": s.repo_name,
                            "version": s.latest.version,
                            "sha": s.latest.sha,
                            "stamp": llms::stamp(&s.repo_name, &s.latest.version, &s.latest.sha),
                            "page_count": s.latest.page_count,
                            "derive_status": s.latest.derive_status.as_str(),
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&serde_json::json!({ "repos": entries }))
                    .map_err(|e| format!("serialization failed: {e}"))
            }
            Some(repo) => {
                let meta = self.resolve_corpus_version(&repo, "latest").await?;
                let nav = self
                    .corpus_store
                    .get_nav(meta.id)
                    .await
                    .map_err(|e| format!("CorpusStore::get_nav failed: {e}"))?
                    .ok_or_else(|| format!("no nav stored for repo \"{repo}\""))?;
                // Nav is PERSISTED with index-relative page paths (that's how
                // they're written in the index markdown). This tool's contract
                // is "page paths feed get_docs_page", which keys files by full
                // corpus key — so resolve before serializing. Without this, any
                // corpus whose index isn't at the artifact root (every
                // pull-bootstrapped one: `docs/README.md`) hands out paths that
                // get_docs_page can't find.
                let nav = resolve_nav_tree(&nav, &meta.index_path);
                serde_json::to_string_pretty(&serde_json::json!({
                    "repo": repo,
                    "stamp": llms::stamp(&repo, &meta.version, &meta.sha),
                    "nav": nav,
                }))
                .map_err(|e| format!("serialization failed: {e}"))
            }
        }
    }

    #[tool(
        name = "get_docs_page",
        description = "Read ONE full docs page (raw markdown, byte-exact) from a repo's \
published corpus, plus its version stamp. `path` is the corpus-relative page path exactly as \
listed by list_docs. `version` accepts \"latest\" (default), an exact version, or a sha prefix."
    )]
    async fn get_docs_page(
        &self,
        Parameters(args): Parameters<GetDocsPageArgs>,
    ) -> Result<String, String> {
        let selector = args.version.as_deref().unwrap_or("latest");
        let meta = self.resolve_corpus_version(&args.repo, selector).await?;
        let file = self
            .corpus_store
            .get_file(meta.id, &args.path)
            .await
            .map_err(|e| format!("CorpusStore::get_file failed: {e}"))?
            .ok_or_else(|| {
                format!(
                    "page \"{}\" not found in {} {} — call list_docs(repo) for valid paths",
                    args.path, args.repo, meta.version
                )
            })?;
        let markdown =
            String::from_utf8(file.content).map_err(|e| format!("page is not valid UTF-8: {e}"))?;
        let title = self
            .corpus_store
            .get_nav(meta.id)
            .await
            .ok()
            .flatten()
            .and_then(|nav| llms::nav_page_title(&nav, &args.path, &meta.index_path))
            .unwrap_or_else(|| llms::path_stem(&args.path));
        serde_json::to_string_pretty(&serde_json::json!({
            "repo": args.repo,
            "path": args.path,
            "title": title,
            "stamp": llms::stamp(&args.repo, &meta.version, &meta.sha),
            "markdown": markdown,
        }))
        .map_err(|e| format!("serialization failed: {e}"))
    }
}
