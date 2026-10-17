"""Shared helpers for the offline asset tools (depth.py, masks.py).

All shape coordinates in masks.toml are normalised image coordinates:
x, y in [0, 1], origin top-left. A shape is either a rect [x0, y0, x1, y1]
or a polygon [[x, y], [x, y], ...].
"""
from __future__ import annotations

import tomllib
from pathlib import Path

import cv2
import numpy as np

ROOT = Path(__file__).resolve().parent.parent
OUT_WIDTH = 1920  # width of every derived asset; height keeps base.jpg's aspect


def scene_dir(scene: str) -> Path:
    return ROOT / "scenes" / scene


def load_config(scene: str) -> dict:
    with open(scene_dir(scene) / "masks.toml", "rb") as f:
        return tomllib.load(f)


def load_base_rgb(scene: str, width: int | None = OUT_WIDTH) -> np.ndarray:
    """base.jpg as float32 RGB in [0, 1] (sRGB-encoded values), resized to `width`."""
    bgr = cv2.imread(str(scene_dir(scene) / "base.jpg"), cv2.IMREAD_COLOR)
    if bgr is None:
        raise FileNotFoundError("base.jpg missing; run tools/fetch-source.sh first")
    if width is not None and width != bgr.shape[1]:
        h = round(width * bgr.shape[0] / bgr.shape[1])
        bgr = cv2.resize(bgr, (width, h), interpolation=cv2.INTER_AREA)
    return cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB).astype(np.float32) / 255.0


def out_size(scene: str) -> tuple[int, int]:
    """(width, height) of the derived assets: OUT_WIDTH wide, base.jpg's aspect."""
    from PIL import Image

    with Image.open(scene_dir(scene) / "base.jpg") as im:
        w, h = im.size
    return OUT_WIDTH, round(OUT_WIDTH * h / w)


def raster_shapes(shapes, w: int, h: int) -> np.ndarray:
    """Rasterise a list of rects/polygons (normalised coords) into a float32 0/1 mask."""
    m = np.zeros((h, w), np.uint8)
    for s in shapes or []:
        if len(s) == 4 and not isinstance(s[0], (list, tuple)):
            x0, y0, x1, y1 = s
            pts = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
        else:
            pts = s
        p = np.array([[x * w, y * h] for x, y in pts], np.float32)
        cv2.fillPoly(m, [np.round(p * 16).astype(np.int32)], 1, lineType=cv2.LINE_8, shift=4)
    return m.astype(np.float32)


def smoothstep(e0: float, e1: float, x: np.ndarray) -> np.ndarray:
    t = np.clip((x - e0) / (e1 - e0), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


def feather(mask: np.ndarray, px: float) -> np.ndarray:
    if px <= 0:
        return mask
    return cv2.GaussianBlur(mask, (0, 0), px)


def luma(rgb: np.ndarray) -> np.ndarray:
    return rgb @ np.array([0.2126, 0.7152, 0.0722], np.float32)


def sky_mask(rgb: np.ndarray, cfg: dict) -> np.ndarray:
    """Soft sky mask: inside the sky polygons AND dark/unsaturated-ish."""
    h, w = rgb.shape[:2]
    c = cfg["sky"]
    poly = raster_shapes(c["polygons"], w, h)
    y = luma(rgb)
    mx = rgb.max(axis=2)
    dark = 1.0 - smoothstep(c["luma_lo"], c["luma_hi"], np.maximum(y, mx * 0.6))
    m = (poly * dark > 0.5).astype(np.uint8)
    # drop speckles (single dark pixels inside lit stuff), close small gaps (wires)
    m = cv2.morphologyEx(m, cv2.MORPH_OPEN, np.ones((3, 3), np.uint8))
    m = cv2.morphologyEx(m, cv2.MORPH_CLOSE, np.ones((5, 5), np.uint8))
    n, lab, stats, _ = cv2.connectedComponentsWithStats(m, connectivity=8)
    keep = np.zeros(n, bool)
    keep[1:] = stats[1:, cv2.CC_STAT_AREA] >= c.get("min_area_px", 400)
    m = keep[lab].astype(np.float32)
    return feather(m, c["feather_px"]) * poly
