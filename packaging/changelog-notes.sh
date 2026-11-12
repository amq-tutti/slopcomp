#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md (without its heading), for release notes.
# Wrapped lines are joined: GitHub shows every newline in release notes as a line break, so a
# bullet wrapped at 100 columns in the changelog would break mid-sentence there.
#   packaging/changelog-notes.sh 0.2.0
#   packaging/changelog-notes.sh Unreleased
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:?usage: $0 VERSION}"
VERSION="${VERSION#v}"

awk -v v="$VERSION" '
  /^## \[/ { if (found) exit; if (index($0, "## [" v "]") == 1) { found = 1; next } }
  /^\[[^]]+\]: / { if (found) exit }
  # A line indented under a bullet or a paragraph line continues it.
  found && n > 0 && /^[[:space:]]+[^[:space:]]/ && lines[n] !~ /^[[:space:]]*$/ && lines[n] !~ /^#/ {
    sub(/^[[:space:]]+/, ""); lines[n] = lines[n] " " $0; next
  }
  found { lines[++n] = $0 }
  END {
    if (!found) { print "No section for " v " in CHANGELOG.md" > "/dev/stderr"; exit 1 }
    first = 1; while (first <= n && lines[first] ~ /^[[:space:]]*$/) first++
    last = n;  while (last >= first && lines[last] ~ /^[[:space:]]*$/) last--
    for (i = first; i <= last; i++) print lines[i]
  }
' CHANGELOG.md
