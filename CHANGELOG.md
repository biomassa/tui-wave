# Changelog

## 2026-07-02

- The Gain dialog now offers **per-channel gain** on stereo buffers: a "Per-channel gain"
  checkbox (only shown when the active document has exactly 2 channels) splits the single
  Gain field into separate Left/Right dB fields when checked, so each channel can be
  boosted or attenuated independently. Unchecked (the default), Gain behaves exactly as
  before — one value applied uniformly to every channel. Vertical order is Gain/Left, then
  Right (blank until checked), then the checkbox, then Tanh limiter; the popup is a fixed
  size whether or not the box is checked, so toggling it never resizes or reflows the
  dialog.

- Graphics-mode waveform is now anti-aliased. Span edges stay in continuous sub-pixel
  coordinates and the fractional first/last pixel of each column blends against the
  background, so sub-pixel amplitude changes render as a smooth curve instead of flat runs
  with hard one-pixel jumps (the staircase visible at some zoom levels). Applies to both
  the mid-zoom min/max bars and the high-zoom polyline.

## 2026-07-01

- Fixed the graphics-mode waveform breaking into dashes at mid zoom levels. Each pixel
  column's min/max bar covered only its own samples, so on steep slopes the inter-sample
  step across a column boundary fell between adjacent bars and the trace visibly
  disconnected. Bars now extend to overlap the previous column's bar by at least one pixel
  row — the bar-mode counterpart of the connection the high-zoom polyline mode already had.

## 2026-06-30

- Files panel: **Rename** (`Ctrl+r`) renames the selected `.wav` on disk via a name dialog (Esc
  cancels; a buffer open on that file follows the rename), and **Delete** (`Del`) removes it
  from disk after a confirmation (deleting is irreversible). Both also appear in the Files
  toolbar.

- The waveform header now shows the active buffer's real name (e.g. `_NEW_006` for a
  never-saved buffer, matching the Buffers panel) instead of "untitled", and drops the
  meaningless "tui-wave —" prefix. The no-file placeholder reads "No file loaded".
- "Reset Config to Defaults" now asks for confirmation before wiping keybindings (and still
  backs the old config up to `.bak`).
- Confirmation dialogs now show "(Esc) cancel" instead of "(n) cancel" — Esc is the natural
  cancel key (any non-confirming key still cancels).
- Shift+Tab now cycles backward, the reverse of Tab — both for panel focus (Waveform →
  Buffers → Files → Waveform) and for fields within a dialog (Save As, Gain, Mix to Mono,
  Export Regions, Fade). Works under the kitty keyboard protocol (Tab+Shift) and on terminals
  that send a legacy BackTab.
- Fixed menu dropdown entries that overlap the Files/Buffers panels being unclickable — the
  panel underneath was stealing the click. An open menu now takes mouse precedence over the
  panels beneath it, matching how it already intercepts the keyboard.
- "Reset Config to Defaults" now backs up the existing `config.toml` to `config.toml.bak`
  before overwriting it, so a reset can be undone.
- Playback that reaches the end of a (non-looping) track now actually stops: previously the
  "playing" state stuck, so the next Space press paused a finished track instead of replaying
  it. Space now replays from the cursor in one press.

- Fixed zoom (Up/Down) restarting playback from the cursor position instead of continuing
  from the current playhead. Navigation actions seek the audio position only when the
  cursor actually moves; zoom-only actions leave the playhead untouched.
- Quick Save (Ctrl+S) now preserves the source file's original bit depth (16-bit int saves
  as 16-bit, 24-bit as 24-bit, float as float) instead of always promoting to 32-bit float.
  Save As now defaults to the document's original bit depth rather than float.
- Renamed "unsaved" buffers (no path yet) from `_UNSAVED_001` to `_NEW_001` in the Buffers
  panel for clearer intent.
- **UI restructure**: removed the Channels menu/toolbar section. Mix to Mono moved to
  Process. New from Left / New from Right moved to File. Both menus and the toolbar
  reflect the change.
- **Export Regions to Subfolder** (Shift+E): chops the active buffer at its markers and
  saves each region as a numbered WAV file into a new subfolder. Opens a dialog to set the
  subfolder name, base filename, bit depth, optional dither, and optional fade in/out (with
  an editable millisecond length, default 5 ms) applied to each region. If no markers are present,
  shows an info popup. The first region is `[file start → first marker]`, the last is
  `[last marker → file end]`; files are named `{base}-001.wav`, `-002.wav`, etc.

## 2026-06-29

- Fixed Fade In / Fade Out silently doing nothing on small selections. When zero-crossing
  snap contracted both endpoints of a short selection to the same crossing (making the range
  degenerate), the fade was skipped with no feedback. The fix falls back to the un-snapped
  range in that case so the fade always applies over at least the selected samples.

## 2026-06-28

- All keyboard shortcuts are now configurable via `~/.config/tui-wave/config.toml` under a
  `[keybindings]` section. Every action lists its default key string(s) there on first save.
  Key string format: `"ctrl+x"`, `"shift+left"`, `"L"`, `"space"`, `"delete"`, etc.
  Menu and toolbar display strings now reflect the configured binding — remapping a key
  updates every shortcut hint in the menu and toolbar accordingly.
- The config file (`~/.config/tui-wave/config.toml`) is now written on the very first
  launch so all available keybindings are immediately visible without having to trigger a
  toggle first. On subsequent launches after an upgrade, any newly-added default bindings
  are appended automatically without touching existing custom entries.
- Shift+letter shortcuts now show as `S+C`, `S+L`, `S+M` etc. in the toolbar (and
  `Shift+C`, `Shift+L`, `Shift+M` in the menus) instead of the bare uppercase letter,
  making it clear that Shift is required.
- **File › Reset Config to Defaults**: resets the `[keybindings]` section of the config
  file to factory defaults while preserving all other settings (snap, zoom, loop, etc.).
  Takes effect immediately — the key map and all shortcut hints update without restarting.
- All option-bearing dialogs now follow a consistent multi-row UX: checkboxes appear as
  `[X] Label` rows, cycle selectors show `◄ Label ►`, and a hints bar at the bottom of
  each popup lists the relevant keys (`Tab:next  Space:check  ←→:change  Enter:apply`).
  Dialogs are now mouse-aware: clicking a row focuses it; clicking a checkbox row also
  toggles it; all other mouse events are absorbed while a dialog is open.
  - **Gain**: text field and `[X] Tanh limiter` checkbox as separate rows; Tab/Space/Enter.
  - **Fade In / Fade Out**: `◄ Curve ►` cycle row; ←/→ to step through Exp/Log/Linear.
  - **Save As**: filename field, format cycle row (`◄ Format ►`), and `[X] Dither` checkbox
    as three distinct rows; Tab to move focus, ←/→ to change format, Space to toggle dither.
  - **Mix to Mono**: added `Space:check` hint to the existing hints bar.
- Mix to Mono dialog: Tab now only cycles between channel input fields (it no longer toggles
  the tanh checkbox as a side effect). Press Space to toggle the tanh limiter when that row
  is focused.
- Markers are now preserved when creating new buffers via Copy to New (`C`), New from Left
  (`L`), New from Right (`R`), and Mix to Mono. If a selection is active, only markers
  within that range are carried over, with their positions shifted to be relative to the
  new buffer's start.
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
