#!/usr/bin/env bash
# restore-local.sh — Restore the latest pulled backup into local staging containers.
#
# Prerequisites:
#   - bash scripts/pull-backups.sh has been run
#   - docker containers aust_staging_postgres + aust_staging_minio are running
#
# Usage:
#   bash scripts/restore-local.sh                    # use newest backup in ~/aust-backups/
#   bash scripts/restore-local.sh 20260418_060104    # use a specific timestamp
#   bash scripts/restore-local.sh -y                 # skip confirmation prompt (non-interactive)
#   bash scripts/restore-local.sh -y 20260418_060104 # skip prompt + specific timestamp

set -euo pipefail

BACKUP_DIR="${HOME}/aust-backups"
PG_CONTAINER="aust_staging_postgres"
PG_USER="aust_staging"
PG_DB="aust_staging"
MINIO_VOLUME="aust-staging_staging_minio_data"

GREEN="\033[0;32m"; RED="\033[0;31m"; BOLD="\033[1m"; RESET="\033[0m"
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
fail() { echo -e "  ${RED}FAIL${RESET} ${1}" >&2; exit 1; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

[[ -d "${BACKUP_DIR}" ]] || fail "${BACKUP_DIR} not found — run scripts/pull-backups.sh first"

# ---------------------------------------------------------------------------
# Parse flags
# ---------------------------------------------------------------------------
YES=0
STAMP=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        -y|--yes) YES=1; shift ;;
        -*) fail "Unknown flag: $1" ;;
        *)  STAMP="$1"; shift ;;
    esac
done

# Pick timestamp: explicit arg, else newest postgres dump
if [[ -z "${STAMP}" ]]; then
    STAMP=$(ls -1 "${BACKUP_DIR}"/postgres_*.sql.gz 2>/dev/null \
            | sed -E 's|.*postgres_(.+)\.sql\.gz|\1|' \
            | sort | tail -1)
fi
[[ -n "${STAMP}" ]] || fail "No postgres_*.sql.gz in ${BACKUP_DIR}"

PG_FILE="${BACKUP_DIR}/postgres_${STAMP}.sql.gz"
MINIO_FILE="${BACKUP_DIR}/minio_${STAMP}.tar.gz"
[[ -f "${PG_FILE}" ]]    || fail "Missing ${PG_FILE}"
[[ -f "${MINIO_FILE}" ]] || fail "Missing ${MINIO_FILE}"

echo -e "${BOLD}Restoring backup: ${STAMP}${RESET}"
echo "  Postgres: ${PG_FILE}"
echo "  MinIO:    ${MINIO_FILE}"
echo "  Target PG: ${PG_CONTAINER} (user=${PG_USER}, db=${PG_DB})"
echo "  Target MinIO volume: ${MINIO_VOLUME}"
if [[ "${YES}" -eq 1 ]]; then
    echo "  (non-interactive mode — skipping confirmation)"
else
    read -rp "Proceed? This will WIPE the local staging DB + MinIO data [y/N] " reply
    [[ "${reply}" =~ ^[Yy]$ ]] || fail "Aborted"
fi

# ---------------------------------------------------------------------------
# 1. Restore Postgres (drop + recreate DB, then import dump)
# ---------------------------------------------------------------------------
step "Restoring Postgres"
docker exec "${PG_CONTAINER}" psql -U "${PG_USER}" -d postgres -c \
    "DROP DATABASE IF EXISTS ${PG_DB} WITH (FORCE);" >/dev/null
docker exec "${PG_CONTAINER}" psql -U "${PG_USER}" -d postgres -c \
    "CREATE DATABASE ${PG_DB};" >/dev/null

# Backup was taken with user `aust` (prod); remap role ownership to `aust_staging` during import.
gunzip -c "${PG_FILE}" \
    | sed -e 's/OWNER TO aust;/OWNER TO aust_staging;/g' \
          -e 's/Owner: aust$/Owner: aust_staging/g' \
    | docker exec -i "${PG_CONTAINER}" psql -U "${PG_USER}" -d "${PG_DB}" >/dev/null
ok "Postgres restored"

# ---------------------------------------------------------------------------
# 2. Restore MinIO volume (wipe + extract tar into fresh volume)
# ---------------------------------------------------------------------------
step "Restoring MinIO volume"
# Stop minio so we can safely replace its data
MINIO_CONTAINER=$(docker ps --filter "volume=${MINIO_VOLUME}" --format '{{.Names}}' | head -1 || true)
[[ -n "${MINIO_CONTAINER}" ]] && docker stop "${MINIO_CONTAINER}" >/dev/null

docker run --rm \
    -v "${MINIO_VOLUME}:/data" \
    -v "${BACKUP_DIR}:/backup:ro" \
    alpine sh -c "rm -rf /data/* /data/.[!.]* /data/..?* 2>/dev/null; tar xzf /backup/$(basename "${MINIO_FILE}") -C /data"

[[ -n "${MINIO_CONTAINER}" ]] && docker start "${MINIO_CONTAINER}" >/dev/null
ok "MinIO restored"

# ---------------------------------------------------------------------------
# 3. Summary
# ---------------------------------------------------------------------------
ROWS=$(docker exec "${PG_CONTAINER}" psql -U "${PG_USER}" -d "${PG_DB}" -tAc \
    "SELECT COUNT(*) FROM inquiries;" 2>/dev/null || echo "?")
echo -e "\n${GREEN}${BOLD}Restore complete.${RESET} inquiries=${ROWS}"
