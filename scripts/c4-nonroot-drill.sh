#!/usr/bin/env bash
# C4 non-root container drill.
#
# Verifies:
#   - 'akashic' user exists with UID/GID 10001
#   - container runs as akashic (whoami != root)
#   - /tmp/akashic-ingest is writable by akashic
#   - /etc/* is NOT writable (proves we're not root)
#   - HF_HOME env points where the spec says
#   - AKASHIC_BUILD_COMMIT --build-arg is embedded into the binary
#
# Usage: bash scripts/c4-nonroot-drill.sh
#
# Requires: docker, docker compose, git checkout (for build).

set -euo pipefail

echo "C4 non-root container drill"

echo "1/9 Building backend image..."
AKASHIC_BUILD_COMMIT=$(git rev-parse --short=12 HEAD 2>/dev/null || echo "drill-test")
export AKASHIC_BUILD_COMMIT
docker compose -f docker-compose.yml build backend 2>&1 | tail -3

# Resolve the image ID once so subsequent docker run invocations are
# self-contained (don't depend on compose state).
IMG=$(docker compose -f docker-compose.yml images -q backend)
if [[ -z "$IMG" ]]; then
    # Fallback: docker compose images may not work without the service up.
    # Use the conventional image name from compose project.
    IMG="akashic-record-backend"
fi

echo "2/9 whoami inside container..."
WHO=$(docker run --rm --entrypoint=whoami "$IMG")
if [[ "$WHO" != "akashic" ]]; then
    echo "FAIL: whoami returned '$WHO', expected 'akashic'"
    exit 1
fi
echo "  PASS  whoami=$WHO"

echo "3/9 id -u inside container..."
UID_VAL=$(docker run --rm --entrypoint=id "$IMG" -u)
if [[ "$UID_VAL" != "10001" ]]; then
    echo "FAIL: id -u returned '$UID_VAL', expected '10001'"
    exit 1
fi
echo "  PASS  id -u=$UID_VAL"

echo "4/9 id -g inside container..."
GID_VAL=$(docker run --rm --entrypoint=id "$IMG" -g)
if [[ "$GID_VAL" != "10001" ]]; then
    echo "FAIL: id -g returned '$GID_VAL', expected '10001'"
    exit 1
fi
echo "  PASS  id -g=$GID_VAL"

echo "5/9 /tmp/akashic-ingest writable..."
if ! docker run --rm --entrypoint=sh "$IMG" -c "touch /tmp/akashic-ingest/probe && rm /tmp/akashic-ingest/probe"; then
    echo "FAIL: cannot write to /tmp/akashic-ingest"
    exit 1
fi
echo "  PASS  /tmp/akashic-ingest writable by akashic"

echo "6/9 /etc/* NOT writable (proves non-root)..."
set +e
docker run --rm --entrypoint=sh "$IMG" -c "touch /etc/probe" >/dev/null 2>&1
RC=$?
set -e
if [[ $RC -eq 0 ]]; then
    echo "FAIL: container could write /etc/probe — process is still effectively root"
    exit 1
fi
echo "  PASS  /etc/probe write rejected (rc=$RC)"

echo "7/9 HF_HOME env points to /home/akashic/.cache/huggingface..."
HF=$(docker run --rm --entrypoint=printenv "$IMG" HF_HOME)
if [[ "$HF" != "/home/akashic/.cache/huggingface" ]]; then
    echo "FAIL: HF_HOME='$HF', expected '/home/akashic/.cache/huggingface'"
    exit 1
fi
echo "  PASS  HF_HOME=$HF"

echo "8/9 AKASHIC_BUILD_COMMIT embedded in binary..."
# debian-slim doesn't ship `strings`; use `tr` to extract printable runs
# from the binary, then grep for the literal commit string.
EMBED=$(docker run --rm --entrypoint=sh "$IMG" -c \
    "tr -c '[:print:]' '\n' < /usr/local/bin/akashic-record | grep -F '$AKASHIC_BUILD_COMMIT' | head -1" \
    || true)
if [[ -z "$EMBED" ]]; then
    echo "FAIL: build-arg commit '$AKASHIC_BUILD_COMMIT' not embedded in binary"
    exit 1
fi
echo "  PASS  binary contains commit string '$AKASHIC_BUILD_COMMIT'"

echo "9/9 Done."
echo
echo "C4 non-root container drill: PASS"
