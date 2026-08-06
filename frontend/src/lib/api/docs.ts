import { BASE, get } from "./http.js";
import { encodeUrlPath } from "$lib/docs/paths.js";

export interface DocsRepoEntry {
	repo: string;
	version: string;
	sha: string;
	page_count: number | null;
	ingested_at: string;
	derive_status: string;
	description: string;
	/** manifest.index — the index page's full corpus key. */
	index: string;
}

export interface DocsVersionEntry {
	version: string;
	sha: string;
	is_tagged: boolean;
	is_latest: boolean;
	ingested_at: string;
	derive_status: string;
	index: string;
}

export interface DocsNavPage {
	title: string;
	/** Relative to the index file's directory — resolve before use. */
	path: string;
	description: string;
}

export interface DocsNavGroup {
	title: string;
	pages: DocsNavPage[];
}

export interface DocsNav {
	description: string;
	groups: DocsNavGroup[];
}

export interface DocsPageResponse {
	markdown: string;
	title: string;
	stamp: string;
	document_id?: string;
}

export function fetchDocsRepos(): Promise<{ repos: DocsRepoEntry[] }> {
	return get<{ repos: DocsRepoEntry[] }>("/docs");
}

export function fetchDocsVersions(repo: string): Promise<DocsVersionEntry[]> {
	return get<DocsVersionEntry[]>(`/docs/${encodeURIComponent(repo)}`);
}

export function fetchDocsNav(repo: string, version: string): Promise<DocsNav> {
	return get<DocsNav>(`/docs/${encodeURIComponent(repo)}/${encodeURIComponent(version)}/nav`);
}

export function fetchDocsPage(
	repo: string,
	version: string,
	fullKey: string,
): Promise<DocsPageResponse> {
	return get<DocsPageResponse>(
		`/docs/${encodeURIComponent(repo)}/${encodeURIComponent(version)}/page/${encodeUrlPath(fullKey)}`,
	);
}

export function docsRawUrl(repo: string, version: string, fullKey: string): string {
	return `${BASE}/docs/${encodeURIComponent(repo)}/${encodeURIComponent(version)}/raw/${encodeUrlPath(fullKey)}`;
}
