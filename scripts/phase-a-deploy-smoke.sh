#!/usr/bin/env bash
# Phase A deploy smoke — captures evidence for the four runtime acceptance
# checks the Phase A verification doc currently leaves as PENDING (deploy):
#   - A3 AC-5/AC-6: backend startup fail-fast / success
#   - A6 AC-1..AC-6: per-route rate-limit cliff
#   - A7 AC-1..AC-3: MCP read/write split
#
# Pre-conditions:
#   - Operator has run `docker compose -f docker-compose.prod.yml up -d`
#     and waited for healthchecks to pass
#   - Run from the repo root
#
# Output:
#   - Markdown evidence file under docs/superpowers/verifications/
#   - Exit 0 if all four scenarios pass, 1 otherwise
#
# Exit codes:
#   0 — all four scenarios passed
#   1 — at least one scenario failed; evidence file written
#   2 — missing dependency (curl, jq, docker, docker compose v2)
#   3 — pre-flight unreachable: API_BASE/health did not respond
#

set -euo pipefail

# ---- Dependency check ----------------------------------------------------

check_dependencies() {
    local missing=()
    for cmd in bash curl jq docker; do
        command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
    done
    docker compose version >/dev/null 2>&1 || missing+=("docker-compose-v2")
    if [[ ${#missing[@]} -gt 0 ]]; then
        printf >&2 'phase-a-deploy-smoke: missing dependencies: %s\n' "${missing[*]}"
        return 1
    fi
}

# ---- Evidence file ------------------------------------------------------

init_evidence_file() {
    local file="$1"
    mkdir -p "$(dirname "$file")"
    local image_sha
    image_sha="$(docker compose -f "$COMPOSE_FILE" images backend --format '{{.Repository}}@{{.ID}}' 2>/dev/null || echo 'unknown')"
    cat > "$file" <<EOF
# Phase A Deploy Smoke Evidence

- **Date**: $(date -u +%Y-%m-%dT%H:%M:%SZ)
- **Operator**: ${SUDO_USER:-${USER:-unknown}}
- **Compose file**: $COMPOSE_FILE
- **Backend image**: $image_sha
- **API base**: $API_BASE
- **MCP base**: $MCP_BASE

EOF
}

write_section_header() {
    local title="$1"
    {
        echo
        echo "## $title"
        echo
    } >> "$EVIDENCE_FILE"
}

write_summary_table() {
    local rc1="$1" rc2="$2" rc3="$3" rc4="$4"
    local r1 r2 r3 r4
    [[ $rc1 -eq 0 ]] && r1='PASS' || r1='FAIL'
    [[ $rc2 -eq 0 ]] && r2='PASS' || r2='FAIL'
    [[ $rc3 -eq 0 ]] && r3='PASS' || r3='FAIL'
    [[ $rc4 -eq 0 ]] && r4='PASS' || r4='FAIL'
    cat >> "$EVIDENCE_FILE" <<EOF

## Summary

| Scenario | Result |
|---|---|
| A3 fail-fast (AC-5)        | $r1 |
| A3 success (AC-6)          | $r2 |
| A6 rate-limit (AC-1..AC-6) | $r3 |
| A7 MCP split (AC-1..AC-3)  | $r4 |
EOF
}

# ---- Result aggregation -------------------------------------------------

aggregate_results() {
    for rc in "$@"; do
        [[ "$rc" == "0" ]] || return 1
    done
}

# ---- Scenario A3 fail-fast ----------------------------------------------

evaluate_a3_failfast() {
    local exit_code="$1"
    local stderr="$2"
    [[ "$exit_code" -ne 0 ]] || return 1
    local matches=0
    grep -q "GITLAB_APP_ID"      <<<"$stderr" && matches=$((matches + 1))
    grep -q "gitlab.example.com" <<<"$stderr" && matches=$((matches + 1))
    grep -q "akashic_secret"     <<<"$stderr" && matches=$((matches + 1))
    [[ "$matches" -ge 2 ]]
}

scenario_a3_failfast() {
    write_section_header "A3 AC-5 — fail-fast diagnostic shape"
    local output exit_code
    # Capture the real exit code: `... || true` would reset $? to 0 (the
    # status of `true`), making the fail-fast assertion below always FAIL.
    # The `&& a=0 || a=$?` form preserves the command's status and is safe
    # under `set -e`.
    output=$(docker compose -f "$COMPOSE_FILE" run --rm --no-deps -T \
        -e AKASHIC_ENV=production \
        -e GITLAB_APP_ID= \
        -e GITLAB_APP_SECRET=ci-test \
        -e GITLAB_URL=https://gitlab.example.com \
        -e NEO4J_PASSWORD=akashic_secret \
        backend 2>&1) && exit_code=0 || exit_code=$?
    {
        echo '**Command:**'
        echo
        echo '```bash'
        echo 'docker compose -f docker-compose.prod.yml run --rm --no-deps -T \'
        echo '  -e AKASHIC_ENV=production -e GITLAB_APP_ID= \'
        echo '  -e GITLAB_APP_SECRET=ci-test -e GITLAB_URL=https://gitlab.example.com \'
        echo '  -e NEO4J_PASSWORD=akashic_secret backend'
        echo '```'
        echo
        echo '**Captured output (combined stdout+stderr):**'
        echo
        echo '```'
        echo "$output"
        echo '```'
        echo
        echo "**Exit code:** $exit_code"
        echo
        echo '**Pass criterion:** exit != 0 AND stderr contains >=2 of {GITLAB_APP_ID, gitlab.example.com, akashic_secret}'
    } >> "$EVIDENCE_FILE"
    if evaluate_a3_failfast "$exit_code" "$output"; then
        echo '**Result:** PASS' >> "$EVIDENCE_FILE"
        return 0
    else
        echo '**Result:** FAIL' >> "$EVIDENCE_FILE"
        return 1
    fi
}

# ---- Scenario A3 success ------------------------------------------------

scenario_a3_success() {
    write_section_header "A3 AC-6 — backend startup success path"
    local health ready
    health=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 5 "$API_BASE/health"  || echo "000")
    ready=$( curl -sS -o /dev/null -w '%{http_code}' --max-time 5 "$API_BASE/ready"   || echo "000")
    {
        echo '**Commands:**'
        echo
        echo '```bash'
        echo "curl $API_BASE/health  -> $health"
        echo "curl $API_BASE/ready   -> $ready"
        echo '```'
        echo
        echo '**Pass criterion:** both return HTTP 200'
    } >> "$EVIDENCE_FILE"
    if [[ "$health" == "200" && "$ready" == "200" ]]; then
        echo '**Result:** PASS' >> "$EVIDENCE_FILE"
        return 0
    else
        echo '**Result:** FAIL' >> "$EVIDENCE_FILE"
        return 1
    fi
}

# ---- HTTP burst helpers -------------------------------------------------

# Send N HTTP requests against URL, using a single curl invocation with
# --next chaining (HTTP/1.1 keepalive within one process). Returns
# space-separated status codes on stdout.
send_n_requests() {
    local method="$1"
    local url="$2"
    local n="$3"
    local body="${4:-}"
    local args=()
    local i
    for ((i = 0; i < n; i++)); do
        if [[ $i -gt 0 ]]; then args+=(--next); fi
        args+=(-sS -o /dev/null -w '%{http_code} ')
        args+=(-X "$method" "$url" --max-time 5)
        if [[ -n "$body" ]]; then
            args+=(-H 'Content-Type: application/json' --data "$body")
        fi
    done
    curl "${args[@]}" 2>/dev/null || true
}

summarize_histogram() {
    local codes_string="$1"
    # Portable histogram: split on whitespace, sort, count, format.
    # Avoids gawk's asorti so this works under mawk (Debian/Ubuntu default).
    tr ' ' '\n' <<<"$codes_string" \
        | sed '/^$/d' \
        | sort \
        | uniq -c \
        | awk '{ printf "%sx%d ", $2, $1 } END { print "" }'
}

# ---- Scenario A6 rate-limit cliff ---------------------------------------

evaluate_rate_limit_histogram() {
    local codes_string="$1"
    # shellcheck disable=SC2206  # we want word-splitting here
    local -a codes=($codes_string)
    local total=${#codes[@]}
    [[ $total -ge 100 ]] || return 1
    local start=$((total - 100))
    local count_429=0
    local i
    for ((i = start; i < total; i++)); do
        [[ "${codes[i]}" == "429" ]] && count_429=$((count_429 + 1))
    done
    [[ $count_429 -ge 1 ]]
}

scenario_a6_rate_limit() {
    write_section_header "A6 AC-1..AC-6 — rate-limit cliffs"
    local overall=0
    # Each entry: METHOD|URL|BODY|LABEL  (BODY may be empty)
    local routes=(
        "GET|$API_BASE/api/sources||general API"
        "POST|$API_BASE/auth/exchange|{}|auth"
        "POST|$MCP_BASE/message|{}|MCP message"
    )
    local entry method url body label codes hist last100
    for entry in "${routes[@]}"; do
        IFS='|' read -r method url body label <<<"$entry"
        echo "  cooldown 70s before route: $label" >&2
        sleep 70
        echo "  sending 200 reqs to $method $url" >&2
        codes=$(send_n_requests "$method" "$url" 200 "$body")
        hist=$(summarize_histogram "$codes")
        last100=$(awk '{ start = NF - 99; if (start < 1) start = 1; for (i=start; i<=NF; i++) printf "%s ", $i; print "" }' <<<"$codes")
        {
            echo
            echo "### Route: $label ($method $url)"
            echo
            echo "**Histogram (200 reqs):** $hist"
            echo
            echo "**Last-100 tail:**"
            echo
            echo '```'
            echo "$last100"
            echo '```'
            echo
            echo '**Pass criterion:** >=1 x 429 in last 100 of this route'
        } >> "$EVIDENCE_FILE"
        if evaluate_rate_limit_histogram "$codes"; then
            echo '**Result:** PASS' >> "$EVIDENCE_FILE"
        else
            echo '**Result:** FAIL' >> "$EVIDENCE_FILE"
            overall=1
        fi
    done
    return $overall
}

# ---- Scenario A7 MCP read/write split ----------------------------------

evaluate_mcp_write() {
    local body="$1"
    local code data_error
    code=$(jq -r '.error.code // empty' <<<"$body" 2>/dev/null) || return 1
    [[ "$code" == "-32001" ]] || return 1
    data_error=$(jq -r '.error.data.error // empty' <<<"$body" 2>/dev/null) || return 1
    [[ "$data_error" == "unauthorized" ]]
}

evaluate_mcp_read() {
    local status="$1"
    local body="$2"
    [[ "$status" == "200" ]] || return 1
    [[ "$body" != *"-32001"* ]]
}

scenario_a7_mcp_split() {
    write_section_header "A7 AC-1..AC-3 — MCP read/write split"
    echo "  cooldown 70s before MCP scenario" >&2
    sleep 70

    local write_payload read_payload
    write_payload='{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"save_note","arguments":{"repo":"smoke","branch":"smoke","title":"smoke","body":"smoke","category":"smoke"}}}'
    read_payload='{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_knowledge","arguments":{"query":"smoke"}}}'

    local write_body
    write_body=$(curl -sS --max-time 10 -X POST -H 'Content-Type: application/json' \
        --data "$write_payload" "$MCP_BASE/message" || echo '{}')

    local read_status read_body_file read_body
    read_body_file=$(mktemp)
    read_status=$(curl -sS --max-time 30 -o "$read_body_file" -w '%{http_code}' \
        -X POST -H 'Content-Type: application/json' \
        --data "$read_payload" "$MCP_BASE/message" || echo "000")
    read_body=$(cat "$read_body_file")
    rm -f "$read_body_file"

    {
        echo '**Write call (save_note):**'
        echo
        echo '```json'
        echo "$write_body"
        echo '```'
        echo
        echo "**Read call (search_knowledge), HTTP $read_status:**"
        echo
        echo '```'
        # Truncate read body to 800 chars to keep evidence file readable
        echo "${read_body:0:800}"
        echo '```'
        echo
        echo '**Pass criterion:**'
        echo '- write: response has error.code = -32001 AND error.data.error = "unauthorized"'
        echo '- read:  HTTP 200 AND body does NOT contain "-32001"'
    } >> "$EVIDENCE_FILE"

    local write_pass=0 read_pass=0
    evaluate_mcp_write "$write_body" && write_pass=1
    evaluate_mcp_read "$read_status" "$read_body" && read_pass=1

    if [[ $write_pass -eq 1 && $read_pass -eq 1 ]]; then
        echo '**Result:** PASS' >> "$EVIDENCE_FILE"
        return 0
    else
        echo "**Result:** FAIL (write=$write_pass read=$read_pass)" >> "$EVIDENCE_FILE"
        return 1
    fi
}

# ---- main and __main__ guard --------------------------------------------

main() {
    # Defaults — override via env vars if needed. Set inside main() so
    # sourcing this script for tests does not trigger $(date) and other
    # side effects.
    COMPOSE_FILE="${COMPOSE_FILE:-docker-compose.prod.yml}"
    API_BASE="${API_BASE:-http://127.0.0.1:8081}"
    MCP_BASE="${MCP_BASE:-http://127.0.0.1:8080}"
    EVIDENCE_FILE="${EVIDENCE_FILE:-docs/superpowers/verifications/phase-a-deploy-smoke-evidence-$(date +%Y%m%d-%H%M).md}"

    check_dependencies || exit 2

    # Pre-flight: TCP reachability of API. We only check that curl can
    # reach the host:port — not the HTTP status — because /health may
    # legitimately return 5xx during slow startup. Operator runs this
    # script after compose healthchecks have settled.
    if ! curl -sS --max-time 3 -o /dev/null "$API_BASE/health"; then
        printf >&2 'phase-a-deploy-smoke: %s/health unreachable. Has `docker compose -f %s up -d` been run and healthchecked?\n' "$API_BASE" "$COMPOSE_FILE"
        exit 3
    fi

    init_evidence_file "$EVIDENCE_FILE"
    echo "Evidence file: $EVIDENCE_FILE"

    local rc1=0 rc2=0 rc3=0 rc4=0
    scenario_a3_failfast    || rc1=$?
    scenario_a3_success     || rc2=$?
    scenario_a6_rate_limit  || rc3=$?
    scenario_a7_mcp_split   || rc4=$?

    write_summary_table "$rc1" "$rc2" "$rc3" "$rc4"

    if aggregate_results "$rc1" "$rc2" "$rc3" "$rc4"; then
        echo "All four scenarios passed. Evidence: $EVIDENCE_FILE"
        return 0
    else
        echo "One or more scenarios failed. Evidence: $EVIDENCE_FILE"
        return 1
    fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
