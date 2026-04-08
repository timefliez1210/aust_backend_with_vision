"""AR per-item volume estimation pipeline.

Unlike VisionPipeline (generic detection over all images), ARVisionPipeline
processes each labeled item independently using the label the user assigned
on device:

  Phase 1 (detection models on GPU):
    For each item — DINO with exact label prompt → SAM2 mask → Depth Anything V2

  Phase 2 (single GPU swap):
    MASt3R MVS reconstruction for every item that has ≥ 2 frames.
    All reconstructions are batched into one swap to avoid repeated
    unload/reload cycles between items.

  Phase 3 (CPU, Open3D):
    For items with reconstruction: apply SAM2 masks to point cloud → DBSCAN
    → OBB → RE catalog lookup.
    For single-frame items: single-view depth + OBB → RE catalog.

Depth maps from the phone (LiDAR) are uploaded to S3 by the Rust backend but
are not passed to Modal — DAv2 provides depth for all items here.
"""
from __future__ import annotations

import json
import logging
import time
from dataclasses import dataclass
from io import BytesIO

import numpy as np
import open3d as o3d
from PIL import Image
from scipy.ndimage import binary_erosion
from sklearn.cluster import DBSCAN

from app.models.schemas import (
    DetectedItem,
    EstimateResponse,
    ItemDimensions,
    classify_item,
    get_item_flags,
    get_max_volume,
    get_packing_multiplier,
    lookup_re_volume,
)
from app.vision.depth import DepthEstimator
from app.vision.detector import Detector
from app.vision.model_loader import ModelRegistry
from app.vision.reconstructor import ReconstructionError, Reconstructor
from app.vision.segmenter import Segmenter
from app.vision.volume import VolumeCalculator, _crop_bbox_thumbnail, compute_intrinsics

logger = logging.getLogger(__name__)

# DINO confidence thresholds for labeled-item detection.
# Lower than the generic photo pipeline because the prompt is already exact.
_THRESHOLD_PRIMARY = 0.20
_THRESHOLD_FALLBACK = 0.10

# Minimum MASt3R confident points to prefer reconstruction over depth fallback.
_MIN_RECON_POINTS = 500

# RE catalog tolerance: use catalog value when |geometric - re| / re < this.
_RE_TOLERANCE = 0.40


@dataclass
class _ItemSpec:
    label: str
    images: list[Image.Image]
    poses: list[np.ndarray | None]  # 4×4 cam-to-world per frame, or None


class ARVisionPipeline:
    """AR per-item volume estimation pipeline."""

    def __init__(self, registry: ModelRegistry) -> None:
        self._registry = registry
        self._detector = Detector(registry)
        self._segmenter = Segmenter(registry)
        self._depth_estimator = DepthEstimator(registry)

    def run(
        self,
        job_id: str,
        image_bytes_list: list[bytes],
        item_manifest_json: str,
        intrinsics_json: str | None,
        poses_json: str | None,
    ) -> EstimateResponse:
        """Run the full AR pipeline and return the estimate response."""
        t0 = time.monotonic()

        manifest: list[dict] = json.loads(item_manifest_json) if item_manifest_json else []
        intrinsics_dict: dict | None = json.loads(intrinsics_json) if intrinsics_json else None
        flat_poses: list | None = json.loads(poses_json) if poses_json else None

        items = self._group_by_item(image_bytes_list, manifest, flat_poses)
        if not items:
            return EstimateResponse(
                job_id=job_id, status="completed",
                detected_items=[], total_volume_m3=0.0,
                confidence_score=0.0,
                processing_time_ms=int((time.monotonic() - t0) * 1000),
            )

        logger.info(
            "AR pipeline start: job_id=%s, items=%d, total_frames=%d",
            job_id, len(items), sum(len(i.images) for i in items),
        )

        # Build intrinsics matrix from device data or fallback HFOV
        K = self._build_intrinsics(intrinsics_dict, items[0].images[0])

        # Phase 1: per-item DINO + SAM2 + DAv2 (detection models on GPU)
        phase1: list[tuple | None] = [self._phase1(item, K) for item in items]

        # Phase 2: batch MASt3R for all multi-frame items (one GPU swap)
        recon_by_idx = self._phase2_reconstruct(items, phase1)

        # Phase 3: volume estimation (CPU + Open3D)
        detected_items: list[DetectedItem] = []
        for idx, (item, p1) in enumerate(zip(items, phase1)):
            if p1 is None:
                logger.warning("Item '%s': no detection, skipping", item.label)
                continue

            det, masks, depth_maps = p1

            if idx in recon_by_idx:
                result = self._volume_from_reconstruction(
                    item.label, recon_by_idx[idx], masks, item.images[0], K,
                )
            else:
                result = self._volume_from_depth(
                    item.label, item.images[0], masks[0], depth_maps[0], K, det,
                )

            if result is not None:
                detected_items.append(result)

        total_volume = sum(i.volume_m3 for i in detected_items if i.is_moveable and not i.packs_into_boxes)
        avg_conf = (
            sum(i.confidence for i in detected_items) / len(detected_items)
            if detected_items else 0.0
        )
        elapsed_ms = int((time.monotonic() - t0) * 1000)

        logger.info(
            "AR pipeline complete: job_id=%s, items=%d, total_volume=%.3f m³, time=%d ms",
            job_id, len(detected_items), total_volume, elapsed_ms,
        )

        return EstimateResponse(
            job_id=job_id, status="completed",
            detected_items=detected_items,
            total_volume_m3=round(total_volume, 4),
            confidence_score=round(avg_conf, 3),
            processing_time_ms=elapsed_ms,
        )

    # ------------------------------------------------------------------
    # Grouping
    # ------------------------------------------------------------------

    def _group_by_item(
        self,
        image_bytes_list: list[bytes],
        manifest: list[dict],
        flat_poses: list | None,
    ) -> list[_ItemSpec]:
        items: list[_ItemSpec] = []
        frame_idx = 0

        for spec in manifest:
            label = spec.get("label", "unknown")
            fc = max(1, int(spec.get("frame_count", 1)))
            end = min(frame_idx + fc, len(image_bytes_list))

            images: list[Image.Image] = []
            for i in range(frame_idx, end):
                try:
                    images.append(Image.open(BytesIO(image_bytes_list[i])).convert("RGB"))
                except Exception as exc:
                    logger.warning("Failed to decode AR frame %d for '%s': %s", i, label, exc)

            poses: list[np.ndarray | None] = []
            if flat_poses is not None:
                for i in range(frame_idx, end):
                    raw = flat_poses[i] if i < len(flat_poses) else None
                    if raw is not None:
                        try:
                            poses.append(np.array(raw, dtype=np.float64).reshape(4, 4))
                        except Exception:
                            poses.append(None)
                    else:
                        poses.append(None)
            else:
                poses = [None] * len(images)

            if images:
                items.append(_ItemSpec(label=label, images=images, poses=poses))

            frame_idx = end

        return items

    # ------------------------------------------------------------------
    # Intrinsics
    # ------------------------------------------------------------------

    def _build_intrinsics(
        self, intrinsics_dict: dict | None, reference_image: Image.Image
    ) -> np.ndarray:
        if intrinsics_dict:
            fx = float(intrinsics_dict.get("fx", 0))
            fy = float(intrinsics_dict.get("fy", 0))
            cx = float(intrinsics_dict.get("cx", reference_image.width / 2))
            cy = float(intrinsics_dict.get("cy", reference_image.height / 2))
            if fx > 0 and fy > 0:
                logger.info(
                    "Using device intrinsics: fx=%.1f fy=%.1f cx=%.1f cy=%.1f", fx, fy, cx, cy,
                )
                return np.array([[fx, 0, cx], [0, fy, cy], [0, 0, 1]], dtype=np.float64)

        logger.info("No valid device intrinsics, falling back to default HFOV")
        return compute_intrinsics(reference_image.width, reference_image.height)

    # ------------------------------------------------------------------
    # Phase 1 — DINO + SAM2 + DAv2
    # ------------------------------------------------------------------

    def _phase1(
        self, item: _ItemSpec, K: np.ndarray
    ) -> tuple | None:
        """Detect the labeled item in the first frame, segment all frames, estimate depth.

        Returns (best_detection, masks, depth_maps) or None if no detection.
        """
        best_det = None
        for threshold in [_THRESHOLD_PRIMARY, _THRESHOLD_FALLBACK]:
            dets = self._detector._detect_single_with_prompt(
                item.images[0], 0, threshold, [item.label],
            )
            if dets:
                best_det = max(dets, key=lambda d: d.confidence)
                break

        if best_det is None:
            return None

        # Segment every frame using the bbox from the first frame.
        # In a 28° arc sweep the object stays roughly in the same image region.
        masks: list[np.ndarray] = []
        for img in item.images:
            img_arr = np.array(img)
            self._segmenter._predictor.set_image(img_arr)
            masks.append(self._segmenter._segment_box(best_det.bbox, img_arr.shape[:2]))

        depth_maps = self._depth_estimator.estimate(item.images)

        logger.info(
            "Phase 1 '%s': detection conf=%.2f, %d frames segmented",
            item.label, best_det.confidence, len(masks),
        )
        return best_det, masks, depth_maps

    # ------------------------------------------------------------------
    # Phase 2 — MASt3R batch reconstruction
    # ------------------------------------------------------------------

    def _phase2_reconstruct(
        self, items: list[_ItemSpec], phase1: list[tuple | None]
    ) -> dict[int, object]:
        """Run MASt3R for all multi-frame items in a single GPU swap."""
        multi = [
            (i, items[i])
            for i, p1 in enumerate(phase1)
            if p1 is not None and len(items[i].images) >= 2
        ]
        if not multi:
            return {}

        logger.info("Phase 2: MASt3R reconstruction for %d item(s)", len(multi))
        self._registry.unload_detection_models()
        self._registry.load_mast3r()

        recon_by_idx: dict[int, object] = {}
        try:
            for item_idx, item in multi:
                try:
                    rec = Reconstructor(self._registry.mast3r_model, self._registry.device)
                    result = rec.reconstruct(item.images, min_conf=1.5, niter=200)
                    if len(result.point_cloud) >= _MIN_RECON_POINTS:
                        recon_by_idx[item_idx] = result
                        logger.info(
                            "MASt3R '%s': %d points", item.label, len(result.point_cloud),
                        )
                    else:
                        logger.warning(
                            "MASt3R '%s': only %d points, falling back to depth",
                            item.label, len(result.point_cloud),
                        )
                except ReconstructionError as exc:
                    logger.warning("MASt3R failed for '%s': %s", item.label, exc)
                except Exception:
                    logger.exception("MASt3R error for '%s'", item.label)
        finally:
            self._registry.unload_mast3r()
            self._registry.ensure_detection_models()

        return recon_by_idx

    # ------------------------------------------------------------------
    # Phase 3a — volume from MASt3R reconstruction
    # ------------------------------------------------------------------

    def _volume_from_reconstruction(
        self,
        label: str,
        recon,
        masks: list[np.ndarray],
        reference_image: Image.Image,
        K: np.ndarray,
    ) -> DetectedItem | None:
        """Project reconstruction point cloud through SAM2 masks → DBSCAN → OBB."""
        fx, fy = K[0, 0], K[1, 1]
        cx, cy = K[0, 2], K[1, 2]

        all_pts: list[np.ndarray] = []

        for frame_idx, mask in enumerate(masks):
            if frame_idx >= len(recon.depth_maps):
                break

            frame_flag = recon.point_to_image == frame_idx
            frame_pts = recon.point_cloud[frame_flag]
            if len(frame_pts) == 0:
                continue

            # Project world points into this camera frame
            cam2world = recon.camera_poses[frame_idx]
            world2cam = np.linalg.inv(cam2world)
            pts_cam = (world2cam[:3, :3] @ frame_pts.T + world2cam[:3, 3:]).T

            z = pts_cam[:, 2]
            valid_z = z > 0.01

            h, w = recon.depth_maps[frame_idx].shape
            px = np.round(pts_cam[valid_z, 0] / z[valid_z] * fx + cx).astype(int)
            py = np.round(pts_cam[valid_z, 1] / z[valid_z] * fy + cy).astype(int)
            in_bounds = (px >= 0) & (px < w) & (py >= 0) & (py < h)
            px, py = px[in_bounds], py[in_bounds]
            valid_indices = np.where(valid_z)[0][in_bounds]

            eroded = binary_erosion(mask, iterations=1)
            in_mask = eroded[py, px]
            selected = frame_pts[valid_indices[in_mask]]
            if len(selected) > 0:
                all_pts.append(selected)

        if not all_pts:
            logger.warning("No masked points for '%s' from reconstruction, using depth", label)
            return None

        pts = np.concatenate(all_pts, axis=0)

        # DBSCAN to remove outlier clusters
        if len(pts) >= 20:
            std = np.std(pts, axis=0)
            eps = float(np.median(std)) * 0.5
            eps = max(0.01, min(eps, 0.5))
            labels = DBSCAN(eps=eps, min_samples=5, n_jobs=1).fit_predict(pts)
            main = pts[labels == 0]
            if len(main) >= 10:
                pts = main

        if len(pts) < 10:
            logger.warning("Too few points after DBSCAN for '%s'", label)
            return None

        pcd = o3d.geometry.PointCloud()
        pcd.points = o3d.utility.Vector3dVector(pts)
        pcd, _ = pcd.remove_statistical_outlier(nb_neighbors=20, std_ratio=2.0)

        if len(pcd.points) < 10:
            return None

        try:
            obb = pcd.get_oriented_bounding_box()
            extent = obb.extent
        except Exception:
            aabb = pcd.get_axis_aligned_bounding_box()
            extent = aabb.get_extent()

        dims = sorted(extent.tolist(), reverse=True)
        obb_volume = float(np.prod(extent))

        logger.info(
            "OBB '%s': %.2f × %.2f × %.2f m = %.4f m³ (from reconstruction)",
            label, dims[0], dims[1], dims[2] if len(dims) > 2 else 0, obb_volume,
        )
        return self._finalize_item(label, dims, obb_volume, reference_image)

    # ------------------------------------------------------------------
    # Phase 3b — volume from single-frame depth
    # ------------------------------------------------------------------

    def _volume_from_depth(
        self,
        label: str,
        image: Image.Image,
        mask: np.ndarray,
        depth_map: np.ndarray,
        K: np.ndarray,
        det,
    ) -> DetectedItem | None:
        """Delegate to VolumeCalculator._estimate_single, then convert to DetectedItem."""
        vc = VolumeCalculator()
        est = vc._estimate_single(det, mask, depth_map, K, np.array(image))
        if est is None:
            return None

        is_moveable, packs_into_boxes = get_item_flags(label)
        return DetectedItem(
            name=est.label,
            volume_m3=est.volume_m3,
            dimensions=est.dimensions,
            confidence=est.confidence,
            seen_in_images=[0],
            category=est.category,
            german_name=est.german_name,
            re_value=est.re_value,
            units=est.units,
            volume_source=est.volume_source,
            bbox=est.bbox,
            bbox_image_index=0,
            crop_base64=est.crop_base64,
            is_moveable=is_moveable,
            packs_into_boxes=packs_into_boxes,
        )

    # ------------------------------------------------------------------
    # Shared — RE lookup + DetectedItem assembly
    # ------------------------------------------------------------------

    def _finalize_item(
        self,
        label: str,
        dims: list[float],
        obb_volume: float,
        reference_image: Image.Image,
    ) -> DetectedItem | None:
        """Apply RE catalog lookup to OBB dims and build DetectedItem."""
        category = classify_item(label)
        is_moveable, packs_into_boxes = get_item_flags(label)

        apparent = sorted(dims[:2], reverse=True)
        height = dims[2] if len(dims) > 2 else dims[-1]

        re_result = lookup_re_volume(
            label=label,
            largest_dim_m=apparent[0],
            second_dim_m=apparent[1],
            height_m=height,
        )

        if re_result is not None:
            volume_m3, re_total, units, german_name = re_result
            volume_source = "re"
            logger.info(
                "RE lookup '%s': %.1f RE = %.3f m³", label, re_total, volume_m3,
            )
        else:
            volume_m3 = obb_volume * get_packing_multiplier(category)
            re_total, units, german_name = None, None, None
            volume_source = "geometric"

        max_vol = get_max_volume(category)
        if volume_m3 > max_vol:
            logger.warning(
                "Outlier filtered: '%s' %.3f m³ > max %.1f m³", label, volume_m3, max_vol,
            )
            return None

        return DetectedItem(
            name=label,
            volume_m3=round(volume_m3, 4),
            dimensions=ItemDimensions(
                length_m=round(dims[0], 3),
                width_m=round(dims[1], 3) if len(dims) > 1 else round(dims[0] * 0.5, 3),
                height_m=round(height, 3),
            ),
            # AR capture is higher confidence than generic detection — user explicitly
            # selected and labeled each item, so we give a baseline of 0.85 for
            # reconstruction-sourced estimates (depth-sourced keep their DINO score).
            confidence=0.85,
            seen_in_images=[0],
            category=category,
            german_name=german_name,
            re_value=re_total,
            units=units,
            volume_source=volume_source,
            is_moveable=is_moveable,
            packs_into_boxes=packs_into_boxes,
        )
