/**
 * Shell data-layer types — verbatim subset of legacy types.ts.
 * Includes shell types (Repository, ActiveJob, SearchKnowledgeResult)
 * and graph-engine types (ChunkNode, CallEdge, GhostNode, DocumentSectionItem)
 * added in SP3.
 */

// ── Graph engine types (SP3) ───────────────────────────────────

/** Chunk node for call-graph layout (Layer 3 drill-down). */
export interface ChunkNode {
	id: string; // "chunk:<uuid>"
	name: string;
	chunk_type: string; // "function" | "class" | "struct" | ...
	signature?: string;
	line_count: number;
}

export interface CallEdge {
	source: string; // chunk id
	target: string; // chunk id
	confidence: number; // 0.0–1.0
	method: string; // "import_resolved" | "same_file" | ...
}

export interface GhostNode {
	id: string; // "chunk:<uuid>"
	name: string;
	chunk_type: string;
	module_path: string;
	module_id: string; // "mod:<pg_id>" — for navigation
	direction: "caller" | "callee";
}

export interface ExplainsTarget {
	chunk_id: string;
	chunk_name: string;
	chunk_type: string;
	repo_name: string;
	module_path: string;
	confidence: number;
	method: string;
}

export interface DocumentSectionItem {
	id: string;
	parent_id: string | null;
	heading: string;
	depth: number;
	content: string;
	tags: string[] | null;
	explains: ExplainsTarget[];
}

// ── Shell types ────────────────────────────────────────────────

export interface Repository {
	id: string;
	name: string;
	last_synced_at: string | null;
	source_type: "gitlab" | "website" | null;
}

export interface ActiveJob {
	repo_name: string;
	status: string;
	processed_files: number | null;
	total_files: number | null;
}

// v3 Scout search result (compact)
export interface SearchKnowledgeResult {
	id: string;
	layer: "MAP" | "NOTE" | "DOC";
	type: string; // chunk_type or category
	name: string; // chunk name or note title
	repo_name: string; // which repo this result belongs to
	module?: string; // module_path for MAP
	summary?: string; // summary for NOTE
	score: number;
	docs?: {
		repo: string;
		/** Full corpus key (documents.source_url). */
		path: string;
		anchor: string;
	};
}

// ── SP4a: graph/module/repo-detail types (ported from legacy types.ts) ────

export interface GraphNode {
	id: string;
	label: string;
	type: "Repository" | "Branch" | "Tag" | "Note" | "Category" | "Module" | "Chunk" | "Saga";
}

export interface GraphEdge {
	source: string;
	target: string;
	type: string;
}

export interface GraphData {
	nodes: GraphNode[];
	edges: GraphEdge[];
}

export type Category = "ARCHITECTURE" | "BUG_FIX" | "CONFIG" | "ONBOARDING" | "DECISION";

export interface Branch {
	name: string;
	last_commit_hash: string | null;
}

export interface IngestionJob {
	id: string;
	repo_name: string;
	git_ref: string;
	status: string;
	total_files: number;
	processed_files: number;
	total_chunks: number;
	error_message: string | null;
	started_at: string;
	completed_at: string | null;
}

export interface RepoDetail {
	name: string;
	source_type: string | null;
	seed_url: string | null;
	crawl_depth: number | null;
	url_pattern: string | null;
	git_url: string | null;
	total_chunks: number;
	total_modules: number;
	total_documents: number;
	total_sections: number;
	latest_job: IngestionJob | null;
}

// --- Module Graph (Layer 1) ---

export interface ModuleNode {
	id: string;
	label: string;
	path: string;
	chunk_count: number;
	note_count: number;
	is_virtual: boolean;
	language: string;
}

export interface ImportEdge {
	source: string;
	target: string;
}

export interface SagaGroup {
	saga_id: string;
	name: string;
	status: string;
	module_ids: string[];
}

export interface ModuleGraphData {
	nodes: ModuleNode[];
	edges: ImportEdge[];
	calls_count: number;
	saga_groups?: SagaGroup[];
}

// --- Module Detail / Chunks (Layer 2) ---

export interface ChunkNoteItem {
	id: string;
	title: string;
	category: string;
}

export interface ChunkItem {
	id: string;
	name: string;
	chunk_type: string;
	signature: string | null;
	content: string;
	language: string;
	notes: ChunkNoteItem[];
}

export interface NoteItem {
	id: string;
	title: string;
	category: string;
	summary: string;
}

export interface ModuleDetailData {
	module: { id: string; path: string; name: string; chunk_count: number; note_count: number };
	chunks: ChunkItem[];
	notes: NoteItem[];
}

// --- Chunk Call Graph (Layer 3 — drill-down) ---
// ChunkNode, CallEdge, GhostNode already present above (SP3).

export interface ChunkCallGraph {
	module: { id: string; path: string; name: string };
	chunks: ChunkNode[];
	calls: CallEdge[];
	external: GhostNode[];
}

// --- Doc Graph (website repos) ---

export interface DocGraphNode {
	id: string;
	label: string;
	path: string;
	section_count: number;
	explains_count: number;
	is_virtual: boolean;
	language: string;
}

export interface DocGraphData {
	nodes: DocGraphNode[];
	edges: ImportEdge[];
	explains_summary: Record<string, number>;
}

// --- Document Detail (section drill-down) ---
// DocumentSectionItem already present above (SP3).

export interface DocumentMeta {
	id: string;
	title: string;
	doc_type: string;
	source_url: string | null;
	section_count: number;
	explains_count: number;
}

export interface DocumentDetailData {
	document: DocumentMeta;
	sections: DocumentSectionItem[];
}

// ── SP4b: Note / Module / Chunk types (ported from legacy types.ts) ────

export interface Note {
	uuid: string;
	content: string;
	author: string;
	created_at: string;
	category: Category;
	repo_name: string;
	branch_name: string | null;
	tags: string[];
	// v3 fields (may be null for older entries)
	summary?: string | null;
	facts?: string[] | null;
	related_files?: string[] | null;
	related_symbols?: string[] | null;
	// Saga + temporal fields
	saga_id?: string | null;
	valid_at?: string | null;
	invalid_at?: string | null;
	superseded_by?: string | null;
}

export interface NotePage {
	items: Note[];
	total: number;
	page: number;
	limit: number;
}

export interface Module {
	id: string;
	// FIX (type drift): backend ModuleItem (api/routes/modules.rs) does NOT
	// serialize repo_name, and emits exports_count/file_count as Option<i32>
	// (nullable). Match the wire shape: drop repo_name, mark counts nullable.
	path: string;
	language: string | null;
	summary: string | null;
	exports_count: number | null;
	file_count: number | null;
}

export interface Chunk {
	id: string;
	repo_name: string;
	module_path: string;
	chunk_type: string;
	name: string;
	signature: string | null;
	content: string;
	language: string | null;
}

// --- God nodes ---

export interface GodNode {
	id: string;
	name: string;
	path: string;
	degree: number;
	node_type: string;
	connectivity: number;
	composition: number;
}

export interface GodNodeResponse {
	god_nodes: GodNode[];
	total_nodes: number;
}

// ── SP4c: sources/ingest types (ported from legacy types.ts) ────

export type SourceType = "gitlab" | "website";

export interface Source {
	name: string;
	url: string;
	source_type: SourceType;
	status: string; // "done" | "running" | "failed" | "pending"
	chunk_count: number;
	last_ingested_at: string | null;
}

export interface GitLabBranch {
	name: string;
	default: boolean;
}

export type SourceHealth = "healthy" | "stale" | "failed" | "ingesting";

export interface ActiveJobInfo {
	job_id: string;
	status: string;
	processed_files: number | null;
	total_files: number | null;
}

export interface SourceOverview {
	name: string;
	source_type: SourceType;
	status: SourceHealth;
	branch: string | null;
	chunk_count: number;
	module_count: number;
	note_count: number;
	section_count: number;
	last_synced_at: string | null;
	active_job: ActiveJobInfo | null;
	last_error: string | null;
	can_resume: boolean;
}

export interface SourcesOverviewResponse {
	sources: SourceOverview[];
	total: number;
	healthy: number;
	stale: number;
	failed: number;
	ingesting: number;
}

// ── SP4d: Saga + NoteDetail types + CATEGORIES (ported from legacy types.ts) ──

export const CATEGORIES: Category[] = [
	"ARCHITECTURE",
	"BUG_FIX",
	"CONFIG",
	"ONBOARDING",
	"DECISION",
];

export type SagaSourceType = "issue" | "branch" | "manual";
export type SagaStatus = "active" | "resolved" | "archived" | "open";

export interface Saga {
	id: string;
	repo_name: string;
	name: string | null;
	source_type: string | null;
	source_ref: string | null;
	summary: string | null;
	status: string;
	created_at: string;
	updated_at: string;
	resolved_at: string | null;
	note_count: number;
}

export interface SagaTimelineEntry {
	uuid: string;
	title: string | null;
	category: Category;
	summary: string | null;
	valid_at: string | null;
	invalid_at: string | null;
	superseded_by: string | null;
	created_at: string;
}

export interface SagaDetail {
	saga: Saga;
	timeline: SagaTimelineEntry[];
}

// v3 Sniper detail for NOTE layer
export interface NoteDetail {
	id: string;
	// Backend emits `null` when the DB title column is NULL.
	title: string | null;
	summary: string;
	facts: string[];
	content: string;
	category: string;
	branch?: string;
	related_symbols: string[];
	related_files: string[];
	tags: string[];
	created_at: string;
}

// ── SP4f: token-management types (ported verbatim from legacy types.ts / api.ts) ──

/** Summary row returned by GET /api/v1/auth/tokens */
export interface McpTokenSummary {
	id: string;
	label: string | null;
	issued_at: string; // ISO 8601
	last_used_at: string | null;
	expires_at: string;
	revoked_at: string | null;
}

/** Request body for POST /api/v1/auth/passthrough/revoke */
export interface PassthroughRevokeRequest {
	token?: string;
	prefix?: string;
}

/** Single row returned by GET /api/v1/auth/audit */
export interface AuditEntry {
	ts: string;
	action: string;
	target_id: string | null;
	actor_token_id: string;
	ip: string | null;
	response_summary: string | null;
}
