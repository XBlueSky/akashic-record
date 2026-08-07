# Akashic Record — Token Revocation SOP

**Reactive** procedure for killing a compromised credential immediately.
For **proactive** scheduled rotation, see `rotation-sop.md`.

Single-operator project. Trigger this when:
- A token was accidentally pasted in a public channel (Slack, GitHub, paste-bin)
- An MCP client appears to have been hijacked (unrecognized usage in
  audit_log)
- A session cookie was leaked (e.g., browser DevTools shared on screen)
- The on-call SOP's R4 (audit log spike) playbook escalates to revocation

**Principle**: revoke first, investigate second. Revocation is
reversible (`UPDATE ... SET revoked_at = NULL`); a compromise window
is not. If in doubt, revoke.

## Token types in scope

Akashic Record has three credential kinds. Each has a different
revocation procedure.

| Type | Storage | Lifetime | Use case |
|---|---|---|---|
| **MCP device-flow PAT** | `mcp_tokens` PG table | 90 days, sliding (each authenticated use, debounced to once per 60s, resets `expires_at` to now + 90 days — an actively-used token effectively never expires) | Long-lived bearer for MCP clients (Claude Code, custom agents) |
| **Web session cookie** | `sessions` PG table | 24 hours (refreshed on activity) | Browser session after GitLab OAuth login |
| **GitLab OAuth grant** | GitLab admin UI | Until user revokes at GitLab | Underlying authorization that mints the above two |

## 1. Revoke an MCP device-flow PAT

### Identify the token

You typically know the token's label (operator assigned it at issue time)
or the user_login of the owner.

```bash
ssh nas
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT id, user_login, label, issued_at, last_used_at, expires_at, revoked_at
   FROM mcp_tokens
   WHERE revoked_at IS NULL
   ORDER BY last_used_at DESC NULLS LAST LIMIT 20"
```

Pick the suspect row by label or user_login + last_used_at pattern.

### Revoke

```bash
# By label (preferred — explicit + auditable):
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "UPDATE mcp_tokens
   SET revoked_at = NOW()
   WHERE label = '<suspect-label>' AND revoked_at IS NULL
   RETURNING id, user_login, label"

# Emergency: revoke ALL tokens for a user.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "UPDATE mcp_tokens
   SET revoked_at = NOW()
   WHERE user_login = '<gitlab-username>' AND revoked_at IS NULL
   RETURNING id, label"
```

### Verify

A revoked `mcp_tokens` row is checked on every request (`validate_mcp_token`
queries Postgres directly, `WHERE revoked_at IS NULL AND expires_at >
now()` — no positive cache on the `ak_*` path), so revocation is
effective immediately, no wait needed. Confirm the next call from that
bearer returns 401:

```bash
# MCP is a single streamable-HTTP endpoint at /mcp — no SSE session
# handshake required. Any request shape (even tools/list) is rejected
# the same way once the bearer is revoked.
curl -i -m 5 -X POST http://localhost:13001/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "Authorization: Bearer <revoked-token>" \
  -d '{"jsonrpc":"2.0","method":"tools/call","id":1,"params":{"name":"save_note","arguments":{"repo_name":"akashic-record","title":"test","content":"test","category":"ARCHITECTURE"}}}'

# Expected: HTTP 401, WWW-Authenticate: Bearer resource_metadata="..."
#           (RFC 9728 challenge — mcp_auth's fail-closed response; the
#           request never reaches the tool dispatcher). Port 13001 is
#           the dev/test single-port mapping (docker-compose.test.yml);
#           against a live deployment use the public host's /mcp
#           instead (e.g. https://akashic.example.com/mcp).
```

If the call still succeeds: the backend was deployed from a build
predating the full-auth `mcp_auth` layer (MCP refactor, 2026-08-07).
Restart container, check build commit.

### Audit cleanup

After revocation, decide whether to delete data the compromised token
created. See `incident-runbook.md` Class B Eradicate.

### Reversal (if revocation was a false alarm)

```bash
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "UPDATE mcp_tokens
   SET revoked_at = NULL
   WHERE id = '<id-from-earlier-RETURNING>'
   RETURNING id, label, revoked_at"
```

## 2. Revoke a web session cookie

Sessions live in the `sessions` PG table keyed by `api_key` (the cookie
value itself).

### Identify the session

```bash
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT api_key, username, name, created_at, expires_at
   FROM sessions
   WHERE username = '<gitlab-username>' AND (expires_at IS NULL OR expires_at > NOW())
   ORDER BY created_at DESC"
```

### Revoke (delete row)

Unlike `mcp_tokens` (soft delete via `revoked_at`), sessions are
hard-deleted:

```bash
# Single session (need the api_key value):
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "DELETE FROM sessions WHERE api_key = '<api-key-value>' RETURNING username"

# All sessions for a user (emergency):
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "DELETE FROM sessions WHERE username = '<gitlab-username>' RETURNING api_key"
```

After deletion, the user's next request that sends the cookie hits the
backend's `validate_session` (`backend/src/auth/store.rs`), which queries
the DB, gets no row, and the request fails auth. The user is
transparently logged out; they re-OAuth via the device flow to get a new
session.

### Verify

Open a fresh browser (or incognito), hit a protected endpoint with the
deleted cookie:

```bash
curl -i -m 3 -b 'ak_session=<deleted-api-key>' \
  https://akashic.example.com/api/v1/auth/me
```

Expected: HTTP 401.

### No reversal

Hard-deleted sessions cannot be undone. If you deleted in error, the
user re-logs-in via OAuth — same effect, mild inconvenience.

## 3. Revoke a GitLab OAuth grant

When the GitLab OAuth grant itself was compromised (the user's GitLab
account is suspected of being controlled by attacker), revocation
happens at GitLab.

### Path A: User-initiated (preferred)

User logs into GitLab → Settings → Applications → Authorized
Applications → revoke "Akashic Record" entry.

After revocation:
- Existing `mcp_tokens` continue to work until they expire OR you
  revoke them per § 1 (recommended).
- Existing sessions continue until they expire OR you revoke them
  per § 2 (recommended).
- Future device-flow polls return 401 (GitLab refuses to validate
  the user's identity). User must re-grant before re-issuing tokens.

### Path B: Admin-initiated (GitLab admin only)

On the GitLab instance:

```bash
# GitLab Rails console.
sudo gitlab-rails console
# Find the application's grants for the user.
user = User.find_by(username: 'gitlab-username')
Doorkeeper::AccessGrant.where(resource_owner_id: user.id, application_id: <akashic-app-id>).destroy_all
Doorkeeper::AccessToken.where(resource_owner_id: user.id, application_id: <akashic-app-id>).destroy_all
```

This kills GitLab-side grants regardless of user cooperation.

### After GitLab revocation

Always follow up with § 1 + § 2 revocations on the Akashic side. GitLab
side cuts new logins; Akashic side cuts existing live sessions/tokens.

## 4. Mass revocation (security event)

If a system-wide compromise is suspected (e.g., the
`GITLAB_CLIENT_SECRET` itself leaked):

```bash
# Step 1: rotate the client secret per rotation-sop.md.

# Step 2: revoke ALL active mcp_tokens.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "UPDATE mcp_tokens SET revoked_at = NOW() WHERE revoked_at IS NULL"

# Step 3: kill ALL active sessions.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "DELETE FROM sessions"

# Step 4: restart backend to flush caches.
docker compose -f /etc/akashic/docker-compose.prod.yml restart akashic-backend

# Step 5: post-recovery: users must re-OAuth (sessions) and re-run
# device flow (mcp_tokens). Communicate the disruption window.
```

Mass revocation is disruptive but proportionate to the threat. Single-
operator project → operator alone makes the call.

## Logging the revocation

Whether the action was small (§1) or wholesale (§4), record in the
operator's private incident log:

```
2026-MM-DD HH:MM  Revoked: <kind> <id-or-label>  Reason: <one-line>  Reversed: <yes/no>
```

If you maintain a public security disclosure feed (the project doesn't
yet), include affected user count + remediation status.

## Related docs

- `rotation-sop.md` — proactive credential replacement (different
  trigger, similar SQL targets)
- `incident-runbook.md` — Class B (security incident) calls into §1
- `on-call-sop.md` — R4 audit log spike playbook can escalate to §1
