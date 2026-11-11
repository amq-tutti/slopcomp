#!/usr/bin/env bash
# Builds "target/release/SlopComp.app" (and the comp CLI inside it) for the host architecture.
# Usage: packaging/macos/bundle.sh [--dmg]
# Unsigned: on first launch, right-click → Open (or sign/notarize for distribution, see PLAN M7).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
cargo build --release -p comp-gui -p comp-cli
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="target/release/SlopComp.app"
rm -rf "$APP" target/release/anime-comp.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/anime-comp "$APP/Contents/MacOS/anime-comp"
cp target/release/comp "$APP/Contents/MacOS/comp"
cp packaging/icon/icon.icns "$APP/Contents/Resources/icon.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>SlopComp</string>
  <key>CFBundleDisplayName</key><string>SlopComp</string>
  <key>CFBundleIdentifier</key><string>io.github.amq-tutti.slopcomp</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>anime-comp</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>SlopComp project</string>
      <key>CFBundleTypeExtensions</key><array><string>toml</string></array>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Alternate</string>
    </dict>
  </array>
</dict>
</plist>
PLIST
# Ad-hoc signature so Gatekeeper treats the bundle consistently on this machine.
codesign --force --deep -s - "$APP" >/dev/null 2>&1 || true
echo "built $APP ($(du -sh "$APP" | cut -f1))"

# Disk image with an Applications shortcut.
if [[ "${1:-}" == "--dmg" ]]; then
  ARCH=$(uname -m)
  DMG="target/release/SlopComp-${VERSION}-macos-${ARCH}.dmg"
  STAGE=$(mktemp -d)
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  cp LICENSE THIRD_PARTY_NOTICES.md "$STAGE/" 2>/dev/null || true
  rm -f "$DMG"
  hdiutil create -volname "SlopComp" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
  rm -rf "$STAGE"
  echo "built $DMG ($(du -h "$DMG" | cut -f1))"
fi
otool -L "$APP/Contents/MacOS/anime-comp" | tail -n +2
