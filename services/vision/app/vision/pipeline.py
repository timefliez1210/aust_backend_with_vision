from __future__ import annotations

import logging
import time

import numpy as np
from PIL import Image
from scipy.spatial.distance import cosine as cosine_distance

from app.config import settings
from app.models.schemas import (
    DetectedItem,
    EstimateResponse,
    ItemDimensions,
    VolumeEstimate,
    get_packing_multiplier,
)
from app.vision.depth import DepthEstimator
from app.vision.detector import Detector
from app.vision.model_loader import ModelRegistry
from app.vision.segmenter import Segmenter
from app.vision.vlm_dedup import vlm_dedup
from app.vision.volume import VolumeCalculator

logger = logging.getLogger(__name__)


class VisionPipeline:
    """Orchestrates the full vision pipeline: detect -> segment -> depth -> volume.

    Includes within-image deduplication to merge overlapping detections.
    """

    def __init__(self, registry: ModelRegistry) -> None:
        self._registry = registry
        self._detector = Detector(registry)
        self._segmenter = Segmenter(registry)
        self._depth_estimator = DepthEstimator(registry)
        self._volume_calculator = VolumeCalculator()
        self._similarity_threshold = settings.dedup_similarity_threshold

    def run(
        self,
        job_id: str,
        images: list[Image.Image],
        detection_threshold: float | None = None,
    ) -> EstimateResponse:
        """Execute the full pipeline and return the estimate response."""
        t0 = time.monotonic()
        threshold = detection_threshold or settings.detection_threshold

        logger.info("Pipeline start: job_id=%s, images=%d, threshold=%.2f",
                     job_id, len(images), threshold)

        # Stage 1: Detection (multi-prompt for better recall)
        detections = self._detector.detect_multi_prompt(
            images,
            threshold=threshold,
            nms_iou_threshold=settings.detection_nms_threshold,
        )
        if not detections:
            return EstimateResponse(
                job_id=job_id,
                status="completed",
                detected_items=[],
                total_volume_m3=0.0,
                confidence_score=0.0,
                processing_time_ms=int((time.monotonic() - t0) * 1000),
            )

        # Stage 2: Segmentation
        masks_by_image = self._segmenter.segment(images, detections)

        # Stage 3: Depth estimation
        depth_maps = self._depth_estimator.estimate(images)

        # Stage 4: Volume calculation
        volume_estimates = self._volume_calculator.estimate_volumes(
            images, detections, masks_by_image, depth_maps
        )

        # Stage 5: Cross-image deduplication
        merged_items = self._deduplicate(volume_estimates)

        # Stage 6: VLM cross-image deduplication (Qwen2-VL-7B)
        # Identifies items that are the same physical object across multiple photos.
        merged_items = vlm_dedup(merged_items, self._registry)

        # Stage 7: Apply packing multipliers to geometric-sourced items only.
        # RE-sourced volumes already include handling/packing space.
        for i, item in enumerate(merged_items):
            if item.volume_source == "re":
                continue  # RE volumes already include packing space
            multiplier = get_packing_multiplier(item.category)
            if multiplier != 1.0:
                merged_items[i] = DetectedItem(
                    name=item.name,
                    volume_m3=round(item.volume_m3 * multiplier, 4),
                    dimensions=item.dimensions,
                    confidence=item.confidence,
                    seen_in_images=item.seen_in_images,
                    category=item.category,
                    german_name=item.german_name,
                    re_value=item.re_value,
                    units=item.units,
                    volume_source=item.volume_source,
                    bbox=item.bbox,
                    bbox_image_index=item.bbox_image_index,
                    crop_base64=item.crop_base64,
                )

        total_volume = sum(item.volume_m3 for item in merged_items)
        avg_confidence = (
            sum(item.confidence for item in merged_items) / len(merged_items)
            if merged_items
            else 0.0
        )

        elapsed_ms = int((time.monotonic() - t0) * 1000)
        logger.info(
            "Pipeline complete: job_id=%s, items=%d, total_volume=%.3f m3, time=%d ms",
            job_id, len(merged_items), total_volume, elapsed_ms,
        )

        return EstimateResponse(
            job_id=job_id,
            status="completed",
            detected_items=merged_items,
            total_volume_m3=round(total_volume, 4),
            confidence_score=round(avg_confidence, 3),
            processing_time_ms=elapsed_ms,
        )

    def _deduplicate(self, estimates: list[VolumeEstimate]) -> list[DetectedItem]:
        """Merge duplicate detections within each image.

        Uses label matching + feature vector cosine similarity.
        Only deduplicates within the same image (cross-image dedup
        is disabled due to unreliable feature vectors).
        """
        if not estimates:
            return []

        # Group by label
        label_groups: dict[str, list[VolumeEstimate]] = {}
        for est in estimates:
            label_groups.setdefault(est.label.lower(), []).append(est)

        merged_items: list[DetectedItem] = []

        for label, group in label_groups.items():
            clusters = self._cluster_by_similarity(group)
            for cluster in clusters:
                merged = self._merge_cluster(cluster)
                merged_items.append(merged)

        return merged_items

    def _cluster_by_similarity(
        self, estimates: list[VolumeEstimate]
    ) -> list[list[VolumeEstimate]]:
        """Cluster estimates of the same label by feature vector similarity.

        Only deduplicates within the same image — cross-image dedup is
        unreliable with the current low-dimensional feature vectors
        (mean color + centroid + OBB dims) because centroids are in
        per-image coordinate frames and not comparable across images.
        """
        clusters: list[list[VolumeEstimate]] = []

        for est in estimates:
            placed = False
            if est.feature_vector:
                for cluster in clusters:
                    representative = cluster[0]
                    # Only merge items from the same image
                    if representative.image_index != est.image_index:
                        continue
                    if representative.feature_vector:
                        similarity = 1.0 - cosine_distance(
                            est.feature_vector, representative.feature_vector
                        )
                        if similarity >= self._similarity_threshold:
                            cluster.append(est)
                            placed = True
                            break

            if not placed:
                clusters.append([est])

        return clusters

    @staticmethod
    def _merge_cluster(cluster: list[VolumeEstimate]) -> DetectedItem:
        """Merge a cluster of duplicate observations into a single DetectedItem.

        Takes the observation with the highest confidence as the primary,
        and averages dimensions across all observations.

        For RE-sourced items, uses the RE volume directly (not averaged
        geometric volume), since RE is the authoritative source.
        """
        best = max(cluster, key=lambda e: e.confidence)

        # Average dimensions across observations for stability
        avg_length = np.mean([e.dimensions.length_m for e in cluster])
        avg_width = np.mean([e.dimensions.width_m for e in cluster])
        avg_height = np.mean([e.dimensions.height_m for e in cluster])

        # For RE-sourced items, use the RE volume from the best observation.
        # For geometric items, use averaged dimensions to compute volume.
        if best.volume_source == "re":
            volume = best.volume_m3
        else:
            volume = float(avg_length * avg_width * avg_height)

        # Collect which images this item appeared in
        seen_in = sorted({e.image_index for e in cluster})

        boosted_confidence = best.confidence

        return DetectedItem(
            name=best.label,
            volume_m3=round(volume, 4),
            dimensions=ItemDimensions(
                length_m=round(float(avg_length), 3),
                width_m=round(float(avg_width), 3),
                height_m=round(float(avg_height), 3),
            ),
            confidence=round(boosted_confidence, 3),
            seen_in_images=seen_in,
            category=best.category,
            german_name=best.german_name,
            re_value=best.re_value,
            units=best.units,
            volume_source=best.volume_source,
            bbox=best.bbox,
            bbox_image_index=best.image_index,
            crop_base64=best.crop_base64,
        )
