#!/usr/bin/env bash
# C6 readiness expansion smoke drill.
#
# Verifies /ready returns 200 with all probes ok, returns 503 within 35s
# of stopping a dependency (PG), and recovers to 200 within 8s of
# restarting the dependency.
#
# Usage: bash scripts/c6-readiness-smoke.sh
#
# Requires: cargo, curl, jq, docker compose, .env populated.

set -euo pipefail

LOG_FILE="$(mktemp -t c6-smoke.XXXXXX.log)"
PID_FILE="$(mktemp -t c6-smoke.XXXXXX.pid)"

cleanup() {
    if [[ -f "$PID_FILE" ]]; then
        local pid
        pid="$(cat "$PID_FILE" 2>/dev/null || true)"
        if [[ -n "${pid:-}" ]] && kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
            sleep 2
            kill -KILL "$pid" 2>/dev/null || true
        fi
    fi
    rm -f "$PID_FILE"
    # Best-effort: ensure PG is back up so subsequent dev work isn't broken.
    docker compose start postgres 2>/dev/null || true
}
trap cleanup EXIT

echo "C6 readiness drill — log: $LOG_FILE"

echo "1/7 Bringing up dependencies (postgres, neo4j)..."
docker compose up -d postgres neo4j 1>/dev/null

echo "2/7 Starting backend..."
(
    cd backend
    cargo run --release > "$LOG_FILE" 2>&1 &
    echo $! > "$PID_FILE"
)
BACKEND_PID="$(cat "$PID_FILE")"

echo "3/7 Waiting for 'API server listening'..."
WAITED=0
while ! grep -q 'API server listening' "$LOG_FILE"; do
    sleep 2
    WAITED=$((WAITED + 2))
    if [[ $WAITED -ge 90 ]]; then
        echo "FAIL: backend not ready in 90s"
        tail -n 30 "$LOG_FILE"
        exit 1
    fi
done

# Wait one extra poll cycle so the readiness state has fresh data.
sleep 7

echo "4/7 Asserting /ready returns 200 with all probes ok..."
RESP=$(curl -s -w "\n%{http_code}" http://127.0.0.1:8081/ready)
BODY=$(echo "$RESP" | head -n -1)
CODE=$(echo "$RESP" | tail -n 1)
if [[ "$CODE" != "200" ]]; then
    echo "FAIL: expected 200, got $CODE"
    echo "$BODY" | jq . 2>/dev/null || echo "$BODY"
    exit 1
fi
ALL_OK=$(echo "$BODY" | jq -r '.all_ok')
if [[ "$ALL_OK" != "true" ]]; then
    echo "FAIL: all_ok=$ALL_OK"
    echo "$BODY" | jq .
    exit 1
fi
echo "  PASS  /ready 200, all_ok=true"

echo "5/7 Stopping PG and waiting 35s for /ready to flip to 503..."
docker compose stop postgres 1>/dev/null
sleep 35

CODE=$(curl -s -o /tmp/c6-after-stop.json -w "%{http_code}" http://127.0.0.1:8081/ready)
if [[ "$CODE" != "503" ]]; then
    echo "FAIL: expected 503 after PG stop, got $CODE"
    jq . /tmp/c6-after-stop.json 2>/dev/null || cat /tmp/c6-after-stop.json
    exit 1
fi
PG_OK=$(jq -r '.probes.postgres.ok' /tmp/c6-after-stop.json)
if [[ "$PG_OK" != "false" ]]; then
    echo "FAIL: probes.postgres.ok=$PG_OK after stop"
    jq . /tmp/c6-after-stop.json
    exit 1
fi
echo "  PASS  /ready 503, probes.postgres.ok=false"

echo "6/7 Starting PG and waiting 8s for /ready to recover to 200..."
docker compose start postgres 1>/dev/null
sleep 8

CODE=$(curl -s -o /tmp/c6-after-start.json -w "%{http_code}" http://127.0.0.1:8081/ready)
if [[ "$CODE" != "200" ]]; then
    echo "FAIL: expected 200 after PG restart, got $CODE"
    jq . /tmp/c6-after-start.json 2>/dev/null || cat /tmp/c6-after-start.json
    exit 1
fi
echo "  PASS  /ready recovered to 200"

echo "7/7 Done."
echo
echo "C6 readiness drill: PASS"
echo "  Log: $LOG_FILE"
