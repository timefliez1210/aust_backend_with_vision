"""Modal deployment for the AUST Vision Service.

Deploys the vision pipeline (Grounding DINO + SAM + Depth Anything V2 + Open3D)
as a serverless GPU endpoint on Modal.

Usage:
    modal deploy services/vision/modal_app.py      # deploy to Modal
    modal serve services/vision/modal_app.py       # dev mode with hot-reload

Test:
    curl https://<your-modal-app>.modal.run/health
    curl https://<your-modal-app>.modal.run/ready
    curl -X POST https://<your-modal-app>.modal.run/estimate/upload \
        -F "job_id=test-1" -F "images=@room1.jpg" -F "images=@room2.jpg"
"""
from pathlib import Path

import modal

LOCAL_APP_DIR = Path(__file__).parent / "app"

# -- Modal image: install deps + bake model weights -------------------------

vision_image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("git", "libgl1", "libglib2.0-0")
    .pip_install(
        "torch>=2.5,<3",
        "torchvision>=0.20,<1",
        gpu="T4",
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
        "transformers>=4.48,<5",
        "groundingdino-py>=0.4,<1",
        "segment-anything @ git+https://github.com/facebookresearch/segment-anything.git",
        "scipy>=1.14,<2",
        "scikit-learn>=1.6,<2",
        "httpx>=0.28,<1",
        "python-multipart>=0.0.18,<1",
    )
    .run_commands(
        # Download and cache model weights into the image
        "python -c \""
        "from transformers import AutoModelForZeroShotObjectDetection, AutoProcessor; "
        "AutoProcessor.from_pretrained('IDEA-Research/grounding-dino-base', cache_dir='/weights/huggingface'); "
        "AutoModelForZeroShotObjectDetection.from_pretrained('IDEA-Research/grounding-dino-base', cache_dir='/weights/huggingface'); "
        "from transformers import AutoImageProcessor, AutoModelForDepthEstimation; "
        "AutoImageProcessor.from_pretrained('depth-anything/Depth-Anything-V2-Small-hf', cache_dir='/weights/huggingface'); "
        "AutoModelForDepthEstimation.from_pretrained('depth-anything/Depth-Anything-V2-Small-hf', cache_dir='/weights/huggingface'); "
        "import urllib.request; "
        "urllib.request.urlretrieve('https://dl.fbaipublicfiles.com/segment_anything/sam_vit_h_4b8939.pth', '/weights/sam_vit_h_4b8939.pth'); "
        "print('All weights downloaded.')\""
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


@app.function(gpu="T4", scaledown_window=60, max_containers=1)
@modal.concurrent(max_inputs=4)
@modal.asgi_app()
def serve():
    """Serve the full FastAPI app on Modal with GPU."""
    import io
    import logging
    import sys

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )
    logger = logging.getLogger("modal_app")

    # Add /root to sys.path so `app` package is importable
    if "/root" not in sys.path:
        sys.path.insert(0, "/root")

    from typing import List

    from fastapi import FastAPI, File, Form, UploadFile
    from fastapi.responses import JSONResponse
    from PIL import Image as PILImage

    from app.models.schemas import EstimateResponse
    from app.vision.model_loader import registry
    from app.vision.pipeline import VisionPipeline

    # Load models synchronously before serving
    logger.info("Loading models on GPU ...")
    registry.load_all()
    logger.info("Models loaded, starting FastAPI app.")

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
        """Direct image upload endpoint for testing.

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
        result = pipeline.run(job_id=job_id, images=pil_images)
        return result

    return web_app
