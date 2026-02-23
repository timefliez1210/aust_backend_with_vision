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
DEPTH_ANYTHING_ID = "depth-anything/Depth-Anything-V2-Metric-Indoor-Large-hf"

# SAM 2.1 (replaces SAM ViT-H)
SAM2_MODEL_ID = "facebook/sam2.1-hiera-large"
SAM2_CHECKPOINT = "sam2.1_hiera_large.pt"

# MASt3R (loaded on demand for video pipeline)
MAST3R_MODEL_ID = "naver/MASt3R_ViTLarge_BaseDecoder_512_catmlpdpt_metric"


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


def load_sam2(device: torch.device) -> tuple:
    """Load SAM 2.1 model.

    Returns (image_predictor, video_predictor) tuple.
    """
    from sam2.build_sam import build_sam2, build_sam2_video_predictor
    from sam2.sam2_image_predictor import SAM2ImagePredictor

    weights_dir = ensure_weights_dir()
    cache_dir = str(weights_dir / "huggingface")

    logger.info("Loading SAM 2.1 (%s) ...", SAM2_MODEL_ID)

    # Build image predictor
    image_predictor = SAM2ImagePredictor.from_pretrained(SAM2_MODEL_ID, cache_dir=cache_dir)
    image_predictor.model = image_predictor.model.to(device)
    image_predictor.model.eval()

    # Build video predictor (shares weights but different interface)
    from sam2.sam2_video_predictor import SAM2VideoPredictor
    video_predictor = SAM2VideoPredictor.from_pretrained(SAM2_MODEL_ID, cache_dir=cache_dir)
    video_predictor.model = video_predictor.model.to(device)
    video_predictor.model.eval()

    logger.info("SAM 2.1 loaded on %s (image + video predictors)", device)
    return image_predictor, video_predictor


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
    """Holds all loaded model references. Populated at startup.

    Manages GPU memory by swapping models between detection and reconstruction:
    - Startup: DINO + SAM 2 + Depth Anything (~3GB idle)
    - Photo pipeline: DINO + SAM 2 + DA active (~8GB)
    - Video Phase 1: MASt3R only (~12-15GB)
    - Video Phase 2: DINO + SAM 2 (~5GB)
    - Video Phase 3: CPU only (Open3D)
    """

    def __init__(self) -> None:
        self.device: torch.device | None = None
        self.grounding_dino_model = None
        self.grounding_dino_processor = None
        self.sam2_image_predictor = None
        self.sam2_video_predictor = None
        self.depth_model = None
        self.depth_processor = None
        self.mast3r_model = None
        self._loaded = False
        # Track whether detection models are on GPU or CPU
        self._detection_on_gpu = False

    @property
    def is_loaded(self) -> bool:
        return self._loaded

    @property
    def gpu_available(self) -> bool:
        return torch.cuda.is_available()

    # Backward compatibility: photo pipeline uses sam_predictor
    @property
    def sam_predictor(self):
        """Backward compatibility: returns SAM 2 image predictor."""
        return self.sam2_image_predictor

    def load_all(self) -> None:
        """Load all detection models. Called once during application startup."""
        self.device = get_device()
        logger.info("Loading all models on device=%s ...", self.device)

        self.grounding_dino_model, self.grounding_dino_processor = load_grounding_dino(
            self.device
        )
        self.sam2_image_predictor, self.sam2_video_predictor = load_sam2(self.device)
        self.depth_model, self.depth_processor = load_depth_anything(self.device)
        self._detection_on_gpu = True

        self._warm_up()
        self._loaded = True
        logger.info("All models loaded and warmed up.")

    def load_mast3r(self) -> None:
        """Load MASt3R model on demand for video reconstruction.

        Should be called after unload_detection_models() to free GPU memory.
        """
        from mast3r.model import AsymmetricMASt3R

        weights_dir = ensure_weights_dir()
        cache_dir = str(weights_dir / "huggingface")

        logger.info("Loading MASt3R (%s) ...", MAST3R_MODEL_ID)
        self.mast3r_model = AsymmetricMASt3R.from_pretrained(
            MAST3R_MODEL_ID, cache_dir=cache_dir
        ).to(self.device)
        self.mast3r_model.eval()
        logger.info("MASt3R loaded on %s", self.device)

    def unload_mast3r(self) -> None:
        """Free GPU memory used by MASt3R."""
        if self.mast3r_model is not None:
            del self.mast3r_model
            self.mast3r_model = None
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
            logger.info("MASt3R unloaded from GPU")

    def unload_detection_models(self) -> None:
        """Move detection models (DINO, SAM 2, DA) to CPU to free GPU for MASt3R."""
        if not self._detection_on_gpu:
            return

        cpu = torch.device("cpu")
        if self.grounding_dino_model is not None:
            self.grounding_dino_model = self.grounding_dino_model.to(cpu)
        if self.sam2_image_predictor is not None:
            self.sam2_image_predictor.model = self.sam2_image_predictor.model.to(cpu)
        if self.sam2_video_predictor is not None:
            self.sam2_video_predictor.model = self.sam2_video_predictor.model.to(cpu)
        if self.depth_model is not None:
            self.depth_model = self.depth_model.to(cpu)

        self._detection_on_gpu = False
        if torch.cuda.is_available():
            torch.cuda.empty_cache()
        logger.info("Detection models moved to CPU")

    def ensure_detection_models(self) -> None:
        """Move detection models back to GPU if they were offloaded."""
        if self._detection_on_gpu:
            return

        if self.grounding_dino_model is not None:
            self.grounding_dino_model = self.grounding_dino_model.to(self.device)
        if self.sam2_image_predictor is not None:
            self.sam2_image_predictor.model = self.sam2_image_predictor.model.to(self.device)
        if self.sam2_video_predictor is not None:
            self.sam2_video_predictor.model = self.sam2_video_predictor.model.to(self.device)
        if self.depth_model is not None:
            self.depth_model = self.depth_model.to(self.device)

        self._detection_on_gpu = True
        logger.info("Detection models restored to GPU")

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

        # Warm up SAM 2 (image predictor)
        self.sam2_image_predictor.set_image(np.zeros((64, 64, 3), dtype=np.uint8))

        # Warm up Depth Anything
        depth_inputs = self.depth_processor(images=dummy, return_tensors="pt").to(self.device)
        with torch.no_grad():
            self.depth_model(**depth_inputs)

        logger.info("Warm-up complete.")


# Singleton instance
registry = ModelRegistry()
