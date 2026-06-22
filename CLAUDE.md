# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

A keyboard-driven TUI (terminal) audio editor for Linux/macOS, written in Rust: zoomable
waveform navigation, playback, selection/cut/copy/paste/undo, a menu bar and toolbar, and
WAV load/save. The full implementation plan (stack rationale, architecture, phased
build-out) lives in the commit history — see the Phase 0-6 commits on `master` for the
reasoning behind each module.

## Commands

- Build: `cargo build`
- Run: `cargo run -- <file.wav>` (no file loaded shows a placeholder screen)
- Test all: `cargo test`
- Test a single test: `cargo test <test_name>` (e.g. `cargo test zoom_in_keeps_anchor`)
- Test a single module: `cargo test model::history`

There is no separate lint command configured beyond `cargo build`'s warnings; treat new
`dead_code` warnings as a signal that something is unused, not noise to suppress.

### Manual/non-automated verification

TUI and audio behavior (resize during playback, real terminal emulator quirks, actual
audio hardware) can't be meaningfully unit-tested. `MANUAL_TESTING.md` has the checklist —
re-run it before tagging a release. For automated end-to-end checks, drive the compiled
binary through a real pty with a virtual terminal (the `pyte` Python package) rather than
`cargo run` directly — recompilation prints warnings straight into the raw-mode terminal
and corrupts captured output. Crossterm mouse events use the X10 protocol by default
(`ESC [ M <Cb+32> <Cx+33> <Cy+33>`) — forgetting the `+32` offset on the button byte sends
a literal NUL and silently drops the event.

## Architecture

Three layers with a strict one-way dependency, which is the key constraint to preserve
when adding features:

```
src/model/    Document, Selection, Command trait, History, Clipboard, WAV I/O
              -- zero dependency on ratatui/cpal/crossterm; pure logic, fully unit-testable
src/commands/ Concrete Command impls (Cut, Paste, Delete)
src/audio/    AudioEngine (rodio/cpal), playback thread, sample-position tracking
src/ui/       ratatui widgets, App event loop, keymap, menu, toolbar, viewport
```

`src/model` must never depend on ratatui, cpal, or crossterm — that's what keeps the
editing/undo logic testable without a terminal or audio device, and what will let CDP
(Composer's Desktop Project) integration land later as new `Command` impls without
touching the UI.

### Document model (`model/document.rs`)

Audio is stored as deinterleaved `Vec<Vec<f32>>` (one Vec per channel), normalized to
[-1.0, 1.0] regardless of source bit depth. `Document` also holds `selection`, `playhead`,
and `dirty`. `slice`/`remove_range`/`insert_range` are the only mutation primitives;
everything else (cut/paste/delete) is built from them.

### Command pattern (`model/command.rs`, `model/history.rs`)

`Command` is a trait object (`Box<dyn Command>`), not an enum, specifically so new
operations — including future CDP-process wrappers — are new files with zero edits to
`History` or the UI. Each command stores whatever it needs to undo itself (e.g.
`RemoveRangeCommand` stores the removed samples). `History::apply` executes and pushes to
the undo stack, clearing redo; `Cut` and `Delete` share `RemoveRangeCommand` under
different labels — cut additionally stashes the removed slice into `Clipboard` (which
lives outside `Document` so paste can eventually target a different open document) before
applying it.

### Audio engine (`audio/engine.rs`, `audio/source.rs`)

A dedicated thread owns the rodio `Player`/mixer for the lifetime of the engine. The UI
thread only ever sends fire-and-forget commands over a `crossbeam_channel`
(`Play`/`Pause`/`Stop`/`Seek`/`Reload`) and polls a lock-free `Arc<AtomicUsize>` for
playback position once per redraw tick — neither side blocks the other.
`DocumentSource::next()` increments that atomic as it yields samples, which is what gives
sample-accurate playhead sync without a channel round-trip per frame. After a document
edit, the UI calls `engine.reload(document.channels.clone())` so future `play`/`seek`
calls pick up the new data (an already-playing source keeps whatever it captured when it
started).

`AudioEngine::try_new` probes device availability and returns `None` on failure —
playback is optional, not required to view/edit a waveform. The probe silences
`log_on_drop` on rodio's sink; otherwise dropping the throwaway probe prints a warning
straight to stderr, corrupting the raw-mode terminal.

### UI event loop and dispatch (`ui/app.rs`)

`App::run` is `terminal.draw()` → poll for one event (16ms budget, ~60fps) → handle it →
`sync_playhead_from_audio()` → repeat. Key handling has a strict precedence: quit
confirmation modal intercepts everything first, then an open menu intercepts navigation
keys, then Alt+mnemonic/F10 opens a menu, then everything else falls through to
`keymap::map_key`.

`ui::keymap::Action` is the single dispatch type — every menu entry (`ui/menu.rs`), every
toolbar button (`ui/toolbar.rs`), and every keyboard shortcut resolves to the same `Action`
and funnels through `App::handle_action`, so there's one place where "what does Cut do"
is defined, not three that can drift apart. `MenuBar` and `Toolbar` are custom widgets
(ratatui has no native menu) that record their rendered `Rect`s each frame for mouse
hit-testing.

### Waveform rendering and viewport (`ui/viewport.rs`, `ui/widgets/waveform.rs`)

`Viewport` holds `samples_per_column`/`scroll_offset`/`amplitude_scale` and is pure state —
no rendering dependency, fully unit-testable. The waveform widget renders via min/max
downsampling: each terminal column gets one min/max pair over its sample span, never
iterating every sample regardless of zoom level. `Viewport::zoom` anchors a given sample
to its current terminal column across a zoom change (zoom-to-cursor) rather than
re-centering, which is what keeps zooming from feeling disorienting.

## Deferred (architecture supports, not built)

FLAC/MP3 via `symphonia`/`claxon` (new `load_*` functions returning the same `Document`);
CDP (Composer's Desktop Project) external-process command category (new `Command` impls —
write selection to temp WAV, shell out, read result, splice in); multi-document support.
