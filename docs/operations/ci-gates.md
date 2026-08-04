# Akashic Record — CI Gates SOP

This document is the canonical reference for **which GitLab CI jobs
block merge** and **what to do when they fail**. Single-operator
project; treat this as "future-you" documentation.

## Required-to-merge jobs

The following CI jobs MUST be green for an MR to merge to `master`.
Configure GitLab Project Settings → Repository → Protected branches
→ `master` → "Required pipeline checks" to enforce this at the platform
level. The list below is the source of truth; if the GitLab settings
drift, treat this doc as canonical and re-align.

| Job | Stage | Source track | What it gates |
|---|---|---|---|
| `secrets-scan` | secrets | A4 | gitleaks against committed secrets |
| `backend-check` | check | A4 / D1 | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` |
| `frontend-check` | check | A4 / D1 | `npm test` (vitest), `svelte-check`, `npm run build` |
| `frontend-audit` | audit | A5 | `npm audit` via `scripts/audit-gate.mjs` (rule-table-driven) |
| `backend-audit` | audit | A5 | `cargo audit` against `backend/security/risk-acceptance.yml` |
| `prod-compose-config` | check | A1 | `docker compose -f docker-compose.prod.yml config` rejects placeholder secrets |
| `backend-rate-limit-smoke` | check | A6 | minimal rate-limit middleware smoke test |
| `backend-mcp-auth-smoke` | check | B1 | minimal MCP auth-gate smoke test |
| **`frontend-e2e`** | test | D3 → **D1** | **3 Playwright golden paths** (anonymous read, authenticated edit, logout cookie parity) against `docker-compose.test.yml` |
| **`mcp-contract`** | test | D4 → **D1** | **14 MCP tool contract tests** (10 read + tools_list count + 2 write + 1 anon-rejection) via rmcp SSE client through proxy |

D1 promoted `frontend-e2e` from `allow_failure: true` to required, and
added `mcp-contract` as required-from-day-one (D4 had local-pass
acceptance before D1 shipped, so the "land-as-allow_failure" path used
for D3 wasn't needed).

## Pipeline shape

```
stages: secrets → check → audit → test

secrets:
  └── secrets-scan (gitleaks)

check:
  ├── backend-check          (cargo fmt + clippy + cargo test)
  ├── frontend-check         (vitest + svelte-check + build)
  ├── prod-compose-config    (compose YAML + placeholder rejection)
  ├── backend-rate-limit-smoke
  └── backend-mcp-auth-smoke

audit:
  ├── frontend-audit         (npm audit + audit-gate.mjs)
  └── backend-audit          (cargo audit + risk-acceptance.yml)

test:
  ├── frontend-e2e           (Playwright × docker-compose.test.yml)
  └── mcp-contract           (cargo test × docker-compose.test.yml)
```

Each stage gates the next: a red `secrets-scan` short-circuits the
pipeline before any expensive build runs. The `test` stage is the
slowest (DinD + image pulls + cargo compile) and lives at the tail.

## What to do when a gate fails

### 1. Inspect the failure

GitLab CI UI: click the failing job → "Job logs" tab. Pipeline artifacts
on failure carry:
- `frontend-e2e`: `frontend/playwright-report/` (HTML report) +
  `compose-logs.txt`
- `mcp-contract`: `compose-logs.txt` + `backend/target/release/deps/`
  (intermediate build artifacts; rarely needed)
- `frontend-audit` / `backend-audit`: `audit-gate-report.json` (or
  similar) listing the violating advisory

### 2. Reproduce locally

Both `test`-stage jobs are fully reproducible against the same
docker-compose.test.yml stack:

```bash
# frontend-e2e equivalent:
docker compose -f docker-compose.test.yml up -d --wait
cd frontend && npm ci && npm run e2e

# mcp-contract equivalent:
docker compose -f docker-compose.test.yml up -d --wait
cd backend
PG_USER=akashic PG_PASS=akashic \
  TEST_DATABASE_URL="postgres://$PG_USER:$PG_PASS@localhost:55432/akashic" \
TEST_NEO4J_URI=bolt://localhost:57687 \
TEST_NEO4J_USER=neo4j TEST_NEO4J_PASSWORD=akashic-test \
TEST_MCP_URL=http://localhost:13002 \
  cargo test --features test-fixtures --test mcp_contract --release
```

If it passes locally but fails in CI, the most likely cause is a flake
in the CI runner's docker daemon (DinD timing, image pull rate-limit).
Retry the pipeline once. If it fails twice in a row on the same MR, it's
a real bug.

### 3. Fix vs bypass

The "should I bypass this gate?" decision tree:

```
Is the failure a real bug?
├── Yes → fix it. Push a new commit. Wait for green.
└── No  → Is it a documented external outage?
         ├── Yes → wait for outage recovery, retry pipeline.
         └── No  → Is it a known flake (e.g., DinD timing)?
                  ├── Yes → retry once. If still red after 2 retries, treat as real bug.
                  └── No  → STOP. Investigate. Do not merge.
```

There is no "skip CI" button. Single-operator project means you have
to actually run through this — but you also can't blame anyone else for
red CI.

## Adding a new gate

When a new track ships a CI-eligible test suite (e.g., a future
`backend-saga-smoke` track):

1. Land the job in `.gitlab-ci.yml` with `allow_failure: true` first.
2. Watch for 1-2 weeks of normal operation; if it stays green, promote
   to required.
3. Add to the "Required-to-merge jobs" table above.
4. Update GitLab branch protection settings to list the new job.

OR: if the track had thorough local-pass acceptance before landing
(D4 pattern), promote directly. Document the reasoning in the spec.

## Deliberately-failing PR proof

To verify the gate is actually enforcing, run this drill once per
quarter (or whenever the protected-branch config rotates):

```bash
# 1. Create a throwaway branch.
git checkout -b ci-gate-drill-$(date +%Y%m%d)

# 2. Introduce a deliberate failure. E.g., for frontend-e2e:
sed -i 's/akashic-record/akashic-RECORD-typo/' frontend/tests/e2e/specs/01-anonymous-read.spec.ts

# 3. Push and open an MR.
git add frontend/tests/e2e/specs/01-anonymous-read.spec.ts
git commit -m "drill: deliberately fail frontend-e2e to verify gate enforcement"
git push -u origin ci-gate-drill-$(date +%Y%m%d)
gh mr create --title "[drill] verify frontend-e2e gate blocks merge"

# 4. Observe in GitLab UI:
#    - Pipeline runs to the `test` stage.
#    - `frontend-e2e` fails (red).
#    - Merge button is disabled with a "required pipeline check failed" message.

# 5. Close the MR without merging. Delete the branch.
git checkout master
git branch -D ci-gate-drill-$(date +%Y%m%d)

# 6. Record the drill in your private ops log:
#    YYYY-MM-DD  CI gate drill: frontend-e2e blocked merge as expected.
```

If the drill DOESN'T block, the protected-branch settings have drifted
from this doc; re-align them before any real merge.

## Related docs

- `incident-runbook.md` — class D includes CI/runner outage scenarios
- `on-call-sop.md` — alert-driven triage; CI failures don't fire alerts
  but can foreshadow real production issues
- `dsm-reverse-proxy-setup.md` — public TLS; unrelated to CI but
  referenced from README

## When CI runner itself is broken

Out of scope for this doc. The GitLab runner is operator-managed
infrastructure; if it's hung or out of disk, address it on the runner
host, not in this repo.
