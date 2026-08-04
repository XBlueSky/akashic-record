/**
 * Scout search API — shell subset ported verbatim from legacy api.ts.
 */

import { get } from "./http.js";
import type { SearchKnowledgeResult } from "../types/index.js";

export interface SearchDocsRef {
  repo: string;
  /** Full corpus key (documents.source_url). */
  path: string;
  anchor: string;
}

// v3 Scout: compact search across MAP and NOTE layers
export async function searchKnowledge(
  query: string,
  repo?: string,
  limit?: number
): Promise<SearchKnowledgeResult[]> {
  const params = new URLSearchParams({ q: query });
  if (repo) params.set("repo", repo);
  if (limit != null) params.set("limit", String(limit));
  return get<SearchKnowledgeResult[]>(`/search?${params.toString()}`);
}
