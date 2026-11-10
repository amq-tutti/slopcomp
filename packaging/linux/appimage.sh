#!/usr/bin/env bash
# Builds a portable AppImage of the desktop app (with the CLI inside) and a CLI tarball.
# Run on Linux after deps/build.sh. Best done in an old-glibc container (manylinux_2_28) so the
# result runs on most distros; see packaging/linux/docker-build.sh.
# Env: ANIME_COMP_DEPS (deps prefix), CARGO_TARGET_DIR.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
ARCH=$(uname -m)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
export COMP_STATIC_STDCXX=1
cargo build --release -p comp-gui -p comp-cli
REL="$TARGET_DIR/release"

APPDIR="$REL/AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
cp "$REL/anime-comp" "$REL/comp" "$APPDIR/usr/bin/"
cp packaging/icon/icon_256.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/anime-comp.png"
cp packaging/icon/icon_256.png "$APPDIR/anime-comp.png"
cat > "$APPDIR/anime-comp.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Anime Comparisons
Comment=Frame-exact anime source comparisons
Exec=anime-comp %f
Icon=anime-comp
Categories=AudioVideo;Video;
MimeType=application/toml;
DESKTOP
cp "$APPDIR/anime-comp.desktop" "$APPDIR/usr/share/applications/"
cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
# `anime-comp.AppImage comp …` runs the CLI.
if [ "$1" = "comp" ]; then shift; exec "$HERE/usr/bin/comp" "$@"; fi
exec "$HERE/usr/bin/anime-comp" "$@"
APPRUN
chmod +x "$APPDIR/AppRun"
cp LICENSE THIRD_PARTY_NOTICES.md "$APPDIR/" 2>/dev/null || true

TOOL="$REL/appimagetool-$ARCH.AppImage"
if [[ ! -x "$TOOL" ]]; then
  curl -fsSL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
  chmod +x "$TOOL"
fi
OUT="$REL/Anime-Comparisons-$VERSION-linux-$ARCH.AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$ARCH "$TOOL" --no-appstream "$APPDIR" "$OUT" >/dev/null
tar -C "$REL" -czf "$REL/comp-$VERSION-linux-$ARCH.tar.gz" comp
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
echo "dynamic deps of anime-comp:"; ldd "$REL/anime-comp" | sed 's/^/  /'
echo "max glibc symbol version: $(objdump -T "$REL/anime-comp" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
