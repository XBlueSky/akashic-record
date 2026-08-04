use crate::resolution::chunk_index::{ChunkIndex, ImportMap, ResolvedEdge, resolve_call_edge};
use crate::types::{EdgeEndpoint, EdgeKind, RawEdge};
use std::collections::HashSet;
use uuid::Uuid;

/// Resolve raw edges into ResolvedEdges. `enclosing_src` maps a source byte
/// offset to the (chunk_id, module_path) of the enclosing chunk. `imported_modules`
/// is the set of module paths the file imports (powers the EXT-3 import_scoped
/// tier). Call edges resolve via the cascade in `resolve_call_edge`; Import and
/// other EdgeKinds are recognized but produce no ResolvedEdge yet (EXT-3b/4/7/8
/// fill those in).
pub fn resolve_edges(
    raw_edges: Vec<RawEdge>,
    enclosing_src: impl Fn(usize) -> Option<(Uuid, String)>,
    idx: &ChunkIndex,
    import_map: &ImportMap,
    imported_modules: &HashSet<String>,
) -> Vec<ResolvedEdge> {
    let mut out = Vec::new();
    for edge in raw_edges {
        // Call edges resolve via the confidence cascade.
        // Import edges and other EdgeKinds (Includes/DependsOn/RoutesTo/...) are
        // recognized but produce no ResolvedEdge yet — EXT-3b/4/7/8 fill those in.
        if edge.kind == EdgeKind::Call
            && let EdgeEndpoint::ByteRange { start, .. } = &edge.source
            && let Some((src_id, src_module)) = enclosing_src(*start)
            && let Some(r) = resolve_call_edge(
                &edge,
                src_id,
                &src_module,
                idx,
                import_map,
                imported_modules,
            )
        {
            out.push(r);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EdgeMetadata, Provenance};

    #[test]
    fn resolves_call_via_enclosing_lookup() {
        let caller = Uuid::new_v4();
        let callee = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("m".into(), "target".into(), callee);
        let edges = vec![RawEdge {
            source: EdgeEndpoint::ByteRange {
                start: 100,
                end: 110,
            },
            target: EdgeEndpoint::Name {
                name: "target".into(),
                module_specifier: None,
            },
            kind: EdgeKind::Call,
            provenance: Provenance::Static,
            line: Some(3),
            metadata: EdgeMetadata::default(),
        }];
        let resolved = resolve_edges(
            edges,
            |byte| {
                if byte == 100 {
                    Some((caller, "m".to_string()))
                } else {
                    None
                }
            },
            &idx,
            &ImportMap::default(),
            &HashSet::new(),
        );
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].src_chunk_id, caller);
        assert_eq!(resolved[0].tgt_chunk_id, callee);
        assert_eq!(resolved[0].method, "same_file");
    }

    #[test]
    fn drops_call_with_no_enclosing_chunk() {
        let idx = ChunkIndex::default();
        let edges = vec![RawEdge {
            source: EdgeEndpoint::ByteRange {
                start: 999,
                end: 1000,
            },
            target: EdgeEndpoint::Name {
                name: "x".into(),
                module_specifier: None,
            },
            kind: EdgeKind::Call,
            provenance: Provenance::Static,
            line: None,
            metadata: EdgeMetadata::default(),
        }];
        let resolved = resolve_edges(
            edges,
            |_| None,
            &idx,
            &ImportMap::default(),
            &HashSet::new(),
        );
        assert!(resolved.is_empty());
    }
}
