#!/usr/bin/env bash
# C3 migration discipline drill.
#
# Exercises the full lifecycle:
#   1. Drop schema → migrate up applies it.
#   2. Verify against fresh DB → 0.
#   3. Drop a table → verify reports it missing → 76.
#   4. Up again is idempotent → 0.
#   5. Down errors out → 70.
#   6. MIGRATE_ON_BOOT=false against missing-schema DB → 76 at boot.
#
# Usage: bash scripts/c3-migration-drill.sh
#
# Requires: cargo, docker compose. .env populated.

set -euo pipefail

cleanup() {
    docker compose start postgres neo4j 2>/dev/null || true
}
trap cleanup EXIT

echo "C3 migration drill"

echo "1/6 Bringing up dependencies..."
docker compose up -d postgres neo4j 1>/dev/null
sleep 5

echo "2/6 Dropping public schema for fresh start..."
docker compose exec -T postgres psql -U akashic -d akashic \
    -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;" 1>/dev/null

echo "3/6 Running 'migrate up' against fresh DB..."
(cd backend && cargo run --release -- migrate up) || {
    echo "FAIL: migrate up returned non-zero on fresh DB"
    exit 1
}

echo "4/6 Running 'migrate verify' (should pass)..."
(cd backend && cargo run --release -- migrate verify) || {
    echo "FAIL: migrate verify failed against post-up DB"
    exit 1
}
echo "  PASS  verify exit 0"

echo "5/6 Dropping 'notes' table and re-running verify (should fail with 76)..."
docker compose exec -T postgres psql -U akashic -d akashic \
    -c "DROP TABLE notes CASCADE;" 1>/dev/null
set +e
(cd backend && cargo run --release -- migrate verify) > /tmp/c3-verify-fail.log 2>&1
RC=$?
set -e
if [[ $RC -ne 76 ]]; then
    echo "FAIL: expected exit 76 on missing notes table, got $RC"
    tail -n 20 /tmp/c3-verify-fail.log
    exit 1
fi
if ! grep -q '"notes"' /tmp/c3-verify-fail.log; then
    echo "FAIL: verify error did not mention missing 'notes' table"
    tail -n 20 /tmp/c3-verify-fail.log
    exit 1
fi
echo "  PASS  verify exit 76 with 'notes' in error message"

echo "6/6 Idempotent re-up + down + boot-verify-fail..."
(cd backend && cargo run --release -- migrate up) || {
    echo "FAIL: idempotent migrate up failed"
    exit 1
}
echo "  PASS  re-up exit 0"

set +e
(cd backend && cargo run --release -- migrate down) > /tmp/c3-down.log 2>&1
RC=$?
set -e
if [[ $RC -ne 70 ]]; then
    echo "FAIL: expected exit 70 on 'migrate down', got $RC"
    tail -n 10 /tmp/c3-down.log
    exit 1
fi
if ! grep -qi "not supported" /tmp/c3-down.log; then
    echo "FAIL: 'migrate down' did not produce 'not supported' message"
    tail -n 10 /tmp/c3-down.log
    exit 1
fi
echo "  PASS  migrate down exit 70 with 'not supported'"

echo "  Final: MIGRATE_ON_BOOT=false against missing-schema DB..."
docker compose exec -T postgres psql -U akashic -d akashic \
    -c "DROP TABLE notes CASCADE;" 1>/dev/null
set +e
(cd backend && MIGRATE_ON_BOOT=false cargo run --release) > /tmp/c3-boot-verify.log 2>&1 &
BOOT_PID=$!
WAITED=0
while kill -0 "$BOOT_PID" 2>/dev/null; do
    sleep 1
    WAITED=$((WAITED + 1))
    if [[ $WAITED -ge 30 ]]; then
        kill -KILL "$BOOT_PID" 2>/dev/null || true
        echo "FAIL: boot did not exit within 30s on verify fail"
        tail -n 30 /tmp/c3-boot-verify.log
        exit 1
    fi
done
wait "$BOOT_PID"
RC=$?
set -e
if [[ $RC -ne 76 ]]; then
    echo "FAIL: expected exit 76 on MIGRATE_ON_BOOT=false verify-fail, got $RC"
    tail -n 30 /tmp/c3-boot-verify.log
    exit 1
fi
echo "  PASS  boot exit 76 on missing-schema DB with MIGRATE_ON_BOOT=false"

echo "Restoring schema..."
(cd backend && cargo run --release -- migrate up) 1>/dev/null

echo
echo "C3 migration drill: PASS"
