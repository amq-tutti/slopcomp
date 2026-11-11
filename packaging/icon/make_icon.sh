#!/usr/bin/env bash
# Generates the app icon: the in-app logo (two overlapping rounded squares, like two sources side
# by side: a violet one behind, a pink one in front with a gap cut around it) on a dark slate
# rounded square.
#
# Drawn on Apple's icon grid: the rounded square is 824 px inside a 1024 px canvas (100 px
# margin, ~185 px corner radius) with a soft shadow, so it sits at the same size as other icons.
#
# Outputs icon.png (1024), icon_256.png (window icon), icon.icns (macOS), icon.ico (Windows).
# Needs ImageMagick 7 (`magick`); iconutil on macOS for .icns.
set -euo pipefail
cd "$(dirname "$0")"
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
S=1024

# Rounded-square mask and shadow on the 824 px grid.
magick -size ${S}x${S} xc:none -fill "rgba(255,255,255,1)" -draw "roundrectangle 100,100 924,924 185,185" "$W/mask.png"
magick -size ${S}x${S} xc:none -fill 'rgba(0,0,0,0.35)' -draw "roundrectangle 100,112 924,936 185,185" -blur 0x14 "$W/shadow.png"

# The logo, scaled from theme::logo (15-unit squares, 9 apart, radius 4, gap 1.5) at 21.6 px per unit:
# 324 px squares, 194 px apart, 86 px radius, 32 px gap; centred on the canvas.
# FRONT is theme::ACCENT_2 at 85% over the bar, as the in-app logo draws it.
BACK='#8e7bff'; FRONT='#c7619c'; BG_TOP='#1f232b'; BG_BOTTOM='#0e0f13'
magick -size ${S}x${S} xc:none \( -size 824x824 gradient:"$BG_TOP"-"$BG_BOTTOM" \) -geometry +100+100 -composite \
  -fill "$BACK" -draw "roundrectangle 253,350 577,674 86,86" \
  "$W/base.png"
# Re-draw the background gradient inside the gap so it blends with the backdrop, then the front square.
magick -size ${S}x${S} xc:none -fill white -draw "roundrectangle 415,318 803,706 118,118" "$W/gapmask.png"
magick -size ${S}x${S} xc:none \( -size 824x824 gradient:"$BG_TOP"-"$BG_BOTTOM" \) -geometry +100+100 -composite \
  "$W/gapmask.png" -compose DstIn -composite "$W/gap.png"
magick "$W/base.png" "$W/gap.png" -compose Over -composite \
  -fill "$FRONT" -draw "roundrectangle 447,350 771,674 86,86" \
  -fill none -stroke 'rgba(255,255,255,0.10)' -strokewidth 6 -draw "roundrectangle 103,103 921,921 182,182" \
  "$W/art.png"
magick "$W/art.png" "$W/mask.png" -compose DstIn -composite -type TrueColorAlpha PNG32:"$W/masked.png"
magick "$W/shadow.png" -colorspace sRGB -type TrueColorAlpha "$W/masked.png" -compose Over -composite \
  -type TrueColorAlpha -depth 8 PNG32:icon.png

# 256 px copy embedded in the binary as the window/taskbar icon.
magick icon.png -resize 256x256 -depth 8 PNG32:icon_256.png
# Windows .ico (multi-size)
magick icon.png -define icon:auto-resize=256,128,64,48,32,16 icon.ico
# macOS .icns
if command -v iconutil >/dev/null; then
  rm -rf icon.iconset && mkdir icon.iconset
  for s in 16 32 128 256 512; do
    magick icon.png -resize ${s}x${s} PNG32:icon.iconset/icon_${s}x${s}.png
    magick icon.png -resize $((s*2))x$((s*2)) PNG32:icon.iconset/icon_${s}x${s}@2x.png
  done
  iconutil -c icns icon.iconset -o icon.icns && rm -rf icon.iconset
fi
ls -la icon.*
