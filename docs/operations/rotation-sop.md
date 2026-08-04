# Akashic Record — Credential Rotation SOP

- **Date created**: 2026-04-28
- **Owner**: deployment operator

## 1. Purpose & scope

This is the rotation runbook for every credential the deployed Akashic
Record stack uses. It also documents how production loads secrets and how
to verify revocation. The runbook applies to any Akashic Record
deployment that uses the `docker-compose.prod.yml` profile produced by
A2.

## 2. Production secret loading

- **File path**: `/etc/akashic/akashic.env`
- **Owner**: `root:root`
- **Mode**: `0600`
- **Verify command**: `stat -c '%a %U:%G' /etc/akashic/akashic.env`
  - Expected output: `600 root:root`
  - Note: Linux/GNU `stat` is assumed (the deployment target is a Linux
    VPS). On BSD/macOS use `stat -f '%Mp%Lp %Su:%Sg'`.
- **Compose wiring**: `docker-compose.prod.yml` references this file via
  `env_file: /etc/akashic/akashic.env` (implemented by A2). Do not place
  the file at any other path; A2 hard-codes this location.
- **Restart procedure** (after editing the file):
  ```bash
  docker compose -f docker-compose.prod.yml up -d --force-recreate backend
  ```

The file is **not** tracked by git. It is hand-placed on the VPS by the
deployment operator. Backup procedures for this file (off-VPS, encrypted)
are the operator's responsibility and out of scope for A1.

## 3. Per-credential rotation procedures

Each subsection follows the same shape: issue new -> update production ->
restart -> revoke old -> verify revocation -> log entry.

### 3.1 `LLM_API_KEY`

- **Provider**: OpenAI dashboard (or whichever LLM vendor is configured)
- **Issue new**:
  1. Log in to <https://platform.openai.com/api-keys>.
  2. Click "Create new secret key", name it `akashic-record-<YYYY-MM-DD>`.
  3. Copy the new key immediately (it is shown only once).
- **Update production**:
  1. SSH to the VPS as a sudoer.
  2. `sudo vim /etc/akashic/akashic.env` and replace `LLM_API_KEY=...`.
  3. Save and verify mode: `sudo stat -c '%a %U:%G' /etc/akashic/akashic.env` -> `600 root:root`.
- **Restart**:
  ```bash
  docker compose -f docker-compose.prod.yml up -d --force-recreate backend
  ```
- **Revoke old**: in the OpenAI dashboard, click the trash icon next to the previous key. Confirm.
- **Verify revocation**:
  ```bash
  curl -s -o /dev/null -w "%{http_code}\n" https://api.openai.com/v1/models -H "Authorization: Bearer <OLD_KEY>"
  ```
  Expected: `401`.
- **Log entry**: append to §5 with date, key fingerprint (last 4 chars), and verification result.

### 3.2 `EMBEDDING_API_KEY`

The procedure depends on `EMBEDDING_PROVIDER`:

- **If `EMBEDDING_PROVIDER=local`** (the `.env.example` default and the
  most likely deployment): there is no external credential to rotate.
  Record this credential as **N/A** in §5; rotation is a no-op. Skip to
  §3.3.
- **If `EMBEDDING_PROVIDER=openai`**: follow §3.1 verbatim — the
  credential, provider, and verification endpoint are identical to the
  LLM key. (If the same OpenAI account is used for both LLM and
  embedding, the operator may choose to use one shared key, in which
  case rotating §3.1 also rotates §3.2; record both log entries
  pointing at the same rotation event.)
- **If `EMBEDDING_PROVIDER` is any other remote provider**: replace the
  provider URL and verification endpoint with the provider-specific
  equivalents, but keep the same shape (issue -> update -> restart ->
  revoke -> verify -> log).

### 3.3 `GITLAB_APP_ID` and `GITLAB_APP_SECRET` (rotated together)

- **Provider**: GitLab admin or user OAuth-applications page
- **Issue new**:
  1. Log in to GitLab; go to **User Settings → Applications** (or **Admin Area → Applications** for instance-wide apps).
  2. Click "New application", name it `akashic-record-<YYYY-MM-DD>`.
  3. Set the redirect URIs to match `GITLAB_REDIRECT_URI` and `GITLAB_WEB_REDIRECT_URI` from the production `.env`.
  4. Select scopes: `read_user`, `read_api` (adjust per the deployment's needs).
  5. Submit; copy `Application ID` and `Secret`.
- **Update production**: edit `/etc/akashic/akashic.env`, replace `GITLAB_APP_ID=...` and `GITLAB_APP_SECRET=...`.
- **Restart**: as in 3.1.
- **Revoke old**: in GitLab, delete the previous OAuth application. Confirm.
- **Verify revocation**:
  ```bash
  curl -s -X POST "https://<GITLAB_URL>/oauth/token" -d "client_id=<OLD_APP_ID>&client_secret=<OLD_SECRET>&grant_type=client_credentials"
  ```
  Expected: HTTP 401 or `{"error":"invalid_client",...}`.
- **Log entry**: as in 3.1.

### 3.4 `GITLAB_WEBHOOK_SECRET`

- **Provider**: GitLab project settings → Webhooks (per project the deployment subscribes to).
- **Issue new**: generate a fresh random string: `openssl rand -hex 32`.
- **Update production**: edit `/etc/akashic/akashic.env`; update the webhook secret in every GitLab project's webhook configuration to match.
- **Restart**: as in 3.1.
- **Revoke old**: by virtue of changing the secret on both ends; old webhook calls will fail signature verification.
- **Verify revocation**: trigger a manual webhook from a GitLab project still using the old secret; confirm the backend returns 401 / signature-mismatch.
- **Log entry**: as in 3.1.

### 3.5 `GITLAB_SERVICE_TOKEN`

- **Provider**: GitLab personal access tokens (or group/project access tokens, depending on deployment).
- **Issue new**: in GitLab, **User Settings → Access Tokens**; create a new token with the minimum required scopes (typically `read_api`, `read_repository`); copy the token.
- **Update production**: edit `/etc/akashic/akashic.env`, replace `GITLAB_SERVICE_TOKEN=...`.
- **Restart**: as in 3.1.
- **Revoke old**: in GitLab, click "Revoke" next to the previous token.
- **Verify revocation**:
  ```bash
  curl -s -o /dev/null -w "%{http_code}\n" -H "PRIVATE-TOKEN: <OLD_TOKEN>" "https://<GITLAB_URL>/api/v4/user"
  ```
  Expected: `401`.
- **Log entry**: as in 3.1.

### 3.6 `NEO4J_PASSWORD`

- **Provider**: the Neo4j container itself (no external provider).
- **Issue new**: pick a strong password (`openssl rand -base64 32`).
- **Update production** — order of operations matters (Neo4j must accept the new password before backend restart):
  1. With the stack running, change the password inside Neo4j:
     ```bash
     docker compose -f docker-compose.prod.yml exec neo4j cypher-shell -u neo4j -p "<OLD_PASSWORD>" "ALTER CURRENT USER SET PASSWORD FROM '<OLD_PASSWORD>' TO '<NEW_PASSWORD>';"
     ```
  2. Edit `/etc/akashic/akashic.env`, replace `NEO4J_PASSWORD=...` with the new value.
- **Restart**:
  ```bash
  docker compose -f docker-compose.prod.yml up -d --force-recreate backend
  ```
- **Revoke old**: the `ALTER CURRENT USER SET PASSWORD` statement in step 1 already invalidates the old password.
- **Verify revocation**:
  ```bash
  docker compose -f docker-compose.prod.yml exec neo4j cypher-shell -u neo4j -p "<OLD_PASSWORD>" "RETURN 1;"
  ```
  Expected: authentication failure (`Neo.ClientError.Security.Unauthorized`).
- **Log entry**: as in 3.1.

### 3.7 PostgreSQL password (segment of `DATABASE_URL`)

- **Provider**: the PostgreSQL container itself.
- **Required env vars**: `/etc/akashic/akashic.env` must define `PG_USER`,
  `PG_PASSWORD`, `PG_DB` (consumed by the postgres service for
  `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB`) AND `DATABASE_URL`
  (consumed by the backend). The user/password/database segments of
  `DATABASE_URL` MUST agree with the `PG_*` trio; rotation must update both.
- **Issue new**: pick a strong password (`openssl rand -base64 32`).
- **Update production** — order of operations:
  1. Change the password in PostgreSQL:
     ```bash
     docker compose -f docker-compose.prod.yml exec postgres psql -U akashic -c "ALTER USER akashic WITH PASSWORD '<NEW_PASSWORD>';"
     ```
  2. Edit `/etc/akashic/akashic.env`, update **both** `PG_PASSWORD` and the
     password segment of `DATABASE_URL`.
- **Restart**:
  ```bash
  docker compose -f docker-compose.prod.yml up -d --force-recreate backend
  ```
- **Revoke old**: the `ALTER USER ... WITH PASSWORD` statement already invalidates the old password.
- **Verify revocation**:
  ```bash
  PGPASSWORD="<OLD_PASSWORD>" docker compose -f docker-compose.prod.yml exec postgres psql -U akashic -h localhost -c "SELECT 1;"
  ```
  Expected: `password authentication failed`.
- **Log entry**: as in 3.1.

## 4. Emergency rotation

If a credential is suspected leaked outside a planned rotation:

1. Immediately revoke the credential at the provider (do *not* wait until
   the new credential is deployed — accept the brief outage).
2. Issue a new credential.
3. Update `/etc/akashic/akashic.env` and restart per §3.
4. Log an emergency entry in §5 with the suspected leak vector.
5. Once Phase B's token-revocation infrastructure (B4) lands, prefer
   revoking the actor's MCP/REST token in addition to the underlying
   provider credential.

## 5. Initial rotation log

Per AC-10, the credentials flagged in `finding.md` P0-1 (OpenAI/LLM,
GitLab OAuth) MUST be rotated at A1 close. The operator records each
rotation as a one-line dated entry below. Recommended: rotate all seven
credentials enumerated in §3 proactively, since the repository is being
made public.

```
2026-04-XX  LLM_API_KEY rotated; old key revoked at OpenAI; verified 401 with old key.
2026-04-XX  EMBEDDING_API_KEY rotated; old key revoked; verified 401 with old key. (or: deferred — embedding provider is local, no external secret to rotate.)
2026-04-XX  GITLAB_APP_ID/SECRET rotated; old OAuth app deleted in GitLab; verified invalid_client with old credentials.
2026-04-XX  GITLAB_WEBHOOK_SECRET rotated; webhook configurations updated in <project list>; verified 401 with old secret.
2026-04-XX  GITLAB_SERVICE_TOKEN rotated; old token revoked; verified 401 with old token.
2026-04-XX  NEO4J_PASSWORD rotated; old password verified Unauthorized.
2026-04-XX  POSTGRES password rotated; old password verified password authentication failed.
```

(Operator: replace `2026-04-XX` with actual dates and remove or annotate
any line that does not apply.)

## 6. Subsequent rotations

For all later rotations (planned or emergency), append entries to a new
"Subsequent rotations" subsection below this one, in the same format.
Recommended cadence: rotate every credential at least once every 12
months, or immediately on any incident.
