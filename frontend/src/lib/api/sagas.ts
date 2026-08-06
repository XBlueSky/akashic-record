/**
 * Saga API — ported verbatim from legacy api.ts.
 * Endpoints: GET /repos/:repo/sagas, GET /repos/:repo/sagas/:sagaId
 */

import type { Saga, SagaDetail } from "../types/index.js";
import { get } from "./http.js";

/**
 * Fetch all sagas for a repo, optionally filtered by status.
 * Legacy preserves the status query param when provided.
 */
export function fetchSagas(repoName: string, status?: string): Promise<Saga[]> {
	const params = new URLSearchParams();
	if (status) params.set("status", status);
	const qs = params.toString();
	return get<Saga[]>(`/repos/${encodeURIComponent(repoName)}/sagas${qs ? `?${qs}` : ""}`);
}

/**
 * Fetch a single saga with its timeline.
 */
export function fetchSagaDetail(repoName: string, sagaId: string): Promise<SagaDetail> {
	return get<SagaDetail>(
		`/repos/${encodeURIComponent(repoName)}/sagas/${encodeURIComponent(sagaId)}`,
	);
}
