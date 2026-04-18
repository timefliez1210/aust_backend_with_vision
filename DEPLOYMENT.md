# DEPLOYMENT

Covers local staging, backup/restore, and production deployment for the AUST backend.

---

## 1. Architecture Overview

```
Dev laptop
  │
  │  ssh -i ~/.ssh/id_ed25519 root@72.62.89.179
  │  scp / rsync
  ▼
VPS (Hostinger 72.62.89.179)
  │
  │  /opt/aust/docker-compose.yml  (= docker/docker-compose.prod.yml)
  │  /opt/aust/.env                (secrets — NOT in repo)
  │  /opt/aust/migrations/         (uploaded by deploy-prod.sh)
  │  /opt/aust/backups/            (daily postgres + minio dumps)
  │
  ├─ container: aust_postgres      (postgres:16-alpine, 127.0.0.1:5432)
  ├─ container: aust_minio         (minio/minio, 127.0.0.1:9000/9001)
  └─ container: aust_backend       (Dockerfile.backend, 127.0.0.1:8080)
       │
       │  HTTP on 127.0.0.1:8080 (not exposed to internet directly)
       ▼
  Cloudflare Tunnel (cloudflared daemon on VPS)
       │
       ▼
  https://aufraeumhelden.com   (public domain, Cloudflare-proxied)

Frontend (SvelteKit)
  — currently deployed via FTP to KAS shared hosting
  — migration to container in progress (Dockerfile.frontend exists,
    staging already runs it; prod containerisation not yet done)
```

---

## 2. Local Staging Stack

### Compose file

`docker/docker-compose.staging.yml` — compose project name `aust-staging`.

| Container                  | Image                   | Host port(s)         | Purpose                      |
|----------------------------|-------------------------|----------------------|------------------------------|
| `aust_staging_postgres`    | postgres:16-alpine      | `5435→5432`          | Isolated staging DB          |
| `aust_staging_minio`       | minio/minio             | `9010→9000`, `9011→9001` | Object storage            |
| `aust_staging_minio_setup` | minio/mc                | —                    | Creates bucket, then exits   |
| `aust_staging_mailpit`     | axllent/mailpit         | `1025→1025`, `8025→8025` | Fake SMTP + web UI       |
| `aust_staging_backend`     | Dockerfile.backend      | `8099→8080`          | Backend (RUN_MODE=staging)   |
| `aust_staging_frontend`    | Dockerfile.frontend     | `4173→80`            | SvelteKit (VITE_API_BASE=http://localhost:8099) |

DB credentials: user `aust_staging`, password `aust_staging_password`, db `aust_staging`.

All service-to-service URLs use compose service hostnames (e.g. `staging-postgres`, `staging-minio`).
LLM defaults to Ollama on `host.docker.internal:11434` (model `qwen2.5:7b`).
Override API keys and tokens in `docker/.env.staging` (not required to start).

### Wrapper script

```
bash scripts/staging-up.sh [FLAG]
```

| Flag             | Behaviour                                                                         |
|------------------|-----------------------------------------------------------------------------------|
| *(none)*         | `docker compose up -d`, waits for postgres + backend healthy, prints URL summary  |
| `--rebuild`      | Same as above but passes `--build` to force image rebuild for backend + frontend  |
| `--restore`      | Runs `pull-backups.sh` then `restore-local.sh -y`, then starts stack              |
| `--restore-only` | Runs `restore-local.sh -y` (skips VPS pull), then starts stack                   |
| `--down`         | `docker compose down` — stops containers, preserves volumes                       |
| `--nuke`         | `docker compose down -v` — stops containers AND deletes all staging volumes (prompts "yes") |
| `--logs`         | `docker compose logs -f staging-backend staging-frontend`                         |

After a successful `up`, URLs are printed:

```
Backend   : http://localhost:8099
Frontend  : http://localhost:4173
Mailpit   : http://localhost:8025
MinIO UI  : http://localhost:9011
```

Health timeout is 120 s; the script polls every 5 s and exits non-zero on `unhealthy`.

---

## 3. Backup & Restore Pipeline

### VPS — daily cron

`scripts/setup-backups.sh` (run once from local machine, requires SSH access):
- Uploads `scripts/backup.sh` to `/opt/aust/backup.sh`.
- Creates `/opt/aust/backups/`.
- Installs cron entry: `0 3 * * * /opt/aust/backup.sh >> /var/log/aust-backup.log 2>&1`
- Runs the script immediately to verify.

`backup.sh` (runs on VPS):
1. `pg_dump -U aust -d aust_backend | gzip` → `/opt/aust/backups/postgres_TIMESTAMP.sql.gz`
2. `docker run alpine tar czf` over volume `aust_minio_data` → `/opt/aust/backups/minio_TIMESTAMP.tar.gz`
3. Deletes files older than 7 days.

Timestamp format: `YYYYmmdd_HHMMSS` (e.g. `20260418_030001`).

### Pull to local machine

```bash
bash scripts/pull-backups.sh
```

Rsyncs `/opt/aust/backups/` on the VPS to `~/aust-backups/` on the dev machine.
Requires SSH access to `root@72.62.89.179` (ProtonVPN may be needed).

### Restore into staging containers

```bash
bash scripts/restore-local.sh                    # newest backup
bash scripts/restore-local.sh 20260418_030001    # specific timestamp
bash scripts/restore-local.sh -y                 # skip confirmation
bash scripts/restore-local.sh -y 20260418_030001
```

Prerequisites: `aust_staging_postgres` and `aust_staging_minio` containers are running.

What it does:
1. **Postgres**: drops `aust_staging`, recreates it, streams the `.sql.gz` through `sed` to remap
   `OWNER TO aust` → `OWNER TO aust_staging`, then pipes into `psql` inside the container.
2. **MinIO**: stops the MinIO container, wipes volume `aust-staging_staging_minio_data` via an
   ephemeral Alpine container, extracts the `.tar.gz` into the volume, restarts the container.
3. Prints row count from `inquiries` as a sanity check.

`--restore` and `--restore-only` in `staging-up.sh` call this script automatically with `-y`.

---

## 4. Production Deployment

### docker-compose.prod.yml

Located at `docker/docker-compose.prod.yml` on the repo; uploaded to `/opt/aust/docker-compose.yml` on the VPS.

Services: `postgres`, `minio`, `minio-setup`, `backend`.

Key backend service config:
- Build: `docker/Dockerfile.backend`, context = repo root.
- `env_file: /opt/aust/.env` (required — deploy fails if absent).
- `environment.RUN_MODE: production`.
- Port binding: `127.0.0.1:8080:8080` (loopback only; Cloudflare tunnel reaches it from there).
- Volume: `/opt/aust/migrations:/app/migrations:ro`.
- Healthcheck: `curl -f http://localhost:8080/health`, 30 s start period, 5 s interval.

### Regular deploy

```bash
bash scripts/deploy-prod.sh
```

Pre-flight checks: working tree clean, on `main` branch, SSH reachable, `/opt/aust/docker-compose.yml` present.

Steps:
1. Build `aust_backend:latest` locally from `docker/Dockerfile.backend`.
2. Tag existing VPS image as `aust_backend:previous` (rollback anchor).
3. `docker save | gzip` → `/tmp/aust_backend.tar.gz`.
4. `scp` tarball to VPS `/tmp/`.
5. `docker load` on VPS, delete tarball.
6. Upload `migrations/` to `/opt/aust/migrations/`.
7. `docker compose up -d backend` on VPS.
8. Health poll: 12 attempts × 5 s. Prints rollback command on failure.

### One-time systemd → container cutover

```bash
bash scripts/cutover-systemd-to-container.sh
```

Steps:
1. `systemctl stop aust-backend && systemctl disable aust-backend`.
2. Uploads `docker/docker-compose.prod.yml` → `/opt/aust/docker-compose.yml`.
3. Calls `deploy-prod.sh` (builds + loads image + starts container).
4. Verifies `https://aufraeumhelden.com/health`.

Expected downtime: ~30 seconds. The binary at `/opt/aust/bin/aust_backend` is **not** deleted;
emergency rollback: `systemctl enable aust-backend && systemctl start aust-backend`.

---

## 5. Open Follow-ups

These must be addressed before (or as part of) the cutover:

**`/opt/aust/.env` hostname check** — The env file on the VPS must use compose service names, not
`localhost`, for internal service URLs. Verify before running the cutover script:
- `DATABASE_URL` (or `AUST__DATABASE__URL`) must point to `postgres:5432`, not `localhost:5432`.
- S3 endpoint must point to `http://minio:9000`, not `http://localhost:9000`.
If either still references `localhost`, the backend container will fail to connect to its dependencies.

**Frontend not containerized for prod** — `Dockerfile.frontend` exists and works in staging, but
production frontend is still deployed via FTP to KAS shared hosting. The prod compose file has no
`frontend` service. Containerising prod frontend requires a separate Nginx/Caddy reverse proxy or
Cloudflare routing change.

**Mailpit healthcheck cosmetic failure** — The `staging-mailpit` container healthcheck reports
unhealthy in some environments. This does not block staging operation; the container serves SMTP
and the web UI regardless.

---

## 6. Rollback

**Backend (Docker)**

```bash
ssh -i ~/.ssh/id_ed25519 root@72.62.89.179 \
  'docker tag aust_backend:previous aust_backend:latest \
   && cd /opt/aust && docker compose up -d backend'
```

`deploy-prod.sh` tags the previous image as `:previous` before each deploy, so one rollback step
is always available.

**Database** — No rollback needed. All migrations are additive-only (columns/tables added, never
dropped or altered destructively). Rolling back the binary without touching the DB is safe.

**Emergency (pre-container)** — If the container is broken and the systemd binary is still present:

```bash
ssh -i ~/.ssh/id_ed25519 root@72.62.89.179 \
  'systemctl enable aust-backend && systemctl start aust-backend'
```
