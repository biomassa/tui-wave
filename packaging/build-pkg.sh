#!/usr/bin/env bash
# Build an Arch Linux package (.pkg.tar.zst) from the release binary, via makepkg.
# Output: dist/tui-wave-<version>-1-<arch>.pkg.tar.zst
# Install with: sudo pacman -U dist/tui-wave-*.pkg.tar.zst
set -euo pipefail

cd "$(dirname "$0")/.."
repo="$PWD"
version="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"

cargo build --release

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp target/release/tui-wave      "$work/tui-wave"
cp packaging/tui-wave.desktop   "$work/tui-wave.desktop"
cp icon.png                     "$work/icon.png"
cp LICENSE                      "$work/LICENSE"

cat > "$work/PKGBUILD" <<EOF
# Maintainer: biomassa
pkgname=tui-wave
pkgver=$version
pkgrel=1
pkgdesc="Keyboard-driven terminal WAV editor"
arch=('x86_64')
url="https://github.com/biomassa/tui-wave"
license=('MIT')
depends=('gcc-libs' 'alsa-lib')
source=('tui-wave' 'tui-wave.desktop' 'icon.png' 'LICENSE')
sha256sums=('SKIP' 'SKIP' 'SKIP' 'SKIP')
package() {
  install -Dm755 "\$srcdir/tui-wave"          "\$pkgdir/usr/bin/tui-wave"
  install -Dm644 "\$srcdir/tui-wave.desktop"  "\$pkgdir/usr/share/applications/tui-wave.desktop"
  install -Dm644 "\$srcdir/icon.png"          "\$pkgdir/usr/share/icons/hicolor/512x512/apps/tui-wave.png"
  # MIT isn't in /usr/share/licenses/common, so the text must ship with the package.
  install -Dm644 "\$srcdir/LICENSE"           "\$pkgdir/usr/share/licenses/tui-wave/LICENSE"
}
EOF

( cd "$work" && makepkg -f )

mkdir -p "$repo/dist"
cp "$work"/tui-wave-*.pkg.tar.zst "$repo/dist/"
echo "pacman package: $(ls "$repo"/dist/tui-wave-*.pkg.tar.zst)"
