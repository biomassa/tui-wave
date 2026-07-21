# tui-wave — User Guide

A keyboard-driven terminal audio editor for WAV files. Runs on Linux, macOS, or Windows
(Windows currently untested).

Three panels: **Files** (left), **Buffers** (middle), **Waveform** (right). `Tab` /
`Shift+Tab` cycle focus; a click focuses a panel directly. The menu bar opens with `F10`
or `Alt`+the highlighted letter.

## Quick Start

1. Open a WAV: arrow through the file panel, press `Enter`.
2. `Space` plays/pauses from the cursor; `←`/`→` move the cursor.
3. Select with `Shift`+arrows or click-drag, then `Ctrl+X` cut · `Ctrl+C` copy · `Ctrl+V`
   paste · `Ctrl+R` reverse.
4. `Ctrl+S` saves, `Ctrl+Z` undoes.

## Navigation

| Key | Action |
|-----|--------|
| `←` `→` | Move cursor |
| `↑` `↓` | Zoom horizontally |
| `Shift`+`↑` `↓` | Zoom vertically |
| `Home` `End` | Jump to start / end |
| `PgUp` `PgDn` | Coarse back / forward |
| `[` `]` | Previous / next marker |
| `/` `?` | Next / previous transient (onset) |
| `+` `-` | Transient sensitivity up / down |
| `` ` `` | Toggle **fine mode** (tiny cursor steps) |

Held arrows accelerate after a moment — except in fine mode, which stays sample-precise.

## Selection

- **Keyboard:** `Shift` + `←` `→` `Home` `End` `PgUp` `PgDn`, or `{` / `}` to a marker.
- **Mouse:** click-drag across the waveform. Double-click selects the region between the
  two nearest markers (or the file edges).
- `Ctrl+A` selects all · `Ctrl+D` clears the selection.

## Editing

| Key | Action |
|-----|--------|
| `Ctrl+X` `Ctrl+C` `Ctrl+V` | Cut · Copy · Paste at cursor |
| `Delete` | Delete selection |
| `Shift+C` | Copy selection to a new buffer |
| `Ctrl+Z` / `Ctrl+Y` | Undo / Redo (`Ctrl+Shift+Z` also redoes) |

Undo is per-buffer and many levels deep.

## Audio Processing

Everything here works on the selection, or the whole file if nothing is selected, and is
undoable.

| Key | Operation |
|-----|-----------|
| `Ctrl+R` | Reverse |
| `Ctrl+N` | Normalize (peak level, default 0 dB) |
| `Ctrl+G` | Gain (optional soft-clip) |
| `Ctrl+F` / `Ctrl+O` | Fade in / out (Exp / Log / Linear) |
| `Ctrl+B` | Technical fades — 5 ms at both ends, the standard pre-export click guard |
| `Ctrl+T` | Trim to selection |
| `Ctrl+E` | Resample (change sample rate) |
| `Ctrl+M` | Mix to mono (per-channel levels, optional soft-clip) |
| `Shift+L` / `Shift+R` | New buffer from left / right channel |

### Export Regions (`Shift+E`)

Chops the buffer at its markers and writes each region as its own numbered WAV — handy
for splitting a long recording into takes. The dialog sets a subfolder (created next to
the file), a base name (`base-001.wav`, `base-002.wav`, …), bit depth (`←`/`→`), optional
dither (`Space`, 16/24-bit only), and independent fade-in/out lengths. `Tab` moves between
fields; **Do!** (or `Enter`) is inactive until the subfolder and base name are filled.

The first region always runs from the file start to the first marker and the last from the
last marker to the end, so intros and outros are captured too.

## CDP processes

`Ctrl+P` (or the **CDP** menu) opens a front-end to the
[Composer's Desktop Project](https://www.composersdesktop.com) — a large suite of external
command-line sound-transformation tools (blur, stretch, morph, spectral filtering, granular
processing, and hundreds more). CDP is free software, not bundled with tui-wave; see
[README.md § Optional: CDP support](README.md#optional-cdp-composers-desktop-project-support)
for install/build and credits. The built-in catalog (~130 processes) is adapted from
[SoundThread](https://github.com/j-p-higgins/SoundThread) (see `THIRD_PARTY_NOTICES.md`).

**First run.** The CDP directory defaults to `~/cdp`; if that's where your binaries live,
nothing to set up. Otherwise the dialog asks for the folder (containing `pvoc`, `modify`,
`blur`, …). Change it any time via **CDP → Configure CDP Directory…**; it's saved as
`cdp_dir` in your config.

**Browse and run.** Type to filter the process list; `↑`/`↓` (or `PgUp`/`PgDn`) select,
`Enter` opens the parameter form. Small badges flag what a process needs — `>1 inputs`,
`pitch curve`, `formants`, `snapshot`. In the form:

- **Number** fields: type a value or nudge with `↑`/`↓`. **Toggle**: `Space`.
  **Choice**: `←`/`→`.
- `Tab` / `Shift+Tab` walk the fields plus the preset row, **Preview**, and **Apply**.
- `Enter` runs on the selection (or whole file) and splices the result in — undoable.
  Tab to **Preview** to audition through your speakers *without* changing the document; an
  unchanged Apply afterward reuses that render instead of re-running CDP.
- Spectral processes are wrapped in phase-vocoder analysis/resynthesis automatically — you
  never touch `.ana` files. Dual-input processes grow a **2nd input** row (`←`/`→` picks
  another open buffer, used whole; sample rates must match).
- On rejection, CDP's own error text appears in a scrollable viewer.

**Automating a parameter.** A field shown in **green** accepts a breakpoint envelope:
focus it and press `e` for an editor graphing value against time (a real curve over a
dimmed reference waveform where kitty/Sixel/iTerm2 graphics are available, else an ASCII
staircase).

| Key | Action |
|-----|--------|
| `←` `→` | Select previous / next point |
| `Shift`+`←` `→` | Move the point's time |
| `↑` `↓` / `Shift`+`↑` `↓` | Change its value (coarse / fine) |
| `n` · `Delete` | Insert · remove a point |
| `c` | Discard the envelope, back to a constant |
| `Enter` · `Esc` | Save · discard and close |

Mouse: click selects the nearest point, double-click inserts, drag moves (`Shift`-drag =
fine), `Shift`-click deletes.

**Presets.** Above the fields: `←`/`→` loads a saved preset for this process, `s` saves the
current values (prefilled name — re-saving is just `Enter`), `d` deletes one. Stored under
`$XDG_CONFIG_HOME/tui-wave/cdp_presets/`.

**Adding processes.** Drop a `*.toml` (same schema as the built-in catalog) into
`$XDG_CONFIG_HOME/tui-wave/cdp/` — reusing a `key` overrides a built-in, a new key adds one.
See `docs/cdp-custom-process-example.toml` for a worked example.

### Pitch curves

Some CDP processes take a **pitch curve** — a time/Hz contour — instead of a fixed number.

1. **CDP → Extract Pitch Curve** analyses the selection (best on a clear monophonic
   note/melody) and adds a `[p]` row to the Buffers panel.
2. `Enter` on that row opens a Time/Hz table editor: arrows select, type overwrites, `n`
   inserts, `Delete` removes, `t` applies a CDP curve transform (quantise, smooth, vibrato,
   pitch-shift, …), `Enter` commits, `Esc` discards. `Ctrl+Z`/`Ctrl+Y` here undo/redo the
   curve, not the document.
3. To drive a process with it: open any `pitch curve`-badged process (e.g. Psow Stretch),
   focus its pitch field, press `e` then `c` to load an open curve into the envelope,
   rescaled to the selection.
4. `Ctrl+S` on a curve row saves it to disk; **CDP → Load Pitch Curve…** reads one back.
   A hand-typed or loaded curve can be edited but can't run a transform (no CDP source).

### Formants

1. **CDP → Extract Formants** captures the selection's spectral envelope as a `[f]`
   buffer (best on voice or an instrument with real timbre). `Enter` shows a read-only info
   popup.
2. **CDP → Freeze Formant Snapshot at Cursor** freezes the timbre at the cursor into a
   `[s]` buffer. If the current document has no formants yet, it extracts them first
   automatically.
3. Impose either onto other audio: open **Formants Put** (uses `[f]` buffers) or
   **Oneform Put** (uses `[s]` buffers), press `b` to pick the buffer, then Apply.

## Playback

| Key | Action |
|-----|--------|
| `Space` | Play / pause from cursor |
| `L` | Loop (selection if any, else whole file) |

After a track plays to its end, the next `Space` replays from the cursor. Cursor- and
viewport-follow behavior are the `i` and `f` toggles below.

## Markers

| Key | Action |
|-----|--------|
| `m` · `Shift+M` | Insert at cursor · delete nearest |
| `t` | Auto-insert at every transient |
| `[` `]` | Jump previous / next |
| `{` `}` | Extend selection to previous / next marker |

Drag a marker to move it; double-click its label to rename. Markers persist as BWF cue
points across save/reload, and are what Export Regions splits on.

## Files & Buffers

**File panel** (left) — `↑`/`↓` navigate, `Enter` opens a file or enters a directory, `/`
filters, `Ctrl+O` opens a directory, `a` auditions without loading, `Ctrl+R` renames and
`Delete` deletes the `.wav` on disk (rename/delete keep any open buffer in sync).

**Buffer panel** (middle) — `↑`/`↓` switch buffers, `/` filters, `Ctrl+S` saves, `Ctrl+W`
closes, `Ctrl+R` renames (and its file, if any), `Ctrl+A` saves all. Rows are tagged by
type: audio, `[p]`, `[f]`, `[s]`. A never-saved buffer shows as
`_NEW_001`, `_NEW_002`, … until you name it.

## Toggles

Pressed without `Ctrl`; enabled ones highlight in the toolbar.

| Key | Toggle |
|-----|--------|
| `z` | Zero-crossing snap (edits cut at silent points, avoiding clicks) |
| `a` | Auto vertical zoom (fit to the loudest visible peak) |
| `g` | Graphics mode (pixel-precise rendering via kitty/Sixel/iTerm2) |
| `i` · `f` | Cursor · viewport follows playback |
| `L` | Loop playback |
| `` ` `` | Fine navigation |

**dB scale.** The gutters flanking the waveform are always absolute dBFS — 0 dB is literal
full scale, never relabeled. With auto vertical zoom **off** the scale is fixed
(0/-3/-6/-12/-18/-24, extending in 6 dB steps as you zoom in). **On**, the view refits the
loudest visible peak each frame, so a quiet passage pushes 0 dB off the top and the scale
starts at the true peak (marked precisely, e.g. "-17") — you always know how loud the
loudest visible moment really is.

## Save formats

| Key | Format |
|-----|--------|
| `Ctrl+S` | Quick save at the file's original depth (16/24-bit int or 32-bit float) |
| `Ctrl+Shift+S` | Save As — choose 16-bit, 24-bit, or 32-bit float |
| `Ctrl+L` | Save all buffers |

In Save As: `Tab` cycles the depth field, `Ctrl+D` toggles dither (noise-shaped rounding
for 16/24-bit), `Enter` confirms, `Esc` cancels.

## Confirmations & config

Quitting or closing with unsaved changes, deleting a file, and resetting keybindings all
prompt first — `y` (or `s` where "save" is offered) proceeds, any other key cancels.
**File → Reset Config to Defaults** backs up `config.toml` to `config.toml.bak` first.

Settings live in `~/.config/tui-wave/config.toml` (toggles, transient threshold, and all
keybindings). Edit it directly or change toggles in-app — remapped keys immediately update
every menu and toolbar hint.
