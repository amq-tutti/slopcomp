#!/usr/bin/env bash
# Generates the app icon: stacked screenshot cards on an indigo rounded square, the front card
# showing a scene split between a pixelated and a crisp half.
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

# The card scene: sky, sun, hills, drawn directly at the card's 4:3 size (520x390) so nothing is
# stretched, plus a pixelated copy for the left half.
magick -size 520x390 gradient:'#60a5fa'-'#dbeafe' \
  -fill '#fde68a' -draw "circle 360,120 360,162" \
  -fill '#34d399' -draw "ellipse 140,380 270,130 0,360" \
  -fill '#059669' -draw "ellipse 410,400 270,125 0,360" \
  -fill '#065f46' -draw "ellipse 260,440 330,90 0,360" "$W/scene.png"
magick "$W/scene.png" -scale 8% -scale 1250% -crop 520x390+0+0 +repage "$W/pix.png"
magick "$W/scene.png" \( "$W/pix.png" -crop 260x390+0+0 +repage \) -geometry +0+0 -composite \
  -stroke white -strokewidth 8 -draw "line 260,0 260,390" \
  \( -size 520x390 xc:none -fill "rgba(255,255,255,1)" -draw "roundrectangle 0,0 519,389 36,36" \) -compose DstIn -composite \
  "$W/card.png"

# Background, two translucent cards behind, the scene card in front.
magick -size ${S}x${S} xc:none \( -size 824x824 gradient:'#6366f1'-'#312e81' \) -geometry +100+100 -composite \
  -fill 'rgba(255,255,255,0.25)' -draw "roundrectangle 300,250 820,640 40,40" \
  -fill 'rgba(255,255,255,0.45)' -draw "roundrectangle 250,320 770,710 40,40" \
  "$W/card.png" -geometry +200+390 -composite "$W/art.png"
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
