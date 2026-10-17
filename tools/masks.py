#!/usr/bin/env python3
"""Emissive and region masks for a scene, driven by scenes/<scene>/masks.toml.

Writes (1920 px wide, base.jpg's aspect, RGBA8, linear values, no colour
profile / gAMA chunk):

  emissive.png
    R  gate bulbs: bulbs ~1.0, red arch panel between them = panel_value
    G  neon signs: every lit sign face outside the gate/sky/people, soft
    B  billboards: the big signs listed in masks.toml, intersected with "lit"
    A  per-sign id: constant pseudo-random value in (0.05, 1] per connected
       lit sign component (G or B), 0 elsewhere

  regions.png
    R  sky (soft)
    G  street / ground band
    B  people (soft)
    A  gate structure: arch + pillars + lightbox, solid

Usage: tools/.venv/bin/python tools/masks.py [scene]   (default kabukicho-gate)
"""
from __future__ import annotations

import sys

import cv2
import numpy as np
from PIL import Image

import common
from common import feather, luma, raster_shapes, smoothstep


def hsv(rgb: np.ndarray):
    """OpenCV HSV_FULL: hue 0..255 (float), sat and val 0..1."""
    u8 = np.round(rgb * 255).astype(np.uint8)
    h, s, v = cv2.split(cv2.cvtColor(u8, cv2.COLOR_RGB2HSV_FULL).astype(np.float32))
    return h, s / 255.0, v / 255.0


def disk(px: int) -> np.ndarray:
    px = max(1, int(px))
    return cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (px, px))


def drop_small(mask: np.ndarray, min_area: int, max_area: int | None = None, thr: float = 0.5) -> np.ndarray:
    m = (mask > thr).astype(np.uint8)
    n, lab, stats, _ = cv2.connectedComponentsWithStats(m, connectivity=8)
    area = stats[:, cv2.CC_STAT_AREA]
    keep = area >= min_area
    if max_area is not None:
        keep &= area <= max_area
    keep[0] = False
    return keep[lab]


def fill_small_holes(mask: np.ndarray, max_area: int) -> np.ndarray:
    """Fill background components smaller than max_area that don't touch the border."""
    inv = (mask < 0.5).astype(np.uint8)
    n, lab, stats, _ = cv2.connectedComponentsWithStats(inv, connectivity=4)
    x, y, w, h, area = (stats[:, i] for i in range(5))
    H, W = mask.shape
    border = (x == 0) | (y == 0) | (x + w == W) | (y + h == H)
    fill = (area <= max_area) & ~border
    fill[0] = False
    return (mask > 0.5) | fill[lab]


def splitmix64(x: int) -> int:
    x = (x + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
    x = ((x ^ (x >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
    x = ((x ^ (x >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
    return x ^ (x >> 31)


def sign_id_value(seed: int, index: int) -> float:
    """Stable pseudo-random value in (0.05, 1] for component `index`."""
    u = splitmix64((seed << 32) ^ index) / 2.0**64  # [0, 1)
    return 0.05 + 0.95 * (1.0 - u)  # (0.05, 1]


def gate_masks(rgb, h, s, v, cfg):
    """-> (panel 0/1, solid gate structure 0/1, bulbs soft 0..1)."""
    H, W = v.shape
    c = cfg["gate"]
    env = raster_shapes(c["envelope"], W, H) > 0.5
    red_hue = (h >= c["hue_min"]) | (h <= c["hue_max"])
    red = env & red_hue & (s >= c["sat_min"]) & (v >= c["val_min"])
    red = cv2.morphologyEx(red.astype(np.uint8), cv2.MORPH_CLOSE, disk(c["close_px"]))
    red = cv2.morphologyEx(red, cv2.MORPH_OPEN, disk(3))
    red &= (raster_shapes(c["panel_exclude"], W, H) < 0.5).astype(np.uint8)
    panel = drop_small(red, c.get("panel_min_area_px", 2000)) & env
    panel = fill_small_holes(panel, c["hole_max_area_px"])

    # bulbs: small bright whitish blobs on the panel
    y = luma(rgb)
    tophat = cv2.morphologyEx(y, cv2.MORPH_TOPHAT, disk(c["bulb_tophat_px"]))
    inner = cv2.erode(panel.astype(np.uint8), disk(3)) > 0
    cand = inner & (v >= c["bulb_val_min"]) & (s <= c["bulb_sat_max"]) & (tophat >= c["bulb_tophat_min"])
    n, lab, st, _ = cv2.connectedComponentsWithStats(cand.astype(np.uint8), connectivity=8)
    bw, bh, area = st[:, cv2.CC_STAT_WIDTH], st[:, cv2.CC_STAT_HEIGHT], st[:, cv2.CC_STAT_AREA]
    keep = (
        (area >= c["bulb_min_area_px"]) & (area <= c["bulb_max_area_px"])
        & (np.maximum(bw, bh) <= c["bulb_max_extent_px"])
        & (area >= c["bulb_min_fill"] * bw * bh)
    )
    keep[0] = False
    cand = keep[lab]
    # grow each bulb by a pixel so the glow edge is included, then soften
    bulbs = cv2.dilate(cand.astype(np.uint8), disk(3)).astype(np.float32) * panel
    bulbs = feather(bulbs, c["bulb_soft_px"])

    solid = panel.astype(np.float32)
    solid = np.maximum(solid, raster_shapes(c["solid"], W, H))
    # fill pinholes (bulb sockets on the panel edge, letters in the lightbox)
    solid = cv2.morphologyEx(solid, cv2.MORPH_CLOSE, disk(5))
    return panel.astype(np.float32), solid, bulbs


def lit_mask(rgb, s, v, cfg):
    c = cfg["signs"]
    k = (1.0 - c["sat_weight"]) + c["sat_weight"] * s
    score = np.maximum(luma(rgb), v * k)
    return smoothstep(c["lit_lo"], c["lit_hi"], score)


def main() -> None:
    scene = sys.argv[1] if len(sys.argv) > 1 else "kabukicho-gate"
    cfg = common.load_config(scene)
    rgb = common.load_base_rgb(scene)
    H, W = rgb.shape[:2]
    h, s, v = hsv(rgb)

    # ---- regions
    sky = common.sky_mask(rgb, cfg)
    panel, gate, bulbs = gate_masks(rgb, h, s, v, cfg)
    pc = cfg["people"]
    people = feather(raster_shapes(pc["shapes"], W, H), pc["feather_px"])
    st = cfg["street"]
    yy = (np.arange(H, dtype=np.float32) / (H - 1))[:, None]
    street = np.broadcast_to(smoothstep(st["y_top"], st["y_full"], yy), (H, W)).copy()
    street *= 1.0 - people
    street *= 1.0 - gate  # the pillar bases stand in the band

    # ---- emissive
    sc = cfg["signs"]
    lit = lit_mask(rgb, s, v, cfg)
    not_sign = np.maximum.reduce([
        cv2.dilate(gate, disk(5)),
        cv2.dilate((sky > 0.05).astype(np.float32), disk(3)),
        cv2.dilate((people > 0.05).astype(np.float32), disk(3)),
        raster_shapes(sc["exclude"], W, H),
    ])
    g = lit * (1.0 - not_sign)
    g *= drop_small(g, sc["min_area_px"], thr=sc["component_thr"])
    g = feather(g, sc["blur_px"])

    # billboards may be part of the gate (the lightbox): not gate-excluded
    bill_shapes = [b["shape"] for b in cfg.get("billboard", [])]
    bill_area = raster_shapes(bill_shapes, W, H)
    not_bill = np.maximum(cv2.dilate((sky > 0.05).astype(np.float32), disk(3)), people)
    b = lit * bill_area * (1.0 - not_bill)
    b *= drop_small(b, sc["min_area_px"], thr=sc["component_thr"])
    b = feather(b, sc["blur_px"])

    r = np.maximum(panel * cfg["gate"]["panel_value"], bulbs)
    r = feather(r, 0.6)

    # per-sign ids. Plain connected components merge whole blocks of the dense
    # street into one blob, so: seeds = bright sign cores (eroded), grown back
    # over the sign mask with a watershed on the colour gradient, so each sign
    # face gets its own id while touching neighbours stay separate.
    ic = cfg["sign_id"]
    thr = sc["component_thr"]
    signmask = ((g > thr) | (b > thr)).astype(np.uint8)
    core = (np.maximum(g, b) > ic["seed_thr"]).astype(np.uint8)
    core = cv2.erode(core, disk(ic["seed_erode_px"]))
    core = drop_small(core, ic["seed_min_area_px"])
    n, markers = cv2.connectedComponents(core.astype(np.uint8), connectivity=8)
    markers = markers.astype(np.int32)
    markers[signmask == 0] = n  # background marker
    bgr8 = cv2.cvtColor(np.round(rgb * 255).astype(np.uint8), cv2.COLOR_RGB2BGR)
    ws = cv2.watershed(bgr8, markers)  # -1 on ridges
    ws[(ws == n) | (signmask == 0)] = 0
    ws[ws < 0] = 0
    lut = np.zeros(n + 1, np.float32)
    for i in range(1, n):
        lut[i] = sign_id_value(ic["seed"], i)
    a = lut[ws]
    # ridge pixels and lit bits no seed reached: take the neighbouring id
    a = np.where(a > 0, a, cv2.dilate(a, disk(2 * ic["dilate_px"] + 1)))
    # each billboard flickers as one sign
    for k, bs in enumerate(bill_shapes):
        m = (raster_shapes([bs], W, H) > 0.5) & (a > 0)
        a[m] = sign_id_value(ic["seed"], 1_000_000 + k)
    a *= ((g > 0.02) | (b > 0.02))
    print(f"sign ids: {n - 1}")

    def save(path, chans):
        arr = np.stack([np.round(np.clip(c, 0, 1) * 255).astype(np.uint8) for c in chans], -1)
        Image.fromarray(arr, "RGBA").save(path, optimize=True)
        print(f"wrote {path}")

    d = common.scene_dir(scene)
    save(d / "emissive.png", [r, g, b, a])
    save(d / "regions.png", [sky, street, people, gate])


if __name__ == "__main__":
    main()
