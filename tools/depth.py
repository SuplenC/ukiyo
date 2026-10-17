#!/usr/bin/env python3
"""Monocular depth for a scene's base.jpg -> depth.png.

Output: 16-bit grayscale PNG, 1920 px wide, base.jpg's aspect,
1.0 = nearest, 0.0 = farthest.

Model: Depth Anything V2 *Small* (depth-anything/Depth-Anything-V2-Small-hf).
Only Small is used on purpose: its weights are Apache-2.0, while the Base and
Large checkpoints are CC BY-NC 4.0, which is incompatible with shipping the
derived depth map in an open-source project that anyone may use commercially.

Pipeline:
  1. Global pass on the whole image at ~1036 px long side: the most coherent
     layering (higher input sizes flatten the scene; tested 518/1036/1540/2058).
  2. Detail pass: 3x3 overlapping tiles (each half the image in both axes,
     so ~2x the effective resolution), each tile affine-fitted to the global
     pass and blended with a sine window. Tiles alone lose global context
     (e.g. the sky above the arch reads as near), so only their detail is kept:
  3. Merge: low frequencies of the global pass + high frequencies of the
     tiled pass. This resolves individual signs, lamps and the pillar bases.
  4. Upsample to 1920 px and edge-snap with a guided filter against the RGB.
  5. Robust normalisation: 1st/99th percentiles -> 0/1.
  6. Documented corrections from masks.toml: [[depth.lift]] raises a shape to
     at least the median depth of a reference rect (the model puts the gate's
     grey pillar bases well behind the red pillars they stand under), and the
     sky ([sky] mask) is pinned to the far plane.

Usage: tools/.venv/bin/python tools/depth.py [scene]   (default kabukicho-gate)
"""
from __future__ import annotations

import sys

import cv2
import numpy as np
import torch
from transformers import AutoModelForDepthEstimation

import common

MODEL_ID = "depth-anything/Depth-Anything-V2-Small-hf"
MODEL_REVISION = "5426e4f0f36572d16453bbda7a8389317b1bef99"  # pinned HF commit
PATCH = 14
GLOBAL_LONG_SIDE = 1036
TILE_LONG_SIDE = 1036
TILE_GRID = 3                 # TILE_GRID x TILE_GRID tiles, each half the image
MERGE_SIGMA_FRAC = 0.012      # low/high split, as a fraction of output width
GUIDE_RADIUS = 4              # guided filter radius (px at 1920)
GUIDE_EPS = 1e-3              # guided filter regularisation (depth in 0..1)
IMAGENET_MEAN = np.array([0.485, 0.456, 0.406], np.float32)
IMAGENET_STD = np.array([0.229, 0.224, 0.225], np.float32)


def device() -> str:
    return "mps" if torch.backends.mps.is_available() else "cpu"


def run_model(model, rgb: np.ndarray, long_side: int, dev: str) -> np.ndarray:
    """Relative inverse depth (bigger = nearer) at the model's input size."""
    h, w = rgb.shape[:2]
    s = long_side / max(h, w)
    iw = max(PATCH, round(w * s / PATCH) * PATCH)
    ih = max(PATCH, round(h * s / PATCH) * PATCH)
    x = cv2.resize(rgb, (iw, ih), interpolation=cv2.INTER_AREA)
    x = (x - IMAGENET_MEAN) / IMAGENET_STD
    t = torch.from_numpy(x.transpose(2, 0, 1).copy())[None].to(dev)
    with torch.no_grad():
        d = model(pixel_values=t).predicted_depth[0]
    return d.float().cpu().numpy()


def affine_fit(src: np.ndarray, dst: np.ndarray) -> np.ndarray:
    """Least-squares a*src+b ~= dst (relative depth has scale/shift ambiguity)."""
    A = np.stack([src.ravel(), np.ones(src.size, np.float32)], 1)
    a, b = np.linalg.lstsq(A, dst.ravel(), rcond=None)[0]
    return (a * src + b).astype(np.float32)


def run_tiled(model, full: np.ndarray, glob: np.ndarray, dev: str) -> np.ndarray:
    """Overlapping-tile inference, each tile fitted to `glob` (output-size)."""
    H, W = glob.shape
    FH, FW = full.shape[:2]
    th, tw = FH // 2, FW // 2
    acc = np.zeros((H, W), np.float32)
    wsum = np.zeros((H, W), np.float32)
    n = TILE_GRID
    for j in range(n):
        for i in range(n):
            x0 = round(i * (FW - tw) / (n - 1))
            y0 = round(j * (FH - th) / (n - 1))
            d = run_model(model, full[y0 : y0 + th, x0 : x0 + tw], TILE_LONG_SIDE, dev)
            ox0, oy0 = round(x0 * W / FW), round(y0 * H / FH)
            ox1, oy1 = min(W, round((x0 + tw) * W / FW)), min(H, round((y0 + th) * H / FH))
            if j == n - 1:
                oy1 = H
            if i == n - 1:
                ox1 = W
            ow, oh = ox1 - ox0, oy1 - oy0
            d = cv2.resize(d, (ow, oh), interpolation=cv2.INTER_CUBIC)
            d = affine_fit(d, glob[oy0:oy1, ox0:ox1])
            wy = np.sin(np.linspace(0, np.pi, oh + 2)[1:-1])[:, None]
            wx = np.sin(np.linspace(0, np.pi, ow + 2)[1:-1])[None]
            w = (wy * wx).astype(np.float32)
            acc[oy0:oy1, ox0:ox1] += d * w
            wsum[oy0:oy1, ox0:ox1] += w
    return acc / np.maximum(wsum, 1e-6)


def main() -> None:
    scene = sys.argv[1] if len(sys.argv) > 1 else "kabukicho-gate"
    cfg = common.load_config(scene)
    W, H = common.out_size(scene)
    full = common.load_base_rgb(scene, width=None)
    rgb = common.load_base_rgb(scene)  # 1920 px

    dev = device()
    print(f"loading {MODEL_ID} on {dev}")
    model = AutoModelForDepthEstimation.from_pretrained(MODEL_ID, revision=MODEL_REVISION).to(dev).eval()

    glob = cv2.resize(run_model(model, full, GLOBAL_LONG_SIDE, dev), (W, H), interpolation=cv2.INTER_CUBIC)
    tiled = run_tiled(model, full, glob, dev)
    sigma = MERGE_SIGMA_FRAC * W
    low = lambda d: cv2.GaussianBlur(d, (0, 0), sigma)
    d = low(glob) + (tiled - low(tiled))

    lo, hi = np.percentile(d, [1, 99])
    dn = np.clip((d - lo) / (hi - lo), 0, 1).astype(np.float32)
    dn = cv2.ximgproc.guidedFilter(rgb, dn, GUIDE_RADIUS, GUIDE_EPS)

    lo, hi = np.percentile(dn, [1, 99])
    dn = np.clip((dn - lo) / (hi - lo), 0, 1)

    dcfg = cfg.get("depth", {})
    for lift in dcfg.get("lift", []):
        ref = common.raster_shapes([lift["ref"]], W, H) > 0.5
        target = float(np.median(dn[ref]))
        m = common.feather(common.raster_shapes([lift["shape"]], W, H), lift.get("feather_px", 2.0))
        dn = dn * (1 - m) + np.maximum(dn, target) * m
        print(f"lift {lift['name']}: >= {target:.3f}")

    sky = common.sky_mask(rgb, cfg)
    sky_v = float(dcfg.get("sky_value", 0.0))
    dn = np.clip(dn * (1 - sky) + sky_v * sky, 0, 1)

    out = common.scene_dir(scene) / "depth.png"
    cv2.imwrite(str(out), np.round(dn * 65535).astype(np.uint16))
    print(f"wrote {out} {W}x{H} 16-bit")


if __name__ == "__main__":
    main()
