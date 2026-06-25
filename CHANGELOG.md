# Changelog

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
