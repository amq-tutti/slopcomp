#!/usr/bin/env bash
# Cut a release: move CHANGELOG.md's Unreleased section under VERSION, set the version in
# Cargo.toml, commit, tag vVERSION and (after you confirm) push. The tag starts
# .github/workflows/release.yml, which builds macOS, Linux and Windows and opens a draft
# GitHub release with the changelog section as its notes. Publish the draft on GitHub.
#
#   packaging/release.sh 0.2.0
set -euo pipefail
cd "$(dirname "$0")/.."

die() { echo "release: $*" >&2; exit 1; }

VERSION="${1:-}"
VERSION="${VERSION#v}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] \
  || die "usage: $0 VERSION   (e.g. 0.2.0 or 0.2.0-beta.1)"
TAG="v$VERSION"

# ------------------------------------------------------------------ checks
BRANCH=$(git symbolic-ref --short HEAD)
[[ "$BRANCH" == master ]] || die "on branch '$BRANCH'; releases are cut from master"
git diff --quiet && git diff --cached --quiet \
  || die "uncommitted changes to tracked files; commit or stash them first"

git fetch --quiet --tags origin
[[ -z "$(git rev-list HEAD..origin/master 2>/dev/null)" ]] \
  || die "master is behind origin/master; pull first"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "tag $TAG already exists"

CURRENT=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[[ -n "$CURRENT" ]] || die "couldn't read the version from Cargo.toml"
LOWEST=$(printf '%s\n%s\n' "$CURRENT" "$VERSION" | sort -V | head -1)
[[ "$LOWEST" == "$CURRENT" ]] || die "$VERSION is lower than the current version $CURRENT"

PREV_TAG=$(git tag --list 'v*' --sort=-v:refname | head -1)
if [[ -n "$PREV_TAG" ]]; then
  LOWEST=$(printf '%s\n%s\n' "${PREV_TAG#v}" "$VERSION" | sort -V | head -1)
  [[ "$LOWEST" == "${PREV_TAG#v}" ]] || die "$VERSION is lower than the latest tag $PREV_TAG"
fi

NOTES=$(packaging/changelog-notes.sh Unreleased)
[[ -n "$NOTES" ]] || die "CHANGELOG.md has nothing under Unreleased"

REMOTE=$(git remote get-url origin)
REPO_URL=${REMOTE%.git}
REPO_URL=${REPO_URL/git@github.com:/https://github.com/}

# ------------------------------------------------------------------ changes
restore() { git checkout -- CHANGELOG.md Cargo.toml Cargo.lock; }
trap 'restore; die "failed; files restored"' ERR

DATE=$(date +%Y-%m-%d)
if [[ -n "$PREV_TAG" ]]; then
  VERSION_LINK="$REPO_URL/compare/$PREV_TAG...$TAG"
else
  VERSION_LINK="$REPO_URL/releases/tag/$TAG"
fi
awk -v v="$VERSION" -v d="$DATE" -v ul="$REPO_URL/compare/$TAG...HEAD" -v vl="$VERSION_LINK" '
  $0 == "## [Unreleased]" { print; print ""; print "## [" v "] - " d; next }
  /^\[Unreleased\]: / { print "[Unreleased]: " ul; print "[" v "]: " vl; next }
  { print }
' CHANGELOG.md > CHANGELOG.md.new
mv CHANGELOG.md.new CHANGELOG.md

if [[ "$VERSION" != "$CURRENT" ]]; then
  # The first `version = ` line is [workspace.package]; every crate inherits it.
  awk -v v="$VERSION" '!done && /^version = "/ { print "version = \"" v "\""; done = 1; next } { print }' \
    Cargo.toml > Cargo.toml.new
  mv Cargo.toml.new Cargo.toml
  cargo update --workspace --offline --quiet
fi
trap - ERR

# ------------------------------------------------------------------ confirm
echo "Release $TAG ($CURRENT -> $VERSION), notes:"
echo "------------------------------------------------------------------"
echo "$NOTES"
echo "------------------------------------------------------------------"
git --no-pager diff --stat
read -r -p "Commit, tag $TAG and push to origin? [y/N] " ANSWER
if [[ ! "$ANSWER" =~ ^[Yy]$ ]]; then
  restore
  echo "Nothing changed."
  exit 0
fi

git add CHANGELOG.md Cargo.toml Cargo.lock
git commit --quiet -m "Release $TAG"
git tag -a "$TAG" -m "SlopComp $VERSION"
git push --atomic origin master "$TAG"

echo
echo "Pushed $TAG. The builds take a while; the draft release appears when all three finish:"
echo "  $REPO_URL/actions"
echo "  $REPO_URL/releases"
