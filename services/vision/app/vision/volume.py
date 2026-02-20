from __future__ import annotations

import logging

import numpy as np
import open3d as o3d
from PIL import Image

from app.config import settings
from app.models.schemas import (
    Detection,
    ItemDimensions,
    VolumeEstimate,
    classify_item,
    get_reference_dims,
)

logger = logging.getLogger(__name__)

# Assumed horizontal FOV in degrees for a typical smartphone camera
DEFAULT_HFOV_DEG = 70.0


def compute_intrinsics(width: int, height: int, hfov_deg: float = DEFAULT_HFOV_DEG) -> np.ndarray:
    """Compute a pinhole camera intrinsic matrix from image dimensions and HFOV."""
    fx = width / (2.0 * np.tan(np.radians(hfov_deg / 2.0)))
    fy = fx  # assume square pixels
    cx = width / 2.0
    cy = height / 2.0
    return np.array([[fx, 0, cx], [0, fy, cy], [0, 0, 1]], dtype=np.float64)


class VolumeCalculator:
    """Estimate 3D volume of detected objects using depth maps and segmentation masks.

    For each object:
    1. Extract masked depth region
    2. Back-project to 3D point cloud
    3. Compute oriented bounding box (OBB) via Open3D
    4. Calibrate scale using known reference dimensions
    5. Report OBB volume and dimensions
    """

    def estimate_volumes(
        self,
        images: list[Image.Image],
        detections: list[Detection],
        masks_by_image: dict[int, list[np.ndarray]],
        depth_maps: list[np.ndarray],
    ) -> list[VolumeEstimate]:
        """Calculate volume estimates for all detections."""
        # Group detections by image (preserve order consistent with masks)
        grouped: dict[int, list[Detection]] = {}
        for det in detections:
            grouped.setdefault(det.image_index, []).append(det)

        results: list[VolumeEstimate] = []

        for img_idx, image in enumerate(images):
            dets = grouped.get(img_idx, [])
            masks = masks_by_image.get(img_idx, [])
            depth_map = depth_maps[img_idx]

            intrinsics = compute_intrinsics(image.width, image.height)

            # First pass: compute raw (uncalibrated) OBB dimensions
            raw_estimates = []
            for det, mask in zip(dets, masks):
                raw = self._estimate_single(
                    det, mask, depth_map, intrinsics, np.array(image)
                )
                raw_estimates.append(raw)

            # Compute per-image scale factor from known reference items
            scale = self._compute_scale_factor(dets, raw_estimates)
            if abs(scale - 1.0) > 0.01:
                logger.info("Image %d: applying scale factor %.3f", img_idx, scale)

            # Second pass: apply scale and build final estimates
            for raw in raw_estimates:
                if raw is not None:
                    calibrated = self._apply_scale(raw, scale)
                    results.append(calibrated)

        logger.info("Volume estimates computed for %d objects", len(results))
        return results

    def _compute_scale_factor(
        self,
        detections: list[Detection],
        raw_estimates: list[VolumeEstimate | None],
    ) -> float:
        """Derive a linear scale factor by comparing estimated vs reference dimensions.

        Uses the highest-confidence detection that has a known reference size.
        Compares the largest estimated dimension against the largest reference dimension.
        """
        best_ratio = None
        best_confidence = -1.0

        for det, est in zip(detections, raw_estimates):
            if est is None:
                continue

            ref = get_reference_dims(det.label)
            if ref is None:
                continue

            # Sort both estimated and reference dims descending
            est_dims = sorted(
                [est.dimensions.length_m, est.dimensions.width_m, est.dimensions.height_m],
                reverse=True,
            )
            ref_dims = sorted(ref, reverse=True)

            # Use the largest dimension for the most stable ratio
            if est_dims[0] > 0.001:
                ratio = ref_dims[0] / est_dims[0]

                if det.confidence > best_confidence:
                    best_confidence = det.confidence
                    best_ratio = ratio
                    logger.info(
                        "Scale ref: '%s' (conf=%.2f) est_max=%.3fm ref_max=%.3fm → ratio=%.3f",
                        det.label, det.confidence, est_dims[0], ref_dims[0], ratio,
                    )

        if best_ratio is not None:
            return best_ratio

        # Fallback: no known items, return 1.0 (no calibration)
        logger.warning("No known reference items found for scale calibration")
        return 1.0

    @staticmethod
    def _apply_scale(est: VolumeEstimate, scale: float) -> VolumeEstimate:
        """Apply a linear scale factor to dimensions and volume."""
        new_l = round(est.dimensions.length_m * scale, 3)
        new_w = round(est.dimensions.width_m * scale, 3)
        new_h = round(est.dimensions.height_m * scale, 3)
        new_vol = round(new_l * new_w * new_h, 4)

        return VolumeEstimate(
            label=est.label,
            volume_m3=new_vol,
            dimensions=ItemDimensions(length_m=new_l, width_m=new_w, height_m=new_h),
            confidence=est.confidence,
            image_index=est.image_index,
            category=est.category,
            feature_vector=est.feature_vector,
        )

    def _estimate_single(
        self,
        detection: Detection,
        mask: np.ndarray,
        depth_map: np.ndarray,
        intrinsics: np.ndarray,
        image_array: np.ndarray,
    ) -> VolumeEstimate | None:
        """Estimate volume for a single masked object."""
        # Extract depth values within the mask
        masked_depth = depth_map.copy()
        masked_depth[~mask] = 0

        # Get pixel coordinates where mask is True
        ys, xs = np.where(mask)
        if len(ys) < 10:
            logger.warning(
                "Too few mask pixels (%d) for '%s', skipping", len(ys), detection.label
            )
            return None

        depths = masked_depth[ys, xs]

        # Filter out zero/invalid depths
        valid = depths > 0.01
        if valid.sum() < 10:
            logger.warning("Too few valid depth pixels for '%s', skipping", detection.label)
            return None

        ys = ys[valid]
        xs = xs[valid]
        depths = depths[valid]

        # Back-project to 3D
        points = self._backproject(xs, ys, depths, intrinsics)

        # Build Open3D point cloud
        pcd = o3d.geometry.PointCloud()
        pcd.points = o3d.utility.Vector3dVector(points)

        # Remove statistical outliers
        pcd, _ = pcd.remove_statistical_outlier(nb_neighbors=20, std_ratio=2.0)

        if len(pcd.points) < 10:
            logger.warning("Too few points after outlier removal for '%s'", detection.label)
            return None

        # Compute oriented bounding box
        try:
            obb = pcd.get_oriented_bounding_box()
        except Exception:
            logger.warning("OBB computation failed for '%s', using AABB", detection.label)
            aabb = pcd.get_axis_aligned_bounding_box()
            extent = aabb.get_extent()
            dims = sorted(extent.tolist(), reverse=True)
            volume = float(np.prod(extent))
            return VolumeEstimate(
                label=detection.label,
                volume_m3=round(volume, 4),
                dimensions=ItemDimensions(
                    length_m=round(dims[0], 3),
                    width_m=round(dims[1], 3),
                    height_m=round(dims[2], 3),
                ),
                confidence=detection.confidence,
                image_index=detection.image_index,
                category=classify_item(detection.label),
            )

        extent = obb.extent
        dims = sorted(extent.tolist(), reverse=True)
        volume = float(np.prod(extent))

        # Extract a simple feature vector for deduplication:
        # mean color within mask + centroid + extent
        colors = image_array[ys, xs].astype(np.float32) / 255.0
        mean_color = colors.mean(axis=0).tolist()
        centroid = np.asarray(obb.center).tolist()
        feature_vector = mean_color + centroid + dims

        return VolumeEstimate(
            label=detection.label,
            volume_m3=round(volume, 4),
            dimensions=ItemDimensions(
                length_m=round(dims[0], 3),
                width_m=round(dims[1], 3),
                height_m=round(dims[2], 3),
            ),
            confidence=detection.confidence,
            image_index=detection.image_index,
            category=classify_item(detection.label),
            feature_vector=feature_vector,
        )

    @staticmethod
    def _backproject(
        xs: np.ndarray, ys: np.ndarray, depths: np.ndarray, intrinsics: np.ndarray
    ) -> np.ndarray:
        """Back-project 2D pixel coordinates + depth to 3D points."""
        fx, fy = intrinsics[0, 0], intrinsics[1, 1]
        cx, cy = intrinsics[0, 2], intrinsics[1, 2]

        x3d = (xs.astype(np.float64) - cx) * depths / fx
        y3d = (ys.astype(np.float64) - cy) * depths / fy
        z3d = depths.astype(np.float64)

        return np.stack([x3d, y3d, z3d], axis=-1)
