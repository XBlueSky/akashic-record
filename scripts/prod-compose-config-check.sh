#!/usr/bin/env bash
# CI gate: prod compose invariants.
#   1. docker-compose.prod.yml renders with a fully-populated env file.
#   2. No inline ${VAR:-default} fallbacks — every required value must
#      come from the env file so a missing var fails before any
#      container starts (AKASHIC_BUILD_COMMIT build-arg is the one
#      allowed exception; it is not a secret).
#   3. Self-test: the placeholder detector must trip on .env.example,
#      proving the gate can actually catch placeholder secrets.
# Verifies: docs/operations/ci-gates.md "prod-compose-config".
# Requires: docker compose v2.
#
# env_file indirection (read this before touching the checks below):
#   docker-compose.prod.yml hard-codes `env_file: /etc/akashic/akashic.env`
#   on the neo4j/postgres/backend services. That path only exists on a
#   provisioned production host (root:root, mode 0600 — see
#   docs/operations/rotation-sop.md); it does not exist on CI runners or
#   most dev machines. `docker compose ... config` opens every env_file
#   eagerly — even for `config`, not just `up` — so with the path missing
#   it fails outright ("open /etc/akashic/akashic.env: no such file or
#   directory"), no matter how complete --env-file is.
#   Verified experimentally: a second `-f overlay.yml` cannot fix this.
#   docker compose (tested: v2.2.1) merges `env_file` lists across -f
#   layers by concatenation — it does not let an overlay replace or clear
#   a base file's env_file — so even `env_file: []` in an overlay still
#   leaves the missing base path in the merged list, and it still fails.
#   Fix used here: for every `config` invocation, this script makes a
#   throwaway *textual* copy of docker-compose.prod.yml (mktemp) with the
#   literal "/etc/akashic/akashic.env" string swapped for a real,
#   temporary substitute env file, then runs `docker compose -f <copy>
#   config`. Nothing else in the file changes, so the rendered output,
#   the fallback grep (which reads the real file, not the copy), and the
#   placeholder self-test all still exercise the actual committed
#   docker-compose.prod.yml content — only the env-file *source* differs.
#   No sudo, no writes outside mktemp, identical behavior on any machine
#   (CI included) that lacks /etc/akashic/akashic.env.
#
# Comment-aware fallback grep:
#   docker-compose.prod.yml's own header comment explains the "no inline
#   ${VAR:-default} fallbacks" rule using that exact ${VAR:-default}
#   token as prose, which a plain grep for the pattern self-matches. The
#   check below only looks for the pattern before the first `#` on each
#   line so the descriptive comment doesn't trip its own gate.
set -euo pipefail
cd "$(dirname "$0")/.."

PROD_COMPOSE=docker-compose.prod.yml
ENV_FILE_PATH='/etc/akashic/akashic.env'

tmpfiles=()
trap 'rm -f "${tmpfiles[@]}"' EXIT

# render_prod_compose SUBST_ENV_FILE INTERP_ENV_FILE
# Prints `docker compose config` output for docker-compose.prod.yml with
# its hard-coded env_file path swapped for SUBST_ENV_FILE (both args must
# be absolute paths), interpolating ${VAR} placeholders in the compose
# file from INTERP_ENV_FILE.
render_prod_compose() {
    local subst_env_file=$1 interp_env_file=$2 patched
    patched=$(mktemp)
    tmpfiles+=("$patched")
    sed "s#${ENV_FILE_PATH}#${subst_env_file}#" "$PROD_COMPOSE" > "$patched"
    docker compose -f "$patched" --env-file "$interp_env_file" config
}

tmpenv=$(mktemp)
tmpfiles+=("$tmpenv")
grep -E '^[A-Za-z_][A-Za-z0-9_]*=' .env.example \
    | sed -E 's/=.*/=ci-dummy-value/' > "$tmpenv"
render_prod_compose "$tmpenv" "$tmpenv" > /dev/null
echo "OK: prod compose renders with a complete env file"

if grep -nE '^[^#]*\$\{[A-Za-z_][A-Za-z0-9_]*:-' "$PROD_COMPOSE" \
    | grep -v 'AKASHIC_BUILD_COMMIT'; then
    echo "FAIL: inline \${VAR:-default} fallback found in docker-compose.prod.yml" >&2
    exit 1
fi
echo "OK: no inline fallbacks"

rendered=$(render_prod_compose "$PWD/.env.example" "$PWD/.env.example" 2>/dev/null || true)
if ! grep -q 'changeme' <<<"$rendered"; then
    echo "FAIL: placeholder self-test did not trip on .env.example — detector broken?" >&2
    exit 1
fi
echo "OK: placeholder detector self-test trips on .env.example"
