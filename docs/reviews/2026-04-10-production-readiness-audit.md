# Akashic Record — Production Readiness Audit

Date: 2026-04-10

## Executive Summary

**Verdict:** This repository is **not production ready yet**.

It is better described as a **high-completion prototype / pre-production system**:

- The **product direction is strong and coherent**.
- The **system architecture is mostly correct** for the problem it is trying to solve.
- The **engineering and operational posture is not yet mature enough** for confident production deployment.

### Scorecard

| Dimension | Score | Notes |
|---|---:|---|
| Product direction | 8/10 | Clear value proposition and good scope selection |
| Architecture direction | 7.5/10 | Good major boundaries, but still uneven in hardening |
| Engineering quality | 5.5/10 | Real functionality exists, but quality gates are inconsistent |
| Production readiness | 4.5/10 | Several deployment, security, CI, and ops gaps remain |

## What This Repo Gets Right

The repository is not a toy. It has a real, deliberate system shape:

- **Frontend**: Svelte 5 + Vite viewer and ingestion UI
- **Backend**: Rust + axum REST API and MCP SSE server
- **Data layer**:
  - PostgreSQL + pgvector for content, embeddings, and relational data
  - Neo4j for graph relationships and topology
- **Integration layer**:
  - GitLab OAuth
  - GitLab webhook ingestion/sync
  - Repository / website ingestion pipeline
- **Knowledge layer**:
  - notes
  - sagas
  - graphRAG / search
  - module and chunk graph exploration

This is the right overall direction for a “knowledge about code” platform. The repo shows clear intent, real product thinking, and meaningful internal module boundaries.

## Architecture Assessment

## Overall Direction: **Correct**

The high-level split is sensible:

- **UI / exploration / ingestion controls** live in the frontend
- **API + auth + ingestion orchestration + MCP** live in the backend
- **Graph and relational concerns** are intentionally separated across Neo4j and PostgreSQL

That is a reasonable architecture for this product.

## Why The Direction Is Good

1. **The domain model is coherent**
   - repo / branch / module / chunk / note / saga all fit the product goal.

2. **The data shape matches the use case**
   - PostgreSQL handles structured content and embeddings.
   - Neo4j handles topology and graph traversal.

3. **The backend responsibilities are ambitious but understandable**
   - REST API
   - MCP tools
   - ingestion
   - auth
   - graph analysis

4. **The frontend is product-oriented, not just decorative**
   - repo browsing
   - graph views
   - modules/chunks
   - sagas
   - ingestion flows

## Where The Architecture Is Still Uneven

1. **Backend scope is becoming very broad**
   - `backend/src/api/routes.rs` is extremely large.
   - The backend currently carries many concerns in one service and some very large modules.
   - This is survivable for pre-production, but it will become a maintainability issue if left as-is.

2. **Frontend app entry is still centralized**
   - `frontend/src/App.svelte` coordinates many top-level view transitions and state decisions.
   - This is workable now, but not ideal for long-term growth.

3. **Runtime migrations are tightly coupled to startup**
   - The app performs schema initialization / evolution at runtime.
   - Good for rapid iteration; risky for production change management.

4. **Operational concepts are mixed with domain concepts**
   - The project includes note “health” logic, which is useful domain health.
   - But it lacks standard service health/readiness concepts expected in deployment environments.

## Production Readiness Assessment

## Current Verdict: **Not Ready**

The repo demonstrates that the product can likely be developed and demoed effectively, but several issues prevent calling it production ready.

### Key reasons:

- quality gates are inconsistent
- deployment artifacts are incomplete / drifted
- security posture is not hardened enough
- ops readiness is missing standard controls
- docs, CI, and implementation are not fully aligned

## Evidence Collected

### Frontend verification

Commands run:

```bash
cd frontend
npm test
npx svelte-check --threshold error
npm run build
```

Observed results:

- `npm test` → **passed**
  - 2 test files
  - 5 tests total
- `svelte-check --threshold error` → **0 errors, 19 warnings**
- `npm run build` → **passed**, but with notable warnings:
  - unused CSS selectors
  - non-reactive update warnings
  - state referenced locally warnings
  - accessibility warning (`tabIndex` on noninteractive element)
  - chunk size warning

Bundle output from production build:

- main JS bundle: **~1.56 MB** before gzip
- gzip: **~424 KB**

Interpretation:

- The frontend is functional.
- It is **not yet in a clean release posture**.
- The current bundle and warning profile suggest ongoing iteration, not production stabilization.

### Backend verification

Commands run:

```bash
cd backend
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo test --lib
```

Observed results:

- `cargo build` → **passed**
- `cargo test` → **passed**
  - 64 tests passed
- `cargo clippy --all-targets -- -D warnings` → **failed** with **60+ lint violations**
- `cargo test --lib` → **failed** because this package has **no library target**

Interpretation:

- The backend can compile and its test suite does have meaningful coverage in some areas.
- However, the repo’s own stricter quality gate (`clippy -D warnings`) does not pass.
- CI is currently configured in a way that does not match the crate layout.

## Major Gaps Blocking Production Readiness

## 1. CI / Quality Gate Drift

### Findings

- `.gitlab-ci.yml` runs:

```yaml
- cargo clippy --all-targets -- -D warnings
- cargo test --lib
```

- Current repo state:
  - `cargo clippy --all-targets -- -D warnings` fails badly
  - `cargo test --lib` is invalid for this crate

### Why this matters

If CI is misconfigured or permanently red, then “main is healthy” becomes meaningless. That alone is enough to reject a production-ready label.

## 2. Deployment Artifact Incompleteness

### Findings

- `frontend/Dockerfile` contains:

```dockerfile
COPY nginx.conf /etc/nginx/conf.d/default.conf
```

- But no `frontend/nginx.conf` exists in the repository.

### Why this matters

This is a real deployment breakage, not a theoretical concern. Production readiness requires the deployment path to be complete and reproducible.

## 3. Security Posture Is Not Hardened Yet

### Findings

- Backend uses:

```rust
.layer(CorsLayer::permissive())
```

- Session/auth state is stored in-memory:
  - `DashMap<String, Session>`
  - `DashMap<String, PendingCode>`
  - `DashMap<String, PendingState>`

- Auth cookie is set with:
  - `HttpOnly`
  - `SameSite=Lax`
  - `Path=/`
  - `Domain=...`

- No `Secure` attribute is visible in the cookie construction.

### Why this matters

For production systems, these are material issues:

- permissive CORS is too broad
- in-memory sessions are fragile across restarts and multi-instance deployment
- missing `Secure` cookie posture is risky for real deployments behind HTTPS

## 4. Missing Standard Service Health / Readiness Endpoints

### Findings

- The repo contains **note health** logic (`backend/src/notes/health.rs`), which is domain-specific.
- No standard `/health`, `/ready`, `/live`, `/healthz`, or `/readyz` route was found.

### Why this matters

Production systems need platform health endpoints for orchestration, rollout checks, monitoring, and incident response.

## 5. Docs / API / Implementation Drift

### Findings

- README still documents old `/contexts` endpoints.
- Actual backend routes are centered around `/notes`, `/modules`, `/chunks`, `/sagas`, etc.
- README says “This starts all 3 services” while `docker-compose.yml` clearly defines **4** services:
  - neo4j
  - postgres
  - backend
  - frontend

### Why this matters

Documentation drift is one of the clearest signs that a codebase is still moving fast and has not been stabilized for operators or external contributors.

## 6. Frontend Release Hygiene Still Needs Work

### Findings

- `svelte-check` returns 19 warnings
- production build reports:
  - accessibility warning(s)
  - state/reactivity warnings
  - unused selectors
  - oversized chunk warning

### Why this matters

Warnings do not automatically block production, but this many warnings usually mean the UI layer is still in active assembly rather than release hardening.

## 7. Maintainability Pressure Is Building

### Findings

- `backend/src/api/routes.rs` is very large
- several backend clippy failures are type-complexity / too-many-arguments / maintainability smells
- top-level frontend orchestration is still fairly centralized in `App.svelte`

### Why this matters

This does not block a small deployment today, but it is a strong signal that the codebase has not yet entered a stable maintainability phase.

## Strengths Worth Preserving

These parts should be considered core assets, not rewritten casually:

1. **Product idea and scope selection** are strong.
2. **Rust backend + graph/relational split** is a good fit here.
3. **Ingestion + graph exploration + notes/sagas** creates a differentiated product story.
4. **The backend already has real tests**, not just placeholders.
5. **The UI is ambitious and product-shaped**, not a thin admin shell.

## Is The Architecture And Direction “Right”? 

## Yes — but not yet “complete”

The architecture and direction are **right enough to continue investing in**.

But I would **not** say they are fully complete yet.

### What is already right

- core service split
- data model intent
- GitLab integration concept
- graph + notes + ingestion as product pillars

### What is still incomplete

- production hardening layer
- standardized ops layer
- deployment correctness and reproducibility
- auth/session durability strategy
- documentation/CI alignment
- release hygiene and frontend cleanup

So the right phrasing is:

> **The direction is correct, but the project is still in pre-production maturation rather than true production completion.**

## Priority Gap List

## P0 — Must Fix Before Calling It Production Ready

1. Fix CI so it matches reality
   - remove or replace invalid `cargo test --lib`
   - make clippy either pass or intentionally scope/enforce it correctly

2. Fix frontend deployment artifact gap
   - add the missing nginx config or remove that dependency from Dockerfile

3. Replace / harden auth session strategy
   - move away from purely in-memory session storage if production deployment is expected

4. Replace permissive CORS with explicit allowed origins / methods / headers

5. Add standard service health/readiness endpoints

6. Align README with actual routes and service count

## P1 — Should Fix Soon After P0

1. Clean up frontend warnings from `svelte-check` and build output
2. Reduce frontend bundle size / split major chunks
3. Add integration tests around auth, ingestion, and key API flows
4. Separate oversized backend/API modules for maintainability

## P2 — Important But Can Follow Stabilization

1. Improve observability beyond logs
   - metrics
   - structured operational dashboards
   - error/event aggregation

2. Improve deployment documentation
   - production env vars
   - reverse proxy / TLS expectations
   - backup / restore guidance

3. Define scaling and persistence assumptions explicitly
   - single-node vs multi-instance
   - auth/session strategy
   - ingestion concurrency constraints

## Final Verdict

## Short answer

- **Production ready?** → **No**
- **Architecture and direction correct?** → **Yes, mostly**
- **Architecture and direction complete?** → **Not yet**

## Final framing

Akashic Record already looks like a **serious pre-production system with strong product instincts**.

It does **not** look like a disposable experiment.

But it also does **not yet** demonstrate the operational discipline, deployment completeness, CI alignment, and security hardening required to honestly call it production ready.

The good news is that the missing work is mostly **hardening and maturation**, not “the whole thing is headed the wrong way.”
