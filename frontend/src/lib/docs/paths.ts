/** Path algebra for the docs corpus (mirror of server-side
 * `corpus_contract` semantics — see plan Context Primer).
 *
 * A "full corpus key" is the exact `corpus_files.path` string (push corpus:
 * `guide/setup.md`; pull corpus: `docs/guide/setup.md`). SPA URLs use the
 * docs_root-relative form with `.md` stripped; the index page is the empty
 * URL path. */

export interface ResolvedPath {
	fullKey: string;
	escaped: boolean;
}

export function fileDir(path: string): string {
	const i = path.lastIndexOf("/");
	return i === -1 ? "" : path.slice(0, i);
}

/** Resolve a relative link (with `.` and `..` segments) to an absolute corpus key.
 *
 * IMPORTANT: `escaped=true` only flags climbing above the passed `dir` string;
 * for a pull corpus (indexDir prefixed, e.g. dir="docs/guide"), a link like
 * `../../CONTRIBUTING.md` can resolve to `{fullKey:"CONTRIBUTING.md", escaped:false}`
 * even though it's outside the corpus subtree (indexDir="docs"). **Callers MUST
 * compose this with `isInCorpus(fullKey, indexDir)` to validate membership.** */
export function resolveRelative(dir: string, link: string): ResolvedPath {
	const parts = (dir === "" ? [] : dir.split("/")).concat(link.split("/"));
	const out: string[] = [];
	let escaped = false;
	for (const p of parts) {
		if (p === "" || p === ".") continue;
		if (p === "..") {
			if (out.length === 0) escaped = true;
			else out.pop();
		} else {
			out.push(p);
		}
	}
	return { fullKey: out.join("/"), escaped };
}

export function isInCorpus(fullKey: string, indexDir: string): boolean {
	return indexDir === "" || fullKey === indexDir || fullKey.startsWith(`${indexDir}/`);
}

/** Convert a full corpus key to an SPA URL path. Assumes `isInCorpus(fullKey, indexDir)`
 * holds; produces garbage for out-of-corpus keys. */
export function fullKeyToUrlPath(fullKey: string, indexDir: string, indexPath: string): string {
	if (fullKey === indexPath) return "";
	const rel = indexDir === "" ? fullKey : fullKey.slice(indexDir.length + 1);
	return rel.toLowerCase().endsWith(".md") ? rel.slice(0, -3) : rel;
}

/** Convert an SPA URL path back to a full corpus key. Performs no `..`-sanitization
 * by design; traversal defense is server-side. */
export function urlPathToFullKey(urlPath: string, indexDir: string, indexPath: string): string {
	if (urlPath === "") return indexPath;
	const key = `${urlPath}.md`;
	return indexDir === "" ? key : `${indexDir}/${key}`;
}

export function encodeUrlPath(p: string): string {
	return p
		.split("/")
		.map((s) => encodeURIComponent(s))
		.join("/");
}

export function splitAnchor(href: string): { path: string; anchor: string } {
	const i = href.indexOf("#");
	return i === -1 ? { path: href, anchor: "" } : { path: href.slice(0, i), anchor: href.slice(i) };
}
