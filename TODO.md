# TODO

The Phase 0-6 MVP (waveform view, keyboard nav/zoom, playback, selection/cut/copy/paste/
undo, menu+toolbar, WAV load/save) is done and verified — see commit history. This file
tracks what's next.

## Feature backlog

- [ ] FLAC/MP3 (and other format) support — add `load_flac`/etc. to `model/io.rs`
      returning the same `Document`; evaluate `symphonia` (decode-only) for read and
      `mp3lame-encoder` or similar if MP3 *write* is ever wanted.
- [ ] CDP (Composer's Desktop Project) integration — new `Command` impls under
      `src/commands/` that write the selection to a temp WAV, shell out to a CDP program,
      read the result back, and splice it in like `Cut`/`Paste` do. No `History`/UI
      changes needed; see CLAUDE.md for the pattern.
- [ ] Define the rest of the "many more operations" list (gain/normalize, fade in/out,
      reverse, silence, etc.) and implement each as its own `Command`.
- [ ] Multi-document support (open more than one file at a time).
- [ ] "Save As" / a file picker — currently `Save` only overwrites the path the file was
      opened from; there's no interactive Open or Save As dialog (CLI arg is the only way
      to choose a file).
- [ ] Mouse drag-to-select on the waveform (currently click-to-seek only; selection
      extension is keyboard-only via Shift+arrows).

## Known rough edges

- `Action::Save` swallows write errors silently (`save_wav(...).is_ok()`) — no user-facing
  error if the save fails (e.g. disk full, permission denied).
- Cut-then-paste-over-a-selection applies as two separate history entries (delete + paste),
  so undoing a "replace selection by pasting" takes two undos, not one.
- Only tested on Linux/ALSA so far — macOS/CoreAudio playback behavior is unverified (see
  `MANUAL_TESTING.md`).
- No CI configured — `cargo test`/`cargo build` are run manually.
- Building the waveform min/max cache (`ui/waveform_cache.rs`) is O(n) and runs
  synchronously on the main thread at load and after every mutating edit — for a very
  large file in a debug build this is a multi-second blocking pause (fine in release).
  Not yet streamed/backgrounded; see CLAUDE.md's note on the cache.
- Toolbar button packing wraps to a fixed 2-row chrome height
  (`ui/layout.rs::TOOLBAR_HEIGHT`) — on a very narrow terminal, buttons that don't fit in
  those 2 rows are silently dropped from the toolbar (still reachable via keyboard/menu).
- The dB scale gutter (`ui/widgets/db_scale.rs::DB_GUTTER_WIDTH`, 4 columns) is a pragmatic
  width for labels like "-36" — on a very narrow terminal this eats a larger fraction of
  total width than ideal; could shrink further or hide the gutter below some width.
