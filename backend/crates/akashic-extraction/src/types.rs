// backend/src/ingestion/extraction/types.rs
//! Core type definitions for the declarative extraction layer.
//!
//! [`LanguageConfig`] drives the generic walker; [`RawChunk`] and [`RawEdge`]
//! are its output; [`ExtractionError`] covers all failure modes.
use std::collections::BTreeMap;
use std::sync::OnceLock;
use tree_sitter::{Language, Node, Query};

// ─────────────────────────────────────────────────────────────
// Dispatch enum: every supported file format is one of these
// ─────────────────────────────────────────────────────────────

pub enum ExtractorKind {
    TreeSitter(&'static LanguageConfig),
    Custom(&'static dyn SpecialExtractor),
}

// ─────────────────────────────────────────────────────────────
// LanguageConfig — declarative per-language extraction config
// ─────────────────────────────────────────────────────────────

pub struct LanguageConfig {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub language_fn: fn() -> Language,

    /// Declarative AST node-kind registry: the tree-sitter node `kind()`
    /// strings the walker matches to classify each construct (functions,
    /// classes, imports, calls, comments, etc.). Empty slices disable a
    /// category for the language.
    pub function_kinds: &'static [&'static str],
    pub class_kinds: &'static [&'static str],
    pub method_kinds: &'static [&'static str],
    pub interface_kinds: &'static [&'static str],
    pub struct_kinds: &'static [&'static str],
    pub enum_kinds: &'static [&'static str],
    pub enum_member_kinds: &'static [&'static str],
    pub type_alias_kinds: &'static [&'static str],
    pub variable_kinds: &'static [&'static str],
    pub constant_kinds: &'static [&'static str],
    pub macro_kinds: &'static [&'static str],
    pub namespace_kinds: &'static [&'static str],
    pub module_kinds: &'static [&'static str],
    pub trait_kinds: &'static [&'static str],
    pub impl_kinds: &'static [&'static str],

    pub import_kinds: &'static [&'static str],
    pub call_kinds: &'static [&'static str],
    pub member_expr_kinds: &'static [&'static str],
    /// Tree-sitter node kinds that denote a TYPE reference (e.g. Rust
    /// `type_identifier`). EXT-6b: the walker's type_pass emits a
    /// `References{ref_kind:type}` edge per such node inside a chunk. Empty =
    /// the language contributes no type references.
    pub type_ref_kinds: &'static [&'static str],

    pub comment_kinds: &'static [&'static str],
    pub doc_comment_kinds: &'static [&'static str],

    /// Tree-sitter field names the walker reads via `child_by_field_name`
    /// to pull out the named child of a matched node (e.g. the identifier,
    /// body block, parameter list, or import source).
    pub name_field: &'static str,
    /// Reserved: declared for future signature/nested extraction (EXT-5+); not yet read by the walker.
    pub body_field: &'static str,
    /// Reserved: declared for future signature/nested extraction (EXT-5+); not yet read by the walker.
    pub params_field: &'static str,
    /// Reserved: declared for future signature/nested extraction (EXT-5+); not yet read by the walker.
    pub return_field: &'static str,
    pub import_source_field: &'static str,
    pub call_function_field: &'static str,
    pub member_property_field: &'static str,
    /// Reserved: declared for future signature/nested extraction (EXT-5+); not yet read by the walker.
    pub receiver_field: &'static str,

    pub queries: &'static LanguageQueries,
    pub hooks: LanguageHooks,
}

// ─────────────────────────────────────────────────────────────
// Lazy-compiled tree-sitter Query escape hatch
// ─────────────────────────────────────────────────────────────

pub struct LanguageQueries {
    pub callbacks: &'static [QueryDef],
    pub framework_routes: &'static [QueryDef],
    /// C1: client-side HTTP-call capture queries (Rust reqwest, literal path).
    /// Empty for languages without client HTTP-call extraction.
    pub http_calls: &'static [QueryDef],
    pub extra_chunks: &'static [QueryDef],
}

pub struct QueryDef {
    pub name: &'static str,
    pub source: &'static str,
    pub compiled: OnceLock<Query>,
}

// ─────────────────────────────────────────────────────────────
// Hook signatures (all Option, default to walker's built-in path)
// ─────────────────────────────────────────────────────────────

pub struct LanguageHooks {
    pub resolve_name: Option<NameResolver>,
    pub resolve_fqn: Option<FqnResolver>,
    pub get_signature: Option<SignatureExtractor>,
    /// Reserved for EXT-5 (nested extraction); not yet read by the walker.
    pub resolve_body: Option<BodyResolver>,
    /// Reserved for EXT-5 (nested extraction); not yet read by the walker.
    pub resolve_params: Option<ParamsResolver>,

    pub should_recurse_into: Option<RecurseGuard>,
    /// Reserved for EXT-5 (nested extraction); not yet read by the walker.
    pub resolve_parent_scope: Option<ScopeResolver>,

    pub is_exported: Option<NodePredicate>,
    pub is_async: Option<NodePredicate>,
    pub is_static: Option<NodePredicate>,
    pub is_const: Option<NodePredicate>,
    pub get_visibility: Option<VisibilityExtractor>,

    pub resolve_method_receiver: Option<ReceiverResolver>,

    pub resolve_import_specifier: Option<SpecifierResolver>,
    pub resolve_import_names: Option<ImportNamesResolver>,
    pub resolve_call_name: Option<CallNameResolver>,
    pub resolve_call_receiver: Option<CallReceiverResolver>,
    pub build_type_env: Option<TypeEnvResolver>,
    pub resolve_call_receiver_expr: Option<CallReceiverResolver>,
    /// D1c-1: front half of 2-hop method-chain resolution. For the OUTER
    /// call node of `<inner_recv>.foo().bar()`, returns `(inner_recv's
    /// type, "foo")` when the inner receiver is a simple typed token/self
    /// found in `type_env`; `None` otherwise (fail-closed — the edge falls
    /// through to the existing cascade unchanged).
    pub resolve_call_receiver_chain: Option<ChainReceiverResolver>,
    /// D1c-2: field-access receiver. For the OUTER call node of
    /// `<inner_recv>.field.method()`, returns `(inner_recv's type, "field")`
    /// when the inner receiver is a simple typed token/self found in
    /// `type_env` AND the field is a NAMED field (not a tuple-index access
    /// like `self.0`); `None` otherwise (fail-closed). Reuses
    /// `ChainReceiverResolver`'s signature verbatim (same shape: `Node`,
    /// `&[u8]`, `&type_env` -> `Option<(String, String)>`) — a distinct type
    /// alias would be structurally identical, so this hook field is typed
    /// `Option<ChainReceiverResolver>` rather than introducing a redundant
    /// `FieldReceiverResolver` alias.
    pub resolve_call_receiver_field: Option<ChainReceiverResolver>,
    pub should_skip_call: Option<NodePredicate>,
    pub resolve_module_path: Option<ModulePathResolver>,

    /// Custom type-name extraction for a `type_ref_kinds` node. Default `None`
    /// → the walker reads the node's text (trailing path segment, generics
    /// stripped).
    pub resolve_type_name: Option<TypeNameResolver>,

    /// Position-aware type-reference hook. When `Some`, `type_pass` calls this
    /// instead of the `type_ref_kinds` node-scan. The hook walks the chunk's
    /// subtree and returns `(name, start_byte, end_byte)` per type reference
    /// found in TYPE positions (params, return types, field/property types,
    /// base classes, generic args). Use for languages where type names are bare
    /// `identifier` nodes that are syntactically indistinguishable from value
    /// identifiers — C# is the canonical case. EXT-6b-3.
    pub resolve_type_refs: Option<TypeRefsResolver>,

    /// EXT-6b-4: expand a `type_ref_kinds` node into MULTIPLE type references.
    /// When `Some`, the walker's `type_pass` calls this on each matched
    /// `type_ref_kinds` node FIRST; a non-empty `Vec` of `(name, start, end)`
    /// REPLACES the single-name `resolve_type_name`/`default_type_name` emission
    /// for that node. An empty `Vec` means "not a multi-ref node" → fall back to
    /// the single-name path. Required for PEP 604 unions (`A | B`, `X | None`),
    /// where the outer annotation node's text (`"A | B"`) is a garbage composite
    /// name and the real operand types are bare `identifier`s the recursion never
    /// reaches. Default `None` → single-name path only (unchanged for all langs
    /// that don't opt in).
    pub expand_type_ref: Option<TypeRefsResolver>,

    /// EXT-5: when true, the walker descends into function/method bodies to chunk
    /// NAMED nested functions as their own chunks (parent_fqn = enclosing
    /// function), pushes a Function scope frame, and bounds each chunk's call-pass
    /// at nested function/method-def boundaries so calls attribute to the
    /// innermost enclosing function. Default false = the top-level-only behavior
    /// (unchanged for languages that don't opt in).
    pub nested_extraction: bool,

    /// Refine the chunk category AFTER node-kind classification. Some grammars
    /// overload ONE node-kind for several constructs (e.g. tree-sitter-swift's
    /// `class_declaration` covers class/struct/enum; kotlin-ng's covers
    /// class/interface/enum-class), so classification by node-kind string alone
    /// lands them all in the same category. This hook inspects the node's child
    /// tokens to split them into the correct [`ChunkCategory`]. It only affects
    /// the emitted `chunk_type`; scope-frame pushing keys off the node-kind, not
    /// the category, so FQN nesting is unaffected.
    pub refine_category: Option<CategoryRefiner>,

    /// EXT-6c: resolve a chunk node's explicit supertypes as `(name, EdgeKind)`
    /// pairs. `EdgeKind::Implements` = interface/trait conformance;
    /// `EdgeKind::Extends` = class inheritance. The walker calls this in
    /// `supertype_pass` immediately after `type_pass` for each emitted chunk.
    /// `None` (the `DEFAULT`) means the language contributes no supertype edges.
    pub resolve_supertypes: Option<SupertypeResolver>,
}

impl LanguageHooks {
    pub const DEFAULT: LanguageHooks = LanguageHooks {
        resolve_name: None,
        resolve_fqn: None,
        get_signature: None,
        resolve_body: None,
        resolve_params: None,
        should_recurse_into: None,
        resolve_parent_scope: None,
        is_exported: None,
        is_async: None,
        is_static: None,
        is_const: None,
        get_visibility: None,
        resolve_method_receiver: None,
        resolve_import_specifier: None,
        resolve_import_names: None,
        resolve_call_name: None,
        resolve_call_receiver: None,
        build_type_env: None,
        resolve_call_receiver_expr: None,
        resolve_call_receiver_chain: None,
        resolve_call_receiver_field: None,
        should_skip_call: None,
        resolve_module_path: None,
        resolve_type_name: None,
        resolve_type_refs: None,
        expand_type_ref: None,
        nested_extraction: false,
        refine_category: None,
        resolve_supertypes: None,
    };
}

/// The category a chunk is classified as, derived from the AST node-kind (and
/// optionally refined by [`LanguageHooks::refine_category`]). `as_str` is the
/// `chunk_type` string persisted on the [`RawChunk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkCategory {
    Function,
    Class,
    Method,
    Interface,
    Struct,
    Enum,
    /// A single member/variant/case/constant of an enum (e.g. Rust
    /// `enum_variant`, Java `enum_constant`, C `enumerator`). Emitted by the
    /// walker's dedicated enum-member pass when `enum_member_kinds` is set.
    EnumMember,
    Trait,
    TypeAlias,
    Variable,
    Constant,
    Macro,
    Namespace,
    Module,
}

impl ChunkCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Method => "method",
            Self::Interface => "interface",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::EnumMember => "enum_member",
            Self::Trait => "trait",
            Self::TypeAlias => "type",
            Self::Variable => "variable",
            Self::Constant => "constant",
            Self::Macro => "macro",
            Self::Namespace => "namespace",
            Self::Module => "module",
        }
    }
}

pub type NameResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<String>;
pub type FqnResolver = for<'a> fn(Node<'a>, &[u8], &ScopeStack) -> Option<String>;
pub type SignatureExtractor = for<'a> fn(Node<'a>, &[u8]) -> Option<String>;
pub type BodyResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<Node<'a>>;
pub type ParamsResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<Node<'a>>;
pub type RecurseGuard = for<'a> fn(Node<'a>) -> bool;
pub type ScopeResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<ScopeFrame>;
pub type NodePredicate = for<'a> fn(Node<'a>) -> bool;
pub type VisibilityExtractor = for<'a> fn(Node<'a>, &[u8]) -> Visibility;
pub type ReceiverResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<MethodReceiver>;
pub type SpecifierResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<String>;
/// Per-symbol import-binding resolver: returns `(local, source, spec)` tuples.
/// `local` is the binding a call site references; `source` is the symbol's name
/// in the defining module (differs from `local` only for aliased imports, e.g.
/// `import { bar as baz }` → `("baz", "bar", Named)`). The walker emits one
/// Import edge per tuple with `target.name = local`, recording `source` in
/// metadata only when it differs, so tier-1 resolution can bridge aliases.
pub type ImportNamesResolver = for<'a> fn(Node<'a>, &[u8]) -> Vec<(String, String, ImportSpec)>;
pub type CallNameResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<String>;
/// Resolve the receiver TYPE name of a path-qualified call (`Foo::bar()` → `"Foo"`).
/// Returns `None` for bare calls and value method calls. D1: powers type-qualified
/// call resolution.
pub type CallReceiverResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<String>;
/// Build a callable chunk's intra-chunk type environment: a map from a value
/// binding name to its bare base type. Populated from `let` annotations,
/// constructor / struct-literal RHS, and typed parameters (the walker adds
/// `self` separately from the scope stack). D1b: powers value-method call
/// resolution. Values are normalized via `normalize_base_type`.
///
/// D3: also returns the set of binding names whose declared type is a trait
/// object (`dyn Trait`/`Box<dyn Trait>`/…) or a generic parameter bound to a
/// trait — the type stored in the map is the TRAIT name, and the binding is
/// additionally recorded here so `extract_call_edge` can flag the resulting
/// edge `recv_is_trait_bound` (→ the resolver's `trait_default` branch).
pub type TypeEnvResolver = for<'a> fn(
    Node<'a>,
    &[u8],
) -> (
    std::collections::HashMap<String, String>,
    std::collections::HashSet<String>,
);
/// D1c-1: front half of 2-hop method-chain resolution — see
/// `LanguageHooks::resolve_call_receiver_chain`.
pub type ChainReceiverResolver = for<'a> fn(
    Node<'a>,
    &[u8],
    &std::collections::HashMap<String, String>,
) -> Option<(String, String)>;
/// Resolve the referenced type's short name from a type-reference node
/// (e.g. a `type_identifier`). Returns `None` to skip the node.
pub type TypeNameResolver = for<'a> fn(Node<'a>, &[u8]) -> Option<String>;
/// Position-aware type-reference resolver for languages where type names are
/// bare `identifier` nodes (indistinguishable from value identifiers by kind
/// alone). Returns `(type_name, start_byte, end_byte)` per type reference
/// found in the chunk's subtree; the walker emits one `References{ref_kind:type}`
/// edge per entry. EXT-6b-3.
pub type TypeRefsResolver = for<'a> fn(Node<'a>, &[u8]) -> Vec<(String, usize, usize)>;
pub type ModulePathResolver = fn(&str, &str) -> Option<String>;
pub type CategoryRefiner = for<'a> fn(Node<'a>, &[u8], ChunkCategory) -> ChunkCategory;
/// Resolve a chunk's explicit supertypes: (supertype name, edge kind) where kind
/// is `EdgeKind::Implements` (interface/trait conformance) or `EdgeKind::Extends`
/// (class inheritance). EXT-6c.
pub type SupertypeResolver = for<'a> fn(Node<'a>, &[u8]) -> Vec<(String, EdgeKind)>;

// ─────────────────────────────────────────────────────────────
// RawChunk — the unit chunks produced by extraction
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawChunk {
    pub chunk_type: String,
    pub name: String,
    pub fqn: Option<String>,
    pub parent_fqn: Option<String>,

    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,

    pub signature: Option<String>,
    pub content: String,
    pub doc: Option<String>,

    pub receiver: Option<MethodReceiver>,

    pub is_async: bool,
    pub is_static: bool,
    pub is_const: bool,
    pub is_exported: bool,
    pub visibility: Visibility,

    pub metadata: ChunkMetadata,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChunkMetadata {
    pub fields: BTreeMap<String, MetadataValue>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MetadataValue {
    String(String),
    Number(f64),
    Bool(bool),
    StringList(Vec<String>),
    Map(BTreeMap<String, MetadataValue>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
    Crate,
    PackagePrivate,
    Unknown,
}

impl Visibility {
    /// Return the lowercase string representation for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
            Visibility::Protected => "protected",
            Visibility::Internal => "internal",
            Visibility::Crate => "crate",
            Visibility::PackagePrivate => "package_private",
            Visibility::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MethodReceiver {
    pub type_name: String,
    pub is_pointer: bool,
    pub is_mutable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeFrame {
    pub kind: ScopeKind,
    pub name: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Module,
    Namespace,
    Class,
    Struct,
    Impl,
    Trait,
    Interface,
    Enum,
    Function,
}

pub type ScopeStack = Vec<ScopeFrame>;

// ─────────────────────────────────────────────────────────────
// RawEdge — unified edge representation (call, import, DSL, ...)
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawEdge {
    pub source: EdgeEndpoint,
    pub target: EdgeEndpoint,
    pub kind: EdgeKind,
    pub provenance: Provenance,
    pub line: Option<usize>,
    pub metadata: EdgeMetadata,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EdgeEndpoint {
    ByteRange {
        start: usize,
        end: usize,
    },
    Name {
        name: String,
        module_specifier: Option<String>,
    },
    Resolved(uuid::Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EdgeKind {
    Call,
    Import,
    Extends,
    Implements,
    References,
    Instantiates,
    Overrides,
    Decorates,
    Includes,
    DependsOn,
    RoutesTo,
    UsesImage,
    Triggers,
    PartOfStage,
    BindsTo,
    Contains,
    ExplainedBy,
    /// C1: a client-side HTTP call (source function → `http_call` chunk).
    MakesHttpCall,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Provenance {
    Static,
    Heuristic { synthesized_by: String },
    Manual,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EdgeMetadata {
    pub fields: BTreeMap<String, MetadataValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImportSpec {
    Default,
    Named,
    Namespace,
    TypeOnly,
    ReExport,
    Glob,
}

// ─────────────────────────────────────────────────────────────
// SpecialExtractor — for non-tree-sitter formats
// ─────────────────────────────────────────────────────────────

pub trait SpecialExtractor: Send + Sync {
    fn name(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn filename_matches(&self) -> &'static [&'static str] {
        &[]
    }
    /// Basename suffixes this extractor handles (e.g. `".routing.yml"`).
    ///
    /// Unlike `extensions()`, which matches only the final dot-extension, suffix
    /// dispatch matches the entire trailing portion of the basename — allowing
    /// compound extensions like `.routing.yml` to be claimed by a specific
    /// extractor without hijacking all `.yml` files. Checked AFTER
    /// `filename_matches` and extension dispatch in `registry::lookup_for_path`.
    fn path_suffix_matches(&self) -> &'static [&'static str] {
        &[]
    }
    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError>;
}

// ─────────────────────────────────────────────────────────────
// ExtractionOutput — what every extractor returns
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExtractionOutput {
    pub chunks: Vec<RawChunk>,
    pub edges: Vec<RawEdge>,
}

// ─────────────────────────────────────────────────────────────
// Error model
// ─────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    #[error("tree-sitter set_language failed for {lang}: {source}")]
    LanguageInit {
        lang: &'static str,
        #[source]
        source: tree_sitter::LanguageError,
    },

    #[error("tree-sitter parse returned None for {path}")]
    ParseEmpty { path: String },

    #[error("query compilation failed for {name} ({lang}): {source}")]
    QueryCompile {
        name: &'static str,
        lang: &'static str,
        #[source]
        source: tree_sitter::QueryError,
    },

    #[error("invalid utf-8 in source at byte {byte}")]
    InvalidUtf8 { byte: usize },

    #[error("invalid LanguageConfig: {detail}")]
    BadConfig { detail: String },
}
