from __future__ import annotations

import logging

import numpy as np
import torch
from PIL import Image

from app.models.schemas import Detection
from app.vision.model_loader import ModelRegistry

logger = logging.getLogger(__name__)


class Segmenter:
    """SAM 2 based instance segmenter. Takes detections and produces binary masks.

    Uses SAM 2's image predictor interface (drop-in replacement for SAM ViT-H).
    6x faster inference, 4x smaller model, better mask accuracy.
    """

    def __init__(self, registry: ModelRegistry) -> None:
        self._predictor = registry.sam2_image_predictor
        self._device = registry.device

    def segment(
        self,
        images: list[Image.Image],
        detections: list[Detection],
    ) -> dict[int, list[np.ndarray]]:
        """Produce segmentation masks for each detection.

        Returns a dict mapping image_index to a list of binary masks (H, W)
        aligned with the detections for that image.
        """
        # Group detections by image
        grouped: dict[int, list[Detection]] = {}
        for det in detections:
            grouped.setdefault(det.image_index, []).append(det)

        masks_by_image: dict[int, list[np.ndarray]] = {}

        for img_idx, image in enumerate(images):
            dets = grouped.get(img_idx, [])
            if not dets:
                masks_by_image[img_idx] = []
                continue

            img_array = np.array(image)
            self._predictor.set_image(img_array)

            masks: list[np.ndarray] = []
            for det in dets:
                mask = self._segment_box(det.bbox, img_array.shape[:2])
                masks.append(mask)

            masks_by_image[img_idx] = masks
            logger.info("Image %d: segmented %d objects", img_idx, len(masks))

        return masks_by_image

    def _segment_box(self, bbox: list[float], image_shape: tuple[int, int]) -> np.ndarray:
        """Run SAM 2 with a bounding box prompt, returning the best mask."""
        box_np = np.array(bbox, dtype=np.float32)

        masks, scores, _ = self._predictor.predict(
            point_coords=None,
            point_labels=None,
            box=box_np[None, :],  # (1, 4)
            multimask_output=True,
        )

        # Pick the mask with the highest predicted IoU
        best_idx = int(np.argmax(scores))
        return masks[best_idx].astype(bool)

    def extract_masked_region(
        self, image: Image.Image, mask: np.ndarray
    ) -> np.ndarray:
        """Extract the image region covered by the mask (for feature extraction).

        Returns an RGB numpy array cropped to the mask bounding box with
        background zeroed out.
        """
        img_array = np.array(image)
        # Zero out background
        masked = img_array.copy()
        masked[~mask] = 0

        # Crop to bounding box of the mask
        ys, xs = np.where(mask)
        if len(ys) == 0:
            return masked

        y_min, y_max = ys.min(), ys.max()
        x_min, x_max = xs.min(), xs.max()
        return masked[y_min : y_max + 1, x_min : x_max + 1]
