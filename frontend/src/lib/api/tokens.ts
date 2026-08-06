/**
 * Token-management API — ported faithfully from legacy api.ts (B4 section).
 *
 * These endpoints live under /api/v1/auth/ and use raw fetch (like the legacy
 * code) to match the exact method/credentials/error-handling contract.
 * None use credentials:"include" or idempotency keys in the legacy source.
 * revokePassthrough has special non-401 error handling preserved verbatim.
 */

import { ApiError, handleError, get } from "./http.js";
import type { McpTokenSummary, PassthroughRevokeRequest, AuditEntry } from "../types/index.js";

/** List MCP tokens for the authenticated user. GET /api/v1/auth/tokens */
export async function listMcpTokens(): Promise<McpTokenSummary[]> {
	return get<McpTokenSummary[]>("/auth/tokens");
}

/** Revoke a single MCP token by id. POST /api/v1/auth/tokens/:id/revoke */
export async function revokeMcpToken(id: string): Promise<void> {
	// Raw fetch (not the shared post()): this endpoint returns an empty body, and
	// post() unconditionally res.json()s the response.
	const res = await fetch(`/api/v1/auth/tokens/${encodeURIComponent(id)}/revoke`, {
		method: "POST",
	});
	if (!res.ok) handleError(res);
}

/**
 * Revoke a passthrough token by full token value or prefix.
 * POST /api/v1/auth/passthrough/revoke
 *
 * Error handling matches legacy: 401 → handleError (never returns);
 * other non-ok statuses → surface backend's JSON `error` message.
 */
export async function revokePassthrough(
	body: PassthroughRevokeRequest,
): Promise<{ prefix: string }> {
	const res = await fetch("/api/v1/auth/passthrough/revoke", {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify(body),
	});
	if (!res.ok) {
		// 401 → handleError clears the user store and raises the global Sign-in
		// toast (consistent with every other endpoint); it throws and never returns.
		if (res.status === 401) handleError(res);
		// For other statuses, keep surfacing the backend's JSON `error` message.
		const err = await res.json().catch(() => ({}));
		throw new ApiError(
			res.status,
			(err as { error?: string }).error ?? `revokePassthrough: ${res.status}`,
		);
	}
	return res.json();
}

/** List audit log entries for the authenticated user. GET /api/v1/auth/audit?limit=N */
export async function listMyAudit(limit = 50): Promise<AuditEntry[]> {
	return get<AuditEntry[]>(`/auth/audit?limit=${limit}`);
}
