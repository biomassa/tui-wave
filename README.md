# tui-wave

![tui-wave screenshot](screenshot1.png)

A keyboard-driven **terminal audio editor** for Linux, macOS, and Windows, written in Rust (mouse works too). It opens WAV files and gives you a zoomable waveform you can navigate,
play, and edit entirely from the keyboard: selection, cut/copy/paste, undo/redo, normalize, gain, fades, reverse, trim, resample, mix-to-mono, and timeline markers with transient
detection — plus a menu bar, a context-aware toolbar, fully configurable keyboard shortcuts via .toml config file, a file browser, and multi-buffer editing. Edits are saved back to WAV at 16-bit, 24-bit, or 32-bit float, and BWF cue points / markers are preserved across the round trip. A dedicated command chops a file at its markers and exports each region as a separate WAV.

Press `F10` (or `Alt`+a menu letter) to open the menus, `Tab` / `Shift+Tab` to move focus
between the waveform, the file list, and the open buffers, and `q` to quit.

See [USERGUIDE.md](USERGUIDE.md) for the full keybinding reference and workflow tips.

## Status and Disclaimer
This is developed with the assistance of LLM. I am not a Rust developer, however I have certain expertise in working with audio files. I needed this instrument for my own work.

## Prerequisites

- **Rust toolchain** (the `cargo` build tool), version **1.85 or newer** — the project uses
  the 2024 edition. Install it from <https://rustup.rs>:

  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

  (On Windows, download and run `rustup-init.exe` from the same site instead.)

- **An audio output device** is optional — without one you can still view and edit
  waveforms, you just won't hear playback.

### Platform-specific build dependencies

- **Linux:** the audio backend needs the ALSA development headers.
  - Debian/Ubuntu: `sudo apt install libasound2-dev pkg-config`
  - Fedora: `sudo dnf install alsa-lib-devel pkg-config`
  - Arch: `sudo pacman -S alsa-lib pkgconf`
- **macOS:** nothing extra — uses the system CoreAudio framework.
- **Windows:** nothing extra — uses the system WASAPI backend.

## Install, compile, and run

Clone the repository and build with Cargo. The commands are the same on all three
platforms (use PowerShell or Windows Terminal on Windows).

```sh
git clone <repository-url> tui-wave
cd tui-wave

# Build an optimized binary (recommended — debug builds are noticeably slower on
# large files because of the one-time waveform-cache build).
cargo build --release

# Run the compiled binary directly:
#   Linux/macOS:
./target/release/tui-wave path/to/audio.wav
#   Windows:
#   .\target\release\tui-wave.exe path\to\audio.wav
```

Running with no file argument opens an empty editor — focus the file panel with `Tab`,
browse, and press `Enter` on a `.wav` to open it. You can also point it at a directory
(`tui-wave path/to/folder`) to start browsing there.

To install it onto your `PATH` so you can call `tui-wave` from anywhere:

```sh
cargo install --path .
```

## Development

```sh
cargo build      # debug build
cargo test       # run the test suite
```

A reasonably large terminal is recommended (≈120×40 or more) so the file and buffer side
panels and the dB gutters all fit.

## Packaging (AppImage)

A Linux AppImage can be built with [`cargo-appimage`](https://crates.io/crates/cargo-appimage)
(`appimagetool` must be on `PATH`):

```sh
./packaging/build-appimage.sh
# -> dist/tui-wave-<version>-<arch>.AppImage   (e.g. dist/tui-wave-0.1.0-x86_64.AppImage)
```

The wrapper runs `cargo appimage` and names the output with the version and target
architecture. The `.desktop` entry sets `Terminal=true` (it's a terminal app), and
`libasound.so.2` is bundled so audio works without a system ALSA runtime.
