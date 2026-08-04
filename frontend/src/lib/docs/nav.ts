import type { DocsNav, DocsVersionEntry } from "$lib/api/docs.js";
import { fileDir, fullKeyToUrlPath, resolveRelative } from "./paths.js";

export interface NavPageResolved {
  title: string;
  description: string;
  fullKey: string;
  urlPath: string;
}

export interface ResolvedNavGroup {
  title: string;
  pages: NavPageResolved[];
}

export interface ResolvedNav {
  groups: ResolvedNavGroup[];
  flat: NavPageResolved[];
}

/** Resolve nav page paths (written relative to the index file, like any
 * markdown link) into full corpus keys + SPA url paths. */
export function resolveNav(nav: DocsNav, indexDir: string, indexPath: string): ResolvedNav {
  const groups: ResolvedNavGroup[] = nav.groups.map((g) => ({
    title: g.title,
    pages: g.pages.map((p) => {
      const { fullKey } = resolveRelative(fileDir(indexPath), p.path);
      return {
        title: p.title,
        description: p.description,
        fullKey,
        urlPath: fullKeyToUrlPath(fullKey, indexDir, indexPath),
      };
    }),
  }));
  return { groups, flat: groups.flatMap((g) => g.pages) };
}

export function prevNext(
  flat: NavPageResolved[],
  currentFullKey: string,
  indexPath: string,
): { prev?: NavPageResolved; next?: NavPageResolved } {
  const i = flat.findIndex((p) => p.fullKey === currentFullKey);
  if (i === -1) {
    // The index page sits "before" the first nav entry; true orphans get
    // no prev/next at all.
    return currentFullKey === indexPath ? { next: flat[0] } : {};
  }
  return { prev: flat[i - 1], next: flat[i + 1] };
}

/** Client-side mirror of the server's version selector rules (display
 * only — page fetches always pass the raw selector through). */
export function resolveVersionEntry(
  versions: DocsVersionEntry[],
  selector: string,
): DocsVersionEntry | undefined {
  if (selector === "latest") return versions.find((v) => v.is_latest) ?? versions[0];
  const exact = versions.find((v) => v.version === selector);
  if (exact) return exact;
  if (selector.length >= 7) {
    return versions
      .filter((v) => v.sha.startsWith(selector))
      .sort((a, b) => a.ingested_at.localeCompare(b.ingested_at))[0];
  }
  return undefined;
}
