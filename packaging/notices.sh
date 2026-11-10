#!/usr/bin/env bash
# Writes THIRD_PARTY_NOTICES.md: licences of the statically linked native libraries (from their
# source trees in deps/build) and of every Rust crate in the build (from cargo metadata).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT=THIRD_PARTY_NOTICES.md
# Licence files: from the deps prefix (copied there by deps/build.sh), else from deps/build.
P="${ANIME_COMP_DEPS:-deps/prefix}/share/licenses"
lic() { if [[ -f "$P/$1/$2" ]]; then cat "$P/$1/$2"; else cat "deps/build/$1/$2"; fi; }
{
  echo "# Third-party notices"
  echo
  echo "Anime Comparisons (anime-comp) is distributed under the GNU General Public License v3 (see LICENSE). It statically links the"
  echo "libraries below. Their licences are reproduced here."
  echo
  echo "## Native libraries"
  echo
  echo "| Library | Version | Licence |"
  echo "|---|---|---|"
  echo "| FFmpeg (libavcodec, libavformat, libavfilter, libavutil, libswscale, libswresample) | $(sed -n 's/^FFMPEG_VER=//p' deps/build.sh) | GPL-3.0-or-later (built with --enable-gpl --enable-version3) |"
  echo "| FFMS2 | $(sed -n 's/^FFMS2_VER=//p' deps/build.sh) | MIT |"
  echo "| zimg | $(sed -n 's/^ZIMG_VER=//p' deps/build.sh) | WTFPL |"
  echo "| dav1d | $(sed -n 's/^DAV1D_VER=//p' deps/build.sh) | BSD-2-Clause |"
  echo
  echo "FFmpeg source: https://ffmpeg.org/releases/ (exact tarball and SHA-256 in deps/build.sh)."
  echo
  for lib in ffms2 zimg dav1d; do
    echo "### $lib"
    echo
    echo '```'
    lic "$lib" COPYING
    echo '```'
    echo
  done
  echo "### FFmpeg"
  echo
  echo '```'
  lic ffmpeg LICENSE.md
  echo '```'
  echo
  echo "## Fonts"
  echo
  echo "IBM Plex Sans, IBM Plex Mono and IBM Plex Sans JP (© IBM Corp., https://github.com/IBM/plex), embedded in"
  echo "the desktop app, are licensed under the SIL Open Font License 1.1:"
  echo
  echo '```'
  tr -d '\r' < crates/comp-gui/fonts/OFL.txt
  echo '```'
  echo
  echo "## Rust crates"
  echo
  echo "| Crate | Version | Licence | Repository |"
  echo "|---|---|---|---|"
  cargo metadata --format-version 1 --locked 2>/dev/null | python3 -c '
import json, sys
m = json.load(sys.stdin)
ws = set(m["workspace_members"])
# Only crates actually in the dependency graph of workspace members (normal + build deps).
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
seen, stack = set(), list(ws)
while stack:
    i = stack.pop()
    if i in seen: continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
            stack.append(d["pkg"])
pk = {p["id"]: p for p in m["packages"]}
rows = sorted((pk[i]["name"], pk[i]["version"], pk[i].get("license") or "see repository", pk[i].get("repository") or "") for i in seen if i not in ws)
for r in rows:
    print("| %s | %s | %s | %s |" % r)
'
  echo
  echo "Full licence texts for the crates are in each crate's source (see Cargo.lock for exact versions)."
} > "$OUT"
echo "wrote $OUT ($(grep -c '^| ' "$OUT") table rows)"
