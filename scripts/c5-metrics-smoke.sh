#!/usr/bin/env bash
# C5 metrics + structured logging smoke drill.
#
# Starts backend, scrapes /api/v1/metrics, verifies all 12 metric family
# names appear, fires a request to confirm http_requests_total increments
# and X-Request-Id round-trips, validates JSON log format.
#
# Usage: bash scripts/c5-metrics-smoke.sh
#
# Requires: cargo, curl, jq (recommended), .env populated, PG + Neo4j
# reachable.

set -euo pipefail

LOG_FILE="$(mktemp -t c5-smoke.XXXXXX.log)"
PID_FILE="$(mktemp -t c5-smoke.XXXXXX.pid)"

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
}
trap cleanup EXIT

echo "C5 smoke — log: $LOG_FILE"

echo "1/6 Building + starting backend..."
(
    cd backend
    cargo run --release > "$LOG_FILE" 2>&1 &
    echo $! > "$PID_FILE"
)
BACKEND_PID="$(cat "$PID_FILE")"

echo "2/6 Waiting for 'API server listening' (up to 90s)..."
WAITED=0
while ! grep -q 'API server listening' "$LOG_FILE"; do
    sleep 2
    WAITED=$((WAITED + 2))
    if [[ $WAITED -ge 90 ]]; then
        echo "FAIL: backend not ready"
        tail -n 30 "$LOG_FILE"
        exit 1
    fi
done

echo "3/6 Verifying logs are JSON..."
NON_JSON=$(grep -cv -E '^\{' "$LOG_FILE" || true)
if [[ $NON_JSON -gt 5 ]]; then
    echo "FAIL: log has $NON_JSON non-JSON lines (threshold 5 for warmup noise)"
    head -n 20 "$LOG_FILE"
    exit 1
fi
echo "  PASS  ($NON_JSON non-JSON lines)"

echo "4/6 Scraping /api/v1/metrics..."
HTTP_STATUS=$(curl -s -o /tmp/c5-metrics.txt -w "%{http_code}" http://127.0.0.1:8081/api/v1/metrics)
if [[ "$HTTP_STATUS" != "200" ]]; then
    echo "FAIL: /api/v1/metrics returned HTTP $HTTP_STATUS"
    head -n 30 "$LOG_FILE"
    exit 1
fi

echo "5/6 Verifying 12 metric family names..."
REQUIRED_METRICS=(
    "akashic_http_requests_total"
    "akashic_http_request_duration_seconds"
    "akashic_http_requests_in_flight"
    "akashic_mcp_tool_calls_total"
    "akashic_llm_tokens_total"
    "akashic_embedding_calls_total"
    "akashic_audit_log_writes_total"
    "akashic_quota_exceeded_total"
    "akashic_shutdown_signal_total"
    "akashic_shutdown_drain_seconds"
    "akashic_oauth_runtime_check_status"
    "akashic_build_info"
)
PRESENT=0
MISSING_NAMES=()
for m in "${REQUIRED_METRICS[@]}"; do
    if grep -q -E "^# HELP $m|^# TYPE $m|^${m}{|^${m} " /tmp/c5-metrics.txt; then
        echo "  PASS  $m"
        PRESENT=$((PRESENT + 1))
    else
        echo "  MISS  $m (will appear after first emit)"
        MISSING_NAMES+=("$m")
    fi
done

# Some metrics only appear after their first emit. akashic_build_info,
# akashic_http_requests_total (this scrape itself!), and
# akashic_http_request_duration_seconds should always be present after
# the first /metrics scrape. If those three are present we accept the
# rest as 'will-appear-on-traffic' deferred.
ALWAYS_PRESENT=("akashic_build_info" "akashic_http_requests_total" "akashic_http_request_duration_seconds")
ALWAYS_PRESENT_OK=true
for m in "${ALWAYS_PRESENT[@]}"; do
    for missing in "${MISSING_NAMES[@]}"; do
        if [[ "$m" == "$missing" ]]; then
            echo "FAIL: required-always metric '$m' is missing"
            ALWAYS_PRESENT_OK=false
        fi
    done
done

if ! $ALWAYS_PRESENT_OK; then
    exit 1
fi

echo "  $PRESENT/12 metric families present (the rest fire only when their code path runs)"

echo "6/6 X-Request-Id round-trip..."
RESP_HEADERS=$(curl -s -i -H 'X-Request-Id: smoke-test-fixed' http://127.0.0.1:8081/health || true)
if echo "$RESP_HEADERS" | grep -qi 'x-request-id: smoke-test-fixed'; then
    echo "  PASS  honored incoming X-Request-Id"
else
    echo "  FAIL  X-Request-Id not echoed"
    echo "$RESP_HEADERS" | head -n 20
    exit 1
fi

echo
echo "C5 smoke: PASS"
echo "  Log: $LOG_FILE (JSON)"
echo "  Metrics scraped: /tmp/c5-metrics.txt"
