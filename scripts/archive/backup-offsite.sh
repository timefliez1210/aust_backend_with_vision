#!/usr/bin/env bash
set -euo pipefail

# Offsite backup: upload latest PostgreSQL + MinIO backups to KAS FTP
# Usage: ./scripts/backup-offsite.sh [--fresh]
#   --fresh  Run backup-db.sh and backup-minio.sh first (default: upload most recent existing)
#
# Reads FTP credentials from frontend/.env (same as deploy scripts).
# Uploads to /backups/ on the FTP server. Only keeps the single most recent
# backup per type — previous one is deleted before uploading.
# The /backups/ directory is blocked via .htaccess (RedirectMatch 403).

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
FRONTEND_ENV="${PROJECT_DIR}/frontend/.env"
DB_BACKUP_DIR="${PROJECT_DIR}/backups/db"
MINIO_BACKUP_DIR="${PROJECT_DIR}/backups/minio"

# ── Load FTP credentials from frontend/.env ──────────────────────────
if [ ! -f "${FRONTEND_ENV}" ]; then
    echo "ERROR: ${FRONTEND_ENV} not found — copy frontend/.env.example and set FTP_PASS"
    exit 1
fi

# shellcheck disable=SC1090
set -a
source <(grep -E '^(FTP_HOST|FTP_USER|FTP_PASS)=' "${FRONTEND_ENV}" | sed 's/^/export /')
set +a

if [ -z "${FTP_PASS:-}" ]; then
    echo "ERROR: FTP_PASS not set in ${FRONTEND_ENV}"
    exit 1
fi

FTP_HOST="${FTP_HOST:-w019276c.kasserver.com}"
FTP_USER="${FTP_USER:-f0180dc8}"

# ── Optionally create fresh backups ──────────────────────────────────
if [ "${1:-}" = "--fresh" ]; then
    echo "Creating fresh backups..."
    "${PROJECT_DIR}/scripts/backup-db.sh"
    "${PROJECT_DIR}/scripts/backup-minio.sh"
    echo ""
fi

# ── Find latest backup files ─────────────────────────────────────────
LATEST_DB=$(ls -1t "${DB_BACKUP_DIR}"/aust_backend_*.sql.gz 2>/dev/null | head -1 || true)
LATEST_MINIO=$(ls -1t "${MINIO_BACKUP_DIR}"/minio_*.tar.gz 2>/dev/null | head -1 || true)

if [ -z "${LATEST_DB}" ] && [ -z "${LATEST_MINIO}" ]; then
    echo "ERROR: No backup files found. Run with --fresh or run backup scripts first."
    exit 1
fi

echo "=== Offsite backup to ${FTP_HOST} ==="
[ -n "${LATEST_DB}" ] && echo "  DB:    $(basename "${LATEST_DB}") ($(du -h "${LATEST_DB}" | cut -f1))"
[ -n "${LATEST_MINIO}" ] && echo "  MinIO: $(basename "${LATEST_MINIO}") ($(du -h "${LATEST_MINIO}" | cut -f1))"

# ── Upload via Python (matches existing FTP pattern) ─────────────────
python3 - "${FTP_HOST}" "${FTP_USER}" "${FTP_PASS}" "${LATEST_DB:-}" "${LATEST_MINIO:-}" <<'PYTHON'
import ftplib
import os
import sys

ftp_host, ftp_user, ftp_pass = sys.argv[1], sys.argv[2], sys.argv[3]
db_file = sys.argv[4] if sys.argv[4] else None
minio_file = sys.argv[5] if sys.argv[5] else None

REMOTE_DIR = "/backups"

ftp = ftplib.FTP_TLS(ftp_host)
ftp.login(ftp_user, ftp_pass)
ftp.prot_p()
ftp.encoding = "utf-8"
print(f"Connected to {ftp_host}")

# Create /backups/ if needed
try:
    ftp.mkd(REMOTE_DIR)
    print(f"Created {REMOTE_DIR}/")
except ftplib.error_perm:
    pass

def upload_replacing(local_path, prefix):
    """Delete any existing file with the same prefix, then upload the new one."""
    if not local_path:
        return
    basename = os.path.basename(local_path)
    remote_path = f"{REMOTE_DIR}/{basename}"
    size_kb = os.path.getsize(local_path) / 1024

    # Delete previous backup(s) matching prefix
    try:
        for f in ftp.nlst(REMOTE_DIR):
            name = os.path.basename(f)
            if name.startswith(prefix) and name != basename:
                ftp.delete(f"{REMOTE_DIR}/{name}")
                print(f"  Deleted old: {name}")
    except ftplib.error_perm:
        pass

    # Upload new
    print(f"Uploading {basename} ({size_kb:.0f} KB)...")
    with open(local_path, "rb") as f:
        ftp.storbinary(f"STOR {remote_path}", f)
    print(f"  Done: {basename}")

upload_replacing(db_file, "aust_backend_")
upload_replacing(minio_file, "minio_")

ftp.quit()
print("\nOffsite backup complete.")
PYTHON

echo ""
echo "Done. Backups uploaded to ${FTP_HOST}:/backups/"
