/**
 * Notes + permissions API — ported verbatim from legacy api.ts.
 * Endpoints: GET /repos/:repo/notes, DELETE /repos/:repo/notes/:uuid,
 *            GET /repos/:repo/permissions.
 */

import type { NotePage, Category, NoteDetail } from "../types/index.js";
import { get, del, put } from "./http.js";

export function fetchNotes(
	repoName: string,
	opts: {
		branch?: string;
		category?: Category;
		page?: number;
		limit?: number;
	} = {},
): Promise<NotePage> {
	const params = new URLSearchParams();
	if (opts.branch) params.set("branch", opts.branch);
	if (opts.category) params.set("category", opts.category);
	if (opts.page != null) params.set("page", String(opts.page));
	if (opts.limit != null) params.set("limit", String(opts.limit));
	const qs = params.toString();
	return get<NotePage>(`/repos/${encodeURIComponent(repoName)}/notes${qs ? `?${qs}` : ""}`);
}

// The single-note endpoint returns a NoteDetail-shaped object (it has `title`,
// `content`, `facts`, etc. — fields the list-row `Note` type lacks), so the
// return type is NoteDetail, not Note.
export function fetchNote(repoName: string, uuid: string): Promise<NoteDetail> {
	return get<NoteDetail>(
		`/repos/${encodeURIComponent(repoName)}/notes/${encodeURIComponent(uuid)}`,
	);
}

/**
 * Delete a note. Legacy sets `credentials: "include"` here (unlike most GETs),
 * so we pass it through to the del() helper which forwards it to fetch().
 */
export async function deleteNote(repoName: string, uuid: string): Promise<void> {
	return del(`/repos/${encodeURIComponent(repoName)}/notes/${encodeURIComponent(uuid)}`, {
		credentials: "include",
	});
}

/**
 * Update a note. Legacy uses PUT + `credentials: "include"` + JSON body.
 * No idempotency key in the legacy implementation — match exactly.
 */
export async function updateNote(
	repoName: string,
	uuid: string,
	body: { title: string; summary: string; content: string; category: string; tags: string[] },
): Promise<void> {
	return put<void>(`/repos/${encodeURIComponent(repoName)}/notes/${encodeURIComponent(uuid)}`, {
		body,
		credentials: "include",
	});
}

export async function fetchPermissions(
	repoName: string,
): Promise<{ access_level: number; can_edit: boolean; can_delete: boolean }> {
	try {
		return await get<{ access_level: number; can_edit: boolean; can_delete: boolean }>(
			`/repos/${encodeURIComponent(repoName)}/permissions`,
		);
	} catch {
		return { access_level: 0, can_edit: false, can_delete: false };
	}
}
