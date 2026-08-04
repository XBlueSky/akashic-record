# Akashic Record — Incident Runbook

When something is wrong but **no alert fired** (D6's `on-call-sop.md` covers
the alert-driven path). This runbook handles user-reported issues,
security concerns, and incidents the metric thresholds didn't catch.

Single-operator project. "Future-you" documentation.

## Classification — which class is this?

| Class | Trigger | Examples |
|---|---|---|
| **A** Data | User reports missing/wrong/corrupted data | "My note from last week is gone"; "Search returns irrelevant results"; "Saga timeline is empty" |
| **B** Security | Suspected compromise, anomalous access patterns | "I accidentally pasted my PAT in a public Slack"; "audit_log shows writes I didn't make"; "Strange user-agent in `sessions` table" |
| **C** Performance | User-perceived slowness; no R1/R3 fire | "Search takes 30s when it used to be 2s"; "MCP tool calls time out intermittently" |
| **D** Third-party | Outage propagating through a dependency | "OpenAI is down"; "GitLab OAuth callback failing"; "container host unreachable" |

Each class below follows the **DCER pattern**: Detect → Contain →
Eradicate → Recover. Skip the section that doesn't apply — e.g.,
for Class D (third-party), eradication is the upstream's job; we
focus on contain + recover.

---

## Class A: User-reported data loss / corruption

### Detect

Confirm the report before acting:

```bash
# If user names a specific note UUID, look it up directly.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT id, title, repo_name, category, created_at, superseded_by
   FROM notes WHERE id = '<uuid>'"

# If user names a title, search by title prefix.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT id, title, created_at, superseded_by FROM notes
   WHERE title ILIKE '%<keyword>%' ORDER BY created_at DESC LIMIT 10"

# Check if the note exists but was superseded.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT id, title, superseded_by, invalid_at FROM notes
   WHERE id = '<uuid>' OR superseded_by = '<uuid>'"
```

Distinguish between **gone** (no row), **superseded** (`superseded_by` is
set), and **stale-but-present** (row exists, content unexpected).

### Contain

If a write loop or compromised token is the cause, see Class B.

If it's a one-off and user just wants the data back, no containment
needed — proceed to Recover.

### Eradicate

If a script or client is mass-overwriting notes (visible as a burst of
`supersede_note` audit rows), revoke the token immediately per
`token-revocation.md`. Wait for audit_log activity to subside.

### Recover

**Path 1 — Note was superseded, want it back as a new note:**

```bash
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT title, content, category, tags FROM notes WHERE id = '<old_uuid>'"
# Copy the content; user re-saves via MCP save_note.
# Or: clear superseded_by + invalid_at on the old note (if appropriate):
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "UPDATE notes SET superseded_by = NULL, invalid_at = NULL WHERE id = '<old_uuid>'"
```

**Path 2 — Note was actually deleted (row gone):**

Restore from Btrfs snapshot per `backup-restore.md` § Note-level recovery.

**Path 3 — Note exists but content is wrong:**

Either roll back the offending `supersede_note` (clear the chain — Path 1
above), OR restore from a snapshot and copy the prior content into a new
save_note.

---

## Class B: Security incident

### Detect

Initial signals:

```bash
# Audit log activity for the last 4 hours, grouped by actor.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT actor_token_id, action, COUNT(*) AS calls, MAX(ts) AS last_call
   FROM audit_log
   WHERE ts > NOW() - INTERVAL '4 hours'
   GROUP BY actor_token_id, action
   ORDER BY calls DESC LIMIT 20"

# Recent session activity.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT username, name, created_at, expires_at FROM sessions
   WHERE created_at > NOW() - INTERVAL '24 hours' ORDER BY created_at DESC"

# MCP tokens issued/used recently.
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT id, user_login, label, issued_at, last_used_at, revoked_at
   FROM mcp_tokens
   WHERE issued_at > NOW() - INTERVAL '7 days' OR last_used_at > NOW() - INTERVAL '24 hours'
   ORDER BY last_used_at DESC NULLS LAST LIMIT 30"
```

Red flags:
- Actor with unrecognized `user_login`
- Tokens with `last_used_at` from unexpected geographic region (cross-check
  with backend logs for source IP)
- Burst of writes from a single actor (also covered by R4 alert, but
  Class B catches the slow-drip case below threshold)

### Contain

**Revoke the suspected token IMMEDIATELY** per `token-revocation.md`. Do
this before further investigation — revocation is reversible (set
`revoked_at = NULL` later); a compromise window is not.

```bash
# Single-command emergency revocation for an mcp_token by label:
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "UPDATE mcp_tokens SET revoked_at = NOW() WHERE label = '<suspect-label>' AND revoked_at IS NULL"
```

For session-cookie compromise:

```bash
# Kill all active sessions for a user (forces re-OAuth):
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "DELETE FROM sessions WHERE username = '<gitlab-username>'"
```

### Eradicate

Identify which data was touched by the compromised actor:

```bash
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT action, target_id, ts, payload
   FROM audit_log
   WHERE actor_token_id = '<compromised-token-id>'
   ORDER BY ts DESC"
```

For each `save_note` / `supersede_note` row attributed to the compromised
actor, decide:
- **Legitimate** (operator under attacker's control was doing real work) →
  leave the note.
- **Malicious** (notes inserted to mislead, or supersede_note used to
  destroy good content) → revert per Class A Recover paths.

### Recover

- If only a token was compromised: revocation is sufficient; the user
  re-authorizes via the device flow with a fresh OAuth grant.
- If the GitLab OAuth client itself was compromised (the secret leaked):
  rotate `GITLAB_CLIENT_SECRET` per `rotation-sop.md` AND revoke all
  active mcp_tokens (they're tied to the GitLab user, not the client).
- File a public incident summary if external users are affected. Single-
  operator project → operator's discretion.

---

## Class C: Performance degradation (no alert)

R1 (5xx rate) and R3 (readiness probe) catch hard failures. Soft
slowdowns — increased latency, intermittent timeouts — fall here.

### Detect

```bash
# Latency histogram for recent HTTP requests (look for tail growth).
curl -sf https://akashic.example.com/api/v1/metrics | \
  grep -E "akashic_http_request_duration_seconds_(sum|count)" | head

# Slow PG queries (requires pg_stat_statements enabled — not on by default).
docker exec akashic-postgres psql -U akashic -d akashic -c \
  "SELECT query, calls, mean_exec_time, total_exec_time
   FROM pg_stat_statements ORDER BY mean_exec_time DESC LIMIT 10"

# Embedding/LLM call timings (OpenAI side latency).
docker logs akashic-backend --tail 1000 | \
  grep -E "openai.*(elapsed|duration)" | tail -20
```

### Contain

If a single user / actor is driving the slowdown:
- Revoke their token (Class B Contain) and observe whether latency returns.
- If yes: re-issue with a tighter rate-limit profile (operator decision).

### Eradicate

Common root causes + fixes:
- **PG connection pool exhausted** → restart backend container. New pool drains stale connections.
- **Neo4j cache cold after restart** → expected for 5-10 min after deploy; if persistent, increase Neo4j heap.
- **OpenAI API slow** → Class D.
- **Saga backlog** → check `SELECT * FROM sagas WHERE status='in_progress'`; if many old entries, the saga propagation task may be stuck.

### Recover

Document the root cause + threshold values in the on-call SOP if it's a
recurring pattern. Consider whether D6 needs a new alert rule (e.g., R6:
saga backlog > N items).

---

## Class D: Third-party outage

### Detect

OpenAI / GitLab / container-host outage usually surfaces as:
- R1 alert firing (5xx propagating out)
- R3 readiness probe failing for the embedding probe
- User reports of auth failures (GitLab) or "thinking forever" (OpenAI)

Confirm at the third party:
- OpenAI: https://status.openai.com/
- GitLab.com: https://status.gitlab.com/ (or self-hosted instance's status)
- Container host: SSH in, check `dmesg | tail` and host resource usage

### Contain (mitigate downstream impact)

- **OpenAI down + we use it for embedding**: env-override
  `EMBEDDING_PROVIDER=local` and restart backend. Falls back to local HF
  model. Embedding quality differs but service stays up.
- **GitLab OAuth callback failing**: existing sessions continue working
  (the validation cache lasts the session TTL); new logins fail until
  GitLab recovers. Communicate to users: "auth is degraded; use existing
  sessions."
- **Container host fully down**: nothing the application can do — wait
  for host recovery.

### Eradicate

Out of scope — the third party owns this. Operator monitors the status
page and the metric recovery curve.

### Recover

After upstream recovery:
- Revert env overrides (e.g., flip `EMBEDDING_PROVIDER` back to `openai`).
- Restart backend to pick up the original config.
- If the outage caused saga backlog, kick the propagation loop with a
  no-op write to flush.

---

## After every incident

Update this runbook with new patterns. The runbook is reactive but the
goal is for each incident class to converge toward "the SOP already
covered it" over time.

Add a one-line entry to the operator log (private ops journal — not
checked in):

```
2026-MM-DD  Class B  Suspected leaked token in chat — revoked TOK-XYZ, audit confirmed 3 legit save_notes only; no rollback needed
```

If the pattern recurs 3+ times, promote it to a named subsection above
with its own DCER playbook.

## Related docs

- `on-call-sop.md` — alert-driven triage (R1-R5)
- `backup-restore.md` — Btrfs snapshot recovery; restore data for Class A path 2
- `token-revocation.md` — kill-on-compromise procedures referenced from Class B Contain
- `rotation-sop.md` — proactive credential rotation (different from revocation)
- `dsm-reverse-proxy-setup.md` — TLS path
- `frontend-e2e.md` — Playwright harness operations
