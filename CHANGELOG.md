# Changelog

All notable changes to SlopComp are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

Add each change under **Unreleased** as you make it. `packaging/release.sh` moves that
section under the new version number when you release, and the GitHub release uses it as
its notes.

## [Unreleased]

## [0.1.0] - 2026-11-11

First public release.

### Added

- Desktop app in three steps: **Select** (drop files or episode folders, pick episodes
  and settings), **Sync** (line sources up with live previews, offsets and flicker) and
  **Create** (summary, progress, log and job history).
- **Viewer** for finished comparisons: side by side, slider and flicker, synced zoom and
  pan, thumbnail strip, keyboard shortcuts. Fix odd frames by hand: move a source ±1
  frame or to an exact frame, or replace the frame. Open one comparison or every
  comparison under a folder.
- Frame-exact decoding of WEB-DL, BD, encodes and DVD sources (statically linked
  ffmpeg, ffms2 and zimg), with lossless PNG screenshots.
- Sources aligned by timestamp, so VFR and CFR releases line up; per-source and
  per-episode frame or time offsets.
- Automatic offset suggestion from scene cuts, with a confidence rating.
- IVTC and deinterlacing for 1080i BD remuxes and NTSC DVDs.
- Black border (pillarbox/letterbox) detection and cropping per source.
- Frame-picking rules: skip black screens and scene changes by default; optional rules
  for near-duplicates, flat frames, dark scenes and picture type (e.g. only B-frames).
- Leave out OP/ED chapters or time ranges.
- Whole seasons: episodes paired across folders by their numbers, one comparison per
  episode, plus an index page.
- Offline HTML viewer (`index.html`) written next to every comparison.
- Screenshot names that suit slow.pics batch upload, and a button that opens the
  output folder and slow.pics for a manual upload.
- Projects saved as TOML, usable from the app and the `comp` command-line tool.
- `comp` command-line tool: `info`, `shot`, `pair`, `sync`, `run`, `project`, `viewer`,
  `fix`.
- Downloads for macOS (Apple Silicon), Linux (x64 AppImage) and Windows (x64).

### Downloads

- **macOS** (Apple Silicon, macOS 11 or later): `SlopComp-0.1.0-macos-arm64.dmg`. The app
  isn't notarized, so the first time, right-click it and choose **Open**, or run
  `xattr -dr com.apple.quarantine /Applications/SlopComp.app`.
- **Linux** (x64, glibc 2.28 or later): `SlopComp-0.1.0-linux-x86_64.AppImage`; make it
  executable and run it. The `comp` command-line tool alone is in the `.tar.gz`.
- **Windows** (x64): unzip `SlopComp-0.1.0-windows-x64.zip` and run `anime-comp.exe`. This
  is the first Windows build, so please report anything that goes wrong.
- `SHA256SUMS.txt` has the checksums of every download.

### Known issues

- For telecined sources (IVTC), the automatic offset suggestion can occasionally be a couple of
  frames off while still reporting high confidence. Check the Sync previews before applying it.

[Unreleased]: https://github.com/amq-tutti/slop-comp/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/amq-tutti/slop-comp/releases/tag/v0.1.0
