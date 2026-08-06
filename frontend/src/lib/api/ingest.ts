/**
 * Ingest + GitLab API — ported verbatim from legacy api.ts.
 * Covers addSource, triggerIngest, fetchIngestStatus, resumeIngest,
 * and fetchGitLabBranches.
 */

import { get, post, ApiError } from "./http.js";
import type { IngestionJob, SourceType, GitLabBranch } from "../types/index.js";

export async function addSource(
	sourceType: SourceType,
	url: string,
	options: { git_ref?: string; crawl_depth?: number; url_pattern?: string } = {},
): Promise<{ job_id: string; repo_name: string; status: string }> {
	return post<{ job_id: string; repo_name: string; status: string }>("/sources/add", {
		body: { source_type: sourceType, url, ...options },
		idempotency: true,
	});
}

/**
 * A website submission is gated: the backend writes the source as
 * pending_review (or unsupported) and starts NO crawl, so it returns an empty
 * job_id and never a pollable job. Map its status to the confirmation card the
 * UI should show instead of the (job-polling) progress view.
 */
export function websiteSubmissionOutcome(status: string): "unsupported" | "review" {
	return status === "unsupported" ? "unsupported" : "review";
}

export async function triggerIngest(
	repoName: string,
	gitRef: string,
	source: string = "gitlab",
	path?: string,
): Promise<{ job_id: string }> {
	return post<{ job_id: string }>(`/repos/${encodeURIComponent(repoName)}/ingest`, {
		body: { git_ref: gitRef, source, path },
		idempotency: true,
	});
}

export async function fetchIngestStatus(repoName: string): Promise<IngestionJob | null> {
	// The backend `ingest_status` handler returns 404 (AppError::NotFound) when no
	// job has ever run for this repo — it does NOT return a JSON null. `get<>` turns
	// that 404 into a thrown ApiError, so the documented `| null` ("no job yet")
	// contract was dead: callers like IngestProgress.svelte (`if (status) job = …`)
	// expect a falsy return, not an exception. Catch only the no-job 404 and map it
	// to null; let every other status (401, 5xx, …) propagate as before.
	try {
		return await get<IngestionJob>(`/repos/${encodeURIComponent(repoName)}/ingest/status`);
	} catch (e) {
		if (e instanceof ApiError && e.status === 404) return null;
		throw e;
	}
}

export async function resumeIngest(repoName: string): Promise<{ job_id: string; status: string }> {
	return post<{ job_id: string; status: string }>(`/repos/${encodeURIComponent(repoName)}/resume`, {
		idempotency: true,
	});
}

export function fetchGitLabBranches(repo: string): Promise<GitLabBranch[]> {
	return get<GitLabBranch[]>(`/gitlab/branches?repo=${encodeURIComponent(repo)}`);
}
