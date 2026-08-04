# Akashic Record — Backup & Restore SOP

This SOP covers data recovery from snapshot. **Primary path**: Btrfs
snapshots on the deployment host (a dedicated backup track was deferred
because host-level Btrfs snapshots already provide this safety net).
**Secondary path**: `pg_dump` + `neo4j-admin` cold storage for operators
on non-Btrfs filesystems or for off-site backup.

Single-operator project. "Future-you" documentation.

## Recovery time objective (RTO)

- **Btrfs snapshot rollback**: ≤ 10 minutes from "decided to roll back"
  to "backend serving from rolled-back state".
- **pg_dump + neo4j-admin cold restore**: ≤ 60 minutes from "have the
  dump files in hand" to "backend serving".
- **Single-note recovery from snapshot**: ≤ 5 minutes.

These are operator estimates, not SLAs. Single-tenant project; no
external commitments.

## Recovery point objective (RPO)

- **Btrfs snapshots taken hourly** (configure in your snapshot
  scheduler, e.g. btrbk or snapper): ≤ 1 hour of data loss on rollback.
- **Off-site cold storage taken weekly** (operator-managed):
  ≤ 7 days of data loss in a full-host-loss scenario.

If the project ever takes external commitments, tighten the cold-storage
cadence first.

## Primary path: Btrfs snapshot recovery

### Snapshot inventory

The akashic stack stores stateful data under two host-managed volumes
(example layout — adjust to your deployment):

| Volume mount | Contents | Container that owns it |
|---|---|---|
| `/srv/akashic/postgres-data` | Postgres data dir (`/var/lib/postgresql/data`) | `akashic-postgres` |
| `/srv/akashic/neo4j-data` | Neo4j data dir (`/data`) | `akashic-neo4j` |

The snapshot scheduler is configured to snapshot the parent
`/srv/akashic/` subvolume on a recurring schedule. Verify via:

```bash
# SSH to the deployment host
ssh <deploy-host>

# List snapshots for the data subvolume.
sudo btrfs subvolume list / | grep akashic
sudo btrfs subvolume show /srv/.snapshots/<recent-timestamp>
```

If your snapshot layout differs, adjust; the spirit is "find the recent
snapshot before the incident."

### Full rollback (data loss event)

When PG OR Neo4j data is corrupted enough that a full rollback is the
fastest path:

```bash
# 1. Stop the akashic stack cleanly (so PG/Neo4j flush before rollback).
ssh <deploy-host>
cd /srv/akashic
docker compose -f docker-compose.prod.yml stop akashic-backend
docker compose -f docker-compose.prod.yml stop akashic-postgres akashic-neo4j

# 2. Identify the snapshot to restore from.
sudo btrfs subvolume list / | grep akashic | tail -10
# Pick the most recent one BEFORE the incident.

# 3. Roll back using your snapshot tooling (btrbk/snapper have restore
#    commands; the manual form is: move the live subvolume aside, then
#    `btrfs subvolume snapshot` the chosen read-only snapshot back into
#    place as read-write).

# 4. Restart the stack.
docker compose -f docker-compose.prod.yml up -d
sleep 30

# 5. Verify health.
curl -sf https://akashic.example.com/health
curl -sf https://akashic.example.com/ready

# 6. Re-issue any tokens that were created post-snapshot (operator log).
```

### Note-level recovery (single-note loss)

Don't roll back the whole DB to retrieve one note. Mount the snapshot
read-only, run a single SELECT, copy the value back:

```bash
ssh <deploy-host>

# 1. Mount the snapshot's PG data dir to a scratch path.
SNAPSHOT=$(sudo btrfs subvolume list / | grep akashic | tail -1 | awk '{print $9}')
sudo mkdir -p /srv/restore-scratch
sudo mount -o subvol="$SNAPSHOT/postgres-data,ro" /dev/<vol> /srv/restore-scratch
# (mount syntax varies by distro/layout.)

# 2. Spin up an ad-hoc Postgres pointing at the read-only data dir.
docker run --rm -d --name pg-restore-temp \
  -v /srv/restore-scratch:/var/lib/postgresql/data:ro \
  -e POSTGRES_PASSWORD=anything \
  -p 5434:5432 \
  pgvector/pgvector:pg16
sleep 10

# 3. Query the note.
docker exec pg-restore-temp psql -U postgres -d akashic -c \
  "SELECT id, title, content, category FROM notes WHERE id = '<uuid>'"

# 4. Copy the content; re-save into live DB via MCP save_note (preserves
#    audit_log + saga side effects).

# 5. Clean up.
docker stop pg-restore-temp
sudo umount /srv/restore-scratch
sudo rmdir /srv/restore-scratch
```

For Neo4j, the analogous approach uses a temporary `neo4j` container
pointed at the snapshot's `/data` mount.

## Secondary path: pg_dump + neo4j-admin cold storage

For operators not on Btrfs, OR for off-site backup that survives
loss of the whole host.

### Take a cold backup

```bash
# Pick a quiet window (low write traffic; tracking via the audit metric).
ssh <deploy-host>
cd /srv/akashic
mkdir -p /srv/backups/$(date +%Y-%m-%d)
cd /srv/backups/$(date +%Y-%m-%d)

# PG dump — custom format compresses + supports parallel restore.
docker exec akashic-postgres pg_dump -U akashic -Fc -d akashic > akashic-pg.dump

# Neo4j dump — stop neo4j first, dump, restart.
docker compose -f /srv/akashic/docker-compose.prod.yml stop akashic-neo4j
docker run --rm \
  -v akashic_neo4j_data:/data \
  -v "$(pwd)":/backups \
  neo4j:5-community \
  neo4j-admin database dump neo4j --to-path=/backups
docker compose -f /srv/akashic/docker-compose.prod.yml up -d akashic-neo4j

# Verify.
ls -la
# Expected:
#   akashic-pg.dump    (~50-500 MB depending on corpus)
#   neo4j.dump         (~10-100 MB)
```

### Off-site copy

Ship the backup folder to off-host storage. Operator's choice — `rclone`
to cloud, `rsync` to a workstation, removable disk, etc. Keep at least
4 weekly + 6 monthly snapshots.

### Restore from cold backup

```bash
ssh <deploy-host>
cd /srv/akashic
docker compose -f docker-compose.prod.yml down

# Wipe the data volumes (DESTRUCTIVE — confirm before running).
docker volume rm akashic_pg_data akashic_neo4j_data
docker volume create akashic_pg_data
docker volume create akashic_neo4j_data

# Restore PG.
docker compose -f docker-compose.prod.yml up -d akashic-postgres
sleep 10
docker exec -i akashic-postgres psql -U akashic -d akashic < /srv/backups/<date>/akashic-pg.dump

# Restore Neo4j.
docker compose -f docker-compose.prod.yml stop akashic-neo4j
docker run --rm \
  -v akashic_neo4j_data:/data \
  -v /srv/backups/<date>:/backups \
  neo4j:5-community \
  neo4j-admin database load neo4j --from-path=/backups --overwrite-destination=true
docker compose -f docker-compose.prod.yml up -d akashic-neo4j

# Restart backend.
docker compose -f docker-compose.prod.yml up -d akashic-backend
sleep 30
curl -sf https://akashic.example.com/health
curl -sf https://akashic.example.com/ready
```

## Verification dry run (quarterly)

The recovery procedures only work if practiced. Quarterly drill:

1. Take a snapshot of the live host.
2. Pick a random note UUID from `notes` table.
3. Execute the note-level recovery procedure (mount snapshot, SELECT, verify content matches).
4. Document the time taken + any procedural surprises.

If the drill reveals a procedural defect (paths changed, tooling
updated its CLI, etc.), update this SOP. Treat the SOP as living
documentation.

## What NOT to back up

- Ephemeral `/tmp/akashic-ingest` (recreated by the container entrypoint).
- `target/` and `node_modules/` (rebuild from git + Cargo.lock /
  package-lock.json).
- HuggingFace cache `/home/akashic/.cache/huggingface` (re-downloads
  on container recreate; accepted trade-off).
- Container logs (the container runtime retains these; don't move into
  backups).

## Related docs

- `incident-runbook.md` — when to invoke this SOP (Class A path 2)
- `on-call-sop.md` — alert-driven triage; readiness probe failures
  may indicate DB corruption requiring this SOP
- `rotation-sop.md` — after restore, verify all credentials still
  match restored state
