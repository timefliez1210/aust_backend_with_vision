"""Modal deployment for the AUST Vision Service.

Deploys the vision pipeline as serverless GPU endpoints on Modal:
- Photo endpoint: Grounding DINO + SAM 2 + Depth Anything V2 (max_inputs=1)
- Video endpoint: + MASt3R 3D reconstruction (max_inputs=1, long-running)

GPU: L4 (24GB VRAM) for both — MASt3R needs ~12-15GB during reconstruction.

Architecture: async submit + polling.
  1. POST /estimate/submit  → accepts images, starts pipeline in background,
                             returns {"job_id": "...", "status": "accepted"} immediately.
  2. GET  /estimate/status/{job_id} → returns processing/succeeded/failed.
  The Rust backend submits, then polls every 60 s until done or timeout.

Deprecated sync endpoints are kept for backward compatibility:
  - POST /estimate/upload  (photo, blocks until done)
  - POST /estimate/video   (video, blocks until done)

Usage:
    modal deploy services/vision/modal_app.py      # deploy to Modal
    modal serve services/vision/modal_app.py       # dev mode with hot-reload

Test:
    curl https://<app>.modal.run/health
    curl -X POST https://<app>.modal.run/estimate/submit \\
        -F "job_id=test-1" -F "images=@room1.jpg"
    curl https://<app>.modal.run/estimate/status/test-1
"""
from pathlib import Path

import modal

LOCAL_APP_DIR = Path(__file__).parent / "app"

# -- Modal image: install deps + bake model weights -------------------------

vision_image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("git", "libgl1", "libglib2.0-0", "ffmpeg")
    .pip_install(
        "torch>=2.5,<3",
        "torchvision>=0.20,<1",
        gpu="L4",
    )
    .pip_install(
        "fastapi>=0.115,<1",
        "uvicorn[standard]>=0.34,<1",
        "pydantic>=2.10,<3",
        "pydantic-settings>=2.7,<3",
        "boto3>=1.36,<2",
        "Pillow>=11,<12",
        "numpy>=1.26,<2",
        "open3d>=0.18,<1",
        "transformers>=4.50,<5",
        "scipy>=1.14,<2",
        "scikit-learn>=1.6,<2",
        "httpx>=0.28,<1",
        "python-multipart>=0.0.18,<1",
        "opencv-python-headless>=4.9,<5",
    )
    # SAM 2.1 (replaces SAM ViT-H) — install from GitHub, package name is "SAM-2"
    .run_commands(
        "pip install git+https://github.com/facebookresearch/sam2.git",
    )
    # MASt3R + DUSt3R (not pip-installable — clone repo, install deps, add to PYTHONPATH)
    .run_commands(
        "git clone --recursive https://github.com/naver/mast3r.git /opt/mast3r && "
        "pip install -r /opt/mast3r/requirements.txt && "
        "pip install -r /opt/mast3r/dust3r/requirements.txt",
        gpu="L4",
    )
    .env({"PYTHONPATH": "/opt/mast3r:/opt/mast3r/dust3r"})
    .run_commands(
        # Download and cache model weights into the image
        "python -c \""
        "from transformers import AutoModelForZeroShotObjectDetection, AutoProcessor; "
        "AutoProcessor.from_pretrained('openmmlab-community/mm_grounding_dino_large_all', cache_dir='/weights/huggingface'); "
        "AutoModelForZeroShotObjectDetection.from_pretrained('openmmlab-community/mm_grounding_dino_large_all', cache_dir='/weights/huggingface'); "
        "from transformers import AutoImageProcessor, AutoModelForDepthEstimation; "
        "AutoImageProcessor.from_pretrained('depth-anything/Depth-Anything-V2-Metric-Indoor-Large-hf', cache_dir='/weights/huggingface'); "
        "AutoModelForDepthEstimation.from_pretrained('depth-anything/Depth-Anything-V2-Metric-Indoor-Large-hf', cache_dir='/weights/huggingface'); "
        "print('HuggingFace weights downloaded.')\""
    )
    .run_commands(
        # Download SAM 2.1 checkpoint (download only, don't load to GPU)
        "python -c \""
        "from huggingface_hub import snapshot_download; "
        "snapshot_download('facebook/sam2.1-hiera-large', cache_dir='/weights/huggingface'); "
        "print('SAM 2.1 weights downloaded.')\""
    )
    .run_commands(
        # Download MASt3R checkpoint (download only, don't load to GPU)
        "python -c \""
        "from huggingface_hub import snapshot_download; "
        "snapshot_download('naver/MASt3R_ViTLarge_BaseDecoder_512_catmlpdpt_metric', cache_dir='/weights/huggingface'); "
        "print('MASt3R weights downloaded.')\""
    )
    .env({
        "VISION_DEVICE": "cuda",
        "VISION_WEIGHTS_DIR": "/weights",
        "VISION_DETECTION_THRESHOLD": "0.3",
        "VISION_S3_ENDPOINT": "http://localhost:9000",
        "VISION_S3_BUCKET": "aust-uploads",
        "VISION_S3_ACCESS_KEY": "minioadmin",
        "VISION_S3_SECRET_KEY": "minioadmin",
    })
    .add_local_dir(str(LOCAL_APP_DIR), remote_path="/root/app")
)

app = modal.App("aust-vision", image=vision_image)


# -- Photo endpoint: single-job async processing ----------------------------

@app.function(gpu="L4", scaledown_window=60, max_containers=1, timeout=900)
@modal.concurrent(max_inputs=1)
@modal.asgi_app()
def serve():
    """Serve photo estimation endpoint on Modal with GPU.

    Exposes two patterns:
    - Async (preferred): POST /estimate/submit + GET /estimate/status/{job_id}
    - Sync (deprecated): POST /estimate/upload  (blocks until done)
    """
    import asyncio
    import io
    import logging
    import sys
    import time

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )
    logger = logging.getLogger("modal_app")

    if "/root" not in sys.path:
        sys.path.insert(0, "/root")

    from typing import List

    from fastapi import FastAPI, File, Form, UploadFile
    from fastapi.responses import JSONResponse
    from PIL import Image as PILImage

    from app.models.schemas import EstimateResponse
    from app.vision.model_loader import registry
    from app.vision.pipeline import VisionPipeline

    logger.info("Loading detection models on GPU ...")
    registry.load_all()
    logger.info("Models loaded, starting FastAPI app (photo).")

    web_app = FastAPI(title="AUST Vision Service (Modal)")

    # In-memory job store: job_id -> {"status": str, "result": dict|None,
    #                                  "error": str|None, "started_at": float,
    #                                  "finished_at": float|None}
    jobs: dict = {}

    def _cleanup_old_jobs() -> None:
        """Remove jobs older than 30 minutes to bound memory usage."""
        cutoff = time.time() - 1800  # 30 minutes
        expired = [
            jid for jid, info in jobs.items()
            if info.get("finished_at", info.get("started_at", 0)) < cutoff
        ]
        for jid in expired:
            del jobs[jid]

    async def _run_photo_job(
        job_id: str,
        pil_images: list,
        threshold: float,
    ) -> None:
        """Execute vision pipeline in a thread and record result in jobs dict."""
        jobs[job_id] = {"status": "processing", "started_at": time.time()}
        try:
            pipeline = VisionPipeline(registry)
            result = await asyncio.to_thread(
                pipeline.run,
                job_id=job_id,
                images=pil_images,
                detection_threshold=threshold,
            )
            jobs[job_id] = {
                "status": "succeeded",
                "result": result.model_dump(),
                "finished_at": time.time(),
            }
            logger.info(
                "Photo job %s succeeded: %d items, %.3f m³",
                job_id, len(result.detected_items), result.total_volume_m3,
            )
        except Exception as exc:  # noqa: BLE001
            logger.exception("Photo pipeline failed for job %s", job_id)
            jobs[job_id] = {
                "status": "failed",
                "error": str(exc),
                "finished_at": time.time(),
            }

    @web_app.get("/health")
    def health():
        return {"status": "ok"}

    @web_app.get("/ready")
    def ready():
        if not registry.is_loaded:
            return JSONResponse(
                status_code=503,
                content={
                    "status": "not_ready",
                    "models_loaded": False,
                    "gpu_available": registry.gpu_available,
                },
            )
        return {
            "status": "ready",
            "models_loaded": True,
            "gpu_available": registry.gpu_available,
        }

    @web_app.post("/estimate/submit")
    async def estimate_submit(
        job_id: str = Form(default="test"),
        images: List[UploadFile] = File(...),
        detection_threshold: float = Form(default=0.3),
    ):
        """Async photo submit endpoint.

        Starts the vision pipeline in the background and returns immediately.
        Poll GET /estimate/status/{job_id} for the result.

        curl -X POST <url>/estimate/submit \\
            -F "job_id=uuid-here" -F "images=@room1.jpg" -F "images=@room2.jpg"
        """
        _cleanup_old_jobs()

        if not registry.is_loaded:
            return JSONResponse(status_code=503, content={"detail": "Models not loaded yet."})

        pil_images = []
        for upload in images:
            data = await upload.read()
            pil_images.append(PILImage.open(io.BytesIO(data)).convert("RGB"))

        # Kick off background task — do not await
        asyncio.create_task(_run_photo_job(job_id, pil_images, detection_threshold))

        return {"job_id": job_id, "status": "accepted"}

    @web_app.get("/estimate/status/{job_id}")
    async def estimate_status(job_id: str):
        """Poll endpoint for async photo jobs.

        Returns:
        - {"status": "processing"} while running
        - {"status": "succeeded", "result": {...}} on success
        - {"status": "failed", "error": "..."} on failure
        - 404 if job_id is unknown (container restarted, or expired)
        """
        _cleanup_old_jobs()

        info = jobs.get(job_id)
        if info is None:
            return JSONResponse(status_code=404, content={"detail": f"Job {job_id!r} not found"})

        response: dict = {"status": info["status"]}
        if info["status"] == "succeeded":
            response["result"] = info["result"]
        elif info["status"] == "failed":
            response["error"] = info.get("error", "Unknown error")
        return response

    @web_app.post("/estimate/upload", response_model=EstimateResponse)
    async def estimate_upload(
        job_id: str = Form(default="test"),
        images: List[UploadFile] = File(...),
    ):
        """Deprecated sync photo upload endpoint — blocks until pipeline completes.

        Kept for backward compatibility. Prefer /estimate/submit + /estimate/status.

        curl -X POST <url>/estimate/upload \\
            -F "job_id=test-1" -F "images=@room1.jpg" -F "images=@room2.jpg"
        """
        if not registry.is_loaded:
            return JSONResponse(status_code=503, content={"detail": "Models not loaded yet."})

        pil_images = []
        for upload in images:
            data = await upload.read()
            pil_images.append(PILImage.open(io.BytesIO(data)).convert("RGB"))

        pipeline = VisionPipeline(registry)
        result = await asyncio.to_thread(pipeline.run, job_id=job_id, images=pil_images)
        return result

    return web_app


# -- Video endpoint: single-job processing with model swapping --------------

@app.function(gpu="L4", scaledown_window=120, max_containers=1, timeout=900)
@modal.concurrent(max_inputs=1)
@modal.asgi_app()
def serve_video():
    """Serve video estimation endpoint on Modal with GPU.

    Handles one video job at a time (max_inputs=1) due to GPU model swapping.
    Processing takes 2-10 minutes per video.

    Exposes two patterns:
    - Async (preferred): POST /estimate/video/submit + GET /estimate/video/status/{job_id}
    - Sync (deprecated): POST /estimate/video  (blocks until done)
    """
    import asyncio
    import io
    import logging
    import sys
    import time

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )
    logger = logging.getLogger("modal_app_video")

    if "/root" not in sys.path:
        sys.path.insert(0, "/root")

    from typing import List

    from fastapi import FastAPI, File, Form, UploadFile
    from fastapi.responses import JSONResponse
    from PIL import Image as PILImage

    from app.models.schemas import EstimateResponse
    from app.vision.model_loader import registry
    from app.vision.video_pipeline import VideoPipeline

    logger.info("Loading detection models on GPU (video endpoint) ...")
    registry.load_all()
    logger.info("Models loaded, starting FastAPI app (video).")

    web_app = FastAPI(title="AUST Vision Service - Video (Modal)")

    # In-memory job store: job_id -> {"status", "result", "error", "started_at", "finished_at"}
    jobs: dict = {}

    def _cleanup_old_jobs() -> None:
        """Remove jobs older than 30 minutes to bound memory usage."""
        cutoff = time.time() - 1800
        expired = [
            jid for jid, info in jobs.items()
            if info.get("finished_at", info.get("started_at", 0)) < cutoff
        ]
        for jid in expired:
            del jobs[jid]

    async def _run_video_job(
        job_id: str,
        video_bytes: bytes,
        detection_threshold: float,
        max_keyframes: int,
    ) -> None:
        """Execute video pipeline in a thread and record result in jobs dict."""
        jobs[job_id] = {"status": "processing", "started_at": time.time()}
        try:
            pipeline = VideoPipeline(registry)
            result = await asyncio.to_thread(
                pipeline.run,
                job_id=job_id,
                video_bytes=video_bytes,
                detection_threshold=detection_threshold,
                max_keyframes=max_keyframes,
            )
            jobs[job_id] = {
                "status": "succeeded",
                "result": result.model_dump(),
                "finished_at": time.time(),
            }
            logger.info(
                "Video job %s succeeded: %d items, %.3f m³",
                job_id, len(result.detected_items), result.total_volume_m3,
            )
        except Exception as exc:  # noqa: BLE001
            logger.exception("Video pipeline failed for job %s", job_id)
            jobs[job_id] = {
                "status": "failed",
                "error": str(exc),
                "finished_at": time.time(),
            }

    @web_app.get("/health")
    def health():
        return {"status": "ok"}

    @web_app.get("/ready")
    def ready():
        if not registry.is_loaded:
            return JSONResponse(
                status_code=503,
                content={
                    "status": "not_ready",
                    "models_loaded": False,
                    "gpu_available": registry.gpu_available,
                },
            )
        return {
            "status": "ready",
            "models_loaded": True,
            "gpu_available": registry.gpu_available,
        }

    @web_app.post("/estimate/video/submit")
    async def estimate_video_submit(
        job_id: str = Form(default="test"),
        video: UploadFile = File(...),
        max_keyframes: int = Form(default=60),
        detection_threshold: float = Form(default=0.3),
    ):
        """Async video submit endpoint.

        Starts the video pipeline in the background and returns immediately.
        Poll GET /estimate/video/status/{job_id} for the result.

        curl -X POST <url>/estimate/video/submit \\
            -F "job_id=uuid-here" -F "video=@room_walkthrough.mp4"
        """
        _cleanup_old_jobs()

        if not registry.is_loaded:
            return JSONResponse(status_code=503, content={"detail": "Models not loaded yet."})

        video_bytes = await video.read()

        # Kick off background task — do not await
        asyncio.create_task(
            _run_video_job(job_id, video_bytes, detection_threshold, max_keyframes)
        )

        return {"job_id": job_id, "status": "accepted"}

    @web_app.get("/estimate/video/status/{job_id}")
    async def estimate_video_status(job_id: str):
        """Poll endpoint for async video jobs.

        Returns:
        - {"status": "processing"} while running
        - {"status": "succeeded", "result": {...}} on success
        - {"status": "failed", "error": "..."} on failure
        - 404 if job_id is unknown (container restarted, or expired)
        """
        _cleanup_old_jobs()

        info = jobs.get(job_id)
        if info is None:
            return JSONResponse(status_code=404, content={"detail": f"Job {job_id!r} not found"})

        response: dict = {"status": info["status"]}
        if info["status"] == "succeeded":
            response["result"] = info["result"]
        elif info["status"] == "failed":
            response["error"] = info.get("error", "Unknown error")
        return response

    @web_app.post("/estimate/video", response_model=EstimateResponse)
    async def estimate_video(
        job_id: str = Form(default="test"),
        video: UploadFile = File(...),
        max_keyframes: int = Form(default=60),
        detection_threshold: float = Form(default=0.3),
    ):
        """Deprecated sync video upload endpoint — blocks until pipeline completes.

        Kept for backward compatibility. Prefer /estimate/video/submit + /estimate/video/status.

        curl -X POST <url>/estimate/video \\
            -F "job_id=test-1" -F "video=@room_walkthrough.mp4"
        """
        if not registry.is_loaded:
            return JSONResponse(status_code=503, content={"detail": "Models not loaded yet."})

        video_bytes = await video.read()

        pipeline = VideoPipeline(registry)
        # Run blocking pipeline in thread pool to keep event loop responsive.
        result = await asyncio.to_thread(
            pipeline.run,
            job_id=job_id,
            video_bytes=video_bytes,
            detection_threshold=detection_threshold,
            max_keyframes=max_keyframes,
        )
        logger.info(
            "Returning response: job_id=%s, items=%d, volume=%.3f m3",
            result.job_id, len(result.detected_items), result.total_volume_m3,
        )
        return result

    # Also expose photo upload on the video endpoint for convenience
    @web_app.post("/estimate/upload", response_model=EstimateResponse)
    async def estimate_upload(
        job_id: str = Form(default="test"),
        images: List[UploadFile] = File(...),
    ):
        """Photo upload endpoint (also available on video function for testing)."""
        if not registry.is_loaded:
            return JSONResponse(status_code=503, content={"detail": "Models not loaded yet."})

        from app.vision.pipeline import VisionPipeline
        pil_images = []
        for upload in images:
            data = await upload.read()
            pil_images.append(PILImage.open(io.BytesIO(data)).convert("RGB"))

        pipeline = VisionPipeline(registry)
        return await asyncio.to_thread(pipeline.run, job_id=job_id, images=pil_images)

    return web_app
