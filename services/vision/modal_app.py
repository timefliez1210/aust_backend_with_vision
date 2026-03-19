"""Modal deployment for the AUST Vision Service.

Architecture:
  PhotoPipeline (@app.cls, GPU L4)
    - @modal.enter() loads all models once when the container starts
    - run_job()  receives image bytes, runs pipeline, writes result to job_store
    - Container shuts down naturally when idle (scaledown_window=120)

  serve (@app.function, no GPU)
    - Thin FastAPI layer: /estimate/submit spawns PhotoPipeline().run_job
    - /estimate/status reads from job_store
    - No background asyncio tasks, no scaledown_window tricks

  VideoPipeline (@app.cls, GPU L4) + serve_video — same pattern for video jobs

  job_store (modal.Dict)
    - Persistent key-value store, visible to all containers
    - Survives container restarts between submit and the first status poll
    - Replaces both the in-memory dict and the S3 job-result fallback

Usage:
    modal deploy services/vision/modal_app.py
    modal serve  services/vision/modal_app.py   # dev hot-reload

Test:
    curl https://<app>.modal.run/health
    curl -X POST https://<app>.modal.run/estimate/submit \\
        -F "job_id=test-1" -F "images=@room1.jpg"
    curl https://<app>.modal.run/estimate/status/test-1
"""
from pathlib import Path

import modal

LOCAL_APP_DIR = Path(__file__).parent / "app"

# ---------------------------------------------------------------------------
# Container image — install deps + bake model weights
# ---------------------------------------------------------------------------

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
        "pillow-heif>=0.18,<1",
        "qwen-vl-utils>=0.0.8,<1",
        "accelerate>=0.26,<2",
    )
    # SAM 2.1
    .run_commands(
        "pip install git+https://github.com/facebookresearch/sam2.git",
    )
    # MASt3R + DUSt3R
    .run_commands(
        "git clone --recursive https://github.com/naver/mast3r.git /opt/mast3r && "
        "pip install -r /opt/mast3r/requirements.txt && "
        "pip install -r /opt/mast3r/dust3r/requirements.txt",
        gpu="L4",
    )
    .env({"PYTHONPATH": "/opt/mast3r:/opt/mast3r/dust3r"})
    .run_commands(
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
        "python -c \""
        "from huggingface_hub import snapshot_download; "
        "snapshot_download('facebook/sam2.1-hiera-large', cache_dir='/weights/huggingface'); "
        "print('SAM 2.1 weights downloaded.')\""
    )
    .run_commands(
        "python -c \""
        "from huggingface_hub import snapshot_download; "
        "snapshot_download('naver/MASt3R_ViTLarge_BaseDecoder_512_catmlpdpt_metric', cache_dir='/weights/huggingface'); "
        "print('MASt3R weights downloaded.')\""
    )
    .run_commands(
        "python -c \""
        "from transformers import Qwen2VLForConditionalGeneration, AutoProcessor; "
        "AutoProcessor.from_pretrained('Qwen/Qwen2-VL-7B-Instruct', cache_dir='/weights/huggingface'); "
        "Qwen2VLForConditionalGeneration.from_pretrained('Qwen/Qwen2-VL-7B-Instruct', cache_dir='/weights/huggingface'); "
        "print('Qwen2-VL-7B weights downloaded.')\""
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

# Persistent job store — visible to all containers, survives restarts.
job_store = modal.Dict.from_name("aust-vision-jobs", create_if_missing=True)


# ---------------------------------------------------------------------------
# Photo pipeline — GPU worker class
# ---------------------------------------------------------------------------

@app.cls(gpu="L4", scaledown_window=120, max_containers=1, timeout=1800)
class PhotoPipeline:
    """GPU worker for photo volume estimation.

    Models are loaded once when the container starts (@modal.enter) and reused
    for every job. The container shuts down 120 s after the last job completes —
    no wasted GPU time, no scaledown_window hacks.
    """

    @modal.enter()
    def setup(self) -> None:
        import logging
        import sys

        logging.basicConfig(
            level=logging.INFO,
            format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        )
        self._logger = logging.getLogger("photo_pipeline")

        if "/root" not in sys.path:
            sys.path.insert(0, "/root")

        from pillow_heif import register_heif_opener
        register_heif_opener()

        from app.vision.model_loader import registry
        registry.load_all()
        self._registry = registry
        self._logger.info("PhotoPipeline ready")

    @modal.method()
    def run_job(self, job_id: str, image_bytes_list: list, threshold: float = 0.3) -> None:
        """Run the full photo vision pipeline for one job.

        Called by: serve() ASGI app via .spawn() — runs in this GPU container.
        Why: Decoupled from the HTTP layer so Modal tracks this as an active
             invocation and keeps the container alive until the method returns.
             Result is written to job_store (modal.Dict) so the HTTP container
             can read it on the next status poll.
        """
        import io
        import time

        from PIL import Image as PILImage
        from app.vision.pipeline import VisionPipeline

        job_store[job_id] = {"status": "processing", "started_at": time.time()}
        try:
            pil_images = [
                PILImage.open(io.BytesIO(b)).convert("RGB")
                for b in image_bytes_list
            ]
            pipeline = VisionPipeline(self._registry)
            result = pipeline.run(
                job_id=job_id,
                images=pil_images,
                detection_threshold=threshold,
            )
            payload = {
                "status": "succeeded",
                "result": result.model_dump(),
                "finished_at": time.time(),
            }
            self._logger.info(
                "Job %s succeeded: %d items, %.3f m³",
                job_id, len(result.detected_items), result.total_volume_m3,
            )
        except Exception as exc:  # noqa: BLE001
            self._logger.exception("Job %s failed", job_id)
            payload = {
                "status": "failed",
                "error": str(exc),
                "finished_at": time.time(),
            }
        job_store[job_id] = payload


# ---------------------------------------------------------------------------
# Photo HTTP layer — no GPU, just submit + status
# ---------------------------------------------------------------------------

@app.function(scaledown_window=60, max_containers=2, timeout=60)
@modal.asgi_app()
def serve():
    """Thin HTTP layer for photo estimation — no GPU.

    POST /estimate/submit  — reads images, spawns PhotoPipeline.run_job, returns 202
    GET  /estimate/status  — reads job_store, returns current status/result
    POST /estimate/upload  — deprecated sync endpoint (kept for compatibility)
    """
    import io
    import logging
    import sys
    import time

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )
    logger = logging.getLogger("serve")

    if "/root" not in sys.path:
        sys.path.insert(0, "/root")

    from typing import List

    from fastapi import FastAPI, File, Form, UploadFile
    from fastapi.responses import JSONResponse
    from pillow_heif import register_heif_opener
    register_heif_opener()

    web_app = FastAPI(title="AUST Vision Service (Modal)")

    @web_app.get("/health")
    def health():
        return {"status": "ok"}

    @web_app.get("/ready")
    def ready():
        # HTTP layer is always ready; pipeline readiness is implicit via job_store
        return {"status": "ready"}

    @web_app.post("/estimate/submit")
    async def estimate_submit(
        job_id: str = Form(default="test"),
        images: List[UploadFile] = File(...),
        detection_threshold: float = Form(default=0.3),
    ):
        """Async submit: spawn GPU job, return immediately.

        Poll GET /estimate/status/{job_id} for the result.
        """
        image_bytes_list = [await img.read() for img in images]
        logger.info("Spawning photo job %s (%d images)", job_id, len(image_bytes_list))

        call = PhotoPipeline().run_job.spawn(job_id, image_bytes_list, detection_threshold)
        job_store[job_id] = {
            "status": "accepted",
            "call_id": call.object_id,
            "started_at": time.time(),
        }

        return {"job_id": job_id, "status": "accepted"}

    @web_app.get("/estimate/status/{job_id}")
    async def estimate_status(job_id: str):
        """Poll endpoint — reads from modal.Dict, works across container restarts.

        If the job is stuck in processing/accepted, verifies the Modal invocation
        is still alive via FunctionCall.from_id(). If it died without writing a
        result (OOM, timeout, hardware failure), marks the job as failed immediately
        instead of leaving it stuck at 'processing' until max_polls is exhausted.
        """
        info = job_store.get(job_id)
        if info is None:
            return JSONResponse(
                status_code=404,
                content={"detail": f"Job {job_id!r} not found"},
            )

        if info["status"] in ("accepted", "processing"):
            call_id = info.get("call_id")
            if call_id:
                try:
                    fc = modal.FunctionCall.from_id(call_id)
                    fc.get(timeout=0)
                    # If we reach here the call finished but didn't write to job_store
                    # (shouldn't happen, but treat as failed to be safe)
                    error = "Invocation completed without writing result"
                    job_store[job_id] = {"status": "failed", "error": error}
                    return JSONResponse(
                        status_code=500,
                        content={"status": "failed", "error": error},
                    )
                except TimeoutError:
                    pass  # still running — normal case
                except modal.exception.InputCancellation as exc:
                    error = f"Job cancelled: {exc}"
                    job_store[job_id] = {"status": "failed", "error": error}
                    return {"status": "failed", "error": error}
                except Exception as exc:
                    # Invocation failed/killed without writing to job_store
                    error = f"Invocation died: {exc}"
                    logger.error("Photo job %s invocation failed: %s", job_id, exc)
                    job_store[job_id] = {"status": "failed", "error": error}
                    return {"status": "failed", "error": error}

        response: dict = {"status": info["status"]}
        if info["status"] == "succeeded":
            response["result"] = info["result"]
        elif info["status"] == "failed":
            response["error"] = info.get("error", "Unknown error")
        return response

    @web_app.post("/estimate/upload")
    async def estimate_upload_deprecated(
        job_id: str = Form(default="test"),
        images: List[UploadFile] = File(...),
        detection_threshold: float = Form(default=0.3),
    ):
        """Deprecated — alias for /estimate/submit. Poll /estimate/status/{job_id} for result."""
        image_bytes_list = [await img.read() for img in images]
        call = PhotoPipeline().run_job.spawn(job_id, image_bytes_list, detection_threshold)
        job_store[job_id] = {"status": "accepted", "call_id": call.object_id, "started_at": time.time()}
        return {"job_id": job_id, "status": "accepted"}

    return web_app


# ---------------------------------------------------------------------------
# Video pipeline — GPU worker class
# ---------------------------------------------------------------------------

@app.cls(gpu="L4", scaledown_window=120, max_containers=1, timeout=900)
class VideoPipeline:
    """GPU worker for video volume estimation.

    Same lifecycle pattern as PhotoPipeline: models loaded once at container
    start, container shuts down 120 s after the last job.
    """

    @modal.enter()
    def setup(self) -> None:
        import logging
        import sys

        logging.basicConfig(
            level=logging.INFO,
            format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        )
        self._logger = logging.getLogger("video_pipeline")

        if "/root" not in sys.path:
            sys.path.insert(0, "/root")

        from pillow_heif import register_heif_opener
        register_heif_opener()

        from app.vision.model_loader import registry
        registry.load_all()
        self._registry = registry
        self._logger.info("VideoPipeline ready")

    @modal.method()
    def run_job(
        self,
        job_id: str,
        video_bytes: bytes,
        detection_threshold: float = 0.3,
        max_keyframes: int = 60,
    ) -> None:
        """Run the full video vision pipeline for one job.

        Called by: serve_video() ASGI app via .spawn().
        Why: Same reasoning as PhotoPipeline.run_job — Modal tracks the method
             invocation and keeps the container alive until it returns.
        """
        import time

        from app.vision.video_pipeline import VideoPipeline as _VP

        job_store[job_id] = {"status": "processing", "started_at": time.time()}
        try:
            pipeline = _VP(self._registry)
            result = pipeline.run(
                job_id=job_id,
                video_bytes=video_bytes,
                detection_threshold=detection_threshold,
                max_keyframes=max_keyframes,
            )
            payload = {
                "status": "succeeded",
                "result": result.model_dump(),
                "finished_at": time.time(),
            }
            self._logger.info(
                "Video job %s succeeded: %d items, %.3f m³",
                job_id, len(result.detected_items), result.total_volume_m3,
            )
        except Exception as exc:  # noqa: BLE001
            self._logger.exception("Video job %s failed", job_id)
            payload = {
                "status": "failed",
                "error": str(exc),
                "finished_at": time.time(),
            }
        job_store[job_id] = payload


# ---------------------------------------------------------------------------
# Video HTTP layer — no GPU, just submit + status
# ---------------------------------------------------------------------------

@app.function(scaledown_window=60, max_containers=2, timeout=60)
@modal.asgi_app()
def serve_video():
    """Thin HTTP layer for video estimation — no GPU.

    POST /estimate/video/submit  — reads video, spawns VideoPipeline.run_job
    GET  /estimate/video/status  — reads job_store
    POST /estimate/video         — deprecated sync endpoint
    """
    import logging
    import sys
    import time

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )
    logger = logging.getLogger("serve_video")

    if "/root" not in sys.path:
        sys.path.insert(0, "/root")

    from typing import List

    from fastapi import FastAPI, File, Form, UploadFile
    from fastapi.responses import JSONResponse
    from pillow_heif import register_heif_opener
    register_heif_opener()

    web_app = FastAPI(title="AUST Vision Service - Video (Modal)")

    @web_app.get("/health")
    def health():
        return {"status": "ok"}

    @web_app.get("/ready")
    def ready():
        return {"status": "ready"}

    @web_app.post("/estimate/video/submit")
    async def estimate_video_submit(
        job_id: str = Form(default="test"),
        video: UploadFile = File(...),
        max_keyframes: int = Form(default=60),
        detection_threshold: float = Form(default=0.3),
    ):
        """Async submit: spawn GPU job, return immediately."""
        video_bytes = await video.read()
        logger.info("Spawning video job %s (%d bytes)", job_id, len(video_bytes))

        call = VideoPipeline().run_job.spawn(job_id, video_bytes, detection_threshold, max_keyframes)
        job_store[job_id] = {
            "status": "accepted",
            "call_id": call.object_id,
            "started_at": time.time(),
        }

        return {"job_id": job_id, "status": "accepted"}

    @web_app.get("/estimate/video/status/{job_id}")
    async def estimate_video_status(job_id: str):
        """Poll endpoint — reads from modal.Dict.

        Same dead-invocation detection as the photo status endpoint.
        """
        info = job_store.get(job_id)
        if info is None:
            return JSONResponse(
                status_code=404,
                content={"detail": f"Job {job_id!r} not found"},
            )

        if info["status"] in ("accepted", "processing"):
            call_id = info.get("call_id")
            if call_id:
                try:
                    fc = modal.FunctionCall.from_id(call_id)
                    fc.get(timeout=0)
                    error = "Invocation completed without writing result"
                    job_store[job_id] = {"status": "failed", "error": error}
                    return JSONResponse(
                        status_code=500,
                        content={"status": "failed", "error": error},
                    )
                except TimeoutError:
                    pass  # still running
                except modal.exception.InputCancellation as exc:
                    error = f"Job cancelled: {exc}"
                    job_store[job_id] = {"status": "failed", "error": error}
                    return {"status": "failed", "error": error}
                except Exception as exc:
                    error = f"Invocation died: {exc}"
                    logger.error("Video job %s invocation failed: %s", job_id, exc)
                    job_store[job_id] = {"status": "failed", "error": error}
                    return {"status": "failed", "error": error}

        response: dict = {"status": info["status"]}
        if info["status"] == "succeeded":
            response["result"] = info["result"]
        elif info["status"] == "failed":
            response["error"] = info.get("error", "Unknown error")
        return response

    @web_app.post("/estimate/video")
    async def estimate_video_deprecated(
        job_id: str = Form(default="test"),
        video: UploadFile = File(...),
        max_keyframes: int = Form(default=60),
        detection_threshold: float = Form(default=0.3),
    ):
        """Deprecated — alias for /estimate/video/submit. Poll /estimate/video/status/{job_id} for result."""
        video_bytes = await video.read()
        call = VideoPipeline().run_job.spawn(job_id, video_bytes, detection_threshold, max_keyframes)
        job_store[job_id] = {"status": "accepted", "call_id": call.object_id, "started_at": time.time()}
        return {"job_id": job_id, "status": "accepted"}

    return web_app
