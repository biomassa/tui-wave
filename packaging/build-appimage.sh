#!/usr/bin/env bash
# Build the release AppImage and emit it with the target architecture in the filename:
#
#     dist/tui-wave-<version>-<arch>.AppImage   (e.g. dist/tui-wave-0.1.0-x86_64.AppImage)
#
# cargo-appimage always writes target/appimage/tui-wave.AppImage (named after the crate),
# so this wrapper copies that to the versioned, arch-tagged name a GitHub release wants.
# Any arguments are forwarded to `cargo appimage` (e.g. --features=foo).
set -euo pipefail

cd "$(dirname "$0")/.."

# Let appimagetool run without FUSE (harmless where FUSE exists; needed in CI/containers).
APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" cargo appimage "$@"

built="target/appimage/tui-wave.AppImage"
[ -f "$built" ] || { echo "error: $built was not produced" >&2; exit 1; }

version="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
arch="$(uname -m)"
out="dist/tui-wave-${version}-${arch}.AppImage"

mkdir -p dist
cp -f "$built" "$out"
chmod +x "$out"
echo "AppImage: $out"
