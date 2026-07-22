# Changelog

## 2026-07-22

- **CDP process titles renamed to a CDP-WASM-SUITE-style, plain-English convention**
  (`catalog_titles.toml`, a title-only override layer so the generated `catalog.toml` never
  needs hand-editing) — every renamed title still reveals its own CDP binary. Fixed 4 real
  catalog-key collisions found along the way (two different processes silently sharing one
  key, merge-by-key semantics shadowing one of each pair). Added 27 new catalog entries for
  confirmed CDP-WASM-SUITE gaps (filter fixed/variable, 8 more distort modes, clip mode 1,
  grain align, specfold fold/invert, glisten, chirikov, packet), verified against the real
  CDP8 binaries. Corrected stale source comments citing "CDP 7.1" — the bundled binaries'
  own usage banner is misleading; this is actually CDP Release 8
  (github.com/ComposersDesktop/CDP8).
- **New "CDP Chain..." (Ctrl+H): multi-step CDP pipelines with unlimited-depth
  side-chains.** A linear list of CDP processes runs as one pipeline; any dual-input step
  can be fed by its own side-chain (a sub-chain run against a separately-picked buffer)
  instead of a raw open buffer, nested to any depth. Reuses the existing Browser/Params flow
  to add/edit each step; a stack-based execution engine walks the chain as a post-order tree
  and splices the final result as a single undo step. Real audio preview works both for the
  whole chain and mid-edit on one in-progress step (upstream steps run for real, plus the
  step's current values). Chains save/load as named presets, track recent use, and
  auto-save the last successfully-run chain to its own recall slot (`l` in the chain editor)
  so an unsaved but carefully built chain is never lost after a bare Run. `p` previews the
  chain up to and including just the selected step.
- Fixed: a multi-step chain's marker-preservation tolerance was derived from only its last
  step's category, as if the whole chain were one CDP process — but each spectral step
  re-analyzes the previous step's already-padded output from scratch, so the drift compounds
  down the chain. A chain of just 3 ordinary spectral steps (nothing "time-altering") could
  drift far enough to silently collapse every cue marker in range. Tolerance is now the sum
  of every top-level step's own tolerance, matching the real compounding drift.
- Added `Ctrl+L` ("Reload from disk") to the Buffers panel: re-reads the active document's
  file wholesale (samples, cue markers, bext, bit depth) and clears its undo history — the
  old stack's commands store sample data from before the reload, so replaying them would
  corrupt it. Confirms first on a dirty buffer, reloads immediately if already clean,
  no-ops on a never-saved buffer. New matching toolbar button.
- Bumped version to 1.4.0, covering the CDP-WASM-SUITE renames/gaps, the CDP Chain feature
  (plus its recall-last-chain and per-step-preview follow-ups), the chain marker-tolerance
  fix, and the Buffer-panel Reload-from-disk shortcut above.

## 2026-07-21

- **Dot-matrix waveform renderer replaces the eighth-block bars entirely**, in both the
  character-glyph and graphics-mode (kitty/Sixel/iTerm2) renderers. Each terminal column
  splits into a left/right sub-column with its own min/max (2x horizontal resolution) and
  each row into 4 braille dot-rows (4x vertical resolution), giving the waveform a textured,
  btop-style look instead of solid bars. Colored by a green → yellow (-6dB) → red (0dB)
  amplitude gradient (`theme::gradient_color`, graded by dB via a new `dsp::linear_to_db`,
  not raw linear position — most of a waveform's on-screen height is quiet in linear terms,
  so a linear-position gradient was nearly invisible). The gradient is a toggle ("Gradient"
  in the View menu); off, the waveform draws flat green instead. A selection now shows as a
  dimmed green background with flat black dots, no gradient.
- **View menu toggles now show a checkmark when active** (Zero-Crossing Snap, Fine Step
  Mode, Auto Vertical Zoom, Insertion Point/Viewport Follows Playback, Graphics Mode,
  Gradient), consistent with the toolbar's own active-state highlighting. Checkmarks and
  shortcuts render in fixed-width columns (measured in characters, not UTF-8 bytes — `✓` is
  one display column but three bytes) so they line up vertically instead of drifting with
  each label's length.
- Fixed: pressing Space to play a selection continued playing past the selection's end once
  loop playback was toggled off, instead of stopping there. `AudioEngine` gained
  `play_bounded`/`seek_bounded` (play/seek once, no wraparound, but still stop at an end
  frame — `DocumentSource` already supported this via `loop_end` with no `loop_start`, it
  just wasn't exposed). `App::playback_bound` now distinguishes looped (loop playback on),
  bounded (a selection with loop playback off), and unbounded playback, and falls back to
  the selection's start when the cursor sits at its far edge (the common case after a
  left-to-right drag) so the whole selection plays instead of nothing.
- Bumped version to 1.3.0, covering the dot-matrix waveform renderer (text and graphics
  mode), the amplitude gradient and its toggle, View menu checkmarks, and the
  selection-playback bound fix above.
- **CDP UI cleanup pass** (user report):
  - The CDP Process browser's Groups column no longer lists "pitch curve" — every process
    tagged with that subcategory is curve-in/curve-out and was already unconditionally
    excluded from this browser (`is_curve_only_process`), so the group could only ever show
    "No matches". `App::cdp_groups` now filters subcategories through the same eligibility
    check `cdp_filter_entries` applies, so a listed group is always populate-able.
    (`psow`'s pitch-subcategory processes, e.g. "Psow Reinforce Harmonics", were never
    actually excluded from the real "pitch" group — they just sit near the end of its long,
    catalog-order list.)
  - Buffers-panel row tags shortened: `[Curve]` → `[p]`, `[Formant]` → `[f]`, `[Snapshot]` →
    `[s]` (`FormantBufferKind::tag`, `App::buffer_names`). The old full-word tags routinely
    ran buffer names out of the panel's width.
  - CDP Process browser capability badges shortened to the same convention: "pitch curve" →
    `[p]`, "formants" → `[f]`, "snapshot" → `[s]` (`cdp_process_badges`) — the old full-word
    badges plus a long process title regularly overflowed the process list's column.
  - The Processes column widened (46 → 62 cols) at the Description column's expense
    (`CDP_BROWSER_PROCESSES_WIDTH`), and the Description column is now mouse-wheel
    scrollable when its text overflows the popup (`cdp_browser_desc_max_scroll`); the
    Processes column is also mouse-wheel scrollable (moves `selected`, same as Up/Down).
    Both are hit-tested against `cdp_browser_layout`, a geometry helper factored out of the
    renderer so a scroll can never land on the wrong column.
  - **Fixed (NASTY BUG):** opening the envelope editor's "use curve" picker (`c`) while
    graphics mode was on left the picker completely obscured by the envelope's own bitmap
    curve, redrawn on top of it every frame. The graphics-mode redraw block matched on
    `dialog.envelope` being `Some` without checking whether the curve picker sub-overlay was
    open, and reused `dialog_row_rects.first()` as its target `Rect` — but the picker's own
    renderer returns an empty row-rect list, so `dialog_row_rects` never got updated for the
    picker's frame and still held the *envelope grid's* stale `Rect` from the frame before
    `c` was pressed. Now gated on `edit.curve_picker.is_none()`.
- Bumped version to 1.3.1, covering the CDP UI cleanup pass above.

## 2026-07-20

- **USERGUIDE and README rewritten for accuracy and brevity.** `USERGUIDE.md` shrinks from
  345 to 234 lines, consolidating redundant sections (e.g. the `i`/`f` toggles previously
  listed under both Playback and Toggles) and tightening prose throughout. Adds the complete
  Tier 2/3 CDP workflows (pitch curves, formants, freeze-at-cursor) that were missing
  entirely, and documents the new capability badges (`>1 inputs`, `pitch curve`,
  `formants`, `snapshot`) and buffer row types (`[Curve]`/`[Formant]`/`[Snapshot]`). Both
  docs also fix a stale menu path — "Options → Configure CDP Directory" is now "CDP →
  Configure CDP Directory" (the Options menu no longer exists).
- **CDP dialog UX consistency audit.** `b`/`e` smart-activation keys now work from anywhere
  in a CDP params form instead of only when the target field already has focus (priority:
  the focused field if eligible, else the first not-yet-configured eligible field, else the
  first eligible field). `Enter` on an unset required envelope/list field, or an unpicked
  formant-buffer field, now opens its editor/picker instead of running Apply and
  immediately failing with a generic "value out of range" error, matching the standalone
  curve-transform dialog. The process browser gained capability badges for "pitch curve"
  and "formants"/"snapshot" alongside the existing ">1 inputs", and curve-only transforms
  (Repitch Exaggerate/Smooth/...) are now hidden from the main browser since they can only
  ever run against an open pitch curve. Also fixed a real bug found while testing the
  above: `Space` was unconditionally intercepted for every dialog, so no free-text field
  anywhere — CDP browser search, every Rename dialog, Open Directory, Save Curve As, Load
  Pitch Curve, CDP Setup — could contain a space; it now falls through to normal text
  insertion except in the four dialogs that use it as a checkbox toggle.
- The CDP directory now defaults to `~/cdp` (resolved against the real `$HOME` at startup)
  before prompting, instead of always starting from an empty setting.
- Bumped version to 1.2.0, covering Tier 2 (pitch curve extraction, editing, CDP
  transforms) and Tier 3 (formant/snapshot buffers, freeze-at-cursor) of the CDP
  integration, the dialog UX consistency audit above, and the new `~/cdp` default.

## 2026-07-19

- Added **"Freeze Formant Snapshot at Cursor"**, a new CDP menu action that freezes a
  `[Snapshot]` buffer at the waveform cursor with no manual steps: it reuses an existing
  `[Formant]` extraction on the current document, or runs Extract Formants automatically
  first and chains the freeze onto its result. Replaces the old per-buffer freeze flow (the
  `f` key and typed-time prompt in the Formant Info popup), which is now purely read-only.
- Reworked the curve-transform params dialog (Repitch Quantise etc.) to match the main CDP
  params dialog's UX, after a user report that it was a bespoke reimplementation that had
  drifted: `Enter` on a required-list field now opens its editor (previously a no-op off
  the Apply row), `Shift+Tab` navigates backward (previously dead — terminals emit
  `BackTab`, which the handler didn't catch), and mouse clicks on form rows now
  focus/open/toggle fields or run Apply.
- Fixed the envelope `c` curve-picker giving no feedback when no curves were open — it now
  always opens the picker, showing "(no open curves)" instead of silently doing nothing.

## 2026-07-18

- **Tier 3 of the CDP integration: formant and snapshot buffers.** CDP → Extract Formants
  captures a selection's spectral envelope as a `[Formant]` buffer (best on voice or an
  instrument with real timbre); Formants Put and Oneform Put impose a `[Formant]` or
  `[Snapshot]` buffer onto other audio (Replace/Layer and Impose/Replace variants), and two
  new pitch/frequency-band Formant Vocode processes round out the catalog additions.
- Fixed the code-review findings in `FABLE-REVIEW.md` (FR-1 through FR-9), each with a
  regression test: a stale Preview cache that didn't invalidate when the picked formant
  buffer changed (could splice stale audio); dirty curves not counting toward the quit
  confirmation and not being covered by Save All; an in-progress hand edit in the curve
  editor being silently discarded on a failed transform; curve-template undo; a `tick_cdp`
  job-id check that ran after (instead of before) consuming pending CDP results; `Ctrl+W`
  not closing a focused curve/formant row in the Buffers panel; no inline validation on the
  freeze-time prompt; a no-op `Enter` in the curve editor; and the formant-buffer picker not
  preselecting the current pick.

## 2026-07-17

- Continued Tier 3 CDP work: formant-related catalog entries (Formants Put, Oneform Put,
  and the pitch/frequency-band Formant Vocode processes) and their pipeline plumbing.

## 2026-07-15 – 2026-07-16

- **Tier 2 of the CDP integration: pitch curves.** CDP → Extract Pitch Curve analyses a
  selection (best on a clear monophonic note/melody) into a `[Curve]` buffer with a
  Time/Hz table editor — arrows select, typing overwrites, `n` inserts and `Delete` removes
  points, `t` applies a CDP curve transform (quantise, smooth, vibrato, and the rest of the
  new Repitch process family). A curve can drive any pitch-curve-badged process (e.g. Psow
  Stretch) by loading it into the process's pitch field, rescaled to the selection; `Ctrl+S`
  saves a curve to disk and CDP → Load Pitch Curve reads one back (a hand-typed or loaded
  curve can be edited but can't run a transform, having no CDP source).

## 2026-07-13

- **CDP: reverb re-added, dual-input processes marked in the browser, 36 new processes, and
  a real sample-rate-dependent-range bug fixed.** `Reverb (Comb/Allpass)` is back in the
  catalog (dropped two sessions ago for a WAV-format incompatibility that now has a real
  fix). The process browser marks any process needing a second buffer as input with a pale
  ">1 inputs" note next to its name, so that's visible before opening it. A further pass
  over every CDP binary not yet in the catalog added `caltrain`, `cantor`, `constrict`,
  `distortt`, `frfractal`, `hover`/`hover2`, `prefix`, `strans`, `tremolo`, `rotor`,
  `synfilt`, `clicknew`, `distmark`, `verges`, `motor`, `shifter`, `superaccu`, `brownian`,
  `phasor`, `fastconv` (a new convolution-with-a-second-buffer effect), `subtract`,
  `specsphinx`, `spectwin`, and the start of a `pitch`/`repitch` family (transpose, pick,
  tune, chord-building) — 36 processes total, each individually verified against the real
  CDP binaries. Also fixed `Inharmonic Glissandos`, reported failing at its own unchanged
  default settings: its real valid range depends on the file's sample rate, which the
  catalog previously declared as a fixed range that only happened to work at common sample
  rates.

## 2026-07-12

- **CDP: fixed `rmverb` silently distorting audio.** Reported as "produces good tail but
  entirely distorts the source audio" — the binary was misreading the app's normal 32-bit
  float WAV input as raw integer samples, producing garbled output without ever raising an
  error. Fixed by writing plain 16-bit audio to the small set of CDP binaries that need it,
  rather than the app's usual working format.

## 2026-07-10

- **CDP: three more real bugs fixed from manual testing**, and a fourth parameter shape
  (plain ordered lists — grain-onset times, per-grain transpositions) added alongside the
  existing breakpoint-envelope one, covering `Grain Reposition`, `Grain Repitch`, `Grain
  Rerhythm`, and `Stutter`. The list editor now enforces ascending order for time-based
  lists and scales its nudge step to the actual selection length instead of the CDP
  binary's own maximum (which made a single tap jump by minutes on a short file); `Grain
  Reposition` failed outright at some parameter combinations because a few of its ranges
  depend on the real selection's duration, not a fixed catalog value; and long CDP error
  messages now wrap to fit the dialog instead of being cut off mid-sentence.

## 2026-07-09

- **CDP: 13 new processes SoundThread never covered, and a smoke-test harness to add more
  safely.** The built-in catalog (previously all SoundThread-derived) gains a hand-authored
  extension file with `Time Stretch (Spectral)` (phase-vocoder time-stretch, distinct from
  the existing granular one), `Iterate`, `Gate (Silence)`/`Gate (Trim)`, `Echo`, `DVD Wind`,
  `Flatten`, `Tremolo Envelope`, `Trim Silent Ends`, `Waveset Double`, `Emphasise Changes`,
  `Spectral Band`, and `Impulse Stream` — each verified against the real CDP binaries via a
  new gated test that runs every catalog entry once and asserts it succeeds
  (`TUI_WAVE_CDP_SMOKE=1 cargo test catalog_smoke_test`), catching two real bugs (a wrong
  binary name, two params in the wrong argv position) before they shipped.

## 2026-07-08

- **CDP: breakpoint automation, a two-step browser/params flow, and per-process presets.**
  Any automatable parameter — shown in **green** in the parameter form — can now be driven
  by a breakpoint envelope instead of a fixed value: press `e` on it to open a dedicated
  editor (insert/delete/drag points, coarse and fine nudging, a graphics-mode curve overlay
  with a reference waveform in terminals that support kitty/Sixel/iTerm2 graphics). The
  process browser is a fixed-size list+description dialog again — it no longer resizes as
  you scroll — with working PageUp/PageDown and click-to-open; selecting a process opens a
  separate parameter dialog sized for just that process, with its own scroll if the process
  has more parameters than fit. That dialog also gained a preset row: `s` saves the current
  values under a name, `d` deletes the selected preset, `←`/`→` cycles through saved ones —
  stored per process under `$XDG_CONFIG_HOME/tui-wave/cdp_presets/`. Also fixes a real bug
  where automating certain parameters (e.g. `blur_blur`'s "Blurring") made CDP reject the
  run with an out-of-range error, and tightens up the parameter form's column alignment and
  the browser's description-text margins.

## 2026-07-07

- **CDP (Composer's Desktop Project) integration.** A new dialog-driven front-end to the CDP
  suite of external command-line sound-transformation tools, reachable with `Ctrl+P` or
  **Process → CDP Process…**. Browse/search a catalog of ~120 processes, edit their
  parameters in a generated form, Preview the result through the speakers without touching
  the document, then Apply it to the selection (or whole file) with full undo. Spectral
  processes are wrapped transparently in phase-vocoder analysis/resynthesis; dual-input
  processes (combine/morph/vocode) take a second open buffer via a picker row; synthesis
  processes insert at the cursor. The external binaries run on a background thread so the UI
  never blocks, with a cancellable progress dialog and CDP's own error text surfaced in a
  scrollable viewer. CDP isn't bundled — configure the binaries directory on first use or via
  the new **Options → Configure CDP Directory…** menu. Custom/override process definitions can
  be dropped into `$XDG_CONFIG_HOME/tui-wave/cdp/*.toml` (see
  `docs/cdp-custom-process-example.toml`). The built-in catalog is derived from SoundThread
  (MIT — see `THIRD_PARTY_NOTICES.md`).

## 2026-07-03

- Export Regions' Limit length/Normalize options (added 2026-07-02) gained the validation,
  layout, and mouse fixes a code review turned up: a checked option with a blank or
  unparseable value now blocks "Do!" and focuses the offending field instead of silently
  falling back to a value (a blank Normalize field used to boost every exported region to
  0 dBFS; a blank limit used to silently disable a cap the checkbox said was on); a
  sub-millisecond length limit no longer rounds down to an empty WAV; the dialog's
  clickable "Do!" row now lines up with the rendered hints bar (it was one row off) and
  never collides with a field row on a short terminal; and clicking a checkbox+value row's
  value text now focuses that field for editing instead of only ever toggling the checkbox.
  Also extracted the peak/dB-gain math shared by Normalize, Gain, mix-to-mono, and the
  dB-scale axis into one `model::dsp` module so it can't drift between call sites again.

- Fixed selecting to the end of the file (Shift+End, Shift+`]` past the last marker, and a
  mouse drag into the last visible column) excluding the file's actual last sample —
  selection bounds are exclusive-end everywhere, but these paths clamped to the last
  sample's *index* rather than one past it, so deleting or trimming a "select to end"
  selection always left a sliver of the original ending behind.

## 2026-07-02

- Export Regions to Subfolder gained two more per-region options, both off by default:
  **Limit length** (ms) truncates the end of each region so it can't exceed the given
  duration, and **Normalize regions** (dB) scales each region independently to a target
  peak level. Per-region processing order is limit length, then normalize, then fades — a
  region is trimmed to size before its peak is measured for normalization, and fades are
  applied last so the envelope taper is never itself part of that peak measurement or of
  what gets cut off by the length limit.

- Pressing Enter on a buffer in the Buffers panel now hands focus to the waveform after
  switching to it (both the plain-Enter and filter-search Enter paths), instead of leaving
  the Buffers panel focused — picking a buffer to work on is almost always followed by
  editing it. The Files panel keeps its existing behavior of staying focused after Enter,
  since browsing to open several files in a row shouldn't require re-focusing in between.

- Fade In/Out with no active selection now defaults to a cursor-relative range instead of
  the whole file: Fade In runs from the start of the file to the insertion point, Fade Out
  runs from the insertion point to the end of the file. Other operations that share the
  same "act on the whole file when nothing's selected" default (Normalize, Gain) are
  unaffected — this is fade-specific, since a fade's direction gives it an obvious anchor
  the others don't have.

- The Gain dialog now offers **per-channel gain** on stereo buffers: a "Per-channel gain"
  checkbox (only shown when the active document has exactly 2 channels) splits the single
  Gain field into separate Left/Right dB fields when checked, so each channel can be
  boosted or attenuated independently. Unchecked (the default), Gain behaves exactly as
  before — one value applied uniformly to every channel. Vertical order is Gain/Left, then
  Right (blank until checked), a blank separator, then the checkbox, then Tanh limiter; the
  popup is a fixed size whether or not the box is checked, so toggling it never resizes or
  reflows the dialog.

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
