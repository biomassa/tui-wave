#!/usr/bin/env bash
# Cut a release: verify, build all three packages, and publish them to GitHub.
#
#     bash packaging/release.sh            # build + publish
#     bash packaging/release.sh --dry-run  # build only, print what would be published
#
# Deliberately refuses rather than fixes. Every check below exists because getting it wrong
# produces a release that is quietly not what it claims to be — a tag pointing at different
# code than the artifacts, or artifacts built from uncommitted changes. Bumping the version,
# writing the CHANGELOG entry, committing and tagging stay manual (see PROCEDURE at the
# bottom); this script's job is to make sure that was all done before anything is published.
set -euo pipefail

cd "$(dirname "$0")/.."
repo="$PWD"

dry_run=0
[ "${1:-}" = "--dry-run" ] && dry_run=1

die() { echo "error: $*" >&2; exit 1; }
step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

version="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
tag="v$version"
[ -n "$version" ] || die "could not read version from Cargo.toml"

step "Releasing $tag"

# ---- Preflight -----------------------------------------------------------------------
# A dirty tree means the artifacts would contain code that is in no commit, so nobody could
# ever reproduce them.
[ -z "$(git status --porcelain)" ] || die "working tree is dirty — commit or stash first"

# The tag must exist and point at HEAD. This is the check that was missing when v1.6.0 was
# first cut: the tag was pushed, a bug fix landed after it, and the artifacts would have been
# built from code the tag did not describe.
git rev-parse -q --verify "refs/tags/$tag" >/dev/null \
  || die "tag $tag does not exist — bump Cargo.toml, commit, then: git tag -a $tag -m 'tui-wave $version'"
[ "$(git rev-parse "$tag^{commit}")" = "$(git rev-parse HEAD)" ] \
  || die "tag $tag does not point at HEAD — the artifacts would not match the tag"

# Cargo.lock has to carry the bumped version too, or a `cargo build --locked` elsewhere fails.
grep -A1 '^name = "tui-wave"$' Cargo.lock | grep -q "^version = \"$version\"$" \
  || die "Cargo.lock still has the old version — run: cargo update -p tui-wave --offline"

grep -q "Bumped version to $version" CHANGELOG.md \
  || die "CHANGELOG.md has no entry for $version"

if [ "$dry_run" -eq 0 ]; then
  gh auth status >/dev/null 2>&1 || die "gh is not authenticated — run: gh auth login"
  ! gh release view "$tag" -R biomassa/tui-wave >/dev/null 2>&1 \
    || die "release $tag already exists — delete it first, or use 'gh release upload --clobber' by hand"
  # Local-only tags are a common miss: the release would reference a tag nobody else can see.
  git ls-remote --tags origin "refs/tags/$tag" | grep -q . \
    || die "tag $tag is not pushed — run: git push origin $tag"
fi

step "Running the test suite"
cargo test --quiet

step "Building packages"
rm -rf dist
bash packaging/build-appimage.sh
bash packaging/build-deb.sh
bash packaging/build-pkg.sh

# Each package must actually contain this version's binary, not a stale one left in target/.
step "Verifying artifacts"
artifacts=(
  "dist/tui-wave-${version}-$(uname -m).AppImage"
  "dist/tui-wave_${version}_amd64.deb"
  "dist/tui-wave-${version}-1-$(uname -m).pkg.tar.zst"
)
for a in "${artifacts[@]}"; do
  [ -f "$a" ] || die "expected artifact missing: $a"
  printf '  %-46s %s\n' "$(basename "$a")" "$(du -h "$a" | cut -f1)"
done
# The AppImage is the only one that is directly runnable without unpacking; a non-zero exit
# on a missing file is the app's own error path, so reaching it proves the binary executes.
"./${artifacts[0]}" /nonexistent-preflight-check.wav >/dev/null 2>&1 && die "AppImage did not run"

# ---- Publish -------------------------------------------------------------------------
# Release notes are the newest CHANGELOG section: everything between the first two `## `
# headings, minus the trailing "Bumped version to ..." line, which is noise in a release body.
notes="$(mktemp)"; trap 'rm -f "$notes"' EXIT
awk '/^## /{n++} n==1 && !/^## /' CHANGELOG.md | sed '/^- Bumped version to /d' > "$notes"
[ -s "$notes" ] || die "could not extract release notes from CHANGELOG.md"

if [ "$dry_run" -eq 1 ]; then
  step "Dry run — would publish $tag with these notes"
  cat "$notes"
  exit 0
fi

step "Publishing $tag"
gh release create "$tag" -R biomassa/tui-wave \
  --title "tui-wave $version" --notes-file "$notes" "${artifacts[@]}"

step "Done"
gh release view "$tag" -R biomassa/tui-wave --json url --jq .url

# PROCEDURE (the manual half, deliberately not automated — each step is a judgement call)
#
#   1. Bump `version` in Cargo.toml
#   2. cargo update -p tui-wave --offline      # carries it into Cargo.lock
#   3. Add a dated CHANGELOG.md section ending with "- Bumped version to X.Y.Z."
#   4. git commit -am "Bump version to X.Y.Z"
#   5. git tag -a vX.Y.Z -m "tui-wave X.Y.Z"
#   6. git push origin master && git push origin vX.Y.Z
#   7. bash packaging/release.sh
