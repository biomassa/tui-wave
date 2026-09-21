# tui-wave

A keyboard-driven audio editor that runs in a terminal (mouse works too!).

![tui-wave screenshot](screenshot1.png)
![tui-wave screenshot](screenshot2.png)

tui-wave draws waveforms, plays and edits audio. It handles mono, stereo and multichannel
files. A file larger than the size threshold (4GB by default, and configurable) opens in
streaming mode and never loads into memory. That mode is what lets you audition, cut and
process the large session captures that software such as Cycling74's Max writes.

tui-wave is also a front end for three process backends. Two of them are optional and you
install them yourself:

- **Composer's Desktop Project (CDP)**, a set of command-line utilities that have a history of
  decades. They do time-domain and frequency-domain work you find nowhere else. Andrew Bentley,
  Archer Endrich, Richard Orton and Trevor Wishart founded the project in 1986.
- **praatAudioTools**, 457 sound-transformation scripts for Praat by Shai Cohen.
- **Airwindows**, 500 effects by Chris Johnson. This one needs no install, because tui-wave
  compiles it in.

One browser lists all three, and one chain can mix them.

Read [DOCUMENTATION.md](DOCUMENTATION.md) to learn how to use it.

## What it does

- **Waveform display.** Zoom from the whole file down to single samples. Terminals such as
  kitty and ghostty get graphics. Every other terminal gets approximation via braille glyphs.
- **Playback.** Play, pause, and loop. The view can follow the play position.
- **Editing.** Cut, copy, paste, delete, and undo, with a separate undo stack per open file.
- **Processing.** Reverse, normalize, gain, fade, trim, resample, and mix to mono.
- **Markers.** Insert markers by hand or at transients. tui-wave saves them as WAV cue points,
  which Audacity and Sound Forge read.
- **Many channels.** Scroll through the channels of a multichannel file. Drop the empty ones.
  Split the rest into mono files or stereo pairs.
- **Large files.** A file above the memory limit opens read-only and disk-backed. tui-wave
  reads and writes RF64 and BW64.
- **Formats.** It reads WAV, FLAC, and AIFF. It writes WAV, FLAC, and MP3.
- **Configurable.** Every key assignment lives in a TOML config file.
- **CDP.** An optional front end to the Composer's Desktop Project, with more than 400
processes implemented.
- **Praat.** An optional front end to praatAudioTools, 457 sound-transformation scripts for
Praat, in the same browser and chainable with CDP.
- **Airwindows.** 500 of Chris Johnson's effects, built in. Nothing to install and nothing to
configure — unlike CDP and Praat, the processing is compiled into tui-wave itself, so it works
on a fresh install and previews return instantly.

The three backends put **1362 processes** in one browser: 405 from CDP, 457 from Praat and 500
from Airwindows. CDP adds 17 more that only a pitch-curve field can reach.

The CDP process browser, the parameter form with automatable green fields and presets, and the
breakpoint envelope editor:

<p>
  <img src="CDP1.png" alt="CDP process browser" width="32%" />
  <img src="CDP2.png" alt="CDP parameter form with presets and automatable fields" width="32%" />
  <img src="CDP3.png" alt="CDP breakpoint envelope editor" width="32%" />
</p>

## Status

An LLM helped to write this program. I am not a Rust developer. I have a lot of experience 
with digital audio, and I put a lot of effort into tui-wave's architecture, logic and UX / UI.
I needed this tool for my own work.

## Prerequisites

**If you can, build from source please.** `./setup.sh` does everything below on macOS and Linux.
It installs the toolchain, the build libraries, Praat, the praatAudioTools scripts, and the
optional Python environment. Then it builds and installs tui-wave. It asks before any step that
needs `sudo`. `--dry-run` shows every command and changes nothing. The script never changes your
system Python. It deletes its Rust build files (about 500 MB) when it ends. Use `--keep-build` to
keep them. It skips the build when the installed binary already comes from the same source. Use
`--rebuild` to build anyway. It keeps the compiled Airwindows library (28 MB) in
`~/.cache/tui-wave`, so a later build does not compile the C++ code again. It does not install
CDP, because those programs are a separate licensed download (see below).

Everything after this section is what that script automates, for anyone who would rather do it
by hand.

You need the Rust toolchain, version 1.85 or newer. The project uses the 2024 edition. Install
it from <https://rustup.rs>:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

An audio output device is optional. Without one you can still view and edit waveforms. You just
hear nothing.

### Build dependencies per platform

- **Linux.** The audio backend needs the ALSA development headers.
  - Debian and Ubuntu: `sudo apt install libasound2-dev pkg-config`
  - Fedora: `sudo dnf install alsa-lib-devel pkg-config`
  - Arch: `sudo pacman -S alsa-lib pkgconf`
- **macOS.** Nothing extra. The program uses the system CoreAudio framework.

A C++ compiler is also needed, for the built-in Airwindows effects. You almost certainly have
one already: it comes with `build-essential` on Debian and Ubuntu, `gcc-c++` on Fedora, `base-devel`
on Arch, and the Xcode command line tools on macOS.

## Installing a released build

Each release carries a macOS build for Intel and Apple Silicon, plus a `.deb` and an `.rpm` for
Linux — [github.com/biomassa/tui-wave/releases](https://github.com/biomassa/tui-wave/releases).

**Run `setup.sh` afterwards.** The packages contain the tui-wave binary and nothing else. But
457 of its processes are *scripts* from the praatAudioTools project, and no package includes them.
Without the scripts, tui-wave lists every Praat process and can run none. The script fetches the
scripts and tells tui-wave where they are. It can also set up the Python environment for the `py`
group.

`setup.sh` is the same script for every way of installing tui-wave. You can find it here:

| how you installed | where the script is |
| --- | --- |
| `.deb` or `.rpm` | `/usr/share/tui-wave/setup.sh` |
| macOS tarball | beside the binary, where you unpacked it |
| any release | attached to the release page on its own |
| source checkout | the repository root |

```sh
./setup.sh              # install what is missing and set up the environment
./setup.sh --dry-run    # print every command, change nothing
./setup.sh --no-python  # skip the venv; the 'py' group stays unavailable
./setup.sh --no-build   # set up the environment only; leave the binary alone
```

From the macOS tarball, the script also copies the binary to `~/.local/bin`. This needs no
`sudo`. From a source checkout, it builds and installs tui-wave. From a `.deb`, an `.rpm`, or the
copy on the release page, the binary is already installed. Then the script sets up the
environment only.

The script fetches praatAudioTools at the **exact commit** that your build's catalog came from.
The commit must match. The catalog records the parameter order of each script, and Praat fills a
script form by position. If the commit is different, Praat does not report an error. It gives
audio that sounds plausible and is wrong. tui-wave shows a warning in the process dialog if the
two ever differ. Run `setup.sh` again after every update. It moves the scripts to the correct
commit. It also corrects the `praat_audiotools_dir` setting if that setting points to an older
copy.

CDP is separate again, and has no installer anywhere; see [Optional: CDP support](#optional-cdp-support).

## Build and run

**`./setup.sh` does all of this for you.** It clones nothing, so run it from the repository. It
handles the toolchain, the build libraries, the Airwindows sources, and the release build. Then it
installs the binary. The rest of this section is the manual equivalent.

Clone the repository **with its submodules**, then build with Cargo.

```sh
git clone <this repository>
cd tui-wave
git submodule update --init
cargo build --release
```

The submodules are not optional: the Airwindows effects are compiled from
`third_party/airwin2rack`, so without that step the build stops with
`.../autogen_airwin is missing`. Use `--init` rather than `--init --recursive` — airwin2rack
declares submodules of its own, several hundred megabytes of upstream history that tui-wave
never reads.

**Updating an existing clone**, note that `git pull` alone does not fetch a submodule that was
added since you cloned. Run both:

```sh
git pull
git submodule update --init
```

Always build with `--release`. A debug build draws long files many times slower, because
tui-wave builds a waveform summary once per file.

Run the binary directly:

```sh
./target/release/tui-wave path/to/audio.wav
```

The argument is optional: pass a file to open it, a directory to start the Files panel there,
or nothing for an empty screen. `--version` (`-V`) and `--help` (`-h`) print and exit without
starting the editor.

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

[DOCUMENTATION.md](DOCUMENTATION.md) covers the rest.

## Airwindows

500 effects by [Chris Johnson](https://www.airwindows.com) — saturation, console emulations,
reverbs, dithers, tape and lo-fi colour — under the **Airwindows** domain of the ExtProcess
browser (`Ctrl+P`).

Nothing to install. The DSP is compiled into tui-wave from the
[airwin2rack](https://github.com/baconpaul/airwin2rack) consolidation of Chris Johnson's MIT
sources, so this is the one process backend that always works. It also means previews come
back immediately: there is no program to start and no temporary file to write, which is most
of what makes a CDP or Praat preview take as long as it does.

Two things behave differently from the rest of the browser:

- **Mono or stereo only.** Every Airwindows effect is hard-wired to two channels, so a
  selection wider than two is refused before Apply is enabled rather than being processed
  two channels at a time. A **mono** buffer is fed to both sides and comes back stereo, which
  is what lets the reverbs and stereo wideners do their job; undo restores it to mono.
- **Parameters read 0 to 1**, and each field shows the effect's own reading of the value
  beside it — the real figure in dB, Hz or whatever the effect uses. Airwindows works this way
  natively; that display is the only place those units exist.

See `THIRD_PARTY_NOTICES.md` for licensing. Everything is MIT.

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
through ExtProcess then Configure CDP Directory. tui-wave saves it as `cdp_dir` in your config
file.

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

- **macOS.** Download a prebuilt release from
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

## Optional: Praat support

praatAudioTools is a collection of sound-transformation scripts for Praat, by Shai Cohen of
Bar-Ilan University. tui-wave runs 457 of them: granular, spectral, reverb, distortion, spatial,
generative and more. They share the `Ctrl+p` browser with CDP, under a **Praat** domain, and a
chain (`Ctrl+h`) can mix the two freely.

Praat support is optional in the same way CDP support is: tui-wave works fully without it.

**`./setup.sh` sets up both of the things below**: the Praat binary and the praatAudioTools
scripts. If you ran it, there is nothing left to do here. What follows is the manual route.

You need two things.

**Praat**, from your package manager:

```sh
sudo pacman -S praat          # Arch
sudo apt install praat        # Debian, Ubuntu
brew install --cask praat     # macOS
```

tui-wave finds it on your `PATH`, so there is usually nothing to configure. Set `praat_bin` in
your config file if yours lives elsewhere — on macOS the executable sits inside the app bundle,
at `/Applications/Praat.app/Contents/MacOS/Praat`.

Praat itself is a separate install. `setup.sh` offers to install it with your package manager. It
also sets up the praatAudioTools *scripts* and the Python environment for the `py` group. If you
run a Praat process without Praat, tui-wave says so and names the fix. It does not show a raw OS
error.

**The scripts** are the praatAudioTools project. `./setup.sh` fetches them at the exact commit that
your build needs. It puts them in `~/.config/tui-wave/praat/audiotools` and sets
`praat_audiotools_dir` to that folder. To do it by hand, clone
<https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools>. Check out the commit named in the
header of `src/model/cdp/praat_catalog.toml`. Then set `praat_audiotools_dir` to that folder.

A source checkout also has the scripts as the git submodule `third_party/praat-audiotools`.
`cargo run` from the checkout uses the submodule when `praat_audiotools_dir` is not set.

If the scripts are missing, tui-wave says so when you run a Praat process.

Nothing is installed into your Praat setup, and your Praat preferences folder is never written
to.

### Optional: the `py` process group

**`./setup.sh` offers to set this up for you.** It uses a virtual environment that it owns. The
rest of this section is the manual route.

46 of those scripts do their work in Python rather than in Praat: they hand the audio to a
helper script and read the result back. They appear under their own **py** group in the browser,
so the extra requirement is visible before you pick one rather than a surprise when you run it.
Everything in the other thirteen groups works without any of this.

They need three Python packages, and two more for the interactive ones:

| Package | Needed for |
|---|---|
| `numpy`, `scipy`, `soundfile` | all 46 — the array maths and WAV I/O every helper uses |
| `sounddevice` | Arranger and Performance Launcher, which audition while you work |
| `pillow` | Spectral Eraser, which paints on a spectrogram image |

`./setup.sh` asks whether to install them and puts them in a virtual environment tui-wave
owns, at `~/.config/tui-wave/praat/pyenv`. **Your system Python is never modified** — which
matters on Arch and recent Debian, where it is marked externally-managed and `pip install`
refuses outright.

tui-wave runs these scripts against that venv's interpreter directly, so it does not matter
which Pythons your machine has or how Praat was launched. This is what makes the `py` group
work on macOS: the scripts pick their own interpreter and on a Mac pick an *absolute* path
(`/opt/homebrew/bin/python3` and friends), which no `PATH` setting can influence — so before
this they quietly used a Python that had none of these packages.

To do it by hand:

```sh
python3 -m venv ~/.config/tui-wave/praat/pyenv
~/.config/tui-wave/praat/pyenv/bin/pip install numpy scipy soundfile sounddevice pillow
```

tui-wave puts that environment ahead of your `PATH` for the Praat process it starts, so the
scripts find it without any of them being edited. If the environment does not exist, `PATH` is
left alone and the scripts use whatever `python3` you already have — so if those packages are
already installed system-wide, nothing more is needed.

Four of these open a window of their own — Arranger, Performance Launcher, Spatial Panner and
Spectral Eraser. They run with no time limit, because you decide when they are finished. `Esc`
cancels.

## Known issues

### macOS: four Praat processes run the wrong script

praatAudioTools contains four pairs of scripts whose filenames differ **only in case**, in the
same folder:

| | |
| --- | --- |
| `Filter & Color/DYNAMIC_FORMANT_SWEEPER.praat` | `Filter & Color/Dynamic_Formant_Sweeper.praat` |
| `Reverb/Stereo_Shimmer.praat` | `Reverb/stereo_shimmer.praat` |
| `py/Paulstretch.praat` | `py/paulstretch.praat` |
| `py/Recomposer.praat` | `py/recomposer.praat` |

A case-insensitive filesystem can hold only one file of each pair, and **APFS is case-insensitive
by default**, so a stock Mac keeps one of each. This is not something tui-wave or its packaging
chooses: `git clone` collapses the pairs the same way, so the limit applies however the scripts
arrive. Linux, and any case-sensitive volume, is unaffected.

Both members of every pair are separate entries in tui-wave's process catalog, and a
case-insensitive lookup resolves both names to the one surviving file. So on such a volume these
four processes —

- **DYNAMIC FORMANT SWEEPER**
- **Stereo Shimmer**
- **Paulstretch**
- **Recomposer**

— do not report an error. They run their case-twin's script instead: **Dynamic Formant
Sweeper (2)**, **stereo shimmer (2)**, **paulstretch (2)** and **recomposer (2)** respectively.
Praat fills a script's form by position, so the parameters you set are handed to a script that
may expect different ones. The result is audio, and it may sound plausible, but it is not the
process you asked for.

The four `(2)` entries themselves are unaffected — they are the scripts that survive, and
everything else in the catalog works normally.

If you need one of the four, run it on Linux, or put the checkout on a case-sensitive volume.

## Development

```sh
cargo build      # debug build
cargo test       # run the test suite
```

`CHANGELOG.md` records what changed in each version. `MANUAL_TESTING.md` holds the checklist for
the parts that no test can cover, such as real audio hardware and real terminal quirks.
