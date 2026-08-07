# Akashic Record

A self-hosted knowledge-about-code platform that bridges **GitLab repositories** with **AI-generated context**. It ingests code, builds a graph of modules, chunks, and call edges, and lets you save architectural decisions, bug fix rationale, and configuration notes anchored to specific Git branches. Claude Code queries and writes knowledge through a native MCP interface.

## Architecture

| Service | Tech | Port | Role |
|---------|------|------|------|
| **backend** | Rust (axum + rmcp) | 8081 (REST API + MCP at `/mcp`) | MCP server, ingestion pipeline, GitLab OAuth, REST API |
| **frontend** | Svelte 5 + Vite | 3000 | Knowledge viewer with constellation graph, search, modules, sagas |
| **postgresql** | PostgreSQL 16 + pgvector | 5432 | Content, embeddings, full-text search (BM25 + vector) |
| **neo4j** | Neo4j 5.x Community | 7474 (HTTP), 7687 (Bolt) | Graph topology (nodes + relationships, CALLS, EXPLAINS, PART_OF) |

## Features

- **MCP Interface** — Claude Code reads/writes knowledge via 27 tools (`search_knowledge`, `save_note`, `get_details`, and more) at a single `/mcp` endpoint. Every tool call requires a Bearer token; see [Authenticating MCP requests](#authenticating-mcp-requests)
- **Code Ingestion** — Parses repositories into Module/Chunk nodes with CALLS edges and confidence scores
- **Dual-Layer Search** — BM25 keyword + vector semantic search across code, docs, and knowledge notes (GraphRAG)
- **Sagas** — Group related notes into narrative threads keyed by issue ref, branch, or topic
- **Call Graph Traversal** — Upstream/downstream CALLS edge traversal with confidence filtering
- **Impact Analysis** — Blast radius scoring across CALLS, IMPORTS_FROM, EXPLAINS, and execution flows
- **Community Detection** — Leiden algorithm for high-level architecture summaries
- **GitLab OAuth** — Session-based authentication; protected write routes require a GitLab token
- **Configurable Embeddings** — Local (candle) or remote (OpenAI-compatible API)
- **Health Endpoints** — `/health` (liveness) and `/ready` (readiness with DB checks)

## Quick Start

### Prerequisites

- Docker and Docker Compose
- A GitLab instance (for OAuth and repository ingestion)

### 1. Configure

```bash
cp .env.example .env
# Edit .env: set Neo4j/PostgreSQL passwords, GitLab OAuth credentials, embedding provider
```

### 2. Start

```bash
docker-compose up -d
```

This starts all 4 services. On first run the backend will:
- Connect to Neo4j and PostgreSQL, initialize schema (constraints, indexes, seed categories)
- Download the embedding model if `EMBEDDING_PROVIDER=local`
- Start the single backend listener on `:8081`, serving both the REST API and the MCP endpoint at `/mcp` (streamable HTTP, MCP protocol revision 2026-07-28; a legacy sessionless `initialize` handshake is still accepted for older clients)

### 3. Connect Claude Code

There is a single MCP endpoint, `/mcp`, on the same port as the REST API — streamable HTTP with JSON responses, not SSE. Every one of the 27 MCP tools requires a Bearer token; anonymous requests (including `tools/list`) get an HTTP 401 with an RFC 9728 `WWW-Authenticate` challenge. See [Authenticating MCP requests](#authenticating-mcp-requests) below for how to obtain a token.

Add to your `.mcp.json`:

```json
{
  "mcpServers": {
    "akashic-record": {
      "type": "http",
      "url": "https://<host>/mcp",
      "headers": {
        "Authorization": "Bearer ak_..."
      }
    }
  }
}
```

For local development, `url` is `http://localhost:8081/mcp` instead. A `glpat-...` GitLab Personal Access Token also works as the bearer value in place of an `ak_...` token.

### 4. Authenticate

This is a *separate* credential from the MCP bearer token in step 3 —
it's a web/REST session for browsing the app and calling protected
`/api/v1/...` routes directly, not an `ak_...` MCP token.

1. Open `http://localhost:8081/auth/login` in your browser
2. Authorize with GitLab
3. Copy the session token from the redirect
4. Use it as a `Bearer` token or `ak_session` cookie for protected REST endpoints

### 5. Browse

Open `http://localhost:3000` to view the constellation graph, module explorer, and saga timeline.

## Production Deployment

For production, use **`docker-compose.prod.yml`**, not the default
`docker-compose.yml` (which is for development).

```bash
docker compose -f docker-compose.prod.yml \
  --env-file /etc/akashic/akashic.env up -d
```

The `--env-file` flag is **required**. Compose has two distinct
env-loading layers: a shell-substitution layer that expands `${VAR}`
inside the compose file itself, and a per-container env layer
populated by service-level `env_file:` directives. The
shell-substitution layer is fed only by the host shell and the
top-level `--env-file` flag, so omitting `--env-file` (or relying on
the working-directory `.env` discovery) leaves required variables like
`${NEO4J_USER}` and `${PG_PASSWORD}` unset and compose refuses to
start.

The production compose file:

- **Sources every secret from `/etc/akashic/akashic.env`** (mode `0600`,
  `root:root`). Create that file before running compose. The full
  rotation procedure and the file-mode contract live in
  [`docs/operations/rotation-sop.md`](docs/operations/rotation-sop.md).
- **Publishes only the frontend on `127.0.0.1:3000:80`** (loopback host
  endpoint `127.0.0.1:3000` reaching the frontend container's port
  80). Database and backend ports are *not* published; they are
  reachable only inside the compose network. TLS termination and
  public exposure are an external responsibility — see
  [Public exposure](#public-exposure) below.
- **Publishes no separate MCP port — there is only one backend port,
  8081, and it is not published either.** MCP shares the REST API's
  listener at `/mcp`; external MCP client access goes through the
  frontend nginx's `/mcp` (+ `/oauth/` + `/.well-known/`) forwarding
  rules, the same public hostname as the web UI. Safety does not rest
  on non-exposure: every `/mcp` request is authenticated end-to-end by
  the `mcp_auth` layer (Bearer-only — `ak_*` device-flow/OAuth token or
  `glpat-*` GitLab PAT — fail-closed 401 for anything else). See
  [Authenticating MCP requests](#authenticating-mcp-requests).
- **Has no insecure built-in defaults.** Every credential
  must be present in `/etc/akashic/akashic.env`.

### Public exposure

Production deployments need an external reverse proxy in front of
`127.0.0.1:3000` to handle TLS termination, public DNS, and
certificate management. Two paths are supported:

Run a reverse proxy (caddy, nginx + certbot, traefik, or your host
platform's built-in one) and point its upstream at `127.0.0.1:3000`,
with TLS certificates issued and renewing automatically. The repo does
not currently ship a turnkey proxy overlay.

Note that the frontend nginx already denies public access to
`/api/v1/metrics` (the Prometheus scrape endpoint). Internal scrapers
running on the same compose network reach `backend:8081/api/v1/metrics`
directly, bypassing nginx.

`frontend/nginx.conf` also forwards `/mcp`, `/oauth/`, and
`/.well-known/` to `backend:8081` — this is what makes the MCP
endpoint and its OAuth/CIMD flow reachable at the same public hostname
as the web UI. The `/mcp` location forwards the `Host` header intact
(`proxy_set_header Host $host`), which the backend's rmcp
`allowed_hosts` guard validates against `PUBLIC_BASE_URL`.

### Dev vs prod compose

| File | Purpose | Publishes | Source of secrets |
|---|---|---|---|
| `docker-compose.yml` | Local development | DB ports (7474, 7687, 5433), backend port (8081) for native `cargo run` workflows | `.env` (or placeholder defaults) |
| `docker-compose.prod.yml` | Production deployments | Only `127.0.0.1:3000` (frontend) | `/etc/akashic/akashic.env` (mode 0600, root:root) |

The two files are intentionally fully separate. Do not mix flags
across them. The prod file fails fast when required env vars are
missing; the dev file falls back to placeholders so a fresh clone
runs out of the box.

Set `AKASHIC_ENV=production` in `/etc/akashic/akashic.env` so the backend
rejects placeholder credentials (`changeme`, `akashic_secret`,
`https://gitlab.example.com`, `sk-test`, etc.), empty OAuth fields, and
container-broken `localhost` DSNs at startup. Without this variable, the
backend boots in development-permissive mode even when invoked via the
production compose file. Validation prints every offending field at once
to stderr and exits with status `2`.

For local development on a fresh clone, the default `docker-compose.yml`
still works without a `.env` file (it falls back to `changeme`
placeholder credentials and publishes DB ports for native `cargo run`
workflows). Do not deploy the development compose to a public host.

### Hardening posture summary

| Concern | Status | Where it lives |
|---|---|---|
| Secrets discipline | `/etc/akashic/akashic.env` mode 0600, root:root; A1 spec enforces | [`rotation-sop.md`](docs/operations/rotation-sop.md) |
| TLS / public exposure | Operator-run reverse proxy in front of `127.0.0.1:3000` | § Public exposure above |
| Container hardening | Non-root user UID 10001, read-only `/etc`, ephemeral `/tmp/akashic-ingest` (C4) | C4 spec |
| Auth gating on writes | OAuth device flow + cookie passthrough; anonymous writes 401 (B1) | B1 spec + D4 contract tests |
| Audit logging | Every write tool emits an `audit_log` row (B2; fix `00f0439`) | B2 spec |
| Graceful shutdown | `drain_with_cap(60s)` after SIGTERM; auto-restart via the container runtime (C2; fix `e1d0b43`) | C2 spec |
| Migration discipline | `migrate verify/up` separate from boot; `MIGRATE_ON_BOOT=auto` for prod (C3) | C3 spec |
| Backup / recovery | Btrfs hourly snapshots (host-native; C1 deferred — Btrfs absorbs role) | [`backup-restore.md`](docs/operations/backup-restore.md) |
| Metrics + alerting | C5 Prometheus registry; D6 5-rule evaluator → incoming webhook | [`on-call-sop.md`](docs/operations/on-call-sop.md) |
| Incident response | DCER playbooks for user-reported / security / perf / 3rd-party | [`incident-runbook.md`](docs/operations/incident-runbook.md) |
| Token revocation | Reactive kill of compromised mcp_tokens / sessions / OAuth grants | [`token-revocation.md`](docs/operations/token-revocation.md) |
| Frontend e2e harness | Playwright via Docker-wrapped chromium; 3 golden paths (D3) | [`frontend-e2e.md`](docs/operations/frontend-e2e.md) |
| CI gates / required-to-merge | `frontend-e2e` + `mcp-contract` block merge to master (D1) | [`ci-gates.md`](docs/operations/ci-gates.md) |

### Pre-launch checklist

Run through every item before opening the deployment to non-operator
users. Each item links to the spec/doc that owns it.

1. **`/etc/akashic/akashic.env` populated and chmod 0600 root:root**. Stat-verify:
   `stat -c '%a %U:%G' /etc/akashic/akashic.env` → `600 root:root`.
2. **`AKASHIC_ENV=production` set** in the env file (rejects placeholder
   credentials at boot per A1).
3. **`GITLAB_CLIENT_SECRET` is a freshly-issued secret, not a dev one.
   `GITLAB_REDIRECT_URI` points at the public hostname's `/auth/callback`.
4. **Reverse proxy is configured** for the public hostname; TLS
   certificate is issued and renewing automatically.
5. **`ALERTS_WEBHOOK_URL` set** in the env file, pointing at the
   operator's incident channel. Run the D6 quarterly smoke per
   `on-call-sop.md` § "Quarterly smoke".
6. **Filesystem snapshots are configured** for the data volumes on
   hourly cadence. Verify a recent snapshot exists.
7. **`migrate verify` exits 0**:
   `docker run --rm akashic-backend:<tag> akashic-record migrate verify`.
8. **Release-binary symbol grep**:
   `nm <released-binary> | grep -c test_fixtures` returns 0 (no fixture
   endpoint leakage per D3 AC-2).
9. **CI required-to-merge gates are active**: `frontend-e2e` and
   `mcp-contract` block merge to master (D1 capstone). Verify via
   the deliberately-failing-PR drill in
   [`ci-gates.md`](docs/operations/ci-gates.md) § "Deliberately-failing
   PR proof".
10. **First on-call drill** — operator runs the quarterly smoke + a
    note-level recovery from `backup-restore.md` end-to-end, records
    times in private ops log. If either exceeds the RTO (10 min for
    snapshot, 5 min for note recovery), iterate on the SOP.

### Operations runbook index

All operational procedures live under `docs/operations/`. They are
designed for a single-operator project and treat "future-you" as the
audience.

- **[`on-call-sop.md`](docs/operations/on-call-sop.md)** — Alert-driven
  triage for R1-R5 (5xx rate, LLM quota, readiness probe, audit spike,
  shutdown drain timeout).
- **[`incident-runbook.md`](docs/operations/incident-runbook.md)** —
  User-reported / security / performance / 3rd-party incidents NOT
  covered by alerts. DCER playbooks per class.
- **[`backup-restore.md`](docs/operations/backup-restore.md)** — Btrfs
  snapshot recovery (primary) + pg_dump/neo4j-admin cold restore
  (secondary).
- **[`token-revocation.md`](docs/operations/token-revocation.md)** —
  Reactive kill of compromised mcp_tokens / sessions / OAuth grants.
  Reversible where possible.
- **[`rotation-sop.md`](docs/operations/rotation-sop.md)** — Proactive
  scheduled credential rotation. Different from revocation.
- **[`frontend-e2e.md`](docs/operations/frontend-e2e.md)** — Playwright
  harness lifecycle, CI failure triage, `test-fixtures` threat model.

## MCP Tools

All 27 tools live at the single `/mcp` endpoint and **all require a Bearer
token** — there is no anonymous tier. `save_note`, `supersede_note`, and
`link_cross_service_calls` additionally mutate Neo4j/PostgreSQL and are
audited (see [Audit log](#audit-log)); the other 24 are read-only. The
read/write split below is retained purely for the audit trail and this
table — it no longer affects access control (see [Authenticating MCP
requests](#authenticating-mcp-requests)).

| Tool | Tier | Description |
|------|------|-------------|
| `search_knowledge` | read | Dual-level (BM25 + vector) search across code, docs, and notes. Supports modes: `explore`, `saga`, `code`, `notes`. |
| `get_details` | read | Fetch full content for specific IDs from `search_knowledge` results. With `repo` only: lists modules. |
| `save_note` | **write** | Save knowledge linked to a Git repo and branch. Category: `ARCHITECTURE`, `BUG_FIX`, `CONFIG`, `ONBOARDING`, `DECISION`. Includes dedup gate. |
| `get_project_summary` | read | Aggregated overview: branches, note counts by category, recent entries, module and chunk counts. |
| `traverse_code_calls` | read | Traverse CALLS edges upstream/downstream from a symbol with confidence filtering. |
| `trace_execution_flow` | read | Trace execution flows from entry points (API handlers, main, tests). |
| `get_note_health` | read | Check health of knowledge notes: stale detection, usage stats, auto-archived notes. |
| `supersede_note` | **write** | Mark an old note as superseded by a new one; retires it from search results. |
| `list_sagas` | read | List narrative sagas (issue/branch/topic threads) for a repository. Filter by status. |
| `get_saga_timeline` | read | Get the chronological timeline of notes within a saga, including superseded notes. |
| `global_query` | read | Answer architecture questions using community detection (Leiden algorithm). |
| `analyze_impact` | read | Evaluate blast radius of changing a symbol or file. Weighted scoring across CALLS, IMPORTS_FROM, EXPLAINS, flows. |
| `goto_definition` | read | Resolve a symbol name to its precise definition(s) by exact fqn/name match. |
| `find_references` | read | Find direct call-site references to a precise fqn. |
| `find_implementations` | read | Find who implements/extends a trait or class (incoming), or what a type declares it implements/extends (outgoing). |
| `detect_code_communities` | read | Cluster the call graph into communities (Leiden algorithm). |
| `detect_dead_code` | read | List ranked dead-code candidates (zero-inbound-CALLS functions, entry points excluded). |
| `analyze_change_impact` | read | Combined blast radius for a SET of changed symbols (e.g. the functions/types touched by a diff or PR). |
| `link_cross_service_calls` | **write** | Rebuild cross-service HTTP_CALLS edges across all repos. |
| `trace_decision_history` | read | Trace the DECISION-note history for a code symbol (attached decisions + their SUPERSEDES chains). |
| `get_decision_lineage` | read | Full SUPERSEDES lineage for one decision note plus the code it is attached to. |
| `get_docs_schema` | read | Canonical JSON Schema for the docs-kit manifest or docs-toml contract. |
| `list_authoring_sections` | read | Table of contents for the docs-authoring guide. |
| `get_authoring_guide` | read | Fetch one docs-authoring guide section's markdown. |
| `check_docs_coverage` | read | Validate a docs corpus tree with the same nav/link/anchor checker the platform's ingest path runs. |
| `list_docs` | read | Published docs corpora: repo list with version stamps, or one repo's full nav tree. |
| `get_docs_page` | read | Read one full docs page (raw markdown + version stamp) by corpus path. |

The classification is enforced by an exhaustive unit test
(`backend/crates/akashic-mcp/src/mcp/tools/mod.rs::tests::all_tools_classified`,
which also pins the total at exactly 27) that fails the build if a tool
is added without being classified as read or write.

### Authenticating MCP requests

**Every** MCP tool call — reads and writes alike, including `tools/list`
and the legacy `initialize` handshake — requires an
`Authorization: Bearer <token>` header. A request with no bearer, a
malformed bearer, an expired/revoked `ak_*` token, or a `glpat-*` PAT
that fails GitLab validation (including on a GitLab 5xx — this fails
closed, not open) gets an immediate HTTP 401 with an RFC 9728
`WWW-Authenticate: Bearer resource_metadata="<base>/.well-known/oauth-protected-resource"`
challenge, before the request ever reaches the MCP tool dispatcher.

There are three ways to obtain a bearer:

#### Option 1 — OAuth Authorization Code + CIMD (for MCP clients with SEP-991 support)

This is the flow a CIMD-aware MCP client (one that supports [Client ID
Metadata Documents](https://modelcontextprotocol.io), SEP-991) drives
automatically. There is **no dynamic client registration** — a client's
`client_id` IS an `https://` URL it hosts itself, pointing at a small
JSON document describing the client:

```json
{
  "client_id": "https://your-client.example/client-metadata.json",
  "client_name": "My MCP Client",
  "redirect_uris": ["http://127.0.0.1:33418/callback"]
}
```

1. Client discovers this server's OAuth endpoints via
   `GET /.well-known/oauth-protected-resource` (RFC 9728) and
   `GET /.well-known/oauth-authorization-server` (RFC 8414 — advertises
   `client_id_metadata_document_supported: true`, not a
   `registration_endpoint`; there is no dynamic-client-registration
   endpoint).
2. Client opens a browser at `GET /oauth/authorize` with
   `response_type=code`, its `client_id` metadata URL, `redirect_uri`,
   `state`, and PKCE `code_challenge`/`code_challenge_method=S256`
   (mandatory — `plain` is never accepted).
3. If the browser has no existing Akashic web session, it's redirected
   to `/auth/web/login?next=...` to complete GitLab OAuth login first,
   then bounced back to `/oauth/authorize`.
4. The server fetches and validates the client's metadata document
   (`https://` only, no redirects followed, 5s timeout, 64 KiB cap,
   private/loopback/link-local IPs rejected — unless
   `MCP_CIMD_ALLOW_LOOPBACK=true`, a dev/test-only escape hatch that
   production config validation refuses to boot with), confirms the
   document's own `client_id` field matches the URL it was fetched
   from, and confirms `redirect_uri` is exactly one of the document's
   declared `redirect_uris`. Any failure here is a flat 400
   `invalid_client` — never a redirect, since `redirect_uri` isn't
   trusted yet.
5. On success, the server renders a server-rendered HTML **consent
   screen** (client name, client_id URL, redirect_uri — all
   HTML-escaped) with a hidden, single-use, session-bound `consent_id`.
6. User clicks **Approve** → `POST /oauth/authorize/consent` (CSRF-safe:
   redemption requires the same session that created the consent row) →
   303 redirect to `redirect_uri?code=...&state=...`. **Deny** → 303
   with `?error=access_denied&state=...`.
7. Client exchanges the one-time code:
   ```bash
   curl -s -X POST https://akashic.example/oauth/token \
     -H 'content-type: application/x-www-form-urlencoded' \
     -d 'grant_type=authorization_code&code=...&redirect_uri=...&client_id=https://your-client.example/client-metadata.json&code_verifier=...'
   # → {"access_token":"ak_...","token_type":"Bearer","expires_in":7776000}
   ```

The minted `ak_...` token is the same token family as the device flow
below: a **90-day sliding TTL** — `issue_mcp_token` sets `expires_at =
now() + interval '90 days'` at mint time, and every successful
`validate_mcp_token` call (debounced to once per 60s) slides it forward
by another 90 days from that use. An actively-used token effectively
never expires; one that goes untouched for 90 days does. There is
**no OAuth refresh-token grant and no scope** — clients never receive a
`refresh_token`; once a token does expire, re-run this flow (or the
device flow) to mint a new one.

#### Option 2 — OAuth Device Flow (fallback for clients without CIMD support)

Initiated by the MCP client, completed in your browser via the existing
GitLab OAuth login. No client-hosted metadata document required — only
the fixed, server-allowlisted `client_id` value `"claude-code"`.

```bash
# Step 1: Client requests a device code
curl -s -X POST https://akashic.example/oauth/device_authorization \
  -H 'content-type: application/json' \
  -d '{"client_id":"claude-code"}'
# → {"device_code":"...","user_code":"ABCD-1234","verification_uri":"https://akashic.example/device", ...}

# Step 2: Open the verification_uri in a browser, log in via GitLab,
#         enter the user_code, click Authorize.

# Step 3: Client polls for the token
curl -s -X POST https://akashic.example/oauth/token \
  -H 'content-type: application/json' \
  -d '{"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code":"..."}'
# → {"access_token":"ak_...","token_type":"Bearer","expires_in":7776000}
```

Add the resulting `ak_...` token to your Claude Code MCP config under
the Akashic server's `Authorization: Bearer ak_...` header. Same
90-day **sliding** TTL as Option 1 (each use — debounced to once per
60s — slides `expires_at` another 90 days forward; only an unused
token actually expires) and no scope; both options mint via the same
`issue_mcp_token`, and there is no OAuth refresh-token grant either
way — re-run this flow to mint a new token once an old one does
expire.

#### Option 3 — GitLab Personal Access Token passthrough

For environments that cannot complete an interactive browser flow (CI
scripts, air-gapped containers):

```
Authorization: Bearer glpat-yourtoken...
```

Akashic validates the PAT against `GET <gitlab>/api/v4/user` on each
request, with a 60-second positive cache. Revocation in GitLab takes
effect within 60s on Akashic; revocation via Akashic's tombstone table
is immediate.

#### Token revocation

The web UI's "My Tokens" tab is **planned, not yet mounted**
(`frontend/src/lib/api/tokens.ts` has the client-side API calls, but no
page routes to it yet). Until it ships, use the SQL runbook in
[`docs/operations/token-revocation.md`](docs/operations/token-revocation.md),
or drive the same underlying REST API directly — it exists and is live
even without a UI:

| Method | Path | Description |
|---|---|---|
| GET | `/api/v1/auth/tokens` | List my MCP tokens |
| POST | `/api/v1/auth/tokens/{id}/revoke` | Revoke one of mine |
| POST | `/api/v1/auth/passthrough/revoke` | Tombstone a `glpat-` PAT (body: `{token}` or `{prefix}`) |
| GET | `/api/v1/auth/audit?limit=50` | My recent audit rows |

These endpoints require a valid `ak_session` cookie (the web login
session, not an MCP bearer). For operators without a session, the SQL
path:

```sql
UPDATE mcp_tokens SET revoked_at = now() WHERE id = '<uuid>';
```

…and a passthrough PAT via tombstone:

```sql
INSERT INTO revoked_passthrough_tokens (token_id_hash)
VALUES (substring(encode(sha256('<token-without-glpat-prefix>'::bytea), 'hex') for 16));
```

**Passthrough revoke has no ownership check** — anyone authenticated can
write a tombstone for any prefix. Blast radius is bounded: the
tombstone only blocks future use of that specific GitLab PAT; no
privilege escalation.

#### Audit log

Every successful MCP write produces a single row in the `audit_log` table containing the actor's GitLab id, the credential id, the auth method (`device_flow` / `gitlab_passthrough`), the action name, a target id (best-effort), the SHA-256 hash of the canonicalized request arguments, the peer IP, and a 200-byte response summary. Audit inserts are fire-and-forget — a database hiccup does not fail the underlying write, but does emit a `tracing::error!(event = "audit_write_failed", ...)` event.

Query recent activity for an operator:

```sql
SELECT ts, action, target_id, encode(after_hash, 'hex') as hash, response_summary
FROM audit_log
WHERE actor_user_id = <gitlab_user_id>
ORDER BY ts DESC
LIMIT 50;
```

Until the "My Tokens" web UI ships, operators query `audit_log`
directly, or use `GET /api/v1/auth/audit?limit=50` with a session
cookie (see [Token revocation](#token-revocation) above).

#### Per-actor LLM/embedding quota

REST API requests that invoke the LLM or embedding providers are tracked per-actor in the `llm_usage` Postgres table. Before each call, the rolling-window sum of tokens for the actor is compared against `MCP_QUOTA_TOKENS_PER_WINDOW` (default 100,000) over `MCP_QUOTA_WINDOW_SECS` seconds (default 3600). Over-cap requests return HTTP 429 with body `{"error":"quota_exceeded","used":...,"cap":...,"window_secs":...}` and write a `quota_exceeded:<llm|embedding>` row to `audit_log`.

**MCP is fully metered.** Now that every `/mcp` request is authenticated, `mcp_auth` always establishes the `CURRENT_ACTOR` quota scope before the tool handler runs — there is no anonymous path left to bypass it, and the earlier MCP-bypasses-quota gap (rmcp couldn't install the actor task-local on the old standalone MCP listener) is closed by construction: `mcp_auth` is a normal tower layer on the merged `/mcp` branch, exactly like REST's `require_auth`.

**Remaining limitation:** Public REST routes that don't go through `require_auth` (e.g., `/api/v1/search`, `/api/v1/graphrag/query`, `/api/v1/relink-explains/:name`) still bypass quota entirely because `CURRENT_ACTOR` is never set on those request paths. For multi-user / public-facing deployments, gate these routes behind authentication (or a coarser IP-rate-limit) before relying on quota for cost protection.

**Disabling enforcement** (still records usage rows): set `MCP_QUOTA_ENABLED=false`.

Operator query for top usage by actor in the last hour:

```sql
SELECT actor_user_id, kind, SUM(tokens_used) as tokens
FROM llm_usage
WHERE ts > now() - INTERVAL '1 hour'
GROUP BY actor_user_id, kind
ORDER BY tokens DESC
LIMIT 20;
```

#### OAuth runtime validation

Beyond static config validation (A3 — empty fields, placeholder values), B5 adds runtime probes that catch GitLab OAuth misconfigurations actively. Six checks (3 static, 3 network) run via three operator-facing surfaces:

- **Startup-time:** controlled by `OAUTH_VALIDATION_MODE`:
  - `off` — skip entirely (no network calls)
  - `warn` (default) — log all check outcomes; continue boot regardless
  - `strict` — exit with code 70 if any check fails
- **`GET /api/v1/health/oauth`** — JSON report for monitoring systems. 60s cache. Status 200 (Ok/Warn) or 503 (Fail).
- **`akashic-record check-oauth`** CLI subcommand — runs the report standalone, prints a human-readable table, exits 0/1.

The 6 checks:

| Name | Type | Detail |
|---|---|---|
| `gitlab_url_reachable` | network | `GET <gitlab_url>/api/v4/version` returns 200 + JSON |
| `oauth_token_endpoint_reachable` | network | `POST <gitlab_url>/oauth/token` returns OAuth-shaped error |
| `client_credentials_valid` | network | `POST <gitlab_url>/oauth/token` with `grant_type=client_credentials` accepted |
| `redirect_uri_consistent` | static | `GITLAB_REDIRECT_URI == <PUBLIC_BASE_URL>/auth/callback` |
| `web_redirect_uri_consistent` | static | `GITLAB_WEB_REDIRECT_URI == <PUBLIC_BASE_URL>/auth/web/callback` |
| `https_in_production` | static | All public URLs use `https://` when `AKASHIC_ENV=production` |

Each check carries `name`, `status` (ok/warn/fail), `detail` (the upstream message or specific value), and `remediation` (which env var to fix). Operators get actionable errors, not "OAuth failed somewhere".

(The `/api/v1/auth/*` token-management endpoints referenced above are documented in full under [Token revocation](#token-revocation).)

## REST API

### Public (no authentication)

```
GET  /health                                              Liveness check
GET  /ready                                               Readiness check (DB connectivity)

GET  /api/v1/repos                                        List repositories
GET  /api/v1/repos/:name/branches                         List branches
GET  /api/v1/repos/:name/permissions                      GitLab access level for current user
GET  /api/v1/repos/:name/detail                           Repository detail

GET  /api/v1/repos/:name/notes                            List notes (paginated)
GET  /api/v1/repos/:name/notes/:uuid                      Note detail
GET  /api/v1/repos/:name/notes/health                     Note health summary

GET  /api/v1/graph/:repo_name                             Core graph (nodes + edges)
GET  /api/v1/god-nodes/:repo_name                         High-degree hub nodes
GET  /api/v1/module-graph/:repo_name                      Module-level graph
GET  /api/v1/modules/:module_id/chunks                    Chunks within a module
GET  /api/v1/modules/:module_id/call-graph                Call graph for a module
GET  /api/v1/doc-graph/:repo_name                         Documentation graph
GET  /api/v1/documents/:doc_id/sections                   Sections of a document
GET  /api/v1/clusters/:cluster_id/sections                Sections in a doc cluster

GET  /api/v1/repos/:name/modules                          List modules
GET  /api/v1/repos/:name/modules/:path/chunks             Chunks for a module path
GET  /api/v1/repos/:name/chunks/:id                       Chunk detail

GET  /api/v1/repos/:name/ingest/status                    Ingestion status
GET  /api/v1/jobs/active                                  Active ingestion jobs
GET  /api/v1/sources/overview                             All sources overview

GET  /api/v1/search                                       Unified search (BM25 + vector)
POST /api/v1/graphrag/query                               GraphRAG query
GET  /api/v1/details                                      Fetch details by ID
POST /api/v1/relink-explains/:name                        Re-link EXPLAINS edges for a repo

GET  /api/v1/repos/:name/sagas                            List sagas
GET  /api/v1/repos/:name/sagas/:saga_id                   Saga detail

GET  /api/v1/events                                       SSE event stream (ingestion progress)
```

### Protected (require GitLab session)

```
POST   /api/v1/repos/:name/ingest                         Trigger ingestion
POST   /api/v1/repos/:name/reingest                       Re-ingest (full)
POST   /api/v1/repos/:name/resume                         Resume paused ingestion
POST   /api/v1/sources/add                                Add a new source
PUT    /api/v1/repos/:name/notes/:uuid                    Update a note
DELETE /api/v1/repos/:name/notes/:uuid                    Delete a note
DELETE /api/v1/repos/:name                                Delete a repository
GET    /api/v1/gitlab/branches                            List GitLab branches for a repo
```

## Neo4j Data Model

```
(:Repository)-[:HAS_BRANCH]->(:Branch)
(:Repository)-[:HAS_MODULE]->(:Module)
(:Module)-[:CONTAINS]->(:Chunk)
(:Chunk)-[:CALLS {confidence, method}]->(:Chunk)
(:Chunk)-[:IMPORTS_FROM]->(:Module)
(:Note)-[:BELONGS_TO]->(:Repository)
(:Note)-[:LINKED_TO]->(:Branch)
(:Note)-[:TAGGED_AS]->(:Category)
(:Note)-[:TAGGED_WITH]->(:Tag)
(:Note)-[:EXPLAINS]->(:Chunk)
(:Note)-[:PART_OF]->(:Saga)
(:Saga {repo_name, name, status})-[:PART_OF]->(:Repository)
(:Flow)-[:FLOW_STEP]->(:Chunk)
```

Node labels: `Repository`, `Branch`, `Module`, `Chunk`, `Note`, `Saga`, `Category`, `Tag`, `Flow`

Categories (closed enum): `ARCHITECTURE`, `BUG_FIX`, `CONFIG`, `ONBOARDING`, `DECISION`

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `NEO4J_URI` | `bolt://neo4j:7687` | Neo4j Bolt URI |
| `NEO4J_USER` | `neo4j` | Neo4j username |
| `NEO4J_PASSWORD` | `changeme` (placeholder in `.env.example`; replace before any deployment — see `docs/operations/rotation-sop.md`) | Neo4j password |
| `DATABASE_URL` | — | PostgreSQL connection string (e.g. `postgres://$USER:$PASS@postgres:5432/akashic`, set via `.env`) |
| `EMBEDDING_PROVIDER` | `local` | `local` (candle) or `openai` (OpenAI-compatible) |
| `EMBEDDING_API_KEY` | — | Required when provider is `openai` |
| `EMBEDDING_MODEL` | `text-embedding-3-small` | Embedding model name |
| `EMBEDDING_BASE_URL` | — | Override base URL for OpenAI-compatible endpoints |
| `LLM_PROVIDER` | `local` | `local` or `openai` — used for module grouping and EXPLAINS edges |
| `LLM_API_KEY` | — | Required when LLM provider is `openai` |
| `LLM_MODEL` | `gpt-5-nano` | LLM model name |
| `API_HOST` | `0.0.0.0` | Bind host for the single backend listener (REST API + `/mcp`) |
| `API_PORT` | `8081` | Bind port for the single backend listener (REST API + `/mcp`) |
| `PUBLIC_BASE_URL` | `http://localhost:8081` | This server's own externally-visible base URL — used to build OAuth/CIMD metadata URLs, the RFC 9728 `resource_metadata` challenge, and the rmcp `allowed_hosts` allowlist |
| `GITLAB_URL` | — | GitLab instance URL |
| `GITLAB_APP_ID` | — | GitLab OAuth Application ID |
| `GITLAB_APP_SECRET` | — | GitLab OAuth Application Secret |
| `GITLAB_REDIRECT_URI` | — | OAuth callback URL (CLI flow) |
| `GITLAB_WEB_REDIRECT_URI` | — | OAuth callback URL (web flow) |
| `GITLAB_SERVICE_TOKEN` | — | Optional service account token for ingestion |
| `GITLAB_WEBHOOK_SECRET` | — | Optional webhook token validation |
| `CORS_EXTRA_ORIGINS` | — | Comma-separated extra allowed CORS origins |
| `COOKIE_SECURE` | `true` | Set `false` for local HTTP development |
| `AUTH_CODE_TTL_SECS` | `300` | Verification code expiry (seconds) |
| `API_KEY_TTL_SECS` | `86400` | Session token expiry (0 = no expiry) |
| `INGEST_CLONE_DIR` | `/tmp/akashic-ingest` | Directory for cloned repositories |
| `INGEST_MAX_FILE_SIZE` | `102400` | Max file size to ingest (bytes) |
| `INGEST_CONCURRENT_JOBS` | `2` | Max parallel ingestion jobs |
| `INGEST_SKIP_PATTERNS` | `node_modules,vendor,...` | Comma-separated directory patterns to skip |
| `MCP_QUOTA_TOKENS_PER_WINDOW` | `100000` | Per-actor LLM+embedding token cap over rolling window (B3) |
| `MCP_QUOTA_WINDOW_SECS` | `3600` | Rolling window length in seconds for quota math (B3) |
| `MCP_QUOTA_ENABLED` | `true` | Enforce quota on REST traffic. `false` records usage but allows over-cap calls (B3) |
| `MCP_PASSTHROUGH_USER_CACHE_TTL_SECS` | `60` | TTL for the in-process cache of GitLab user-info during PAT passthrough validation. `0` bypasses the cache (every call hits GitLab) (B4) |
| `OAUTH_VALIDATION_MODE` | `warn` | Startup OAuth validation behavior. `off` skips, `warn` logs and continues, `strict` exits non-zero on Fail (B5) |
| `MCP_CIMD_ALLOW_LOOPBACK` | `false` | Dev/test-only escape hatch: allows `http://` and loopback/private-network hosts for OAuth `client_id` Client ID Metadata Document URLs, which the CIMD SSRF guard otherwise rejects. Production config validation hard-fails startup if this is `true`. |

## Development

```bash
# Backend (Rust)
cd backend
cargo check        # type-check
cargo build        # compile
cargo test         # run tests
cargo run          # run locally (requires Neo4j + PostgreSQL)

# Frontend (Svelte 5)
cd frontend
npm install
npm run dev        # dev server with HMR on :5173
npm run build      # production build
npm run check      # svelte-check type validation
```

## Security

- Frontend dependency policy: [`frontend/security/README.md`](frontend/security/README.md)
- Backend dependency policy: [`backend/security/README.md`](backend/security/README.md)

### Rate limiting

Akashic Record applies a per-IP rate limit on every externally reachable
HTTP surface (REST `/api/`, `/auth/`, ingestion sub-paths, and the MCP
`/mcp` endpoint). The `/health` and `/ready` probes are exempt.

Per-route quotas (hard-coded; not env-configurable in this release):

| Prefix | Burst | Replenish | Sustained |
|--------|-------|-----------|-----------|
| `/health`, `/ready` | exempt | — | unlimited |
| `/auth/` | 10 | 1 token / 6 s | 10 / min |
| `/api/v1/.../ingest`, `.../reingest`, `.../resume`, `/api/v1/sources/add` | 5 | 1 token / 12 s | 5 / min |
| `/api/` (general) | 60 | 1 token / 1 s | 60 / min |
| `/mcp` | 30 | 1 token / 2 s | 30 / min |

Configuration:

- `RATE_LIMIT_ENABLED` — `true` (default) / `false`. Disables the layer.
- `RATE_LIMIT_TRUSTED_PROXIES` — CIDR list. Default: loopback + RFC1918.
- `RATE_LIMIT_ALLOWLIST` — CIDR list. Default empty.

When a client exceeds the limit, the response is `429 Too Many Requests`
with a `Retry-After` header (seconds) and `X-RateLimit-*` headers.

**Deployment note:** MCP is no longer a separate listener. `/mcp` is a
branch of the same main router the REST API is on, merged in **before**
the global layers are applied — so it inherits `MetricsLayer`,
`RequestIdLayer`, `TraceLayer`, and CORS exactly like any REST route.
Inside the `/mcp` branch specifically, the layer order (outer to inner)
is: this rate limit → `mcp_auth` (Bearer validation — see
[Authenticating MCP requests](#authenticating-mcp-requests)) →
the rmcp streamable-HTTP service. There is no internal loopback proxy
step anymore.

## License

MIT License. See [LICENSE](LICENSE) for details.
