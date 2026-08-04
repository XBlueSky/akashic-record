//! EXT-8-1 Tier-0: inheritance-aware call resolution.
//!
//! When the normal four-tier cascade fails to resolve a method call, this
//! module checks whether the caller's enclosing type has a supertype
//! (transitively, via `Implements`/`Extends` edges) that defines a method
//! of the same name.  If it does, we emit a `Call` edge with confidence 0.9
//! and method tag `"inherited"`.
//!
//! This is **purely additive** — it only produces edges for calls that the
//! cascade already dropped (i.e. returned `None`).

use std::collections::{HashMap, HashSet, VecDeque};

use uuid::Uuid;

use akashic_extraction::EdgeKind;
use akashic_extraction::resolution::ResolvedEdge;

/// For each dropped call `(method_name, caller_chunk_id)`, attempt to resolve
/// the call via the caller's type hierarchy (BFS, depth cap 8, cycle-safe).
///
/// # Maps
/// * `supertypes_of` — subtype chunk id → list of direct supertype chunk ids
///   (derived from `Implements`/`Extends` resolved edges)
/// * `fqn_by_id` — chunk id → fully-qualified name (skips `None`)
/// * `type_id_by_fqn` — fqn → chunk id, only for type-level chunks
///   (`struct`, `class`, `interface`, `trait`, `enum`, `type`)
/// * `parent_fqn_by_id` — chunk id → enclosing type's fqn (skips `None`)
/// * `method_by_type_and_name` — `(type_fqn, method_name)` → method chunk id
///
/// # Output
/// A deduplicated `Vec<ResolvedEdge>` — at most one edge per `(src, tgt)` pair.
pub fn inherited_call_targets(
    dropped: &[(String, Uuid)],
    supertypes_of: &HashMap<Uuid, Vec<Uuid>>,
    fqn_by_id: &HashMap<Uuid, String>,
    type_id_by_fqn: &HashMap<String, Uuid>,
    parent_fqn_by_id: &HashMap<Uuid, String>,
    method_by_type_and_name: &HashMap<(String, String), Uuid>,
) -> Vec<ResolvedEdge> {
    let mut edges: Vec<ResolvedEdge> = Vec::new();
    let mut seen_pairs: HashSet<(Uuid, Uuid)> = HashSet::new();

    for (method_name, caller_chunk_id) in dropped {
        // Locate the enclosing type of the caller chunk.
        let caller_type_fqn = match parent_fqn_by_id.get(caller_chunk_id) {
            Some(fqn) => fqn,
            None => continue,
        };
        let caller_type_id = match type_id_by_fqn.get(caller_type_fqn) {
            Some(&id) => id,
            None => continue,
        };

        // BFS over the supertype graph, depth-capped at 8 to handle pathological
        // hierarchies without blowing the stack or running forever.
        let mut visited: HashSet<Uuid> = HashSet::new();
        visited.insert(caller_type_id);

        let mut queue: VecDeque<(Uuid, u8)> = VecDeque::new();
        // Seed the queue with direct supertypes.
        if let Some(direct) = supertypes_of.get(&caller_type_id) {
            for &st in direct {
                if visited.insert(st) {
                    queue.push_back((st, 1));
                }
            }
        }

        while let Some((supertype_id, depth)) = queue.pop_front() {
            let supertype_fqn = match fqn_by_id.get(&supertype_id) {
                Some(f) => f,
                None => continue,
            };

            // Check if this supertype defines the method.
            let key = (supertype_fqn.clone(), method_name.clone());
            if let Some(&method_chunk_id) = method_by_type_and_name.get(&key) {
                let pair = (*caller_chunk_id, method_chunk_id);
                if seen_pairs.insert(pair) {
                    edges.push(ResolvedEdge {
                        src_chunk_id: *caller_chunk_id,
                        tgt_chunk_id: method_chunk_id,
                        kind: EdgeKind::Call,
                        confidence: 0.9,
                        method: "inherited".to_string(),
                        line: None,
                        ref_kind: None,
                    });
                }
                // Found the closest ancestor defining this method — stop searching
                // further up for this particular call (nearest wins).
                break;
            }

            // Enqueue supertypes of this supertype if within depth cap.
            if depth < 8
                && let Some(next_supers) = supertypes_of.get(&supertype_id)
            {
                for &st in next_supers {
                    if visited.insert(st) {
                        queue.push_back((st, depth + 1));
                    }
                }
            }
        }
    }

    edges
}

/// Drop inherited (tier-0) edges that would collide with an already-resolved
/// edge for the same `(src, tgt, kind)`.
///
/// The Neo4j writer `MERGE (src)-[r:CALLS]->(tgt) SET r.confidence/r.method`
/// applies one row per `(src, tgt)` in array order, so an inherited edge
/// (confidence 0.9, method `"inherited"`) appended after a genuinely-resolved
/// edge to the same target would silently overwrite the higher-confidence
/// edge. We retain only inherited edges whose `(src, tgt, kind)` triple is not
/// already present among the resolved edges; genuinely-new inherited edges
/// (no pre-existing pair) pass through untouched.
pub fn filter_new_inherited_edges(
    existing: &[ResolvedEdge],
    inherited: Vec<ResolvedEdge>,
) -> Vec<ResolvedEdge> {
    let existing_keys: HashSet<(Uuid, Uuid, EdgeKind)> = existing
        .iter()
        .map(|e| (e.src_chunk_id, e.tgt_chunk_id, e.kind))
        .collect();

    inherited
        .into_iter()
        .filter(|e| !existing_keys.contains(&(e.src_chunk_id, e.tgt_chunk_id, e.kind)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Helper: fixed deterministic UUIDs for tests.
    fn uid(n: u8) -> Uuid {
        Uuid::from_bytes([n, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }

    // ── shared fixture IDs ────────────────────────────────────────────────────
    // uid(1) = sub_type chunk (the "Sub" class node)
    // uid(2) = sub_method chunk (a method inside "Sub" that drops the call)
    // uid(3) = base_type chunk (the "Base" class node)
    // uid(4) = base_greet chunk (Base::greet method)
    // uid(5) = mid_type chunk  (used in transitive tests)
    // uid(6) = mid_greet chunk (Mid::greet — NOT present in basic tests)

    /// (a) Direct supertype defines the method → one edge resolved.
    #[test]
    fn direct_supertype_resolves_dropped_call() {
        let sub = uid(1);
        let sub_method = uid(2);
        let base = uid(3);
        let base_greet = uid(4);

        let dropped = vec![("greet".to_string(), sub_method)];

        let mut supertypes_of = HashMap::new();
        supertypes_of.insert(sub, vec![base]);

        let mut fqn_by_id = HashMap::new();
        fqn_by_id.insert(sub, "Sub".to_string());
        fqn_by_id.insert(base, "Base".to_string());

        let mut type_id_by_fqn = HashMap::new();
        type_id_by_fqn.insert("Sub".to_string(), sub);
        type_id_by_fqn.insert("Base".to_string(), base);

        let mut parent_fqn_by_id = HashMap::new();
        parent_fqn_by_id.insert(sub_method, "Sub".to_string());

        let mut method_by_type_and_name = HashMap::new();
        method_by_type_and_name.insert(("Base".to_string(), "greet".to_string()), base_greet);

        let result = inherited_call_targets(
            &dropped,
            &supertypes_of,
            &fqn_by_id,
            &type_id_by_fqn,
            &parent_fqn_by_id,
            &method_by_type_and_name,
        );

        assert_eq!(result.len(), 1, "expected exactly one resolved edge");
        let e = &result[0];
        assert_eq!(e.src_chunk_id, sub_method);
        assert_eq!(e.tgt_chunk_id, base_greet);
        assert_eq!(e.kind, EdgeKind::Call);
        assert!((e.confidence - 0.9).abs() < 1e-6, "confidence must be 0.9");
        assert_eq!(e.method, "inherited");
        assert!(e.line.is_none());
        assert!(e.ref_kind.is_none());
    }

    /// (b) Caller chunk has no enclosing type (no parent_fqn) → empty.
    #[test]
    fn no_enclosing_type_yields_empty() {
        let orphan_method = uid(2);
        let dropped = vec![("greet".to_string(), orphan_method)];

        let result = inherited_call_targets(
            &dropped,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(), // no parent_fqn entry for orphan_method
            &HashMap::new(),
        );

        assert!(result.is_empty());
    }

    /// (c) Enclosing type has a supertype but the supertype does NOT define the
    ///     method → empty.
    #[test]
    fn method_not_on_supertype_yields_empty() {
        let sub = uid(1);
        let sub_method = uid(2);
        let base = uid(3);
        // base_greet NOT inserted into method_by_type_and_name

        let dropped = vec![("greet".to_string(), sub_method)];

        let mut supertypes_of = HashMap::new();
        supertypes_of.insert(sub, vec![base]);

        let mut fqn_by_id = HashMap::new();
        fqn_by_id.insert(sub, "Sub".to_string());
        fqn_by_id.insert(base, "Base".to_string());

        let mut type_id_by_fqn = HashMap::new();
        type_id_by_fqn.insert("Sub".to_string(), sub);
        type_id_by_fqn.insert("Base".to_string(), base);

        let mut parent_fqn_by_id = HashMap::new();
        parent_fqn_by_id.insert(sub_method, "Sub".to_string());

        let result = inherited_call_targets(
            &dropped,
            &supertypes_of,
            &fqn_by_id,
            &type_id_by_fqn,
            &parent_fqn_by_id,
            &HashMap::new(), // no methods defined
        );

        assert!(result.is_empty());
    }

    /// (d) Transitive hierarchy Sub→Mid→Base; only Base defines greet → resolves.
    #[test]
    fn transitive_supertype_resolves_dropped_call() {
        let sub = uid(1);
        let sub_method = uid(2);
        let base = uid(3);
        let base_greet = uid(4);
        let mid = uid(5);

        let dropped = vec![("greet".to_string(), sub_method)];

        let mut supertypes_of = HashMap::new();
        supertypes_of.insert(sub, vec![mid]);
        supertypes_of.insert(mid, vec![base]);

        let mut fqn_by_id = HashMap::new();
        fqn_by_id.insert(sub, "Sub".to_string());
        fqn_by_id.insert(mid, "Mid".to_string());
        fqn_by_id.insert(base, "Base".to_string());

        let mut type_id_by_fqn = HashMap::new();
        type_id_by_fqn.insert("Sub".to_string(), sub);
        type_id_by_fqn.insert("Mid".to_string(), mid);
        type_id_by_fqn.insert("Base".to_string(), base);

        let mut parent_fqn_by_id = HashMap::new();
        parent_fqn_by_id.insert(sub_method, "Sub".to_string());

        let mut method_by_type_and_name = HashMap::new();
        // Only Base defines greet; Mid does not.
        method_by_type_and_name.insert(("Base".to_string(), "greet".to_string()), base_greet);

        let result = inherited_call_targets(
            &dropped,
            &supertypes_of,
            &fqn_by_id,
            &type_id_by_fqn,
            &parent_fqn_by_id,
            &method_by_type_and_name,
        );

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].src_chunk_id, sub_method);
        assert_eq!(result[0].tgt_chunk_id, base_greet);
    }

    /// (e) Cycle in the supertype graph (Sub→Base→Sub) must not infinite-loop.
    #[test]
    fn cycle_in_supertype_graph_does_not_loop() {
        let sub = uid(1);
        let sub_method = uid(2);
        let base = uid(3);
        // Deliberately form a cycle: Sub→Base and Base→Sub.

        let dropped = vec![("greet".to_string(), sub_method)];

        let mut supertypes_of = HashMap::new();
        supertypes_of.insert(sub, vec![base]);
        supertypes_of.insert(base, vec![sub]); // cycle

        let mut fqn_by_id = HashMap::new();
        fqn_by_id.insert(sub, "Sub".to_string());
        fqn_by_id.insert(base, "Base".to_string());

        let mut type_id_by_fqn = HashMap::new();
        type_id_by_fqn.insert("Sub".to_string(), sub);
        type_id_by_fqn.insert("Base".to_string(), base);

        let mut parent_fqn_by_id = HashMap::new();
        parent_fqn_by_id.insert(sub_method, "Sub".to_string());

        // Neither type defines greet → should return empty without looping.
        let result = inherited_call_targets(
            &dropped,
            &supertypes_of,
            &fqn_by_id,
            &type_id_by_fqn,
            &parent_fqn_by_id,
            &HashMap::new(),
        );

        assert!(
            result.is_empty(),
            "cyclic graph with no matching method → empty"
        );
    }

    fn call_edge(src: Uuid, tgt: Uuid, conf: f32, method: &str) -> ResolvedEdge {
        ResolvedEdge {
            src_chunk_id: src,
            tgt_chunk_id: tgt,
            kind: EdgeKind::Call,
            confidence: conf,
            method: method.to_string(),
            line: None,
            ref_kind: None,
        }
    }

    /// (f) An inherited edge to a target that already has a resolved edge must
    ///     be dropped (so it cannot overwrite the higher-confidence real edge),
    ///     while an inherited edge to a brand-new target is kept.
    #[test]
    fn inherited_edge_does_not_overwrite_existing_resolved_edge() {
        let src = uid(2);
        let x = uid(4); // already-resolved target
        let y = uid(6); // brand-new inherited target

        let existing = vec![call_edge(src, x, 1.0, "import_resolved")];
        let inherited = vec![
            call_edge(src, x, 0.9, "inherited"), // collides — must be dropped
            call_edge(src, y, 0.9, "inherited"), // new — must be kept
        ];

        let kept = filter_new_inherited_edges(&existing, inherited);

        assert_eq!(
            kept.len(),
            1,
            "only the genuinely-new inherited edge survives"
        );
        assert_eq!(kept[0].src_chunk_id, src);
        assert_eq!(kept[0].tgt_chunk_id, y);
        assert_eq!(kept[0].method, "inherited");

        // The pre-existing 1.0 "import_resolved" edge for (src→x) is untouched
        // and would not be overwritten when the kept set is appended downstream.
        assert!(
            !kept.iter().any(|e| e.tgt_chunk_id == x),
            "inherited edge to an already-resolved target must not be emitted"
        );
    }
}
