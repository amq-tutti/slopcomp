<div align="center">

<h1><img src="docs/images/icon.png" width="128" height="128" alt="SlopComp icon"><br>SlopComp</h1>

**Frame-exact anime source comparisons, without the scripting.**

Line up WEB-DLs, Blu-rays, fansub encodes and DVDs, pick frames automatically,
and get lossless PNG screenshots ready for slow.pics.

[![Latest release](https://img.shields.io/github/v/release/amq-tutti/slopcomp?label=download&color=8b5cf6)](https://github.com/amq-tutti/slopcomp/releases/latest)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-ec4899)](LICENSE)
![macOS · Linux · Windows](https://img.shields.io/badge/platforms-macOS%20%C2%B7%20Linux%20%C2%B7%20Windows-334155)
[![CI](https://github.com/amq-tutti/slopcomp/actions/workflows/ci.yml/badge.svg)](https://github.com/amq-tutti/slopcomp/actions/workflows/ci.yml)

[**Download**](#download) · [Features](#features) · [How it works](#how-it-works) · [Under the hood](#under-the-hood) · [Command line](#command-line) · [Building](#building-from-source)

<img src="docs/images/viewer.png" alt="SlopComp's viewer showing the same frame from a BD remux, a BD encode and a DVD encode side by side">

</div>

## Features

- **Frame-exact decoding.** Seeking is tested against an independent ffmpeg decode, frame by
  frame, read straight through and in random order, so the frame you ask for is the frame you get.
- **Mixed releases line up.** Sources are aligned by timestamp, so VFR encodes and CFR DVDs match.
  Offsets can be set per source and per episode.
- **Automatic offset suggestions.** Scene cuts are matched between sources, with a confidence
  rating that flags a wrong match.
- **IVTC and deinterlacing** for 1080i BD remuxes and telecined NTSC DVDs.
- **Smart frame picking.** Black screens and scene changes are skipped. Optional rules skip
  near-duplicates, flat or dark frames, or keep only B-frames from an encode.
- **Whole seasons at once.** Drop in episode folders: episodes are paired by their numbers,
  OP/ED chapters can be left out, and you get one comparison per episode.
- **Black borders cropped** per source, and anamorphic DVDs stretched to their display size.
- **Built-in viewer** with side by side, slider and flicker modes, synced zoom and pan, and
  frame-by-frame fixes when a shot isn't right.
- **Shareable output.** Lossless PNGs named for slow.pics batch upload, plus an offline
  `index.html` viewer next to every comparison.
- **Command-line tool** (`comp`) for scripting, with the same projects as the app.

## Download

Get the latest version from the [**Releases page**](https://github.com/amq-tutti/slopcomp/releases/latest).

| Platform | File | Notes |
| --- | --- | --- |
| **macOS** (Apple Silicon, 11+) | `SlopComp-<version>-macos-arm64.dmg` | Not notarized: the first time, right-click the app and choose **Open**. |
| **Linux** (x64, glibc 2.28+) | `SlopComp-<version>-linux-x86_64.AppImage` | `chmod +x` it and run. The `comp` tool alone is in the `.tar.gz`. |
| **Windows** (x64) | `SlopComp-<version>-windows-x64.zip` | Unzip and run `SlopComp.exe`. |

Everything is statically linked (ffmpeg, ffms2, zimg, dav1d), so there's nothing else to install.
The Releases page shows each download's SHA-256 checksum next to it.

## How it works

The app walks you through three steps, then opens the result in the viewer.

### 1 · Select

Drop video files or whole episode folders onto the window. The first source is the reference;
set field handling (progressive, IVTC, deinterlace) and colour matrix per source, pick episodes,
and choose how many frames to take.

<img src="docs/images/select.png" alt="Select step with three sources and episode pairing">

### 2 · Sync

Check that the sources line up with live previews, flick between them, and apply the suggested
offsets (or set your own, per episode if a release differs).

<img src="docs/images/sync.png" alt="Sync step with suggested offsets and side-by-side previews">

### 3 · Create

Review the setup and start. Progress is shown per source and per episode, with a log and a
history of finished jobs.

<img src="docs/images/create.png" alt="Create step with the summary and a comparison in progress">

### Viewer

Look through the result in side by side, slider or flicker mode, zoom in on details, and fix
any shot: move a source a frame either way, jump to an exact frame, or swap the frame for
another. The PNGs, `comp.json` and `index.html` update immediately. From here you can also open
the output folder or slow.pics for upload.

Setups can be saved as TOML projects, which the command-line tool can run too
(`comp project file.toml`), and `slopcomp file.toml` opens one in the app.

<sub>Screenshots show *To Heart* (© AQUAPLUS / KSS) for illustration.</sub>

## Under the hood

Each comparison goes through the same stages. The app and `comp` share one core library
(`crates/comp-core`), so both behave identically.

### Decoding: ffms2

Every file is indexed with [ffms2](https://github.com/FFMS/ffms2), the same indexer behind
VapourSynth's `ffms2.Source`. The index maps every frame to its packet, keyframe and timestamp, so
asking for frame N returns exactly frame N, including with open GOPs, VFR and m2ts. Indexes are
cached, keyed by file size, modification time and a hash of the file's first and last MiB.

MPEG program streams (DVD VOB/`.mpg`) return the frame *before* the requested one when ffms2
seeks to a keyframe, so they're decoded linearly instead. The test suite checks every frame of
every test clip, read straight through and in random order, against an independent ffmpeg decode.

### Alignment: timestamps, then offsets

Sources are lined up by **time, not frame number**. A reference frame at time `t` maps to the
frame on screen at `t + offset` in each other source, plus an optional frame nudge. This is what
lets a VFR BD encode (23.976 with 29.97 sections) line up with a CFR DVD. Counting frames, those
two drift ~780 frames apart over an episode.

Suggested offsets (`comp sync`, or **Auto** in the Sync step) come from scene cuts:

1. Black borders are detected and cropped, so a pillarboxed BD can be matched against a 4:3 DVD.
2. A 60-second window of the reference and a ±15 s wider window of the other source are decoded
   into 16×16 luma thumbnails.
3. Scene cuts are found in both. The time offset that lines up the most cuts wins. With too few
   cuts, it falls back to a correlation search over every offset.
4. The winner is confirmed by correlating brightness-normalised thumbnails. That works across
   resolution, codec and colour differences.
5. The offset is re-checked at 4 more points in the episode to catch releases whose timing drifts
   (seen with BD vs DVD).

The confidence score compares the match to the correlation at *wrong* offsets. A low score means
"check it in the Sync view"; it's never applied silently.

### Field processing: IVTC and deinterlacing

IVTC runs ffmpeg's filters in-process through libavfilter:

```
fieldmatch (order=tff|bff) → bwdif (deint=interlaced) → decimate
```

`fieldmatch` and `decimate` are ffmpeg's ports of VIVTC's VFM and VDecimate, the same
algorithms a VapourSynth IVTC script uses. `bwdif` only touches frames that are still combed after
field matching. **Deinterlace** mode runs `bwdif` on every frame instead.

libavfilter works on a stream, and decimation drops 1 frame in 5, so "output frame k" only has a
meaning after the whole file has gone through the filters. Random access is built on top in two
steps:

1. **Analysis pass**, once per file, then cached. Every source frame goes through the chain tagged
   with its index (`AVFrame.opaque`, which every filter carries over). For each output frame the
   app records which source frame it came from and an xxh3 hash of its pixels.
2. **Random access.** To fetch output frame k, the chain is rebuilt and fed from two 5-frame
   cycles earlier, starting on a cycle boundary. The output's hash must match the analysis pass.
   If it doesn't, the window grows, down to processing from frame 0. Every frame you get is
   therefore verified identical to processing the whole file in order.

IVTC output gets constant timestamps at 4/5 of the source rate, anchored per cycle. Telecined
timestamps sit up to half a film frame early, which would make time alignment pick the
neighbouring frame. IVTC is refused on sources already at 25 fps or less, since decimating
would drop real frames.

The tests run IVTC on 3:2-telecined copies of a known progressive clip. On a lossless copy every
frame must match the original exactly. On lossy H.264 and DVD MPEG-2 copies, every output must
be the right original frame.

Analysis runs at ~150 fps for 1080i on an M-series Mac, so a 24-minute 1080i episode takes about
5 minutes once.

### Frame picking

Picks are made on the reference timeline, and only from frames that exist in every source:

- The usable range is split into N equal bins, with one random frame per bin, so picks spread
  across the whole episode. The RNG (ChaCha8) is seeded, so the same seed gives the same frames.
- Each candidate is judged on a small luma thumbnail. By default only black screens (mean luma
  below 3%) and frames next to a scene cut or inside a fade are rejected, and a rejected candidate
  is re-rolled up to 16 times in its bin. Near-duplicates (difference hash), flat frames and dark
  scenes are opt-in. **B-frames only** checks the picture type ffms2 reports for an encode.
- Black screens and scene changes are never used as fallbacks. If a bin has nothing else, the
  pick moves to the largest gap between the other picks.
- OP/ED can be left out using chapter names (`OP`, `Opening`, `オープニング`, …) or time ranges.

### Screenshots: zimg

[zimg](https://github.com/sekrit-twc/zimg), the library behind VapourSynth's `resize`,
converts each frame to 8-bit RGB:

- The matrix, range and chroma siting come from the stream. If the stream doesn't say, SD sizes
  use BT.601 and everything else BT.709. Each source can override this.
- Chroma is upsampled with Spline36. Sources deeper than 8 bits get error-diffusion dithering;
  8-bit sources are only rounded.
- Frames stay at their native resolution. The one exception is anamorphic sources, which are
  widened to their display aspect ratio in the same pass (a 16:9 NTSC DVD becomes 853×480).
  Black borders are cropped after that.

PNGs are named `{frame}_{label}.png`, the layout slow.pics' batch upload expects, and an offline
`index.html` viewer is written next to them.

## SlopComp and VapourSynth

Most people making comparisons today write a [VapourSynth](https://www.vapoursynth.com) script:
load the sources with `ffms2.Source` or `lsmas.LWLibavSource`, trim and IVTC them, and then save
frames or upload them with a comparison helper. The one that came up while building SlopComp is
[vs-preview](https://github.com/Jaded-Encoding-Thaumaturgy/vs-preview)'s comparison tool, which
picks frames, saves them and uploads to slow.pics. SlopComp's planned slow.pics client was based
on its upload flow.

SlopComp uses the same building blocks: ffms2 for decoding, zimg for colour conversion, and the
VFM/VDecimate algorithms for IVTC. What's different is what's around them.

| | VapourSynth script | SlopComp |
| --- | --- | --- |
| **Setup** | VapourSynth, Python, source and IVTC plugins, a previewer, and a script | One download, nothing else to install |
| **Alignment** | `Trim` by frame number, offsets found by eye | By timestamp (VFR and CFR line up), with suggested offsets, a confidence score and drift checks |
| **Frame picking** | Whatever the script does, often random or hand-picked | Spread across the episode and seeded. Black screens and scene changes are skipped. B-frames only and OP/ED exclusion are optional |
| **IVTC** | VIVTC with every option, per-frame overrides, Wobbly for hybrid or VFR content | VFM/VDecimate defaults, a fixed 5-frame cycle, 8-bit only, no overrides |
| **Other filters** | Anything: descale, debanding, your own encode chain | None. Sources are shown as they are, apart from IVTC, crop and anamorphic stretch |
| **Whole seasons** | Usually one script per episode | Folders are paired by episode number, one comparison per episode |
| **Fixing shots** | Edit the script and run it again | Nudge or replace a frame in the viewer, and only those PNGs are redone |

If you need filtering beyond IVTC, such as comparing your own encode chain or doing a descale
test, a script is the better tool. SlopComp is for comparing releases as they are.

### Why VapourSynth isn't bundled

- **Plugins are loaded from files.** VapourSynth's core can be linked in and used through its C
  API without Python. But VIVTC and the ffms2 source are plugins, which the core loads as shared
  libraries at runtime. Linking them statically would mean patching the core to register them
  in-process, and maintaining that fork. Otherwise every release would ship extra
  `.dylib`/`.dll`/`.so` files on three platforms. On macOS each one is another file to get past
  Gatekeeper without a Developer ID.
- **It wouldn't change the result.** Decoding and colour conversion already use ffms2 and zimg
  directly. ffmpeg's `fieldmatch`/`decimate` are ports of the VIVTC filters. Bundling VapourSynth
  would mostly add a second copy of libraries the app already has.
- **No scripts means no scripting runtime.** Without user scripts there's nothing for Python or
  VSScript to do, and they're the heaviest part to ship.

The cost of using libavfilter instead is random access: VapourSynth can compute any frame on
demand, while libavfilter needs the analysis pass and hash-verified windows described
[above](#field-processing-ivtc-and-deinterlacing).

## Command line

Every download includes the `comp` command-line tool:

- **Windows:** `comp.exe`, next to `SlopComp.exe` in the zip.
- **macOS:** inside the app, at `SlopComp.app/Contents/MacOS/comp`. To run it as `comp` from any
  terminal: `ln -s /Applications/SlopComp.app/Contents/MacOS/comp /usr/local/bin/comp`.
- **Linux:** on its own in `comp-<version>-linux-x86_64.tar.gz`, or through the AppImage:
  `./SlopComp-<version>-linux-x86_64.AppImage comp …`.

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

## Building from source

```bash
deps/build.sh                 # static dav1d, ffmpeg, zimg, ffms2 -> deps/prefix (one-off, ~5 min)
cargo build --release         # builds target/release/comp
```

Requirements: Rust, a C/C++ toolchain, autoconf/automake/libtool, meson, ninja, pkg-config, nasm (x86 only), libclang.
On Windows, build inside MSYS2 UCRT64 (see `.github/workflows/ci.yml`).

Run the app with `cargo run --release -p comp-gui`. See [PLAN.md](PLAN.md) for the architecture and roadmap.

### Testing

```bash
testdata/make_corpus.sh       # synthetic clips (needs system ffmpeg with x264/x265/svt-av1/vpx)
cargo test --release
```

The seek-accuracy test checks every frame of every clip, read straight through and in random
order, against the MD5s from an independent system ffmpeg decode.

### Packaging

```bash
packaging/macos/bundle.sh --dmg      # macOS: "target/release/SlopComp.app" + .dmg
packaging/linux/docker-build.sh      # Linux AppImage + CLI tarball in manylinux_2_28 (Docker) → target/linux/
packaging/windows/package.sh         # Windows (MSYS2 UCRT64): zip, fails on non-system DLL imports
packaging/notices.sh                 # regenerate THIRD_PARTY_NOTICES.md
```

### Releasing

Note each user-visible change under **Unreleased** in [CHANGELOG.md](CHANGELOG.md) as you go. To release:

```bash
packaging/release.sh 0.2.0
```

It moves the Unreleased notes under the new version, sets the version in `Cargo.toml`, shows the notes and asks before it commits, tags `v0.2.0` and pushes. The tag runs `.github/workflows/release.yml`: all three platforms are built and a **draft** GitHub release is created with the changelog notes and the downloads. Check it on the repo's Releases page and publish it. Versions with a suffix (`0.2.0-beta.1`) are marked as pre-releases.

The workflow can also be started from the Actions tab without a tag, to test the builds; that run doesn't create a release. macOS builds are ad-hoc signed unless the Developer ID secrets listed in the workflow are set.

## License

SlopComp is free software under the [GNU GPL v3.0 or later](LICENSE). It bundles ffmpeg, ffms2,
zimg, dav1d and other libraries; their licences are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
