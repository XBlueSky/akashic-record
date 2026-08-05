/**
 * SSE event stream client.
 *
 * Connects once to GET /api/v1/events and dispatches typed callbacks
 * for `job_update`, `repos_changed` and `server_shutting_down` events.
 * EventSource handles automatic reconnection on network errors; the
 * graceful-shutdown event is handled explicitly (see below).
 */

type EventCallback = (data: Record<string, unknown>) => void;

const listeners = new Map<string, Set<EventCallback>>();
let source: EventSource | null = null;
// Pending timer for a hint-driven reconnect, so we never stack reconnects.
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

/** Open the SSE connection (idempotent). */
export function connectEvents() {
	if (source) return;
	// A fresh connect attempt supersedes any pending hint-driven reconnect.
	if (reconnectTimer !== null) {
		clearTimeout(reconnectTimer);
		reconnectTimer = null;
	}
	source = new EventSource("/api/v1/events");

	source.addEventListener("job_update", (e) => {
		dispatch("job_update", JSON.parse((e as MessageEvent).data));
	});

	source.addEventListener("repos_changed", (e) => {
		dispatch("repos_changed", JSON.parse((e as MessageEvent).data));
	});

	// C2 graceful shutdown: the backend emits `server_shutting_down` with a
	// top-level `reconnect_hint_secs` (see backend/src/api/events.rs) as its
	// final event before closing the stream. Surface it to subscribers, then
	// proactively close the source so EventSource's built-in fast retry does
	// not hammer a server that is intentionally going down, and reconnect once
	// after the hinted back-off instead.
	source.addEventListener("server_shutting_down", (e) => {
		const data = JSON.parse((e as MessageEvent).data) as Record<string, unknown>;
		dispatch("server_shutting_down", data);

		const hintSecs = typeof data.reconnect_hint_secs === "number" ? data.reconnect_hint_secs : 30;
		scheduleReconnect(hintSecs);
	});

	source.onerror = () => {
		// EventSource auto-reconnects on transport errors; nothing extra needed.
	};
}

/**
 * Close the SSE connection and cancel any pending reconnect (idempotent).
 * Enables clean teardown (e.g. on layout destroy or in tests). Subscriber
 * registrations added via `onEvent` are left intact, so a later
 * `connectEvents()` resumes delivery to them.
 */
export function disconnectEvents() {
	if (reconnectTimer !== null) {
		clearTimeout(reconnectTimer);
		reconnectTimer = null;
	}
	if (source) {
		source.close();
		source = null;
	}
}

/**
 * Tear down the current source and schedule a single reconnect after the
 * server-provided back-off (seconds). Idempotent: an already-pending timer is
 * replaced so a burst of shutdown events cannot stack reconnects.
 */
function scheduleReconnect(hintSecs: number) {
	if (source) {
		source.close();
		source = null;
	}
	if (reconnectTimer !== null) clearTimeout(reconnectTimer);
	reconnectTimer = setTimeout(() => {
		reconnectTimer = null;
		connectEvents();
	}, hintSecs * 1000);
}

/** Subscribe to an event type. Returns an unsubscribe function. */
export function onEvent(type: string, cb: EventCallback): () => void {
	if (!listeners.has(type)) listeners.set(type, new Set());
	listeners.get(type)!.add(cb);
	return () => {
		listeners.get(type)?.delete(cb);
	};
}

function dispatch(type: string, data: Record<string, unknown>) {
	listeners.get(type)?.forEach((cb) => cb(data));
}
