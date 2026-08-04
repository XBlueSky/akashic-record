/**
 * Shared HTTP fetch wrapper — extracted faithfully from legacy api.ts.
 * Provides: ApiError, isAuthError, idempotencyKey, and the internal get<T>()
 * helper. Other modules in api/ consume get() directly.
 */

/** Custom error class for API responses with status code */
export class ApiError extends Error {
  constructor(
    public status: number,
    message: string
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/**
 * Returns true if the error is a 401 ApiError. The app shell is responsible
 * for the user-facing response (toast + auth-store teardown); this predicate
 * only classifies the error.
 */
export function isAuthError(e: unknown): boolean {
  return e instanceof ApiError && e.status === 401;
}

/**
 * Generate a unique idempotency key (UUID v4) for mutation requests.
 *
 * `crypto.randomUUID()` is only exposed in *secure contexts* (HTTPS, or
 * `http://localhost`). When the app is served over plain HTTP on a LAN host
 * (e.g. `http://dev-box.internal:3000`) it is `undefined`, which previously
 * threw `TypeError: crypto.randomUUID is not a function` and broke every
 * mutation (ingest, add source, note edits). `crypto.getRandomValues()`
 * carries no secure-context restriction, so we derive a v4 UUID from it as a
 * fallback. Exported for unit testing.
 */
export function idempotencyKey(): string {
  if (typeof crypto?.randomUUID === "function") {
    return crypto.randomUUID();
  }
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6] & 0x0f) | 0x40; // version 4
  b[8] = (b[8] & 0x3f) | 0x80; // variant 1 (RFC 4122)
  const h = Array.from(b, (x) => x.toString(16).padStart(2, "0"));
  return `${h[0]}${h[1]}${h[2]}${h[3]}-${h[4]}${h[5]}-${h[6]}${h[7]}-${h[8]}${h[9]}-${h[10]}${h[11]}${h[12]}${h[13]}${h[14]}${h[15]}`;
}

export const BASE = "/api/v1";

/**
 * Handle non-ok API responses.
 * 401 → throw ApiError(401); the app shell layer that imports this module owns
 * the user-facing response (toast + auth-store teardown).
 * Other errors → throw ApiError for callers to handle.
 */
export function handleError(res: Response): never {
  if (res.status === 401) {
    throw new ApiError(401, "Unauthorized");
  }
  // Friendlier messages for the status codes the backend actually returns on
  // mutations (e.g. reingest → 409 when a job is already running, 429 from the
  // rate limiter). Callers surface `ApiError.message` directly to the user.
  const friendly: Record<number, string> = {
    409: "Already in progress — a job for this resource is still running.",
    429: "Too many requests — please wait a moment and retry.",
    500: "Server error — please retry; check the backend logs if it persists.",
    503: "Service unavailable — the backend may still be starting up.",
  };
  throw new ApiError(res.status, friendly[res.status] ?? `API ${res.status}: ${res.statusText}`);
}

export async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`);
  if (!res.ok) handleError(res);
  return res.json() as Promise<T>;
}

/**
 * POST helper — mirrors the legacy fetch calls for mutation endpoints.
 * Pass `idempotency: true` to include an `Idempotency-Key` header (used by
 * reingest, addSource, triggerIngest, resumeIngest in the legacy api).
 * Pass a JSON-serialisable `body` for endpoints that require a request body.
 */
export async function post<T>(
  path: string,
  opts: { body?: unknown; idempotency?: boolean } = {}
): Promise<T> {
  const headers: Record<string, string> = {};
  if (opts.idempotency) headers["Idempotency-Key"] = idempotencyKey();
  if (opts.body !== undefined) headers["Content-Type"] = "application/json";
  const res = await fetch(`${BASE}${path}`, {
    method: "POST",
    headers,
    body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
  });
  if (!res.ok) handleError(res);
  return res.json() as Promise<T>;
}

/**
 * DELETE helper — mirrors the legacy fetch calls for delete endpoints.
 * Returns void (no response body expected).
 * Pass `credentials: "include"` for endpoints that require session cookies
 * (e.g. deleteNote which sets credentials: "include" in the legacy api).
 */
export async function del(
  path: string,
  opts: { credentials?: RequestCredentials } = {}
): Promise<void> {
  const res = await fetch(`${BASE}${path}`, {
    method: "DELETE",
    credentials: opts.credentials,
  });
  if (!res.ok) handleError(res);
}

/**
 * PUT helper — mirrors the legacy fetch calls for full-update endpoints.
 * Returns void (callers that need a body can cast, but legacy updateNote
 * discards the response body).
 * Pass `credentials: "include"` for session-cookie-gated endpoints.
 */
export async function put<T = void>(
  path: string,
  opts: { body?: unknown; credentials?: RequestCredentials; idempotency?: boolean } = {}
): Promise<T> {
  const headers: Record<string, string> = {};
  if (opts.idempotency) headers["Idempotency-Key"] = idempotencyKey();
  if (opts.body !== undefined) headers["Content-Type"] = "application/json";
  const res = await fetch(`${BASE}${path}`, {
    method: "PUT",
    headers,
    credentials: opts.credentials,
    body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
  });
  if (!res.ok) handleError(res);
  // Some PUT endpoints return an empty body; guard before parsing.
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}
