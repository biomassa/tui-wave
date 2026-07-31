# tui-wave

A keyboard-driven audio editor that runs in a terminal.

![tui-wave screenshot](screenshot1.png)
![tui-wave screenshot](screenshot2.png)

tui-wave draws waveforms, plays audio, and edits samples. It handles files with one channel,
two channels, or more than fifty channels. It also opens files of 20GB or more without loading
them into memory.

Read [documentation.md](documentation.md) to learn how to use it.

## What it does

- **Waveform display.** Zoom from the whole file down to single samples. Terminals such as
  Kitty get a real image. Every other terminal gets braille and block glyphs.
- **Playback.** Play, pause, and loop. The view can follow the play position.
- **Editing.** Cut, copy, paste, delete, and undo, with a separate undo stack per open file.
- **Processing.** Reverse, normalize, gain, fade, trim, resample, technical fades, and mix to
  mono.
- **Markers.** Insert markers by hand or at transients. tui-wave saves them as WAV cue points,
  which Audacity and Sound Forge read.
- **Many channels.** Scroll through the channels of a 58-channel file. Drop the empty ones.
  Split the rest into mono files or stereo pairs.
- **Large files.** A file above the memory limit opens read-only and disk-backed. tui-wave
  reads and writes RF64 and BW64.
- **Formats.** It reads WAV, FLAC, and AIFF. It writes WAV, FLAC, and MP3.
- **Configurable.** Every key assignment lives in a TOML config file.
- **CDP.** An optional front end to the Composer's Desktop Project, with about 130 processes.

The CDP process browser, the parameter form with automatable green fields and presets, and the
breakpoint envelope editor:

<p>
  <img src="CDP1.png" alt="CDP process browser" width="32%" />
  <img src="CDP2.png" alt="CDP parameter form with presets and automatable fields" width="32%" />
  <img src="CDP3.png" alt="CDP breakpoint envelope editor" width="32%" />
</p>

## Status

An LLM helped to write this program. I am not a Rust developer. I do know audio files, and I
needed this tool for my own work.

Release builds for Linux exist. A build from source gives you the most complete program.

## Prerequisites

You need the Rust toolchain, version 1.85 or newer. The project uses the 2024 edition. Install
it from <https://rustup.rs>:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

On Windows, download and run `rustup-init.exe` from the same site.

An audio output device is optional. Without one you can still view and edit waveforms. You just
hear nothing.

### Build dependencies per platform

- **Linux.** The audio backend needs the ALSA development headers.
  - Debian and Ubuntu: `sudo apt install libasound2-dev pkg-config`
  - Fedora: `sudo dnf install alsa-lib-devel pkg-config`
  - Arch: `sudo pacman -S alsa-lib pkgconf`
- **macOS.** Nothing extra. The program uses the system CoreAudio framework.
- **Windows.** Nothing extra. The program uses the system WASAPI backend.

## Build and run

Clone the repository, then build with Cargo. The commands work the same on all three platforms.
On Windows, use PowerShell or Windows Terminal.

```sh
git clone <this repository>
cd tui-wave
cargo build --release
```

Always build with `--release`. A debug build draws long files many times slower, because
tui-wave builds a waveform summary once per file.

Run the binary directly:

```sh
./target/release/tui-wave path/to/audio.wav
```

On Windows the path reads `.\target\release\tui-wave.exe`.

Use a terminal window of about 120 by 40 characters or larger. That leaves room for the side
panels and the decibel gutters.

## First steps

1. Start the program with an audio file, or with a directory to browse.
2. Press `Tab` to move focus between the Waveform, Files, and Buffers panels.
3. Press the Up and Down arrows to zoom. Press Left and Right to move the cursor.
4. Hold `Shift` and press Left or Right to select audio.
5. Press `Space` to play.
6. Press `F10` to open the menu bar.
7. Press `q` to quit.

[documentation.md](documentation.md) covers the rest.

## Optional: CDP support

CDP is the Composer's Desktop Project, a large set of offline sound transformation programs.
It covers spectral blurring, granulation, morphing, waveset distortion, time stretching, and
hundreds more. tui-wave gives you a dialog-driven front end to it, at `Ctrl+p` or through the
CDP menu. Browse the catalog, edit parameters, draw breakpoint curves, preview through your
speakers, and apply with full undo.

CDP stays optional. tui-wave works fully without it. The feature waits until you point it at a
CDP directory, and a first-use prompt explains why.

tui-wave looks in `~/cdp` by default. Unpack or build CDP there and the program finds it with no
setup. Anywhere else, answer the prompt with the real path. You can change the path later
through CDP then Configure CDP Directory. tui-wave saves it as `cdp_dir` in your config file.

### About CDP

Four composers founded the Composer's Desktop Project in 1986 in Yorkshire, UK: Andrew Bentley,
Archer Endrich, Richard Orton, and Trevor Wishart. They wanted to bring sound transformation
power from institutional mainframes to a personal desktop.

CDP has been free and open-source software since 2014, under the
[GNU LGPL 2.1+](https://github.com/ComposersDesktop/CDP8/blob/main/LICENSE). Work on it
continues. CDP8, from 2023, added about 80 processes over CDP7.

All credit for CDP, and for the roughly 250 command-line programs tui-wave calls, belongs to the
Composer's Desktop Project. tui-wave neither bundles nor redistributes any CDP binary.

The process catalog in tui-wave mostly adapts parameter names, ranges, and descriptions from
[SoundThread](https://github.com/j-p-higgins/SoundThread) by Jonathan Higgins, under the MIT
license. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the full text. A growing set
of hand-written entries covers CDP programs that SoundThread never reached.

### Installing CDP

CDP does not go on your `PATH`, and no package manager carries it. Download it or build it
yourself, then tell tui-wave where the binaries live.

- **Windows and macOS.** Download a prebuilt release from
  <https://www.unstablesound.net/cdp.html>, the official CDP download mirror. Unzip or mount it
  anywhere. The binaries land in a folder such as `_cdprogs` or `NewRelease`.
- **Linux.** CDP offers no prebuilt Linux binaries. Build from source:

  ```sh
  git clone https://github.com/ComposersDesktop/CDP8.git
  cd CDP8
  mkdir build && cd build
  cmake ..
  make
  ```

  This needs `cmake` and a C compiler on your `PATH`. The CDP repository holds its own
  [building.txt](https://github.com/ComposersDesktop/CDP8/blob/main/building.txt) with notes per
  platform. The compiled binaries land in a top-level `NewRelease` directory.

The older [CDP7](https://github.com/ComposersDesktop/CDP7) source builds the same way and works
just as well. The tui-wave catalog does not depend on one CDP release.

## Development

```sh
cargo build      # debug build
cargo test       # run the test suite
```

`CHANGELOG.md` records what changed in each version. `MANUAL_TESTING.md` holds the checklist for
the parts that no test can cover, such as real audio hardware and real terminal quirks.

## Packaging

The scripts under `packaging/` build Linux packages into `dist/`. Each package carries the same
`Terminal=true` desktop entry and 512 by 512 icon. Each file name carries the version and the
target architecture.

```sh
./packaging/build-appimage.sh   # -> dist/tui-wave-<ver>-<arch>.AppImage
./packaging/build-pkg.sh        # -> dist/tui-wave-<ver>-1-<arch>.pkg.tar.zst
./packaging/build-deb.sh        # -> dist/tui-wave_<ver>_amd64.deb
```

- **AppImage.** Built with [cargo-appimage](https://crates.io/crates/cargo-appimage), which
  needs `appimagetool` on your `PATH`. It bundles `libasound.so.2`, so audio works without a
  system ALSA.
- **Arch.** `makepkg` wraps the release binary. It depends on `gcc-libs` and `alsa-lib`.
- **Debian.** Assembled with `ar` and `tar`, so you do not need `dpkg-deb`. It depends on
  `libc6`, `libgcc-s1`, and `libasound2`.

The native packages link against the glibc of the build machine. To target an older system,
build inside a matching container.
