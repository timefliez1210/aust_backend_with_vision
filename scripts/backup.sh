#!/usr/bin/env bash
# backup.sh — Daily backup of postgres + minio (runs ON the VPS via cron)
#
# Saves to /opt/aust/backups/, retains 7 days.
# Install: bash scripts/setup-backups.sh

set -euo pipefail

BACKUP_DIR="/opt/aust/backups"
DATE=$(date +%Y%m%d_%H%M%S)
RETAIN_DAYS=7

mkdir -p "${BACKUP_DIR}"

log() { echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"; }

# ---------------------------------------------------------------------------
# 1. PostgreSQL dump
# ---------------------------------------------------------------------------
PG_FILE="${BACKUP_DIR}/postgres_${DATE}.sql.gz"
log "Dumping postgres → ${PG_FILE}"
docker exec aust_postgres pg_dump -U aust -d aust_backend | gzip > "${PG_FILE}"
log "Postgres done: $(du -sh "${PG_FILE}" | cut -f1)"

# ---------------------------------------------------------------------------
# 2. MinIO volume snapshot
# ---------------------------------------------------------------------------
MINIO_FILE="${BACKUP_DIR}/minio_${DATE}.tar.gz"
log "Snapshotting minio_data → ${MINIO_FILE}"
docker run --rm \
    -v aust_minio_data:/data:ro \
    -v "${BACKUP_DIR}:/backup" \
    alpine \
    tar czf "/backup/minio_${DATE}.tar.gz" -C /data .
log "MinIO done: $(du -sh "${MINIO_FILE}" | cut -f1)"

# ---------------------------------------------------------------------------
# 3. Rotate old backups
# ---------------------------------------------------------------------------
log "Rotating backups older than ${RETAIN_DAYS} days"
find "${BACKUP_DIR}" -maxdepth 1 -name "postgres_*.sql.gz" -mtime +${RETAIN_DAYS} -delete
find "${BACKUP_DIR}" -maxdepth 1 -name "minio_*.tar.gz"   -mtime +${RETAIN_DAYS} -delete

log "Backup complete. Current backups:"
ls -lh "${BACKUP_DIR}"
