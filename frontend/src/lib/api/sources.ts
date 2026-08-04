/**
 * Sources overview API — ported verbatim from legacy api.ts.
 * Covers fetchSourcesOverview, fetchAllSources, and the pure
 * transform helper sourceFromOverview.
 */

import { get } from "./http.js";
import type { Source, SourceOverview, SourcesOverviewResponse } from "../types/index.js";

export function fetchSourcesOverview(): Promise<SourcesOverviewResponse> {
  return get<SourcesOverviewResponse>("/sources/overview");
}

/**
 * Map a backend `SourceOverview` row to the flat `Source` shape consumed by
 * SourcePicker's "Recent Sources" list. Exported for unit testing the parse.
 *
 * The overview endpoint reports `last_synced_at` and a `SourceHealth` status
 * (`healthy | stale | failed | ingesting`), so we forward those onto the
 * `Source` fields the picker reads (`last_ingested_at`, `status`). `url` is not
 * part of the overview row (and the picker doesn't render it), so it defaults
 * to "".
 */
export function sourceFromOverview(s: SourceOverview): Source {
  return {
    name: s.name,
    url: "",
    source_type: s.source_type,
    status: s.status,
    chunk_count: s.chunk_count,
    last_ingested_at: s.last_synced_at,
  };
}

/**
 * Bug fix: this previously hit `GET /api/v1/sources`, a route the backend never
 * registers (only `GET /api/v1/sources/overview` and `POST /api/v1/sources/add`
 * exist — see backend/src/api/routes/mod.rs). Every call 404'd, so SourcePicker's
 * "Recent Sources" was permanently empty. Point at the real overview endpoint
 * and adapt its `{ sources: SourceOverview[] }` payload to `Source[]`.
 */
export async function fetchAllSources(): Promise<Source[]> {
  const res = await get<SourcesOverviewResponse>("/sources/overview");
  return res.sources.map(sourceFromOverview);
}
