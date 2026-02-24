#!/usr/bin/env bash
set -euo pipefail

# Standalone PostgreSQL backup script
# Usage: ./scripts/backup-db.sh
# Creates a gzipped pg_dump in backups/db/ and rotates old backups

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BACKUP_DIR="${PROJECT_DIR}/backups/db"
CONTAINER="aust_postgres"
DB_NAME="aust_backend"
DB_USER="aust"
MAX_BACKUPS=30

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_FILE="${BACKUP_DIR}/aust_backend_${TIMESTAMP}.sql.gz"

mkdir -p "${BACKUP_DIR}"

# Check Docker is running
if ! docker info >/dev/null 2>&1; then
    echo "ERROR: Docker is not running"
    exit 1
fi

# Check postgres container is running
if ! docker ps --format '{{.Names}}' | grep -q "^${CONTAINER}$"; then
    echo "ERROR: Container '${CONTAINER}' is not running"
    exit 1
fi

# Check postgres is healthy
if ! docker exec "${CONTAINER}" pg_isready -U "${DB_USER}" -d "${DB_NAME}" >/dev/null 2>&1; then
    echo "ERROR: PostgreSQL is not ready"
    exit 1
fi

echo "Backing up ${DB_NAME}..."
docker exec "${CONTAINER}" pg_dump -U "${DB_USER}" "${DB_NAME}" | gzip > "${BACKUP_FILE}"

BACKUP_SIZE=$(du -h "${BACKUP_FILE}" | cut -f1)
echo "Backup created: ${BACKUP_FILE} (${BACKUP_SIZE})"

# Rotate old backups (keep last MAX_BACKUPS)
BACKUP_COUNT=$(ls -1 "${BACKUP_DIR}"/aust_backend_*.sql.gz 2>/dev/null | wc -l)
if [ "${BACKUP_COUNT}" -gt "${MAX_BACKUPS}" ]; then
    REMOVE_COUNT=$((BACKUP_COUNT - MAX_BACKUPS))
    ls -1t "${BACKUP_DIR}"/aust_backend_*.sql.gz | tail -n "${REMOVE_COUNT}" | xargs rm -f
    echo "Rotated ${REMOVE_COUNT} old backup(s), keeping last ${MAX_BACKUPS}"
fi
