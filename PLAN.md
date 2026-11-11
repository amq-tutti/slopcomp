# anime-comp — Architecture Plan

A native, cross-platform tool for anime source comparisons (WEB-DL vs BD Remux vs encodes vs hybrids, including DVD).
It picks the same frames across sources, captures lossless PNGs and uploads them to slow.pics.

## 1. Decisions (from the clarifying wizard)

| Area | Decision |
|---|---|
| Language / UI | Rust + egui (eframe). Fully native, no webview, no Electron. |
| Platforms | macOS arm64, Windows x64, Linux x64 |
| License | GPLv3 (lets us bundle a full ffmpeg build) |
| DVD | In scope, including IVTC and deinterlacing. PAL speedup is out of scope. |
| Resolution | Native: each source is captured at its own resolution. The one exception is that anamorphic sources (e.g. DVD) are stretched to their display aspect ratio. |
| OP/ED | Kept: episodes are compared in full. Opt-in `--exclude-op-ed` (chapters). Separate NCOP/NCED files are just ordinary sources. |
| Frame picking rules | Default: skip only black screens and scene changes (cuts, fades). Near-duplicates, flat frames and dark scenes are opt-in. |
| HDR | Out of scope (no tonemapping) |
| Hybrids | Treated like any other encoded file. No special handling. |
| Offsets | One frame offset per source, set manually with a sync preview, plus an "auto-suggest" button |
| Pairing | Automatic by episode number parsed from filenames, editable by hand |
| Frame picking | Seeded random, skipping dark frames and scene transitions, optional frame-type preference (I/P/B) |
| Upload | Anonymous slow.pics only |
| Interfaces | A shared core library used by a CLI and a GUI |

## 2. Workspace layout

```
anime-comp/
├─ Cargo.toml                # workspace
├─ crates/
│  ├─ comp-sys/              # bindgen FFI to ffms2 + zimg + libavutil pixdesc (static link)
│  ├─ comp-core/             # all logic, no UI
│  │  ├─ source/             # open + index + frame-accurate decode (ffms2)
│  │  ├─ filter/             # IVTC / deinterlace (libavfilter graphs)
│  │  ├─ convert/            # YUV -> RGB via zimg, PNG encode
│  │  ├─ select/             # frame selection (seeded RNG, filters, frame types)
│  │  ├─ sync/               # offset model + auto-suggest (perceptual hashing)
│  │  ├─ pairing/            # filename -> episode number parsing, matching
│  │  ├─ project/            # serialisable project file (TOML)
│  │  ├─ job/                # pipeline runner, progress events, cancellation
│  │  └─ slowpics/           # anonymous upload client
│  ├─ comp-cli/              # clap front-end
│  └─ comp-gui/              # eframe/egui front-end
├─ deps/                     # build scripts for static ffmpeg / ffms2 / zimg / dav1d
└─ .github/workflows/        # CI matrix + packaging
```

The GUI and CLI only talk to `comp-core` through a `Project` (the inputs) and a `Job` (which emits progress events).
The project file is shared, so a comparison set up in the GUI can be re-run headless with `comp run project.toml`.

## 3. Video backend

### 3.1 Frame-accurate decoding — ffms2
- **Why ffms2:** it builds a full index (frame → packet/keyframe/PTS), so seeking returns *exactly* frame N, even with open-GOP, VFR, broken timestamps and m2ts. A plain `ffmpeg -ss` cannot promise that.
  ffms2 has a stable **C API**, so there is no Python or VapourSynth runtime to bundle.
- ffms2 reports the picture type per frame (`FFMS_Frame::PictType`: `I`/`P`/`B`), which the frame-type picker uses.
- **Index cache:** stored in `<cache_dir>/anime-comp/index/<key>.ffindex`. The key is file size + mtime + xxh3 of the first and last 1 MiB. Indexing runs on a background thread and reports progress.
- Output format: native planar YUV (8/10/16-bit) with colour metadata (matrix, transfer, primaries, range, chroma location, SAR).
- **Seek policy (found in M0):** with keyframe seeking, MPEG program streams (DVD VOB/.mpg) consistently return the frame *before* the one requested.
  `SeekMode::Auto` therefore uses linear decoding for the `mpeg`/`mpegvideo` demuxers. Callers request frames in ascending order, so this costs one sequential pass.
  MPEG-2 in MKV and in TS seeks correctly.
- **Correctness test (M0):** decode N random frames with seeking and the same frames sequentially from the start, then compare hashes. Run this across a corpus of h264, hevc 10-bit, AV1, MPEG-2 (DVD VOB/MKV) and VC-1 (old BD) files.

### 3.2 IVTC / deinterlacing — libavfilter (implemented, `comp-core::filter`)
- **IVTC chain:** `fieldmatch` (TFM port, default `combmatch=sc` as in VFM), then `bwdif` (`deint=interlaced`, only for frames still combed after matching), then `decimate` (TDecimate port, 5 → 4).
  **Deinterlace chain:** `bwdif` with `deint=all`, one output per input. Input must be 8-bit YUV, as `fieldmatch` requires.
  `combmatch=full` was rejected: it turns false comb detections on sharp detail into wrong field matches.
- **Source tagging:** `decimate` rewrites pts, so every input AVFrame carries its source index in `AVFrame.opaque`, which every filter in the chain copies. Each output therefore knows its source frame.
- **Analysis pass** (once per file, cached as `<key>.<mode>-<order>.fieldmap`): sequential decode through the chain, recording for each output `(source index, xxh3 of pixels)`.
- **Random access:** rebuild the chain and feed from 2 decimation cycles before the target (cycle-aligned), then take the output tagged with the wanted source index. **Its hash must match the analysis pass**, otherwise the window grows ×4, down to frame 0. Ascending requests continue the running chain.
  Verified bit-exact against lossless telecined ground truth, with 0 window retries on the corpus.
- **Timestamps:** IVTC output is CFR at 4/5 of the source rate, anchored per decimation cycle. Telecined field timestamps sit up to half a film frame early, which would make time alignment against progressive sources pick the neighbouring frame.
- **Guard:** IVTC is refused on sources already at ≤ 25 fps, since decimating would drop real frames.
- Per-source options: `--ivtc LABEL[:tff|bff]`, `--deint LABEL[:tff|bff]`. Field order is auto-detected from the stream otherwise.
- Speed: analysis runs at ~150 fps for 1080i on an M-series Mac, so a 24-minute 1080i episode takes ~5 minutes once. A cached shot costs a ~15-frame replay window.

### 3.3 Colour conversion — zimg
- zimg (the same library VapourSynth's resize uses) converts YUV to RGB24 with correct matrix, range and chroma siting, and error-diffusion dithering for >8-bit input.
- Metadata comes from the stream. When it is missing, fall back by resolution: width ≤ 1024 and height ≤ 576 → BT.601 (170M, or 470BG for PAL-sized frames), otherwise BT.709, with limited range. Each source can override this.
- Chroma upsampling kernel: global setting, default **spline36**, with **point** as an option for chroma-artifact comparisons.
- Output: 8-bit RGB PNG (default, which slow.pics renders properly), optional 16-bit PNG for local analysis.
- PNG encoder: the `png` crate with fast compression, then optional `oxipng` level 2 in a worker pool. Smaller uploads matter on slow.pics.
- **Native resolution, with an anamorphic stretch:** frames are saved at storage resolution unless the sample aspect ratio (SAR) isn't 1:1.
  In that case only the **width** is resized to the display aspect ratio, and the height stays native. For example, a 720×480 NTSC 16:9 DVD (SAR 32:27) becomes 853×480, and a 4:3 one (SAR 8:9 → 640×480).
  The resize happens in the same zimg pass as the RGB conversion, using spline36 (configurable). Each source can override the SAR for files with wrong or missing flags.
  Sources with square pixels (all normal WEB/BD) are never resampled.

## 4. Comparison pipeline

```
Sources (≥2 groups) ──► pairing ──► per-episode set
        │                               │
        ▼                               ▼
  index (ffms2) ──► optional IVTC map ──► offsets (manual / auto-suggest)
                                        │
                                        ▼
                         frame selection on reference timeline
                                        │
                                        ▼
         for each selected frame × source: decode(frame + offset_s) ──► zimg ──► PNG
                                        │
                                        ▼
                           output dir  ──►  slow.pics upload (optional)
```

### 4.1 Sources and pairing
- A **source group** = a label ("BD Remux", "WEB-DL (CR)", "Encode XYZ") + 1..n files (dropped in or picked from a folder).
- An episode-number parser handles common release naming: `S01E03`, ` - 03 `, `[03]`, `E03`, `_03_`, `Ep03`, `03v2`, specials (`OVA`, `SP`, `NCOP`, `NCED`). It strips CRCs, release groups and resolution tags first.
  A small hand-written parser with a test table, or the `anitomy` crate if a maintained one exists.
- Pairing table: rows = episodes, columns = source groups. Missing cells are flagged, and cells can be edited by drag and drop in the GUI or by an explicit mapping in the project file.

### 4.2 Offsets
- **Alignment is by timestamp, not by frame number (found in M1).** Real releases mix VFR and CFR. For example, a VFR BD encode (23.976 with 29.97 sections) against a CFR 23.976 DVD encode drifts by ~780 frames over an episode with no change in timing.
  A reference frame at time `t` maps to the frame on screen at `t + offset_seconds` in each source, and then `offset_frames` is applied on top. Timestamps are normalised so each source's first frame is at 0.
- One group is the **reference** (default: the first, usually the BD remux). Every other group has a signed frame offset `o_s`, so that source frame = reference frame + `o_s`.
- Offsets are stored per episode, defaulting to a group-wide value. A WEB-DL often has a constant offset across a season (e.g. a 24-frame studio logo), so set it once and override only where needed.
- **Auto-suggest:**
  1. Sample a window (e.g. 3 minutes from 10% into the episode) in both sources.
  2. Downscale the Y plane to 32×32, compute a 64-bit DCT pHash per frame, plus a scene-change signal (frame-to-frame luma difference).
  3. Cross-correlate the scene-change signals over a search range (±10 s by default) to get candidate offsets. Confirm the best candidate by its mean pHash Hamming distance.
  4. Report the offset and a confidence score. The user accepts it in the sync view. Low confidence → warning, never applied silently.
  This works across different resolutions, colour handling and encoders, because it compares structure rather than pixels.
- Each pick is checked against the overlapping range of all sources, so it never lands before the start or after the end of any source.

### 4.3 Frame selection
Selection runs on the reference timeline and checks each candidate in every source:
1. **Seeded RNG** (ChaCha8; the seed is stored in the project and shown in the UI, so runs can be reproduced).
2. **Stratified sampling:** split the usable range into N equal bins and pick one candidate per bin, so picks spread across the episode instead of clustering.
   OP/ED ranges stay in by default. `--exclude-op-ed` removes them (see 4.3.1).
3. **Rejection filters** (a candidate is re-rolled within its bin, up to K tries). *As built:* the defaults reject only black screens (mean luma < 0.03) and scene changes (cut next door or fade). Duplicates, flat frames and dark scenes are opt-in.
   Hard rejections (black, scene change) are never used as fallbacks. Their bins are refilled from the largest gaps between picks.
   - *Dark frame:* mean luma of the reference frame below a threshold (default ~ 16% of range), computed on the Y plane.
   - *Transition / scene cut:* a large luma difference to the previous or next frame, or a fade (monotonic brightness ramp over ±3 frames).
   - *Near-duplicate:* pHash too close to an already chosen frame (avoids static shots twice).
4. **Frame-type preference:** pick a "frame-type source" (an encode) and a wanted type (B / P / I / any). A candidate passes only if that source's frame at `ref + o_s` has the wanted `PictType`. B-frames are the usual choice, because they get the least bits and show encoder weaknesses.
5. Optional extras later: manual picks added from the GUI scrubber, a fixed count of "dark scene" picks on purpose for banding/dither comparisons.

#### 4.3.1 OP/ED detection
OP/ED ranges are worked out on the reference timeline. Each method below is tried in order, and the first one that gives an answer is used:
1. **Manual ranges** set per episode or per group (e.g. "OP 01:30–03:00"). These always take priority.
2. **Chapters**, read through libavformat. ffms2 doesn't expose chapters, so we use a cheap demux-only open. Chapter titles are matched case-insensitively against `OP`, `Opening`, `ED`, `Ending`, `Intro`, `Outro`, `Credits` and similar names, including common Japanese ones (`オープニング`, `エンディング`).
   Many BD remuxes have these chapters. Unnamed chapters (`Chapter 02`) are ignored, unless the user maps a chapter number to OP/ED for the whole group.
3. **Cross-episode repeat detection**, used when a group has 2+ episodes without usable chapters. It reuses the pHash sequences from the sync module to find a long run of frames (≥ 60 s) that appears in several episodes of the same source. That run is the OP or ED.
   Detected ranges are shown in the Sync view's timeline, where you can confirm or adjust them.
4. Nothing found → nothing is excluded, and the result is marked with a warning badge.

The range is removed from every source through its offset, so a pick never lands in the OP of any source.

### 4.4 Output
```
out/<comp name>/<episode>/<NNNNN>_<group-label>.png
out/<comp name>/comp.json   # project snapshot, seed, offsets, frame list, slow.pics URL
```
File names stay ASCII-safe. Group labels go into the slow.pics image names unchanged.

## 5. slow.pics client (anonymous) — ON HOLD

> **Status (M3):** slow.pics' rules (https://slow.pics/about#scripts) say upload scripts and tools need permission from staff, and undiscussed tools may be blocked. The site also refuses automated requests. Without that permission, the tool doesn't talk to slow.pics. Instead:
> - Screenshots are named `{frame:06}_{label}.png` with the label as typed. slow.pics' batch upload groups files by the frame number and uses the label as the column title (`--open-slowpics` opens the folder and the page).
> - Every comparison gets an offline `index.html` viewer (side-by-side, slider, flicker, synced zoom/pan, keyboard navigation). Season runs also get an index page linking each episode.
>
> The flow below is kept for reference in case permission is granted.

This is an **unofficial** flow: the same HTTP calls the website makes, as reverse-engineered by vs-preview. It is isolated behind a trait so breakage only affects one module, and it uses a descriptive User-Agent.

1. `GET https://slow.pics/comparison` with a cookie jar, which sets the `XSRF-TOKEN` and session cookies.
2. `POST https://slow.pics/upload/comparison` (multipart), with headers `X-XSRF-TOKEN: <cookie value>`, `Origin`, `Referer: https://slow.pics/comparison`. Fields:
   - `collectionName`, `public` (false = unlisted), `hentai` (false), `optimizeImages`, `browserId` (UUIDv4, reused for the whole upload), optional `removeAfter`, optional `tmdbId`
   - per frame `j`: `comparisons[j].name` (e.g. `E03 – 12345`), and per source `i`: `comparisons[j].imageNames[i]` (group label)
   - Response JSON: `collectionUuid`, `key`, `images[j][i]` (image UUIDs)
3. For every image: `POST https://slow.pics/upload/image/{imageUuid}` (multipart: `collectionUuid`, `imageUuid`, `file` as `image/png`, `browserId`), with the same headers.
   Runs with limited concurrency (4–5), with exponential backoff and a **capped** number of retries (vs-preview retries forever every 60 s, which we won't do).
4. Result URL: `https://slow.pics/c/{key}`, shown, copied to the clipboard and written to `comp.json`.

- With several episodes, the options are one comparison per episode (default) or one combined comparison with names prefixed by episode.
- Settings exposed: collection name template (`{show} {ep} – {groups}`), unlisted/public, remove-after, TMDB id.
- Implementation: `reqwest` (rustls, cookies, multipart) on a small tokio runtime inside the core job runner.
- To verify in M3: size and count limits per comparison, and whether anonymous comparisons need `public=true`.

## 6. GUI (egui / eframe)

Single window, with a left navigation of steps you can go back and forth between:

1. **Sources** — one column per source group. Drop files or folders onto a column, rename the label, set per-group options (IVTC mode, matrix override). Indexing progress appears per file.
2. **Pairing** — an episode × group grid with auto-matched cells, warnings for gaps, and drag to reassign.
3. **Sync** — pick an episode, see the reference and another source side by side (or as a flicker/overlay/difference view). There's a frame scrubber, an offset spinner (±1, ±12, ±24), an "Auto-suggest" button with a confidence readout, and "apply to all episodes".
4. **Settings** — frames per episode, seed (with a 🎲 button), dark/transition thresholds, frame-type source and type, chroma kernel, PNG options, output dir.
5. **Run** — progress per episode and stage, cancel, with a log.
6. **Results** — a thumbnail grid per frame with click-to-zoom and a quick A/B flicker. "Upload to slow.pics" leads to the URL plus copy and open-in-browser buttons.

- All heavy work runs on worker threads. The UI gets `JobEvent`s over a crossbeam channel and calls `ctx.request_repaint()`.
- Preview textures use a downscaled RGB path through zimg's bilinear resize, used *only* for display and never for saved PNGs.
- Native file dialogs via `rfd`. Drag-and-drop via eframe's `dropped_files`.

## 7. CLI

```
comp index   <files…>                       # pre-build indexes
comp pair    --group "BD=/bd/*.mkv" --group "WEB=/web/*.mkv"   # print the pairing table
comp sync    project.toml --episode 3       # print suggested offsets + confidence
comp run     project.toml [--frames 20] [--seed 42] [--upload]
comp run     --group "BD=…" --group "WEB=…" --offset WEB=-24 --frames 15 --upload   # one-shot, no project file
comp upload  out/<comp>/                    # upload an existing output dir
```
Output is human-readable by default, with `--json` for scripting.

## 8. Dependencies and packaging

**Static native deps** (built by `deps/` scripts, cached as CI artifacts per target):
- **ffmpeg** (GPL build, trimmed with `--disable-everything` and then enabling only what's needed):
  - decoders: h264, hevc, mpeg2video, mpeg1video, vc1, vp9, av1 via **dav1d**, prores
  - demuxers/parsers: matroska, mov/mp4, mpegts (m2ts), mpegps (VOB), avi, ivf, h264/hevc elementary streams
  - filters: fieldmatch, decimate, bwdif, yadif, plus buffer/format plumbing
  - no encoders, no network, no programs
- **dav1d**, **ffms2** (static, built against the ffmpeg above), **zimg**
- Rust crates: eframe/egui, rfd, clap, serde/toml, reqwest (rustls), tokio, rand_chacha, png, oxipng, image_hasher (or a hand-rolled pHash), crossbeam, tracing, directories.

**Bundles:**
- macOS arm64: a `.app` with a single static binary, signed and notarized (needs an Apple Developer ID), shipped as a `.dmg`.
- Windows x64: MSVC toolchain, deps built with MSVC (or clang-cl). One `.exe` with a statically linked CRT, shipped as a zip (an MSI/Inno installer is optional).
- Linux x64: an AppImage, with the CLI also as a plain static-ish binary. Built on an older glibc base (e.g. manylinux_2_28) for compatibility.
- Both binaries (`comp` and `anime-comp`) go in each bundle, with the CLI optional on macOS.

**CI:** a GitHub Actions matrix (macos-15 arm64, windows-2022, ubuntu-22.04 or a manylinux container). Jobs: deps (cached by script hash) → `cargo test` → build → package → release on tag.
Licence: ship `LICENSE` (GPLv3) and the third-party notices for ffmpeg, ffms2, zimg and dav1d.

## 9. Milestones

| # | Milestone | Done when |
|---|---|---|
| M0 | **Deps + FFI spike** | ffmpeg/ffms2/zimg build statically on all 3 targets in CI. A Rust test decodes frame N via ffms2, converts it via zimg and writes a PNG. The seek-vs-sequential hash test passes on the sample corpus. |
| M1 | **Core pipeline (CLI)** | `comp run` with 2+ progressive sources, manual offsets and plain seeded random frames writes the correct PNGs at native resolution. Colour matrix fallbacks are tested. |
| M2 | **Selection + pairing** ✅ | Stratified sampling, dark/transition/near-duplicate rejection, frame-type preference, episode parser with a test table, multi-episode runs, project file. OP/ED exclusion from manual ranges and chapters. |
| M3 | **slow.pics upload** (on hold: needs site permission; offline viewer + manual-upload layout shipped instead) | Anonymous upload creates a working comparison, with retries and a cap. `comp upload` works on existing output. Limits are documented. |
| M4 | **GUI** ✅ (egui; Sources+Settings / Sync / Run; season pairing; TOML projects; macOS .app via packaging/macos/bundle.sh) | Sources → Pairing → Sync (manual offset) → Settings → Run → Results → Upload, all working end to end. |
| M5 | **Auto-suggest offsets** ✅ (cut voting + fine correlation search, border crop, confidence vs wrong-offset baseline, ±N-frame precision; `comp sync`, GUI Auto/Apply). Cross-episode OP/ED repeat detection deferred: OP/ED exclusion is opt-in and chapters cover the common case. Finding: two releases can drift 1–2 frames against each other within an episode (To Heart BD vs DVD), so per-scene offsets are a possible follow-up | Correct offset found on test pairs (WEB vs BD with a logo offset, DVD vs BD at different resolutions), with a confidence score that clearly flags wrong matches. Cross-episode OP/ED repeat detection (it reuses the same pHash sequences). |
| M6 | **IVTC / deinterlace** ✅ implemented early (real BD sources are often 1080i telecined); still to do: real-remux validation, auto-detecting telecine vs interlaced | A telecined NTSC DVD source produces clean progressive frames. Output-timeline frame N is deterministic (windowed result = sequential result) and is cached. |
| M7 | **Packaging + release** ✅ scripts + release workflow (macOS .app/.dmg verified locally; Linux AppImage verified in manylinux_2_28 → Ubuntu 24.04, arm64; Windows zip with DLL-import check, CI-only). Developer ID signing/notarization wired but needs Apple secrets | Signed .app/.dmg, Windows zip, AppImage built from a tag, each tested on a clean machine or VM. |

M5 and M6 are independent of M4 and can be built in parallel once M2 lands.

## 10. Risks

- **IVTC random access** (M6): windowed fieldmatch/decimate may disagree with sequential processing near cycle boundaries. Mitigation: the analysis pass verifies it, and per-frame fallback to a wider window. Worst case: decode sequentially through the selected frames in order (slower, still correct).
- **slow.pics breakage:** it's an unofficial API. Mitigation: isolated module, clear error messages, and the local output is always kept so you can upload manually.
- **Static ffmpeg/ffms2 on Windows:** the most build-fragile target. Mitigation: do it first in M0. vcpkg is a fallback for ffmpeg/zimg.
- **macOS notarization:** needs a paid Apple Developer account. Without one, users have to right-click → Open.
- **Colour metadata lies:** some WEB-DLs tag the wrong matrix or range. Mitigation: per-source overrides, plus a visible "matrix used" label in the results.

## 11. Resolved questions

- **Anamorphic DVD:** stretch the width to the display aspect ratio (section 3.3).
- **OP/ED:** left out of picks by default, using manual ranges → chapters → cross-episode repeat detection (section 4.3.1).
- **HDR/WCG:** out of scope.
- **Hybrids:** they're ordinary encoded files (e.g. someone's encode built from both WEB and BD), compared like any other source. No VapourSynth integration needed.
