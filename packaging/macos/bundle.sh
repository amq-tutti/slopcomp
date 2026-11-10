#!/usr/bin/env bash
# Builds target/release/anime-comp.app (and the CLI inside it) for the host architecture.
# Unsigned: on first launch, right-click → Open (or sign/notarize for distribution, see PLAN M7).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
cargo build --release -p comp-gui -p comp-cli
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="target/release/anime-comp.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/anime-comp "$APP/Contents/MacOS/anime-comp"
cp target/release/comp "$APP/Contents/MacOS/comp"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>anime-comp</string>
  <key>CFBundleDisplayName</key><string>anime-comp</string>
  <key>CFBundleIdentifier</key><string>io.github.amq-tutti.slopcomp</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>anime-comp</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>anime-comp project</string>
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
otool -L "$APP/Contents/MacOS/anime-comp" | tail -n +2
