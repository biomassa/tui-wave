# tui-wave

A keyboard-driven **terminal audio editor** for Linux, macOS, and Windows, written in Rust.
It opens WAV files and gives you a zoomable waveform you can navigate, play, and edit
entirely from the keyboard: selection, cut/copy/paste, undo/redo, normalize, gain, fades,
reverse, trim, resample, and timeline markers — plus a menu bar, a context-aware toolbar,
a file browser, and multi-buffer editing. Edits are saved back to WAV (16/24-bit int or
32-bit float), and BWF cue points / broadcast metadata are preserved.

Press `F10` (or `Alt`+a menu letter) to open the menus, `Tab` to move focus between the
waveform, the file list, and the open buffers, and `q` to quit.

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
browse, and press `Enter` on a `.wav` to open it.

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
