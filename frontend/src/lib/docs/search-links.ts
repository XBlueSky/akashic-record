import { fetchDocsRepos } from "$lib/api";
import type { SearchDocsRef } from "$lib/api/search.js";
import { encodeUrlPath, fileDir, fullKeyToUrlPath } from "./paths.js";

export interface DocsIndexInfo {
  indexPath: string;
  indexDir: string;
}

let cache: Promise<Map<string, DocsIndexInfo>> | null = null;

/** repo → index info, fetched once per session (palette use). */
export function getDocsIndexMap(): Promise<Map<string, DocsIndexInfo>> {
  cache ??= fetchDocsRepos().then(
    (r) =>
      new Map(
        r.repos.map((e) => [e.repo, { indexPath: e.index, indexDir: fileDir(e.index) }]),
      ),
  );
  return cache;
}

export function docsHitHref(docs: SearchDocsRef, info: DocsIndexInfo | undefined): string {
  const base = `/docs/${encodeURIComponent(docs.repo)}/latest`;
  if (!info) return base;
  const urlPath = fullKeyToUrlPath(docs.path, info.indexDir, info.indexPath);
  const frag = docs.anchor ? `#${docs.anchor}` : "";
  return (urlPath === "" ? base : `${base}/${encodeUrlPath(urlPath)}`) + frag;
}
