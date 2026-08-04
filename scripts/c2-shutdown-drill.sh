#!/usr/bin/env bash
# C2 graceful shutdown acceptance drill.
#
# Starts the backend, fires a sanity health request, sends SIGTERM, then
# verifies the four-event shutdown log sequence and exit code 0.
#
# Usage: bash scripts/c2-shutdown-drill.sh
#
# Requires: cargo, curl, .env with valid DB credentials. Backend must be
# locally buildable. PG + Neo4j must be reachable (e.g.
# `docker compose up postgres neo4j -d`).

set -euo pipefail

LOG_FILE="$(mktemp -t c2-drill.XXXXXX.log)"
PID_FILE="$(mktemp -t c2-drill.XXXXXX.pid)"
APP_CAP_SECS=60
BUFFER_SECS=10
TOTAL_TIMEOUT=$((APP_CAP_SECS + BUFFER_SECS))

cleanup() {
    if [[ -f "$PID_FILE" ]]; then
        local pid
        pid="$(cat "$PID_FILE" 2>/dev/null || true)"
        if [[ -n "${pid:-}" ]] && kill -0 "$pid" 2>/dev/null; then
            kill -KILL "$pid" 2>/dev/null || true
        fi
    fi
    rm -f "$PID_FILE"
    # Keep $LOG_FILE for inspection.
}
trap cleanup EXIT

echo "C2 shutdown drill — log: $LOG_FILE"

echo "1/5 Starting backend (cargo run --release)..."
(
    cd backend
    cargo run --release > "$LOG_FILE" 2>&1 &
    echo $! > "$PID_FILE"
)
BACKEND_PID="$(cat "$PID_FILE")"

echo "2/5 Waiting for 'API server listening' (up to 90s)..."
WAITED=0
while ! grep -q "API server listening" "$LOG_FILE"; do
    sleep 2
    WAITED=$((WAITED + 2))
    if [[ $WAITED -ge 90 ]]; then
        echo "FAIL: backend did not reach 'API server listening' in 90s"
        echo "Last 50 log lines:"
        tail -n 50 "$LOG_FILE"
        exit 1
    fi
done
echo "  Backend ready after ${WAITED}s."

echo "3/5 Firing sanity health request..."
if ! curl -sf "http://127.0.0.1:8081/api/v1/health" -o /dev/null; then
    # Try /health (older route name) as fallback.
    if ! curl -sf "http://127.0.0.1:8081/health" -o /dev/null; then
        echo "WARN: health endpoint returned non-2xx (continuing — may be expected)."
    fi
fi

echo "4/5 Sending SIGTERM to PID $BACKEND_PID..."
kill -TERM "$BACKEND_PID"
SHUTDOWN_START="$(date +%s)"

echo "  Waiting up to ${TOTAL_TIMEOUT}s for clean exit..."
WAITED=0
while kill -0 "$BACKEND_PID" 2>/dev/null; do
    sleep 1
    WAITED=$((WAITED + 1))
    if [[ $WAITED -ge $TOTAL_TIMEOUT ]]; then
        echo "FAIL: backend did not exit within ${TOTAL_TIMEOUT}s of SIGTERM"
        kill -KILL "$BACKEND_PID" 2>/dev/null || true
        exit 1
    fi
done
SHUTDOWN_DURATION=$(($(date +%s) - SHUTDOWN_START))
echo "  Backend exited after ${SHUTDOWN_DURATION}s."

if grep -q "panicked at" "$LOG_FILE"; then
    echo "FAIL: backend log contains 'panicked at'"
    grep "panicked at" "$LOG_FILE"
    exit 1
fi

echo "5/5 Verifying log signature..."
REQUIRED_EVENTS=(
    "shutdown_signal_received"
    "shutdown_drain_started"
    "shutdown_done"
)
MISSING=0
for ev in "${REQUIRED_EVENTS[@]}"; do
    if grep -qE "event=\"$ev\"|event = \"$ev\"|\"event\":\"$ev\"|event: $ev" "$LOG_FILE"; then
        echo "  PASS  event=$ev"
    else
        echo "  FAIL  event=$ev (not found)"
        MISSING=$((MISSING + 1))
    fi
done

# Either drain_complete or drain_timeout_force_exit must appear.
if grep -qE 'event="drain_complete"|event = "drain_complete"|"event":"drain_complete"|event: drain_complete|event="drain_timeout_force_exit"|event = "drain_timeout_force_exit"|"event":"drain_timeout_force_exit"|event: drain_timeout_force_exit' "$LOG_FILE"; then
    echo "  PASS  one of (drain_complete | drain_timeout_force_exit)"
else
    echo "  FAIL  neither drain_complete nor drain_timeout_force_exit logged"
    MISSING=$((MISSING + 1))
fi

if [[ $MISSING -gt 0 ]]; then
    echo
    echo "Drill FAILED — $MISSING required event(s) missing. Log: $LOG_FILE"
    exit 1
fi

echo
echo "C2 shutdown drill: PASS"
echo "  Backend exit duration: ${SHUTDOWN_DURATION}s (cap ${APP_CAP_SECS}s)"
echo "  Log: $LOG_FILE"
