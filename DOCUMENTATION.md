## 1. Install

### If you downloaded a release

Releases carry a macOS build for Intel and Apple Silicon and a `.deb`/`.rpm` for Linux. They
contain the tui-wave binary and nothing else.

**Run `setup-environment.sh` after installing.** 458 of tui-wave's processes are scripts
from the praatAudioTools project, which no package bundles — without them tui-wave lists every
Praat process and can run none of them. The script fetches the scripts, writes their location
into your config, and (after asking) sets up the Python environment the 46 processes in the `py`
group need. It also checks whether Praat itself is installed and says where to get it.

The 500 Airwindows effects need none of this. They are compiled into the binary, so section 16
works on a fresh install with nothing fetched and nothing configured.

| how you installed | where the script is |
| --- | --- |
| `.deb` or `.rpm` | `/usr/share/tui-wave/setup-environment.sh` |
| macOS tarball | beside the binary, where you unpacked it |
| any release | attached to the release page on its own |
| source checkout | the repository root |

```sh
./setup-environment.sh              # fetch the scripts, configure, set up Python
./setup-environment.sh --dry-run    # print every command, change nothing
./setup-environment.sh --yes        # take every prompt as yes
./setup-environment.sh --no-python  # skip the venv; the 'py' group stays unavailable
```

It clones praatAudioTools at the **exact commit** your build's process catalog was generated
from, and re-running it moves an existing checkout to that commit. The pin is not cosmetic: the
catalog records every script's parameter names, types and *order*, and Praat fills a script's
form positionally — a checkout at a different commit does not error, it hands arguments to
fields that have moved and produces plausible, wrong audio. If the two ever drift apart, the
process dialog says so.

It does not install CDP: that is a separate download with no installer on any platform, and
tui-wave asks for its directory the first time you run a CDP process.

### If you are building from source

**Start here: run `./install.sh` from the repository.** On macOS and Linux it does the whole of
this section for you — it installs the Rust toolchain if missing, the build dependencies for your
platform, Praat, the script submodule, and (after asking) the Python environment the `py` process
group needs, then builds and installs tui-wave. It asks before anything requiring `sudo`, and
`./install.sh --dry-run` prints every command it would run without changing anything. It does not
install CDP, which is a separate licensed download.

Everything you may want to install lives in this section: tui-wave itself, and the two optional
external tool suites it can drive. Only tui-wave is required — it opens, edits, plays and saves
files with neither of the others present.

The rest of this section is what the script does, for anyone doing it by hand.

### tui-wave

To build tui-wave from source:

1. Install the Rust toolchain from https://rustup.rs.
2. Clone the repository, with its submodules:

   ```sh
   git clone --recursive https://github.com/biomassa/tui-wave
   ```

   The submodule holds the praatAudioTools scripts. You can add it later if you skip it now —
   see the Praat part below.
3. Run `cargo build --release` in the repository directory.
4. Copy `target/release/tui-wave` to a directory on your `PATH`.

Always build with `--release`. A debug build draws waveforms many times slower.

Or install in one line, from the repository directory:

```sh
cargo install --path .
```

This does steps 3 and 4 together. It builds with `--release` (that is the default; `--debug`
exists to opt out) and puts the binary in `~/.cargo/bin`, which the Rust installer adds to your
`PATH`. Everything the program needs at runtime is compiled into the binary, so it works from
anywhere.

Two things to know. The command builds in a temporary directory rather than `target/`, so it
compiles from scratch even if you have just built the project. And it installs a copy: after
pulling new changes, run it again to update.

The program runs on Linux and macOS. Windows is not supported.

Audio playback needs a working sound device. If the program finds no device, it still opens,
draws, and edits files. Only playback stops working.

### CDP (optional)

CDP is the Composer's Desktop Project, a large set of offline sound transformation programs.
Section 14 covers using them.

CDP does not go on your `PATH`, and no package manager carries it. Download or build it, then
tell tui-wave where the binaries live.

On macOS, download a prebuilt release from <https://www.unstablesound.net/cdp.html>, the
official download mirror, and unzip it anywhere. The binaries land in a folder such as
`_cdprogs` or `NewRelease`.

On Linux there are no prebuilt binaries, so build from source:

```sh
git clone https://github.com/ComposersDesktop/CDP8.git
cd CDP8
mkdir build && cd build
cmake ..
make
```

That needs `cmake` and a C compiler on your `PATH`. The compiled binaries land in a top-level
`NewRelease` directory. The older [CDP7](https://github.com/ComposersDesktop/CDP7) source builds
the same way and works just as well; the tui-wave catalog does not depend on one CDP release.

tui-wave looks in `~/cdp` by default, so unpacking or building there needs no further setup.
Anywhere else, answer the first-use prompt with the real path, or set it later through the
ExtProcess menu with Configure CDP Directory. tui-wave saves it as `cdp_dir` in your config file.

### Praat and praatAudioTools (optional)

Praat is a speech-analysis program with a scripting language. praatAudioTools is a large
collection of sound-transformation scripts written for it by Shai Cohen. Section 15 covers using
them.

**`./install.sh` installs both of the things below.** If you ran it, skip to section 15. What
follows is the manual route.

You need two things.

**Praat itself.** Install it from your package manager.

On Linux:

```sh
sudo pacman -S praat          # Arch
sudo apt install praat        # Debian, Ubuntu
```

On macOS:

```sh
brew install --cask praat
```

Without Homebrew, download the disk image from https://www.praat.org and drag Praat to your
Applications folder.

tui-wave finds Praat on your `PATH`, so on Linux there is usually nothing to configure. Check it
with `praat --version`.

macOS installs Praat as an application bundle, and the program inside it is not on your `PATH`.
Set `praat_bin` in your config file to the program itself:

```toml
praat_bin = "/Applications/Praat.app/Contents/MacOS/Praat"
```

Use the same setting on Linux if your Praat lives somewhere your `PATH` does not reach.

**The scripts.** They ship with tui-wave as a git submodule, so a recursive clone already has
them:

```sh
git clone --recursive https://github.com/biomassa/tui-wave
```

If you cloned without `--recursive`, fetch them once from inside the repository:

```sh
git submodule update --init
```

That writes them to `third_party/praat-audiotools`, which tui-wave uses by default. If you
forget the step, tui-wave names this exact command when you run a Praat process.

To use your own copy instead, clone praatAudioTools wherever you like and point the config at
it:

```sh
git clone https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools ~/praat-audiotools
```

```toml
praat_audiotools_dir = "/home/you/praat-audiotools"
```

You do **not** need to install the scripts into Praat itself. tui-wave runs them by path and
never writes to your Praat preferences folder, so an existing Praat setup is left alone.

**Python, for the `py` group only** — and again, `./install.sh` offers to do all of this for you,
into a virtual environment it owns. 46 of the scripts do their work in Python instead of in
Praat: they hand the audio to a helper and read back the result. They sit in their own **py**
group in the browser so you can see the extra requirement before choosing one. The other
thirteen groups need nothing beyond Praat.

| Package | What needs it |
|---|---|
| `numpy`, `scipy`, `soundfile` | all 46 — array maths and WAV reading/writing |
| `sounddevice` | Arranger, Performance Launcher — they audition as you work |
| `pillow` | Spectral Eraser — it paints on a spectrogram image |

`./install.sh` asks whether to install these and puts them in a virtual environment tui-wave
owns, at `~/.config/tui-wave/praat/pyenv`. Your system Python is never modified. That is not
only tidiness: Arch and recent Debian mark the system interpreter externally-managed, and
`pip install` there fails outright.

tui-wave runs these scripts against that venv's interpreter directly, so it does not matter
which Pythons your machine has or how Praat was launched. This is what makes the `py` group
work on macOS: the scripts pick their own interpreter and on a Mac pick an *absolute* path
(`/opt/homebrew/bin/python3` and friends), which no `PATH` setting can influence — so before
this they quietly used a Python that had none of these packages.

By hand:

```sh
python3 -m venv ~/.config/tui-wave/praat/pyenv
~/.config/tui-wave/praat/pyenv/bin/pip install numpy scipy soundfile sounddevice pillow
```

tui-wave runs each of these scripts from a temporary *copy* whose interpreter is repointed at
that environment, and also puts it at the front of `PATH` for the Praat process it starts. Your
own copy of the plugin is never modified. With no such environment neither happens: the scripts
resolve their interpreter exactly as they always would, so a system-wide install of those
packages works too.

Four of the `py` processes open a window of their own — Arranger, Performance Launcher,
Spatial Panner and Spectral Eraser. Those run with no time limit, because you decide when they
are done. `Esc` cancels.

---

## 2. Start the program

Run the program with no argument to get an empty screen:

```sh
tui-wave
```

Run it with a file to open that file:

```sh
tui-wave take.wav
```

Run it with a directory to start the Files panel there:

```sh
tui-wave ~/recordings
```

tui-wave reads WAV, FLAC, AIFF, and RF64 files. It writes WAV, FLAC, and MP3 files.

Two options print and exit without starting the editor:

```sh
tui-wave --version     # or -V
tui-wave --help        # or -h
```

Anything else beginning with `-` is rejected as an unknown option rather than treated as a
filename, so a typo tells you so instead of opening the editor on a file that cannot exist.

To leave the program, press `q`. If a file has unsaved changes, tui-wave asks you first.

---

## 3. The screen

The screen has five parts, from top to bottom:

1. The **menu bar**, with the titles File, Edit, View, Process, ExtProcess, Markers, and
   Transport. ExtProcess holds the three external backends — CDP, Praat and Airwindows.
2. The **toolbar**, a row of clickable commands.
3. The **Files** panel on the left, which lists the current directory.
4. The **Buffers** panel in the middle, which lists the open files.
5. The **Waveform** panel on the right, which draws the audio.

The **status bar** sits along the bottom. It shows the cursor time, the selection length, the
sample rate, and the state of each toggle.

### Focus

One panel at a time has focus. A peach border marks it. Press `Tab` to move focus forward and
`Shift+Tab` to move it back. The order runs Waveform, Files, Buffers, and back to Waveform.

Focus matters for two reasons. The toolbar shows a different command set for each panel. Some
keys also change meaning. For example `Ctrl+o` is Fade Out in the Waveform panel and Open
Directory in the Files panel.

### The menu bar

Press `F10` to open the first menu. Press `Alt` and a letter from the title to open one menu
directly: `Alt+f` File, `Alt+e` Edit, `Alt+v` View, `Alt+p` Process, `Alt+x` ExtProcess,
`Alt+m` Markers, `Alt+t` Transport.

Inside a menu, the Left and Right arrows move between menus. The Up and Down arrows move
between entries. Press `Enter` to run the entry and `Esc` to close the menu.

Three dots after an entry mean that the entry opens a dialog. An entry in grey means that the
command does not apply to the current file.

### The key reference

Press `?` for a window listing every key and what it does, in two columns. The toolbar names it
`Keys`. It is read-only: nothing you press inside it reaches your audio.

| Key | Action |
| --- | --- |
| `Up` / `Down` | Scroll one row |
| `PgUp` / `PgDn` | Scroll one screen |
| `Home` / `End` | Jump to the top or the bottom |
| Wheel | Scroll three rows |
| `?`, `Esc`, `Enter` or `q` | Close the window |

The count at the bottom right says how far down the list you are. The key column shows *your*
bindings, so a key you rebound in `config.toml` appears as you bound it.

The window lists keys only. A command reached from a menu and nothing else is not in it — the
menu is its own reference, and a row that answered "which key" with "none" would be a row to
skip past on every pass down the list. Section 22 names those commands.

The window works on a streamed read-only buffer, where almost nothing else does.

### Dialogs

Every dialog works the same way. `Tab` moves between fields. `Enter` accepts the dialog, and
`Esc` cancels it. A hint row at the bottom of each dialog names the keys, with the key names in
peach.

---

## 4. Move through the waveform

Give the Waveform panel focus first.

| Key | Action |
| --- | --- |
| `Left` / `Right` | Move the cursor one column |
| `Home` / `End` | Jump to the start or the end |
| `PageUp` / `PageDown` | Move one screen back or forward |
| `Up` / `Down` | Zoom in or out along time |
| `Shift+Up` / `Shift+Down` | Zoom in or out along amplitude |
| `a` | Fit the amplitude zoom to the loudest sample |
| `Backtick` | Turn fine step mode on or off |

The zoom keys are the arrows, not `Ctrl+1` and `Ctrl+3`. Terminals take many key combinations
for themselves before a program sees them, so tui-wave keeps to plain keys.

Zoom along time anchors on the cursor column. The sample under the cursor stays in the same
column across the zoom change. Your place on the timeline therefore does not jump.

### Fine step mode

The Left and Right arrows normally move the cursor by one screen column. At a wide zoom that is
thousands of samples.

Press the backtick key to turn on fine step mode. The same arrows then move about one eighth of
a column. The status bar shows `Fine: on` while the mode is active. Press the backtick key again
to turn the mode off.

### The vertical scale

Each channel pane carries a decibel scale on both sides. The marks read 0dB, -3, -6, -12, -18,
and -24. The scale is always absolute, so 0dB means an amplitude of 1.0.

Vertical zoom moves the marks. A quiet file with a peak of -6 dBFS shows -6 at the top of the
pane after you press `a`. The scale never relabels the peak as 0dB.

A horizontal time ruler sits under the waveform. Turn it off from View if you want the extra
row for audio.

---

## 5. Select audio

A selection sets what the edit and process commands act on. With no selection, most commands
act on the whole file.

| Key | Action |
| --- | --- |
| `Shift+Left` / `Shift+Right` | Extend the selection one column |
| `Shift+Home` / `Shift+End` | Extend the selection to the start or the end |
| `Shift+PgUp` / `Shift+PgDn` | Extend the selection one screen |
| `Ctrl+a` | Select the whole file |
| `Ctrl+d` | Clear the selection |
| `{` / `}` | Extend the selection to the previous or next marker |

You can also drag with the mouse across the waveform.

### Zero-crossing snap

Press `z` to turn zero-crossing snap on or off. With the snap on, the edges of a new selection
move to the nearest point where the waveform crosses zero. This removes the click that a cut at
a non-zero amplitude leaves behind.

---

## 6. Play audio

| Key | Action |
| --- | --- |
| `Space` | Play or pause |
| `l` | Turn loop playback on or off |
| `i` | The cursor follows playback, on or off |
| `f` | The view follows playback, on or off |

Playback starts at the cursor. With a selection active, playback covers the selection only.

A bold vertical line marks the play position. tui-wave keeps that line on screen. Every command
that moves the play position also scrolls the view to it.

### Playing a file with three or more channels

Your output is stereo, and the file may have fifty-six channels. tui-wave folds them down as it
plays:

- Odd-numbered channels (1, 3, 5, …) are summed into the left output.
- Even-numbered channels (2, 4, 6, …) are summed into the right output.
- Each side is then divided by the square root of how many of its channels carry signal.
- The result passes through a limiter that keeps the output at or below -1 dBFS.

The division is what keeps the level sane without making the material quiet. Summing a dozen loud
channels raw would drive the limiter so hard that it distorts rather than limits, while dividing
by the plain channel count would make everything far too quiet.

Only channels that carry signal are counted, using the same -48 dBFS threshold as Remove Empty
Channels. This matters on real takes, which are mostly empty: a 56-channel recording with six live
channels is divided as a six-channel file, not as a fifty-six-channel one. Empty inputs never
quieten the channels you are trying to hear, and dropping them with Remove Empty Channels does not
change the playback level.

The limiter then catches what the division does not. Channels carrying the same sound sum faster
than the division allows for, so on a loud passage you may still hear it saturate gently. That is
intended.

Mono and stereo files are not folded, not divided and not limited. They play exactly as they
always have.

Nothing here changes the audio on disk. It applies to monitoring only.

---

## 7. Edit audio

| Key | Action |
| --- | --- |
| `Ctrl+x` | Cut the selection |
| `Ctrl+c` | Copy the selection |
| `Ctrl+v` | Paste at the cursor |
| `Del` | Delete the selection |
| `C` | Copy the selection into a new buffer |
| `Ctrl+z` | Undo |
| `Ctrl+y` or `Ctrl+Shift+z` | Redo |

The clipboard lives outside the open files. You can therefore cut from one buffer and paste
into another.

Undo history belongs to each buffer. tui-wave keeps a separate stack per open file, so undo in
one buffer never touches another. Markers move with the audio across a cut or a paste.

---

## 8. Process audio

Open the Process menu, or use the keys below. Each command acts on the selection, or on the
whole file when you select nothing.

| Key | Command | What it does |
| --- | --- | --- |
| `Ctrl+r` | Reverse | Plays the samples backward |
| `Ctrl+n` | Normalize | Raises the level to a target peak |
| `Ctrl+g` | Gain | Changes the level by a number of decibels |
| `Ctrl+f` | Fade In | Raises the level from silence |
| `Ctrl+o` | Fade Out | Lowers the level to silence |
| `Ctrl+t` | Trim | Throws away everything outside the selection |
| `Ctrl+e` | Resample | Changes the sample rate |
| `Ctrl+b` | Technical Fades | Adds very short fades at both ends |
| `Ctrl+m` | Mix to Mono | Sums the channels into one |

Four more Process commands have no key. Open the Process menu for them.

| Command | What it does |
| --- | --- |
| Mix Multichannel to Stereo | Routes every channel to Left, Right, Both or Skip, into a new buffer. Section 11 |
| Remove Empty Channels | Drops the channels that hold nothing. Section 11 |
| Remove DC Offset | Recentres each channel on zero |
| High-Pass Filter | Removes a drifting baseline below a cutoff you give |

Technical Fades removes the click at a hard edit point. The fades last a few milliseconds, so
you do not hear them as a fade.

Resample rewrites the whole file with a windowed-sinc conversion. It also rebuilds the audio
engine, because the engine reads the sample rate once at startup.

Gain opens with one field. On a stereo file it also offers a per-channel mode, which splits that
field into a Left gain and a Right gain, and a soft-clip option that runs the result through a
tanh limiter instead of letting it clip.

### Remove DC Offset and High-Pass Filter

These two answer different questions, which is why they are two commands.

**Remove DC Offset** subtracts one constant per channel. That is the right correction for the
fixed bias a capture chain adds, and no correction at all for a baseline that wanders. The
dialog offers two ways to measure that constant. `Tab` cycles them.

- **Median**, the default: the level the signal sits on. Use this.
- **Mean**: the DC component in the strict sense. It drives the file's average to exactly zero,
  which some measurements want. On real material the mean mostly reports the waveform's own
  asymmetry, so it can lift a file that had no offset at all off the zero line.

It measures each channel on its own, and always over the whole file. Two channels biased in
opposite directions would cancel in one shared figure and neither would be fixed. A constant
subtracted across part of a file is a step at each edge, which you hear as a click.

**High-Pass Filter** is for the drifting baseline, and it does honour the selection. Give it a
cutoff in Hz. The filter is a 2nd-order Butterworth run forward and then backward, so the two
passes cancel each other's phase shift exactly and the slope works out at 24 dB per octave. A
cutoff at or above half the sample rate is refused, because such a filter has no passband left.

Every command in this section supports undo.

---

## 9. Markers

A marker names one point on the timeline. tui-wave saves markers into the WAV file as cue
points with labels. Audacity and Sound Forge read the same chunks.

| Key | Action |
| --- | --- |
| `m` | Insert a marker at the cursor |
| `M` | Delete the marker nearest the cursor |
| `[` / `]` | Jump to the previous or next marker |
| `{` / `}` | Extend the selection to the previous or next marker |

A new marker gets the name `Marker N`. To rename one, double-click its label. To move one, drag
its line with the mouse.

### Markers at transients

Press `t` to place a marker at every transient in the selection. A transient here means a sharp
rise in level.

The `+` and `-` keys raise and lower the threshold that decides what counts as a transient.
Press `/` to jump to the next rising edge and `\` to jump to the previous one. Use those two
keys to check the threshold before you place markers with `t`.

---

## 10. Head and tail marks

Head and tail marks form a second, separate list. They exist for the CDP DISTMORE family of
processes, which cuts audio into segments.

The list alternates. The first mark is a Head, the second is a Tail, the third is a Head, and so
on. The screen labels them `H1`, `T1`, `H2`, `T2`, and onward. tui-wave draws them in orange
with a dashed line, one row below the ordinary marker labels.

| Key | Action |
| --- | --- |
| `h` | Insert a head or tail mark at the cursor |
| `H` | Delete the mark nearest the cursor |

The role comes from the position in the list, so a new mark in the middle renumbers everything
after it. Head and tail marks carry no name and have no jump keys.

tui-wave saves them next to the audio in a file named `<stem>.headstails`, not inside the WAV.
That file holds one time in seconds per line, which is the format CDP itself reads. You can edit
it by hand, and you can pass it straight to the `distmore` program.

A save with no marks deletes an existing sidecar file. A save with no marks and no sidecar file
writes nothing.

---

## 11. Files with many channels

tui-wave treats multichannel files as a normal case.

The Waveform panel draws six channel panes at a time by default. A taller terminal window shows
more. Use these keys to move the channel window:

| Key | Action |
| --- | --- |
| `,` / `.` | Move the channel window up or down by one |
| `<` / `>` | Move the channel window up or down by one page |

The mouse wheel over the waveform does the same.

### Remove Empty Channels

Many recording rigs write every input, and most inputs hold nothing. Open Process and choose
Remove Empty Channels to drop them.

The command drops every channel whose peak sits below a threshold. The default threshold is -48
dBFS. The command measures the whole file, never the selection. It also supports undo.

### Mix Multichannel to Stereo

Open Process and choose Mix Multichannel to Stereo to fold a wide file down to two channels
deliberately, rather than leaving it to the monitoring fold of section 6.

The dialog lists every channel with a destination — Left, Right, Both or Skip — and a dB
attenuation. The list scrolls, so thirty channels are as workable as three. `←` and `→` cycle
the destination of the selected row, and typing edits its gain, so the arrows do not move a
text caret here as they do in other dialogs.

The defaults are channel 1 left, channel 2 right, and so on alternating, at -6 dB a channel.
That is the same interleave the monitoring fold uses, so the dialog opens on the routing you
were already hearing. -6 dB rather than unity because unity on every channel of a mixdown is
the one setting guaranteed to clip. A channel set to Both loses a further 3 dB in each leg, so
centred material does not sit louder than panned material.

The limiter is on by default and works on the summed legs. Its ceiling is the field beside the
checkbox, at -1 dBFS to match the fold. A ceiling above 0 dBFS is allowed: this is a saturator,
not a cap. A gain field you cannot parse reads as silence for that channel.

The result is a new buffer. Your source file is untouched. The command is refused below three
channels, where Gain and Mix to Mono say the same things more directly.

The summary line counts every channel exactly once, in four buckets that add up to the channel
count: left, right, both, and dropped. A channel counts as dropped when it is silent in the
output, whatever its destination says.

### Export Channels

Open File and choose Export Channels to split a multichannel file into separate WAV files.

The dialog lists every channel with three choices: Mono, Skip, and Pair with next. tui-wave
writes one file per Mono channel and one file per pair.

The names read `<stem>_ch03.wav` and `<stem>_ch03-04.wav`. The channel numbers carry leading
zeros, so a directory listing sorts in channel order.

Markers and head and tail marks carry over into every output file. The timeline does not change,
so the times still mean what they meant.

---

## 12. Large files

A long multichannel take can reach tens of GB. tui-wave opens such a file in **streamed
read-only mode**.

### What triggers the mode

tui-wave reads the file header first, which takes microseconds. It then works out the decoded
size in memory. The working format is 32-bit float whatever the source depth, so a 24-bit file
grows by one third and a 32-bit float file stays the same size.

If that decoded size exceeds `max_resident_mb`, the file opens streamed. The default is 4096, or
4GB. Section 21 shows how to change it.

Below the threshold nothing changes. The file opens the way it always did.

### What the mode does

tui-wave keeps only a small summary of the audio in memory, not the samples. It reads short
windows from disk while it draws. A streamed file therefore costs about one thirtieth of its
size in memory.

The waveform title carries `[streamed, read-only]` so you always know which mode you are in.

Opening takes time, because tui-wave reads the whole file once to build the summary. A 30GB file
with 56 channels takes about 53 seconds. A progress panel shows how far the read has got. Keys
you press during the read arrive after it finishes.

### What works in the mode

Only these commands work on a streamed file:

- Every navigation, zoom, and view command.
- Selection and cursor movement.
- Playback, including loop and selection playback.
- Remove Empty Channels, with undo.
- Export Channels.
- Save As.

tui-wave refuses everything else and names the reason. Editing needs a full copy of the samples
for undo. Markers would have no save path, so the work would go missing on close.

Playback reads the audio off disk as it plays rather than loading it, so hearing a 30GB take
costs under a megabyte of buffering. Seeking, looping and playing a selection all behave as they
do on an ordinary file.

### The workflow this mode exists for

1. Open the large take.
2. Run Remove Empty Channels.
3. Run Save As to write a smaller file, or run Export Channels to split it.

Save As on a streamed file reads the source and writes the output in one pass. It never holds
the audio in memory.

### RF64

A plain WAV file cannot exceed 4GB, because the size field holds 32 bits. Recorders work around
that with the RF64 and BW64 formats. Those use the same chunk layout, set the affected size
fields to a sentinel value, and put the real 64-bit sizes in a `ds64` chunk.

tui-wave reads RF64 and BW64 files. It also writes RF64 when an output grows past 4GB. It
tolerates a header with a wrong size field, which some recorders write when a take ends without
a clean stop.

---

## 13. Save your work

| Key | Command | What it writes |
| --- | --- | --- |
| `Ctrl+s` | Save | The same path, as 32-bit float WAV |
| `Shift+S` or `Ctrl+Shift+S` | Save As | A new path, with a choice of depth |
| `Ctrl+l` | Save All | Every buffer with changes |

Save As answers to two keys. Both stay bound, because a terminal does not always deliver a
two-modifier combination — `Shift+S` is the one that always arrives.

Every write is staged. tui-wave writes a temporary file beside the target and renames it over
the target only once the write is complete, so a full disk or a crash leaves your original file
where it was.

Save As lets you pick the bit depth. Press `Tab` to move between 16-bit, 24-bit, and 32-bit
float. Press `Ctrl+d` to turn dither on or off. Dither helps at 16-bit and 24-bit, and does
nothing at 32-bit float.

Every save path writes WAV bytes. A buffer that you loaded from a FLAC or an AIFF file therefore
has no path that Save may write to. Save redirects to Save As with a `.wav` name. Save All skips
such a buffer and leaves it marked as changed.

### Export to FLAC and MP3

Open File and choose Export to write a FLAC or an MP3 file. Both formats accept one channel or
two channels only. MP3 export uses a constant bit rate.

tui-wave states the reason inline if the buffer has too many channels, or if the sample rate is
illegal for MP3.

### Export Regions

Press `Shift+E` to write the audio between markers into separate files in a subfolder. Use this
to split one long take at the marks you placed.

The dialog takes a subfolder name, a base name for the files, a bit depth and the dither
toggle. Four optional steps sit below that, each a checkbox with its own value field: limit the
length of a region, normalize it, fade it in, fade it out. tui-wave applies them in that order.
It limits the length first, so normalize measures the peak of the audio you actually keep, and
it fades last, so the taper never affects that measurement.

---

## 14. External processes: the browser and the parameter form

tui-wave drives three process backends through one browser:

- **CDP**, the Composer's Desktop Project — a large set of command-line transformation
  programs you install yourself. More than 400 processes.
- **Praat** with the praatAudioTools scripts, again installed separately. 458 processes.
  Section 15 covers what is particular to them.
- **Airwindows**, 500 effects compiled into tui-wave. Nothing to install. Section 16.

This section describes what all three share: the browser, the parameter form, previews,
presets, envelopes and chains. tui-wave runs a process on your selection and splices the result
back in, and that edit supports undo.

| Key | Command |
| --- | --- |
| `Ctrl+p` | ExtProcess |
| `Ctrl+h` | ExtProcess Chain |

Install CDP first if you want that domain — section 1 covers it. You can change the path later
from the ExtProcess menu with Configure CDP Directory. The Airwindows domain never needs any of
this and is always available.

### The browser

The ExtProcess dialog opens a browser with three columns:

1. **Domain**: All, Recent, Time-domain, Spectral, Praat, Airwindows. The first two are the
   whole catalog and your recent picks; the middle two are CDP's two domains.
2. **Groups**: the groups inside that domain — CDP's own `DISTORT`, `BLUR`, `FOCUS` and so on,
   praatAudioTools' folders, Chris Johnson's Airwindows categories.
3. **Description**: the processes themselves.

Every group name comes from the backend's own documentation. Anything written about CDP,
praatAudioTools or Airwindows therefore names the same group you see here.

`Tab` and `Shift+Tab` move between columns and wrap around. Left and Right also move between
columns and stop at the ends. Focus skips the Groups column when it holds nothing, because All
and Recent have no groups.

Type to filter the process list. The Up and Down arrows move the selection, and `Enter` opens
the parameter form. Press `Ctrl+l` to reopen the last process you ran, with its values.

Every row carries one pale backend badge — `[cdp]`, `[pr]` or `[air]` — so you always know what
a process will need. Further badges mark what it wants from you: `>1 inputs`, `pitch curve`,
`formants`, `snapshot`, and `[pvoc]` for CDP's spectral domain.

### The parameter form

Each process opens a form of fields.

| Key | Action |
| --- | --- |
| Up / Down | Move between fields |
| Left / Right | Move the slider of the focused field |
| `Tab` / `Shift+Tab` | Move between fields, presets, Preview, and Apply |
| `Space` | Change a toggle field |
| type a digit | Replace the value of a number field |
| `e` | Open the envelope, list, table or curve editor of that field |
| `b` | Pick a buffer for a field that wants one |
| `p` | Preview, from any row |
| `Enter` | Run the process |
| `Esc` | Go back one level |

**Every parameter with two finite bounds carries a slider**, a dotted track with a round knob,
to the left of its number. Left and Right step it through 15 stops, or through one stop per
value when a whole-number range holds fewer than 15. The stops follow the parameter's own
scale, so a 20 Hz to 20 kHz range steps logarithmically rather than putting thirteen stops
below 2 kHz.

The slider is a way to reach a value, not a limit on it. A number you type is submitted exactly
as typed and merely drawn at the nearest stop. A parameter bounded on one side only — most of
the Praat catalog — gets no slider at all, because there is no honest place to draw the knob
when one end runs to infinity. On a narrow terminal the slider column is dropped and the rows
render as plain fields.

`Esc` walks back one level per press: parameter form, then browser, then the chain editor if
you came from one, then the waveform. Picking the wrong process out of the catalog costs one
key, not a reopen and a re-search.

Apply runs the process on your selection, or on the whole file when nothing is selected, and
splices the result back in.

**Preview** hears the result first and changes nothing on disk. It loops, because one pass over
a short selection decides nothing. `p` starts one from any row of the form. It stops on its own
when you leave the dialog, and when you change any value, because what loops is the result of
the values it ran with. A `[Preview ✓]` label means Apply will splice exactly what you last
heard, with no second run.

A blocked run is stated inline the moment the dialog opens, with Preview and Apply dimmed —
too many channels, no buffer open, no image picked, not enough head and tail marks. The reason
is in the dialog rather than in a failure after Apply, because nothing in the fields looks
wrong in those cases.

Spectral CDP processes need a phase-vocoder analysis pass first. tui-wave does that for you, so
you never touch an analysis file. A process that takes two inputs grows a second input row.
Left and Right pick another open buffer, which tui-wave uses whole. The sample rates must
match.

If a backend rejects the run, tui-wave shows its own error text in a scrollable viewer.

### Chains

`Ctrl+h` opens the chain editor, which runs several processes in order, each on the output of
the one before. A chain may mix all three backends freely. A chain of Praat steps alone needs
no CDP installed.

| Key | Action |
| --- | --- |
| Up / Down | Move between steps |
| Left / Right | Cycle the value on the selected row |
| `Shift`+Up / Down | Reorder a step |
| `Enter` | Open a step, or run the chain |
| `p` | Preview the whole chain |
| `h` | Preview as far as the selected step |
| `d` | Delete the step |
| `s` | Save the chain under a name |
| `l` | Load the chain you last ran |
| `Esc` | Close |

The top row cycles your saved chains, exactly as the preset row of a parameter form does.

### Automating a parameter

A field in green accepts a breakpoint envelope, which changes the value over time. Move to the
field and press `e` to open the envelope editor.

The editor graphs value against time over a dimmed copy of your waveform.

| Key | Action |
| --- | --- |
| Left / Right | Select the previous or next point |
| `Shift`+Left / Right | Move the point along time |
| Up / Down | Change the value |
| `Shift`+Up / Down | Change the value finely |
| `n` | Insert a point |
| `Del` | Remove a point |
| `c` | Throw away the envelope and go back to one value |
| `Enter` | Commit the shape and close |
| `Esc` | Close without keeping it |

The editor has a preset row of its own at the top, with the same `Tab`, `s` and `d` keys the
parameter form's preset row uses.

The mouse works too. A click selects the nearest point. A drag moves one, and a drag with
`Shift` moves it finely. **A double-click adds a point, or removes the one you double-clicked**
— removal is the second meaning of the double-click because a terminal never delivers
`Shift`+click to a program at all. Both xterm and kitty keep that gesture for their own text
selection. A `Shift`+drag does arrive, because the terminal forwards the drag once the button
is already down.

`c` asks before it acts. It discards the drawn shape and writes one constant back into the
field, and nothing can undo that: the editor keeps no history, and a plain number field cannot
remember a curve. On a field that requires a datafile there is no constant to go back to, so
`c` means "use an open curve" instead and opens a picker.

`Enter` means done, not save: it commits the drawn shape into the field and closes. `s` on the
same bar is what writes a named preset to disk.

### Presets

A preset row sits above the fields. Left and Right load a saved preset for this process. Press
`s` to save the current values under a name, and `d` to delete one. tui-wave stores presets in
`~/.config/tui-wave/cdp_presets/`.

`s`, `d`, `p`, `b`, `e` and `x` mean these commands everywhere in the form except in a field
that takes free text, and in the preset-name prompt. There they are typed as characters. The
hint bar greys a key exactly where it would be typed instead, so a hint never promises
something the key will not do.

### Adding your own processes

Drop a TOML file into `~/.config/tui-wave/cdp/`. The file uses the same schema as the built-in
catalog. A new `key` adds a process. A `key` that matches a built-in one replaces it, which lets
you correct a range or a default. Files there load after every built-in catalog, so this works
for the Praat and Airwindows entries too, not only the CDP ones.

The repository holds a worked example at `docs/cdp-custom-process-example.toml`. Copy it, edit
it, and restart tui-wave.

### Pitch curves

Some CDP processes take a pitch curve, which is a contour of time against frequency, instead of
one number.

1. Choose ExtProcess then CDP Extract Pitch Curve. This works best on a clear single-note
   melody. A `[p]` row appears in the Buffers panel.
2. Press `Enter` on that row to open a table of times and frequencies. The arrows select a row,
   and typing overwrites a value. Press `n` to insert a row and `Del` to remove one. Press `t`
   to apply a CDP curve transform, such as quantise, smooth, vibrato, or pitch shift. Press
   `Enter` to commit and `Esc` to discard.
3. Open any process with the `pitch curve` badge, such as Psow Stretch. Move to its pitch field,
   press `e`, then press `c` to load an open curve into the envelope. tui-wave rescales the
   curve to your selection.
4. Press `Ctrl+s` on a curve row to save it to disk. ExtProcess then CDP Load Pitch Curve reads
   one back.

`Ctrl+z` and `Ctrl+y` inside the curve editor undo and redo the curve, not the audio. A curve
that you typed or loaded by hand cannot run a transform, because it has no CDP source.

### Formants

Formants describe the timbre of a sound, apart from its pitch.

1. Choose ExtProcess then CDP Extract Formants to capture the spectral envelope of your
   selection into an `[f]` buffer. This works best on a voice or an instrument with real
   timbre. The menu offers two of them, which differ in how they size the analysis bands:
   pitch-wise uses musically log-spaced bands, frequency-wise uses bands of equal width in Hz.
2. Choose ExtProcess then CDP Freeze Formant Snapshot at Cursor to freeze the timbre at one
   moment into an `[s]` buffer. If the file has no formants yet, tui-wave extracts them first.
3. To put either onto other audio, open Formants Put for an `[f]` buffer or Oneform Put for an
   `[s]` buffer. Press `b` to pick the buffer, then Apply.

### Processes that read the waveform

Thirteen DISTMORE processes read your head and tail marks instead of a typed list of times.
Eight `scramble` modes read the same marks as cut points. For those, every mark counts on its
own, and one mark is enough.

A process of this kind has no dialog field to look at, so the dialog states any shortfall
inline. It also dims Preview and Apply until you have enough marks. DISTMORE needs at least two
complete head and tail pairs.

A selection that holds fewer than two pairs is almost always an accidental drag. tui-wave
therefore widens back to the whole file for those processes. Every other process honours a small
selection.

---

## 15. Praat processes

Praat is a speech-analysis program with a scripting language. praatAudioTools is a large
collection of sound-transformation scripts written for it by Shai Cohen. tui-wave runs those
scripts on your selection and splices the result back in, exactly as it does with CDP.

They share one browser. Open it with `Ctrl+p` and pick **Praat** in the Domain column. The menu
they live under is named ExtProcess.

Install Praat and the scripts first — section 1 covers both.

Chains mix freely. An ExtProcess Chain (`Ctrl+h`) can put a Praat step after a CDP one and back
again; the audio simply passes from each step to the next. A chain made only of Praat processes
does not need CDP installed at all.

### The groups

458 scripts are listed, in fourteen groups. The Groups column follows the plugin's own folders,
so anything written about praatAudioTools names the same groups you see here. Four are shortened
to fit the column: Generative is Generative & Synthesis, Dynamics is Dynamics & Envelope,
Spatial is Spatial & Surround, and Time/Granular is Time & Granular. The **py** group is the one
that needs the Python environment of section 1.

Everything else works as it does for CDP: the parameter form, presets, Preview, Apply and undo.

### Things worth knowing

**Parameter ranges are guesses.** A Praat script declares a starting value for each parameter
but no minimum or maximum, so tui-wave invents a generous range around the starting value. Push
a parameter past anything sensible and Praat simply refuses with a clear message. Nothing is
harmed by trying.

**Generative processes open a new buffer, and most need no file open at all.** Everything in
the Generative group synthesises from scratch: its length comes from its own Duration setting
and its rate from its own Sample Rate setting, neither of which has anything to do with the
selection you launched it from. The result therefore arrives as a new buffer and your original
is left untouched. Set Sample Rate to 96000 and you get a 96kHz buffer.

47 of the 53 read no audio whatever, so you can run them on an empty screen, the way Record
works. Four more read a picture instead, which is the next item. The remaining two do read a
sound, and say so in the dialog when none is open, with Preview and Apply dimmed.

**Four processes read a picture, not a sound.** Percussive, Spectral and Brightness-Controlled
Photo Sonification, and plain Photo Sonification, turn an image into audio. They take an extra
row in the parameter form: press `Enter` on it to open a file picker with a preview pane beside
it, and pick your image there.

**PNG only.** That is Praat's own limit — it links libpng and nothing else — so the picker lists
nothing else. A JPEG or a TIFF fails inside Praat with a read error, which is why tui-wave does
not offer to convert one for you. Until you pick an image the dialog says so and Apply stays
dimmed.

**Some processes draw.** A script that produces a Praat figure shows it in a popup over the
editor when the run finishes. `Esc` closes it. A picture that arrives from a Preview stays up
while the audition loops, so you can judge the two together.

Undo closes that buffer again. There is nothing else for it to undo — nothing was spliced — so
`Ctrl+z` removes what the process made. If you have edited the new buffer, undo reverses those
edits first and closes it on the next press. Once you save it, undo leaves it alone.

**Other processes must keep the document's sample rate.** A transforming process that returns a
different rate cannot be spliced into an existing buffer, so tui-wave refuses and says so rather
than playing it back at the wrong speed.

**Two kinds of preset.** The row at the top of the dialog, labelled Preset, holds parameter sets
you saved yourself. It works the same for every process, CDP or Praat.

Many Praat processes also carry presets of their own, written into the script by its author.
Those appear as a parameter row named **Internal Preset**.

Cycle Internal Preset with Left and Right and the other fields fill in at once, so you can see
what each preset does. The row goes on naming the preset until you change one of those values;
at that point it moves to Custom, which is what makes your edit take effect — left naming a
preset, the script would re-apply it and overwrite you.

A few scripts write their presets in a form tui-wave cannot read. Those still work — the preset
is applied inside the script — but the fields go on showing the manual values.

**Markers are not carried through.** Praat discards marker and broadcast metadata, so a Praat
process returns audio only.

**A run is stopped after two minutes.** Some scripts play their result aloud, which takes as
long as the audio does, and a few can hang outright. A script that draws gets four minutes,
because rendering a figure is slow. The four interactive `py` editors of section 1 get no limit
at all, because you decide when they are done. Press `Esc` to stop any run early.

**Not every script is listed.** 458 of the 499 are. The other 41 cannot be driven without a
window, need a corpus of other files, or work on things that are not sounds.
`docs/praat-excluded-scripts.md` names every one and why.

---

## 16. Airwindows processes

500 effects by Chris Johnson — saturation, console emulations, reverbs, tape, dithers, lo-fi
colour. They live in the same browser as CDP and Praat (`Ctrl+P`), under the **Airwindows**
domain, and can be mixed with either in an ExtProcess Chain.

**There is nothing to install.** Unlike CDP and Praat, the processing is compiled into tui-wave
itself, so this domain works on a fresh install with no configuration and cannot report a
missing tool. It also means previews come back immediately: no program starts and no temporary
file is written, which is most of what makes a CDP or Praat preview take as long as it does.
Nothing round-trips through a file at all, so unlike Praat it cannot lose your cue points or
`bext` metadata.

The Groups column holds Chris Johnson's own categories, so anything written about an Airwindows
plugin — the Airwindopedia, the videos, the forum posts — points at the same bucket here.

### Parameters read 0 to 1

Every Airwindows control is a number from 0.0 to 1.0. That is genuinely how the effects work,
not a simplification made here. Both bounds are finite, so every one of them carries a slider:
Left and Right walk the range, and you never have to guess what a sensible number would be.

Beside each field, dimmed, is the effect's own reading of that value in its own units:

```
  Density     0.0 - 1.0    0.2      = 0.0000
  Highpass    0.0 - 1.0    0.0      = 0.0000
  Out Level   0.0 - 1.0    1.0      = 1.0000
```

Set Density to 1.0 and the readout becomes `4.0000`, because that plugin maps its control to
`(value × 5) − 1`. The mapping differs per effect and per parameter, and it is asked of the
running effect rather than stored — which is why the figure is always right, and why it is
shown rather than the raw number alone.

### Mono and stereo only

Every Airwindows effect is built for exactly two channels, with several genuinely coupling them
(the reverbs, the mid/side processors). So:

- A **stereo** selection is processed as you would expect.
- A **mono** selection is fed to both sides and comes back **stereo**. This is what lets the
  reverbs and stereo wideners build a stereo image from a mono source. `Ctrl+Z` restores the
  buffer to mono.
- A selection of **three or more channels** is refused, stated in the dialog the moment it
  opens, with Apply dimmed. Processing two channels of a 30-channel take and calling it done
  would look plausible and be wrong. To use these on a multichannel file, split it first with
  **File ▸ Export Channels**.

### Reverb tails

An effect with a decay keeps sounding after its input stops, and tui-wave renders that decay
rather than cutting it off at the edge of the selection. It works out how long the tail is by
following the decay until it falls away, so nothing has to be set and effects without a tail
(most of them) are unaffected.

Where the tail goes depends on what follows the selection:

- Selection **at the end of the file**, or a whole file with nothing selected: the tail is
  appended and the file gets longer.
- Selection **in the middle**: the tail rings out over the audio that follows, mixed into it.
  The file does not get longer and nothing shifts in time — the same thing that happens with an
  insert effect in a DAW. If the tail outlasts what remains, the leftover is appended.

Markers keep their positions through either case, and anything after a tail that lengthened the
file moves along with the audio it was attached to.

---

## 17. The Files panel

The Files panel lists the current directory. Give it focus with `Tab`.

| Key | Action |
| --- | --- |
| `Up` / `Down` | Move the selection |
| `Home` / `End` | Jump to the first or last row |
| `PageUp` / `PageDown` | Move one screen |
| `Enter` | Enter a directory, or open a file |
| `/` | Filter the list as you type |
| `Ctrl+o` | Open a directory by path |
| `Ctrl+r` | Rename the selected file on disk |
| `Del` | Delete the selected file from disk |
| `a` | Turn audition on or off |

The panel marks a parent row, the directories, and the audio files. `Enter` on the parent row
moves up one level.

With audition on, tui-wave plays the selected file as you move through the list. The playback
starts after a short pause, so a fast scroll plays nothing.

---

## 18. The Buffers panel

The Buffers panel lists every open file. Give it focus with `Tab` twice.

| Key | Action |
| --- | --- |
| `Up` / `Down` | Switch to that buffer |
| `Enter` | Switch to it and move focus to the waveform |
| `/` | Search the list |
| `Ctrl+s` | Save that buffer |
| `Ctrl+w` | Close that buffer |
| `Ctrl+r` | Rename that buffer |
| `Ctrl+a` | Save every buffer |
| `Ctrl+l` | Reload that buffer from disk |

Moving the selection loads the buffer at once, so there is nothing left for `Enter` to commit.
It hands focus to the waveform instead, because picking a buffer is almost always followed by
editing it.

The panel also lists the pitch curves and formant buffers of section 14, tagged `[p]`, `[f]`
and `[s]`.

These `Ctrl` keys mean something else in the Waveform panel. `Ctrl+r` is Reverse there, and
`Ctrl+a` is Select All. The panel with focus decides.

Reload reads the file from disk again: samples, markers, metadata, and bit depth. It then clears
the undo history for that buffer, because the old commands hold sample data from before the
reload. A buffer that you never saved has no path to reload from, so the command does nothing.

tui-wave asks first if the buffer has changes.

---

## 19. Mouse

The mouse works alongside the keyboard.

| Action | Result |
| --- | --- |
| Click the waveform | Move the play position |
| Drag across the waveform | Make a selection |
| Double-click the waveform | Select the region between the markers either side of the click |
| Wheel over the waveform | Move the channel window |
| Wheel over a panel | Scroll the list |
| Click a menu title | Open that menu |
| Click a toolbar button | Run that command |
| Drag a marker line | Move the marker |
| Drag a head or tail mark | Move that mark |
| Double-click a marker label | Rename the marker |
| Double-click a file in the Files panel | Open it |

With no marker on one side of a double-click, that edge of the region is the start or the end
of the file. With zero-crossing snap on, both edges snap.

`Shift`+click never reaches tui-wave. Terminals keep that gesture for their own text selection.
Nothing in the editor is bound to it — see the envelope editor in section 14, where a
double-click removes a point for exactly this reason.

---

## 20. Display modes

Press `g` to turn graphics mode on or off.

In graphics mode tui-wave draws the waveform as a real image, through the terminal graphics
protocol. Kitty and other modern terminals support this. The result is much sharper, and it
draws faster on files with many channels.

Without graphics mode tui-wave draws with braille dots and block glyphs. That works in every
terminal. The View menu also holds a Gradient toggle for both modes.

Both modes draw an amplitude-zero line across the centre of each pane.

---

## 21. Configuration

tui-wave writes its settings to `~/.config/tui-wave/config.toml`. It saves the file whenever you
change a toggle, so your settings come back on the next start.

Useful settings:

| Setting | Meaning |
| --- | --- |
| `max_resident_mb` | Largest decoded size, in MB, to load into memory. Default 4096 |
| `cdp_dir` | Path to your CDP installation |
| `praat_bin` | Path to the Praat program. Empty means find it on your `PATH` |
| `praat_audiotools_dir` | Path to the praatAudioTools scripts. Defaults to the bundled copy |
| `graphics_mode` | Draw with terminal graphics |
| `dot_matrix_gradient` | Shade the waveform by amplitude |
| `time_ruler` | Show the time ruler row |
| `snap_to_zero` | Zero-crossing snap |
| `fine_mode` | Fine step mode |
| `auto_vertical_zoom` | Fit the amplitude zoom to the peak |
| `loop_playback` | Loop playback |
| `cursor_follows_playback` | The cursor follows playback |
| `viewport_follows_playback` | The view follows playback |
| `audition` | Play the file under the Files panel highlight |
| `transient_threshold_db` | Threshold for transient markers |
| `keybindings` | Your own key assignments |

Every toggle in that list is one you set from the View menu or a key. The file records where
you left it, so it comes back on the next start.

Raise `max_resident_mb` to edit larger files in memory. Remember that a file open for playback
costs about twice its decoded size. A 4GB buffer with playback running therefore needs about 8GB.

Lower `max_resident_mb` to send more files to the streamed read-only path.

The value is a fixed number, not a share of free memory. The same file therefore behaves the
same way every day.

File then Reset Config to Defaults throws away your settings.

---

## 22. Key reference

The Waveform panel must have focus for these keys, unless the table says otherwise.

### Navigation and view

| Key | Action |
| --- | --- |
| `Left` / `Right` | Move the cursor |
| `Home` / `End` | Jump to start or end |
| `PageUp` / `PageDown` | Move one screen |
| `Up` / `Down` | Zoom along time |
| `Shift+Up` / `Shift+Down` | Zoom along amplitude |
| `a` | Auto vertical zoom |
| `Backtick` | Fine step mode |
| `z` | Zero-crossing snap |
| `g` | Graphics mode |
| `,` / `.` | Channel window up or down |
| `<` / `>` | Channel window one page |
| `Tab` / `Shift+Tab` | Move focus |
| `F10` / `Alt`+letter | Open the menu bar |
| `?` | Open the key reference |

### Selection

| Key | Action |
| --- | --- |
| `Shift+Left` / `Shift+Right` | Extend the selection |
| `Shift+Home` / `Shift+End` | Extend to start or end |
| `Shift+PgUp` / `Shift+PgDn` | Extend one screen |
| `Ctrl+a` | Select all |
| `Ctrl+d` | Clear the selection |
| `{` / `}` | Extend to the previous or next marker |

### Edit

| Key | Action |
| --- | --- |
| `Ctrl+x` | Cut |
| `Ctrl+c` | Copy |
| `Ctrl+v` | Paste |
| `Del` | Delete |
| `C` | Copy to new buffer |
| `Ctrl+z` | Undo |
| `Ctrl+y` or `Ctrl+Shift+z` | Redo |

### Process

| Key | Action |
| --- | --- |
| `Ctrl+r` | Reverse |
| `Ctrl+n` | Normalize |
| `Ctrl+g` | Gain |
| `Ctrl+f` | Fade In |
| `Ctrl+o` | Fade Out |
| `Ctrl+t` | Trim |
| `Ctrl+e` | Resample |
| `Ctrl+b` | Technical Fades |
| `Ctrl+m` | Mix to Mono |

### Markers

| Key | Action |
| --- | --- |
| `m` / `M` | Insert or delete a marker |
| `h` / `H` | Insert or delete a head or tail mark |
| `[` / `]` | Jump to the previous or next marker |
| `t` | Markers at transients |
| `+` / `-` | Raise or lower the transient threshold |
| `/` / `\` | Next or previous rising edge |

### Files and transport

| Key | Action |
| --- | --- |
| `Ctrl+s` | Save |
| `Shift+S` or `Ctrl+Shift+S` | Save As |
| `Ctrl+l` | Save All |
| `Shift+E` | Export Regions |
| `L` / `R` | New buffer from the left or right channel |
| `Space` | Play or pause |
| `l` | Loop playback |
| `i` | Cursor follows playback |
| `f` | View follows playback |
| `Ctrl+p` | ExtProcess |
| `Ctrl+h` | ExtProcess Chain |
| `q` | Quit |

### Menu-only commands

These have no key. Open the menu named beside each.

| Menu | Command |
| --- | --- |
| File | Export Channels, Export to FLAC or MP3, Reset Config to Defaults |
| View | Gradient, Time Ruler |
| Process | Mix Multichannel to Stereo, Remove Empty Channels, Remove DC Offset, High-Pass Filter |
| ExtProcess | CDP Extract Pitch Curve, CDP Load Pitch Curve, CDP Extract Formants (both), CDP Freeze Formant Snapshot at Cursor, Configure CDP Directory |

---

