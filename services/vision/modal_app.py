"""Modal deployment for the AUST Vision Service.

Deploys the vision pipeline as serverless GPU endpoints on Modal:
- Photo endpoint: Grounding DINO + SAM 2 + Depth Anything V2 (max_inputs=4)
- Video endpoint: + MASt3R 3D reconstruction (max_inputs=1, long-running)

GPU: L4 (24GB VRAM) for both — MASt3R needs ~12-15GB during reconstruction.

Usage:
    modal deploy services/vision/modal_app.py      # deploy to Modal
    modal serve services/vision/modal_app.py       # dev mode with hot-reload

Test:
    curl https://<app>.modal.run/health
    curl -X POST https://<app>.modal.run/estimate/upload \\
        -F "job_id=test-1" -F "images=@room1.jpg"
    curl -X POST https://<app>.modal.run/estimate/video \\
        -F "job_id=test-1" -F "video=@room.mp4"
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


# -- Photo endpoint: fast, concurrent processing ----------------------------

@app.function(gpu="L4", scaledown_window=60, max_containers=1)
@modal.concurrent(max_inputs=4)
@modal.asgi_app()
def serve():
    """Serve photo estimation endpoint on Modal with GPU.

    Handles concurrent photo requests (up to 4). Fast ~5s per job.
    """
    import io
    import logging
    import sys

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

    @web_app.post("/estimate/upload", response_model=EstimateResponse)
    async def estimate_upload(
        job_id: str = Form(default="test"),
        images: List[UploadFile] = File(...),
    ):
        """Direct image upload endpoint for photo estimation.

        curl -X POST <url>/estimate/upload \\
            -F "job_id=test-1" -F "images=@room1.jpg" -F "images=@room2.jpg"
        """
        import asyncio

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
    """
    import io
    import logging
    import sys

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )
    logger = logging.getLogger("modal_app_video")

    if "/root" not in sys.path:
        sys.path.insert(0, "/root")

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

    @web_app.post("/estimate/video", response_model=EstimateResponse)
    async def estimate_video(
        job_id: str = Form(default="test"),
        video: UploadFile = File(...),
        max_keyframes: int = Form(default=60),
        detection_threshold: float = Form(default=0.3),
    ):
        """Video upload endpoint for 3D volume estimation.

        curl -X POST <url>/estimate/video \\
            -F "job_id=test-1" -F "video=@room_walkthrough.mp4"
        """
        import asyncio

        if not registry.is_loaded:
            return JSONResponse(status_code=503, content={"detail": "Models not loaded yet."})

        video_bytes = await video.read()

        pipeline = VideoPipeline(registry)
        # Run blocking pipeline in thread pool to keep event loop responsive.
        # Without this, the 2-10 min sync call blocks uvicorn's event loop,
        # causing Modal's proxy to drop the connection before response is sent.
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
    from typing import List

    @web_app.post("/estimate/upload", response_model=EstimateResponse)
    async def estimate_upload(
        job_id: str = Form(default="test"),
        images: List[UploadFile] = File(...),
    ):
        """Photo upload endpoint (also available on video function for testing)."""
        import asyncio

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
