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

        Returns a list of depth arrays (H, W) in metric-like units.
        Values are relative depth scaled by the configured depth_scale.
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

        # Normalize to approximate metric depth.
        # Monocular depth is relative, so we scale it to a plausible range.
        # The depth_scale config lets operators tune this for their setup.
        depth_min = depth_np.min()
        depth_max = depth_np.max()
        if depth_max - depth_min > 1e-6:
            depth_normalized = (depth_np - depth_min) / (depth_max - depth_min)
        else:
            depth_normalized = np.zeros_like(depth_np)

        # Scale to approximate meters (typical room depth ~0.5 to 5m)
        depth_metric = depth_normalized * 5.0 * settings.depth_scale

        return depth_metric
