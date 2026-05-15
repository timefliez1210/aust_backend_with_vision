# Redeploy to a New VPS

The backend currently runs from the laptop (2026-05-15 takeover). When Alex
provisions a new VPS, follow this runbook to move it back.

## Prerequisites
- Fresh VPS (Debian 12 recommended) with a public IP and SSH access as root.
- A new Cloudflare Tunnel token for `api.aufraeumhelden.com` (create in
  Cloudflare Zero Trust → Networks → Tunnels), OR plan to install one
  interactively on the VPS.

## 1. Bootstrap

```
bash scripts/bootstrap-new-vps.sh <VPS_IP> root <CLOUDFLARED_TOKEN>
```

What it does:
- Installs Docker + Compose, cloudflared, ufw.
- Creates `/opt/aust/{migrations,backups,config,bin}`.
- Drops in the compose file, migrations, backup script, and `.env.example`.
- Registers cloudflared as a systemd service using the token you pass.

## 2. Fill in secrets

SSH to the VPS and edit `/opt/aust/.env`. The current laptop `.env` is the
authoritative source — copy values from `/media/timefliez/FileSystem/projects/aust_backend/.env`
(keep `AUST__DATABASE__URL` and `AUST__STORAGE__ENDPOINT` pointing at
`localhost` — compose overrides them).

## 3. First deploy (build + push images)

```
VPS_IP=<VPS_IP> bash scripts/deploy-prod.sh
```

This builds backend + flash-bot Docker images locally, ships them, runs
migrations, restarts containers, health-checks. (`scripts/deploy-prod.sh`
currently hard-codes `VPS_IP=72.62.89.179` — edit the constant or wrap with
an env override before running.)

## 4. Move live data from the laptop

```
bash scripts/migrate-from-laptop.sh <VPS_IP>
```

Stops local services briefly, snapshots Postgres + MinIO, restores on the VPS.
The laptop comes back up immediately afterwards (still serving traffic).

## 5. Cutover

```
sudo systemctl stop aust-backend aust-flash-bot
sudo systemctl stop cloudflared
```

The VPS's cloudflared is already serving the same hostname (same tunnel,
different connector). Verify:

```
curl https://api.aufraeumhelden.com/health
```

## 6. Cleanup

Once you've confirmed the VPS is serving correctly for a day or two:
- `sudo systemctl disable aust-backend aust-flash-bot cloudflared` on the laptop.
- Drop the `project_local_takeover.md` memory note.
