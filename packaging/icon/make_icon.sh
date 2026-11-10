#!/usr/bin/env bash
# Generates the app icon: a rounded square split diagonally into two halves (source A / source B)
# with a vertical comparison divider. Outputs icon.png (1024), icon.icns (macOS) and icon.ico (Windows).
# Needs ImageMagick 7 (`magick`); iconutil on macOS for .icns.
set -euo pipefail
cd "$(dirname "$0")"
S=1024
magick -size ${S}x${S} xc:none \
  \( -size ${S}x${S} xc:none -fill '#1f2937' -draw "roundrectangle 40,40 984,984 200,200" \) -composite \
  \( -size ${S}x${S} xc:none -fill '#f472b6' -draw "polygon 40,40 600,40 424,984 40,984" \
     \( -size ${S}x${S} xc:none -fill white -draw "roundrectangle 40,40 984,984 200,200" \) -compose DstIn -composite \) -compose Over -composite \
  \( -size ${S}x${S} xc:none -fill '#60a5fa' -draw "polygon 600,40 984,40 984,984 424,984" \
     \( -size ${S}x${S} xc:none -fill white -draw "roundrectangle 40,40 984,984 200,200" \) -compose DstIn -composite \) -compose Over -composite \
  -stroke white -strokewidth 34 -draw "line 600,40 424,984" \
  -fill white -stroke none -draw "circle 512,512 512,600" \
  -fill '#1f2937' -draw "polygon 470,470 470,554 552,512" \
  -depth 8 PNG32:icon.png
# 256 px copy embedded in the binary as the window/taskbar icon.
magick icon.png -resize 256x256 -depth 8 PNG32:icon_256.png
# Windows .ico (multi-size)
magick icon.png -define icon:auto-resize=256,128,64,48,32,16 icon.ico
# macOS .icns
if command -v iconutil >/dev/null; then
  rm -rf icon.iconset && mkdir icon.iconset
  for s in 16 32 128 256 512; do
    magick icon.png -resize ${s}x${s} icon.iconset/icon_${s}x${s}.png
    magick icon.png -resize $((s*2))x$((s*2)) icon.iconset/icon_${s}x${s}@2x.png
  done
  iconutil -c icns icon.iconset -o icon.icns && rm -rf icon.iconset
fi
ls -la icon.*
