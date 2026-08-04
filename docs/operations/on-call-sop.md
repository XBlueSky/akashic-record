# Akashic Record — On-Call SOP

Single-operator project. This document is "future-you" documentation —
when an alert fires, follow this runbook before reaching for ad-hoc
diagnosis.

## General triage (run for every alert)

1. **Open the alert webhook message.** Note: rule (R1-R5), severity emoji,
   metric values, build commit footer.
2. **SSH to the deploy host** (or open your container manager UI).
3. **Check container state**:
   ```bash
   docker ps | grep akashic-backend
   docker logs akashic-backend --tail 200
   ```
4. **Probe metrics directly**:
   ```bash
   curl -sf https://akashic.example.com/api/v1/metrics | grep -E "^akashic_(http_requests|llm_quota|readiness|audit_log|shutdown)" | head -30
   ```
5. **Decide**: hotfix in-place / rollback to last-known-good image tag /
   wait-and-watch (if alert was a flap).

## R1: HTTP 5xx rate

**Severity:** Warn. **Threshold:** > 5/min over 5min.

**Likely causes:**
- Postgres / Neo4j connection pool exhaustion
- Upstream MCP proxy or rmcp transport bug
- OpenAI API outage propagating to embedding/llm callers as 5xx
- Sudden traffic spike exceeding rate-limit (returns 429 not 5xx, but
  closely related backpressure)

**Investigation:**
```bash
docker logs akashic-backend --tail 500 | grep -E "ERROR|panic|5[0-9][0-9] " | tail -30
curl -sf https://akashic.example.com/api/v1/metrics | grep akashic_http_requests_total | grep ' 5'
# Check DB pool capacity
docker logs akashic-backend | grep -E "pool.*exhausted|connection refused" | tail
```

**Mitigation (priority order):**
1. **Pool exhaustion** → restart the backend container. New pool
   will drain stale connections.
2. **OpenAI outage** → switch `EMBEDDING_PROVIDER=local` to fall back
   to local HF model (deploy via env update + restart). Note: changes
   embedding behavior; revert when OpenAI recovers.
3. **Persistent 5xx with no obvious cause** → roll back to the
   previous known-good image tag and redeploy.

## R2: LLM quota near exhaustion

**Severity:** Info. **Threshold:** any `akashic_llm_quota_remaining` < 1000.

**Note (2026-05-14):** R2 is forward-compatible. The
`akashic_llm_quota_remaining` metric is not yet emitted by any
production code path — the rule will never fire today. R2 was
written to be wired up automatically when a future commit adds the
gauge emission (likely as part of B3 quota observability). No false
positives until then; no maintenance burden.

**Likely causes (when emission lands):**
- Tool calling consumed quota faster than expected
- B3 quota config too tight for usage pattern
- Specific actor token has gone wild (loop in client code)

**Investigation:**
```bash
curl -sf https://akashic.example.com/api/v1/metrics | grep akashic_llm_quota_remaining
# Check llm_usage table for the consumer
docker exec akashic-postgres psql -U akashic -d akashic \
  -c "SELECT actor_token_id, SUM(tokens_used) AS total, COUNT(*) AS calls
      FROM llm_usage
      WHERE ts > NOW() - INTERVAL '1 hour'
      GROUP BY actor_token_id ORDER BY total DESC LIMIT 5"
```

**Mitigation:**
1. **Expected usage** → raise `LLM_QUOTA_PER_DAY` env var (consult B3 spec for the right env name) and restart.
2. **Runaway client** → revoke the offending mcp_token (set `revoked_at = NOW()` in `mcp_tokens` table). Operator-issued tokens have known purposes; if you don't recognize the actor, revoke first and ask later.

## R3: Readiness probe failure

**Severity:** Critical. **Threshold:** ≥ 2 consecutive ticks (≥ 120s sustained).

**Likely causes:**
- PG container died (check `akashic-postgres` container state)
- Neo4j container died (same)
- Embedding probe failed (HF Hub network outage if local provider, or OpenAI outage with mocked-out endpoint)

**Investigation:**
```bash
docker ps -a | grep -E "akashic-(postgres|neo4j)"
docker logs akashic-postgres --tail 50
docker logs akashic-neo4j --tail 50
curl -sf https://akashic.example.com/ready
```

**Mitigation:**
1. **Database container down** → restart the container.
   If repeated: check disk usage on the postgres/neo4j data volumes.
2. **Embedding probe** → if `EMBEDDING_PROVIDER=local` and HF Hub
   unreachable: switch to `EMBEDDING_PROVIDER=openai` with the prod
   API key.
3. **Readiness poller bug** → if metrics show probes never recover but
   the underlying service is healthy: restart backend.

## R4: Audit log write spike

**Severity:** Warn. **Threshold:** > 30 writes/min.

**Likely causes:**
- Legitimate burst (operator running a batch import via MCP — usually pre-coordinated)
- Compromised token mass-creating notes
- Loop in a client application

**Investigation:**
```bash
curl -sf https://akashic.example.com/api/v1/metrics | grep akashic_audit_log_writes_total
docker exec akashic-postgres psql -U akashic -d akashic \
  -c "SELECT action, actor_token_id, COUNT(*)
      FROM audit_log
      WHERE ts > NOW() - INTERVAL '10 minutes'
      GROUP BY action, actor_token_id ORDER BY 3 DESC LIMIT 10"
```

**Mitigation:**
1. **Recognized actor + intentional burst** → no action; cooldown will
   silence further alerts in 5 min.
2. **Unrecognized actor** → revoke the token immediately. Wait for the
   alert to subside (no further increments). Investigate via audit_log
   what was created; consider rolling back via `superseded_by` or
   manual delete.

## R5: Shutdown drain timeout

**Severity:** Critical. **Threshold:** any increment to `outcome="timed_out"`.

**Likely causes:**
- C2 drain cap (60s) was insufficient for the in-flight load — operator should investigate which subsystem held shutdown open
- A subsystem ignored the shutdown token (regression of the C2 cooperative-shutdown pattern)
- Container received SIGKILL after SIGTERM (container runtime `stop_grace_period` too short — should be ≥ 65s per C2 spec)

**Investigation:**
```bash
docker logs akashic-backend --tail 300 | grep -E "shutdown|drain"
# Specifically look for which subsystem logged shutdown_drain_started
# but didn't log a corresponding *_shutdown event before timeout.
```

**Mitigation:**
1. **Operator-triggered restart with active load** → review the in-flight
   work; if expected (e.g., a batch ingest), no action needed.
2. **Recurring timeouts** → file a backend issue with the logs;
   investigate which subsystem is blocking. The C2 drain cap can be
   raised via Cargo source change but should be a last resort.

## Escalation

For a single-operator project, escalation is:

```bash
# Open a GitHub issue with the alert payload + steps tried.
gh issue create \
  --title "[on-call] R{N} - {brief description}" \
  --body "Alert: {paste alert message}\n\nSteps tried: {list}\n\nLogs: {gist}"
```

No on-call rotation; the operator handles or defers to next session.

## Quarterly smoke (verify the alert path)

Run once per quarter (or whenever the webhook URL rotates).

```bash
# 1. Verify backend is up + alerts evaluator wired.
docker logs akashic-backend 2>&1 | grep alerts_evaluator | tail -5
# Expected: alerts_evaluator_started + periodic alerts_tick events.

# 2. Accelerate ticks for the smoke (env override requires restart).
# Edit /etc/akashic/akashic.env temporarily:
#   ALERTS_TICK_SECS=5
# Then:
docker compose -f /etc/akashic/docker-compose.prod.yml restart akashic-backend
sleep 10

# 3. Trigger R5 via SIGTERM during a no-traffic moment.
docker kill --signal SIGTERM akashic-backend
sleep 70  # drain cap + tick

# 4. The webhook channel should now have a "🚨 Shutdown drain TIMEOUT" message.
#    If yes: alerts path is healthy.
#    If no: check docker logs akashic-backend for sink errors.

# 5. Restore production defaults: revert ALERTS_TICK_SECS in akashic.env,
#    restart akashic-backend.
```

Record the date + outcome in your private ops log.

## Maintenance

- **Webhook URL rotation**: update `ALERTS_WEBHOOK_URL` in
  `/etc/akashic/akashic.env`, restart akashic-backend, run quarterly
  smoke immediately to verify.
- **Threshold tuning**: thresholds are constants in
  `backend/src/alerts/rules.rs`. To tune, edit, run unit tests,
  rebuild, redeploy.
- **Adding a new rule**: extend `RuleId` enum, add the rule function,
  add it to `evaluate_all`, add unit tests, document the playbook
  here.
