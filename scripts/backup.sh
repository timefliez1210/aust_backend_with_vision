#!/usr/bin/env bash
# backup.sh — Daily backup of postgres + minio (runs ON the VPS via cron)
#
# Saves to /opt/aust/backups/, retains 7 days.
# Install: bash scripts/setup-backups.sh

set -euo pipefail

BACKUP_DIR="/opt/aust/backups"
DATE=$(date +%Y%m%d_%H%M%S)
RETAIN_DAYS=7
ALERT_LOG="/var/log/aust-backup-alerts.log"

mkdir -p "${BACKUP_DIR}"

log() { echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"; }

# ---------------------------------------------------------------------------
# Telegram alert helper (reads from /opt/aust/.env if available)
# ---------------------------------------------------------------------------
send_telegram_alert() {
    local message="$1"
    local bot_token=""
    local chat_id=""

    if [[ -f /opt/aust/.env ]]; then
        bot_token=$(grep -E '^TELEGRAM__BOT_TOKEN=' /opt/aust/.env | cut -d= -f2- | tr -d '"' | tr -d "'")
        chat_id=$(grep -E '^ADMIN_CHAT_ID=' /opt/aust/.env | cut -d= -f2- | tr -d '"' | tr -d "'")
    fi

    local timestamp
    timestamp=$(date '+%Y-%m-%d %H:%M:%S')

    if [[ -n "${bot_token}" && -n "${chat_id}" ]]; then
        curl -s -X POST "https://api.telegram.org/bot${bot_token}/sendMessage" \
            -d "chat_id=${chat_id}" \
            -d "text=${message}" \
            -d "parse_mode=HTML" > /dev/null 2>&1 || true
        log "Telegram alert sent"
    else
        log "ALERT (Telegram not configured, writing to ${ALERT_LOG}): ${message}"
        echo "[${timestamp}] ${message}" >> "${ALERT_LOG}"
    fi
}

# ---------------------------------------------------------------------------
# 1. PostgreSQL dump
# ---------------------------------------------------------------------------
PG_FILE="${BACKUP_DIR}/postgres_${DATE}.sql.gz"
log "Dumping postgres → ${PG_FILE}"
docker exec aust_postgres pg_dump -U aust -d aust_backend | gzip > "${PG_FILE}"
PG_BYTES=$(stat -c%s "${PG_FILE}")
log "Postgres done: $(du -sh "${PG_FILE}" | cut -f1) (${PG_BYTES} bytes)"

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
MINIO_BYTES=$(stat -c%s "${MINIO_FILE}")
log "MinIO done: $(du -sh "${MINIO_FILE}" | cut -f1) (${MINIO_BYTES} bytes)"

# ---------------------------------------------------------------------------
# 3. MinIO size sanity check
#    Alert if: < 100 KB, OR more than 50% smaller than previous day's backup
# ---------------------------------------------------------------------------
MIN_BYTES=102400  # 100 KB
MINIO_ALERT=""

if [[ "${MINIO_BYTES}" -lt "${MIN_BYTES}" ]]; then
    MINIO_ALERT="MinIO tarball is dangerously small: ${MINIO_BYTES} bytes (threshold: ${MIN_BYTES}). Possible data loss!"
fi

if [[ -z "${MINIO_ALERT}" ]]; then
    # Find the previous minio tarball (second most recent, excluding the one we just created)
    PREV_MINIO=$(find "${BACKUP_DIR}" -maxdepth 1 -name "minio_*.tar.gz" ! -name "minio_${DATE}.tar.gz" \
        -printf '%T@ %p\n' | sort -rn | head -1 | awk '{print $2}')
    if [[ -n "${PREV_MINIO}" ]]; then
        PREV_BYTES=$(stat -c%s "${PREV_MINIO}")
        if [[ "${PREV_BYTES}" -gt 0 ]]; then
            # Alert if current is less than 50% of previous
            THRESHOLD=$(( PREV_BYTES / 2 ))
            if [[ "${MINIO_BYTES}" -lt "${THRESHOLD}" ]]; then
                MINIO_ALERT="MinIO tarball shrank by more than 50%: was ${PREV_BYTES} bytes, now ${MINIO_BYTES} bytes. Possible data loss or volume wipe!"
            fi
        fi
    fi
fi

if [[ -n "${MINIO_ALERT}" ]]; then
    log "WARNING: ${MINIO_ALERT}"
    HOSTNAME_LABEL=$(hostname -s 2>/dev/null || echo "VPS")
    send_telegram_alert "&#x26A0;&#xFE0F; <b>AUST Backup Alert [${HOSTNAME_LABEL}]</b>&#10;${MINIO_ALERT}&#10;&#10;Check /var/log/aust-backup.log and /var/lib/docker/volumes/aust_minio_data/_data/"
fi

# ---------------------------------------------------------------------------
# 4. Rotate old backups
# ---------------------------------------------------------------------------
log "Rotating backups older than ${RETAIN_DAYS} days"
find "${BACKUP_DIR}" -maxdepth 1 -name "postgres_*.sql.gz" -mtime +${RETAIN_DAYS} -delete
find "${BACKUP_DIR}" -maxdepth 1 -name "minio_*.tar.gz"   -mtime +${RETAIN_DAYS} -delete

log "Backup complete. Current backups:"
ls -lh "${BACKUP_DIR}"
