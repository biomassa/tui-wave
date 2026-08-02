# Praat / praatAudioTools integration — research and implementation plan

**Status: IMPLEMENTED (2026-08-02).** The plan below is kept as the record of the research and
the reasoning; §11 at the end records what was actually built, where it diverged, and what was
deliberately deferred.

**Date of research:** 2026-08-01. **Verified against:** Praat 6.6.30 (June 30 2026) at `/usr/bin/praat`,
and a `main`-branch checkout of praatAudioTools.

---

## 1. Purpose

tui-wave already shells out to one external DSP suite (CDP) through a clean three-layer seam: pure
planning in `src/model/cdp/`, a worker-thread runner in `src/cdp/runner.rs`, and a single `Command`
impl that splices the result back into the document.

The question this document answers: **can praatAudioTools ride that same seam?**

[praatAudioTools](https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools) (MIT) is a large
collection of experimental audio-processing scripts for [Praat](https://praat.org) (GPLv3), not
unlike CDP in spirit — granular, spectral, spatial, distortion and generative processes aimed at
electroacoustic composition.

**Verdict: viable. No hard blockers.** The integration is meaningfully *cheaper* than CDP's was,
because the parameter model maps onto types that already exist in this repo. Everything below was
measured against the real binary, not inferred from documentation.

---

## 2. Findings — Praat as a headless process

| Property | Finding |
|---|---|
| Headless invocation | `praat --run --no-pref-files --no-plugins <driver.praat> <args…>` works with `DISPLAY` and `WAYLAND_DISPLAY` unset. GUI construction in `sys/praat.cpp` sits behind `if (! Melder_batch)`, so no display connection is ever opened. |
| Binary | Single `praat` executable. `praat_nogui` exists only on Debian/Ubuntu; upstream also ships a `-barren` build. **Neither is needed** — and barren is riskier, since 40 plugin scripts draw unconditionally and the normal binary makes the Picture window an offscreen no-op in batch. |
| Exit codes | **0** on success, **255** on any error. |
| Error reporting | stderr carries the message, the offending line number, and the source text. stdout carries only `writeInfoLine` output. Clean and machine-readable. |
| `--run` is mandatory | Per the manual, without it "Praat's behaviour is not guaranteed" when output is redirected — it may start the GUI instead. Always pass it explicitly. |
| `--FULL-TRUST` | **Not needed.** `Save as WAV file:`, `deleteFile:` and `runSystem:` all work under plain `--run`. |
| Float output | **Praat cannot write 32-bit float WAV.** `Save as WAV file:` = 16-bit; `Save as 24-bit WAV file:` = int24; `Save as 32-bit WAV file:` = **int32 PCM** in `WAVE_FORMAT_EXTENSIBLE`. It *reads* f32 natively, so only the return leg quantizes. |
| Metadata | **Destroyed.** No `cue `/`bext`/`adtl` handling on either side of `melder_audiofiles.cpp`. |
| Large files | No RF64/BW64; refuses files >4GB. Irrelevant for selection-sized temp files, fatal for whole takes. |
| Multichannel | Reads and writes >2 channels fine (a 56-channel f32 file round-tripped). |
| Distribution | Packaged on Arch (`extra/praat`), Debian/Ubuntu (`praat`), Homebrew (`--cask praat`). Also standalone tarballs. macOS executable lives at `/Applications/Praat.app/Contents/MacOS/Praat`. |
| Licence | **GPLv3.** Shelling out is not linking and does not affect tui-wave's licensing — the same relationship already held with CDP. Never link or vendor Praat itself. |

### Already de-risked for free

`src/model/wavread.rs:152-176` already resolves `WAVE_FORMAT_EXTENSIBLE` via its SubFormat GUID and
accepts `(WAVE_FORMAT_PCM, 32)`. **Praat's return leg needs no decoder changes at all.**

### Playback cannot be suppressed by environment

`PULSE_SERVER=/dev/null` makes Praat **hang** (timed out at 30s), it does not fail fast. **Do not
attempt to redirect, null out, or disable the audio server.** `Play` also blocks for the audio's
real-time duration, which caused the single timeout in the sample run.

The only safe mitigations:
1. Pass `0` to each script's own `Play`/`Draw` form boolean (~398 scripts have one).
2. Give the runner a **hard wall-clock timeout** — which CDP's runner does not currently have.

---

## 3. Findings — the plugin

480 `.praat` files. 64 in `py/` shell out to Python/IRCAM/VST3 tooling (**out of scope**), leaving
**416** across 14 categories:

| Category | N | Category | N |
|---|---|---|---|
| Generative & Synthesis | 54 | Spectral | 28 |
| Time & Granular | 47 | Pitch | 28 |
| Spatial & Surround | 41 | Modulation | 22 |
| Reverb | 37 | Vector Chain | 19 |
| Filter & Color | 37 | Dynamics & Envelope | 19 |
| Analysis | 36 | Distortion | 16 |
| AI & Adaptive | 30 | Max-MSP | 1 |

### The critical structural fact

**The scripts operate on Praat's selected Sound object and do not read or write files themselves.**
464/480 reference `selected("Sound")`; only ~52 save a WAV, nearly all of those in `py/`. Invoking
one directly fails with `Error: Please select exactly one Sound object.` (exit 255).

Therefore **every invocation needs a generated driver script**. This was verified end to end.

Only ~180 of 416 end with an explicit `selectObject: result`; the rest end on `endproc`, `endif`,
`Play` or `appendInfoLine`. So the driver cannot rely on "whatever is selected at the end" — but a
generic driver that does `select all` and takes the highest-numbered Sound works empirically.

### Empirical success rate

**99/120 (82.5%)** on a random sample (seed 7), driven through a generic driver with auto-derived
defaults, against `tests/fixtures/mono_sine.wav`. Extrapolates to roughly **340 usable processes**
— against CDP's ~407 catalog entries.

The first probe run scored 68%; the gap was entirely my parser mishandling the colon syntax, not
real failures.

### Measured failure classes across the 416

| Class | Count | Detection | Disposition |
|---|---|---|---|
| GUI-blocking: `beginPause`, `pauseScript`, `demo`, `chooseReadFile$`, `chooseFolder$` | 36 | static grep | **Hard exclude** — these segfault (`Gtk-CRITICAL`, exit -11) or hang in batch |
| Non-Sound input (Photo / TextGrid / Table / Matrix) | ~55 | static grep, **loose** | Exclude, refine later |
| Requires 2+ selected Sounds | 21 | static grep | Exclude in v1 |
| Unconditional `Play` | 32 | static grep | Allow; covered by runner timeout |
| Hardcoded absolute paths (`C:/ffmpeg`, `/home/<user>/.praat-dir/…`) | 5 | static grep | Exclude |

These classes overlap, which is why 416 minus the column does not equal 340.

**Vector Chain (19 scripts) is a special case:** its scripts call *sibling* scripts via the
canonical installed prefs path (`~/.praat-dir/plugin_AudioTools/Pitch/Spiral_Pitch_Dance.praat`),
so they only work if the plugin is installed globally. See §7.

---

## 4. Findings — the parameter model

Form blocks are machine-parseable. Keyword histogram across all scripts:

```
positive 1833   boolean 1211   real 1030   optionmenu 954 (+4913 `option` lines)
integer   318   natural  105   sentence 74   choice  12
word        7   text       3   infile     1   comment 2270 (skip)
```

### Parser hazards — both confirmed in the wild

1. **Two syntaxes coexist.** Classic (`real Threshold 0.5`) and colon
   (`optionmenu Preset: 1`, `option: "Custom (use values below)"`). Mishandling the colon form was
   the single largest cause of failures in the first probe run — it produced doubled quotes and
   `Error: Unknown value ""Custom (use values below)"" for option menu "Preset".`
2. **Files are CRLF.**

### Ranges are not machine-readable

Only **19 of 3286** numeric params carry a `(0-1)`-style range, and 82 a unit suffix
(`_Hz`, `_dB`, `_ms`, `_percent`). Praat forms declare a **default only**.

What *is* available is the type's implicit constraint: `positive` > 0, `natural` >= 1,
`integer`/`natural` are whole numbers. tui-wave's `ParamKind::Number` requires min/max/step/default
as mandatory fields, so the converter must synthesize bounds. See §6.

### Calling convention

- `optionmenu`/`choice` arguments must be passed as the **option's label string**, not its index.
- Arity must match the form exactly, or Praat exits 255 with `Found N arguments but expected more.`
- Variable names derive from labels: spaces → underscores, `(Hz)` chopped, first letter lowercased.

---

## 5. Decisions taken

| Decision | Choice | Rationale |
|---|---|---|
| Catalog scope | Auto-convert everything passing the static exclusion filter (~340). No hand-curation gate. | Coverage over polish; the smoke test is the safety net. |
| Synthesized ranges | **Wide — 10× the declared default**, respecting Praat's type floors. | Extreme values are useful for this project's experimental work. An out-of-range value returns a clean Praat error rather than corrupting anything. |
| Code structure | **Fully parallel runner** in `src/praat/runner.rs`, duplicating the CDP pattern. No shared abstraction layer. | Zero risk to working, well-tested CDP code. Accepts one duplicated spawn/cancel/temp-dir implementation. |
| UI surface | **One catalog.** Praat appears as a Domain row in the existing browser. | See §6 — this turns out to be nearly free. |
| Plugin files | **Git submodule**, not vendored. | See below. |

### Plugin distribution — git submodule

The plugin is **not** copied into this repo. Instead it is added as a git submodule and the user
initialises it:

```
git submodule add https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools \
    third_party/praat-audiotools
git submodule update --init          # for an existing clone
```

Why this over vendoring:

- The non-`py` scripts alone are **9.9 MB** — a large payload to carry in-tree.
- Upstream has **no top-level LICENSE file**. The README states MIT and ~all scripts carry a
  `# License: MIT License` header, which is adequate for *attribution* but is a thinner basis for
  *redistribution*. A submodule sidesteps the redistribution question entirely.
- A submodule **pins an exact commit**, which matters because the generated catalog is derived from
  script contents. See the SHA-pinning note in §6.

Consequences to design for:

- `Config.praat_audiotools_dir` defaults to the submodule path but stays overridable, so a user can
  point at their own checkout.
- **An uninitialised submodule is an empty directory, not a missing one.** Validation must detect
  that specifically and say `run git submodule update --init`, rather than reporting a generic
  "not found". This is the most likely first-run failure and deserves its own message.
- The sentinel check (analogous to `cdp::validate_cdp_dir`'s `SENTINEL_BINARIES`) should be the
  presence of a couple of known category directories, e.g. `Distortion/` and `Reverb/`.

---

## 6. Architecture

### The key insight: a Praat process *is* a `ProcessDef`

Two existing types already fit Praat exactly:

- `ParamKind::Choice { options: Vec<String>, default: usize }` (`src/model/cdp/def.rs:395`) stores
  option **label strings** — precisely what `optionmenu` needs passed verbatim.
- `ParamDef.flag: Option<String>` with `None` already means "bare positional argument" — precisely
  Praat's form-order calling convention.

So Praat entries load into the **same** `CdpCatalog` as ordinary `ProcessDef`s. The browser, params
form, `cdp_validate_fields`, preview and undo all work **unchanged** — none of the **171**
`catalog_index` sites in `src/ui/app.rs` are touched. Only planning and running branch.

This is why the integration is cheaper than CDP's was. No new `ParamKind` is required; `Number`,
`Toggle` and `Choice` cover Praat's entire parameter surface.

### Distinguishing the backend — derived, not stored

Add `Category::Praat` to `src/model/cdp/def.rs:14`. This is the right axis because the browser's
Domain column is literally `CdpDomainRow::Domain(Category)` (`src/ui/app.rs:2019`), so the new
domain row comes for free.

Add a *derived* accessor rather than a second stored field, matching this codebase's existing
"derive, never store" stance (the same reasoning documented for `group.rs`):

```rust
impl ProcessDef {
    pub fn backend(&self) -> Backend {
        match self.category {
            Category::Praat => Backend::Praat,
            _ => Backend::Cdp,
        }
    }
}
```

One field, no possibility of category and backend disagreeing.

### Files

**New**

| Path | Role |
|---|---|
| `src/model/praat/mod.rs` | module wiring |
| `src/model/praat/driver.rs` | driver-script generation + Praat string escaping. **Pure, fully unit-testable** — most of the value and most of the tests live here |
| `src/model/praat/plan.rs` | `plan_praat_job(def, values, input) -> Result<PraatPlannedJob, PraatPlanError>`. Pure, no I/O |
| `src/praat/mod.rs`, `src/praat/runner.rs` | worker thread, temp dir + `TempDirGuard`, spawn, pipe draining, cancel, **timeout**, stale-dir sweep |
| `scripts/convert_praat_audiotools.py` | catalog converter |
| `src/model/cdp/praat_catalog.toml` | generated; same "do not hand-edit" header as `catalog.toml` |
| `third_party/praat-audiotools/` | **git submodule** (not committed content) |

**Modified**

| Path | Change |
|---|---|
| `src/model/cdp/def.rs` | `Category::Praat`, `Backend`, `ProcessDef::backend()` |
| `src/model/cdp/catalog.rs` | merge `praat_catalog.toml` into the load chain |
| `src/model/cdp/group.rs` | `Category::Praat` branch deriving the group from `bin`'s leading directory (the 14 plugin categories) instead of the `TIME_BINS`/`PVOC_BINS` tables |
| `src/ui/app.rs` | `CdpDomainRow::label` gains `"Praat"`; the run path branches on `def.backend()` |
| `src/commands/cdp.rs` | `timing_tolerance` gains `Category::Praat => 256` (Praat scripts are time-domain by nature) |
| `src/config.rs` | `praat_bin`, `praat_audiotools_dir` |
| `.gitmodules` | the submodule entry |
| `THIRD_PARTY_NOTICES.md`, `DOCUMENTATION.md`, `MANUAL_TESTING.md` | docs |

**Deliberately not reused:** `PlannedJob` carries ~12 CDP-only fields (`output_curve`,
`output_curve_binary_template`, `output_formant_buffer`, `glob_output`,
`matrix_gain_calibration`, …). A parallel `PraatPlannedJob` with three fields is clearer than
threading a dozen `None`s through it.

### The driver script

Generated per job into the job's temp dir. **This is the exact shape verified working:**

```praat
form Driver
    infile Input_file
    outfile Output_file
endform
snd = Read from file: input_file$
selectObject: snd
runScript: "<absolute path to plugin script>", <arg>, <arg>, …
select all
n = numberOfSelected("Sound")
if n < 1
    exitScript: "praat script produced no Sound object"
endif
last = selected("Sound", n)
selectObject: last
Save as 32-bit WAV file: output_file$
```

Rules for `driver.rs`:

- Numeric args emitted as bare literals; string args (`Choice`, `sentence`, `word`, `text`) wrapped
  in `"` with any embedded `"` **doubled** — Praat's escaping rule.
- `select all` then `selected("Sound", n)` takes the highest-id Sound, i.e. the most recently
  created. Robust across the ~236 scripts that do not end on an explicit `selectObject`.
- **Do not treat `last == snd` as an error.** Some scripts legitimately modify the input Sound in
  place (via `Formula…`). Instead, have the smoke test flag processes whose output is bit-identical
  to their input, so silent no-ops are caught at catalog-build time rather than at runtime.
- Always pass absolute paths. Relative paths resolve against the *calling script's* folder for
  `runScript:` but against the terminal cwd for `--run` — a real trap.

### Runner differences from CDP

1. **Hard wall-clock timeout** — the material new requirement. `Play` blocks in real time and a
   pathological script can hang. Default ~120s, cancellable through the existing `AtomicBool` +
   `POLL_INTERVAL` pattern. Timeout should be its own error variant, not a generic failure.
2. **Binary resolution differs.** CDP resolves `cdp_dir.join(bin)` with **no PATH lookup**. Praat
   should **prefer PATH** (it is packaged nearly everywhere) with `praat_bin` as an override. Probe
   with `praat --version` (exit 0, one line) exactly as `AudioEngine::try_new` probes the audio
   device — Praat support must stay optional and must never block startup.
3. **Return leg is int32**, not f32. No decoder change needed; the working buffer is f32 either way.
4. **Markers and `bext` must be reattached by tui-wave.** Existing `CdpProcessCommand` marker
   remapping already covers this, since it snapshots before the edit.
5. **Channel-count changes are common** — many Spatial & Surround scripts turn mono into 4 or 8
   channels. `CdpProcessCommand::execute` already widens the document. Worth an explicit test:
   splicing an 8-channel result into a stereo document widens the *whole* document to 8 channels.
6. **Sample-rate changes** — carried by `JobOutput.sample_rate`; reuse `App::after_sample_mutation`'s
   existing rebuild-on-rate-change path.
7. **Leave the Praat action off** `action_allowed_on_streamed_buffer` (`src/ui/app.rs:13290`),
   matching every other editing process.

### Converter (`scripts/convert_praat_audiotools.py`)

Mirrors `scripts/convert_soundthread_catalog.py`: run manually, output committed.

- Parse `form … endform`, handling **both syntaxes and CRLF**.
- Type mapping: `real`/`positive`/`integer`/`natural` → `Number`; `boolean` → `Toggle`;
  `optionmenu`/`choice` (plus following `option`/`button` lines) → `Choice`; `sentence`/`word`/`text`
  → text param; skip `comment`.
- **Range synthesis at 10× default**, respecting Praat's floors:

  ```
  real     Threshold      0.5  ->  min -5.0    max 5.0     step 0.01
  positive Grain_size_ms  50   ->  min  0.001  max 500     step 1
  positive Base_freq_Hz   440  ->  min  0.001  max 4400    step 1
  natural  Iterations     3    ->  min  1      max 30      step 1   integer = true
  ```

- **Force `Play`/`Draw`/`Show`/`Visuali*` booleans to `false` and hide them from the UI**
  (name-matched on `^(play|draw|show|visuali)`). The Picture window is unreachable from a TUI and
  `Play` actively blocks.
- Emit the **exclusion filter as a machine-checked report, not silent dropping** — each excluded
  script records a reason (`gui_blocking`, `multi_sound_input`, `non_sound_input`,
  `hardcoded_path`). Keeping reasons in-tree makes the exclusion set reviewable and lets upstream
  fixes be re-tested.
- Group = the script's leading directory; title = cleaned filename; key = `praat_<group>_<stem>`.
- **Record the submodule commit SHA in the generated catalog header**, and add a test that warns
  when the checked-out SHA differs from the one the catalog was generated against. The catalog is
  derived from script contents, so the two must not drift silently.

---

## 7. Phases

Each phase is independently shippable and testable.

1. **Pure core (no UI, no catalog).** `src/model/praat/driver.rs` + `plan.rs`, plus
   `Category::Praat` / `Backend` / `ProcessDef::backend()`. Unit tests assert on generated driver
   text and planned argv. *Riskiest logic, zero integration surface, fastest feedback.*
2. **Runner.** `src/praat/runner.rs` with timeout and cancel. Tests use a fake script and self-skip
   via a `require_praat!` macro mirroring `require_cdp!`.
3. **Submodule + converter.** `.gitmodules`, `scripts/convert_praat_audiotools.py`, generated
   `praat_catalog.toml`, exclusion report. Static catalog tests mirroring `builtin_catalog_parses`
   and `builtin_keys_are_unique`.
4. **Wire into the UI.** Catalog merge, `Category::Praat` domain row, group derivation, backend
   branch at the run site. This is where it becomes usable.
5. **Smoke test + exclusion refinement.** A `catalog_smoke_test` analogue gated behind
   `TUI_WAVE_PRAAT_SMOKE=1`, driving every entry at its defaults against a fixture, with a
   documented `KNOWN_FAILURES` list. Refine the loose `non_sound_input` grep against real results.
   **This is what converts "82.5% on a sample" into a known-good shipped set.**
6. **Docs.** `DOCUMENTATION.md` section, `THIRD_PARTY_NOTICES.md` MIT attribution,
   `MANUAL_TESTING.md` entries.

---

## 8. Verification

- `cargo test` — phases 1-3 are fully covered **without Praat installed** (pure planning,
  driver-text assertions, static catalog assertions).
- `cargo test praat` — runner tests exercise spawn / timeout / cancel against a fake script.
- `TUI_WAVE_PRAAT_SMOKE=1 cargo test --release praat_smoke` — drives every catalog entry at its
  defaults against `tests/fixtures/mono_sine.wav`. Expect ~82.5% first-pass, rising as exclusions
  are refined.
- Manual (add to `MANUAL_TESTING.md`):
  - Open a file, `Ctrl+p`, select the **Praat** domain, run a Distortion process, confirm preview
    audio, Apply, undo.
  - A Spatial & Surround process on a mono file, to confirm channel widening.
  - A long selection with a `Play`-calling script, to confirm the timeout fires cleanly rather than
    hanging the UI.
  - Point `praat_bin` at a nonexistent path and confirm the setup dialog appears and startup is
    unaffected.
  - Deinitialise the submodule and confirm the "run `git submodule update --init`" message appears.

---

## 9. Risks, and what to cut

- **The `non_sound_input` exclusion grep (~55) is loose** and certainly over-excludes — it matches
  scripts that merely *create* a TextGrid or Matrix internally. Phase 5's smoke test should drive
  this number down; do not trust the grep as-is.
- **Vector Chain (19 scripts)** call siblings by the installed prefs path
  (`~/.praat-dir/plugin_AudioTools/…`). Either exclude the category or have the runner symlink the
  submodule into the Praat prefs folder. **Recommend excluding in v1** — it is 19 of 416, and the
  symlink introduces exactly the kind of global-state dependency `--no-plugins` exists to avoid.
- **Multi-Sound-input scripts (21)** map naturally onto CDP's existing `IoKind::DualWav` /
  `VariadicWav` machinery and the `CdpVariadicPicker` UI. Genuinely feasible, but a second project
  — **cut from v1**.
- **Do not** build a Praat equivalent of the envelope/breakpoint editors. Praat forms have no
  breakpoint-shaped parameters.
- **Upstream quality is uneven** — 5 scripts ship with hardcoded Windows paths, 40 draw
  unconditionally, and the README's own counts and directory names disagree with the actual tree
  (README says "415 scripts across 13 categories" and cites a `~/.praat-dir/plugins/` install path
  that does not exist; the correct path is `<prefs>/plugin_AudioTools/`). Treat the repo as data to
  be validated, not as a specification.

---

## 10. Reproducing the research

The probe harness that produced the 82.5% figure:

1. Clone the plugin.
2. For each script, parse its `form` block and derive a default argument list (both syntaxes;
   `optionmenu` → the option label at the declared index; booleans matching
   `^(play|draw|show|visuali)` forced to `0`).
3. Generate the §6 driver script pointing at that plugin script.
4. Run `praat --run --no-pref-files --no-plugins driver.praat in.wav out.wav` with `DISPLAY` and
   `WAYLAND_DISPLAY` unset and a 25s timeout.
5. Classify: exit 0 + non-trivial output file = success; otherwise bucket by the first stderr line.

Sample was 120 scripts, `random.seed(7)`, drawn from the 416 non-`py` scripts, input
`tests/fixtures/mono_sine.wav`.

**Note:** running the harness plays audio through the system output, because 32 scripts call `Play`
unconditionally and it cannot be suppressed from outside. Expect noise.

---

## 11. What was actually built (2026-08-02)

Shipped in four commits, phases 1-6 of §7. **352 processes catalogued**, 1135 tests green.

### Divergences from the plan above, and why

**Vector Chain was included, not excluded** (§9 recommended cutting it). Those 19 scripts locate
sibling scripts through `preferencesDirectory$`. The plan assumed the only fix was symlinking into
the user's own `~/.praat-dir`, which is why it was recommended against. `praat --pref-dir=<DIR>`
turns out to redirect `preferencesDirectory$`, so the runner instead builds an app-owned
preferences directory under `~/.config/tui-wave/praat/` holding a `plugin_AudioTools` symlink.
Same result, no collision with a plugin the user may already have installed, and nothing written
to their Praat setup. Verified end to end: `chain_2` locates and runs `Spiral_Pitch_Dance`.

**Two-Sound scripts were included, not cut** (§9 recommended deferring them). They read inputs
positionally as `selected("Sound", 1)` and `("Sound", 2)`, so the driver reads N files and selects
them together; Praat orders a selection by object number, so read order fixes selection order.
They are catalogued as `IoKind::DualWav`, which meant the existing second-buffer picker worked
with no dialog changes at all — the reuse §6 predicted, holding in practice.

**`Play`/`Draw` parameters are visible, not hidden.** The plan said to hide them. Hiding requires
the field list and the parameter list to diverge, which breaks the index-parallel mapping the
shared dialog relies on. They are instead emitted as ordinary toggles defaulting to **off**, which
achieves the actual goal (nothing plays or draws unless asked) without destabilising a dialog
shared with CDP.

### Real bugs the tests caught

- A `positive` parameter with a 1.1e-9 default made the hardcoded 0.001 range floor exceed the
  computed max. Range synthesis now scales the floor with the default and clamps defensively.
- `option "Normal (1.0)"` (classic syntax) names an option whose text **includes the quotes**,
  while `option: "Normal (1.0)"` (colon syntax) does not. Unquoting both was wrong in each
  direction in turn, producing the identical `Unknown value` error from opposite mistakes. Only
  the keyword's own trailing colon distinguishes them.
- Prose-matching for two-Sound scripts ("select 2 sounds") caught scripts that take one *or* two
  depending on a mode, because the phrase appears in `comment` lines and `option` labels. Detection
  is now an unindented `numberOfSelected("Sound") <> 2` guard, which is unconditional by
  construction.
- `open_cdp_entry` refused to open the browser without a valid `cdp_dir`, which would have locked
  out a Praat-only user entirely.

### Smoke-test result

`TUI_WAVE_PRAAT_SMOKE=1 cargo test --release praat_catalog_smoke` runs every entry at its
defaults. First full run: **347 ran, 19 failed (94.5%)** — well above the 82.5% sampled estimate,
because the static exclusion filter removes the unusable scripts first. Of those 19, five were the
converter bugs above, six are upstream script bugs, and eight are fixture-dependent (they need
stereo, speech or loop content that a synthetic tone cannot provide).

### Deferred, for future consideration

**Free-text parameters — 28 scripts.** Excluded because a `sentence`/`word`/`text` form field has
no bounded editor here. Most of those fields are filesystem paths to corpora (`Folder`,
`Corpus_folder`, `Tools_folder`, `Sofa_file`) that would fail at run time regardless, but a
minority are genuine musical data (`Target_pitches`, `Rhythm_pattern`, `Rule_D`, `Series_values`)
and those are worth having. Doing it properly means splitting the rule — reject a script whose
text field looks like a path, expose the rest — and adding a free-text field type to the parameter
dialog. Every affected script is named under `unparseable_form` in
`docs/praat-excluded-scripts.md`.

**The remaining excluded classes** are not worth revisiting: `py/` (64, shells out to Python/VST3),
`gui_blocking` (22, segfault or hang under `--run`), `non_sound_input`, `not_a_sound_process` and
`hardcoded_path`.
