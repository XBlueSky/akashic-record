/**
 * Graph + module + document-detail API calls — ported verbatim from legacy api.ts.
 */

import { get } from "./http.js";
import type {
	GraphData,
	ModuleGraphData,
	ChunkCallGraph,
	DocGraphData,
	DocumentDetailData,
	GodNodeResponse,
	ModuleDetailData,
} from "../types/index.js";

export function fetchGraph(repoName: string): Promise<GraphData> {
	return get<GraphData>(`/graph/${encodeURIComponent(repoName)}`);
}

export function fetchModuleGraph(repoName: string): Promise<ModuleGraphData> {
	return get<ModuleGraphData>(`/module-graph/${encodeURIComponent(repoName)}`);
}

export function fetchModuleDetail(moduleId: string): Promise<ModuleDetailData> {
	return get<ModuleDetailData>(`/modules/${encodeURIComponent(moduleId)}/chunks`);
}

export function fetchModuleCallGraph(moduleId: string): Promise<ChunkCallGraph> {
	return get<ChunkCallGraph>(`/modules/${encodeURIComponent(moduleId)}/call-graph`);
}

export function fetchGodNodes(repoName: string): Promise<GodNodeResponse> {
	return get<GodNodeResponse>(`/god-nodes/${encodeURIComponent(repoName)}`);
}

export function fetchDocGraph(repoName: string): Promise<DocGraphData> {
	return get<DocGraphData>(`/doc-graph/${encodeURIComponent(repoName)}`);
}

export function fetchDocumentDetail(docId: string): Promise<DocumentDetailData> {
	return get<DocumentDetailData>(`/documents/${encodeURIComponent(docId)}/sections`);
}

export function fetchClusterDetail(clusterId: string): Promise<DocumentDetailData> {
	return get<DocumentDetailData>(`/clusters/${encodeURIComponent(clusterId)}/sections`);
}
