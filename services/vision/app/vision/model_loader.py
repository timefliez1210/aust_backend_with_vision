from __future__ import annotations

import logging
import os
from pathlib import Path

import torch
from huggingface_hub import hf_hub_download

from app.config import settings

logger = logging.getLogger(__name__)

# Model identifiers
GROUNDING_DINO_ID = "IDEA-Research/grounding-dino-base"
SAM_CHECKPOINT = "sam_vit_h_4b8939.pth"
SAM_MODEL_TYPE = "vit_h"
DEPTH_ANYTHING_ID = "depth-anything/Depth-Anything-V2-Metric-Indoor-Large-hf"


def get_device() -> torch.device:
    """Return the configured torch device, falling back to CPU."""
    requested = settings.device.lower()
    if requested == "cuda" and torch.cuda.is_available():
        logger.info("Using CUDA device: %s", torch.cuda.get_device_name(0))
        return torch.device("cuda")
    if requested == "cuda" and not torch.cuda.is_available():
        logger.warning("CUDA requested but not available, falling back to CPU")
    return torch.device("cpu")


def ensure_weights_dir() -> Path:
    """Create weights directory if it does not exist."""
    path = Path(settings.weights_dir)
    path.mkdir(parents=True, exist_ok=True)
    return path


def load_grounding_dino(device: torch.device) -> tuple:
    """Load Grounding DINO model and processor.

    Returns (model, processor) tuple.
    """
    from transformers import AutoModelForZeroShotObjectDetection, AutoProcessor

    logger.info("Loading Grounding DINO from %s ...", GROUNDING_DINO_ID)
    weights_dir = ensure_weights_dir()
    cache_dir = str(weights_dir / "huggingface")

    processor = AutoProcessor.from_pretrained(GROUNDING_DINO_ID, cache_dir=cache_dir)
    model = AutoModelForZeroShotObjectDetection.from_pretrained(
        GROUNDING_DINO_ID, cache_dir=cache_dir
    )
    model = model.to(device)
    model.eval()
    logger.info("Grounding DINO loaded on %s", device)
    return model, processor


def load_sam(device: torch.device):
    """Load SAM model.

    Returns a SamPredictor instance.
    """
    from segment_anything import SamPredictor, sam_model_registry

    weights_dir = ensure_weights_dir()
    checkpoint_path = weights_dir / SAM_CHECKPOINT

    if not checkpoint_path.exists():
        import urllib.request
        sam_url = f"https://dl.fbaipublicfiles.com/segment_anything/{SAM_CHECKPOINT}"
        logger.info("Downloading SAM checkpoint from %s ...", sam_url)
        urllib.request.urlretrieve(sam_url, str(checkpoint_path))

    logger.info("Loading SAM (%s) from %s ...", SAM_MODEL_TYPE, checkpoint_path)
    sam = sam_model_registry[SAM_MODEL_TYPE](checkpoint=str(checkpoint_path))
    sam = sam.to(device)
    sam.eval()
    predictor = SamPredictor(sam)
    logger.info("SAM loaded on %s", device)
    return predictor


def load_depth_anything(device: torch.device) -> tuple:
    """Load Depth Anything V2 model and processor.

    Returns (model, processor) tuple.
    """
    from transformers import AutoImageProcessor, AutoModelForDepthEstimation

    logger.info("Loading Depth Anything V2 from %s ...", DEPTH_ANYTHING_ID)
    weights_dir = ensure_weights_dir()
    cache_dir = str(weights_dir / "huggingface")

    processor = AutoImageProcessor.from_pretrained(DEPTH_ANYTHING_ID, cache_dir=cache_dir)
    model = AutoModelForDepthEstimation.from_pretrained(DEPTH_ANYTHING_ID, cache_dir=cache_dir)
    model = model.to(device)
    model.eval()
    logger.info("Depth Anything V2 loaded on %s", device)
    return model, processor


class ModelRegistry:
    """Holds all loaded model references. Populated at startup."""

    def __init__(self) -> None:
        self.device: torch.device | None = None
        self.grounding_dino_model = None
        self.grounding_dino_processor = None
        self.sam_predictor = None
        self.depth_model = None
        self.depth_processor = None
        self._loaded = False

    @property
    def is_loaded(self) -> bool:
        return self._loaded

    @property
    def gpu_available(self) -> bool:
        return torch.cuda.is_available()

    def load_all(self) -> None:
        """Load all models. Called once during application startup."""
        self.device = get_device()
        logger.info("Loading all models on device=%s ...", self.device)

        self.grounding_dino_model, self.grounding_dino_processor = load_grounding_dino(
            self.device
        )
        self.sam_predictor = load_sam(self.device)
        self.depth_model, self.depth_processor = load_depth_anything(self.device)

        self._warm_up()
        self._loaded = True
        logger.info("All models loaded and warmed up.")

    def _warm_up(self) -> None:
        """Run a small dummy inference through each model to trigger JIT compilation."""
        import numpy as np
        from PIL import Image

        logger.info("Warming up models ...")
        dummy = Image.fromarray(np.zeros((64, 64, 3), dtype=np.uint8))

        # Warm up Grounding DINO
        inputs = self.grounding_dino_processor(
            images=dummy, text="object.", return_tensors="pt"
        ).to(self.device)
        with torch.no_grad():
            self.grounding_dino_model(**inputs)

        # Warm up SAM
        self.sam_predictor.set_image(np.zeros((64, 64, 3), dtype=np.uint8))

        # Warm up Depth Anything
        depth_inputs = self.depth_processor(images=dummy, return_tensors="pt").to(self.device)
        with torch.no_grad():
            self.depth_model(**depth_inputs)

        logger.info("Warm-up complete.")


# Singleton instance
registry = ModelRegistry()
