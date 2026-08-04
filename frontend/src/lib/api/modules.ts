/**
 * Modules + chunks API — ported verbatim from legacy api.ts.
 * Endpoints: GET /repos/:repo/modules, GET /repos/:repo/modules/:path/chunks.
 * Both endpoints may return either Paginated<T> ({ data: T[] }) or a bare T[],
 * so the paginated envelope is unwrapped for backwards compat.
 */

import type { Module, Chunk } from "../types/index.js";
import { get } from "./http.js";

export async function fetchModules(repoName: string): Promise<Module[]> {
  // Backend now returns Paginated<Module>; unwrap for backwards compat
  const res = await get<{ data: Module[] } | Module[]>(
    `/repos/${encodeURIComponent(repoName)}/modules`
  );
  return Array.isArray(res) ? res : res.data;
}

export async function fetchModuleChunks(repoName: string, modulePath: string): Promise<Chunk[]> {
  const res = await get<{ data: Chunk[] } | Chunk[]>(
    `/repos/${encodeURIComponent(repoName)}/modules/${encodeURIComponent(modulePath)}/chunks`
  );
  return Array.isArray(res) ? res : res.data;
}
