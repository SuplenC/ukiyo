#!/usr/bin/env bash
# Download the source photo for the kabukicho-gate scene and rebuild base.jpg.
#
# Source: "Kabukicho red gate and colorful neon street signs at night, Shinjuku,
# Tokyo, Japan" by Basile Morin, Wikimedia Commons, CC BY-SA 4.0.
# base.jpg and every asset derived from it inherit CC BY-SA 4.0.
#
# Requires: curl, shasum (or sha256sum), ImageMagick 7 (`magick`).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$ROOT/assets-src"
SCENE_DIR="$ROOT/scenes/kabukicho-gate"
ORIG="$SRC_DIR/kabukicho-gate-orig.jpg"

URL="https://upload.wikimedia.org/wikipedia/commons/9/92/Kabukicho_red_gate_and_colorful_neon_street_signs_at_night%2C_Shinjuku%2C_Tokyo%2C_Japan.jpg"
SHA256="59690a42be85ea1e2cad30adb2c8ae4d3ac1dd818f2cf446722af66a4af83bfe"
UA="ukiyo-asset-fetch/0.1 (https://github.com/ukiyo; open-source live wallpaper engine) curl/$(curl --version | head -1 | cut -d' ' -f2)"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

mkdir -p "$SRC_DIR" "$SCENE_DIR"

if [[ -f "$ORIG" && "$(sha256 "$ORIG")" == "$SHA256" ]]; then
  echo "source already present: $ORIG"
else
  echo "downloading $URL"
  curl -fL --retry 3 -A "$UA" -o "$ORIG.part" "$URL"
  got="$(sha256 "$ORIG.part")"
  if [[ "$got" != "$SHA256" ]]; then
    echo "sha256 mismatch: expected $SHA256, got $got" >&2
    rm -f "$ORIG.part"
    exit 1
  fi
  mv "$ORIG.part" "$ORIG"
fi

echo "building $SCENE_DIR/base.jpg"
magick "$ORIG" -crop 4950x3200+0+100 +repage -filter Lanczos -resize 3840x \
  -strip -sampling-factor 4:4:4 -quality 92 "$SCENE_DIR/base.jpg"
magick identify "$SCENE_DIR/base.jpg"
