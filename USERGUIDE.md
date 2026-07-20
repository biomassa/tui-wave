# tui-wave — User Guide

A keyboard-driven terminal audio editor for WAV files. Runs in your terminal on Linux,
macOS, or Windows (currently untested).

---

The UI has three panels: **Files** (left), **Buffers** (middle), **Waveform** (right).
`Tab` moves focus forward through them, `Shift+Tab` backward. A click anywhere also
focuses that panel directly.

---

## Quick Start

1. Open a WAV file (navigate the file panel with arrow keys, press `Enter`)
2. `Space` — play/pause from cursor
3. `Left`/`Right` — move cursor
4. Make a selection (`Shift`+arrows or click-drag) and try:
   - `Ctrl+X` — cut
   - `Ctrl+C` — copy
   - `Ctrl+V` — paste
   - `Ctrl+R` — reverse
5. `Ctrl+S` — save
6. `Ctrl+Z` — undo

## Navigation

| Key | What it does |
|-----|-------------|
| `←` `→` | Move cursor |
| `↑` `↓` | Zoom in/out horizontally |
| `Shift`+`↑` `↓` | Zoom in/out vertically |
| `Home` / `End` | Jump to start / end of file |
| `PgUp` / `PgDn` | Coarse backward / forward |
| `` ` `` (backtick) | Toggle **fine mode** (tiny cursor steps) |
| `[` / `]` | Jump to previous / next marker |
| `{` / `}` | Extend selection to previous / next marker |
| `/` / `?` | Jump to next / previous transient (onset) |
| `+` / `-` | Increase / decrease transient sensitivity |

---

## Selection

- **Keyboard:** Hold `Shift` and use `←` `→` `Home` `End` `PgUp` `PgDn`
- **Mouse:** Click and drag across the waveform
- `Ctrl+A` — select entire file
- `Ctrl+D` — clear selection

---

## Editing

| Key | Operation |
|-----|-----------|
| `Ctrl+X` | Cut to clipboard |
| `Ctrl+C` | Copy to clipboard |
| `Ctrl+V` | Paste at cursor |
| `Delete` | Delete selection (with zero-crossing snap) |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` (or `Ctrl+Shift+Z`) | Redo |
| `Shift+C` | Copy selection to a new buffer |

---

## Audio Processing

All operations work on the selection, or the whole file if nothing is selected. All are undoable.

| Key | Operation |
|-----|-----------|
| `Ctrl+R` | Reverse audio |
| `Ctrl+N` | Normalize (set peak level, default 0dB) |
| `Ctrl+G` | Gain (with optional soft-clip) |
| `Ctrl+F` | Fade in (Exp / Log / Linear) |
| `Ctrl+O` | Fade out (Exp / Log / Linear) |
| `Ctrl+T` | Trim to selection (discards everything outside) |
| `Ctrl+E` | Resample (change sample rate) |
| `Ctrl+B` | Technical fades (5ms fade at both ends — the standard pre-export click guard) |

### Channels

| Key | Operation |
|-----|-----------|
| `Ctrl+M` | Mix to mono (per-channel levels, optional soft-clip) |
| `Shift+L` | New buffer from left channel |
| `Shift+R` | New buffer from right channel |

### Export Regions to Subfolder

`Shift+E` chops the current buffer at its markers and saves each region as its own WAV
file — useful for splitting a long recording into numbered takes or tracks. It opens a
dialog with:

- **Subfolder** — created inside the current file's directory (or the file panel's
  current directory for a never-saved buffer)
- **Base name** — files are written as `basename-001.wav`, `basename-002.wav`, …
- **Format** — 16-bit, 24-bit, or 32-bit float, cycled with `←`/`→`
- **Dither** — toggled with `Space`, only available for 16/24-bit
- **Fade in / Fade out** — each an independent checkbox plus an editable length in
  milliseconds (default 5ms), applied to the start/end of every exported region

`Tab` / `Shift+Tab` move between fields. The subfolder and base name are required —
`Enter` (or clicking **Do!**) does nothing until both are filled in. `Esc` cancels.

The first region always runs from the start of the file to the first marker, and the
last from the last marker to the end of the file — so a file with no leading/trailing
marker still gets its intro/outro captured as their own regions.

### CDP processes

`Ctrl+P` (or **Process → CDP Process…**) opens a dialog-driven front-end to the
[Composer's Desktop Project](https://www.composersdesktop.com) (CDP) — a large, decades-old
suite of external command-line sound-transformation tools (blur, stretch, morph, spectral
filtering, granular processing, and hundreds more). CDP itself is free software maintained by
the Composer's Desktop Project, not bundled with tui-wave — see
[README.md § Optional: CDP support](README.md#optional-cdp-composers-desktop-project-support)
for what it is, full credits, and how to install or build it. tui-wave's built-in catalog of
process definitions is adapted from [SoundThread](https://github.com/j-p-higgins/SoundThread)
(MIT license, see `THIRD_PARTY_NOTICES.md`).

**First run.** The CDP directory defaults to `~/cdp` (`~` resolved to your actual home
directory at startup) — if that's where you unpacked or built CDP, no setup is needed. If it
doesn't validate there, the dialog asks for one — enter the path to the folder containing the
CDP binaries (e.g. `pvoc`, `modify`, `blur`). You can change it later from
**Options → Configure CDP Directory…**. The setting is saved in your config file (`cdp_dir`).

**Browsing.** A dedicated, fixed-size dialog (it doesn't resize as you scroll) lists every
matching process with the highlighted one's description alongside it. Type to filter (matches
the title, id, and short description); `↑`/`↓` selects one at a time, `PgUp`/`PgDn` a page at a
time; `Enter`, or clicking a process's name, opens its parameter form; `Esc` backs out.

**Parameters.** Selecting a process opens a second dialog — sized to fit that process's own
parameters, scrolling if there are more than fit your terminal — with a form of its controls:

- **Number** fields — type a value, or nudge by the parameter's step with `↑`/`↓`. Out-of-range
  values are rejected on run with an inline message.
- **Toggle** fields — `Space` flips them.
- **Choice** fields — `←`/`→` cycle the options.
- `Tab` / `Shift+Tab` move between fields (and the preset row, Preview, and Apply).
- A parameter's label is shown in **green** when it can be automated with a breakpoint envelope
  instead of a fixed value — see below.

**Automating a parameter (breakpoint envelopes).** With a green (automatable) Number field
focused, press `e` to open its envelope editor — a graph of value against time, seeded with two
flat points at the field's current value. In terminals that support kitty/Sixel/iTerm2 graphics
it renders as a real curve over a dimmed reference waveform of the audio being processed;
otherwise it falls back to an ASCII staircase.

| Key | What it does |
|-----|-------------|
| `←` / `→` | Select the previous / next point |
| `Shift+←` / `→` | Move the selected point's time |
| `↑` / `↓` | Change the selected point's value (coarse) |
| `Shift+↑` / `↓` | Change the selected point's value (fine, exact parameter step) |
| `n` | Insert a new point |
| `Delete` | Remove the selected point |
| `c` | Discard the envelope, back to a plain constant value |
| `Enter` | Save the envelope and close the editor |
| `Esc` | Discard changes made this session and close the editor |

Mouse: click selects the nearest point, double-click inserts one, drag moves the selected
point, `Shift`+drag moves it at reduced (fine) speed, and `Shift`+click deletes the nearest
point. A field showing an envelope reads "envelope (N pts, e to edit)" in place of its value;
running the process on it emits a CDP breakpoint (`.brk`) file instead of a constant.

**Presets.** The parameter dialog has a preset row above the fields: `←`/`→` cycles through
presets saved for *this* process (loading the selected one's values immediately), `s` opens a
name prompt to save the current values (prefilled with the current preset's name, so re-saving
is just `Enter`), and `d` deletes the selected preset. Presets are stored per process as
`$XDG_CONFIG_HOME/tui-wave/cdp_presets/<process-key>.toml`.

**Preview and Apply.** `Enter` runs the process on the current selection (or the whole file if
nothing is selected) and splices the result back in — fully undoable with `Ctrl+Z`. Tab to
**Preview** first to hear the result through your speakers *without* modifying the document;
if you then Apply without changing any parameter, it reuses the already-rendered audio instead
of running CDP again. After a process is applied the selection is cleared and the cursor sits
at the start of the result, so `Space` plays it straight away.

**Spectral processes** (blur, morph, spectral filtering, …) are wrapped automatically: the app
runs CDP's phase-vocoder analysis and resynthesis around them at a sensible default resolution,
so you just pick the process and never deal with `.ana` files or FFT settings by hand.

**Dual-input processes** (combine, morph, vocode, …) take a second sound. The parameter form
gains a **2nd input** row — `←`/`→` picks which open buffer supplies it (open the other file
in another buffer first). The second buffer is used whole; both inputs must share a sample rate.

**Errors.** If CDP rejects the input or parameters, its own error text is shown in a scrollable
viewer (`↑`/`↓`/`PgUp`/`PgDn` to scroll, `Enter`/`Esc` to dismiss). A long-running process shows
a progress dialog with a step counter; `Esc` cancels it.

**Adding processes.** The built-in catalog covers ~130 common CDP processes. To add or override
one, drop a `*.toml` file (same schema as the built-in catalog) into
`$XDG_CONFIG_HOME/tui-wave/cdp/` — see `docs/cdp-custom-process-example.toml` in the source tree
for a worked example. A file reusing an existing process's `key` replaces the built-in
definition; a new key adds a process.

---

## Playback

| Key | What it does |
|-----|-------------|
| `Space` | Play / Pause from cursor |
| `L` | Toggle loop playback (selection loops if active, else whole file) |
| `i` | Toggle: cursor follows playback stop position |
| `f` | Toggle: viewport scrolls to follow playhead |

When a track plays to the end on its own, the next `Space` replays from the cursor —
it doesn't need pressing twice.

---

## Markers

| Key | What it does |
|-----|-------------|
| `m` | Insert marker at cursor |
| `Shift+M` | Delete nearest marker |
| `t` | Auto-insert markers at all transients |
| `[` / `]` | Jump to previous / next marker |
| `{` / `}` | Extend selection to prev / next marker |
| Drag | Move marker with mouse |
| Double-click label | Rename marker |

Markers are saved as BWF cue points and survive round-trips (open, edit, save). They're
also what `Shift+E` (Export Regions) splits the file on.

---

## File & Buffer Management

### File panel (left)

| Key | What it does |
|-----|-------------|
| `↑` `↓` | Navigate list |
| `Enter` | Open file / enter directory |
| `/` | Search / filter |
| `Ctrl+O` | Open a directory |
| `a` | Preview (audition) file without loading |
| `Ctrl+r` | Rename the selected `.wav` on disk |
| `Delete` | Delete the selected `.wav` from disk (asks to confirm first) |
| `PgUp` / `PgDn` | Page through list |

Rename and delete act on the file itself, not a buffer — if that file happens to be open,
the open buffer is kept in sync (renamed) or left in memory, still re-savable (deleted).

### Buffer panel (middle)

| Key | What it does |
|-----|-------------|
| `↑` `↓` | Switch buffers |
| `Enter` | Confirm buffer switch |
| `/` | Search / filter |
| `Ctrl+S` | Save buffer |
| `Ctrl+W` | Close buffer |
| `Ctrl+r` | Rename buffer (also renames the file on disk, if it has one) |
| `Ctrl+A` | Save all |

A never-saved buffer is labeled `_NEW_001`, `_NEW_002`, … in both the Buffers panel and
the waveform header, until you give it a name with Save or Save As.

---

## Toggles

Press these without `Ctrl`:

| Key | Toggle |
|-----|--------|
| `z` | Zero-crossing snap (edges cut at silent points) |
| `a` | Auto vertical zoom (fits waveform to the loudest visible peak) |
| `g` | Graphics mode (pixel-precise rendering via kitty/Sixel/iTerm2) |
| `i` | Cursor follows playback |
| `f` | Viewport follows playback |
| `` ` `` | Fine navigation mode |
| `L` | Loop playback |

Enabled toggles show highlighted labels in the toolbar.

### Auto vertical zoom and the dB scale

The dB gutters on either side of the waveform are always absolute dBFS — 0dB is always
literal full scale, never relabeled to match the loudest thing on screen. With auto
vertical zoom **off**, the scale is fixed at 0/-3/-6/-12/-18/-24 (and continues further in
6dB steps if you manually zoom in past -24). With it **on**, the view refits to the
loudest visible peak every frame, which pushes 0dB off the top edge for a quiet passage —
so you'll see the scale start at whatever the true peak level is (e.g. -18, -24, -30…)
instead. The exact peak level is also marked precisely (it can be any dB value, not just
a multiple of 3 or 6 — e.g. "-17"), so you always know exactly how loud the loudest
visible moment really is.

---

## Save formats

| Key | Format |
|-----|--------|
| `Ctrl+S` | Quick save, at the file's original bit depth (16/24-bit int, or 32-bit float) |
| `Ctrl+Shift+S` | Save As (choose: **16-bit**, **24-bit**, or **32-bit float**) |
| `Ctrl+L` | Save all open buffers |

In the Save As dialog:
- `Tab` / `Shift+Tab` — cycle bit depth field forward/backward
- `Ctrl+D` — toggle dither (adds noise-shaped rounding for 16/24-bit)
- `Enter` — confirm
- `Esc` — cancel

---

## Confirmations

Quitting or closing a buffer with unsaved changes, deleting a file, and resetting your
keybindings to defaults all ask first:

- `y` (or `s` where "save" is offered) — proceed
- `Esc` — cancel (any other key also cancels)

**File → Reset Config to Defaults** additionally backs up your current
`config.toml` to `config.toml.bak` before overwriting it, so a reset is always
recoverable.

---

## Config

Settings are saved to `~/.config/tui-wave/config.toml`. Includes: snap-to-zero, auto
vertical zoom, fine mode, loop, audition, playback follow modes, transient threshold,
graphics mode, and all keybindings. You can edit the file directly or change toggles
in-app — remapped keys immediately update every menu/toolbar shortcut hint to match.

---

## Tips

- **Fine mode** (`` ` ``) + arrow keys = sample-accurate positioning. Great for surgical edits.
- **Transient detection** (`/` `?` `t`) uses a 10ms sliding RMS window. Adjust sensitivity with `+`/`-`.
- **Zero-crossing snap** on cuts prevents clicks. Enable with `z` before cutting or deleting.
- **Graphics mode** (`g`) renders the waveform as a bitmap at pixel resolution in terminal emulators that support kitty-graphics. Falls back to text mode if they do not.
- **Arrow key acceleration** — held arrows speed up after a moment (disabled in fine mode).
- **Quit** (`q` / `Q`) warns about unsaved buffers and offers to save all.
- **Mouse:** click to focus panels, drag on waveform to select, click toolbar buttons, click menu items (including entries that overlap a side panel), scroll file lists, drag markers.
