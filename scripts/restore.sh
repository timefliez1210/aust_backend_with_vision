#!/usr/bin/env bash
set -euo pipefail

# Full restore script — PostgreSQL + MinIO
#
# Usage:
#   ./scripts/restore.sh                         # restore latest of both
#   ./scripts/restore.sh --db-only               # restore latest DB backup only
#   ./scripts/restore.sh --minio-only            # restore latest MinIO backup only
#   ./scripts/restore.sh --db   backups/db/aust_backend_20260305_063102.sql.gz
#   ./scripts/restore.sh --minio backups/minio/minio_20260305_063102.tar.gz
#
# The script:
#   1. Stops aust-backend (so nothing holds DB connections)
#   2. Drops + recreates the DB cleanly (avoids all FK ordering issues)
#   3. Restores PostgreSQL dump in one shot
#   4. Restores MinIO bucket from tar.gz
#   5. Restarts aust-backend and verifies health

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DB_BACKUP_DIR="${PROJECT_DIR}/backups/db"
MINIO_BACKUP_DIR="${PROJECT_DIR}/backups/minio"
PG_CONTAINER="aust_postgres"
MINIO_CONTAINER="aust_minio"
DB_NAME="aust_backend"
DB_USER="aust"
BUCKET="aust-uploads"

# ── Argument parsing ─────────────────────────────────────────────────────────
RESTORE_DB=true
RESTORE_MINIO=true
DB_FILE=""
MINIO_FILE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --db-only)    RESTORE_MINIO=false; shift ;;
        --minio-only) RESTORE_DB=false;    shift ;;
        --db)         DB_FILE="$2";        shift 2 ;;
        --minio)      MINIO_FILE="$2";     shift 2 ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
done

# ── Preflight ────────────────────────────────────────────────────────────────
if ! docker info >/dev/null 2>&1; then
    echo "ERROR: Docker is not running"
    exit 1
fi

echo "========================================"
echo " AUST Backend — Full Restore"
echo "========================================"

# Stop the backend so it doesn't hold DB connections during restore
if systemctl is-active --quiet aust-backend 2>/dev/null; then
    echo "Stopping aust-backend..."
    sudo systemctl stop aust-backend
    BACKEND_WAS_RUNNING=true
else
    BACKEND_WAS_RUNNING=false
fi

# ── PostgreSQL restore ────────────────────────────────────────────────────────
if $RESTORE_DB; then
    if [[ -z "$DB_FILE" ]]; then
        DB_FILE=$(ls -1t "${DB_BACKUP_DIR}"/aust_backend_*.sql.gz 2>/dev/null | head -1)
        if [[ -z "$DB_FILE" ]]; then
            echo "ERROR: No DB backups found in ${DB_BACKUP_DIR}"
            exit 1
        fi
    fi

    echo ""
    echo "DB backup : ${DB_FILE}"

    if ! docker ps --format '{{.Names}}' | grep -q "^${PG_CONTAINER}$"; then
        echo "ERROR: Container '${PG_CONTAINER}' is not running — start it first:"
        echo "  cd docker && docker compose up -d postgres"
        exit 1
    fi

    # Wait for postgres to be ready
    for i in $(seq 1 10); do
        docker exec "${PG_CONTAINER}" pg_isready -U "${DB_USER}" >/dev/null 2>&1 && break
        echo "  Waiting for PostgreSQL... (${i}/10)"
        sleep 2
    done

    echo "Dropping and recreating '${DB_NAME}'..."
    # Terminate any remaining connections
    docker exec "${PG_CONTAINER}" psql -U "${DB_USER}" -d postgres -q \
        -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='${DB_NAME}' AND pid <> pg_backend_pid();" \
        >/dev/null 2>&1 || true
    docker exec "${PG_CONTAINER}" psql -U "${DB_USER}" -d postgres -q \
        -c "DROP DATABASE IF EXISTS ${DB_NAME};" >/dev/null
    docker exec "${PG_CONTAINER}" psql -U "${DB_USER}" -d postgres -q \
        -c "CREATE DATABASE ${DB_NAME} OWNER ${DB_USER};" >/dev/null

    echo "Restoring PostgreSQL..."
    # Suppress routine noise but let real ERRORs through
    gunzip -c "${DB_FILE}" \
        | docker exec -i "${PG_CONTAINER}" psql -U "${DB_USER}" -d "${DB_NAME}" -q 2>&1 \
        | grep "^ERROR" || true

    # Verify
    COUNTS=$(docker exec "${PG_CONTAINER}" psql -U "${DB_USER}" -d "${DB_NAME}" -tA \
        -c "SELECT 'inquiries:'||count(*) FROM inquiries UNION ALL
            SELECT 'offers:'||count(*) FROM offers UNION ALL
            SELECT 'customers:'||count(*) FROM customers UNION ALL
            SELECT 'calendar_bookings:'||count(*) FROM calendar_bookings UNION ALL
            SELECT 'email_threads:'||count(*) FROM email_threads;" 2>/dev/null)
    echo "PostgreSQL restored:"
    echo "${COUNTS}" | sed 's/^/  /'
fi

# ── MinIO restore ─────────────────────────────────────────────────────────────
if $RESTORE_MINIO; then
    if [[ -z "$MINIO_FILE" ]]; then
        MINIO_FILE=$(ls -1t "${MINIO_BACKUP_DIR}"/minio_*.tar.gz 2>/dev/null | head -1 || true)
    fi

    if ! docker ps --format '{{.Names}}' | grep -q "^${MINIO_CONTAINER}$"; then
        echo "ERROR: Container '${MINIO_CONTAINER}' is not running — start it first:"
        echo "  cd docker && docker compose up -d minio"
        exit 1
    fi

    echo ""
    if [[ -n "$MINIO_FILE" ]]; then
        echo "MinIO backup: ${MINIO_FILE}"
        echo "Restoring MinIO bucket '${BUCKET}'..."
        docker exec "${MINIO_CONTAINER}" rm -rf "/data/${BUCKET}" 2>/dev/null || true
        gunzip -c "${MINIO_FILE}" | docker exec -i "${MINIO_CONTAINER}" tar -xf - -C /data 2>/dev/null || true
        FILE_COUNT=$(docker exec "${MINIO_CONTAINER}" find "/data/${BUCKET}" -type f 2>/dev/null | wc -l || echo "0")
        echo "MinIO restored — ${FILE_COUNT} file(s) in bucket."
    else
        echo "MinIO backup: (none found — creating empty bucket)"
    fi

    # Always ensure bucket exists with correct policy
    docker exec "${MINIO_CONTAINER}" mc alias set local http://localhost:9000 minioadmin minioadmin >/dev/null 2>&1 || true
    docker exec "${MINIO_CONTAINER}" mc mb --ignore-existing "local/${BUCKET}" >/dev/null 2>&1 || true
    docker exec "${MINIO_CONTAINER}" mc anonymous set download "local/${BUCKET}" >/dev/null 2>&1 || true
    echo "Bucket '${BUCKET}' ready."
fi

# ── Restart backend ───────────────────────────────────────────────────────────
echo ""
if $BACKEND_WAS_RUNNING; then
    echo "Restarting aust-backend..."
    sudo systemctl start aust-backend
    sleep 3
    for i in $(seq 1 5); do
        if curl -sf http://localhost:8080/health >/dev/null 2>&1; then
            echo "Health check passed."
            break
        fi
        echo "  Waiting for backend... (${i}/5)"
        sleep 3
    done
fi

echo ""
echo "========================================"
echo " Restore complete."
echo "========================================"
