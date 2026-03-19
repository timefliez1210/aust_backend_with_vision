#!/usr/bin/env python3
"""
Retry a failed vision estimation by downloading images from MinIO and
re-submitting to Modal, then writing the result back to the DB.

Usage:
    python scripts/retry_estimation.py <estimation_id>

Example:
    python scripts/retry_estimation.py 019cfb35-4120-7273-ba18-55f5e38444b6
"""
import base64
import json
import sys
import time
from pathlib import Path

import boto3
import psycopg2
import requests
from botocore.config import Config

# ── Config ────────────────────────────────────────────────────────────────────
PG_DSN        = "host=localhost port=5432 dbname=aust_backend user=aust password=aust_dev_password"
S3_ENDPOINT   = "http://localhost:9000"
S3_BUCKET     = "aust-uploads"
S3_ACCESS     = "minioadmin"
S3_SECRET     = "minioadmin"
MODAL_BASE    = "https://crfabig--aust-vision-serve.modal.run"
POLL_INTERVAL = 60   # seconds between status polls
MAX_POLLS     = 40   # 40 × 60s = 40 min max
# ─────────────────────────────────────────────────────────────────────────────


def main(estimation_id: str) -> None:
    s3 = boto3.client(
        "s3",
        endpoint_url=S3_ENDPOINT,
        aws_access_key_id=S3_ACCESS,
        aws_secret_access_key=S3_SECRET,
        config=Config(signature_version="s3v4"),
        region_name="us-east-1",
    )

    conn = psycopg2.connect(PG_DSN)
    cur  = conn.cursor()

    # ── 1. Load estimation record ─────────────────────────────────────────────
    cur.execute(
        "SELECT inquiry_id, source_data FROM volume_estimations WHERE id = %s",
        (estimation_id,),
    )
    row = cur.fetchone()
    if not row:
        sys.exit(f"No estimation found with id {estimation_id}")

    inquiry_id, source_data = row
    s3_keys: list[str] = source_data.get("s3_keys", [])
    if not s3_keys:
        sys.exit("No s3_keys in source_data — nothing to resubmit")

    print(f"Inquiry : {inquiry_id}")
    print(f"Images  : {len(s3_keys)}")

    # ── 2. Download images from MinIO ─────────────────────────────────────────
    print("Downloading images from MinIO...", flush=True)
    images: list[tuple[bytes, str]] = []
    for key in s3_keys:
        obj = s3.get_object(Bucket=S3_BUCKET, Key=key)
        data = obj["Body"].read()
        content_type = obj.get("ContentType", "image/jpeg")
        images.append((data, content_type))
    print(f"  Downloaded {len(images)} images")

    # ── 3. Submit to Modal ────────────────────────────────────────────────────
    print("Submitting to Modal...", flush=True)
    files = [
        ("images", (f"{i}.jpg", img_bytes, mime))
        for i, (img_bytes, mime) in enumerate(images)
    ]
    resp = requests.post(
        f"{MODAL_BASE}/estimate/submit",
        data={"job_id": estimation_id},
        files=files,
        timeout=120,
    )
    resp.raise_for_status()
    print(f"  Modal accepted: {resp.json()}")

    # ── 4. Poll until done ────────────────────────────────────────────────────
    print("Polling for result...", flush=True)
    result = None
    for poll in range(1, MAX_POLLS + 1):
        time.sleep(POLL_INTERVAL)
        status_resp = requests.get(
            f"{MODAL_BASE}/estimate/status/{estimation_id}", timeout=30
        )
        status_resp.raise_for_status()
        info = status_resp.json()
        status = info.get("status")
        print(f"  Poll {poll}/{MAX_POLLS}: {status}", flush=True)

        if status == "succeeded":
            result = info["result"]
            break
        elif status == "failed":
            sys.exit(f"Job failed: {info.get('error')}")
    else:
        sys.exit(f"Timed out after {MAX_POLLS} polls")

    total_volume = result["total_volume_m3"]
    confidence   = result["confidence_score"]
    items        = result["detected_items"]
    print(f"  {len(items)} items, {total_volume:.2f} m³ (confidence {confidence:.2f})")

    # ── 5. Upload crop thumbnails to MinIO ────────────────────────────────────
    print("Uploading crop thumbnails...", flush=True)
    uploaded = 0
    for idx, item in enumerate(items):
        crop_b64 = item.pop("crop_base64", None)
        if crop_b64:
            name = item.get("name", "item").replace(" ", "_").lower()
            key  = f"estimates/{inquiry_id}/{estimation_id}/crops/{name}_{idx}.jpg"
            try:
                decoded = base64.b64decode(crop_b64)
                s3.put_object(
                    Bucket=S3_BUCKET,
                    Key=key,
                    Body=decoded,
                    ContentType="image/jpeg",
                )
                item["crop_s3_key"] = key
                uploaded += 1
            except Exception as e:
                print(f"  Warning: failed to upload crop for item {idx}: {e}")
    print(f"  Uploaded {uploaded} crop thumbnails")

    # ── 6. Update estimation record in DB ─────────────────────────────────────
    print("Updating DB...", flush=True)
    # Store as raw array — same format as try_vision_service_async in inquiries.rs
    # parse_detected_items() expects either a raw Vec<DetectedItem> or DepthSensorResult,
    # NOT a wrapper object with an "items" key.
    result_data = items
    cur.execute(
        """
        UPDATE volume_estimations
           SET status          = 'completed',
               total_volume_m3 = %s,
               confidence_score= %s,
               result_data     = %s
         WHERE id = %s
        """,
        (total_volume, confidence, json.dumps(result_data), estimation_id),
    )
    # Also update the inquiry's estimated_volume_m3
    cur.execute(
        "UPDATE inquiries SET estimated_volume_m3 = %s WHERE id = %s",
        (total_volume, str(inquiry_id)),
    )
    conn.commit()
    cur.close()
    conn.close()
    print(f"Done. Estimation {estimation_id} updated: {total_volume:.2f} m³, {len(items)} items.")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"Usage: {sys.argv[0]} <estimation_id>")
    main(sys.argv[1])
