# Anime Comparisons

Frame-exact anime source comparisons (WEB-DL vs BD vs encodes vs DVD), with lossless PNG screenshots and slow.pics upload.
Native Rust app with statically linked ffmpeg/ffms2/zimg. See [PLAN.md](PLAN.md) for the architecture and roadmap.

## Building

```bash
deps/build.sh                 # static dav1d, ffmpeg, zimg, ffms2 -> deps/prefix (one-off, ~5 min)
cargo build --release         # builds target/release/comp
```

Requirements: Rust, a C/C++ toolchain, autoconf/automake/libtool, meson, ninja, pkg-config, nasm (x86 only), libclang.
On Windows, build inside MSYS2 UCRT64 (see `.github/workflows/ci.yml`).

## Desktop app

```bash
cargo run --release -p comp-gui          # or: packaging/macos/bundle.sh → "target/release/Anime Comparisons.app"
```

Three steps: **1 Select** (drop files or episode folders onto the window, pick episodes and settings), **2 Sync** (line the sources up: offsets, flicker), **3 Create** (review the setup, create the comparison, follow progress and history). **Viewer** shows finished comparisons, including older ones, and is where you adjust frames and upload to slow.pics.
**Viewer** shows any comparison (side by side, slider, flicker, synced zoom) and lets you fix odd frames by hand:
move a source ±1 frame or to an exact frame, or replace the frame. The PNGs, `comp.json` and `index.html` update immediately. Setups save as TOML projects that the CLI can also run (`comp project file.toml`), and `anime-comp file.toml` opens one directly.

## Packaging

```bash
packaging/macos/bundle.sh --dmg      # macOS: "target/release/Anime Comparisons.app" + .dmg
packaging/linux/docker-build.sh      # Linux AppImage + CLI tarball in manylinux_2_28 (Docker) → target/linux/
packaging/windows/package.sh         # Windows (MSYS2 UCRT64): zip, fails on non-system DLL imports
packaging/notices.sh                 # regenerate THIRD_PARTY_NOTICES.md
```

Pushing a `v*` tag runs `.github/workflows/release.yml` (all three platforms → draft GitHub release with SHA256SUMS). macOS builds are ad-hoc signed unless the Developer ID secrets listed in the workflow are set.

## Testing

```bash
testdata/make_corpus.sh       # synthetic clips (needs system ffmpeg with x264/x265/svt-av1/vpx)
cargo test --release
```

The seek-accuracy test checks every frame of every clip, read straight through and in random
order, against the MD5s from an independent system ffmpeg decode.

## CLI

```bash
comp info <files…>
comp shot <file> <frame…> -o <dir>              # lossless PNGs of single frames
comp pair -s BD=/bd/folder -s DVD=/dvd/folder   # check episode pairing (no decoding)
comp sync -s BD=/bd/ep01.mkv -s DVD=/dvd/ep01.mkv   # suggest offsets (scene cuts), with confidence
comp run  -s BD=/bd/ep01.mkv -s DVD=/dvd/ep01.mkv -n 20 --seed 1
comp run  -s BD=/bd/folder -s DVD=/dvd/folder   # whole season, one comparison per episode (--episodes 1,2,5 for some)
comp project setup.toml                         # run a saved project (see --save-project)
comp viewer comps/<name>                        # refresh index.html of existing comparisons (no decoding)
comp fix "comps/<name>" --frame 12713 --source DVD --nudge -1   # re-shoot one source's frame
comp fix "comps/<name>" --frame 12713 --replace                 # swap for a random nearby frame
```

Useful `run` options:

- `--offset LABEL=N` / `LABEL=Xs`: frame or time offset. Sources are aligned by timestamp first, so VFR and CFR releases line up.
- `--ivtc LABEL[:tff|bff]` / `--deint LABEL`: field processing for 1080i BD remuxes and NTSC DVDs.
- `--frame-type ENC=B`: only frames that are B-frames in the `ENC` source.
- `--exclude-op-ed`, `--exclude 1:30-3:00`: leave out OP/ED chapters or time ranges. Episodes are compared in full by default.
- `--skip-duplicates`, `--skip-flat`, `--dark 0.1`, `--no-reject`: change the frame-picking rules. By default only black screens and scene changes are skipped.
- `--matrix LABEL=709`, `--no-sar`.
- Black borders (pillarbox/letterbox) are detected per source and cropped from every shot; `--no-crop` keeps them.
- `--open`: open the offline viewer (`index.html`, written next to the PNGs) when done.
- `--open-slowpics`: open the output folder and slow.pics' upload page for a manual drag-and-drop upload. Automated uploads need slow.pics staff permission, so the tool doesn't upload itself.
