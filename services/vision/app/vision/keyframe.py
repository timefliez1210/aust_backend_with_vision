from __future__ import annotations

import logging
import os
import tempfile
from dataclasses import dataclass, field

import cv2
import numpy as np
from PIL import Image

from app.config import settings

logger = logging.getLogger(__name__)


@dataclass
class KeyframeResult:
    """Result of keyframe extraction from a video."""

    frames: list[Image.Image]
    frame_indices: list[int]
    fps: float
    total_frames: int
    width: int
    height: int
    pause_keyframe_indices: list[int] = field(default_factory=list)  # indices into frames[] that are pause reps


def extract_keyframes(
    video_bytes: bytes,
    max_frames: int = 60,
    min_frames: int = 30,
    blur_threshold: float = 50.0,
    min_interval_sec: float = 0.3,
) -> KeyframeResult:
    """Extract keyframes from a video based on scene changes.

    Selects frames with the largest inter-frame differences, spaced at least
    min_interval_sec apart, rejecting blurry frames. Always includes first
    and last frames.

    Args:
        video_bytes: Raw video file bytes (mp4, mov, webm, mkv).
        max_frames: Maximum number of keyframes to extract.
        min_frames: Minimum number of keyframes; fills with uniform sampling
            if filtering leaves fewer.
        blur_threshold: Laplacian variance threshold; frames below are blurry.
        min_interval_sec: Minimum time between selected keyframes.

    Returns:
        KeyframeResult with extracted frames and metadata.
    """
    with tempfile.NamedTemporaryFile(suffix=".mp4", delete=False) as tmp:
        tmp.write(video_bytes)
        tmp_path = tmp.name

    try:
        return _extract_from_file(
            tmp_path, max_frames, min_frames, blur_threshold, min_interval_sec
        )
    finally:
        os.unlink(tmp_path)


def _extract_from_file(
    video_path: str,
    max_frames: int,
    min_frames: int,
    blur_threshold: float,
    min_interval_sec: float,
) -> KeyframeResult:
    """Internal: extract keyframes from a video file on disk."""
    cap = cv2.VideoCapture(video_path)
    if not cap.isOpened():
        raise ValueError("Failed to open video file")

    fps = cap.get(cv2.CAP_PROP_FPS) or 30.0
    total_frames = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
    width = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
    height = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
    min_interval_frames = max(1, int(min_interval_sec * fps))

    # Detect side-by-side stereo / dual-camera video.
    # Only crop if aspect ratio is very wide AND left/right halves look similar.
    sbs_crop = False
    if width / height > 2.0:
        # Read first frame to check if left and right halves are visually similar
        ret, first_frame = cap.read()
        if ret:
            gray = cv2.cvtColor(first_frame, cv2.COLOR_BGR2GRAY)
            mid = gray.shape[1] // 2
            left_half = gray[:, :mid]
            right_half = gray[:, mid:]
            # Compare histograms — SBS stereo halves will be very similar
            hist_l = cv2.calcHist([left_half], [0], None, [64], [0, 256])
            hist_r = cv2.calcHist([right_half], [0], None, [64], [0, 256])
            cv2.normalize(hist_l, hist_l)
            cv2.normalize(hist_r, hist_r)
            corr = cv2.compareHist(hist_l, hist_r, cv2.HISTCMP_CORREL)
            # Also check structural similarity via L1 difference
            # Resize both halves to same size for comparison
            h = min(left_half.shape[0], right_half.shape[0])
            w = min(left_half.shape[1], right_half.shape[1])
            l1_diff = np.mean(np.abs(
                left_half[:h, :w].astype(np.float32) - right_half[:h, :w].astype(np.float32)
            ))
            sbs_crop = corr > 0.95 and l1_diff < 15.0
            logger.info(
                "SBS check: aspect=%.2f, hist_corr=%.3f, l1_diff=%.1f -> %s",
                width / height, corr, l1_diff, "SBS" if sbs_crop else "ultra-wide",
            )
        # Reset to beginning for the actual frame reading
        cap.set(cv2.CAP_PROP_POS_FRAMES, 0)

    if sbs_crop:
        crop_width = width // 2
        logger.info(
            "Video: %dx%d (SBS stereo, cropping to %dx%d), %.1f fps, %d frames (%.1fs)",
            width, height, crop_width, height, fps, total_frames, total_frames / fps,
        )
        width = crop_width
    else:
        logger.info(
            "Video: %dx%d, %.1f fps, %d frames (%.1fs)",
            width, height, fps, total_frames, total_frames / fps,
        )

    # First pass: read all frames as grayscale, compute diffs + blur scores
    gray_frames: list[np.ndarray] = []
    while True:
        ret, frame = cap.read()
        if not ret:
            break
        if sbs_crop:
            frame = frame[:, :frame.shape[1] // 2]
        gray_frames.append(cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY))
    cap.release()

    n = len(gray_frames)
    if n < 2:
        raise ValueError(f"Video too short: only {n} frames")

    # Inter-frame L1 difference
    diffs = np.zeros(n)
    for i in range(1, n):
        diffs[i] = np.mean(np.abs(
            gray_frames[i].astype(np.float32) - gray_frames[i - 1].astype(np.float32)
        ))

    # Blur score (Laplacian variance)
    blur_scores = np.array([
        cv2.Laplacian(g, cv2.CV_64F).var() for g in gray_frames
    ])
    del gray_frames  # free memory

    # Detect pause clusters (consecutive stable + sharp frames)
    cluster_index_lists = _detect_pauses(diffs, blur_scores, fps, blur_threshold)

    # Seed selection with 1 representative per pause cluster (sharpest frame).
    # This guarantees every pause region is represented in the dense keyframe set.
    selected: list[int] = []
    pause_video_indices: list[int] = []
    for core_indices in cluster_index_lists:
        best = max(core_indices, key=lambda i: blur_scores[i])
        if blur_scores[best] >= blur_threshold:
            selected.append(best)
            pause_video_indices.append(best)

    if pause_video_indices:
        logger.info(
            "Seeded %d pause representatives into keyframe selection",
            len(pause_video_indices),
        )

    # Candidates: non-blurry frames
    candidates = set(i for i in range(n) if blur_scores[i] >= blur_threshold)
    # Always include first and last
    candidates.add(0)
    candidates.add(n - 1)

    # Sort candidates by scene change score (descending)
    candidates_scored = sorted(candidates, key=lambda i: diffs[i], reverse=True)

    # Greedily add frames with minimum spacing (pause reps are already in selected)
    for idx in candidates_scored:
        if len(selected) >= max_frames:
            break
        if all(abs(idx - s) >= min_interval_frames for s in selected):
            selected.append(idx)

    # If too few, fill with uniform sampling
    if len(selected) < min_frames:
        uniform = np.linspace(0, n - 1, min_frames, dtype=int).tolist()
        for idx in uniform:
            if idx not in selected and len(selected) < max_frames:
                selected.append(idx)

    selected.sort()
    blur_rejected = sum(1 for i in range(n) if blur_scores[i] < blur_threshold)
    logger.info(
        "Selected %d keyframes from %d total (blur rejected: %d)",
        len(selected), n, blur_rejected,
    )

    # Map pause video indices to keyframe positions
    pause_kf_indices: list[int] = []
    for vi in pause_video_indices:
        try:
            pause_kf_indices.append(selected.index(vi))
        except ValueError:
            pass  # Pause rep got displaced by spacing constraint

    # Second pass: extract RGB frames for selected indices only
    cap = cv2.VideoCapture(video_path)
    selected_set = set(selected)
    frames: list[Image.Image] = []
    frame_idx = 0
    while True:
        ret, frame = cap.read()
        if not ret:
            break
        if frame_idx in selected_set:
            if sbs_crop:
                frame = frame[:, :frame.shape[1] // 2]
            rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            frames.append(Image.fromarray(rgb))
        frame_idx += 1
    cap.release()

    return KeyframeResult(
        frames=frames,
        frame_indices=selected,
        fps=fps,
        total_frames=n,
        width=width,
        height=height,
        pause_keyframe_indices=pause_kf_indices,
    )


def _detect_pauses(
    diffs: np.ndarray,
    blur_scores: np.ndarray,
    fps: float,
    blur_threshold: float,
    min_duration_sec: float | None = None,
    max_cluster_frames: int | None = None,
) -> list[list[int]]:
    """Find pause segments: consecutive frames with low motion AND high sharpness.

    Returns a list of index lists, one per detected pause cluster.
    """
    if min_duration_sec is None:
        min_duration_sec = settings.pause_min_duration_sec
    if max_cluster_frames is None:
        max_cluster_frames = settings.pause_max_cluster_frames

    n = len(diffs)
    if n < 2:
        return []

    # Adaptive threshold: frames below 30th percentile of motion are "stable"
    stability_threshold = float(np.percentile(diffs[1:], 30))  # skip frame 0 (diff=0)

    # Mark stable + sharp frames
    is_stable = np.zeros(n, dtype=bool)
    for i in range(n):
        is_stable[i] = (diffs[i] <= stability_threshold) and (blur_scores[i] >= blur_threshold)

    # Find consecutive runs of stable frames
    min_frames = max(1, int(min_duration_sec * fps))
    clusters: list[tuple[int, int]] = []
    start: int | None = None
    for i in range(n):
        if is_stable[i]:
            if start is None:
                start = i
        else:
            if start is not None and (i - start) >= min_frames:
                clusters.append((start, i))
            start = None
    if start is not None and (n - start) >= min_frames:
        clusters.append((start, n))

    # For each cluster, select up to max_cluster_frames (evenly spaced)
    result: list[list[int]] = []
    for seg_start, seg_end in clusters:
        seg_len = seg_end - seg_start
        if seg_len <= max_cluster_frames:
            indices = list(range(seg_start, seg_end))
        else:
            indices = np.linspace(seg_start, seg_end - 1, max_cluster_frames, dtype=int).tolist()
        result.append(indices)

    logger.info(
        "Pause detection: %d stable segments found (threshold=%.1f, min_frames=%d)",
        len(result), stability_threshold, min_frames,
    )
    return result
