# Frontend e2e — Operator Runbook

D3's Playwright harness exercises three golden paths against an
isolated test stack defined by `docker-compose.test.yml`. This
document covers the local + CI workflows and the threat model behind
the `test-fixtures` cargo feature.

## TL;DR

```bash
# From the repo root:
docker compose -f docker-compose.test.yml up -d --wait
cd frontend && npm run e2e
docker compose -f docker-compose.test.yml down -v
```

## Stack lifecycle

The compose file brings up five services on dev+50000 ports so it can
coexist with `docker-compose.yml`:

| Service        | Port  | Purpose |
|----------------|-------|---------|
| postgres-test  | 55432 | PG + pgvector, tmpfs storage |
| neo4j-test     | 57474/57687 | Neo4j 5 community, tmpfs storage |
| gitlab-mock    | (internal) | nginx serving canned GitLab + OpenAI responses |
| backend-test   | 13001 | Backend built with `--features test-fixtures`; aliased as `backend` in the compose network |
| frontend-test  | 18080 | nginx + Vite-built assets |

- `up -d --wait` blocks until all healthchecks pass (~10s warm, ~5 min cold).
- `down -v` removes containers + tmpfs volumes; the next `up` starts
  from a blank schema.

## Running locally

The npm scripts wrap Playwright in the official `mcr.microsoft.com/playwright:v1.62.1-jammy`
Docker image. This makes the harness work on hosts where Playwright
itself doesn't (e.g., Ubuntu 18.04 hosts that Playwright 1.60 no
longer supports).

**Keep the pairing in sync.** The `@playwright/test` version in
`frontend/package.json` and this Docker image's tag must move
together — the image ships a matching `chromium_headless_shell` build,
and `playwright test` refuses to launch against a mismatched one
("Please update docker image as well"). `@playwright/test` is pinned
exact (no `^`) for this reason: a caret range lets `npm install`
silently drift the installed version ahead of whatever tag is
hardcoded in the `e2e`/`e2e:headed`/`e2e:debug` scripts below. When
bumping one, bump the other in the same commit.

```bash
# Default — all three golden paths, headless:
npm run e2e

# Single spec:
npm run e2e -- 02-authenticated-edit.spec.ts

# Headed (browser visible — requires X11 forwarding):
npm run e2e:headed

# Playwright Inspector (step-through):
npm run e2e:debug
```

**Ubuntu 18.04 host caveat:** `npm run e2e:install` runs `playwright
install --with-deps chromium` directly on the host, which fails on
Ubuntu 18.04 (Playwright 1.60 dropped support). On 18.04 hosts, use
`npm run e2e` instead — the Docker wrap delegates to a jammy-based
Playwright image with browsers pre-installed. Newer hosts (Ubuntu
20.04+, 22.04, 24.04) can use `npm run e2e:install` then a host-local
Playwright invocation, but the Docker-wrap path also works there.

## Verifying both cookie_secure branches (AC-5)

Path C's default run exercises `cookie_secure=false` (matches
`docker-compose.test.yml`'s `COOKIE_SECURE: "false"`). To exercise the
`cookie_secure=true` branch:

```bash
# 1. Restart backend with override:
docker compose -f docker-compose.test.yml stop backend-test
COOKIE_SECURE=true docker compose -f docker-compose.test.yml run -d --rm \
  --service-ports --name akashic-cookie-secure-true \
  -e COOKIE_SECURE=true backend-test

# 2. Wait for /health to return 200, then run Path C only:
until curl -m 2 -sf http://localhost:13001/health >/dev/null; do sleep 2; done
cd frontend
docker run --rm --network host -e E2E_EXPECTED_SECURE=true \
  -v "$(pwd)":/work -w /work \
  mcr.microsoft.com/playwright:v1.62.1-jammy \
  bash -c 'PLAYWRIGHT_BASE_URL=http://localhost:18080 \
    npx playwright test 03-logout-cookie-parity.spec.ts'

# 3. Restore default backend:
docker stop akashic-cookie-secure-true
docker compose -f docker-compose.test.yml up -d --no-deps backend-test --wait
```

The `E2E_EXPECTED_SECURE=true` env var tells the parity helper to
assert the `Secure` attribute is present on both login and logout
Set-Cookie headers.

## Traces and reports

After a failing run:

- `frontend/playwright-report/` — HTML report (open `index.html`).
- `frontend/test-results/<spec>/trace.zip` — open via
  `npx playwright show-trace frontend/test-results/.../trace.zip`.
- `frontend/test-results/<spec>/video.webm` — video recording.

## CI failure triage

The GitLab `frontend-e2e` job uploads `playwright-report/`,
`test-results/`, and `compose-logs.txt` as artifacts on failure. To
reproduce locally:

1. Fetch the failing pipeline's artifact bundle.
2. Open `playwright-report/index.html` in a browser.
3. For backend-side context, scan `compose-logs.txt` —
   `backend-test` logs include the live `resolve_gitlab_access` calls
   and any `403 Forbidden` paths.

Common failure modes and fixes:

- **Path B fails with 403 on PUT** → `gitlab-mock` not reachable from
  `backend-test`. Check the `GITLAB_URL` env var in compose.test.yml.
- **Path A times out on graph mount** → `neo4j-test` not seeded, OR
  `globalSetup` didn't run. Check Playwright `globalSetup` log for
  errors; verify `/test/fixtures/repo` returned 200.
- **Path C parity assertion fires** → D8's `build_session_cookie`
  helper may have been edited; logout no longer uses it. Re-confirm
  with `grep -n build_session_cookie backend/src/auth/web.rs`.
- **Rate-limit 429 in combined runs** → ensure `RATE_LIMIT_ENABLED=false`
  is set on `backend-test` (it is by default in `docker-compose.test.yml`).

## `test-fixtures` feature gate — threat model

The fixture endpoints (`/test/fixtures/{session,note,repo}`) are a
deliberately powerful surface — they mint sessions WITHOUT any OAuth
validation. To make accidental exposure structurally impossible:

- The module file `backend/src/api/routes/test_fixtures.rs` is gated
  with `#![cfg(feature = "test-fixtures")]`. The release Docker image
  builds with NO features (default), so the file is not compiled and
  its symbols are not present in the binary.
- `docker-compose.test.yml` is the ONLY production artifact that
  passes `--build-arg CARGO_FEATURES=test-fixtures`.
- `docker-compose.yml` (dev) and `docker-compose.prod.yml` do NOT
  pass the build-arg.

**Verification:** a release binary should not contain any symbol with
the substring `test_fixtures`. To check:

```bash
cd backend
cargo build --release  # default features only
nm target/release/akashic-record | grep -c test_fixtures
# Expected output: 0
```

A non-zero count means either the feature was accidentally enabled
in the release Dockerfile, or the gate is leaking. Either is a
deployment-blocking finding.

## Adding a new e2e spec

1. If a new fixture surface is needed, add an endpoint to
   `backend/src/api/routes/test_fixtures.rs` and an integration test
   in `backend/tests/test_fixtures.rs`. Run `cargo test --features
   test-fixtures` (with `TEST_DATABASE_URL` / `TEST_NEO4J_URI` env
   vars pointing at the persistent stack — see `backend/tests/common/mod.rs`'s
   `acquire_endpoints`) to confirm.
2. Add a typed wrapper to `frontend/tests/e2e/fixtures/seed.ts`.
3. Create `frontend/tests/e2e/specs/0N-<name>.spec.ts`.
4. Run `npm run e2e:headed -- 0N-<name>.spec.ts` to iterate on
   selectors (requires X11).
5. Final headless run; commit.

Per-spec wall-clock should stay under ~10s warm. If a spec needs
more, consider whether the harness is doing too much.
