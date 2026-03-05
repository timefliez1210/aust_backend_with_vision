#!/usr/bin/env bash
set -euo pipefail

# MinIO backup script
# Usage: ./scripts/backup-minio.sh
# Creates a tar.gz snapshot of the aust-uploads bucket in backups/minio/ and rotates old backups

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BACKUP_DIR="${PROJECT_DIR}/backups/minio"
CONTAINER="aust_minio"
BUCKET="aust-uploads"
MAX_BACKUPS=30

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_FILE="${BACKUP_DIR}/minio_${TIMESTAMP}.tar.gz"

mkdir -p "${BACKUP_DIR}"

# Check Docker is running
if ! docker info >/dev/null 2>&1; then
    echo "ERROR: Docker is not running"
    exit 1
fi

# Check minio container is running
if ! docker ps --format '{{.Names}}' | grep -q "^${CONTAINER}$"; then
    echo "ERROR: Container '${CONTAINER}' is not running"
    exit 1
fi

echo "Backing up MinIO bucket '${BUCKET}'..."

# Get the data directory inside the container and tar it out
docker exec "${CONTAINER}" tar -czf - -C /data "${BUCKET}" 2>/dev/null > "${BACKUP_FILE}" || {
    # Bucket may be empty — still create an archive so restore logic is consistent
    echo "  (bucket is empty or tar warning — creating empty archive)"
    tar -czf "${BACKUP_FILE}" -T /dev/null
}

BACKUP_SIZE=$(du -h "${BACKUP_FILE}" | cut -f1)
echo "Backup created: ${BACKUP_FILE} (${BACKUP_SIZE})"

# Rotate old backups (keep last MAX_BACKUPS)
BACKUP_COUNT=$(ls -1 "${BACKUP_DIR}"/minio_*.tar.gz 2>/dev/null | wc -l)
if [ "${BACKUP_COUNT}" -gt "${MAX_BACKUPS}" ]; then
    REMOVE_COUNT=$((BACKUP_COUNT - MAX_BACKUPS))
    ls -1t "${BACKUP_DIR}"/minio_*.tar.gz | tail -n "${REMOVE_COUNT}" | xargs rm -f
    echo "Rotated ${REMOVE_COUNT} old backup(s), keeping last ${MAX_BACKUPS}"
fi
