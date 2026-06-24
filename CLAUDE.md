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
src/model/    Document (samples + markers + bext), Selection, Command trait, History,
              Clipboard, WAV I/O (io.rs), BWF cue/bext chunks (bwf.rs)
              -- zero dependency on ratatui/cpal/crossterm; pure logic, fully unit-testable
src/commands/ Concrete Command impls: Cut/Delete (RemoveRangeCommand), Paste, Reverse,
              Normalize, Gain, Fade, Trim, Resample
src/audio/    AudioEngine (rodio/cpal), playback thread, sample-position tracking
src/ui/       ratatui widgets, App event loop, keymap, menu, toolbar, viewport,
              file_panel (dir browser), buffer_panel (open documents)
```

`src/model` must never depend on ratatui, cpal, or crossterm — that's what keeps the
editing/undo logic testable without a terminal or audio device, and what will let CDP
(Composer's Desktop Project) integration land later as new `Command` impls without
touching the UI.

### Document model (`model/document.rs`)

Audio is stored as deinterleaved `Vec<Vec<f32>>` (one Vec per channel), normalized to
[-1.0, 1.0] regardless of source bit depth. `Document` also holds `selection`, `cursor`,
`dirty`, `markers` (cue points), and `bext` (preserved BWF metadata bytes).
`slice`/`remove_range`/`insert_range` are the only mutation primitives; everything else
(cut/paste/delete) is built from them. `remove_range`/`insert_range` also shift `markers`
so they stay anchored to the same audio across edits.

### Command pattern (`model/command.rs`, `model/history.rs`)

`Command` is a trait object (`Box<dyn Command>`), not an enum, specifically so new
operations — including future CDP-process wrappers — are new files with zero edits to
`History` or the UI. Each command stores whatever it needs to undo itself (e.g.
`RemoveRangeCommand` stores the removed samples). `History::apply` executes and pushes to
the undo stack, clearing redo; `Cut` and `Delete` share `RemoveRangeCommand` under
different labels — cut additionally stashes the removed slice into `Clipboard` (which
lives outside `Document` so paste can target a different open document) before applying it.

History is **per-document**: `App` holds a `Vec<History>` kept index-parallel to
`documents`, because each command stores sample data from the document it was applied to —
replaying it against a different buffer would corrupt it. Marker/Save-As/playback actions
are not undoable; length-changing commands that touch markers (Cut/Delete/Trim/Resample)
snapshot them for exact undo.

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

**Focus + modal command panel.** `App::focus()` returns `Focus { Waveform, Files, Buffers }`
(derived from the panel `.focused` flags) — the single source of truth for: which command
set the toolbar shows, the accent color on the active panel (peach `theme::FOCUS`, incl. the
waveform title/border), and which contextual keys apply. The toolbar is **modal**: it holds
three command sets and renders the one for the current focus. Most actions in `handle_action`
work regardless of focus (so a toolbar click always works); the *contextual keys* are
resolved in the panel branches of `handle_key` before the global keymap — e.g. `^o` opens a
directory only when Files is focused (it's Fade Out in waveform focus), and `^s`/`^w`/`^r`/`^a`
are Save/Close/Rename/SaveAll only when Buffers is focused (so `^r`/`^a` don't clash with the
global Reverse/SaveAll). The Files panel is directory-aware (`file_panel::EntryKind`
Parent/Dir/File): Enter navigates into a dir or `..`, or opens a `.wav`. Confirmations
(quit, close-dirty-buffer) share one `Confirm` modal.

### Waveform rendering, viewport, and the min/max cache

`Viewport` holds `samples_per_column`/`scroll_offset`/`amplitude_scale` and is pure state —
no rendering dependency, fully unit-testable. `Viewport::zoom` anchors a given sample to
its current terminal column across a zoom change (zoom-to-cursor) rather than re-centering,
which is what keeps zooming from feeling disorienting.

The waveform widget always draws the playhead as a bold vertical line
(`waveform::playhead_column`, themed `theme::PLAYHEAD`), on top of the waveform, at
whatever column `document.playhead` maps to — every code path that moves `playhead` (nav,
playback sync, mouse seek) calls `viewport.ensure_visible` first (mouse seek is exempt
since its target is computed from an already-visible column), which is what keeps the
marker on-screen rather than scrolled out of view. `App::sync_playhead_from_audio` clamps
the audio thread's position to `total_len - 1` — the position counter can land exactly on
`total_len` once a track finishes playing, which would otherwise push the marker one
column past the right edge.

A bar's top/bottom edges land at fractional (sub-row) amplitude positions almost
everywhere; floor/ceil-ing them to whole rows (the original implementation) throws away
most of that precision, which matters most for quiet signals or a zoomed-in bar that's
only 1-2 rows tall. The widget instead draws the boundary row at each edge with a
lower-eighth-block glyph (`▁▂▃▄▅▆▇█`, `waveform::lower_eighth`) sized to its fractional
coverage: directly for the top edge (a lower-N/8 glyph already fills "from the bottom up,"
the right orientation there), and via an fg/bg swap on the complementary glyph for the
bottom edge (filling "from the top down" using only bottom-aligned glyphs — the upper-N/8
counterparts are real Unicode but live in the Legacy Computing Supplement block with much
patchier font support, so the swap trick gets the same effect with universal compatibility
instead). This is deliberately *not* Nerd Font glyphs, which are icon-style symbols (file
types, git status, etc.) rather than graduated fill levels — eighth-blocks are standard
Unicode and what terminal sparkline/plot tools already rely on.

### Theme (`ui/theme.rs`)

Catppuccin Mocha, restricted to the handful of colors actually in use and given semantic
names (`theme::WAVEFORM`, `theme::SHORTCUT`, etc.) — change a role's color in one place
rather than hunting down hex values scattered across widgets. Keyboard shortcuts in the
menu and toolbar are always rendered in `theme::SHORTCUT` (peach) against the surrounding
label's `theme::CHROME_FG` (text) specifically so a shortcut hint never blends into the
label it's attached to. The one exception is a *selected* menu entry, which uses one
uniform highlight color for the whole row rather than layering a third accent on top of
it — two light pastel accents together risk a low-contrast clash.

The waveform widget renders via min/max downsampling (one min/max pair per terminal
column), but it does **not** scan raw samples to get there — it consults a `WaveformCache`
(`ui/waveform_cache.rs`), a precomputed multi-resolution min/max pyramid (base bins of 64
samples, each higher level reducing the previous by 16x) built once per channel whenever
the document's sample data changes (load, cut, paste, undo, redo — see
`App::rebuild_waveform_caches`). Scanning raw samples for the visible range on every frame
made the editor unusably slow on large files at zoomed-out views (every redraw rescanned
the whole visible range — for a multi-minute file, tens of millions of comparisons per
frame); the cache bounds render cost to roughly the screen width regardless of file length
or zoom level.

Building the cache itself is an O(n) one-time cost — fast in a release build, noticeably
slower in debug for very large files (multiple seconds for a 10-minute stereo file). Build
with `cargo build --release` when working with large files; the per-keystroke render cost
is unaffected by build profile once the cache exists.

`Viewport.total_len` is kept in sync with the document's sample count every frame
(`App::render`) and `ensure_visible` clamps `scroll_offset` so the visible window never
overhangs past end-of-file — without this, certain scroll/zoom combinations left a blank
gap between the right edge of the waveform and the window's right border. When the whole
file fits within one window's span, `scroll_offset` is forced to 0 (there's no valid
nonzero position that doesn't overhang).

### Vertical zoom and the dB scale (`ui/widgets/db_scale.rs`)

`Viewport.auto_vertical_zoom` is off by default — vertical zoom starts at 1.0 (literal
amplitude) and only changes via explicit Shift+Up/Down, or by toggling auto vertical zoom
(`Action::ToggleAutoVerticalZoom`, key `a`), which fits `amplitude_scale` to the document's
peak (`App::waveform_peak`, derived from the `WaveformCache`s) and re-fits after every
edit while it stays enabled.

Each channel's pane reserves a `DB_GUTTER_WIDTH`-column gutter on both the left and right
edge (`App::render`) showing a mirrored dB axis (0dB, -3, -6, -12, -18, -24) computed
through the same amplitude→row mapping the waveform itself uses, so the marks always line
up with where those levels actually render. `DbScaleWidget.reference_amplitude` is the
pivot: 1.0 when auto vertical zoom is off (absolute dBFS — 0dB always means literal full
scale), or the document's peak when it's on (dB relative to peak — 0dB always tracks
wherever the loudest sample in the file actually is, which is the "dynamic" behavior).
`DB_MARKS` is ordered most- to least-important; when the pane is too short to give every
mark a distinct row, `draw_label`'s `claimed_rows` tracking makes the first mark to land on
a row win rather than a later, less important mark silently overwriting it.

### Keybindings

`ui/keymap.rs` is the single source of truth, and `ui/menu.rs`/`ui/toolbar.rs` must show
shortcut text that matches it exactly (toolbar buttons render as `[Label:Shortcut]`).
Convention, deliberately diverging from Audacity's Ctrl+1/Ctrl+3 zoom shortcuts in favor of
an arrow-key-only scheme suited to a terminal with no reliable mouse/menu access: Left/Right
move the cursor, Shift+Left/Right extend the selection, Up/Down zoom horizontally,
Shift+Up/Down zoom vertically. **Fine stepping is a modifier-free toggle:** backtick (`` ` ``)
flips `App.fine_mode` (mirrored in the View menu, the OPTS toolbar group, and a "Fine: on"
status indicator); while it's on, the same Left/Right and Shift+Left/Right step ~1/8th of a
column instead of a whole one (`step = (column_step / 8).max(1)`) — finer than coarse but
still faster than one sample per press, bottoming out at a single sample only when zoomed in
far enough that an eighth-column rounds down to one.
This deliberately avoids Ctrl/Alt+arrow: **every** double-modifier+arrow combo is swallowed
before the app sees it — kitty binds Ctrl+Shift+Left/Right to prev/next-tab, Alt+Shift is a
common desktop layout-switch shortcut, and Ctrl+Alt+arrow is often the DE's workspace switch.
A plain key sidesteps all of them. (`ui/terminal.rs` still pushes the kitty keyboard-protocol
`DISAMBIGUATE_ESCAPE_CODES` flag when supported, which is unrelated but keeps modified keys
from being mis-decoded as text.)

## Markers (`model/document.rs`, `model/bwf.rs`, marker UI in `ui/app.rs`)

Timeline markers are `Marker { position, label }` on `Document`, persisted as WAV `cue `
points + `adtl`/`labl` labels (interoperable with Audacity/Sound Forge). `hound` can't
read/write those chunks, so `bwf.rs` walks the RIFF chunk list to read them and *appends*
the extra chunks after `hound`'s `fmt `/`data` on save (patching the top-level RIFF size).
A file's `bext` (broadcast metadata) chunk is read and written back verbatim. Keys: `m`
insert (auto-named "Marker N"), `M` delete nearest, `[`/`]` jump; mouse drag moves a marker
line, double-click its label renames it.

## Save formats (`model/io.rs`)

`save_wav` writes 32-bit float (the lossless working format, used by quick Save). Save As
goes through `save_wav_with`, letting the user pick 16/24-bit int (re-quantized, optional
TPDF dither — `DitherRng`) or 32-bit float; Tab cycles depth, Ctrl+D toggles dither.
Resample (`commands/resample.rs`) is a whole-file windowed-sinc conversion; changing the
rate rebuilds the audio engine (it captures the rate at construction — see
`App::after_sample_mutation`, which rebuilds rather than reloads when the rate changed).

## Deferred (architecture supports, not built)

FLAC/MP3 via `symphonia`/`claxon` (new `load_*` functions returning the same `Document`);
CDP (Composer's Desktop Project) external-process command category (new `Command` impls —
write selection to temp WAV, shell out, read result, splice in). Multi-document support
and Save As are now built (`App.documents`/`histories`, `file_panel`, `buffer_panel`).
