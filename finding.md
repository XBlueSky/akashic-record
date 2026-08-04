# Akashic Record Production Readiness Findings

Date: 2026-04-28

## Executive Verdict

Akashic Record is **not production ready yet**.

The repository is in a strong pre-production state: the core architecture is present, the frontend and backend build successfully, CI gates are defined, REST health checks exist, and the MCP implementation is real code rather than a roadmap-only feature. However, several launch blockers remain around secrets, dependency security, MCP exposure, production configuration validation, operational hardening, and test depth.

Recommended classification:

- **Demo / local development:** acceptable.
- **Internal dogfood on a trusted network:** acceptable only if secrets are rotated and MCP is not exposed publicly.
- **Production / multi-user / exposed deployment:** not ready.

## Validation Performed

The following checks were run during the review:

| Area | Command | Result |
|---|---|---|
| Frontend production build | `npm run build` in `frontend/` | Passed |
| Frontend tests | `npm test` in `frontend/` | Passed: 2 files, 5 tests |
| Svelte / TypeScript diagnostics | `npx svelte-check --threshold error` in `frontend/` | Passed: 0 errors, 0 warnings |
| Backend tests | `cargo test` in `backend/` | Passed: 64 tests |
| Backend strict lint | `cargo clippy --all-targets -- -D warnings` in `backend/` | Passed |
| Docker Compose syntax | `docker compose config` | Parsed successfully |
| Frontend dependency audit | `npm audit --omit=dev` in `frontend/` | Failed: 6 vulnerabilities, including 1 high |
| Backend dependency audit | `cargo audit` in `backend/` | Failed: 5 vulnerabilities plus unmaintained/unsound warnings |

## What Is Already in Good Shape

### 1. Frontend build and Svelte diagnostics are clean

Evidence:

- `frontend/package.json`
- `frontend/vite.config.ts`
- `frontend/svelte.config.js`

Findings:

- `npm run build` completed successfully.
- `svelte-check --threshold error` reported 0 errors and 0 warnings.
- The app uses a Svelte 5/Vite stack and has production chunking configured for large libraries such as Three.js, D3, GSAP, and lucide-svelte.

### 2. Frontend deployment artifact exists

Evidence:

- `frontend/Dockerfile`
- `frontend/nginx.conf`

Findings:

- The frontend has a multi-stage Docker build.
- `nginx.conf` exists and serves the SPA with fallback routing.
- `/api/` and `/auth/` are reverse-proxied to `backend:8081`.
- `/api/v1/events` disables buffering for SSE.
- Static assets under `/assets/` have long-cache headers.

### 3. Backend build/test/lint gates are currently clean

Evidence:

- `backend/Cargo.toml`
- `.gitlab-ci.yml`

Findings:

- `cargo test` passed with 64 tests.
- `cargo clippy --all-targets -- -D warnings` passed.
- CI now uses Rust 1.85 and runs `cargo test`, not the previously invalid `cargo test --lib` command.

### 4. Standard REST health endpoints exist

Evidence:

- `backend/src/api/routes/health.rs`
- `backend/src/api/routes/mod.rs`
- `docker-compose.yml`

Findings:

- `/health` exists as a liveness endpoint.
- `/ready` checks PostgreSQL and Neo4j connectivity.
- `docker-compose.yml` wires backend and frontend health checks.

### 5. CORS is no longer fully permissive

Evidence:

- `backend/src/main.rs`
- `backend/src/config.rs`

Findings:

- CORS uses `FRONTEND_URL` plus `CORS_EXTRA_ORIGINS` rather than `CorsLayer::permissive()`.
- Credentials are explicitly enabled.

### 6. Sessions are persisted in PostgreSQL

Evidence:

- `backend/src/auth/store.rs`
- `backend/src/db/pg.rs`

Findings:

- Active sessions are stored in the `sessions` table.
- Expired sessions are cleaned from PostgreSQL.
- Short-lived pending OAuth artifacts remain in memory, which is acceptable for a single instance but needs consideration for multi-instance deployments.

### 7. MCP implementation exists as real backend code

Evidence:

- `backend/src/mcp/server.rs`
- `backend/src/mcp/tools.rs`
- `backend/src/mcp/types.rs`
- `backend/src/main.rs`
- `README.md`

Findings:

- MCP SSE server starts from `main.rs`.
- `backend/src/mcp/tools.rs` implements 12 tools, including `search_knowledge`, `get_details`, `save_note`, `get_project_summary`, `traverse_code_calls`, `trace_execution_flow`, `get_note_health`, `supersede_note`, `list_sagas`, `get_saga_timeline`, `global_query`, and `analyze_impact`.
- Tool schemas are typed through Rust structs and `schemars`/`rmcp` integration.

## Launch Blockers

### P0-1. Live-looking secrets are present in local environment output

Evidence:

- `docker compose config` expanded `.env` into concrete values including OpenAI-looking API keys and GitLab OAuth credentials.
- `.env` exists in the repository working tree.

Risk:

- The observed values should be treated as compromised if they are real.
- Production readiness requires clean secret management and verified non-committed local secrets.

Required action:

- Rotate all exposed OpenAI/LLM/embedding/GitLab credentials.
- Ensure `.env` is not committed or distributed.
- Use deployment secret stores instead of local plaintext files for production.

### P0-2. Docker Compose can resolve to container-broken service URLs

Evidence:

- `docker-compose.yml` uses `env_file: .env` for backend.
- `docker compose config` showed `DATABASE_URL=postgres://...@localhost:5433/...` and `NEO4J_URI=bolt://localhost:7687` from the current local `.env`.

Risk:

- Inside the backend container, `localhost` points to the backend container itself, not the PostgreSQL or Neo4j containers.
- A production compose deployment may fail to connect to databases unless `.env` is corrected.

Required action:

- For compose deployments, set:
  - `DATABASE_URL=postgres://${USER}:${DB_PASSWORD}@postgres:5432/<db>`
  - `NEO4J_URI=bolt://neo4j:7687`
- Add a production `.env.example` note that container networking must use service names.
- Consider validating these values at startup and warning when `localhost` is used inside container mode.

### P0-3. MCP write-capable tools appear unauthenticated

Evidence:

- `backend/src/mcp/server.rs` starts `SseServer::serve(addr)` and attaches `AkashicMcp` directly through `with_service`.
- `backend/src/mcp/tools.rs` exposes write-capable tools such as `save_note` and `supersede_note`.
- REST protected routes use auth middleware, but equivalent MCP auth was not observed.

Risk:

- Anyone able to reach the MCP SSE port may be able to read or mutate knowledge records.
- This is the largest blocker for any exposed production deployment.

Required action:

- Add MCP-level authentication and authorization.
- Require bearer/session validation for write tools.
- Consider making read-only MCP mode the default unless explicitly configured.
- Do not expose port 8080 outside a trusted environment until this is fixed.

### P0-4. Dependency security audits fail

Evidence:

- `npm audit --omit=dev` reported 6 vulnerabilities:
  - `lodash`: high severity code injection / prototype pollution advisories.
  - `dompurify`: multiple moderate advisories, including XSS-related bypasses.
  - `svelte`: moderate XSS advisories.
  - `devalue`: moderate prototype pollution advisories.
  - `esbuild` via `svelte-i18n`: moderate dev-server advisory.
- `cargo audit` reported 5 vulnerabilities:
  - `rustls-webpki` advisories including certificate validation / CRL parsing issues.
  - `rsa` timing side-channel advisory.
  - Additional unmaintained/unsound warnings involving transitive dependencies such as `git2`, `rand`, `backoff`, `paste`, and others.

Risk:

- Frontend includes XSS-relevant dependencies such as DOMPurify/Svelte.
- Backend uses TLS/HTTP/Git-related crates in ingestion and provider integrations.
- Production deployment should not launch with known unresolved advisories unless there is a documented risk acceptance.

Required action:

- Run dependency upgrades and re-run audits.
- Where no fix exists, document risk acceptance and exposure analysis.
- Add dependency audit checks to CI once resolved.

### P0-5. Insecure default database credentials exist in compose and examples

Evidence:

- `docker-compose.yml` defaults Neo4j and PostgreSQL credentials to `akashic_secret`.
- `.env.example` contains `NEO4J_PASSWORD=akashic_secret` and a PostgreSQL URL using `akashic_secret`.
- `README.md` documents `NEO4J_PASSWORD` default as `akashic_secret`.

Risk:

- Operators can accidentally deploy with public/default credentials.
- Neo4j and PostgreSQL ports are published by default in `docker-compose.yml`.

Required action:

- Remove insecure production defaults or clearly mark them development-only.
- Require explicit passwords for production profile.
- Avoid publishing database ports in production compose configurations.

### P0-6. GitLab OAuth production config is not strictly validated

Evidence:

- `backend/src/config.rs` defaults `GITLAB_APP_ID` and `GITLAB_APP_SECRET` to empty strings.
- `GITLAB_URL` defaults to `https://gitlab.example.com`.
- `.env.example` leaves GitLab OAuth credentials empty.

Risk:

- Authentication failures may occur only at runtime.
- Placeholder config can accidentally reach deployment.

Required action:

- Validate GitLab OAuth config at startup when auth routes or protected write operations are enabled.
- Fail fast for empty `GITLAB_APP_ID`, empty `GITLAB_APP_SECRET`, or placeholder `GITLAB_URL` in production mode.

## High-Priority Non-Blocking Findings

### P1-1. Test coverage is too shallow for production confidence

Evidence:

- Frontend test suite currently has 2 test files and 5 tests.
- Backend has 64 tests, mostly unit/verification tests around internal logic.
- MCP-specific tests were not found.

Risk:

- Core user flows such as login, ingestion, graph browsing, note editing, saga timeline, and MCP tool behavior are not covered deeply enough.

Recommended action:

- Add frontend integration/e2e tests for auth, repo browsing, graph views, ingestion forms, note editing, and SSE progress.
- Add backend API integration tests with test PostgreSQL/Neo4j fixtures.
- Add MCP tool tests for all 12 tools, especially write tools.

### P1-2. MCP dependency maturity is uncertain

Evidence:

- `backend/Cargo.toml` uses `rmcp = { version = "0.1", ... }`.

Risk:

- Early-version protocol/server dependencies may change behavior or carry unreviewed edge cases.

Recommended action:

- Review `rmcp` release maturity and upgrade if a stable version exists.
- Add protocol compliance tests for MCP request/response behavior.

### P1-3. MCP has limited operational visibility

Evidence:

- REST API uses `TraceLayer` in `backend/src/main.rs`.
- MCP server logs startup, but equivalent request/response logging and metrics were not observed in `backend/src/mcp/server.rs`.

Risk:

- Operators cannot easily trace MCP tool usage, latency, failures, or abuse.

Recommended action:

- Add structured logging for MCP tool calls.
- Add metrics for tool count, latency, success/failure, and caller identity after auth is implemented.
- Add an MCP-specific health signal or include MCP readiness in `/ready`.

### P1-4. Backend lacks some standard operational hardening

Evidence:

- `backend/src/main.rs` binds and serves with `axum::serve(listener, app).await?`.
- No explicit graceful shutdown handling was observed.
- No rate limiting middleware was observed.
- `backend/Dockerfile` runs the binary in a slim Debian image without a non-root user.

Risk:

- Shutdowns may interrupt in-flight requests and ingestion jobs.
- Auth and ingestion endpoints may be easier to abuse.
- Container compromise impact is higher when running as root.

Recommended action:

- Add graceful shutdown with signal handling.
- Add rate limits for auth, ingestion, GraphRAG, and MCP endpoints.
- Run backend container as a non-root user.
- Add metrics and alerting for readiness, job failures, and provider errors.

### P1-5. Runtime migrations and startup schema changes need production discipline

Evidence:

- `backend/src/main.rs` initializes PostgreSQL and Neo4j schemas on startup.
- It also runs embedding precision migration and logs website sources needing re-ingestion for Doc Space migration.

Risk:

- Runtime migrations are convenient for development but risky for production rollouts.
- Data migration warnings indicate that existing website sources may need manual re-ingestion.

Recommended action:

- Separate schema migrations from normal app startup for production.
- Add migration planning, backup guidance, and rollback procedures.
- Resolve or document website source re-ingestion requirements before launch.

### P1-6. Production config surface needs clearer separation from development

Evidence:

- `docker-compose.yml` publishes Neo4j, PostgreSQL, MCP SSE, REST API, and frontend ports by default.
- `.env.example` mixes local defaults and production-sensitive values.

Risk:

- A user may deploy the development compose file as-is.
- Internal services may be exposed unnecessarily.

Recommended action:

- Provide separate `docker-compose.dev.yml` and `docker-compose.prod.yml` or profiles.
- In production, avoid publishing database ports and restrict MCP exposure.
- Document required reverse proxy/TLS expectations.

## Medium-Priority Findings

### P2-1. Frontend bundle remains large but acceptable for current scope

Evidence:

- Production build output included separate large chunks for Three.js, D3, GSAP, lucide-svelte, and the main app.
- `frontend/vite.config.ts` manually chunks these dependencies and raises chunk warning limit to 800 KB.

Risk:

- Initial load may be heavier than desired, especially for slower clients.

Recommended action:

- Consider dynamic-importing landing/3D graph features.
- Measure real loading performance before broad deployment.

### P2-2. README is comprehensive but should document production caveats

Evidence:

- `README.md` documents architecture, service ports, MCP tools, REST routes, and configuration.

Risk:

- The README currently presents a smooth quick start but does not strongly distinguish development from production.

Recommended action:

- Add a production hardening section covering secrets, TLS, database exposure, MCP auth, dependency audits, and backup/restore.

### P2-3. Auth cookie logout clearing is less complete than login cookie attributes

Evidence:

- `backend/src/auth/web.rs` login cookie includes `HttpOnly`, `SameSite=Lax`, `Path=/`, `Max-Age`, optional `Secure`, and `Domain`.
- Logout clear cookie uses `ak_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0` without matching `Secure` or `Domain`.

Risk:

- Depending on browser/domain behavior, logout may fail to clear the exact cookie set during login.

Recommended action:

- Match cookie attributes when clearing the session cookie.

## Area-by-Area Readiness Summary

| Area | Status | Summary |
|---|---|---|
| Frontend | Pre-production | Build/typecheck clean, Docker/nginx present, but dependency audit and test depth block production confidence. |
| Backend REST API | Pre-production | Tests/lints pass, health/readiness exist, but dependency audit, config validation, runtime migrations, and operational hardening remain. |
| Auth | Pre-production | GitLab OAuth/session persistence exists, but production config validation and cookie cleanup should be hardened. |
| Data layer | Pre-production | PostgreSQL/Neo4j schema initialization exists, but production migration discipline and compose networking/config need hardening. |
| MCP | Not production-ready | Implementation exists with 12 tools, but write-capable unauthenticated surface, missing MCP tests, and weak observability are blockers. |
| Deployment | Not production-ready as-is | Compose and Dockerfiles exist, but dev defaults, secret handling, exposed ports, and non-root hardening need work. |
| CI / quality gates | Improving | Current build/test/lint gates pass, but dependency audit and integration/e2e coverage should be added. |

## Minimum Required Before Production

1. Rotate all exposed or live-looking credentials.
2. Fix compose/runtime configuration to use container service names for PostgreSQL and Neo4j.
3. Add authentication/authorization for MCP, or do not expose MCP in production.
4. Resolve or explicitly risk-accept all `npm audit` and `cargo audit` findings.
5. Remove insecure default database passwords from production paths.
6. Validate GitLab OAuth and provider configuration at startup in production mode.
7. Add MCP tool tests and critical frontend/backend integration tests.
8. Add graceful shutdown, rate limiting, metrics, and non-root backend container execution.
9. Separate development and production compose profiles/configuration.
10. Document production deployment requirements, including TLS, secrets, database exposure, MCP exposure, backups, and migrations.

## Final Recommendation

Do **not** deploy this project as a production service yet.

It is suitable for continued development, demos, and controlled internal dogfooding after secrets are rotated and MCP exposure is restricted. The fastest path to production is to prioritize MCP auth, dependency audit remediation, production configuration validation, and operational hardening before expanding feature work.
