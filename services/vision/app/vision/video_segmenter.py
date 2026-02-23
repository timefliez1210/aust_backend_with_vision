from __future__ import annotations

import logging
import os
import tempfile
from dataclasses import dataclass, field

import numpy as np
import torch
from PIL import Image

from app.models.schemas import Detection

logger = logging.getLogger(__name__)


@dataclass
class TrackedObject:
    """A single object tracked across multiple keyframes by SAM 2."""

    object_id: int
    detection: Detection
    masks_per_frame: dict[int, np.ndarray]  # frame_idx -> binary mask (H, W)
    best_frame_index: int
    confidence: float


class VideoSegmenter:
    """SAM 2 video segmenter with temporal tracking.

    Uses SAM 2's video predictor to propagate object masks across keyframes,
    merging duplicate tracks that represent the same physical object.
    """

    def __init__(self, predictor, device: torch.device) -> None:
        self._predictor = predictor
        self._device = device

    def segment_and_track(
        self,
        frames: list[Image.Image],
        detections: list[Detection],
        iou_merge_threshold: float = 0.5,
        cross_label_iou_threshold: float = 0.4,
    ) -> list[TrackedObject]:
        """Segment and track objects across keyframes using SAM 2 video predictor.

        Args:
            frames: Keyframe images (RGB PIL).
            detections: DINO detections with image_index referencing frames.
            iou_merge_threshold: IoU threshold for merging duplicate tracks
                (same label + mask overlap > threshold).
            cross_label_iou_threshold: IoU threshold for merging tracks at the
                same physical location regardless of label.

        Returns:
            List of tracked objects with masks across frames.
        """
        if not detections:
            return []

        # Write frames to temp dir for SAM 2 video predictor
        with tempfile.TemporaryDirectory() as tmpdir:
            for i, frame in enumerate(frames):
                path = os.path.join(tmpdir, f"{i:05d}.jpg")
                frame.save(path, "JPEG", quality=95)

            # Use autocast for consistent dtypes — SAM 2 weights may be
            # bfloat16 while frame features are float32 after CPU→GPU move.
            autocast_ctx = (
                torch.autocast(device_type="cuda", dtype=torch.bfloat16)
                if self._device.type == "cuda"
                else torch.autocast(device_type="cpu", dtype=torch.float32)
            )

            with autocast_ctx:
                # Initialize video state
                inference_state = self._predictor.init_state(video_path=tmpdir)

                # Add each detection as a prompt
                obj_id_counter = 0
                obj_id_to_detection: dict[int, Detection] = {}

                for det in detections:
                    box = np.array(det.bbox, dtype=np.float32)
                    frame_idx = det.image_index

                    self._predictor.add_new_points_or_box(
                        inference_state=inference_state,
                        frame_idx=frame_idx,
                        obj_id=obj_id_counter,
                        box=box,
                    )
                    obj_id_to_detection[obj_id_counter] = det
                    obj_id_counter += 1

                logger.info(
                    "Added %d detection prompts to SAM 2 video predictor",
                    obj_id_counter,
                )

                # Propagate masks across all frames
                raw_tracks: dict[int, dict[int, np.ndarray]] = {}
                for frame_idx, obj_ids, mask_logits in self._predictor.propagate_in_video(
                    inference_state
                ):
                    for i, oid in enumerate(obj_ids):
                        oid = int(oid)
                        mask = (mask_logits[i, 0] > 0.0).cpu().numpy().astype(bool)
                        # Only keep masks with meaningful area
                        if mask.sum() > 50:
                            raw_tracks.setdefault(oid, {})[frame_idx] = mask

                self._predictor.reset_state(inference_state)

        logger.info("SAM 2 propagation: %d tracks across frames", len(raw_tracks))

        # Build TrackedObject list
        tracked: list[TrackedObject] = []
        for oid, masks_dict in raw_tracks.items():
            det = obj_id_to_detection.get(oid)
            if det is None:
                continue
            tracked.append(TrackedObject(
                object_id=oid,
                detection=det,
                masks_per_frame=masks_dict,
                best_frame_index=det.image_index,
                confidence=det.confidence,
            ))

        # Pass 1: Merge duplicate tracks with same label + high mask IoU
        merged = self._merge_duplicate_tracks(tracked, iou_merge_threshold)
        logger.info(
            "After label merge: %d unique objects (from %d tracks)",
            len(merged), len(tracked),
        )

        # Pass 2: Merge tracks at the same physical location regardless of label
        merged = _merge_cross_label_tracks(merged, cross_label_iou_threshold)
        logger.info(
            "After cross-label merge: %d unique objects",
            len(merged),
        )

        return merged

    def segment_single_image(
        self,
        image: Image.Image,
        detections: list[Detection],
    ) -> dict[int, list[np.ndarray]]:
        """Segment detections in a single image using SAM 2 image predictor.

        Compatibility wrapper for the photo pipeline. Returns the same format
        as the old SAM ViT-H segmenter.
        """
        from sam2.sam2_image_predictor import SAM2ImagePredictor

        # For single images, use the image predictor interface
        # The video predictor's underlying model supports image mode too
        img_array = np.array(image)

        grouped: dict[int, list[Detection]] = {}
        for det in detections:
            grouped.setdefault(det.image_index, []).append(det)

        masks_by_image: dict[int, list[np.ndarray]] = {}
        for img_idx, dets in grouped.items():
            masks = []
            for det in dets:
                box = np.array(det.bbox, dtype=np.float32)
                # Use the predictor in image mode
                self._predictor.set_image(img_array)
                mask_logits, scores, _ = self._predictor.predict(
                    point_coords=None,
                    point_labels=None,
                    box=box[None, :],
                    multimask_output=True,
                )
                best_idx = int(np.argmax(scores))
                masks.append(mask_logits[best_idx].astype(bool))
            masks_by_image[img_idx] = masks

        return masks_by_image

    @staticmethod
    def _merge_duplicate_tracks(
        tracks: list[TrackedObject],
        iou_threshold: float,
    ) -> list[TrackedObject]:
        """Merge tracks with the same label and high mask IoU on overlapping frames."""
        if not tracks:
            return []

        # Group by normalized label
        label_groups: dict[str, list[TrackedObject]] = {}
        for track in tracks:
            key = track.detection.label.lower().strip()
            label_groups.setdefault(key, []).append(track)

        merged: list[TrackedObject] = []
        for label, group in label_groups.items():
            clusters: list[list[TrackedObject]] = []

            for track in group:
                placed = False
                for cluster in clusters:
                    rep = cluster[0]
                    iou = _compute_track_iou(rep.masks_per_frame, track.masks_per_frame)
                    if iou > iou_threshold:
                        cluster.append(track)
                        placed = True
                        break
                if not placed:
                    clusters.append([track])

            for cluster in clusters:
                # Keep the detection with highest confidence as primary
                best = max(cluster, key=lambda t: t.confidence)
                # Merge all masks
                all_masks: dict[int, np.ndarray] = {}
                for track in cluster:
                    for fidx, mask in track.masks_per_frame.items():
                        if fidx not in all_masks:
                            all_masks[fidx] = mask
                        else:
                            all_masks[fidx] = all_masks[fidx] | mask

                merged.append(TrackedObject(
                    object_id=best.object_id,
                    detection=best.detection,
                    masks_per_frame=all_masks,
                    best_frame_index=best.best_frame_index,
                    confidence=best.confidence,
                ))

        return merged


def _merge_cross_label_tracks(
    tracks: list[TrackedObject],
    iou_threshold: float = 0.4,
) -> list[TrackedObject]:
    """Second-pass merge: same physical location = same object, regardless of label.

    Handles cases like "armchair chair" + "chair" + "armchair" all referring to
    the same physical object detected by different prompt groups.
    """
    if not tracks:
        return []

    merged: list[TrackedObject] = []
    used: set[int] = set()

    for i, track_a in enumerate(tracks):
        if i in used:
            continue
        cluster = [track_a]
        for j, track_b in enumerate(tracks):
            if j <= i or j in used:
                continue
            iou = _compute_track_iou(track_a.masks_per_frame, track_b.masks_per_frame)
            if iou > iou_threshold:
                cluster.append(track_b)
                used.add(j)
        used.add(i)

        # Keep highest-confidence detection as representative
        best = max(cluster, key=lambda t: t.confidence)
        all_masks: dict[int, np.ndarray] = {}
        for t in cluster:
            for fidx, mask in t.masks_per_frame.items():
                if fidx not in all_masks:
                    all_masks[fidx] = mask
                else:
                    all_masks[fidx] = all_masks[fidx] | mask

        merged.append(TrackedObject(
            object_id=best.object_id,
            detection=best.detection,
            masks_per_frame=all_masks,
            best_frame_index=best.best_frame_index,
            confidence=best.confidence,
        ))

    return merged


def _compute_track_iou(
    masks_a: dict[int, np.ndarray],
    masks_b: dict[int, np.ndarray],
) -> float:
    """Compute average mask IoU over overlapping frames between two tracks."""
    common_frames = set(masks_a.keys()) & set(masks_b.keys())
    if not common_frames:
        return 0.0

    ious = []
    for fidx in common_frames:
        ma = masks_a[fidx]
        mb = masks_b[fidx]
        intersection = (ma & mb).sum()
        union = (ma | mb).sum()
        if union > 0:
            ious.append(intersection / union)

    return float(np.mean(ious)) if ious else 0.0
