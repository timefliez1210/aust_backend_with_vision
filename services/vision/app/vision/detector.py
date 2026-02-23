from __future__ import annotations

import logging

import numpy as np
import torch
from PIL import Image

from app.models.schemas import Detection
from app.vision.model_loader import ModelRegistry

logger = logging.getLogger(__name__)

# Single combined prompt (used by photo pipeline)
# MM-Grounding-DINO uses list-of-strings format instead of dot-separated
DETECTION_PROMPT = [
    "sofa", "couch", "armchair", "chair", "stool", "bench", "ottoman",
    "table", "desk", "dining table", "coffee table",
    "bed", "mattress", "crib", "bunk bed",
    "wardrobe", "closet", "dresser", "chest of drawers", "shelf", "bookshelf",
    "cabinet", "cupboard", "nightstand",
    "tv", "monitor", "computer", "printer", "speaker", "stereo",
    "lamp", "floor lamp", "chandelier",
    "refrigerator", "freezer", "washing machine", "dryer", "dishwasher",
    "oven", "stove", "microwave",
    "box", "carton", "moving box", "suitcase", "bag", "basket", "storage container",
    "bicycle", "treadmill", "exercise equipment",
    "piano", "keyboard", "guitar",
    "plant", "painting", "mirror", "rug", "carpet", "curtain",
    "shoe rack", "coat rack", "ironing board", "vacuum cleaner", "fan", "heater",
    "kitchen island", "bar stool", "recliner",
]

# Multi-prompt groups (~10 items each) for better detection recall.
# Shorter prompts produce significantly higher recall.
DETECTION_PROMPTS = [
    # Seating
    ["sofa", "couch", "armchair", "chair", "stool", "bench", "ottoman", "recliner", "bar stool"],
    # Tables & beds
    ["table", "desk", "dining table", "coffee table", "bed", "mattress", "crib", "bunk bed", "kitchen island"],
    # Storage & shelving
    ["wardrobe", "closet", "dresser", "chest of drawers", "shelf", "bookshelf", "cabinet", "cupboard", "nightstand"],
    # Electronics & lighting
    ["tv", "monitor", "computer", "printer", "speaker", "stereo", "lamp", "floor lamp", "chandelier"],
    # Appliances
    ["refrigerator", "freezer", "washing machine", "dryer", "dishwasher", "oven", "stove", "microwave", "heater"],
    # Misc household
    ["bicycle", "treadmill", "piano", "guitar", "plant", "painting", "mirror", "rug", "curtain", "fan",
     "vacuum cleaner", "ironing board", "shoe rack", "coat rack", "aquarium", "stroller"],
]


# Ultra-wide tiling: split frames wider than this into overlapping tiles
_ULTRAWIDE_ASPECT = 2.0
# Target max aspect ratio per tile (keeps objects large enough for DINO)
_TILE_TARGET_ASPECT = 1.6


def _tile_ultrawide(image: Image.Image) -> list[tuple[Image.Image, int]]:
    """Split an ultra-wide image into overlapping tiles for DINO detection.

    Standard aspect ratios (<=2.0) pass through unchanged. Ultra-wide frames
    (e.g. 0.5x phone zoom producing 2640x1080) are split into overlapping tiles
    so objects are large enough for DINO to detect.

    Returns list of (tile_image, x_offset) tuples.
    """
    w, h = image.size
    if w / h <= _ULTRAWIDE_ASPECT:
        return [(image, 0)]

    tile_w = min(int(h * _TILE_TARGET_ASPECT), w)
    overlap = tile_w // 3
    stride = tile_w - overlap

    tiles: list[tuple[Image.Image, int]] = []
    x = 0
    while x + tile_w <= w:
        tiles.append((image.crop((x, 0, x + tile_w, h)), x))
        x += stride

    # Cover right edge if not fully captured
    if not tiles or tiles[-1][1] + tile_w < w:
        right_x = w - tile_w
        tiles.append((image.crop((right_x, 0, w, h)), right_x))

    return tiles


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
        """Run detection on a list of images using the single combined prompt.

        Ultra-wide images (aspect > 2.0) are automatically tiled for better recall.
        """
        all_detections: list[Detection] = []

        for idx, image in enumerate(images):
            tiles = _tile_ultrawide(image)
            frame_dets: list[Detection] = []
            for tile_img, x_offset in tiles:
                dets = self._detect_single_with_prompt(tile_img, idx, threshold, DETECTION_PROMPT)
                if x_offset > 0:
                    for d in dets:
                        d.bbox = [d.bbox[0] + x_offset, d.bbox[1],
                                  d.bbox[2] + x_offset, d.bbox[3]]
                frame_dets.extend(dets)

            if len(tiles) > 1:
                before_nms = len(frame_dets)
                frame_dets = self._nms(frame_dets, iou_threshold=0.7)
                logger.info(
                    "Image %d: detected %d objects (%d tiles, %d before NMS, threshold=%.2f)",
                    idx, len(frame_dets), len(tiles), before_nms, threshold,
                )
            else:
                logger.info(
                    "Image %d: detected %d objects (threshold=%.2f)",
                    idx, len(frame_dets), threshold,
                )

            all_detections.extend(frame_dets)

        logger.info("Total detections across %d images: %d", len(images), len(all_detections))
        return all_detections

    def detect_multi_prompt(
        self,
        images: list[Image.Image],
        threshold: float = 0.3,
        prompt_groups: list[str] | None = None,
        nms_iou_threshold: float = 0.7,
    ) -> list[Detection]:
        """Run detection with multiple prompt groups per image, then NMS.

        Shorter prompts dramatically improve DINO recall. Each group is run
        independently, then per-frame NMS removes overlapping detections.
        Ultra-wide images (aspect > 2.0) are automatically tiled.
        """
        prompt_groups = prompt_groups or DETECTION_PROMPTS
        all_detections: list[Detection] = []

        for idx, image in enumerate(images):
            tiles = _tile_ultrawide(image)
            frame_dets: list[Detection] = []

            for tile_img, x_offset in tiles:
                for prompt in prompt_groups:
                    dets = self._detect_single_with_prompt(tile_img, idx, threshold, prompt)
                    if x_offset > 0:
                        for d in dets:
                            d.bbox = [d.bbox[0] + x_offset, d.bbox[1],
                                      d.bbox[2] + x_offset, d.bbox[3]]
                    frame_dets.extend(dets)

            # NMS within frame: remove overlapping detections across tiles + prompts
            before_nms = len(frame_dets)
            frame_dets = self._nms(frame_dets, iou_threshold=nms_iou_threshold)
            all_detections.extend(frame_dets)
            logger.info(
                "Image %d: %d detections (%d prompts%s, %d before NMS)",
                idx, len(frame_dets), len(prompt_groups),
                f", {len(tiles)} tiles" if len(tiles) > 1 else "",
                before_nms,
            )

        logger.info(
            "Multi-prompt total: %d detections across %d images",
            len(all_detections), len(images),
        )
        return all_detections

    def _detect_single_with_prompt(
        self, image: Image.Image, image_index: int, threshold: float, prompt: list[str],
    ) -> list[Detection]:
        """Run detection on a single image with a list of category labels."""
        inputs = self._processor(
            images=image, text=[prompt], return_tensors="pt"
        ).to(self._device)

        with torch.no_grad():
            outputs = self._model(**inputs)

        results = self._processor.post_process_grounded_object_detection(
            outputs,
            threshold=threshold,
            text_threshold=threshold,
            target_sizes=[(image.height, image.width)],
        )[0]

        detections: list[Detection] = []
        boxes = results["boxes"].cpu().numpy()
        scores = results["scores"].cpu().numpy()
        labels = results.get("text_labels", results.get("labels", []))

        for box, score, label in zip(boxes, scores, labels):
            detections.append(
                Detection(
                    bbox=box.tolist(),
                    label=str(label).strip(),
                    confidence=float(score),
                    image_index=image_index,
                )
            )

        return detections

    @staticmethod
    def _nms(detections: list[Detection], iou_threshold: float) -> list[Detection]:
        """Non-maximum suppression: for overlapping boxes, keep highest confidence."""
        if not detections:
            return []

        # Sort by confidence descending
        dets = sorted(detections, key=lambda d: d.confidence, reverse=True)
        keep: list[Detection] = []

        for det in dets:
            suppress = False
            for kept in keep:
                if _bbox_iou(det.bbox, kept.bbox) > iou_threshold:
                    suppress = True
                    break
            if not suppress:
                keep.append(det)

        return keep


def _bbox_iou(box_a: list[float], box_b: list[float]) -> float:
    """Compute IoU between two [x1, y1, x2, y2] bounding boxes."""
    x1 = max(box_a[0], box_b[0])
    y1 = max(box_a[1], box_b[1])
    x2 = min(box_a[2], box_b[2])
    y2 = min(box_a[3], box_b[3])

    inter = max(0.0, x2 - x1) * max(0.0, y2 - y1)
    area_a = (box_a[2] - box_a[0]) * (box_a[3] - box_a[1])
    area_b = (box_b[2] - box_b[0]) * (box_b[3] - box_b[1])
    union = area_a + area_b - inter

    return inter / union if union > 0 else 0.0
