# Credits and asset licences

The code is MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`). Scene assets carry their own licences, listed here and in each `scene.toml`.

## scenes/kabukicho-gate

| File | What | Source | Licence |
|---|---|---|---|
| `base.jpg` | The photo, cropped (rows 100 to 3300 of 4411) and downscaled to 3840 px | Basile Morin, ["Kabukicho red gate and colorful neon street signs at night, Shinjuku, Tokyo, Japan"](https://commons.wikimedia.org/wiki/File:Kabukicho_red_gate_and_colorful_neon_street_signs_at_night,_Shinjuku,_Tokyo,_Japan.jpg), Wikimedia Commons, 2019 | [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) |
| `depth.png` | Depth, Depth Anything V2 Small (`tools/depth.py`) | derived from the photo | CC BY-SA 4.0 |
| `emissive.png`, `regions.png` | Masks (`tools/masks.py` + `masks.toml`) | derived from the photo | CC BY-SA 4.0 |
| `poster.jpg` | A frame of the scene, rendered by `ukiyo-preview` | derived from the photo | CC BY-SA 4.0 |
| `scene.toml`, `scene.wgsl` | The scene itself | ukiyo | MIT OR Apache-2.0 |

`tools/fetch-source.sh` downloads the original and checks its sha256, so every derived file can be rebuilt from it.

**Why this photo:** it's the whole gate, sharp, with a deep street behind it, which is what parallax and depth-tested rain need. And it's CC BY-SA, which allows redistribution, including in a wallpaper engine. The usual stock sites don't: the Unsplash License forbids "compiling photos from Unsplash to replicate a similar or competing service", and the Pexels License forbids redistributing content "on other stock photo or wallpaper platforms". Those are fine for your own desktop, not for a scene bundled in a public repo.

The Depth Anything V2 Small model is Apache-2.0. The Base and Large models are CC BY-NC 4.0, so they're not used.

Fonts in `ukiyo-preview`'s recording overlay come from egui (`epaint_default_fonts`: Ubuntu Light, Hack), under their own licences (Ubuntu Font Licence, MIT).
