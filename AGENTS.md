# AGENTS.md

Single-crate Rust project (edition 2024), no workspaces, no CI.

## Commands

- Build: `cargo build`
- Run: `cargo run -- <file.wav>` (no arg shows placeholder)
- Test all: `cargo test`
- Single test: `cargo test <name>` (e.g. `cargo test zoom_in_keeps_anchor`)
- No separate lint/format/typecheck — `cargo build` warnings are the gating check

## Key architecture constraint

`src/model/` must never depend on ratatui, cpal, or crossterm. This keeps editing/undo
logic testable without a terminal or audio device. `src/model/command.rs::Command` is a
trait object (`Box<dyn Command>`), not an enum — new operations are new files, not new
variants.

## Keybinding scheme (diverges from Audacity)

Left/Right = move cursor, Ctrl+arrows = fine (single-sample), Shift+arrows = extend
selection, Up/Down = zoom horizontal, Shift+Up/Down = zoom vertical. Source of truth:
`src/ui/keymap.rs::map_key`. Menu and toolbar shortcut text must match exactly.

## Performance note

Building WaveformCache (`src/ui/waveform_cache.rs`) is O(n) synchronous on the main
thread — slow in debug builds on large files. Use `cargo build --release` for real-world
use. Once cached, render cost is ~screen width regardless of file length.

## Manual testing

End-to-end TUI/audio behavior (resize mid-playback, terminal quirks, audio hardware) is
not unit-testable. Run `MANUAL_TESTING.md` checklist before any release.

## Recent additions (since IMMEDIATE-PLAN)

- Auto vertical zoom now fits to the **visible window** peak, updated every frame
- dB scale tracks the visible-window reference when auto zoom is active
- Mouse drag-to-select on waveform (click-drag-release)
- Reverse audio operation (`Ctrl+R`), Normalize (`Ctrl+N`) — both undoable
- Delete key (`Del`) with zero-crossing snap
- Zero-crossing snapping on Cut and Delete (256-sample search window)

## Known issues (from TODO.md)

- Save silently ignores write errors
- Cut-then-paste-over-selection needs two undos, not one
- No macOS/CoreAudio testing yet
- Toolbar buttons beyond 2-row chrome height are silently dropped

## Loop playback

`L` toggles loop mode (`App.loop_playback`). When on and nothing is selected, the
entire file loops. When on and a selection exists, the selection loops. The loop range
is baked into `DocumentSource` (loop_start/loop_end wrapping in `next()`) — for the
audio thread this means no `repeat_infinite()` wrapper, just a frame-index wrap inside
the existing source. The App passes the loop range to `play_looped`/`seek_looped` on
the engine; seek preserves loop bounds. Menu: Transport > Loop Playback. Toolbar:
`[Loop:L]`.

## Cursor / Playhead split

`Document.cursor` (yellow `│`) is the insertion-point / playback-start marker —
always visible. `App.playhead_position: Option<usize>` (red `│`) is the playback
position, read from `AudioEngine.position` each frame — only rendered during
playback. The playhead renders on top of the cursor so it visually overrides at
overlapping columns. `Action::Stop` does *not* reset the cursor to 0.
