# Asset tools

Offline tooling that builds the image assets for a scene. None of it runs at
wallpaper runtime; the outputs are committed under `scenes/<scene>/`.

## Reproduce `scenes/kabukicho-gate/` from scratch

Needs `curl`, ImageMagick 7 (`magick`) and [uv](https://docs.astral.sh/uv/).

```sh
# 1. source photo -> assets-src/ (gitignored), sha256-checked, then base.jpg
tools/fetch-source.sh

# 2. Python env (Python 3.12, pinned deps)
uv venv --python 3.12 tools/.venv
uv pip install --python tools/.venv/bin/python -r tools/requirements.txt

# 3. depth.png (downloads the model into the Hugging Face cache on first run)
tools/.venv/bin/python tools/depth.py kabukicho-gate

# 4. emissive.png + regions.png
tools/.venv/bin/python tools/masks.py kabukicho-gate
```

`fetch-source.sh` rebuilds `base.jpg` byte-identically (checked with
ImageMagick 7.1.2). `depth.py` uses Apple MPS when available, otherwise CPU;
expect tiny numeric differences between devices.

## Outputs

All maps are 1920 px wide with `base.jpg`'s aspect, PNG, no colour profile.
Values are linear data, not colours.

| file | format | channels |
|---|---|---|
| `depth.png` | 16-bit gray | 1.0 = nearest, 0.0 = farthest (sky pinned to 0) |
| `emissive.png` | RGBA8 | R gate bulbs (bulbs 1.0, arch panel 0.35) · G neon signs · B billboards (music-reactive subset) · A per-sign id in (0.05, 1], 0 = no sign |
| `regions.png` | RGBA8 | R sky · G street band · B people · A gate structure (solid) |

Everything hand-tuned lives in `scenes/<scene>/masks.toml`: shapes in
normalised [0, 1] image coordinates (resolution independent), thresholds and
the small documented depth corrections. Edit it and rerun the scripts.

## Model and licence

Depth comes from **Depth Anything V2 Small**
(`depth-anything/Depth-Anything-V2-Small-hf`, pinned to a commit in
`depth.py`). Small is the only size whose weights are **Apache-2.0**; Base and
Large are **CC BY-NC 4.0**, and a non-commercial clause can't sit inside an
open-source project's committed assets. Don't swap in a larger checkpoint.

The photo is "Kabukicho red gate and colorful neon street signs at night,
Shinjuku, Tokyo, Japan" by Basile Morin,
[Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Kabukicho_red_gate_and_colorful_neon_street_signs_at_night,_Shinjuku,_Tokyo,_Japan.jpg),
licensed **CC BY-SA 4.0**. `base.jpg` and every map derived from it
(`depth.png`, `emissive.png`, `regions.png`) are adaptations and are
distributed under CC BY-SA 4.0 with the same attribution.
