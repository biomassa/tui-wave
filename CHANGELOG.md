# Changelog

Notable user-facing changes to tui-wave, newest first. Internal refactors, test-only
changes, and documentation-only commits are omitted.

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
