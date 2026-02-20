from __future__ import annotations

import logging

import numpy as np
import torch
from PIL import Image

from app.models.schemas import Detection
from app.vision.model_loader import ModelRegistry

logger = logging.getLogger(__name__)

# Prompt for Grounding DINO: common household / moving items
DETECTION_PROMPT = (
    "sofa. chair. table. desk. bed. mattress. wardrobe. dresser. shelf. bookshelf. "
    "cabinet. nightstand. tv. monitor. computer. lamp. speaker. "
    "refrigerator. washing machine. dryer. dishwasher. oven. microwave. "
    "box. carton. suitcase. bag. bicycle. piano. guitar. plant. painting. mirror. rug."
)


class Detector:
    """Grounding DINO based open-vocabulary object detector."""

    def __init__(self, registry: ModelRegistry) -> None:
        self._model = registry.grounding_dino_model
        self._processor = registry.grounding_dino_processor
        self._device = registry.device

    def detect(
        self,
        images: list[Image.Image],
        threshold: float = 0.3,
    ) -> list[Detection]:
        """Run detection on a list of images and return all detections."""
        all_detections: list[Detection] = []

        for idx, image in enumerate(images):
            detections = self._detect_single(image, idx, threshold)
            all_detections.extend(detections)
            logger.info(
                "Image %d: detected %d objects (threshold=%.2f)", idx, len(detections), threshold
            )

        logger.info("Total detections across %d images: %d", len(images), len(all_detections))
        return all_detections

    def _detect_single(
        self, image: Image.Image, image_index: int, threshold: float
    ) -> list[Detection]:
        inputs = self._processor(
            images=image, text=DETECTION_PROMPT, return_tensors="pt"
        ).to(self._device)

        with torch.no_grad():
            outputs = self._model(**inputs)

        results = self._processor.post_process_grounded_object_detection(
            outputs,
            inputs["input_ids"],
            threshold=threshold,
            text_threshold=threshold,
            target_sizes=[image.size[::-1]],  # (height, width)
        )[0]

        detections: list[Detection] = []
        boxes = results["boxes"].cpu().numpy()
        scores = results["scores"].cpu().numpy()
        # Newer transformers returns integer IDs in "labels", use "text_labels" for strings
        labels = results.get("text_labels", results.get("labels", []))

        for box, score, label in zip(boxes, scores, labels):
            detections.append(
                Detection(
                    bbox=box.tolist(),
                    label=label.strip().rstrip("."),
                    confidence=float(score),
                    image_index=image_index,
                )
            )

        return detections
