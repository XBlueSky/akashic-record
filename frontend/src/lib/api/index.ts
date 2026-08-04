/**
 * Public API layer — re-exports all shell functions and shared utilities so
 * consumers can use: `import { fetchRepos, searchKnowledge } from '$lib/api'`
 */

export { fetchRepos, fetchActiveJobs, fetchRepoDetail, fetchBranches, deleteRepo, reingest } from "./repos.js";
export { searchKnowledge } from "./search.js";
export {
  fetchGraph,
  fetchModuleGraph,
  fetchModuleDetail,
  fetchModuleCallGraph,
  fetchGodNodes,
  fetchDocGraph,
  fetchDocumentDetail,
  fetchClusterDetail,
} from "./graph.js";
export { ApiError, isAuthError, idempotencyKey } from "./http.js";
export { fetchNotes, fetchNote, deleteNote, fetchPermissions, updateNote } from "./notes.js";
export { fetchSagas, fetchSagaDetail } from "./sagas.js";
export { fetchModules, fetchModuleChunks } from "./modules.js";
export { fetchSourcesOverview, fetchAllSources, sourceFromOverview } from "./sources.js";
export {
  addSource,
  triggerIngest,
  fetchIngestStatus,
  resumeIngest,
  fetchGitLabBranches,
} from "./ingest.js";
export {
  fetchDocsRepos,
  fetchDocsVersions,
  fetchDocsNav,
  fetchDocsPage,
  docsRawUrl,
} from "./docs.js";
export type { Repository, ActiveJob, SearchKnowledgeResult } from "../types/index.js";
export type {
  GraphData,
  GraphNode,
  GraphEdge,
  ModuleGraphData,
  ModuleNode,
  ImportEdge,
  SagaGroup,
  ChunkCallGraph,
  DocGraphData,
  DocGraphNode,
  DocumentDetailData,
  DocumentMeta,
  GodNode,
  GodNodeResponse,
  ModuleDetailData,
  ChunkItem,
  ChunkNoteItem,
  NoteItem,
  Category,
  Branch,
  RepoDetail,
  IngestionJob,
  Note,
  NotePage,
  Module,
  Chunk,
  SourceType,
  Source,
  GitLabBranch,
  SourceHealth,
  ActiveJobInfo,
  SourceOverview,
  SourcesOverviewResponse,
  Saga,
  SagaDetail,
  SagaTimelineEntry,
  SagaSourceType,
  SagaStatus,
  NoteDetail,
} from "../types/index.js";
export type {
  DocsRepoEntry,
  DocsVersionEntry,
  DocsNavPage,
  DocsNavGroup,
  DocsNav,
  DocsPageResponse,
} from "./docs.js";
export { CATEGORIES } from "../types/index.js";
export { listMcpTokens, revokeMcpToken, revokePassthrough, listMyAudit } from "./tokens.js";
export type { McpTokenSummary, PassthroughRevokeRequest, AuditEntry } from "../types/index.js";
