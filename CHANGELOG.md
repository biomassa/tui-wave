# Changelog

## 2026-06-28

- Fixed Auto-Insert Markers missing a transient at the very start of a file: if the
  opening audio decays significantly into the second analysis frame, position 0 is now
  correctly marked.
- Fixed the transient threshold not actually defaulting to 13 dB on a fresh install
  (the toolbar field was updated but the config default wasn't).

- Added a **Channels** menu and toolbar group with three commands: **Mix to Mono**
  (`Ctrl+m`), **New from Left** (`L`), and **New from Right** (`R`). All three are
  selection-aware — if a selection is active, only that range goes into the new buffer;
  otherwise the whole file does.
- Mix to Mono opens a dialog to set per-channel gain in dB (`0` = unity, `-inf` = silence
  that channel). Tab cycles through fields and the tanh soft-limiter toggle; Del sets the
  current field to `-inf`.
- The selected-range waveform is now rendered as a dark bar on a cyan background instead of
  yellow-on-dark, giving much higher contrast.
- The dB scale no longer pins 0 dB to the top row when zoomed in vertically — marks that
  fall outside the visible amplitude range disappear, so the scale always reflects what's
  actually on screen.
- The transient detection threshold defaults to 13 dB (was 6 dB).
- Added graphics-mode waveform rendering (kitty/Sixel/iTerm2): when a supported terminal
  is detected, the waveform is drawn as a real bitmap at pixel resolution rather than
  character blocks. Toggled with `g`; persists between sessions. Falls back to text mode
  silently in tmux, screen, or unsupported terminals.

## 2026-06-25

- Added an **Audition** toggle to the Files panel: navigating to a file previews it by
  playing straight from disk, without loading it into a buffer. A single click in the
  Files panel now selects (and auditions, if enabled); double-click opens.
- Added PgUp/PgDn paging in the Files panel for browsing directories with many files.
- Settings — zero-crossing snap, auto vertical zoom, fine-step mode, loop playback, and
  Audition — now persist between sessions (`~/.config/tui-wave/config.toml`).
- The Files panel is focused on startup, so the first thing you do is pick a file.
- Double-clicking the waveform between two markers (or before the first / after the last)
  selects that whole region.
- Clicking any panel — including the waveform — now focuses it.
- The Buffers panel loads the selected buffer immediately as you navigate, no Enter needed.
- Added Ctrl+A (Select All); Ctrl+R (Reverse) now works with no selection by reversing the
  whole file.
- Normalize now defaults to 0 dB instead of -1 dB.
- Arrow-key navigation accelerates the longer you hold a key — fixed so that fast manual
  tapping (not an actual held key) never falsely triggers acceleration.
- Renamed a few on-screen shortcut legends for clarity (backtick → `~`, Snap/Auto/Fine →
  zeroXSnap/AutoVZoom/fineNavi).
- Fixed the waveform going blank at high zoom (down to 1 sample/column): a single-sample
  column now draws a thin mark at its amplitude instead of vanishing.
- Added **Insertion Point Follows Playback** (`i`): pausing snaps the cursor to wherever
  playback stopped.
- Added **Viewport Follows Playback** (`f`): once the playhead reaches the right edge
  during playback, the view recenters and keeps scrolling so the playhead stays visible.
- Audition's shortcut moved from `p` to `a` in the Files panel (the same `a` still means
  Auto Vertical Zoom when the Waveform is focused — the app is modal).
- Marker insert/delete/rename/drag-move are now all undoable, like any other edit.
- A marker sitting exactly on the insertion point now renders in the cursor's accent
  color, so it no longer looks like the cursor has disappeared.
- Fixed the menu's dropdown rendering underneath the waveform/toolbar instead of on top of it.
- Added **Next Rising Edge** (`/`): jumps the cursor to right before the next transient
  (a sudden rise in volume) from the current position onward. The detection threshold
  defaults to 6dB, is adjustable with `+`/`-`, and persists between sessions.
- Added **Auto-Insert Markers at Transients** (`t`): scans the whole file and drops a
  marker before every detected transient, using the same threshold as Next Rising Edge,
  as a single undoable action.
- Added **Technical Fades** (`Ctrl+b`): a fixed 5ms exponential fade in at the start and
  fade out at the end of the whole file — the standard pre-export move to mask the click a
  hard cut to/from silence would otherwise leave at the file's boundaries.
- Fixed Next Rising Edge stopping well before the actual transient: a faint puff of
  pre-roll noise rising out of near-silence no longer gets mistaken for the real one.
- Added **Previous Rising Edge** (`?`): the same transient detection, searching backward.
- Next/Previous Rising Edge now center the viewport on the new cursor position instead of
  just nudging it into view, so there's context on both sides at any zoom level.
- Added Shift+[ / Shift+] (`{` / `}`): selects from the cursor to the previous/next marker
  (or the start/end of the file if there's none), advancing the cursor to the selection's
  new edge and scrolling it into view.
- Added a "Deselect" button to the toolbar's EDIT group.
- The toolbar's transient threshold now reads "Thresh 6dB" instead of a bare "6dB".
- Fixed "Save All & Quit" (and closing a single buffer with unsaved changes) silently
  discarding never-saved buffers instead of asking for a filename — it now prompts for a
  name for each one, in turn, before actually quitting/closing.
- Fixed a waveform display glitch right after a fade (most visible with Technical Fades'
  short 5ms ramp): the cache backing the waveform could report a column's level as already
  back at full volume one column early, bleeding in the next bin's content. The fade math
  itself was always correct — this was purely a display-precision bug in the cache, now exact.
- Added graphics-mode waveform rendering: on terminals that support the kitty (or
  compatible Sixel/iTerm2) graphics protocol, the waveform now draws as a real bitmap
  instead of character glyphs, with markers, the insertion point, and the playhead
  rasterized directly into the image. Falls back automatically (and unconditionally on
  tmux/screen) to the existing text renderer when no compatible protocol is detected.
  Toggle with `g`; persists between sessions via `graphics_mode` in the config file,
  defaulting to on whenever a capable terminal is detected.

## 2026-06-24

- Added a directory-aware Files panel (browse folders, not just load one fixed file) and a
  Buffers panel for working with multiple open documents at once.
- Added a modal command toolbar that shows different commands depending on whether the
  Waveform, Files, or Buffers panel is focused.
- Added search/filter within both the Files and Buffers panels.
- Added a modifier-free "fine-step" toggle for single-sample-precision navigation.
- Added a Process menu.
- Several toolbar layout and visual polish passes (grouped sections, column alignment,
  accent colors).
- Added a README with a screenshot.

## 2026-06-23

- Added Fade In/Out, Trim, and Resample (sample-rate conversion) commands.
- Added timeline markers (cue points), saved/loaded via BWF-compatible WAV chunks
  (interoperable with Audacity/Sound Forge).
- Added Gain with optional soft-clip (tanh) saturation, and a Normalize dialog.
- Fixed zero-crossing snapping for multi-channel audio.

## 2026-06-22

- Initial release: load and view a WAV file as a waveform, with keyboard navigation and
  zoom.
- Audio playback with sample-accurate position tracking.
- Selection, cut/copy/paste, and undo/redo.
- Menu bar and toolbar.
- Save/export WAV, with dirty-flag tracking and a quit confirmation.
- Catppuccin Mocha theme, sub-cell-precise waveform rendering, a dB scale, and a visible
  playhead marker.
- Loop playback and zero-crossing snapping for selections.
- Fixed a performance issue with large files and reworked zoom keybindings.
