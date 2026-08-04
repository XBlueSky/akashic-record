use crate::types::{EdgeEndpoint, EdgeKind, RawEdge};
use std::collections::HashMap;
use uuid::Uuid;

/// Index of chunks for reference resolution. Mirrors the pre-refactor
/// calls/resolver.rs ChunkIndex, generalized for RawEdge.
#[derive(Default, Debug, Clone)]
pub struct ChunkIndex {
    /// (module_path, name) -> chunk id
    pub by_path_and_name: HashMap<(String, String), Uuid>,
    /// name -> [chunk ids]
    pub by_name: HashMap<String, Vec<Uuid>>,
    /// chunk id -> module_path
    pub chunk_module: HashMap<Uuid, String>,
    /// (type_name, method_name) -> chunk ids. Populated from chunks whose
    /// `parent_fqn` names their owning type (D1: powers type-qualified call
    /// resolution). Vec because a (type, method) pair is normally unique but
    /// duplicate names across impl blocks are possible; a single hit resolves.
    pub by_type_and_name: HashMap<(String, String), Vec<Uuid>>,
    /// (owner_type, method_name) -> normalized return type. Populated from
    /// each method chunk's parsed `signature` (D1c-1: powers method-chain
    /// return-type resolution `x.foo().bar()` -> foo's return type -> bar).
    /// A (type, method) key whose signatures disagree on return type across
    /// duplicate/generic-impl collisions is dropped (see
    /// `return_type_conflicts`) rather than guessing — fail-closed, the same
    /// policy as every ambiguous lookup in `resolve_call_edge`.
    pub by_return_type: HashMap<(String, String), String>,
    /// (owner_type, method_name) keys permanently excluded from
    /// `by_return_type` because two chunks disagreed on the return type.
    /// Prevents a later single-value insert from resurrecting a key already
    /// proven ambiguous.
    return_type_conflicts: std::collections::HashSet<(String, String)>,
    /// (struct_type, field_name) -> normalized field type. Populated from
    /// each STRUCT chunk's parsed `content` (D1c-2: powers field-access
    /// resolution `x.field.method()` -> field's declared type -> method). A
    /// (type, field) key whose struct chunks disagree on the field's type
    /// across duplicate/mod-nested-name collisions is dropped (see
    /// `field_type_conflicts`) rather than guessing — fail-closed, the same
    /// policy as `by_return_type`.
    pub by_field_type: HashMap<(String, String), String>,
    /// (struct_type, field_name) keys permanently excluded from
    /// `by_field_type` because two struct chunks disagreed on the field's
    /// type.
    field_type_conflicts: std::collections::HashSet<(String, String)>,
    /// (struct_type, field_name) keys whose stored `by_field_type` value came
    /// from a `resolve_dyn_trait_name` hit (the field is a `dyn Trait` /
    /// `Box<dyn Trait>` / `Arc<dyn Trait>` / `Rc<dyn Trait>` object), so a
    /// field-access call through it resolves to the trait's OWN default method
    /// and is labeled `trait_default` (D3) rather than `field_type`. Keyed
    /// identically to `by_field_type` (`(normalize_base_type(owner), field)`);
    /// a poisoned/dropped `by_field_type` key makes a stale entry here moot
    /// because the resolver requires the `by_field_type` hit FIRST.
    pub field_type_is_trait: std::collections::HashSet<(String, String)>,
}

/// Normalize a Rust type expression to its bare base name so the two sides of
/// type-qualified / receiver-type resolution key identically. Applied to BOTH
/// the `by_type_and_name` index key (in [`ChunkIndex::insert_method`]) and the
/// walker's `type_env` values.
///
/// Transforms, in order:
/// 1. strip leading `&` / `&mut` and surrounding whitespace (`&mut Widget` → `Widget`),
/// 2. strip a leading lifetime token (`'a `) left exposed by the reference strip
///    (`&'a Widget` → `Widget`, `&'a mut Widget` → `Widget`),
/// 3. drop a generic argument list — truncate at the first `<` (`Vec<Foo>` → `Vec`),
/// 4. reduce a path to its trailing segment (`outer::Widget` → `Widget`).
///
/// `Self` is preserved verbatim (it is resolved to the enclosing impl type by
/// the walker before it ever reaches an index key). Idempotent on already-bare
/// names. Turbofish-shaped `Type::<T>` input (call-syntax-only) is out of scope.
pub fn normalize_base_type(ty: &str) -> String {
    // 1. strip references / leading whitespace.
    let mut s = ty.trim();
    loop {
        let stripped = s
            .strip_prefix("&mut ")
            .or_else(|| s.strip_prefix("&mut"))
            .or_else(|| s.strip_prefix('&'))
            .map(str::trim_start);
        match stripped {
            Some(rest) => s = rest,
            None => break,
        }
    }
    // 2. strip a leading lifetime (`'a `, `'a mut `) exposed by the ref strip.
    //    tree-sitter's `reference_type` puts the lifetime BEFORE `mut`
    //    (`&'a mut Widget`), so a lone `&` strip above can leave `'a mut Widget`
    //    too — handle both orderings.
    if let Some(rest) = s.strip_prefix('\'')
        && let Some((_, after)) = rest.split_once(char::is_whitespace)
    {
        s = after.trim_start();
        s = s.strip_prefix("mut ").unwrap_or(s).trim_start();
    }
    // 3. drop generics: everything from the first `<` onward.
    let s = s.split('<').next().unwrap_or(s).trim();
    // 4. trailing path segment.
    s.rsplit("::").next().unwrap_or(s).trim().to_string()
}

/// Resolve a receiver TYPE expression that is a trait object (`dyn Trait`) or a
/// recognized smart-pointer-wrapped trait object (`Box<dyn Trait>` /
/// `Arc<dyn Trait>` / `Rc<dyn Trait>`) to the bare TRAIT name, so a call through
/// such a receiver can be looked up in `by_type_and_name` under the trait — the
/// D3 mechanism that resolves a trait-object/generic-bound call to the trait's
/// own default-bodied method chunk.
///
/// This is deliberately a NEW, narrowly-scoped function rather than a change to
/// [`normalize_base_type`]: that function is used by `parse_return_type` and
/// other callers this slice must not perturb, so baking dyn-awareness into it
/// would silently change return-type normalization too. Every D3 call site tries
/// `resolve_dyn_trait_name` FIRST and falls back to `normalize_base_type` on
/// `None`.
///
/// - strips a leading `&`/`&mut`/lifetime exactly as `normalize_base_type` does,
/// - `dyn Trait[ + AutoTrait ...]` → first `+`-segment, normalized → `Some(trait)`,
/// - `Box<dyn …>` / `Arc<dyn …>` / `Rc<dyn …>` (exact outer identifier + a
///   `dyn ` interior) → unwrap one layer and recurse → `Some(trait)`,
/// - anything else (concrete type, tuple, primitive, `Box<Widget>`,
///   `Cow<'_, dyn T>`, a custom wrapper) → `None`.
pub fn resolve_dyn_trait_name(type_text: &str) -> Option<String> {
    // Strip leading references / lifetimes, mirroring `normalize_base_type`
    // steps 1-2, so `&dyn Trait` / `&'a dyn Trait` are recognized.
    let mut s = type_text.trim();
    loop {
        let stripped = s
            .strip_prefix("&mut ")
            .or_else(|| s.strip_prefix("&mut"))
            .or_else(|| s.strip_prefix('&'))
            .map(str::trim_start);
        match stripped {
            Some(rest) => s = rest,
            None => break,
        }
    }
    if let Some(rest) = s.strip_prefix('\'')
        && let Some((_, after)) = rest.split_once(char::is_whitespace)
    {
        s = after.trim_start();
        s = s.strip_prefix("mut ").unwrap_or(s).trim_start();
    }

    // `dyn Trait[ + …]` — take the first bound, normalize.
    if let Some(rest) = s.strip_prefix("dyn ") {
        let first = rest.split('+').next().unwrap_or(rest).trim();
        let name = normalize_base_type(first);
        return (!name.is_empty()).then_some(name);
    }

    // `Box<dyn …>` / `Arc<dyn …>` / `Rc<dyn …>` — unwrap one layer, recurse.
    // The `<` requirement after the exact wrapper identifier rejects lookalikes
    // (`BoxThing<dyn X>` → strip "Box" leaves "Thing<…>", not "<…>").
    for wrapper in ["Box", "Arc", "Rc"] {
        if let Some(inner) = s
            .strip_prefix(wrapper)
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('<'))
            .and_then(|r| r.strip_suffix('>'))
        {
            let inner = inner.trim();
            if inner.starts_with("dyn ") {
                return resolve_dyn_trait_name(inner);
            }
        }
    }
    None
}

/// Parse a method/function signature's declared return type, normalized via
/// [`normalize_base_type`] (D1c-1: populates `ChunkIndex::by_return_type`,
/// the registry that powers method-chain resolution — `x.foo().bar()` binds
/// `bar` to the method defined on `foo`'s return type).
///
/// `signature` is `RawChunk.signature`'s captured text. By construction
/// (`walker_helpers::default_signature`) this is the chunk node's FIRST LINE
/// only, right-trimmed, with any trailing `{` already stripped — a return
/// type declared on a LATER line (a rustfmt-wrapped signature) is invisible
/// here and correctly yields `None`, the same as a genuine unit-return
/// signature: nothing unsafe happens, the chain simply doesn't resolve for
/// that method (a documented recall ceiling, not a bug). `owner_type` is the
/// method's OWNING type's name, already normalized — the caller passes the
/// same `normalize_base_type(parent_fqn)` value used to key `by_type_and_name`.
///
/// - Finds the return arrow via a bracket-depth-aware scan of the signature's
///   head (before the body `{` / `where` clause / end-of-string): the
///   method's own return arrow is the first `"-> "` at bracket depth 0 (not
///   nested inside `()`/`<>`/`[]`). An arrow inside a parameter type (a
///   fn-pointer parameter `f: fn() -> i32`) or inside the return type itself
///   (`Box<dyn Fn() -> T>`, `impl Fn() -> T`) is at depth ≥1 and correctly
///   ignored.
/// - No return arrow (unit `()` return, e.g. `"fn f(&self)"`) -> `None`
///   (nothing to index; a chain through a unit-returning method can't
///   resolve, which is correct).
/// - The type expression is normalized via `normalize_base_type`. A
///   normalized result of exactly `"Self"` is replaced with `owner_type`
///   VERBATIM (the builder pattern: `fn with(self) -> Self` on `Widget`
///   yields `"Widget"`, not `"Self"`) — this also covers `&Self`/`&mut Self`
///   since `normalize_base_type` strips the reference first.
/// - Wrapper types (`Result<Widget>`, `Option<T>`, `Vec<T>`) normalize to
///   their OUTER base only (`normalize_base_type` truncates at the first
///   `<`) — a documented recall ceiling; unwrapping to the inner `T` is a
///   follow-on (D1c-1b).
pub fn parse_return_type(signature: &str, owner_type: &str) -> Option<String> {
    let head = signature.find('{').map_or(signature, |i| &signature[..i]);
    let head = head.find(" where ").map_or(head, |i| &head[..i]);
    // The method's OWN return arrow is the first `-> ` at bracket depth 0 —
    // NOT `rfind`, which would wrongly pick an arrow embedded in the return
    // type itself (`-> Box<dyn Fn() -> Widget>`, `-> impl Fn() -> i32`) or,
    // historically, one inside a fn-pointer parameter. Track `()`/`<>`/`[]`
    // depth; a `>` that is the tail of `->` never counts as a bracket close.
    let bytes = head.as_bytes();
    let mut depth: i32 = 0;
    let mut arrow: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'>') {
            if depth == 0 {
                arrow = Some(i);
                break;
            }
            i += 2; // skip the whole `->` so its `>` isn't treated as a close
            continue;
        }
        match bytes[i] {
            b'(' | b'<' | b'[' => depth += 1,
            b')' | b']' | b'>' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    let arrow = arrow?;
    let ty = head[arrow + 2..].trim();
    if ty.is_empty() {
        return None;
    }
    let normalized = normalize_base_type(ty);
    if normalized == "Self" {
        Some(owner_type.to_string())
    } else {
        Some(normalized)
    }
}

/// Parse a struct declaration's FULL source text into `(field_name,
/// normalized_field_type)` pairs (D1c-2: populates `ChunkIndex::by_field_type`,
/// the registry that powers field-access resolution — `x.field.method()`
/// binds `.method()` to the method defined on `field`'s declared type).
///
/// `content` is `RawChunk.content`'s captured text for a STRUCT chunk — the
/// `struct_item` node's FULL source span, unlike `signature` (first-line-only;
/// see [`parse_return_type`]'s doc comment). This function is only ever
/// called for `chunk_type == "struct"` rows (see [`ChunkIndex::build_from`]).
///
/// - Locates the struct BODY: the first top-level `{` in `content` through
///   its MATCHING `}` (a simple `{`/`}` depth count — Rust struct
///   headers/generics/where-clauses never themselves contain `{`/`}`, so the
///   first `{` is always the body opening). No `{` at all (a tuple struct
///   `struct T(u32, Foo);` or a unit struct `struct U;`) -> empty (no named
///   fields; documented D1c-2 ceiling — positional/tuple-struct fields
///   aren't indexed).
/// - Splits the body on top-level (bracket-depth-0 w.r.t. `(`/`<`/`[`/`{`)
///   commas -> one segment per field declaration. A comma nested inside a
///   generic argument list (`HashMap<K, V>`) or an attribute's argument list
///   (`#[serde(rename = "a", default)]`, which opens with `[`) is NOT a
///   splitter.
/// - Each segment is parsed by [`parse_one_field`]: leading doc-comment/
///   attribute lines are dropped, a leading `pub`/`pub(...)` visibility
///   modifier is stripped, then the FIRST `:` splits `(field_name,
///   field_type)` (a field name is a plain identifier and can never itself
///   contain `:`, so no depth-tracking is needed for this inner split). A
///   segment with no `:` after stripping (an empty trailing-comma segment, a
///   comment-only segment, or malformed text) is skipped.
/// - `field_type` is normalized via [`normalize_base_type`] (strips
///   `&`/lifetimes/generics -> base; `HashMap<K, V>` -> `"HashMap"`, `&'a
///   Widget` -> `"Widget"` — same wrapper-outer-base ceiling
///   `parse_return_type` documents).
pub fn parse_struct_fields(content: &str) -> Vec<(String, String, bool)> {
    let Some(open) = content.find('{') else {
        return Vec::new();
    };
    let bytes = content.as_bytes();
    let mut depth: i32 = 0;
    let mut close = None;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return Vec::new();
    };
    let body = &content[open + 1..close];

    // Split on top-level commas (bracket-depth-aware over `(`/`<`/`[`/`{`).
    let body_bytes = body.as_bytes();
    let mut segments: Vec<&str> = Vec::new();
    let mut seg_start = 0usize;
    let mut d: i32 = 0;
    let mut i = 0;
    while i < body_bytes.len() {
        // A `>` that is the tail of `->` is NOT a bracket close — skip the whole
        // `->` so a function type (`fn() -> T`, `Box<dyn Fn() -> T>`) can't drive
        // `d` negative and desync the split (mirrors parse_return_type).
        if body_bytes[i] == b'-' && body_bytes.get(i + 1) == Some(&b'>') {
            i += 2;
            continue;
        }
        match body_bytes[i] {
            b'(' | b'<' | b'[' | b'{' => d += 1,
            b')' | b'>' | b']' | b'}' => d -= 1,
            b',' if d == 0 => {
                segments.push(&body[seg_start..i]);
                seg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    segments.push(&body[seg_start..]);

    segments.into_iter().filter_map(parse_one_field).collect()
}

/// Parse a single comma-delimited field-declaration segment (see
/// [`parse_struct_fields`]) into `(name, normalized_type)`. Drops leading
/// doc-comment (`//...`) / attribute (`#[...]`) lines, strips a leading
/// `pub`/`pub(...)` visibility prefix (guarding against a false match on a
/// field name that merely STARTS WITH "pub", e.g. `public_key`), then splits
/// on the first `:`.
///
/// Only SINGLE-LINE attributes are skipped (each continuation line must
/// itself start with `#[`) — a multi-line-wrapped attribute
/// (`#[serde(\n    rename = "x"\n)]`) has continuation lines that don't
/// match either skip prefix, so they get folded into the field's own
/// declaration text and the `:`-split then yields a garbage key. This is a
/// documented, fail-closed ceiling: the garbage key can never be looked up
/// (it contains characters no real field name has), so the affected field
/// is simply absent from `by_field_type` — a missed recall gain, never a
/// mis-bind.
fn parse_one_field(segment: &str) -> Option<(String, String, bool)> {
    let mut lines = segment.lines();
    let mut rest_lines: Vec<&str> = Vec::new();
    for line in lines.by_ref() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") || t.starts_with("#[") {
            continue;
        }
        rest_lines.push(line);
        break;
    }
    rest_lines.extend(lines);
    let rest = rest_lines.join(" ");
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    let rest = if rest.starts_with("pub(") {
        let after = &rest[3..];
        let paren_close = after.find(')')?;
        after[paren_close + 1..].trim_start()
    } else if let Some(after) = rest.strip_prefix("pub ") {
        after.trim_start()
    } else {
        rest
    };

    let (name, ty) = rest.split_once(':')?;
    let name = name.trim();
    let ty = ty.trim();
    if name.is_empty() || ty.is_empty() {
        return None;
    }
    // D3: a `dyn`/smart-pointer trait field stores the TRAIT name and flags
    // itself; every other field stores its normalized base, unflagged.
    match resolve_dyn_trait_name(ty) {
        Some(trait_name) => Some((name.to_string(), trait_name, true)),
        None => Some((name.to_string(), normalize_base_type(ty), false)),
    }
}

impl ChunkIndex {
    pub fn insert(&mut self, module_path: String, name: String, id: Uuid) {
        self.by_path_and_name
            .insert((module_path.clone(), name.clone()), id);
        self.by_name.entry(name).or_default().push(id);
        self.chunk_module.insert(id, module_path);
    }

    /// Indexes a method under its owning type's BARE base name, stripping any
    /// generic parameter list (`Holder<T>` -> `Holder`). `parent_fqn` for a
    /// generic impl block (`impl<T> Holder<T> { .. }`) is recorded WITH
    /// generics — see `tests/fixtures/rust/11-generic-qualified-impls.rs.golden.json`
    /// — but the call-receiver side (`trailing_type_name` in
    /// `languages/rust.rs`) already strips turbofish generics down to the
    /// bare type for a qualified call (`Holder::<i32>::x()` -> "Holder").
    /// Centralizing the strip here (the sole `insert_method` call site) keeps
    /// both sides keyed on the same bare name; non-generic types are
    /// unaffected. Two impls that strip to the same base (e.g. a hypothetical
    /// `Holder<T>` and specialized `Holder<i32>`) collide into one bucket of
    /// len() > 1, which tier-0 in `resolve_call_edge` treats as ambiguous and
    /// falls through — fails closed, not incorrect.
    pub fn insert_method(&mut self, type_name: String, method: String, id: Uuid) {
        let base = normalize_base_type(&type_name);
        self.by_type_and_name
            .entry((base, method))
            .or_default()
            .push(id);
    }

    /// Insert a method's parsed return type into the return-type registry,
    /// keyed `(normalize_base_type(owner_type), method)`. Mirrors
    /// `insert_method`'s normalization so both sides of a chain lookup agree.
    ///
    /// A (type, method) key that has already produced TWO DIFFERENT return
    /// types (e.g. a generic-impl collision, mirroring `insert_method`'s
    /// same-base-type collision) is dropped from `by_return_type` and
    /// permanently excluded via `return_type_conflicts` — fail-closed: a
    /// (type, method) whose return type can't be trusted to mean one thing
    /// is worse than not indexing it at all.
    pub fn insert_return_type(&mut self, owner_type: &str, method: String, return_type: String) {
        let key = (normalize_base_type(owner_type), method);
        if self.return_type_conflicts.contains(&key) {
            return;
        }
        if let Some(existing) = self.by_return_type.get(&key) {
            if *existing != return_type {
                self.by_return_type.remove(&key);
                self.return_type_conflicts.insert(key);
            }
            return;
        }
        self.by_return_type.insert(key, return_type);
    }

    /// Insert a struct's parsed field type into the field-type registry,
    /// keyed `(normalize_base_type(owner_type), field)`. Mirrors
    /// `insert_return_type`'s normalization/poisoning so both sides of a
    /// field-access lookup agree and stay fail-closed on conflict.
    ///
    /// A (type, field) key that has already produced TWO DIFFERENT field
    /// types (e.g. a mod-nested same-bare-name struct collision) is dropped
    /// from `by_field_type` and permanently excluded via
    /// `field_type_conflicts` — fail-closed: a (type, field) whose type
    /// can't be trusted to mean one thing is worse than not indexing it.
    pub fn insert_field_type(&mut self, owner_type: &str, field: String, field_type: String) {
        let key = (normalize_base_type(owner_type), field);
        if self.field_type_conflicts.contains(&key) {
            return;
        }
        if let Some(existing) = self.by_field_type.get(&key) {
            if *existing != field_type {
                self.by_field_type.remove(&key);
                self.field_type_conflicts.insert(key);
            }
            return;
        }
        self.by_field_type.insert(key, field_type);
    }

    /// `rows` is `(id, name, module_path, parent_fqn, signature, chunk_type,
    /// content)`. `signature` (D1c-1) is parsed via [`parse_return_type`]
    /// into `by_return_type` for every row that has BOTH a `parent_fqn` (an
    /// owning type) and a parseable return-type signature. `content` (D1c-2)
    /// is parsed via [`parse_struct_fields`] into `by_field_type`, keyed on
    /// the row's OWN `name` (a struct chunk's `name` is always its bare,
    /// unqualified identifier — see `rust_resolve_name` — matching how
    /// `insert_method` already normalizes the OTHER side of a lookup down to
    /// the same bare base via `normalize_base_type`), for every row whose
    /// `chunk_type == "struct"`. Anything else contributes nothing to either
    /// registry (additive — `by_type_and_name` population is unchanged).
    pub fn build_from(
        rows: Vec<(
            Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
        )>,
    ) -> Self {
        let mut idx = Self::default();
        for (id, name, mp, parent_fqn, signature, chunk_type, content) in rows {
            idx.insert(mp, name.clone(), id);
            if let Some(t) = parent_fqn {
                let owner = normalize_base_type(&t);
                idx.insert_method(t, name.clone(), id);
                if let Some(sig) = signature
                    && let Some(ret) = parse_return_type(&sig, &owner)
                {
                    idx.insert_return_type(&owner, name.clone(), ret);
                }
            }
            if chunk_type == "struct" {
                for (field, field_type, is_trait) in parse_struct_fields(&content) {
                    // D3: record dyn-trait fields under the SAME key shape
                    // insert_field_type normalizes to, so the resolver's
                    // field_type_is_trait check lines up with by_field_type.
                    if is_trait {
                        idx.field_type_is_trait
                            .insert((normalize_base_type(&name), field.clone()));
                    }
                    idx.insert_field_type(&name, field, field_type);
                }
            }
        }
        idx
    }
}

/// local_name -> (resolved module_path, source_name) (built from Import edges).
///
/// `source_name` is the symbol's name in the defining module; it equals
/// `local_name` for non-aliased imports and differs for aliased ones
/// (`import { bar as baz }` → `baz -> (module, "bar")`). Tier-1 resolution looks
/// the chunk up by `(module, source_name)` so aliased calls bridge to the real
/// definition.
pub type ImportMap = HashMap<String, (String, String)>;

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEdge {
    pub src_chunk_id: Uuid,
    pub tgt_chunk_id: Uuid,
    pub kind: EdgeKind,
    pub confidence: f32,
    pub method: String,
    pub line: Option<usize>,
    /// EXT-6b: for `References` edges, "type" (or later "import"); `None` for calls.
    pub ref_kind: Option<String>,
}

/// Confidence resolution for a Call edge. Checked top-down; confidence roughly
/// tracks specificity, not check order (tier 0 at 0.9 sits below tier 1's 1.0):
/// * tier 0 — the edge carries an explicit receiver type and `(type, name)`
///   resolves uniquely in `idx.by_type_and_name`; ambiguous/miss falls
///   through. Path-qualified (`Foo::bar()`, no `recv_inferred`) → 0.9
///   "type_qualified"; inferred value-method receiver (`x.m()`/`self.m()`,
///   `recv_inferred=true`) → 0.8 "receiver_type"; a trait-object/generic-bound
///   inferred receiver (`recv_is_trait_bound=true`, D3) → 0.5 "trait_default"
///   (resolves to the trait's OWN default-bodied method, a semantic approximation);
/// * tier 0-chain (D1c-1) — the edge carries `chain_recv_type`/
///   `chain_recv_method` (the inner link of a 2-hop method chain
///   `x.foo().bar()`); `by_return_type[(chain_recv_type,chain_recv_method)]`
///   gives `foo`'s return type `R`, then `(R, name)` resolves uniquely in
///   `idx.by_type_and_name` → 0.75 "method_chain". Ambiguous/miss at either
///   hop falls through;
/// * tier 0-field (D1c-2) — the edge carries `field_recv_type`/`field_name`
///   (a field-access receiver `x.field.method()`/`self.field.method()`);
///   `by_field_type[(field_recv_type,field_name)]` gives the field's
///   declared type `FT`, then `(FT, name)` resolves uniquely in
///   `idx.by_type_and_name` → 0.75 "field_type", or → 0.5 "trait_default"
///   (D3) when `(field_recv_type,field_name)` is in `field_type_is_trait`
///   (the field was an `Arc<dyn T>`-style trait object). Ambiguous/miss at
///   either hop falls through;
/// * tier 1 — `import_map` exact per-symbol hit (1.0, "import_resolved");
/// * tier 2 — same module (0.8, "same_file") — a local definition shadows imports;
/// * tier 3 — unique `name` among the caller file's imported modules
///   (0.7, "import_scoped") — uses the import graph to disambiguate a call that is
///   ambiguous repo-wide but unique within what this file imports;
/// * tier 4 — repo-wide unique name (0.6, "heuristic"); ambiguous -> fall through.
///
/// `imported_modules` is the set of module paths the caller's file `IMPORTS_FROM`
/// (built by the pipeline from the resolved Import edges). Empty => tier 3 is a
/// no-op, preserving the prior three-tier behavior.
pub fn resolve_call_edge(
    edge: &RawEdge,
    src_chunk_id: Uuid,
    src_module: &str,
    idx: &ChunkIndex,
    import_map: &ImportMap,
    imported_modules: &std::collections::HashSet<String>,
) -> Option<ResolvedEdge> {
    let name = match &edge.target {
        EdgeEndpoint::Name { name, .. } => name.clone(),
        _ => return None,
    };

    // D1 tier-0 (runs FIRST, only for calls carrying a receiver type). A path-
    // qualified call (`Foo::bar()`) has recv_type but no recv_inferred → 0.9
    // "type_qualified" (D1-core). A value-method call (`x.m()`/`self.m()`) whose
    // receiver was resolved through the walker's intra-chunk type_env carries
    // recv_inferred=true → 0.8 "receiver_type" (D1b). Both use the same
    // (type, method) lookup, unique-hit-wins, fail-closed fall-through.
    if let Some(crate::types::MetadataValue::String(recv_type)) =
        edge.metadata.fields.get("edge.recv_type")
        && let Some(ids) = idx.by_type_and_name.get(&(recv_type.clone(), name.clone()))
        && ids.len() == 1
    {
        let inferred = matches!(
            edge.metadata.fields.get("edge.recv_inferred"),
            Some(crate::types::MetadataValue::Bool(true))
        );
        let is_trait_bound = matches!(
            edge.metadata.fields.get("edge.recv_is_trait_bound"),
            Some(crate::types::MetadataValue::Bool(true))
        );
        // D3: an inferred receiver whose declared type was a trait object /
        // trait-bound generic resolves to the trait's OWN default-bodied method
        // chunk — a semantic approximation, so 0.5 "trait_default", below every
        // concrete tier. `recv_is_trait_bound` is only ever set alongside
        // `recv_inferred`, so it takes precedence over plain receiver_type here.
        debug_assert!(
            !is_trait_bound || inferred,
            "edge.recv_is_trait_bound must only ever be set alongside edge.recv_inferred"
        );
        let (confidence, method) = if is_trait_bound {
            (0.5, "trait_default")
        } else if inferred {
            (0.8, "receiver_type")
        } else {
            (0.9, "type_qualified")
        };
        return Some(ResolvedEdge {
            src_chunk_id,
            tgt_chunk_id: ids[0],
            kind: EdgeKind::Call,
            confidence,
            method: method.into(),
            line: edge.line,
            ref_kind: None,
        });
    }

    // D1c-1 tier (chain): the edge's INNER receiver was a typed token/self
    // whose method's return type chains to THIS call's target. Ordered
    // AFTER tier-0 (a direct receiver type always wins when present) and
    // BEFORE tier-1 import_resolved, mirroring tier-0's position in the
    // cascade (specificity, not check-order-by-confidence-number).
    if let Some(crate::types::MetadataValue::String(chain_type)) =
        edge.metadata.fields.get("edge.chain_recv_type")
        && let Some(crate::types::MetadataValue::String(chain_method)) =
            edge.metadata.fields.get("edge.chain_recv_method")
        && let Some(inner_return) = idx
            .by_return_type
            .get(&(chain_type.clone(), chain_method.clone()))
        && let Some(ids) = idx
            .by_type_and_name
            .get(&(inner_return.clone(), name.clone()))
        && ids.len() == 1
    {
        return Some(ResolvedEdge {
            src_chunk_id,
            tgt_chunk_id: ids[0],
            kind: EdgeKind::Call,
            confidence: 0.75,
            method: "method_chain".into(),
            line: edge.line,
            ref_kind: None,
        });
    }

    // D1c-2 tier (field): the edge's INNER receiver was a typed token/self
    // whose FIELD's declared type chains to THIS call's target. Ordered
    // AFTER the D1c-1 chain tier and BEFORE tier-1 import_resolved — same
    // "specificity, not confidence-number" cascade position as every other
    // D1/D1b/D1c tier.
    if let Some(crate::types::MetadataValue::String(field_recv_type)) =
        edge.metadata.fields.get("edge.field_recv_type")
        && let Some(crate::types::MetadataValue::String(field_name)) =
            edge.metadata.fields.get("edge.field_name")
        && let Some(field_type) = idx
            .by_field_type
            .get(&(field_recv_type.clone(), field_name.clone()))
        && let Some(ids) = idx
            .by_type_and_name
            .get(&(field_type.clone(), name.clone()))
        && ids.len() == 1
    {
        // D3: if the field's declared type was a trait object (`Arc<dyn T>` …),
        // the 2-hop target is the trait's OWN default method — relabel the hit
        // 0.5 "trait_default" (the shared D3 label), else the usual 0.75
        // "field_type". Same two lookup hops, same fail-closed fall-through.
        let (confidence, method) = if idx
            .field_type_is_trait
            .contains(&(field_recv_type.clone(), field_name.clone()))
        {
            (0.5, "trait_default")
        } else {
            (0.75, "field_type")
        };
        return Some(ResolvedEdge {
            src_chunk_id,
            tgt_chunk_id: ids[0],
            kind: EdgeKind::Call,
            confidence,
            method: method.into(),
            line: edge.line,
            ref_kind: None,
        });
    }

    // Tier 1: import_map (exact per-symbol import binding). The call uses the
    // LOCAL name; we look the definition chunk up by the SOURCE name so aliased
    // imports (local != source) bridge to the real definition.
    if let Some((resolved_module, source)) = import_map.get(&name)
        && let Some(&tgt) = idx
            .by_path_and_name
            .get(&(resolved_module.clone(), source.clone()))
    {
        return Some(ResolvedEdge {
            src_chunk_id,
            tgt_chunk_id: tgt,
            kind: EdgeKind::Call,
            confidence: 1.0,
            method: "import_resolved".into(),
            line: edge.line,
            ref_kind: None,
        });
    }
    // Tier 2: same module (local definition shadows imports)
    if let Some(&tgt) = idx
        .by_path_and_name
        .get(&(src_module.to_string(), name.clone()))
    {
        return Some(ResolvedEdge {
            src_chunk_id,
            tgt_chunk_id: tgt,
            kind: EdgeKind::Call,
            confidence: 0.8,
            method: "same_file".into(),
            line: edge.line,
            ref_kind: None,
        });
    }
    // Tier 3: unique among the modules this file imports. Disambiguates a call
    // that is ambiguous repo-wide but resolves uniquely within the import set.
    if !imported_modules.is_empty()
        && let Some(candidates) = idx.by_name.get(&name)
    {
        let mut scoped = candidates.iter().filter(|&&id| {
            idx.chunk_module
                .get(&id)
                .is_some_and(|m| imported_modules.contains(m))
        });
        if let Some(&tgt) = scoped.next() {
            // Exactly one match among imported modules?
            if scoped.next().is_none() {
                return Some(ResolvedEdge {
                    src_chunk_id,
                    tgt_chunk_id: tgt,
                    kind: EdgeKind::Call,
                    confidence: 0.7,
                    method: "import_scoped".into(),
                    line: edge.line,
                    ref_kind: None,
                });
            }
        }
    }
    // Tier 4: repo-wide unique
    if let Some(candidates) = idx.by_name.get(&name)
        && candidates.len() == 1
    {
        return Some(ResolvedEdge {
            src_chunk_id,
            tgt_chunk_id: candidates[0],
            kind: EdgeKind::Call,
            confidence: 0.6,
            method: "heuristic".into(),
            line: edge.line,
            ref_kind: None,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EdgeMetadata, Provenance};
    use std::collections::HashSet;

    fn no_imports() -> HashSet<String> {
        HashSet::new()
    }

    fn call_edge(name: &str) -> RawEdge {
        RawEdge {
            source: EdgeEndpoint::ByteRange { start: 0, end: 5 },
            target: EdgeEndpoint::Name {
                name: name.into(),
                module_specifier: None,
            },
            kind: EdgeKind::Call,
            provenance: Provenance::Static,
            line: Some(7),
            metadata: EdgeMetadata::default(),
        }
    }

    fn call_edge_with_recv(name: &str, recv_type: &str) -> RawEdge {
        let mut e = call_edge(name);
        e.metadata.fields.insert(
            "edge.recv_type".into(),
            crate::types::MetadataValue::String(recv_type.into()),
        );
        e
    }

    fn call_edge_with_inferred_recv(name: &str, recv_type: &str) -> RawEdge {
        let mut e = call_edge_with_recv(name, recv_type);
        e.metadata.fields.insert(
            "edge.recv_inferred".into(),
            crate::types::MetadataValue::Bool(true),
        );
        e
    }

    fn call_edge_with_trait_bound_recv(name: &str, recv_type: &str) -> RawEdge {
        let mut e = call_edge_with_inferred_recv(name, recv_type);
        e.metadata.fields.insert(
            "edge.recv_is_trait_bound".into(),
            crate::types::MetadataValue::Bool(true),
        );
        e
    }

    fn call_edge_with_chain_recv(name: &str, chain_type: &str, chain_method: &str) -> RawEdge {
        let mut e = call_edge(name);
        e.metadata.fields.insert(
            "edge.chain_recv_type".into(),
            crate::types::MetadataValue::String(chain_type.into()),
        );
        e.metadata.fields.insert(
            "edge.chain_recv_method".into(),
            crate::types::MetadataValue::String(chain_method.into()),
        );
        e
    }

    fn call_edge_with_field_recv(name: &str, field_type: &str, field_name: &str) -> RawEdge {
        let mut e = call_edge(name);
        e.metadata.fields.insert(
            "edge.field_recv_type".into(),
            crate::types::MetadataValue::String(field_type.into()),
        );
        e.metadata.fields.insert(
            "edge.field_name".into(),
            crate::types::MetadataValue::String(field_name.into()),
        );
        e
    }

    #[test]
    fn tier0_receiver_type_inferred_resolves_at_0_8() {
        // An inferred value-method receiver (`x.tick()` with x: Counter)
        // resolves to Counter's `tick`, at 0.8 "receiver_type" (NOT 0.9).
        let tick = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("app".into(), "tick".into(), Uuid::new_v4()); // free-fn decoy
        idx.insert_method("Counter".into(), "tick".into(), tick);
        let r = resolve_call_edge(
            &call_edge_with_inferred_recv("tick", "Counter"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, tick);
        assert_eq!(r.confidence, 0.8);
        assert_eq!(r.method, "receiver_type");
    }

    #[test]
    fn tier0_path_qualified_stays_type_qualified_at_0_9() {
        // Regression guard: a path-qualified edge (no recv_inferred) still
        // resolves at 0.9 "type_qualified" — the D1b split must not touch it.
        let foo_new = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_method("Foo".into(), "new".into(), foo_new);
        let r = resolve_call_edge(
            &call_edge_with_recv("new", "Foo"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, foo_new);
        assert_eq!(r.confidence, 0.9);
        assert_eq!(r.method, "type_qualified");
    }

    #[test]
    fn receiver_type_ambiguous_falls_through() {
        // (Counter, tick) has two defs (e.g. a generic-impl collision) →
        // ambiguous → fall through to the existing cascade → None here.
        let mut idx = ChunkIndex::default();
        idx.insert_method("Counter".into(), "tick".into(), Uuid::new_v4());
        idx.insert_method("Counter".into(), "tick".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge_with_inferred_recv("tick", "Counter"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn tier0_type_qualified_resolves() {
        // `Foo::new()` resolves to Foo's `new`, NOT a same-module free `new`.
        let foo_new = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("app".into(), "new".into(), Uuid::new_v4()); // same-module free new (decoy)
        idx.insert_method("Foo".into(), "new".into(), foo_new);
        let r = resolve_call_edge(
            &call_edge_with_recv("new", "Foo"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, foo_new);
        assert_eq!(r.confidence, 0.9);
        assert_eq!(r.method, "type_qualified");
    }

    #[test]
    fn tier0_type_qualified_generic_type_strips_to_base() {
        // `impl<T> Holder<T> { fn x() }` records parent_fqn "Holder<T>" (see
        // tests/fixtures/rust/11-generic-qualified-impls.rs.golden.json), but
        // the call-receiver side (`trailing_type_name`) yields the bare base
        // "Holder" for `Holder::<i32>::x()`. insert_method must strip the
        // generic param list so both sides key on the same bare type.
        let holder_x = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_method("Holder<T>".into(), "x".into(), holder_x);
        let r = resolve_call_edge(
            &call_edge_with_recv("x", "Holder"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, holder_x);
        assert_eq!(r.method, "type_qualified");
    }

    #[test]
    fn type_qualified_ambiguous_falls_through() {
        // (Foo,new) has two defs → ambiguous → fall through to existing cascade
        // (which also can't resolve here) → None.
        let mut idx = ChunkIndex::default();
        idx.insert_method("Foo".into(), "new".into(), Uuid::new_v4());
        idx.insert_method("Foo".into(), "new".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge_with_recv("new", "Foo"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn tier0_trait_bound_receiver_resolves_at_0_5_trait_default() {
        // A trait-object/generic-bound receiver (`x.hello()` with x: dyn Greeter)
        // resolves to Greeter's default `hello` at 0.5 "trait_default".
        let hello = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_method("Greeter".into(), "hello".into(), hello);
        let r = resolve_call_edge(
            &call_edge_with_trait_bound_recv("hello", "Greeter"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, hello);
        assert_eq!(r.confidence, 0.5);
        assert_eq!(r.method, "trait_default");
    }

    #[test]
    fn tier0_inferred_without_trait_flag_stays_receiver_type() {
        // Regression guard: the SAME inferred edge MINUS recv_is_trait_bound
        // still resolves 0.8 "receiver_type" — the new branch must not leak.
        let hello = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_method("Greeter".into(), "hello".into(), hello);
        let r = resolve_call_edge(
            &call_edge_with_inferred_recv("hello", "Greeter"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.confidence, 0.8);
        assert_eq!(r.method, "receiver_type");
    }

    #[test]
    fn tier0_trait_bound_ambiguous_falls_through() {
        // (Greeter, hello) has two defs → ambiguous → fall through (None here).
        let mut idx = ChunkIndex::default();
        idx.insert_method("Greeter".into(), "hello".into(), Uuid::new_v4());
        idx.insert_method("Greeter".into(), "hello".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge_with_trait_bound_recv("hello", "Greeter"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn tier0_trait_bound_missing_hit_falls_through() {
        // No (Greeter, hello) chunk (trait has no default-bodied hello) → miss
        // → fall through to the existing cascade (None here).
        let mut idx = ChunkIndex::default();
        idx.insert_method("Greeter".into(), "other".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge_with_trait_bound_recv("hello", "Greeter"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn no_recv_type_uses_existing_cascade() {
        // A bare call with no recv_type still resolves via tier-2 same_file.
        let id_a = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("x".into(), "helper".into(), id_a);
        let r = resolve_call_edge(
            &call_edge("helper"),
            Uuid::new_v4(),
            "x",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.method, "same_file");
    }

    #[test]
    fn tier1_import_resolved() {
        let id_a = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("lib".into(), "add".into(), id_a);
        let mut im = ImportMap::default();
        im.insert("add".into(), ("lib".into(), "add".into()));
        let r = resolve_call_edge(
            &call_edge("add"),
            Uuid::new_v4(),
            "main",
            &idx,
            &im,
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, id_a);
        assert_eq!(r.confidence, 1.0);
        assert_eq!(r.method, "import_resolved");
        assert_eq!(r.line, Some(7));
    }

    #[test]
    fn tier1_aliased_import_bridges_to_source() {
        // `import { bar as baz } from "m"` then `baz()`: the call's target name
        // is the LOCAL alias `baz`, but the definition chunk in module `m` is
        // named `bar`. The import_map entry baz -> (m, bar) bridges the alias to
        // the source so tier-1 resolves at 1.0 "import_resolved".
        let bar_id = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("m".into(), "bar".into(), bar_id);
        let mut im = ImportMap::default();
        im.insert("baz".into(), ("m".into(), "bar".into()));
        let r = resolve_call_edge(
            &call_edge("baz"),
            Uuid::new_v4(),
            "caller",
            &idx,
            &im,
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, bar_id);
        assert_eq!(r.confidence, 1.0);
        assert_eq!(r.method, "import_resolved");
    }

    #[test]
    fn tier2_same_file() {
        let id_a = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("x".into(), "helper".into(), id_a);
        let r = resolve_call_edge(
            &call_edge("helper"),
            Uuid::new_v4(),
            "x",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, id_a);
        assert_eq!(r.confidence, 0.8);
        assert_eq!(r.method, "same_file");
    }

    #[test]
    fn tier3_repo_unique() {
        let id_a = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("util".into(), "uniq".into(), id_a);
        let r = resolve_call_edge(
            &call_edge("uniq"),
            Uuid::new_v4(),
            "main",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.confidence, 0.6);
        assert_eq!(r.method, "heuristic");
    }

    #[test]
    fn tier3_import_scoped_disambiguates() {
        // `render` is defined in TWO modules (ambiguous repo-wide) -> tier-4
        // heuristic would drop it. But the caller's file imports only `ui`, so
        // import_scoped resolves it uniquely.
        let ui_render = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("ui".into(), "render".into(), ui_render);
        idx.insert("pdf".into(), "render".into(), Uuid::new_v4());
        let imports: HashSet<String> = ["ui".to_string()].into_iter().collect();
        let r = resolve_call_edge(
            &call_edge("render"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &imports,
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, ui_render);
        assert_eq!(r.confidence, 0.7);
        assert_eq!(r.method, "import_scoped");
    }

    #[test]
    fn same_file_shadows_import_scoped() {
        // `render` defined both in the caller's own module and an imported one.
        // same_file (tier 2) must win.
        let local = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert("app".into(), "render".into(), local);
        idx.insert("ui".into(), "render".into(), Uuid::new_v4());
        let imports: HashSet<String> = ["ui".to_string()].into_iter().collect();
        let r = resolve_call_edge(
            &call_edge("render"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &imports,
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, local);
        assert_eq!(r.method, "same_file");
    }

    #[test]
    fn import_scoped_ambiguous_within_imports_falls_through() {
        // `render` in two imported modules -> ambiguous even within imports;
        // import_scoped falls through, and tier-4 also drops (repo-wide >1).
        let mut idx = ChunkIndex::default();
        idx.insert("ui".into(), "render".into(), Uuid::new_v4());
        idx.insert("web".into(), "render".into(), Uuid::new_v4());
        let imports: HashSet<String> = ["ui".to_string(), "web".to_string()].into_iter().collect();
        assert!(
            resolve_call_edge(
                &call_edge("render"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &imports,
            )
            .is_none()
        );
    }

    #[test]
    fn tier3_ambiguous_drops() {
        let mut idx = ChunkIndex::default();
        idx.insert("a".into(), "amb".into(), Uuid::new_v4());
        idx.insert("b".into(), "amb".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge("amb"),
                Uuid::new_v4(),
                "any",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn method_index_build_and_lookup() {
        let m_new = Uuid::new_v4();
        let idx = ChunkIndex::build_from(vec![
            (
                m_new,
                "new".into(),
                "llm".into(),
                Some("OpenAiLlmProvider".into()),
                None,
                "method".into(),
                String::new(),
            ),
            (
                Uuid::new_v4(),
                "free_fn".into(),
                "util".into(),
                None,
                None,
                "function".into(),
                String::new(),
            ),
        ]);
        assert_eq!(
            idx.by_type_and_name
                .get(&("OpenAiLlmProvider".into(), "new".into())),
            Some(&vec![m_new])
        );
        // free functions (no parent_fqn) do not enter the method index
        assert_eq!(idx.by_type_and_name.len(), 1);
    }

    #[test]
    fn normalize_base_type_strips_refs_generics_and_path() {
        assert_eq!(normalize_base_type("Widget"), "Widget");
        assert_eq!(normalize_base_type("&Widget"), "Widget");
        assert_eq!(normalize_base_type("&mut Widget"), "Widget");
        assert_eq!(normalize_base_type("Vec<Foo>"), "Vec");
        assert_eq!(normalize_base_type("&mut Vec<Foo>"), "Vec");
        assert_eq!(normalize_base_type("outer::Widget"), "Widget");
        assert_eq!(normalize_base_type("crate::a::b::Holder<T>"), "Holder");
        assert_eq!(normalize_base_type("Self"), "Self");
        assert_eq!(normalize_base_type("&'a Widget"), "Widget");
        assert_eq!(normalize_base_type("&'a mut Widget"), "Widget");
    }

    #[test]
    fn insert_method_normalizes_mod_nested_and_ref_keys() {
        // A mod-nested impl records parent_fqn "outer::Widget"; the call-receiver
        // side yields bare "Widget". insert_method must key under "Widget" so
        // both sides agree (closes the D1-core mod-nested gap).
        let make = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_method("outer::Widget".into(), "make".into(), make);
        assert_eq!(
            idx.by_type_and_name.get(&("Widget".into(), "make".into())),
            Some(&vec![make])
        );
        // A generic AND path-qualified type also strips to the bare base.
        let x = Uuid::new_v4();
        idx.insert_method("a::b::Holder<T>".into(), "x".into(), x);
        assert_eq!(
            idx.by_type_and_name.get(&("Holder".into(), "x".into())),
            Some(&vec![x])
        );
    }

    #[test]
    fn parse_return_type_simple() {
        assert_eq!(
            parse_return_type("fn f(&self) -> Widget", "Owner"),
            Some("Widget".to_string())
        );
    }

    #[test]
    fn parse_return_type_strips_reference() {
        assert_eq!(
            parse_return_type("fn f(&self) -> &Foo", "Owner"),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn parse_return_type_wrapper_normalizes_to_outer_base() {
        // Documented D1c-1 ceiling: Result<Widget> / Vec<T> normalize to their
        // OUTER base only; unwrapping to the inner T is deferred (D1c-1b).
        assert_eq!(
            parse_return_type("fn f(&self) -> Result<Widget>", "Owner"),
            Some("Result".to_string())
        );
        assert_eq!(
            parse_return_type("fn f(&self) -> Vec<T>", "Owner"),
            Some("Vec".to_string())
        );
    }

    #[test]
    fn parse_return_type_self_maps_to_owner() {
        assert_eq!(
            parse_return_type("fn f(self) -> Self", "Bar"),
            Some("Bar".to_string())
        );
    }

    #[test]
    fn parse_return_type_reference_to_self_maps_to_owner() {
        // normalize_base_type strips the reference BEFORE the Self check, so
        // &Self / &mut Self also map to owner_type, not the literal "Self".
        assert_eq!(
            parse_return_type("fn f(&self) -> &Self", "Bar"),
            Some("Bar".to_string())
        );
    }

    #[test]
    fn parse_return_type_no_arrow_returns_none() {
        assert_eq!(parse_return_type("fn f()", "Owner"), None);
        assert_eq!(parse_return_type("fn f(&self)", "Owner"), None);
    }

    #[test]
    fn parse_return_type_strips_lifetime_and_generics() {
        assert_eq!(
            parse_return_type("fn f<'a>(&'a self) -> &'a Widget", "Owner"),
            Some("Widget".to_string())
        );
    }

    #[test]
    fn parse_return_type_ignores_where_clause() {
        assert_eq!(
            parse_return_type("fn f<T>(&self) -> T where T: Default", "Owner"),
            Some("T".to_string())
        );
    }

    #[test]
    fn parse_return_type_stops_at_body_brace() {
        assert_eq!(
            parse_return_type("fn f(&self) -> Widget { todo!() }", "Owner"),
            Some("Widget".to_string())
        );
    }

    #[test]
    fn parse_return_type_first_depth0_arrow_over_fn_pointer_param() {
        // A fn-pointer PARAMETER's own `->` is inside the param list (bracket
        // depth >= 1), so it is skipped: the method's own return arrow is the
        // FIRST `-> ` at bracket depth 0, which comes after the param list's
        // closing paren.
        assert_eq!(
            parse_return_type("fn f(g: fn() -> i32) -> Widget", "Owner"),
            Some("Widget".to_string())
        );
    }

    #[test]
    fn parse_return_type_ignores_arrow_inside_return_type() {
        // The return type itself embeds `->` (a boxed closure). The method's
        // own arrow is the FIRST at depth 0 → Box, NOT the false-positive
        // Widget from the closure's inner arrow.
        assert_eq!(
            parse_return_type("fn make(&self) -> Box<dyn Fn() -> Widget>", "Owner"),
            Some("Box".to_string())
        );
    }

    #[test]
    fn parse_return_type_ignores_arrow_in_generic_bound() {
        // A trait-fn bound in the generic list has its own `->` (inside `<>`);
        // the method's `-> W` is the first at depth 0.
        assert_eq!(
            parse_return_type("fn f<T: Fn() -> U>(x: T) -> Widget", "Owner"),
            Some("Widget".to_string())
        );
    }

    #[test]
    fn insert_return_type_populates_by_return_type() {
        let mut idx = ChunkIndex::default();
        idx.insert_return_type("Foo", "x".into(), "Widget".into());
        assert_eq!(
            idx.by_return_type.get(&("Foo".into(), "x".into())),
            Some(&"Widget".to_string())
        );
    }

    #[test]
    fn insert_return_type_agreeing_duplicate_keeps_entry() {
        let mut idx = ChunkIndex::default();
        idx.insert_return_type("Foo", "x".into(), "Widget".into());
        idx.insert_return_type("Foo", "x".into(), "Widget".into());
        assert_eq!(
            idx.by_return_type.get(&("Foo".into(), "x".into())),
            Some(&"Widget".to_string())
        );
    }

    #[test]
    fn insert_return_type_conflict_drops_and_poisons() {
        let mut idx = ChunkIndex::default();
        idx.insert_return_type("Foo", "x".into(), "Widget".into());
        idx.insert_return_type("Foo", "x".into(), "Gadget".into());
        assert_eq!(idx.by_return_type.get(&("Foo".into(), "x".into())), None);
        // Poisoned: a THIRD insert agreeing with the FIRST value must NOT
        // resurrect the entry — a (type, method) once proven ambiguous stays
        // untrusted (fail-closed).
        idx.insert_return_type("Foo", "x".into(), "Widget".into());
        assert_eq!(idx.by_return_type.get(&("Foo".into(), "x".into())), None);
    }

    #[test]
    fn insert_return_type_normalizes_owner_type() {
        let mut idx = ChunkIndex::default();
        idx.insert_return_type("outer::Widget", "x".into(), "Gadget".into());
        assert_eq!(
            idx.by_return_type.get(&("Widget".into(), "x".into())),
            Some(&"Gadget".to_string())
        );
    }

    #[test]
    fn build_from_populates_by_return_type_from_signature() {
        let tick = Uuid::new_v4();
        let idx = ChunkIndex::build_from(vec![(
            tick,
            "tick".into(),
            "widget".into(),
            Some("Widget".into()),
            Some("fn tick(&self) -> Gadget".into()),
            "method".into(),
            String::new(),
        )]);
        assert_eq!(
            idx.by_return_type.get(&("Widget".into(), "tick".into())),
            Some(&"Gadget".to_string())
        );
    }

    #[test]
    fn build_from_no_signature_or_unit_return_not_indexed() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let idx = ChunkIndex::build_from(vec![
            (
                a,
                "no_sig".into(),
                "widget".into(),
                Some("Widget".into()),
                None,
                "method".into(),
                String::new(),
            ),
            (
                b,
                "unit_ret".into(),
                "widget".into(),
                Some("Widget".into()),
                Some("fn unit_ret(&self)".into()),
                "method".into(),
                String::new(),
            ),
        ]);
        assert!(
            !idx.by_return_type
                .contains_key(&("Widget".into(), "no_sig".into()))
        );
        assert!(
            !idx.by_return_type
                .contains_key(&("Widget".into(), "unit_ret".into()))
        );
        // by_type_and_name is UNAFFECTED — the two registries are independent.
        assert!(
            idx.by_type_and_name
                .contains_key(&("Widget".into(), "no_sig".into()))
        );
    }

    #[test]
    fn build_from_conflicting_return_types_drops_entry() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let idx = ChunkIndex::build_from(vec![
            (
                a,
                "x".into(),
                "m1".into(),
                Some("Foo".into()),
                Some("fn x(&self) -> Widget".into()),
                "method".into(),
                String::new(),
            ),
            (
                b,
                "x".into(),
                "m2".into(),
                Some("Foo".into()),
                Some("fn x(&self) -> Gadget".into()),
                "method".into(),
                String::new(),
            ),
        ]);
        assert_eq!(idx.by_return_type.get(&("Foo".into(), "x".into())), None);
    }

    #[test]
    fn method_chain_tier_resolves() {
        // by_return_type[("T","foo")] = "R"; by_type_and_name[("R","bar")] =
        // [id]. An edge targeting "bar" carrying chain_recv_type="T"/
        // chain_recv_method="foo" resolves to id at 0.75 "method_chain".
        let bar_on_r = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_return_type("T", "foo".into(), "R".into());
        idx.insert_method("R".into(), "bar".into(), bar_on_r);
        let r = resolve_call_edge(
            &call_edge_with_chain_recv("bar", "T", "foo"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, bar_on_r);
        assert_eq!(r.confidence, 0.75);
        assert_eq!(r.method, "method_chain");
    }

    #[test]
    fn method_chain_tier_ambiguous_second_hop_falls_through() {
        // by_return_type[("T","foo")] = "R", but (R,bar) has TWO defs ->
        // ambiguous second hop -> fall through.
        let mut idx = ChunkIndex::default();
        idx.insert_return_type("T", "foo".into(), "R".into());
        idx.insert_method("R".into(), "bar".into(), Uuid::new_v4());
        idx.insert_method("R".into(), "bar".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge_with_chain_recv("bar", "T", "foo"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn method_chain_tier_missing_return_type_falls_through() {
        // No by_return_type[("T","foo")] entry (foo's return type unknown/
        // unparsed) -> the chain can't seed its second hop -> fall through.
        let mut idx = ChunkIndex::default();
        idx.insert_method("R".into(), "bar".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge_with_chain_recv("bar", "T", "foo"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn method_chain_tier_yields_to_direct_receiver_type_tier0() {
        // Regression guard on tier ORDER: if an edge carried BOTH
        // edge.recv_type (tier-0) AND chain metadata, tier-0 must win
        // (checked first) — mirrors D1b's
        // tier0_path_qualified_stays_type_qualified_at_0_9.
        let direct = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_method("Direct".into(), "bar".into(), direct);
        idx.insert_return_type("T", "foo".into(), "R".into());
        idx.insert_method("R".into(), "bar".into(), Uuid::new_v4());
        let mut edge = call_edge_with_chain_recv("bar", "T", "foo");
        edge.metadata.fields.insert(
            "edge.recv_type".into(),
            crate::types::MetadataValue::String("Direct".into()),
        );
        let r = resolve_call_edge(
            &edge,
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, direct);
        assert_eq!(r.method, "type_qualified");
    }

    #[test]
    fn struct_fields_simple() {
        assert_eq!(
            parse_struct_fields("struct S { a: Foo, b: Bar }"),
            vec![
                ("a".to_string(), "Foo".to_string(), false),
                ("b".to_string(), "Bar".to_string(), false)
            ]
        );
    }

    #[test]
    fn struct_fields_strips_pub_and_reference() {
        assert_eq!(
            parse_struct_fields("struct S { pub a: &Foo }"),
            vec![("a".to_string(), "Foo".to_string(), false)]
        );
    }

    #[test]
    fn struct_fields_strips_pub_crate_visibility() {
        assert_eq!(
            parse_struct_fields("struct S { pub(crate) a: Foo }"),
            vec![("a".to_string(), "Foo".to_string(), false)]
        );
    }

    #[test]
    fn struct_fields_pub_prefix_does_not_corrupt_field_names_starting_with_pub() {
        // "public_key" merely STARTS WITH "pub" — must not be mistaken for a
        // `pub ` visibility modifier and truncated to "lic_key".
        assert_eq!(
            parse_struct_fields("struct S { public_key: Foo }"),
            vec![("public_key".to_string(), "Foo".to_string(), false)]
        );
    }

    #[test]
    fn struct_fields_nested_generic_comma_not_split() {
        // The comma inside HashMap<K, V> is NOT a field separator (bracket-
        // depth-aware split); the wrapper type normalizes to its OUTER base
        // (documented ceiling, same as parse_return_type's Vec<T>/Result<T>).
        assert_eq!(
            parse_struct_fields("struct S { m: HashMap<K, V> }"),
            vec![("m".to_string(), "HashMap".to_string(), false)]
        );
    }

    #[test]
    fn struct_fields_strips_lifetime() {
        assert_eq!(
            parse_struct_fields("struct S { r: &'a Widget }"),
            vec![("r".to_string(), "Widget".to_string(), false)]
        );
    }

    #[test]
    fn struct_fields_tuple_struct_is_empty() {
        assert_eq!(
            parse_struct_fields("struct T(u32, Foo);"),
            Vec::<(String, String, bool)>::new()
        );
    }

    #[test]
    fn struct_fields_unit_struct_is_empty() {
        assert_eq!(
            parse_struct_fields("struct U;"),
            Vec::<(String, String, bool)>::new()
        );
    }

    #[test]
    fn struct_fields_skips_doc_comment_and_attribute_lines() {
        let content =
            "struct S {\n    /// The engine field.\n    #[serde(default)]\n    a: Foo,\n}";
        assert_eq!(
            parse_struct_fields(content),
            vec![("a".to_string(), "Foo".to_string(), false)]
        );
    }

    #[test]
    fn struct_fields_trailing_comma_tolerated() {
        assert_eq!(
            parse_struct_fields("struct S { a: Foo, }"),
            vec![("a".to_string(), "Foo".to_string(), false)]
        );
    }

    #[test]
    fn struct_fields_multiple_fields_multiline() {
        let content = "pub struct Host {\n    pub engine: Engine,\n    pub name: String,\n}";
        assert_eq!(
            parse_struct_fields(content),
            vec![
                ("engine".to_string(), "Engine".to_string(), false),
                ("name".to_string(), "String".to_string(), false),
            ]
        );
    }

    #[test]
    fn struct_fields_arrow_in_field_type_does_not_swallow_later_fields() {
        // The `>` that is the tail of `->` inside a field type must NOT be
        // counted as a bracket close (mirrors parse_return_type). Regression
        // guard: a field whose type contains a function type used to drive the
        // top-level-comma depth counter negative, folding every following field
        // into one garbage segment (so `engine` vanished from by_field_type).
        assert_eq!(
            parse_struct_fields("struct S { cb: Option<fn() -> u32>, engine: Engine }"),
            vec![
                ("cb".to_string(), "Option".to_string(), false),
                ("engine".to_string(), "Engine".to_string(), false),
            ]
        );
    }

    #[test]
    fn struct_fields_bare_fn_pointer_does_not_swallow_later_fields() {
        // A top-level `fn(..) -> T` field: the `->` arrow must not desync the
        // comma splitter, so the field declared after it is still separated.
        let fields = parse_struct_fields("struct S { f: fn(u32) -> bool, x: X }");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1], ("x".to_string(), "X".to_string(), false));
    }

    #[test]
    fn insert_field_type_populates_by_field_type() {
        let mut idx = ChunkIndex::default();
        idx.insert_field_type("Host", "engine".into(), "Engine".into());
        assert_eq!(
            idx.by_field_type.get(&("Host".into(), "engine".into())),
            Some(&"Engine".to_string())
        );
    }

    #[test]
    fn insert_field_type_agreeing_duplicate_keeps_entry() {
        let mut idx = ChunkIndex::default();
        idx.insert_field_type("Host", "engine".into(), "Engine".into());
        idx.insert_field_type("Host", "engine".into(), "Engine".into());
        assert_eq!(
            idx.by_field_type.get(&("Host".into(), "engine".into())),
            Some(&"Engine".to_string())
        );
    }

    #[test]
    fn insert_field_type_conflict_drops_and_poisons() {
        let mut idx = ChunkIndex::default();
        idx.insert_field_type("Host", "engine".into(), "Engine".into());
        idx.insert_field_type("Host", "engine".into(), "OtherEngine".into());
        assert_eq!(
            idx.by_field_type.get(&("Host".into(), "engine".into())),
            None
        );
        // Poisoned: a THIRD insert agreeing with the FIRST value must NOT
        // resurrect the entry — fail-closed, same policy as by_return_type.
        idx.insert_field_type("Host", "engine".into(), "Engine".into());
        assert_eq!(
            idx.by_field_type.get(&("Host".into(), "engine".into())),
            None
        );
    }

    #[test]
    fn insert_field_type_normalizes_owner_type() {
        let mut idx = ChunkIndex::default();
        idx.insert_field_type("outer::Host", "engine".into(), "Engine".into());
        assert_eq!(
            idx.by_field_type.get(&("Host".into(), "engine".into())),
            Some(&"Engine".to_string())
        );
    }

    #[test]
    fn build_from_populates_by_field_type_from_struct_content() {
        let host_id = Uuid::new_v4();
        let idx = ChunkIndex::build_from(vec![(
            host_id,
            "Host".into(),
            "app".into(),
            None,
            None,
            "struct".into(),
            "struct Host { engine: Gadget }".into(),
        )]);
        assert_eq!(
            idx.by_field_type.get(&("Host".into(), "engine".into())),
            Some(&"Gadget".to_string())
        );
    }

    #[test]
    fn build_from_non_struct_chunk_not_parsed_for_fields() {
        // A non-struct chunk whose content happens to look like a struct
        // body must NOT populate by_field_type — only chunk_type == "struct"
        // rows are parsed.
        let idx = ChunkIndex::build_from(vec![(
            Uuid::new_v4(),
            "not_a_struct".into(),
            "app".into(),
            None,
            None,
            "function".into(),
            "struct Host { engine: Gadget }".into(),
        )]);
        assert!(
            !idx.by_field_type
                .contains_key(&("Host".into(), "engine".into()))
        );
    }

    #[test]
    fn build_from_conflicting_field_types_drops_entry() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let idx = ChunkIndex::build_from(vec![
            (
                a,
                "Host".into(),
                "m1".into(),
                None,
                None,
                "struct".into(),
                "struct Host { engine: Gadget }".into(),
            ),
            (
                b,
                "Host".into(),
                "m2".into(),
                None,
                None,
                "struct".into(),
                "struct Host { engine: OtherGadget }".into(),
            ),
        ]);
        assert_eq!(
            idx.by_field_type.get(&("Host".into(), "engine".into())),
            None
        );
    }

    #[test]
    fn field_type_tier_resolves() {
        // by_field_type[("T","f")] = "FT"; by_type_and_name[("FT","m")] =
        // [id]. An edge targeting "m" carrying field_recv_type="T"/
        // field_name="f" resolves to id at 0.75 "field_type".
        let m_on_ft = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_field_type("T", "f".into(), "FT".into());
        idx.insert_method("FT".into(), "m".into(), m_on_ft);
        let r = resolve_call_edge(
            &call_edge_with_field_recv("m", "T", "f"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, m_on_ft);
        assert_eq!(r.confidence, 0.75);
        assert_eq!(r.method, "field_type");
    }

    #[test]
    fn field_type_tier_ambiguous_second_hop_falls_through() {
        // by_field_type[("T","f")] = "FT", but (FT,m) has TWO defs ->
        // ambiguous second hop -> fall through.
        let mut idx = ChunkIndex::default();
        idx.insert_field_type("T", "f".into(), "FT".into());
        idx.insert_method("FT".into(), "m".into(), Uuid::new_v4());
        idx.insert_method("FT".into(), "m".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge_with_field_recv("m", "T", "f"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn field_type_tier_missing_field_type_falls_through() {
        // No by_field_type[("T","f")] entry (field's type unknown/unparsed)
        // -> the field lookup can't seed its second hop -> fall through.
        let mut idx = ChunkIndex::default();
        idx.insert_method("FT".into(), "m".into(), Uuid::new_v4());
        assert!(
            resolve_call_edge(
                &call_edge_with_field_recv("m", "T", "f"),
                Uuid::new_v4(),
                "app",
                &idx,
                &ImportMap::default(),
                &no_imports(),
            )
            .is_none()
        );
    }

    #[test]
    fn field_type_tier_yields_to_direct_receiver_type_tier0() {
        // Regression guard on tier ORDER: if an edge carried BOTH
        // edge.recv_type (tier-0) AND field metadata, tier-0 must win
        // (checked first) — mirrors
        // method_chain_tier_yields_to_direct_receiver_type_tier0.
        let direct = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_method("Direct".into(), "m".into(), direct);
        idx.insert_field_type("T", "f".into(), "FT".into());
        idx.insert_method("FT".into(), "m".into(), Uuid::new_v4());
        let mut edge = call_edge_with_field_recv("m", "T", "f");
        edge.metadata.fields.insert(
            "edge.recv_type".into(),
            crate::types::MetadataValue::String("Direct".into()),
        );
        let r = resolve_call_edge(
            &edge,
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, direct);
        assert_eq!(r.method, "type_qualified");
    }

    #[test]
    fn field_type_tier_yields_to_method_chain_tier() {
        // Regression / insertion-order guard: if an edge somehow carried
        // BOTH D1c-1 chain AND D1c-2 field metadata, the method_chain tier
        // (checked FIRST, per the resolved insertion point) must win.
        let chain_target = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_return_type("T", "foo".into(), "R".into());
        idx.insert_method("R".into(), "m".into(), chain_target);
        idx.insert_field_type("T", "f".into(), "FT".into());
        idx.insert_method("FT".into(), "m".into(), Uuid::new_v4());
        let mut edge = call_edge_with_field_recv("m", "T", "f");
        edge.metadata.fields.insert(
            "edge.chain_recv_type".into(),
            crate::types::MetadataValue::String("T".into()),
        );
        edge.metadata.fields.insert(
            "edge.chain_recv_method".into(),
            crate::types::MetadataValue::String("foo".into()),
        );
        let r = resolve_call_edge(
            &edge,
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, chain_target);
        assert_eq!(r.method, "method_chain");
    }

    #[test]
    fn field_type_tier_trait_field_resolves_trait_default() {
        // by_field_type[("Host","greeter")] = "Greeter" AND field_type_is_trait
        // contains ("Host","greeter"); by_type_and_name[("Greeter","hello")]
        // unique → 0.5 "trait_default".
        let hello = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_field_type("Host", "greeter".into(), "Greeter".into());
        idx.field_type_is_trait
            .insert(("Host".to_string(), "greeter".to_string()));
        idx.insert_method("Greeter".into(), "hello".into(), hello);
        let r = resolve_call_edge(
            &call_edge_with_field_recv("hello", "Host", "greeter"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.tgt_chunk_id, hello);
        assert_eq!(r.confidence, 0.5);
        assert_eq!(r.method, "trait_default");
    }

    #[test]
    fn field_type_tier_concrete_field_stays_field_type() {
        // Regression guard: the SAME 2-hop hit WITHOUT a field_type_is_trait
        // entry still resolves 0.75 "field_type".
        let m = Uuid::new_v4();
        let mut idx = ChunkIndex::default();
        idx.insert_field_type("T", "f".into(), "FT".into());
        idx.insert_method("FT".into(), "m".into(), m);
        let r = resolve_call_edge(
            &call_edge_with_field_recv("m", "T", "f"),
            Uuid::new_v4(),
            "app",
            &idx,
            &ImportMap::default(),
            &no_imports(),
        )
        .unwrap();
        assert_eq!(r.confidence, 0.75);
        assert_eq!(r.method, "field_type");
    }

    #[test]
    fn resolve_dyn_trait_name_bare_dyn() {
        assert_eq!(
            resolve_dyn_trait_name("dyn Trait"),
            Some("Trait".to_string())
        );
    }

    #[test]
    fn resolve_dyn_trait_name_multi_trait_takes_first() {
        assert_eq!(
            resolve_dyn_trait_name("dyn Trait + Send"),
            Some("Trait".to_string())
        );
    }

    #[test]
    fn resolve_dyn_trait_name_smart_pointer_wrappers() {
        assert_eq!(
            resolve_dyn_trait_name("Box<dyn Trait>"),
            Some("Trait".to_string())
        );
        assert_eq!(
            resolve_dyn_trait_name("Arc<dyn Trait>"),
            Some("Trait".to_string())
        );
        assert_eq!(
            resolve_dyn_trait_name("Rc<dyn Trait>"),
            Some("Trait".to_string())
        );
    }

    #[test]
    fn resolve_dyn_trait_name_wrapped_multi_trait_takes_first() {
        assert_eq!(
            resolve_dyn_trait_name("Arc<dyn Trait + Send + Sync>"),
            Some("Trait".to_string())
        );
    }

    #[test]
    fn resolve_dyn_trait_name_strips_reference_and_lifetime() {
        assert_eq!(
            resolve_dyn_trait_name("&dyn Trait"),
            Some("Trait".to_string())
        );
        assert_eq!(
            resolve_dyn_trait_name("&'a dyn Trait"),
            Some("Trait".to_string())
        );
    }

    #[test]
    fn resolve_dyn_trait_name_path_qualified_dyn_normalizes() {
        // `dyn a::b::Trait` → the trailing segment, via normalize_base_type.
        assert_eq!(
            resolve_dyn_trait_name("dyn a::b::Trait"),
            Some("Trait".to_string())
        );
    }

    #[test]
    fn resolve_dyn_trait_name_concrete_types_are_none() {
        assert_eq!(resolve_dyn_trait_name("Widget"), None);
        assert_eq!(resolve_dyn_trait_name("HashMap<K,V>"), None);
        assert_eq!(resolve_dyn_trait_name("Box<Widget>"), None);
    }

    #[test]
    fn struct_fields_dyn_trait_field_stores_trait_name_and_flags_it() {
        // A `dyn`/smart-pointer-wrapped trait field stores the bare TRAIT name
        // (via resolve_dyn_trait_name) and is flagged is_trait=true; a concrete
        // field stores its normalized base and is_trait=false.
        assert_eq!(
            parse_struct_fields("struct S { e: Arc<dyn Greeter>, n: Widget }"),
            vec![
                ("e".to_string(), "Greeter".to_string(), true),
                ("n".to_string(), "Widget".to_string(), false),
            ]
        );
    }

    #[test]
    fn build_from_populates_field_type_is_trait_for_dyn_field() {
        let host = Uuid::new_v4();
        let idx = ChunkIndex::build_from(vec![(
            host,
            "GreeterHost".into(),
            "app".into(),
            None,
            None,
            "struct".into(),
            "struct GreeterHost { greeter: Arc<dyn Greeter> }".into(),
        )]);
        // The field's stored type is the TRAIT name (the 2-hop lookup key)...
        assert_eq!(
            idx.by_field_type
                .get(&("GreeterHost".into(), "greeter".into())),
            Some(&"Greeter".to_string())
        );
        // ...and the companion set records it came from a dyn-trait hit.
        assert!(
            idx.field_type_is_trait
                .contains(&("GreeterHost".to_string(), "greeter".to_string()))
        );
    }

    #[test]
    fn build_from_concrete_field_not_in_field_type_is_trait() {
        let host = Uuid::new_v4();
        let idx = ChunkIndex::build_from(vec![(
            host,
            "Host".into(),
            "app".into(),
            None,
            None,
            "struct".into(),
            "struct Host { engine: Engine }".into(),
        )]);
        assert_eq!(
            idx.by_field_type.get(&("Host".into(), "engine".into())),
            Some(&"Engine".to_string())
        );
        assert!(
            !idx.field_type_is_trait
                .contains(&("Host".to_string(), "engine".to_string()))
        );
    }
}
