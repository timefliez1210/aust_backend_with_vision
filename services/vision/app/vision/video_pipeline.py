from __future__ import annotations

import logging
import time

import numpy as np
import open3d as o3d
from PIL import Image
from scipy.ndimage import binary_erosion

from app.config import settings
from app.models.schemas import (
    DetectedItem,
    EstimateResponse,
    ItemDimensions,
    VolumeEstimate,
    classify_item,
    get_max_volume,
    get_packing_multiplier,
    get_reference_dims,
    lookup_re_volume,
)
from app.vision.detector import Detector
from app.vision.keyframe import KeyframeResult, extract_keyframes
from app.vision.model_loader import ModelRegistry
from app.vision.reconstructor import ReconstructionError, ReconstructionResult, Reconstructor
from app.vision.video_segmenter import TrackedObject, VideoSegmenter
from app.vision.volume import _crop_bbox_thumbnail

logger = logging.getLogger(__name__)


def _select_subset(frames: list, n: int) -> tuple[list, list[int]]:
    """Select n evenly-spaced frames from a list.

    Returns (selected_frames, indices_into_original_list).
    """
    if n >= len(frames):
        return frames, list(range(len(frames)))
    indices = np.linspace(0, len(frames) - 1, n, dtype=int).tolist()
    return [frames[i] for i in indices], indices


# Minimum confident points to accept MASt3R reconstruction
_MIN_RECONSTRUCTION_POINTS = 1000

# Floor plane detection: expected ceiling height range (meters)
_CEILING_MIN = 2.2
_CEILING_MAX = 3.5
_CEILING_TARGET = 2.5


class VideoPipeline:
    """Orchestrates the full video pipeline: keyframes -> 3D reconstruction ->
    detection + tracking -> volume estimation.

    GPU memory is managed by swapping models between phases:
    - Phase 1: MASt3R only (~12-15GB)
    - Phase 2: DINO + SAM 2 (~5GB)
    - Phase 3: CPU only (Open3D)
    """

    def __init__(self, registry: ModelRegistry) -> None:
        self._registry = registry

    def run(
        self,
        job_id: str,
        video_bytes: bytes,
        detection_threshold: float | None = None,
        max_keyframes: int | None = None,
    ) -> EstimateResponse:
        """Execute the full video pipeline and return the estimate response.

        Three-tier frame extraction:
        - Dense keyframes (60): used for SAM 2 temporal tracking
        - Recon subset (20): uniform for MASt3R parallax
        - Detection subset (20): pause-aware — seeds with 1 rep per pause, fills uniform
        """
        t0 = time.monotonic()
        threshold = detection_threshold or settings.detection_threshold
        dense_kf = max_keyframes or settings.video_dense_keyframes
        recon_kf = settings.video_recon_keyframes

        logger.info("Video pipeline start: job_id=%s, threshold=%.2f", job_id, threshold)

        # Phase 0: Extract dense keyframes + pause detection (CPU)
        keyframes = extract_keyframes(
            video_bytes,
            max_frames=dense_kf,
            min_frames=settings.video_min_keyframes,
            blur_threshold=settings.video_blur_threshold,
        )
        logger.info(
            "Extracted %d dense keyframes from %d total frames",
            len(keyframes.frames), keyframes.total_frames,
        )

        # Select subsets for reconstruction (uniform for parallax)
        recon_frames, recon_indices = _select_subset(keyframes.frames, recon_kf)

        # Select detection frames: seed with pause reps, fill with uniform
        if keyframes.pause_keyframe_indices:
            detect_indices = list(keyframes.pause_keyframe_indices)
            # Fill remaining with evenly spaced, avoiding duplicates
            uniform = np.linspace(0, len(keyframes.frames) - 1, recon_kf, dtype=int).tolist()
            used = set(detect_indices)
            for idx in uniform:
                if idx not in used and len(detect_indices) < recon_kf:
                    detect_indices.append(idx)
                    used.add(idx)
            detect_indices.sort()
            detect_frames = [keyframes.frames[i] for i in detect_indices]
            logger.info(
                "Pause-aware detection: %d frames (%d pause reps + %d uniform fill) "
                "from %d dense, %d recon frames",
                len(detect_indices), len(keyframes.pause_keyframe_indices),
                len(detect_indices) - len(keyframes.pause_keyframe_indices),
                len(keyframes.frames), len(recon_frames),
            )
        else:
            detect_frames = recon_frames
            detect_indices = recon_indices
            logger.info(
                "Uniform detection: %d recon/detect frames from %d dense",
                len(recon_frames), len(keyframes.frames),
            )

        # Phase 1: 3D Reconstruction (MASt3R on recon subset only)
        try:
            reconstruction = self._run_reconstruction(recon_frames)
            scale_factor = self._validate_and_correct_scale(reconstruction)
            if scale_factor != 1.0:
                logger.info("Applying scale correction factor: %.3f", scale_factor)
                reconstruction.point_cloud *= scale_factor
        except (ReconstructionError, Exception) as exc:
            logger.warning(
                "MASt3R reconstruction failed (%s), falling back to per-keyframe "
                "Depth Anything V2",
                exc,
            )
            return self._fallback_photo_pipeline(job_id, keyframes, threshold, t0)

        # Phase 2: Detection + Tracking (DINO + SAM 2 on GPU)
        self._registry.ensure_detection_models()
        detector = Detector(self._registry)

        detections = detector.detect_multi_prompt(
            detect_frames,
            threshold=threshold,
            nms_iou_threshold=settings.detection_nms_threshold,
        )

        # Remap detection image_index from detect-frame-local to dense-frame-global
        for det in detections:
            det.image_index = detect_indices[det.image_index]

        if not detections:
            elapsed_ms = int((time.monotonic() - t0) * 1000)
            return EstimateResponse(
                job_id=job_id,
                status="completed",
                detected_items=[],
                total_volume_m3=0.0,
                confidence_score=0.0,
                processing_time_ms=elapsed_ms,
            )

        # SAM 2 tracking on ALL dense frames
        segmenter = VideoSegmenter(self._registry.sam2_video_predictor, self._registry.device)
        tracked_objects = segmenter.segment_and_track(
            keyframes.frames,
            detections,
            iou_merge_threshold=0.5,
            cross_label_iou_threshold=settings.cross_label_iou_threshold,
        )

        logger.info("Tracked %d unique objects across keyframes", len(tracked_objects))

        # Build mapping: dense_frame_index -> recon_frame_index
        recon_mapping = {
            dense_idx: recon_idx
            for recon_idx, dense_idx in enumerate(recon_indices)
        }

        # Phase 3: Volume estimation (CPU + Open3D)
        volume_results = self._compute_volumes(
            tracked_objects, reconstruction, keyframes.frames, recon_mapping
        )

        items = self._process_items(volume_results)

        total_volume = sum(item.volume_m3 for item in items)
        avg_confidence = (
            sum(item.confidence for item in items) / len(items) if items else 0.0
        )

        elapsed_ms = int((time.monotonic() - t0) * 1000)
        logger.info(
            "Video pipeline complete: job_id=%s, items=%d, total_volume=%.3f m3, time=%d ms",
            job_id, len(items), total_volume, elapsed_ms,
        )

        return EstimateResponse(
            job_id=job_id,
            status="completed",
            detected_items=items,
            total_volume_m3=round(total_volume, 4),
            confidence_score=round(avg_confidence, 3),
            processing_time_ms=elapsed_ms,
        )

    def _process_items(
        self,
        volume_results: list[tuple[VolumeEstimate, list[int]]],
    ) -> list[DetectedItem]:
        """Apply packing multipliers, outlier filtering, and build DetectedItem list."""
        items: list[DetectedItem] = []
        for est, seen_frames in volume_results:
            if est.volume_source == "geometric":
                multiplier = get_packing_multiplier(est.category)
                if multiplier != 1.0:
                    est = VolumeEstimate(
                        label=est.label,
                        volume_m3=round(est.volume_m3 * multiplier, 4),
                        dimensions=est.dimensions,
                        confidence=est.confidence,
                        image_index=est.image_index,
                        category=est.category,
                        feature_vector=est.feature_vector,
                        volume_source=est.volume_source,
                        re_value=est.re_value,
                        german_name=est.german_name,
                        units=est.units,
                        bbox=est.bbox,
                        crop_base64=est.crop_base64,
                    )

            # Outlier filtering
            max_vol = get_max_volume(est.category)
            if est.volume_m3 > max_vol:
                logger.warning(
                    "Outlier filtered: '%s' volume=%.3f m3 > max %.1f m3",
                    est.label, est.volume_m3, max_vol,
                )
                continue

            items.append(DetectedItem(
                name=est.label,
                volume_m3=est.volume_m3,
                dimensions=est.dimensions,
                confidence=est.confidence,
                seen_in_images=seen_frames,
                category=est.category,
                german_name=est.german_name,
                re_value=est.re_value,
                units=est.units,
                volume_source=est.volume_source,
                bbox=est.bbox,
                bbox_image_index=est.image_index,
                crop_base64=est.crop_base64,
            ))

        return items

    def _run_reconstruction(self, frames: list[Image.Image]) -> ReconstructionResult:
        """Run MASt3R 3D reconstruction on a subset of frames, managing GPU memory."""
        # Swap models: unload detection, load MASt3R
        self._registry.unload_detection_models()
        self._registry.load_mast3r()

        try:
            reconstructor = Reconstructor(self._registry.mast3r_model, self._registry.device)
            result = reconstructor.reconstruct(
                frames,
                min_conf=settings.mast3r_min_conf,
                niter=settings.mast3r_alignment_iters,
            )
        finally:
            # Always unload MASt3R and restore detection models
            self._registry.unload_mast3r()

        return result

    def _validate_and_correct_scale(self, reconstruction: ReconstructionResult) -> float:
        """Validate MASt3R scale by detecting floor plane and measuring ceiling height.

        Returns a scale correction factor (1.0 if scale is good).
        """
        pts = reconstruction.point_cloud
        if len(pts) < 100:
            return 1.0

        try:
            pcd = o3d.geometry.PointCloud()
            pcd.points = o3d.utility.Vector3dVector(pts)

            # Detect floor plane (largest horizontal plane)
            plane_model, inliers = pcd.segment_plane(
                distance_threshold=0.05, ransac_n=3, num_iterations=1000
            )
            # plane_model: [a, b, c, d] where ax + by + cz + d = 0
            normal = np.array(plane_model[:3])

            # Check if plane is roughly horizontal (normal ~= [0, ±1, 0] or [0, 0, ±1])
            # MASt3R typically uses y-up or z-up depending on initialization
            vertical_component = max(abs(normal[1]), abs(normal[2]))
            if vertical_component < 0.8:
                logger.info("Floor plane normal not vertical enough (%.2f), skipping scale check", vertical_component)
                return 1.0

            # Measure ceiling height: range of points along vertical axis
            if abs(normal[1]) > abs(normal[2]):
                vertical_axis = 1
            else:
                vertical_axis = 2

            y_values = pts[:, vertical_axis]
            floor_y = np.percentile(y_values, 5)
            ceiling_y = np.percentile(y_values, 95)
            measured_height = abs(ceiling_y - floor_y)

            logger.info("Measured ceiling height: %.2f m", measured_height)

            if _CEILING_MIN <= measured_height <= _CEILING_MAX:
                return 1.0

            # Scale correction
            correction = _CEILING_TARGET / measured_height
            logger.warning(
                "Ceiling height %.2f m outside [%.1f, %.1f] range, "
                "applying scale correction: %.3f",
                measured_height, _CEILING_MIN, _CEILING_MAX, correction,
            )
            return correction

        except Exception as exc:
            logger.warning("Scale validation failed: %s", exc)
            return 1.0

    def _compute_volumes(
        self,
        tracked_objects: list[TrackedObject],
        reconstruction: ReconstructionResult,
        frames: list[Image.Image],
        recon_mapping: dict[int, int] | None = None,
    ) -> list[tuple[VolumeEstimate, list[int]]]:
        """Compute volume for each tracked object by projecting masks into the point cloud.

        Args:
            tracked_objects: Tracked objects with masks indexed by dense frame index.
            reconstruction: MASt3R result indexed by recon frame index.
            frames: All dense keyframes.
            recon_mapping: Maps dense_frame_index -> recon_frame_index. If None,
                assumes 1:1 mapping (backwards compat).

        Returns list of (VolumeEstimate, seen_in_frames) tuples.
        """
        results: list[tuple[VolumeEstimate, list[int]]] = []
        pts_world = reconstruction.point_cloud
        pts_source = reconstruction.point_to_image

        # Scale correction using RE catalog anchors
        re_scale = self._compute_re_scale_correction(
            tracked_objects, reconstruction, frames, recon_mapping
        )
        if abs(re_scale - 1.0) > 0.01:
            logger.info("RE catalog scale correction: %.3f", re_scale)

        for obj in tracked_objects:
            label = obj.detection.label
            category = classify_item(label)

            # Collect 3D points for this object across frames with 3D data
            object_points = self._extract_object_points(
                obj, reconstruction, recon_mapping
            )

            if len(object_points) < 20:
                logger.warning(
                    "Too few 3D points (%d) for '%s', skipping",
                    len(object_points), label,
                )
                continue

            # Fit OBB
            pcd = o3d.geometry.PointCloud()
            pcd.points = o3d.utility.Vector3dVector(object_points)
            pcd, _ = pcd.remove_statistical_outlier(nb_neighbors=20, std_ratio=2.0)

            if len(pcd.points) < 10:
                continue

            try:
                obb = pcd.get_oriented_bounding_box()
                extent = obb.extent * re_scale
                dims = sorted(extent.tolist(), reverse=True)
            except Exception:
                aabb = pcd.get_axis_aligned_bounding_box()
                extent = aabb.get_extent() * re_scale
                dims = sorted(extent.tolist(), reverse=True)

            # Generate crop thumbnail from best frame
            best_frame = frames[obj.best_frame_index]
            crop_b64 = _crop_bbox_thumbnail(np.array(best_frame), obj.detection.bbox)

            # RE catalog lookup
            re_result = lookup_re_volume(
                label=label,
                largest_dim_m=dims[0],
                second_dim_m=dims[1] if len(dims) > 1 else None,
                height_m=dims[2] if len(dims) > 2 else None,
            )

            seen_frames = sorted(obj.masks_per_frame.keys())

            if re_result is not None:
                volume_m3, re_total, units, german_name = re_result
                logger.info(
                    "RE lookup: '%s' -> %s (%.1f RE, %d units, %.2f m3) "
                    "[OBB: %.2fx%.2fx%.2f]",
                    label, german_name, re_total, units, volume_m3,
                    dims[0], dims[1], dims[2],
                )
                results.append((VolumeEstimate(
                    label=label,
                    volume_m3=round(volume_m3, 4),
                    dimensions=ItemDimensions(
                        length_m=round(dims[0], 3),
                        width_m=round(dims[1], 3),
                        height_m=round(dims[2], 3),
                    ),
                    confidence=obj.confidence,
                    image_index=obj.best_frame_index,
                    category=category,
                    volume_source="re",
                    re_value=re_total,
                    german_name=german_name,
                    units=units,
                    bbox=list(obj.detection.bbox),
                    crop_base64=crop_b64,
                ), seen_frames))
            else:
                # Geometric fallback
                obb_volume = float(np.prod(dims))
                logger.info(
                    "Geometric fallback: '%s' OBB=%.4f m3 [%.2fx%.2fx%.2f]",
                    label, obb_volume, dims[0], dims[1], dims[2],
                )
                results.append((VolumeEstimate(
                    label=label,
                    volume_m3=round(obb_volume, 4),
                    dimensions=ItemDimensions(
                        length_m=round(dims[0], 3),
                        width_m=round(dims[1], 3),
                        height_m=round(dims[2], 3),
                    ),
                    confidence=obj.confidence,
                    image_index=obj.best_frame_index,
                    category=category,
                    volume_source="geometric",
                    bbox=list(obj.detection.bbox),
                    crop_base64=crop_b64,
                ), seen_frames))

        return results

    def _extract_object_points(
        self,
        obj: TrackedObject,
        reconstruction: ReconstructionResult,
        recon_mapping: dict[int, int] | None = None,
    ) -> np.ndarray:
        """Extract 3D points belonging to an object by projecting SAM 2 masks
        into the MASt3R point cloud.

        For each frame where the object has a mask AND 3D data exists:
        1. Get points originating from that frame (via point_to_image)
        2. Project those points to 2D using camera intrinsics + pose
        3. Keep only points that fall inside the (eroded) SAM 2 mask

        Args:
            obj: Tracked object with masks indexed by dense frame index.
            reconstruction: MASt3R result indexed by recon frame index.
            recon_mapping: Maps dense_frame_index -> recon_frame_index. If None,
                assumes 1:1 mapping (backwards compat).
        """
        pts_world = reconstruction.point_cloud
        pts_source = reconstruction.point_to_image
        all_object_pts = []
        logged_resolutions = False

        for frame_idx, mask in obj.masks_per_frame.items():
            # Map dense frame index to reconstruction frame index
            if recon_mapping is not None:
                recon_idx = recon_mapping.get(frame_idx)
                if recon_idx is None:
                    continue  # This dense frame has no 3D data
            else:
                recon_idx = frame_idx

            if recon_idx >= len(reconstruction.camera_poses):
                continue

            if not logged_resolutions:
                mast3r_shape = reconstruction.depth_maps[recon_idx].shape
                logger.info(
                    "Projection resolutions for '%s': mask=%s, mast3r=%s, orig=%s",
                    obj.detection.label, mask.shape, mast3r_shape,
                    reconstruction.image_sizes[recon_idx],
                )
                logged_resolutions = True

            # Erode mask by 2px to avoid boundary noise
            eroded_mask = binary_erosion(mask, iterations=2)
            if eroded_mask.sum() < 10:
                eroded_mask = mask  # use original if erosion removed too much

            # Get points from this recon frame
            frame_mask = pts_source == recon_idx
            if frame_mask.sum() == 0:
                continue

            frame_pts = pts_world[frame_mask]

            # Project to 2D
            cam2world = reconstruction.camera_poses[recon_idx]
            world2cam = np.linalg.inv(cam2world)
            K = reconstruction.intrinsics[recon_idx]

            # Transform to camera frame
            pts_cam = (world2cam[:3, :3] @ frame_pts.T + world2cam[:3, 3:]).T

            # Only keep points in front of camera
            in_front = pts_cam[:, 2] > 0.01
            if in_front.sum() == 0:
                continue

            pts_cam_valid = pts_cam[in_front]
            frame_pts_valid = frame_pts[in_front]

            # Project to pixel coordinates
            pts_2d = (K @ pts_cam_valid.T).T
            px = (pts_2d[:, 0] / pts_2d[:, 2]).astype(int)
            py = (pts_2d[:, 1] / pts_2d[:, 2]).astype(int)

            # Check which projected points fall inside the mask.
            # K produces coordinates in MASt3R's processing resolution (e.g. 512x208),
            # NOT the original image resolution. Scale from MASt3R space to mask space.
            mask_h, mask_w = eroded_mask.shape
            mast3r_h, mast3r_w = reconstruction.depth_maps[recon_idx].shape
            px_scaled = (px * mask_w / mast3r_w).astype(int)
            py_scaled = (py * mask_h / mast3r_h).astype(int)

            in_bounds = (
                (px_scaled >= 0) & (px_scaled < mask_w) &
                (py_scaled >= 0) & (py_scaled < mask_h)
            )

            if in_bounds.sum() == 0:
                continue

            inside_mask = eroded_mask[py_scaled[in_bounds], px_scaled[in_bounds]]
            valid_pts = frame_pts_valid[in_bounds][inside_mask]
            all_object_pts.append(valid_pts)

        if not all_object_pts:
            return np.empty((0, 3))

        return np.concatenate(all_object_pts, axis=0)

    def _compute_re_scale_correction(
        self,
        tracked_objects: list[TrackedObject],
        reconstruction: ReconstructionResult,
        frames: list[Image.Image],
        recon_mapping: dict[int, int] | None = None,
    ) -> float:
        """Use high-confidence RE catalog items as scale anchors.

        Compares measured OBB dimensions against known RE reference dimensions
        and returns a median correction factor.
        """
        scale_ratios = []

        for obj in tracked_objects:
            label = obj.detection.label
            ref_dims = get_reference_dims(label)
            if ref_dims is None:
                continue

            # Quick OBB measurement for scale reference
            object_points = self._extract_object_points(obj, reconstruction, recon_mapping)
            if len(object_points) < 20:
                continue

            pcd = o3d.geometry.PointCloud()
            pcd.points = o3d.utility.Vector3dVector(object_points)
            pcd, _ = pcd.remove_statistical_outlier(nb_neighbors=20, std_ratio=2.0)
            if len(pcd.points) < 10:
                continue

            try:
                obb = pcd.get_oriented_bounding_box()
                measured = sorted(obb.extent.tolist(), reverse=True)
            except Exception:
                continue

            ref_sorted = sorted(ref_dims, reverse=True)
            if measured[0] > 0.01:
                ratio = ref_sorted[0] / measured[0]
                if 0.2 < ratio < 5.0:
                    scale_ratios.append(ratio)
                    logger.info(
                        "Scale anchor: '%s' measured=%.2fm ref=%.2fm ratio=%.3f",
                        label, measured[0], ref_sorted[0], ratio,
                    )

        if not scale_ratios:
            return 1.0

        median_scale = float(np.median(scale_ratios))
        logger.info(
            "RE scale correction: median=%.3f from %d anchors",
            median_scale, len(scale_ratios),
        )
        return median_scale

    def _fallback_photo_pipeline(
        self,
        job_id: str,
        keyframes: KeyframeResult,
        threshold: float,
        t0: float,
    ) -> EstimateResponse:
        """Fall back to per-keyframe Depth Anything V2 photo pipeline
        when MASt3R reconstruction fails.
        """
        from app.vision.pipeline import VisionPipeline

        logger.info("Running fallback photo pipeline on %d keyframes", len(keyframes.frames))
        self._registry.ensure_detection_models()

        pipeline = VisionPipeline(self._registry)
        result = pipeline.run(
            job_id=job_id,
            images=keyframes.frames,
            detection_threshold=threshold,
        )

        # Adjust processing time to include keyframe extraction
        total_ms = int((time.monotonic() - t0) * 1000)
        return EstimateResponse(
            job_id=result.job_id,
            status=result.status,
            detected_items=result.detected_items,
            total_volume_m3=result.total_volume_m3,
            confidence_score=result.confidence_score,
            processing_time_ms=total_ms,
        )
