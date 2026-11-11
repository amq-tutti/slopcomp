#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md (without its heading), for release notes.
#   packaging/changelog-notes.sh 0.2.0
#   packaging/changelog-notes.sh Unreleased
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:?usage: $0 VERSION}"
VERSION="${VERSION#v}"

awk -v v="$VERSION" '
  /^## \[/ { if (found) exit; if (index($0, "## [" v "]") == 1) { found = 1; next } }
  /^\[[^]]+\]: / { if (found) exit }
  found { lines[++n] = $0 }
  END {
    if (!found) { print "No section for " v " in CHANGELOG.md" > "/dev/stderr"; exit 1 }
    first = 1; while (first <= n && lines[first] ~ /^[[:space:]]*$/) first++
    last = n;  while (last >= first && lines[last] ~ /^[[:space:]]*$/) last--
    for (i = first; i <= last; i++) print lines[i]
  }
' CHANGELOG.md
