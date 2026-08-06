# Akashic Record — CI Gates SOP

This document is the canonical reference for **which GitHub Actions
jobs block merge** and **what to do when they fail**. Single-operator
project; treat this as "future-you" documentation.

## Required-to-merge jobs

The following jobs (defined in `.github/workflows/ci.yml`) MUST be
green for a PR to merge to `main`. Branch protection on `main` lists
them as required status checks; the list below is the source of truth
— if the GitHub settings drift, re-align them with the command in
"Reconstructing branch protection" below.

| Job | Stage | Source track | What it gates |
|---|---|---|---|
| `secrets-scan` | secrets | A4 | gitleaks against committed secrets |
| `backend-check` | check | A4 / D1 | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, schema provisioning (`cargo run -p akashic-server --bin akashic-server -- migrate up` against Postgres + Neo4j service containers), `cargo test --workspace` |
| `frontend-check` | check | A4 / D1 | `npm run lint` (eslint), `npm run format:check` (prettier), `npm test` (vitest), `npm run check` (svelte-check), `npm run build` |
| `frontend-audit` | audit | A5 | `npm run audit` (`npm audit --omit=dev`) piped through `frontend/scripts/audit-gate.mjs` (rule-table-driven) |
| `backend-audit` | audit | A5 | `cargo audit` piped through `backend/scripts/audit-gate-rs.mjs` against `backend/security/risk-acceptance.yml` (gate's own vitest suite runs first) |
| `prod-compose-config` | check | A1 | `scripts/prod-compose-config-check.sh`: prod compose renders, no inline fallbacks, placeholder detector self-test |
| `backend-rate-limit-smoke` | check | A6 | `cargo test -p akashic-platform rate_limit` |
| `backend-mcp-auth-smoke` | check | B1 | `cargo test -p akashic-mcp mcp_middleware` + `cargo test -p akashic-http --lib mcp_oauth` (DB-backed — same Postgres + Neo4j service containers as `backend-check`) |
| **`frontend-e2e`** | test | D3 → **D1** | **4 Playwright golden paths** against `docker-compose.test.yml` |
| **`mcp-contract`** | test | D4 → **D1** | **MCP tool contract tests** via rmcp client through the public proxy |

**Service containers on `check`-stage jobs.** Two jobs spin up
short-lived Postgres (`pgvector/pgvector:pg16`) + Neo4j
(`neo4j:5-community`) service containers directly (GitHub Actions
`services:`, not the `docker-compose.test.yml` stack used by the
`test` stage):

- `backend-check` — `auth::{account,mcp_oauth,oauth_device}`,
  `api::routes::health_oauth`, and `api::extractors` idempotency tests
  connect to a live Postgres + Neo4j rather than testcontainers, and
  `akashic-ingestion`'s `community/summarize.rs` + corpus-derive tests
  need the full schema. The job runs `cargo run -p akashic-server
  --bin akashic-server -- migrate up` first to provision it — this is
  new since the tracing-subscriber fix made the CLI usable in CI.
- `backend-mcp-auth-smoke` — `mcp_oauth`'s token-exchange tests are
  similarly DB-backed. No migration step here; the job doesn't touch
  schema.

**Playwright pinning.** `frontend-e2e` runs Playwright pinned exactly
at `1.62.1`, paired with the `mcr.microsoft.com/playwright:v1.62.1-jammy`
image. The version-pairing rule (bump both together in the same
commit) is documented in `docs/operations/frontend-e2e.md` ("Keep the
pairing in sync") — this doc cross-references rather than duplicates
it.

## Pipeline shape

```
stages (via `needs:` in ci.yml): secrets → check + audit → test

secrets-scan ──┬─→ backend-check ──────────────┐
               ├─→ frontend-check ─────────────┤
               ├─→ prod-compose-config ────────┤
               ├─→ backend-rate-limit-smoke ───┼─→ frontend-e2e
               ├─→ backend-mcp-auth-smoke ─────┼─→ mcp-contract
               ├─→ frontend-audit ─────────────┤
               └─→ backend-audit ──────────────┘
```

A red `secrets-scan` short-circuits the pipeline before any expensive
build runs. The `test` stage jobs are the slowest (image build + cargo
compile) and only start once every check/audit job is green.

All jobs run on every PR — there are deliberately no path filters,
because every job is a required check and a skipped required check
blocks the merge forever.

## What to do when a gate fails

### 1. Inspect the failure

GitHub PR page → Checks tab → failing job → logs. Artifacts uploaded
on failure (Actions run page, "Artifacts" section):
- `frontend-e2e`: `frontend-e2e-artifacts` = `frontend/playwright-report/`
  (HTML report) + `compose-logs.txt`
- `mcp-contract`: `mcp-contract-compose-logs` = `compose-logs.txt`
- `frontend-audit`: `frontend-audit-report` = `frontend/.audit-output.json`

### 2. Reproduce locally

The `test`-stage jobs are fully reproducible against the same
docker-compose.test.yml stack:

```bash
# frontend-e2e equivalent:
docker compose -f docker-compose.test.yml up -d --wait --build
cd frontend && npm ci --no-audit --no-fund && npm run e2e
cd .. && docker compose -f docker-compose.test.yml down -v

# mcp-contract equivalent:
docker compose -f docker-compose.test.yml up -d --wait --build
cd backend
TEST_DATABASE_URL="postgres://akashic:akashic@localhost:55432/akashic" \
TEST_NEO4J_URI=bolt://localhost:57687 \
TEST_NEO4J_USER=neo4j TEST_NEO4J_PASSWORD=akashic-test \
TEST_MCP_URL=http://localhost:13002 \
  cargo test -p akashic-server --features test-fixtures --test mcp_contract --release
cd .. && docker compose -f docker-compose.test.yml down -v
```

The `check`-stage DB-backed jobs are reproducible without the compose
stack — just the two service containers CI uses directly:

```bash
# backend-check equivalent (schema migration + full workspace tests):
docker run -d --name akashic-ci-pg \
  -e POSTGRES_USER=akashic -e POSTGRES_PASSWORD=akashic_secret -e POSTGRES_DB=akashic \
  -p 5433:5432 pgvector/pgvector:pg16
docker run -d --name akashic-ci-neo4j \
  -e NEO4J_AUTH=neo4j/akashic_secret -p 7687:7687 neo4j:5-community

cd backend
export DATABASE_URL=postgres://akashic:akashic_secret@localhost:5433/akashic
export NEO4J_URI=bolt://localhost:7687 NEO4J_USER=neo4j NEO4J_PASSWORD=akashic_secret
export EMBEDDING_PROVIDER=openai EMBEDDING_API_KEY=ci-unused-embedding-key

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p akashic-server --bin akashic-server -- migrate up
cargo test --workspace

cd .. && docker rm -f akashic-ci-pg akashic-ci-neo4j   # cleanup

# backend-mcp-auth-smoke equivalent (same service containers, no
# migration step and no EMBEDDING_* — that job's env block doesn't set them):
cd backend
DATABASE_URL=postgres://akashic:akashic_secret@localhost:5433/akashic \
NEO4J_URI=bolt://localhost:7687 NEO4J_USER=neo4j NEO4J_PASSWORD=akashic_secret \
  cargo test -p akashic-mcp mcp_middleware
DATABASE_URL=postgres://akashic:akashic_secret@localhost:5433/akashic \
NEO4J_URI=bolt://localhost:7687 NEO4J_USER=neo4j NEO4J_PASSWORD=akashic_secret \
  cargo test -p akashic-http --lib mcp_oauth
```

### 3. Known CI flakes

Three patterns are already understood and (partially) mitigated in
the workflow itself — check this list before assuming a new flake:

- **`npm ci` "Exit handler never called!"** (npm/cli#4028) — can hit
  `frontend-check`, `frontend-audit`, and `frontend-e2e` (every job
  that runs `npm ci`). Root cause: `package-lock.json` "resolved" URLs
  pointing at an internal registry unreachable from GitHub-hosted
  runners. Fixed at the source — `frontend/.npmrc` pins
  `registry=https://registry.npmjs.org` and the lockfile was
  regenerated against it — but each of those jobs still carries one
  automatic `npm ci` retry (cache clean + reinstall) as a safety net
  for genuine transient `registry.npmjs.org` blips. A failed first
  attempt dumps the npm debug log before retrying, so a real
  recurrence is diagnosable instead of guessed at.
- **Hosted-runner disk pressure** on `frontend-e2e` / `mcp-contract` —
  the two jobs that `docker compose -f docker-compose.test.yml up
  --build` a five-service stack. Both run a "Free disk space" step
  (`sudo rm -rf /usr/local/lib/android /opt/hostedtoolcache/CodeQL
  /usr/share/dotnet`) before the build, reclaiming space from
  preinstalled toolchains this project never uses.
- **Image pull rate limits** — generic Docker Hub / GHCR throttling.
  No structural mitigation exists; rerun the failed job.

### 4. Fix vs bypass

The "should I bypass this gate?" decision tree:

```
Is the failure a real bug?
├── Yes → fix it. Push a new commit. Wait for green.
└── No  → Is it a documented external outage? (githubstatus.com)
         ├── Yes → wait for outage recovery, rerun failed jobs.
         └── No  → Is it a known flake? (see "Known CI flakes" above)
                  ├── Yes → rerun once. If still red after 2 retries, treat as real bug.
                  └── No  → STOP. Investigate. Do not merge.
```

There is no "skip CI" button. Branch protection has `enforce_admins`
on, so even the repo owner cannot merge red. Single-operator project
means you have to actually run through this — but you also can't blame
anyone else for red CI.

## Adding a new gate

When a new track ships a CI-eligible test suite:

1. Land the job in `.github/workflows/ci.yml` with
   `continue-on-error: true` first (GitHub's equivalent of
   allow_failure).
2. Watch for 1-2 weeks of normal operation; if it stays green, remove
   `continue-on-error` and add the job name to the required-checks
   contexts in branch protection.
3. Add it to the "Required-to-merge jobs" table above.

OR: if the track had thorough local-pass acceptance before landing,
promote directly. Document the reasoning in the spec.

(Pre-release, single-operator note: the original 10 gates skipped the
observation window deliberately — there was no traffic to observe.)

## Reconstructing branch protection

If the settings are ever lost or drift, re-apply with:

```bash
gh api -X PUT repos/XBlueSky/akashic-record/branches/main/protection --input - <<'JSON'
{
  "required_status_checks": {
    "strict": false,
    "contexts": [
      "secrets-scan", "backend-check", "frontend-check",
      "prod-compose-config", "backend-rate-limit-smoke",
      "backend-mcp-auth-smoke", "frontend-audit", "backend-audit",
      "frontend-e2e", "mcp-contract"
    ]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "required_approving_review_count": 0
  },
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false
}
JSON
```

Verify:

```bash
gh api repos/XBlueSky/akashic-record/branches/main/protection \
  --jq '.required_status_checks.contexts'
```

## Deliberately-failing PR proof

To verify the gate is actually enforcing, run this drill once per
quarter (or whenever branch protection is touched):

```bash
# 1. Create a throwaway branch.
git checkout -b ci-gate-drill-$(date +%Y%m%d)

# 2. Introduce a deliberate failure. E.g., for frontend-e2e:
sed -i '' 's/akashic-record/akashic-RECORD-typo/' frontend/tests/e2e/specs/01-anonymous-read.spec.ts

# 3. Push and open a PR.
git add frontend/tests/e2e/specs/01-anonymous-read.spec.ts
git commit -m "drill: deliberately fail frontend-e2e to verify gate enforcement"
git push -u origin ci-gate-drill-$(date +%Y%m%d)
gh pr create --title "[drill] verify frontend-e2e gate blocks merge" --body "Do not merge."

# 4. Observe:
#    - `gh pr checks` shows frontend-e2e failing.
#    - `gh pr view --json mergeStateStatus` reports "BLOCKED".

# 5. Close the PR without merging. Delete the branch.
gh pr close --delete-branch
git checkout main
git branch -D ci-gate-drill-$(date +%Y%m%d) 2>/dev/null || true

# 6. Record the drill in your private ops log:
#    YYYY-MM-DD  CI gate drill: frontend-e2e blocked merge as expected.
```

If the drill DOESN'T block, branch protection has drifted from this
doc; re-align with "Reconstructing branch protection" before any real
merge.

## Related docs

- `frontend-e2e.md` — operator runbook for the `frontend-e2e` job:
  local Playwright workflow, version-pairing rule, `test-fixtures`
  threat model, failure-mode triage
- `incident-runbook.md` — class D includes CI/runner outage scenarios
- `on-call-sop.md` — alert-driven triage; CI failures don't fire alerts
  but can foreshadow real production issues

## When the runner itself is broken

Runners are GitHub-hosted. If jobs queue forever or infrastructure
errors appear, check https://www.githubstatus.com/ — there is nothing
to fix in this repo. Wait for recovery, then rerun failed jobs. (Disk
pressure specifically is already partly mitigated by the "Free disk
space" step on the two heavy jobs — see "Known CI flakes" above; if
that step itself starts being insufficient, that's a sign the compose
stack's image footprint has grown, not a runner problem.)
