# tui- — User Guide

A keyboard-driven terminal audio editor for WAV files. Runs in your terminal on Linux, macOS, or Windows.

---

The UI has three panels: **Files** (left), **Buffers** (middle), **Waveform** (right). Press `Tab` to cycle focus between them.

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
| `` ~ `` (tilde) | Toggle **fine mode** (tiny cursor steps) |
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
| `Ctrl+B` | Technical fades (5ms fade at both ends) |

### Channel operations

| Key | Operation |
|-----|-----------|
| `Ctrl+M` | Mix to mono (per-channel levels) |
| `Shift+L` | New buffer from left channel |
| `Shift+R` | New buffer from right channel |

---

## Playback

| Key | What it does |
|-----|-------------|
| `Space` | Play / Pause from cursor |
| `L` | Toggle loop playback (selection loops if active, else whole file) |
| `i` | Toggle: cursor follows playback stop position |
| `f` | Toggle: viewport scrolls to follow playhead |

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

Markers are saved as BWF cue points and survive round-trips (open, edit, save).

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
| `PgUp` / `PgDn` | Page through list |

### Buffer panel (middle)

| Key | What it does |
|-----|-------------|
| `↑` `↓` | Switch buffers |
| `Enter` | Confirm buffer switch |
| `/` | Search / filter |
| `Ctrl+S` | Save buffer |
| `Ctrl+W` | Close buffer |
| `Ctrl+R` | Rename buffer |
| `Ctrl+A` | Save all |

---

## Toggles

Press these without `Ctrl`:

| Key | Toggle |
|-----|--------|
| `z` | Zero-crossing snap (edges cut at silent points) |
| `a` | Auto vertical zoom (fits waveform to window) |
| `g` | Graphics mode (pixel-precise rendering via kitty/Sixel/iTerm2) |
| `i` | Cursor follows playback |
| `f` | Viewport follows playback |
| `` ` `` | Fine navigation mode |
| `L` | Loop playback |

Enabled toggles show green labels in the toolbar.

---

## Save formats

| Key | Format |
|-----|--------|
| `Ctrl+S` | Save as 32-bit float |
| `Ctrl+Shift+S` | Save As (choose: **16-bit**, **24-bit**, or **32-bit float**) |

In the Save As dialog:
- `Tab` — cycle bit depth
- `Ctrl+D` — toggle dither (adds noise-shaped rounding for 16/24-bit)
- `Enter` — confirm
- `Esc` — cancel

---

## Config

Settings are saved to `~/.config/tui-wave/config.toml`. Includes: snap-to-zero, auto vertical zoom, fine mode, loop, audition, playback follow modes, transient threshold, and graphics mode. You can edit the file directly or change toggles in-app.

---

## Tips

- **Fine mode** (`` ` ``) + arrow keys = sample-accurate positioning. Great for surgical edits.
- **Transient detection** (`/` `?` `t`) uses a 10ms sliding RMS window. Adjust sensitivity with `+`/`-`.
- **Zero-crossing snap** on cuts prevents clicks. Enable with `z` before cutting or deleting.
- **Graphics mode** (`g`) renders the waveform as a bitmap at pixel resolution in terminal emulators that support kitty-graphics. Falls back to text mode if they do not.
- **Arrow key acceleration** — held arrows speed up after a moment (disabled in fine mode).
- **Quit** (`q` / `Q`) warns about unsaved buffers and offers to save all.
- **Mouse:** click to focus panels, drag on waveform to select, click toolbar buttons, click menu items, scroll file lists, drag markers.
