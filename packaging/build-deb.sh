#!/usr/bin/env bash
# Build a Debian package (.deb) from the release binary, assembled with `ar`+`tar`
# (no dpkg-deb needed). Output: dist/tui-wave_<version>_amd64.deb
# Install with: sudo apt install ./dist/tui-wave_<version>_amd64.deb   (or dpkg -i)
#
# NOTE: the binary is compiled against this machine's glibc; for a .deb that runs on
# older Debian/Ubuntu, build it inside a matching container.
set -euo pipefail

cd "$(dirname "$0")/.."
repo="$PWD"
version="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
arch=amd64   # Debian's name for x86_64

cargo build --release

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
root="$work/root"

install -Dm755 target/release/tui-wave        "$root/usr/bin/tui-wave"
strip "$root/usr/bin/tui-wave" 2>/dev/null || true
install -Dm644 packaging/tui-wave.desktop     "$root/usr/share/applications/tui-wave.desktop"
install -Dm644 icon.png                       "$root/usr/share/icons/hicolor/512x512/apps/tui-wave.png"
install -Dm644 LICENSE                         "$root/usr/share/doc/tui-wave/copyright"

installed_kb=$(du -sk "$root" | cut -f1)

# control + md5sums (paths relative to the install root, no leading ./)
cat > "$work/control" <<EOF
Package: tui-wave
Version: $version
Architecture: $arch
Maintainer: biomassa <noreply@users.noreply.github.com>
Installed-Size: $installed_kb
Depends: libc6, libgcc-s1, libasound2
Section: sound
Priority: optional
Homepage: https://github.com/biomassa/tui-wave
Description: Keyboard-driven terminal WAV editor
 A zoomable waveform TUI audio editor: playback, selection/cut/copy/paste/undo,
 timeline markers, and 16/24/32-bit WAV load/save.
EOF
( cd "$root" && find . -type f -printf '%P\0' | xargs -0 md5sum ) > "$work/md5sums"

# Assemble the three ar members (order matters: debian-binary, control, data).
echo "2.0" > "$work/debian-binary"
tar -C "$work" --owner=0 --group=0 --numeric-owner -czf "$work/control.tar.gz" control md5sums
tar -C "$root" --owner=0 --group=0 --numeric-owner -cJf "$work/data.tar.xz" .

mkdir -p "$repo/dist"
out="$repo/dist/tui-wave_${version}_${arch}.deb"
rm -f "$out"
( cd "$work" && ar rc "$out" debian-binary control.tar.gz data.tar.xz )
echo "deb package: $out"
