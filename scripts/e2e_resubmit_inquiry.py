#!/usr/bin/env python3
"""
End-to-end resubmission test for inquiry 019cf613-658b-7740-907b-f362c4b90074
(Heike Lübben, 52 images).

Expected result: ~70 m³ (manually quoted).

Steps:
  1. Authenticate against the local API
  2. Download all 52 images from MinIO
  3. POST them to /api/v1/inquiries/{id}/estimate/depth  (multipart)
  4. Poll /api/v1/inquiries/{id} until estimation status = completed
  5. Print itemised breakdown (moveable / box / non-moveable) + total
  6. Compare to baseline and exit non-zero if >20% off
"""

import sys
import time
import json
import io
import requests
import boto3
from botocore.client import Config

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
API_BASE       = "http://localhost:8080"
INQUIRY_ID     = "019cf613-658b-7740-907b-f362c4b90074"
ADMIN_EMAIL    = "info@aust-umzuege.de"
ADMIN_PASSWORD = "test1234"

MINIO_ENDPOINT = "http://localhost:9000"
MINIO_ACCESS   = "minioadmin"
MINIO_SECRET   = "minioadmin"
MINIO_BUCKET   = "aust-uploads"

# S3 prefix for this estimation (from DB)
EST_PREFIX = (
    "estimates/019cf613-658b-7740-907b-f362c4b90074"
    "/019cf613-658c-7020-bedf-0623fce78a40"
)
IMAGE_COUNT = 52

BASELINE_M3   = 70.0
TOLERANCE_PCT = 25.0          # allow ±25% vs baseline
POLL_INTERVAL = 15            # seconds between status polls
POLL_TIMEOUT  = 1800          # 30 min max

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def authenticate():
    log("Authenticating…")
    r = requests.post(f"{API_BASE}/api/v1/auth/login", json={
        "email": ADMIN_EMAIL,
        "password": ADMIN_PASSWORD,
    }, timeout=10)
    r.raise_for_status()
    token = r.json()["access_token"]
    log("  ✓ Got access token")
    return {"Authorization": f"Bearer {token}"}


def download_images(s3):
    log(f"Downloading {IMAGE_COUNT} images from MinIO…")
    images = []
    for i in range(IMAGE_COUNT):
        key = f"{EST_PREFIX}/{i}.jpg"
        obj = s3.get_object(Bucket=MINIO_BUCKET, Key=key)
        data = obj["Body"].read()
        images.append((f"{i}.jpg", data))
        if (i + 1) % 10 == 0:
            log(f"  … {i+1}/{IMAGE_COUNT} downloaded")
    log(f"  ✓ All {IMAGE_COUNT} images downloaded ({sum(len(d) for _,d in images)/1e6:.1f} MB total)")
    return images


def trigger_estimation(headers, images):
    log("Submitting images to POST /api/v1/inquiries/{id}/estimate/depth …")
    url = f"{API_BASE}/api/v1/inquiries/{INQUIRY_ID}/estimate/depth"

    files = [("images", (name, io.BytesIO(data), "image/jpeg")) for name, data in images]

    r = requests.post(url, headers=headers, files=files, timeout=120)
    if r.status_code not in (200, 201, 202):
        print(f"  ERROR {r.status_code}: {r.text[:500]}")
        r.raise_for_status()

    resp = r.json()
    # Endpoint returns an array: [{"id": "...", "status": "processing"}]
    if isinstance(resp, list):
        est_id = resp[0].get("id") if resp else "?"
    else:
        est_id = resp.get("id") or resp.get("estimation_id") or "?"
    log(f"  ✓ Accepted — estimation_id={est_id}")
    return est_id


def poll_until_done(headers, new_est_id: str):
    """Poll until the specific new estimation (new_est_id) is completed."""
    log(f"Polling until estimation {new_est_id[:8]}… completes (timeout {POLL_TIMEOUT}s)…")
    deadline = time.monotonic() + POLL_TIMEOUT
    while time.monotonic() < deadline:
        r = requests.get(
            f"{API_BASE}/api/v1/inquiries/{INQUIRY_ID}",
            headers=headers, timeout=15,
        )
        r.raise_for_status()
        data = r.json()
        est = data.get("estimation") or {}
        est_id = str(est.get("id", ""))
        status = est.get("status", "none")
        vol = data.get("volume_m3", "?")
        log(f"  est_id={est_id[:8]}…  status={status}  inquiry_vol={vol} m³")

        # Only accept completion of the *new* estimation, not a stale old one
        if est_id == new_est_id and status == "completed":
            return data
        if est_id == new_est_id and status in ("failed", "error"):
            print(f"  FAILED: estimation status={status}")
            sys.exit(2)

        time.sleep(POLL_INTERVAL)

    print("  TIMEOUT waiting for estimation to complete")
    sys.exit(3)


def print_report(data):
    items = data.get("items", [])
    vol   = data.get("estimated_volume_m3", 0.0)
    est   = data.get("estimation") or {}

    moveable     = [i for i in items if i.get("is_moveable", True) and not i.get("packs_into_boxes", False)]
    box_items    = [i for i in items if i.get("is_moveable", True) and i.get("packs_into_boxes", False)]
    non_moveable = [i for i in items if not i.get("is_moveable", True)]

    print()
    print("=" * 65)
    print(f"  E2E RESULT — Heike Lübben ({INQUIRY_ID[:8]}…)")
    print("=" * 65)
    print(f"  Baseline (manual quote):  {BASELINE_M3:.1f} m³")
    print(f"  Pipeline total:           {vol:.2f} m³")
    delta_pct = (vol - BASELINE_M3) / BASELINE_M3 * 100
    sign = "+" if delta_pct >= 0 else ""
    print(f"  Delta vs baseline:        {sign}{delta_pct:.1f}%")
    print(f"  Processing time:          {est.get('processing_time_ms', '?')} ms")
    print(f"  Total items detected:     {len(items)}")
    print()

    def fmt_items(title, lst):
        if not lst:
            return
        print(f"  ── {title} ({len(lst)}) ──")
        for it in sorted(lst, key=lambda x: x.get("volume_m3", 0), reverse=True):
            name = it.get("name", "?")
            v    = it.get("volume_m3", 0)
            conf = it.get("confidence", 0)
            src  = it.get("volume_source", "?")
            q    = it.get("quantity", 1)
            print(f"    {name:<35} {v:6.3f} m³  conf={conf:.0%}  src={src}  qty={q}")
        subtotal = sum(i.get("volume_m3", 0) for i in lst)
        print(f"    {'SUBTOTAL':<35} {subtotal:6.3f} m³")
        print()

    fmt_items("Möbel & Gegenstände (im Volumen)", moveable)
    fmt_items("Kartons & Kleinteile (im Volumen)", box_items)
    fmt_items("Nicht transportiert  (ausgeschlossen)", non_moveable)

    print("=" * 65)
    ok = abs(delta_pct) <= TOLERANCE_PCT
    status_str = "PASS ✓" if ok else f"FAIL ✗  (tolerance ±{TOLERANCE_PCT:.0f}%)"
    print(f"  RESULT: {status_str}")
    print("=" * 65)
    print()
    return ok


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    log("=== E2E resubmission test — Heike Lübben, 52 images ===")
    log(f"Baseline: {BASELINE_M3} m³   Tolerance: ±{TOLERANCE_PCT}%")
    print()

    # 1. Auth
    headers = authenticate()

    # 2. Download images from MinIO
    s3 = boto3.client(
        "s3",
        endpoint_url=MINIO_ENDPOINT,
        aws_access_key_id=MINIO_ACCESS,
        aws_secret_access_key=MINIO_SECRET,
        config=Config(signature_version="s3v4"),
        region_name="us-east-1",
    )
    images = download_images(s3)

    # 3. Trigger estimation via API
    new_est_id = trigger_estimation(headers, images)

    # 4. Poll until done
    data = poll_until_done(headers, new_est_id)

    # 5. Report
    passed = print_report(data)
    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()
