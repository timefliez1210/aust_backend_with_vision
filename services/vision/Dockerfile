# GPU Dockerfile with CUDA + baked model weights
FROM nvidia/cuda:12.4.1-runtime-ubuntu22.04 AS base

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1

RUN apt-get update && apt-get install -y --no-install-recommends \
    python3.11 python3.11-venv python3-pip git \
    libgl1 libglib2.0-0 \
    && rm -rf /var/lib/apt/lists/*

RUN update-alternatives --install /usr/bin/python3 python3 /usr/bin/python3.11 1 \
    && update-alternatives --install /usr/bin/python python /usr/bin/python3.11 1

WORKDIR /app

# Install dependencies first for layer caching
COPY pyproject.toml .
RUN python -m pip install --upgrade pip setuptools wheel \
    && python -m pip install .

# Download and bake model weights into the image
ENV VISION_WEIGHTS_DIR=/app/weights
RUN python -c "\
from transformers import AutoModelForZeroShotObjectDetection, AutoProcessor; \
AutoProcessor.from_pretrained('IDEA-Research/grounding-dino-base', cache_dir='/app/weights/huggingface'); \
AutoModelForZeroShotObjectDetection.from_pretrained('IDEA-Research/grounding-dino-base', cache_dir='/app/weights/huggingface'); \
from transformers import AutoImageProcessor, AutoModelForDepthEstimation; \
AutoImageProcessor.from_pretrained('depth-anything/Depth-Anything-V2-Small-hf', cache_dir='/app/weights/huggingface'); \
AutoModelForDepthEstimation.from_pretrained('depth-anything/Depth-Anything-V2-Small-hf', cache_dir='/app/weights/huggingface'); \
import urllib.request; \
urllib.request.urlretrieve('https://dl.fbaipublicfiles.com/segment_anything/sam_vit_h_4b8939.pth', '/app/weights/sam_vit_h_4b8939.pth'); \
print('All weights downloaded.')"

# Copy application code
COPY app/ ./app/

EXPOSE 8090

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD python -c "import httpx; r = httpx.get('http://localhost:8090/health'); r.raise_for_status()"

CMD ["python", "-m", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8090"]
