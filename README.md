# anime-comp

Frame-exact anime source comparisons (WEB-DL vs BD vs encodes vs DVD), with lossless PNG screenshots and slow.pics upload.
Native Rust app with statically linked ffmpeg/ffms2/zimg. See [PLAN.md](PLAN.md) for the architecture and roadmap.

## Building

```bash
deps/build.sh                 # static dav1d, ffmpeg, zimg, ffms2 -> deps/prefix (one-off, ~5 min)
cargo build --release         # builds target/release/comp
```

Requirements: Rust, a C/C++ toolchain, autoconf/automake/libtool, meson, ninja, pkg-config, nasm (x86 only), libclang.
On Windows, build inside MSYS2 UCRT64 (see `.github/workflows/ci.yml`).

## Testing

```bash
testdata/make_corpus.sh       # synthetic clips (needs system ffmpeg with x264/x265/svt-av1/vpx)
cargo test --release -- --nocapture --test-threads=1
```

The seek-accuracy test checks every frame of every clip, read straight through and in random
order, against the MD5s from an independent system ffmpeg decode.

## CLI (M0)

```bash
comp info <files…>
comp shot <file> <frame…> -o <dir>     # lossless PNGs; anamorphic sources stretched to DAR
```
