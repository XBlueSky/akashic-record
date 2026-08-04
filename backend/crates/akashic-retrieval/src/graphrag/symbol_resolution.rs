//! EXT-6a: precise symbol resolution (goto-definition) and direct references.
//!
//! `resolve_symbol` is the structured-query complement to the semantic
//! `search_knowledge`: it matches PG `chunks` on EXACT fqn/name (never
//! `CONTAINS`) and ranks candidates so a bare short name resolves to the one
//! intended definition. `find_references` returns the DIRECT (1-hop) incoming
//! CALLS to a precise fqn, confidence-filtered, enriched from PG. Both are
//! free functions so they are testable without constructing the MCP server.
//!
//! Uses fully-qualified paths (no `use`) so the module compiles warning-free at
//! each incremental task commit (DB-bound fns land in later tasks).

use std::sync::Arc;

use akashic_domain::ports::{GraphTraversalRepo, SymbolRepo};

/// A candidate definition for a symbol name, as stored in PG `chunks`.
#[derive(Debug, Clone)]
pub struct SymbolCandidate {
    pub fqn: Option<String>,
    pub name: String,
    pub chunk_type: String,
    pub module_path: String,
    pub visibility: Option<String>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub signature: Option<String>,
}

/// A single direct reference (caller) of a target symbol.
#[derive(Debug, Clone)]
pub struct ReferenceRow {
    pub caller_fqn: String,
    pub caller_name: String,
    pub module_path: String,
    pub chunk_type: String,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    /// "call" in 6a; "import"/"type" (6b) and "implements" (6c) union in later.
    pub ref_kind: String,
    pub confidence: f64,
    pub method: String,
}

/// Match precision of a candidate against the queried name.
/// 3 = exact fqn, 2 = fqn ends with ".name", 1 = exact short name, 0 = other.
///
/// `dot_suffix` is the pre-computed `.{query}` needle: the caller allocates it
/// ONCE (see [`rank_candidates`]) instead of re-formatting it on every call,
/// which matters because the sort comparator invokes this twice per comparison
/// across an O(n log n) sort.
fn match_precision(c: &SymbolCandidate, query: &str, dot_suffix: &str) -> u8 {
    if let Some(fqn) = &c.fqn {
        if fqn == query {
            return 3;
        }
        if fqn.ends_with(dot_suffix) {
            return 2;
        }
    }
    if c.name == query {
        return 1;
    }
    0
}

/// Visibility precedence for ranking: public/exported > crate > protected >
/// private > unknown. Compared case-insensitively on the stored text.
fn visibility_rank(v: &Option<String>) -> u8 {
    match v.as_deref().map(str::to_lowercase) {
        Some(ref s) if s.contains("public") || s.contains("export") => 4,
        Some(ref s) if s.contains("crate") => 3,
        Some(ref s) if s.contains("protected") => 2,
        Some(ref s) if s.contains("private") => 1,
        _ => 0,
    }
}

/// Rank candidates for `query`: match precision desc, visibility desc,
/// module_path asc, fqn asc (stable, deterministic output).
pub fn rank_candidates(mut cands: Vec<SymbolCandidate>, query: &str) -> Vec<SymbolCandidate> {
    // Compute the `.{query}` suffix needle ONCE here rather than re-allocating it
    // inside every `match_precision` call (the comparator runs it twice per
    // comparison across an O(n log n) sort).
    let dot_suffix = format!(".{query}");
    cands.sort_by(|a, b| {
        match_precision(b, query, &dot_suffix)
            .cmp(&match_precision(a, query, &dot_suffix))
            .then(visibility_rank(&b.visibility).cmp(&visibility_rank(&a.visibility)))
            .then(a.module_path.cmp(&b.module_path))
            .then(a.fqn.cmp(&b.fqn))
    });
    cands
}

/// Resolve a symbol NAME to ranked candidate definitions via PG `chunks`.
///
/// Matches EXACTLY (`fqn = name` OR `fqn` ends with `.name` OR `name = name`);
/// `repo`/`kind_hint` are exact filters, `module_hint` a forgiving substring.
/// Ranked by [`rank_candidates`]. Empty Vec = not found.
///
/// All SQL is moved verbatim to `SymbolRepo::resolve_symbol_candidates`
/// (via `PgSymbolRepo`) — retrieval is now infra-free.
pub async fn resolve_symbol(
    symbol_repo: &Arc<dyn SymbolRepo>,
    name: &str,
    repo: Option<&str>,
    module_hint: Option<&str>,
    kind_hint: Option<&str>,
) -> anyhow::Result<Vec<SymbolCandidate>> {
    // Escape LIKE wildcards in the name so snake_case '_' / '%' match literally
    // (Postgres' default LIKE escape char is '\').
    let escaped = name
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let suffix = format!("%.{escaped}");

    let rows = symbol_repo
        .resolve_symbol_candidates(name, &suffix, repo, module_hint, kind_hint)
        .await?;

    let cands: Vec<SymbolCandidate> = rows
        .into_iter()
        .map(|r| SymbolCandidate {
            fqn: r.fqn,
            name: r.name,
            chunk_type: r.chunk_type,
            module_path: r.module_path,
            visibility: r.visibility,
            start_line: r.start_line,
            end_line: r.end_line,
            signature: r.signature,
        })
        .collect();

    Ok(rank_candidates(cands, name))
}

/// Direct (1-hop) incoming references to a precise `fqn`.
///
/// 6b unions CALLS and REFERENCES edges, labeling each row by `ref_kind`
/// (CALLS edges coalesce to "call"; REFERENCES edges carry their stored kind,
/// e.g. "type"). `include_kinds` is an optional allowlist — pass an empty
/// slice to return all kinds. Caller rows are enriched with
/// module_path/chunk_type from PG by fqn.
///
/// All SQL/Cypher is moved verbatim to `SymbolRepo` (PG enrich) and
/// `GraphTraversalRepo` (Cypher traversal) — retrieval is now infra-free.
pub async fn find_references(
    symbol_repo: &Arc<dyn SymbolRepo>,
    traversal_repo: &Arc<dyn GraphTraversalRepo>,
    fqn: &str,
    repo: Option<&str>,
    min_confidence: f64,
    include_kinds: &[String],
) -> anyhow::Result<Vec<ReferenceRow>> {
    // Cypher traversal via GraphTraversalRepo
    let edges = traversal_repo
        .find_references_by_fqn(fqn, repo, min_confidence)
        .await?;

    let mut refs: Vec<ReferenceRow> = Vec::new();
    let mut caller_fqns: Vec<String> = Vec::new();

    for edge in &edges {
        if !include_kinds.is_empty() && !include_kinds.iter().any(|k| k == &edge.ref_kind) {
            continue;
        }
        caller_fqns.push(edge.caller_fqn.clone());
        refs.push(ReferenceRow {
            caller_fqn: edge.caller_fqn.clone(),
            caller_name: edge.caller_name.clone(),
            module_path: String::new(),
            chunk_type: String::new(),
            start_line: edge.start_line,
            end_line: edge.end_line,
            ref_kind: edge.ref_kind.clone(),
            confidence: edge.confidence,
            method: edge.method.clone(),
        });
    }

    // PG enrich module_path/chunk_type by fqn (single batch query).
    // Enrich by fqn ONLY — do NOT constrain to the target symbol's `repo`.
    // Callers come back from Neo4j filtered on `target.repo_name`, so a
    // cross-repo caller is a legitimate result; constraining the PG lookup to
    // the target's repo would drop that caller's row and leave its
    // module_path/chunk_type blank even though it was returned. fqns are
    // globally unique enough that an unconstrained lookup resolves correctly.
    if !caller_fqns.is_empty() {
        let meta = symbol_repo.enrich_chunk_meta_by_fqns(&caller_fqns).await?;
        let by_fqn: std::collections::HashMap<String, (String, String)> = meta
            .into_iter()
            .map(|r| (r.fqn, (r.module_path, r.chunk_type)))
            .collect();
        for r in &mut refs {
            if let Some((m, t)) = by_fqn.get(&r.caller_fqn) {
                r.module_path = m.clone();
                r.chunk_type = t.clone();
            }
        }
    }

    // EXT-6b-1b: module-level import references (which modules import this symbol).
    if include_kinds.is_empty() || include_kinds.iter().any(|k| k == "import") {
        let module_imports = traversal_repo.find_module_imports_by_fqn(fqn, repo).await?;
        for mref in module_imports {
            refs.push(ReferenceRow {
                caller_fqn: mref.module_path.clone(),
                caller_name: mref.module_path,
                module_path: String::new(),
                chunk_type: "module".to_string(),
                start_line: None,
                end_line: None,
                ref_kind: "import".to_string(),
                confidence: 1.0,
                method: "import".to_string(),
            });
        }
    }

    Ok(refs)
}

/// EXT-6c: implementations of a trait/interface/class. `implementors=true` →
/// who implements/extends `fqn` (incoming IMPLEMENTS); `implementors=false` →
/// what `fqn` itself implements/extends (outgoing). ref_kind carries impl_kind.
///
/// All SQL/Cypher is moved verbatim to `SymbolRepo` (PG enrich) and
/// `GraphTraversalRepo` (Cypher traversal) — retrieval is now infra-free.
pub async fn find_implementations(
    symbol_repo: &Arc<dyn SymbolRepo>,
    traversal_repo: &Arc<dyn GraphTraversalRepo>,
    fqn: &str,
    repo: Option<&str>,
    implementors: bool,
) -> anyhow::Result<Vec<ReferenceRow>> {
    let edges = traversal_repo
        .find_implementations_by_fqn(fqn, repo, implementors)
        .await?;

    let mut refs: Vec<ReferenceRow> = Vec::new();
    let mut fqns: Vec<String> = Vec::new();

    for edge in &edges {
        fqns.push(edge.caller_fqn.clone());
        refs.push(ReferenceRow {
            caller_fqn: edge.caller_fqn.clone(),
            caller_name: edge.caller_name.clone(),
            module_path: String::new(),
            chunk_type: String::new(),
            start_line: edge.start_line,
            end_line: edge.end_line,
            ref_kind: edge.ref_kind.clone(),
            confidence: 1.0,
            method: "implements".to_string(),
        });
    }

    if !fqns.is_empty() {
        let meta = symbol_repo
            .enrich_chunk_meta_by_fqns_and_repo(&fqns, repo)
            .await?;
        let by: std::collections::HashMap<String, (String, String)> = meta
            .into_iter()
            .map(|r| (r.fqn, (r.module_path, r.chunk_type)))
            .collect();
        for r in &mut refs {
            if let Some((m, t)) = by.get(&r.caller_fqn) {
                r.module_path = m.clone();
                r.chunk_type = t.clone();
            }
        }
    }

    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(fqn: &str, name: &str, module: &str, vis: &str) -> SymbolCandidate {
        SymbolCandidate {
            fqn: Some(fqn.to_string()),
            name: name.to_string(),
            chunk_type: "function".into(),
            module_path: module.into(),
            visibility: Some(vis.into()),
            start_line: Some(1),
            end_line: Some(2),
            signature: None,
        }
    }

    #[test]
    fn exact_fqn_beats_suffix_beats_name() {
        let cands = vec![
            cand("a.b.parse", "parse", "a/b", "public"), // suffix match (2)
            cand("parse", "parse", "root", "public"),    // exact fqn (3)
            cand("x.y.helper", "parse", "x/y", "public"), // name-only (1)
        ];
        let ranked = rank_candidates(cands, "parse");
        assert_eq!(ranked[0].fqn.as_deref(), Some("parse"));
        assert_eq!(ranked[1].fqn.as_deref(), Some("a.b.parse"));
        assert_eq!(ranked[2].fqn.as_deref(), Some("x.y.helper"));
    }

    #[test]
    fn visibility_breaks_ties_at_equal_precision() {
        let cands = vec![
            cand("a.b.run", "run", "a/b", "private"),
            cand("c.d.run", "run", "c/d", "public"),
        ];
        let ranked = rank_candidates(cands, "run");
        // both suffix-match precision 2 → public wins
        assert_eq!(ranked[0].fqn.as_deref(), Some("c.d.run"));
    }

    #[test]
    fn module_path_is_stable_final_tiebreak() {
        let cands = vec![
            cand("z.run", "run", "z", "public"),
            cand("a.run", "run", "a", "public"),
        ];
        let ranked = rank_candidates(cands, "run");
        assert_eq!(ranked[0].module_path, "a");
    }

    // Finding 1: `match_precision` now takes a pre-computed `.{query}` needle.
    // Confirm the four precision tiers still classify correctly and that a
    // fqn merely *containing* the query (not as a dotted suffix) is NOT a
    // suffix match — i.e. the needle must include the leading dot.
    #[test]
    fn match_precision_tiers_with_precomputed_suffix() {
        let dot = format!(".{}", "parse");
        // exact fqn → 3
        assert_eq!(
            match_precision(&cand("parse", "parse", "root", "public"), "parse", &dot),
            3
        );
        // dotted suffix → 2
        assert_eq!(
            match_precision(&cand("a.b.parse", "parse", "a/b", "public"), "parse", &dot),
            2
        );
        // short-name-only (fqn does not end with ".parse") → 1
        assert_eq!(
            match_precision(&cand("x.y.helper", "parse", "x/y", "public"), "parse", &dot),
            1
        );
        // fqn contains "parse" but not as a ".parse" suffix, and name differs → 0
        assert_eq!(
            match_precision(&cand("a.parser", "parser", "a", "public"), "parse", &dot),
            0
        );
    }
}
