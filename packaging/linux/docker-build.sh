#!/usr/bin/env bash
# Builds the Linux AppImage inside manylinux_2_28 (glibc 2.28) for the Docker host's architecture.
# Work files go to a Docker volume, so host builds aren't disturbed; artifacts land in target/linux/.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ARCH=$(docker info --format '{{.Architecture}}')
case "$ARCH" in aarch64|arm64) IMG=quay.io/pypa/manylinux_2_28_aarch64 ;; *) IMG=quay.io/pypa/manylinux_2_28_x86_64 ;; esac
mkdir -p "$ROOT/target/linux"
docker run --rm -v "$ROOT":/src -v anime-comp-linux-work:/work -v "$ROOT/target/linux":/out -w /src "$IMG" bash -lc '
  set -euo pipefail
  dnf install -y -q autoconf automake libtool nasm ninja-build clang-devel zlib-devel file >/dev/null
  # manylinux ships its Pythons in /opt/python; meson comes from pip.
  PY=$(ls -d /opt/python/cp312-*/bin | head -1)
  "$PY/pip" install -q meson
  export PATH=$PY:$PATH
  if [ ! -x /work/cargo/bin/cargo ]; then
    curl -fsSL https://sh.rustup.rs | CARGO_HOME=/work/cargo RUSTUP_HOME=/work/rustup sh -s -- -y --profile minimal --default-toolchain 1.87.0 >/dev/null
  fi
  export CARGO_HOME=/work/cargo RUSTUP_HOME=/work/rustup PATH=/work/cargo/bin:$PATH
  PREFIX=/work/prefix BUILD=/work/build deps/build.sh > /work/deps.log 2>&1 || { tail -40 /work/deps.log; exit 1; }
  ANIME_COMP_DEPS=/work/prefix packaging/notices.sh
  ANIME_COMP_DEPS=/work/prefix CARGO_TARGET_DIR=/work/target packaging/linux/appimage.sh
  cp /work/target/release/Anime-Comparisons-*.AppImage /work/target/release/comp-*.tar.gz /out/
'
ls -la "$ROOT/target/linux"
