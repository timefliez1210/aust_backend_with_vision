from __future__ import annotations

import logging

import numpy as np
import torch
from PIL import Image

from app.config import settings
from app.vision.model_loader import ModelRegistry

logger = logging.getLogger(__name__)


class DepthEstimator:
    """Depth Anything V2 based monocular depth estimator."""

    def __init__(self, registry: ModelRegistry) -> None:
        self._model = registry.depth_model
        self._processor = registry.depth_processor
        self._device = registry.device

    def estimate(self, images: list[Image.Image]) -> list[np.ndarray]:
        """Produce depth maps for a list of images.

        Returns a list of depth arrays (H, W) in meters.
        Uses Depth Anything V2 Metric Indoor model which outputs metric depth directly.
        """
        depth_maps: list[np.ndarray] = []

        for idx, image in enumerate(images):
            depth_map = self._estimate_single(image)
            depth_maps.append(depth_map)
            logger.info(
                "Image %d: depth map shape=%s, range=[%.3f, %.3f]",
                idx,
                depth_map.shape,
                depth_map.min(),
                depth_map.max(),
            )

        return depth_maps

    def _estimate_single(self, image: Image.Image) -> np.ndarray:
        """Run depth estimation on a single image."""
        inputs = self._processor(images=image, return_tensors="pt").to(self._device)

        with torch.no_grad():
            outputs = self._model(**inputs)

        predicted_depth = outputs.predicted_depth

        # Interpolate to original image size
        prediction = torch.nn.functional.interpolate(
            predicted_depth.unsqueeze(1),
            size=image.size[::-1],  # (height, width)
            mode="bicubic",
            align_corners=False,
        ).squeeze()

        depth_np = prediction.cpu().numpy()

        # The Metric Indoor model outputs depth in meters directly.
        # Clamp to reasonable indoor range (0.1m to 20m).
        depth_metric = np.clip(depth_np, 0.1, 20.0)

        return depth_metric
