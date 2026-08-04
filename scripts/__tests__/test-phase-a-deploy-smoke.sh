#!/usr/bin/env bash
# Unit tests for scripts/phase-a-deploy-smoke.sh pure evaluator functions.
# Sources the script (which has a __main__ guard) and invokes evaluators
# with canned inputs.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../phase-a-deploy-smoke.sh
source "$HERE/../phase-a-deploy-smoke.sh"

PASS=0
FAIL=0

assert_zero() {
    local name="$1"
    shift
    if "$@"; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $name (expected zero exit, got $?)"
        FAIL=$((FAIL + 1))
    fi
}

assert_nonzero() {
    local name="$1"
    shift
    if "$@"; then
        echo "  FAIL: $name (expected non-zero, got 0)"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    fi
}

echo "== check_dependencies =="
assert_zero "all deps present" check_dependencies

echo
echo "== aggregate_results =="
assert_zero    "all zero"        aggregate_results 0 0 0 0
assert_nonzero "one fail"        aggregate_results 0 1 0 0
assert_nonzero "all fail"        aggregate_results 1 1 1 1
assert_zero    "no args (vacuous)" aggregate_results
assert_nonzero "non-numeric"        aggregate_results notanumber

echo
echo "== evaluate_a3_failfast =="
assert_zero    "exit=2 with 3 keywords"        evaluate_a3_failfast 2 \
    "config error: GITLAB_APP_ID empty; gitlab.example.com placeholder; akashic_secret rejected"
assert_zero    "exit=1 with 2 keywords"        evaluate_a3_failfast 1 \
    "panicked: GITLAB_APP_ID is required and akashic_secret is forbidden"
assert_nonzero "exit=0 fails (must be non-zero)" evaluate_a3_failfast 0 \
    "GITLAB_APP_ID empty gitlab.example.com akashic_secret"
assert_nonzero "only 1 keyword fails"           evaluate_a3_failfast 2 \
    "config error: GITLAB_APP_ID is empty"
assert_nonzero "no keywords fails"              evaluate_a3_failfast 2 \
    "some unrelated stderr"

echo
echo "== evaluate_rate_limit_histogram =="

# Generate a 200-code string with N×429 in the last 100
gen_codes() {
    local n_429="$1"
    local prefix=""
    for ((i = 0; i < 100; i++)); do prefix+="200 "; done
    local tail=""
    for ((i = 0; i < n_429; i++)); do tail+="429 "; done
    for ((i = n_429; i < 100; i++)); do tail+="200 "; done
    echo "${prefix}${tail}"
}

assert_zero    "1x 429 in last 100"   evaluate_rate_limit_histogram "$(gen_codes 1)"
assert_zero    "50x 429 in last 100"  evaluate_rate_limit_histogram "$(gen_codes 50)"
assert_nonzero "0x 429 in last 100"   evaluate_rate_limit_histogram "$(gen_codes 0)"
assert_nonzero "<100 codes total"     evaluate_rate_limit_histogram "200 200 200"

echo
echo "== evaluate_mcp_write =="
assert_zero    "valid 401 envelope" evaluate_mcp_write \
    '{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"unauthorized","data":{"error":"unauthorized","tool":"save_note"}}}'
assert_nonzero "wrong code"          evaluate_mcp_write \
    '{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"server error","data":{"error":"unauthorized"}}}'
assert_nonzero "missing data.error"  evaluate_mcp_write \
    '{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"unauthorized"}}'
assert_nonzero "no error field"      evaluate_mcp_write \
    '{"jsonrpc":"2.0","id":1,"result":{}}'
assert_nonzero "non-JSON"            evaluate_mcp_write 'not json'

echo
echo "== evaluate_mcp_read =="
assert_zero    "200 with result"     evaluate_mcp_read 200 \
    '{"jsonrpc":"2.0","id":2,"result":{"content":[]}}'
assert_zero    "200 empty body"      evaluate_mcp_read 200 ""
assert_nonzero "200 but contains -32001" evaluate_mcp_read 200 \
    '{"jsonrpc":"2.0","id":2,"error":{"code":-32001,"message":"unauthorized"}}'
assert_nonzero "non-200"             evaluate_mcp_read 401 \
    '{"jsonrpc":"2.0","id":2,"result":{}}'
assert_nonzero "000 (timeout)"       evaluate_mcp_read 000 ""

echo
echo "Tests: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]
