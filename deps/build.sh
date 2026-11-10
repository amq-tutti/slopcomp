#!/usr/bin/env bash
# Builds the static native dependencies of anime-comp into deps/prefix.
#
#   dav1d  -> AV1 decoding for ffmpeg
#   ffmpeg -> trimmed GPL build: decoders/demuxers/filters only, no encoders, no network
#   zimg   -> colour conversion / resize
#   ffms2  -> frame-accurate indexing + seeking on top of ffmpeg
#
# Supported hosts: macOS (arm64/x86_64), Linux (x86_64), Windows via MSYS2 UCRT64 (mingw-w64).
# Usage: deps/build.sh [--clean]
# Env:   JOBS (parallelism), PREFIX (install dir, default deps/prefix), BUILD (scratch dir, default deps/build)
set -euo pipefail

DAV1D_VER=1.5.1
FFMPEG_VER=7.1.1
ZIMG_VER=3.0.5
FFMS2_VER=5.0
DAV1D_SHA256=fa635e2bdb25147b1384007c83e15de44c589582bb3b9a53fc1579cb9d74b695
FFMPEG_SHA256=733984395e0dbbe5c046abda2dc49a5544e7e0e1e2366bba849222ae9e3a03b1
ZIMG_SHA256=a9a0226bf85e0d83c41a8ebe4e3e690e1348682f6a2a7838f1b8cbff1b799bcf
FFMS2_SHA256=7770af0bbc0063f9580a6a5c8e7c51f1788f171d7da0b352e48a1e60943a8c3c

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$ROOT/src"
BUILD="${BUILD:-$ROOT/build}"
PREFIX="${PREFIX:-$ROOT/prefix}"
JOBS="${JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"

if [[ "${1:-}" == "--clean" ]]; then rm -rf "$BUILD" "$PREFIX"; fi
mkdir -p "$SRC" "$BUILD" "$PREFIX"

case "$(uname -s)" in
  Darwin) HOST=macos ;;
  Linux) HOST=linux ;;
  MINGW*|MSYS*) HOST=windows ;;
  *) echo "unsupported host $(uname -s)" >&2; exit 1 ;;
esac

export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"
COMMON_CFLAGS="-O2 -fPIC"
if [[ $HOST == macos ]]; then
  export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
  COMMON_CFLAGS="$COMMON_CFLAGS -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET"
  # Homebrew's libtool is installed as glibtoolize
  command -v glibtoolize >/dev/null && export LIBTOOLIZE=glibtoolize
fi
if [[ $HOST == windows ]]; then COMMON_CFLAGS="-O2"; fi
COMMON_CXXFLAGS="$COMMON_CFLAGS"
if [[ $HOST == macos ]]; then
  # Some Command Line Tools installs ship a partial usr/include/c++/v1 that shadows the SDK's
  # libc++ headers. If a trivial C++ program fails to compile, point at the SDK headers explicitly.
  probe="$BUILD/cxxprobe.cpp"; printf '#include <algorithm>\nint main(){}\n' > "$probe"
  if ! c++ -fsyntax-only "$probe" 2>/dev/null; then
    COMMON_CXXFLAGS="$COMMON_CXXFLAGS -isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"
    echo ">> broken CLT libc++ headers detected; using SDK headers"
  fi
fi

fetch() { # url file
  local url="$1" out="$SRC/$2"
  if [[ ! -f "$out" ]]; then
    echo ">> fetching $url"
    curl -fL --retry 3 -o "$out.part" "$url"
    mv "$out.part" "$out"
  fi
}

verify() { # file sha256
  local f="$SRC/$1" want="$2" got
  if command -v sha256sum >/dev/null; then got=$(sha256sum "$f" | cut -d' ' -f1); else got=$(shasum -a 256 "$f" | cut -d' ' -f1); fi
  if [[ "$got" != "$want" ]]; then echo "checksum mismatch for $1: got $got want $want" >&2; exit 1; fi
}

unpack() { # file dir
  rm -rf "$BUILD/$2"; mkdir -p "$BUILD/$2"
  tar -xf "$SRC/$1" -C "$BUILD/$2" --strip-components=1
}

stamp() { [[ -f "$PREFIX/.done-$1" ]]; }
mark() { touch "$PREFIX/.done-$1"; }

# ---------------------------------------------------------------- dav1d
if ! stamp dav1d-$DAV1D_VER; then
  fetch "https://code.videolan.org/videolan/dav1d/-/archive/$DAV1D_VER/dav1d-$DAV1D_VER.tar.gz" dav1d.tar.gz
  verify dav1d.tar.gz "${DAV1D_SHA256}"
  unpack dav1d.tar.gz dav1d
  (cd "$BUILD/dav1d" && \
    CFLAGS="$COMMON_CFLAGS" meson setup _b --prefix="$PREFIX" --libdir=lib --buildtype=release \
      --default-library=static -Denable_tools=false -Denable_tests=false && \
    ninja -C _b -j"$JOBS" install)
  mark dav1d-$DAV1D_VER
fi

# ---------------------------------------------------------------- ffmpeg
if ! stamp ffmpeg-$FFMPEG_VER; then
  fetch "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VER.tar.xz" ffmpeg.tar.xz
  verify ffmpeg.tar.xz "${FFMPEG_SHA256}"
  unpack ffmpeg.tar.xz ffmpeg
  extra=()
  [[ $HOST == windows ]] && extra+=(--target-os=mingw32 --enable-w32threads)
  [[ $HOST != windows ]] && extra+=(--enable-pthreads)
  (cd "$BUILD/ffmpeg" && ./configure --prefix="$PREFIX" \
    --enable-gpl --enable-version3 --enable-static --disable-shared --enable-pic \
    --disable-autodetect --disable-programs --disable-doc --disable-network \
    --disable-everything --disable-debug \
    --enable-zlib --enable-libdav1d \
    --enable-avcodec --enable-avformat --enable-avfilter --enable-swscale --enable-swresample \
    --enable-protocol=file \
    --enable-demuxer=matroska,mov,mpegts,mpegps,mpegvideo,avi,ivf,obu,h264,hevc,m4v,vc1,flv \
    --enable-parser=h264,hevc,mpegvideo,mpeg4video,vc1,vp9,av1,dirac \
    --enable-decoder=h264,hevc,mpeg1video,mpeg2video,mpeg4,vc1,wmv3,vp9,libdav1d,prores,ffv1,utvideo,rawvideo \
    --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,vp9_superframe_split,av1_frame_split,extract_extradata \
    --enable-filter=fieldmatch,decimate,bwdif,yadif,format,null,buffer,buffersink,setfield,setpts,select,scale \
    --extra-cflags="$COMMON_CFLAGS -I$PREFIX/include" --extra-ldflags="-L$PREFIX/lib" \
    "${extra[@]}" && \
    make -j"$JOBS" && make install)
  mark ffmpeg-$FFMPEG_VER
fi

# ---------------------------------------------------------------- zimg
if ! stamp zimg-$ZIMG_VER; then
  fetch "https://github.com/sekrit-twc/zimg/archive/refs/tags/release-$ZIMG_VER.tar.gz" zimg.tar.gz
  verify zimg.tar.gz "${ZIMG_SHA256}"
  unpack zimg.tar.gz zimg
  (cd "$BUILD/zimg" && ./autogen.sh && \
    CFLAGS="$COMMON_CFLAGS" CXXFLAGS="$COMMON_CXXFLAGS" ./configure --prefix="$PREFIX" \
      --enable-static --disable-shared --disable-testapp --disable-example --disable-unit-test && \
    make -j"$JOBS" && make install)
  mark zimg-$ZIMG_VER
fi

# ---------------------------------------------------------------- ffms2
if ! stamp ffms2-$FFMS2_VER; then
  fetch "https://github.com/FFMS/ffms2/archive/refs/tags/$FFMS2_VER.tar.gz" ffms2.tar.gz
  verify ffms2.tar.gz "${FFMS2_SHA256}"
  unpack ffms2.tar.gz ffms2
  (cd "$BUILD/ffms2" && mkdir -p src/config && autoreconf -fi && \
    CFLAGS="$COMMON_CFLAGS" CXXFLAGS="$COMMON_CXXFLAGS" ./configure --prefix="$PREFIX" \
      --enable-static --disable-shared && \
    make -j"$JOBS" && make install)
  mark ffms2-$FFMS2_VER
fi

# Licence texts travel with the prefix (CI caches only the prefix; packaging/notices.sh reads these).
mkdir -p "$PREFIX/share/licenses"
for pair in "ffmpeg:LICENSE.md" "ffms2:COPYING" "zimg:COPYING" "dav1d:COPYING"; do
  lib=${pair%%:*}; file=${pair#*:}
  if [[ -f "$BUILD/$lib/$file" ]]; then
    mkdir -p "$PREFIX/share/licenses/$lib"
    cp "$BUILD/$lib/$file" "$PREFIX/share/licenses/$lib/$file"
  fi
done

echo ">> deps installed into $PREFIX"
