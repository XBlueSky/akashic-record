import { error } from "@sveltejs/kit";
import type { LayoutLoad } from "./$types.js";
import { fetchDocsNav, fetchDocsVersions } from "$lib/api";
import { ApiError } from "$lib/api/http.js";
import { fileDir } from "$lib/docs/paths.js";
import { resolveNav, resolveVersionEntry } from "$lib/docs/nav.js";

export const load: LayoutLoad = async ({ params }) => {
  let nav;
  let versions;
  try {
    [nav, versions] = await Promise.all([
      fetchDocsNav(params.repo, params.version),
      fetchDocsVersions(params.repo),
    ]);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) error(404, `no docs for ${params.repo} @ ${params.version}`);
    throw e;
  }
  const entry = resolveVersionEntry(versions, params.version);
  if (!entry) error(404, `no docs for ${params.repo} @ ${params.version}`);
  const indexPath = entry.index;
  const indexDir = fileDir(indexPath);
  return {
    repo: params.repo,
    selector: params.version,
    entry,
    indexPath,
    indexDir,
    nav,
    resolvedNav: resolveNav(nav, indexDir, indexPath),
    stamp: `documents ${params.repo} ${entry.version} @ ${entry.sha.slice(0, 7)}`,
  };
};
