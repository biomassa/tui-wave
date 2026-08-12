//! Turns a `ProcessDef` plus concrete `ParamValue`s into the exact sequence of CDP
//! invocations needed to process one selection — pure planning, no process spawning and no
//! file I/O (that's `src/cdp/runner.rs`, which executes a `PlannedJob` and does the actual
//! temp-file reads/writes using the real sample data).
//!
//! Since we hold deinterleaved `Vec<Vec<f32>>` in memory, channel split/merge for
//! non-stereo-native processes happens in Rust (`TempWavSpec`/`OutputWavSpec` describe which
//! source/destination channels a temp file corresponds to) — CDP's own `housekeep
//! chans`/`submix interleave` are never invoked. Spectral (`Ana`) processes are wrapped
//! transparently in `pvoc anal`/`pvoc synth` so the browser just shows "Blur -> Average" as
//! one selectable process, not three.

use super::def::{IoKind, NumberScale, ParamValue, ProcessDef};

/// Describes the audio being processed — just enough for plan-time duration/lane
/// calculations. The real sample data lives only in the runner.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputSpec {
    pub channels: usize,
    pub sample_rate: u32,
    pub len_samples: usize,
    /// Head/Tail marks for the DISTMORE family (`ProcessDef.needs_head_tail_marks`), as
    /// sample positions **relative to the start of this input** — already rebased to the
    /// selection rather than absolute in the source document, and already filtered to those
    /// that fall inside it.
    ///
    /// Rebased by the caller (`App::cdp_input_spec`) rather than here because only the caller
    /// knows the selection's offset in the document. What CDP receives is a temp WAV of the
    /// selection alone, so an absolute mark position would point somewhere else entirely in
    /// the file the process actually opens.
    ///
    /// Empty for every process that doesn't need them, which is all but thirteen.
    pub head_tail_marks: Vec<usize>,
}

impl InputSpec {
    fn duration_secs(&self) -> f64 {
        self.len_samples as f64 / self.sample_rate as f64
    }
}

/// FFT analysis settings for spectral processes — exposed as dialog fields (not global
/// config) since window size is a musical parameter, not a fixed preference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PvocSettings {
    pub points: u32,
    pub overlap: u32,
}

impl Default for PvocSettings {
    fn default() -> Self {
        Self { points: 1024, overlap: 3 }
    }
}

/// One external process invocation. `bin` is a bare binary name (e.g. `"blur"`); the runner
/// resolves it against the configured CDP directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub bin: String,
    pub args: Vec<String>,
    /// Short human-readable label for progress display, e.g. `"pvoc anal (L)"`.
    pub label: String,
    /// Relative filename this step is expected to produce — checked for existence
    /// (non-empty) after the step exits, independent of whether it's an intermediate
    /// `.ana`/`.wav` or the job's final output. CDP never creates an output file on failure,
    /// so this is belt-and-braces — but the check is cheap and catches any exit-0-but-no-output
    /// edge case.
    pub expected_output: String,
}

/// A temp input file the runner must write before running the job, and which source audio
/// channels its content comes from (in order — more than one entry means an interleaved
/// multi-channel file). `input_index` selects which of the job's input audio buffers the
/// channels are taken from: 0 is always the processed selection; 1 is the second input
/// (another open buffer) for dual-input processes.
#[derive(Debug, Clone, PartialEq)]
pub struct TempWavSpec {
    pub relative_name: String,
    pub input_index: usize,
    pub source_channels: Vec<usize>,
    /// A linear gain multiplier applied to the raw samples before writing this file —
    /// `None` (every input before 2026-07-26) means no attenuation, same as an implicit
    /// `1.0`. Added for `matrix_matrix_1`'s "Auto Gain Reduction" two-pass scheme
    /// (`MatrixGainCalibration`'s doc comment): the preview pass's input needs a fixed safe
    /// attenuation, and the final pass's input is initially written at that same safe value
    /// as a placeholder (the real gain isn't known until the preview pass completes), then
    /// overwritten in place once it is.
    pub gain: Option<f64>,
}

/// A temp output file the runner must read after the job completes, and which destination
/// channel(s) of the final result its content fills.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputWavSpec {
    pub relative_name: String,
    pub dest_channels: Vec<usize>,
}

/// A `PercentOfAnaWindowCount` parameter can't be resolved until the real `.ana` file
/// exists — CDP recalculates the actual analysis window length from the requested overlap
/// factor in a way that can't be predicted before `pvoc anal` runs. The runner parses
/// `ana_relative_name`'s header for `decfactor` after
/// that step completes, computes the window count, and patches `target` before spawning
/// that step.
///
/// One entry per (channel lane, deferred param) — a stereo file run through a spectral
/// process with this scale produces one entry per channel, since each lane analyzes its
/// own `.ana` file and gets its own real window count. A single `Option` here was the bug
/// behind "blur gives an error" on stereo input: only the last lane's entry survived a
/// plain overwrite, so every earlier channel's argv kept the unresolved "0" placeholder,
/// which CDP rejects as out of range.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredWindowParam {
    pub ana_relative_name: String,
    pub step_index: usize,
    pub target: DeferredWindowTarget,
}

/// What a deferred `PercentOfAnaWindowCount` value patches once the real window count is
/// known — a plain constant patches one argv token; an automated (`ParamValue::Breakpoints`)
/// value instead rewrites a `.brk` file's per-point *values* (never their times, which are
/// already real seconds), since CDP reads breakpoint values in the same units a constant
/// would use. Regression fix: before this existed, an envelope on this one param wrote its
/// raw 0-100 percent values straight into the `.brk` file — CDP then rejected them as
/// literal (and far too small) window counts, e.g. "Value (0.100000) out of range (1.0 to
/// 1632.0)". The `.brk` file is written with placeholder values at plan time (the real
/// count isn't known yet) and rewritten in place once it is.
#[derive(Debug, Clone, PartialEq)]
pub enum DeferredWindowTarget {
    Arg { arg_index: usize, flag: Option<String>, percent: f64 },
    BrkFile { relative_name: String, points: Vec<(f64, f64)> },
}

/// `matrix_matrix_1`'s "Auto Gain Reduction" two-pass gain scheme (2026-07-26) — a fixed
/// formula based on Analysis Channels alone (tried first) turned out insufficient in
/// practice (real user content still clipped, just less than before), since the actual
/// output level depends on the specific random matrix generated *and* the source's own
/// content, neither of which a formula can know in advance. This instead *measures* the
/// real answer: `PlannedJob.steps[0]` runs `matrix matrix 1` on a copy of the input safely
/// pre-attenuated by `preview_attenuation` (small enough that this pass can never clip,
/// regardless of channel count or content) and produces both the transformed preview *and*
/// the matrix data itself (`matrix matrix 1`'s own two-file output). `steps[1]` then runs
/// `matrix matrix 2`, reusing that *same* saved matrix (never regenerated — mode 1's
/// randomness only happens once), applied to a freshly-written copy of the input gained by
/// the exact factor computed from the preview's measured peak. Since `matrix matrix 2`
/// applies a fixed matrix, this is a genuinely linear operator: scaling the input by `k`
/// scales the output by exactly `k` too, so the computed gain is exact, not an estimate —
/// unlike the abandoned formula, this can't under- or over-correct regardless of source
/// content or how the random matrix happened to come out.
#[derive(Debug, Clone, PartialEq)]
pub struct MatrixGainCalibration {
    /// Relative name of `steps[0]`'s real transformed-preview output — its peak amplitude
    /// drives the gain computation.
    pub preview_output_relative_name: String,
    /// The fixed, guaranteed-safe gain `steps[0]`'s own input file was attenuated by.
    pub preview_attenuation: f64,
    /// Peak amplitude to target for the final output — a little under 1.0 to leave some
    /// headroom rather than aiming for exactly full scale.
    pub target_peak: f64,
    /// Every final-pass (`steps[1..]`) input file to (re)write once the real gain is known —
    /// one entry per channel lane (always 1 for mono, 2 for stereo; `matrix` is never
    /// `stereo_native`, so a stereo document still gets one independent final-pass
    /// invocation per channel — see `plan_matrix_with_gain_calibration`'s doc comment for
    /// why every lane shares this one calibration rather than getting its own). Each
    /// entry's own `gain` is a safe placeholder at plan time (mirroring `write_inputs`'
    /// normal behavior); `cdp::runner::resolve_matrix_gain_calibration` overwrites every
    /// entry's file in place with the real computed gain, once, right before the first
    /// final-pass step runs.
    pub final_inputs: Vec<TempWavSpec>,
}

/// A process that produces an unknown number of numbered mono output files sharing a
/// prefix (`IoKind::WavGlob`, e.g. `distcut`/`envcut`'s `cutout0.wav`, `cutout1.wav`, …)
/// instead of one result. The runner scans the temp dir for every `<prefix>N.wav` it finds
/// (sorted numerically) after the job's steps complete, and the UI opens each as its own
/// new buffer rather than splicing a single result into the current selection — the same
/// "one new buffer per output" shape `Action::NewFromLeft`/`NewFromRight` already use.
/// Deliberately mono-only: only the source's first channel is ever written to the temp
/// input file (see `plan_wav_glob`), since merging independently-numbered file sets across
/// stereo lanes (which could even produce different *counts* of files per lane, since the
/// cycle/event detection these processes do is content-dependent) has no well-defined
/// pairing.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobOutputSpec {
    /// Prefix shared by every produced file, e.g. `"cutout"` for `cutout0.wav`,
    /// `cutout1.wav`, ….
    pub prefix: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedJob {
    pub steps: Vec<Invocation>,
    pub input_files: Vec<TempWavSpec>,
    pub output_files: Vec<OutputWavSpec>,
    /// `Some` only for a glob-output process (`IoKind::WavGlob`); `output_files` is always
    /// empty in that case — the two are mutually exclusive result shapes.
    pub glob_output: Option<GlobOutputSpec>,
    /// `Some` only for a curve-in/curve-out process (`IoKind::Curve` — the `repitch` family's
    /// pitch-curve transforms, CDP-Ext-Plan.md Phase 4 "hard tier"). Names the relative temp
    /// file holding the job's final result as plain-text time/Hz breakpoint pairs — never
    /// spliced into an audio `Document` the way `output_files`/`glob_output` are, instead
    /// read back into a `model::curve::PitchCurve`. Mutually exclusive with both of those.
    pub output_curve: Option<String>,
    /// `Some` only for a curve-producing job (extraction or a transform) — the raw-byte
    /// counterpart to `output_curve`. Every subprogram in this family both requires and
    /// produces CDP's binary pitch-WAV format (confirmed against the real binary — see
    /// `plan_curve_transform_job`'s doc comment), so this always names the *pre-
    /// normalization* raw file, before the `repitch pchtotext` step that produces
    /// `output_curve`'s plain-text file runs. Kept as the curve's next `binary_template`,
    /// so a chain of transforms never needs to re-derive one from scratch.
    pub output_curve_binary_template: Option<String>,
    /// `Some` only for a job producing a `model::formant::FormantBuffer` (CDP-Ext-Plan.md
    /// Phase 5 — `plan_extract_formants`'s `formants get` or `plan_oneform_get`'s `oneform
    /// get`), mutually exclusive with `output_curve`/`glob_output`/`output_files`. Unlike a
    /// pitch curve there's no plain-text representation to normalize into at all (formant
    /// data has no hand-editable shape — see `model::formant`'s doc comments), so this just
    /// names the relative temp file whose raw bytes become the new buffer's content
    /// verbatim.
    pub output_formant_buffer: Option<String>,
    /// `Some` only for a normal single-`Wav`-output process whose `ProcessDef` declares
    /// `sidecar_extension` (e.g. `matrix matrix 1`'s generated-matrix-data `.txt` file,
    /// written alongside its real `out.wav` under the same base name) — names the relative
    /// temp file whose raw bytes `cdp::runner` reads back (before the temp dir is cleaned
    /// up) into `CompletedJob.sidecar_bytes`, for the app layer to offer a Save-As prompt on
    /// (`App::tick_cdp`). Unlike `output_curve`/`output_formant_buffer`, this is a genuine
    /// *secondary* result alongside a normal primary one, not the job's only output.
    /// `plan_wav`'s dual-mono-lane branch (a stereo document run through a mono-only
    /// process) only captures the *first* lane's sidecar — each lane's matrix is
    /// independently random-generated, so there's no single file representing both; see
    /// that branch's own doc comment.
    pub output_sidecar: Option<String>,
    /// `Some` only for `matrix_matrix_1`'s "Auto Gain Reduction" (2026-07-26) — see
    /// `MatrixGainCalibration`'s own doc comment for the two-pass scheme this drives.
    /// `cdp::runner` resolves it between `PlannedJob.steps[0]` (the safely pre-attenuated
    /// preview) and `steps[1]` (the final, correctly-gained pass), the same "read an
    /// earlier step's real output, patch a later step's input before it runs" shape
    /// `DeferredWindowParam` already established for `PercentOfAnaWindowCount`.
    pub matrix_gain_calibration: Option<MatrixGainCalibration>,
    pub brk_files: Vec<(String, String)>,
    /// Raw-byte input files to write before running (parallel to `brk_files`, which is
    /// text-only) — used for a curve-transform job's binary pitchfile input, spliced from a
    /// template via `model::curve::splice_pitch_wav_data` before this job is even planned
    /// (see `plan_curve_transform_job`).
    pub binary_input_files: Vec<(String, Vec<u8>)>,
    pub deferred_window_params: Vec<DeferredWindowParam>,
    /// Copied straight from `ProcessDef.requires_simple_wav_input` — carried on the planned
    /// job (rather than the runner needing the `ProcessDef` again) so `cdp::runner`'s
    /// `write_inputs` knows to write plain 16-bit integer PCM instead of the normal 32-bit
    /// float for this one job's input file(s). See that field's doc comment for why.
    pub needs_simple_wav_input: bool,
    /// `Some(g)` for a process in `CLIP_HEADROOM_PROCESSES`: every input file was written
    /// attenuated by `CLIP_HEADROOM_ATTENUATION`, and the runner must multiply the finished
    /// result by `g` (the exact inverse) to undo it. See that list's doc comment for the
    /// measurements this came from, and `cdp::runner`'s `restore_clip_headroom` for what
    /// happens when the restored peak still exceeds full scale.
    pub clip_headroom_restore: Option<f64>,
}

/// How far a `CLIP_HEADROOM_PROCESSES` entry's input is attenuated before it reaches CDP.
///
/// −24 dB, chosen by measurement rather than taste: a catalog-wide sweep at a realistic 0.95
/// input peak found 59-63 entries whose output clipped (the count varies slightly run to run —
/// a few sit right on the threshold), and re-running that same sweep with the input scaled by
/// this factor left exactly two still clipping. Neither of those two is fixable this way —
/// `hilite_arpeg_1` normalizes to full scale by design (it still returns a peak of 1.0000 from
/// a −36 dB input, a gain of +36.6 dB, so no amount of attenuation changes its output), and
/// `fastconv` is handled by forcing its own `-f` float-output flag instead.
///
/// Costs **no** dynamic range, which is what makes so generous a figure affordable. The value
/// is deliberately a power of two and the path is 32-bit float end to end (temp inputs are
/// written `BitDepth::Float32`, CDP works in float, `pvoc synth` writes float back), so
/// scaling by 2⁻⁴ only decrements the exponent — every mantissa bit survives, and multiplying
/// by 2⁴ on the way back restores the original bit pattern exactly. Verified over 2 million
/// random float32 samples: the ÷16 → ×16 round trip is bit-identical, where a non-power-of-two
/// factor (0.1 → ×10) is not. So a process that never needed the headroom is returned
/// unchanged, not merely close to unchanged.
///
/// The reasoning would not hold for a fixed-point path, where 4 bits of headroom really is 4
/// bits of resolution gone — which is why `requires_simple_wav_input` processes (the only ones
/// handed 16-bit integer temp files) must never appear in `CLIP_HEADROOM_PROCESSES`. None do
/// today, and `clip_headroom_never_applies_to_integer_input_processes` keeps it that way.
pub const CLIP_HEADROOM_ATTENUATION: f64 = 1.0 / 16.0;

/// Processes whose output CDP **destructively** clamps at ±1.0 — the clipped samples are gone
/// before the app ever sees the result, so no amount of post-hoc gain can recover them. Their
/// inputs get `CLIP_HEADROOM_ATTENUATION` applied so the clamp is never reached, and the
/// result is scaled back up afterwards.
///
/// Measured, not guessed: this is the set the catalog-wide clipping sweep reported at a 0.95
/// input peak with a peak of exactly 1.0000 *and* a run of consecutive samples pinned there
/// (a flat run is what distinguishes a real clamp from a signal that merely touches full scale
/// once). Runs ranged from 5 to 45056 samples. The union of two sweep runs is used because a
/// few entries sit right on the detection threshold and appear in one run but not the other.
///
/// Deliberately **not** the sweep's other group: 13 entries returned honest peaks *above* 1.0
/// (up to 1.94) rather than clamping. Those lose nothing and are left alone.
///
/// `hilite_arpeg_1` is excluded despite clamping — see `CLIP_HEADROOM_ATTENUATION`.
///
/// Kept here rather than as a `ProcessDef` field because all but a couple of these live in
/// `catalog.toml`, which is machine-generated and carries a "do not hand-edit" header; adding
/// a flag there would mean either losing it on the next converter run or duplicating 45
/// generated entries into `catalog_extra.toml` purely to set one boolean. This is an empirical
/// measurement about CDP's binaries, not a property of the catalog data, so it lives in code
/// next to the constant that explains it.
pub const CLIP_HEADROOM_PROCESSES: &[&str] = &[
    "analjoin_join",
    "blur_chorus_5",
    "blur_drunk",
    "blur_scatter",
    "combine_sum",
    "fastconv_fastconv",
    "filter_bank_5",
    "filter_sweeping_1",
    "filter_sweeping_2",
    "filter_sweeping_3",
    "filter_sweeping_4",
    "filter_variable_3",
    "focus_accu",
    "focus_exag",
    "focus_freeze_1",
    "focus_hold",
    "formants_put_1",
    "formants_put_2",
    "fractal_spectrum",
    "hilite_arpeg_2",
    "matrix_matrix_3",
    "oneform_put_1",
    "oneform_put_2",
    "pitch_octmove_1",
    "pitch_octmove_2",
    "pitch_octmove_3",
    "repitch_transpose_1",
    "repitch_transpose_2",
    "repitch_transpose_3",
    "repitch_transpose_3b",
    "repitch_transposef_1",
    "repitch_transposef_2",
    "repitch_transposef_3",
    "repitch_transposef_3b",
    "specfnu_specfnu_10",
    "specfnu_specfnu_23",
    "specfnu_specfnu_9",
    "spectstr_stretch",
    "spectwin_spectwin_1",
    "spectwin_spectwin_3",
    "strange_glis_1",
    "strange_glis_2",
    "strange_glis_3",
    "stretch_spectrum_2",
    "superaccu_superaccu_1",
    "superaccu_superaccu_2",
];

/// Peak a headroom-restored result is scaled down to when it still exceeds full scale after
/// the attenuation is undone. A little under 1.0 rather than exactly 1.0, so the result has
/// somewhere to go when it is later resampled, summed with neighbouring audio, or converted to
/// an integer depth on save — all of which can overshoot a signal sitting exactly at full
/// scale. Matches `MatrixGainCalibration.target_peak`'s reasoning.
pub const CLIP_HEADROOM_TARGET_PEAK: f32 = 0.99;

/// Whether `key` is in `CLIP_HEADROOM_PROCESSES`.
pub fn needs_clip_headroom(key: &str) -> bool {
    CLIP_HEADROOM_PROCESSES.contains(&key)
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanError {
    /// The process needs per-process special handling that isn't built (currently only
    /// `morph_glide`, which requires a `spec grab` pre-pass to extract single windows
    /// from each input before the glide itself — see SoundThread's make_process special
    /// case).
    UnsupportedInV1 { reason: String },
    /// The process needs audio input but none was given (or vice versa).
    MissingInput,
    ParamCountMismatch { expected: usize, actual: usize },
    /// `plan_job` was handed the wrong number of `InputSpec`s for the process's `IoKind`
    /// arity (0 for synthesis, 1 for Wav/Ana, 2 for DualWav/DualAna).
    InputCountMismatch { expected: usize, actual: usize },
    /// A variadic-input process (`IoKind::VariadicWav`/`GroupedWav`) got a file count its
    /// own shape can't accept. Distinct from `InputCountMismatch`, which names one exact
    /// expected arity: here the valid set is a *range* (and, for `GroupedWav`, a parity
    /// constraint too — CDP itself rejects an odd count with "NUMBER OF INPUT FILES IS NOT
    /// A MULTIPLE OF 2"), so the message has to describe the rule rather than a number.
    VariadicInputCount { reason: String, actual: usize },
    /// Dual-input processing requires both inputs at the same sample rate — CDP itself
    /// rejects mismatched-rate inputs, so this is caught up front with a clearer message.
    /// Variadic-input processes get the same check against input 0 (the selection), for the
    /// same reason: every real binary in that family rejects a mismatch with "Incompatible
    /// sample-rate in input file <name>".
    SampleRateMismatch { first: u32, second: u32 },
    /// A compound datafile param's value breaks a structural rule the CDP binary itself
    /// enforces while parsing that file (`ParamKind::CrystalVdat` — vertices outside the
    /// unit sphere, an event envelope that doesn't start and end at 0, a vertex count that
    /// doesn't match the input-file count, …). Distinct from every error above in *when* it
    /// can happen: the value is structurally well-formed as far as the type system and the
    /// per-cell range clamps go, and only fails a cross-field rule — so it can't be caught
    /// by the UI's own per-field `cdp_validate_fields` and is checked here, where both the
    /// param values and the real input count are in hand. `reason` is written to be shown
    /// verbatim (see `CrystalVdat::validate`); `param` names the field it belongs to.
    InvalidParamData { param: String, reason: String },
    /// A DISTMORE-family process (`ProcessDef.needs_head_tail_marks`) was run against a
    /// selection holding fewer than [`MIN_HEAD_TAIL_PAIRS`] complete Head/Tail pairs. Unlike
    /// every other error here this isn't about the *parameters* at all — the marks live on the
    /// document, so the fix is to place more of them (`h`), not to change a field.
    MissingHeadTailMarks { pairs: usize },
    /// A `head_tail_marks_unpaired` process (scramble's per-segment modes) with no usable cut
    /// time inside the range — every mark counts on its own here, so one is enough and zero
    /// is the only failing case.
    MissingCutTimes { found: usize },
    /// The selection is the wrong width for what this process's binary demands of its input
    /// (`ProcessDef::input_channels`) — a mono selection into a stereo-only process, or a
    /// mono/stereo one into a process that needs more than two channels. Like
    /// `MissingHeadTailMarks` this is not about a *parameter*: no field in the dialog can fix
    /// it, so `reason` is written as a whole user-facing sentence naming the shortfall.
    InputChannelCount { reason: String },
}

/// Complete Head/Tail pairs a DISTMORE process needs before it will run. CDP's own
/// documentation: *"At least two pairs of time-values must be given"* — i.e. four marks.
pub const MIN_HEAD_TAIL_PAIRS: usize = 2;

/// Parses the `decfactor` field out of a `.ana` file's RIFF `note` chunk (hex-encoded
/// little-endian ints, one `key\nhex\n` pair per line — verified against real CDP8
/// output during the Phase 0 spike). Pure byte-parsing so it's unit-testable without
/// touching the filesystem; the runner is what actually reads the file.
pub fn parse_ana_decfactor(data: &[u8]) -> Option<u32> {
    let idx = find_subslice(data, b"note")?;
    let body_start = idx + 4;
    let size = u32::from_le_bytes(data.get(body_start..body_start + 4)?.try_into().ok()?) as usize;
    let body = data.get(body_start + 4..body_start + 4 + size)?;
    let text = std::str::from_utf8(body).ok()?;
    let mut lines = text.split('\n');
    while let Some(key) = lines.next() {
        let Some(value_hex) = lines.next() else { break };
        if key.trim() == "decfactor" {
            let bytes = hex_decode(value_hex.trim())?;
            let arr: [u8; 4] = bytes.try_into().ok()?;
            return Some(u32::from_le_bytes(arr));
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Computes the real analysis window count from a `.ana` file's `decfactor` and the number
/// of samples that went into the analysis.
pub fn window_count_from_decfactor(len_samples: usize, decfactor: u32) -> u32 {
    ((len_samples as f64 / decfactor as f64).ceil() as u32).max(1)
}

fn format_number(v: f64) -> String {
    format!("{v}")
}

/// Formats one resolved value as its final argv token(s) is a single token: bare, or
/// `<flag><value>` when flagged. Returns `None` for a `Toggle(false)` (emits no token at
/// all).
fn format_arg(flag: &Option<String>, value_text: &str) -> Option<String> {
    Some(match flag {
        Some(f) => format!("{f}{value_text}"),
        None => value_text.to_string(),
    })
}

/// Resolves every `NumberScale` variant *except* `PercentOfAnaWindowCount`, which can't be
/// resolved at plan time at all (see `DeferredWindowTarget`'s doc comment) — shared between
/// a plain constant `Number` value and each point's *value* in an automated `Breakpoints`
/// envelope, so both take exactly the same percent-of-duration/percent-of-fft-size math.
fn scale_number_value(
    scale: NumberScale,
    raw: f64,
    duration_secs: f64,
    pvoc: &PvocSettings,
    sample_rate: u32,
) -> f64 {
    match scale {
        NumberScale::Plain | NumberScale::OutputDurationSeconds => raw,
        NumberScale::PercentOfInputDuration => {
            // `.max(0.0)`: for a selection shorter than the 0.1s margin the subtraction
            // goes negative, and a bare (unflagged) negative token like "-0.05" risks
            // being parsed by CDP as an unknown *flag* rather than rejected as an
            // out-of-range value. Zero stays a plain value CDP can reject with its own
            // clear range error — same guard `CappedAtInputDuration` below already has.
            if raw >= 100.0 { (duration_secs - 0.1).max(0.0) } else { duration_secs * raw / 100.0 }
        }
        NumberScale::PercentOfFftSize => (pvoc.points as f64 * raw / 100.0).max(1.0).round(),
        NumberScale::PercentOfAnaWindowCount => {
            unreachable!("PercentOfAnaWindowCount is deferred, never resolved here")
        }
        // Same small safety margin `PercentOfInputDuration`'s 100% case already uses, for
        // the same reason: dodges CDP rejecting a value exactly equal to the file's own
        // duration due to rounding. Left below the catalog's own literal `min` (a genuine
        // CDP-enforced floor, independent of duration) is not this scale's job to protect —
        // a selection shorter than that floor has no valid value at all, which is an
        // inherent CDP limitation for very short selections, not something to work around.
        NumberScale::CappedAtInputDuration => raw.min((duration_secs - 0.01).max(0.0)),
        // See NumberScale::HzCappedToAnalysisRange's doc comment (def.rs) for the finding
        // this came from -- the real accepted range for a Hz-domain param bounded by the
        // analysis window is [sample_rate/points, sample_rate/4], not a fixed Hz range.
        NumberScale::HzCappedToAnalysisRange => {
            let channel_width = sample_rate as f64 / pvoc.points as f64;
            let nyquist_half = sample_rate as f64 / 4.0;
            raw.clamp(channel_width, nyquist_half)
        }
        // See NumberScale::HzCappedToNyquist's doc comment (def.rs): only the *ceiling* is
        // sample-rate-dependent here (the floor is a fixed Hz value the catalog's own `min`
        // already declares), so this clamps down and never up — unlike
        // `HzCappedToAnalysisRange`, whose lower bound is data-dependent too.
        NumberScale::HzCappedToNyquist => raw.min(sample_rate as f64 / 2.0),
        // `duration_secs * sample_rate` recovers the input's sample count exactly —
        // `InputSpec::duration_secs` is `len_samples / sample_rate`, so this is that division
        // undone, not an approximation. Reconstructing it here keeps `len_samples` off this
        // function's signature and out of all seven of its call sites for the sake of one
        // scale. `- 1.0` for the same reason `CappedAtInputDuration` keeps a margin: a start
        // position exactly at end-of-file leaves nothing to process.
        NumberScale::CappedAtInputSamples => {
            raw.min((duration_secs * sample_rate as f64 - 1.0).max(0.0)).round()
        }
        // See NumberScale::AnaFrameStepSeconds (def.rs) for the measurements: the floor is two
        // analysis frames of `points / 2^overlap` samples each, the ceiling the input's own
        // duration. `.max(floor)` on the ceiling keeps the clamp well-formed for a selection
        // shorter than two frames — there is no valid value at all then, so it emits the floor
        // and lets CDP report its own range rather than silently sending a smaller number.
        NumberScale::AnaFrameStepSeconds => {
            let decimation = pvoc.points as f64 / 2f64.powi(pvoc.overlap as i32);
            let floor = 2.0 * decimation / sample_rate.max(1) as f64;
            raw.clamp(floor, (duration_secs - 0.01).max(floor))
        }
    }
}

/// What a param still needs once its argv token (or, for an automated value, a `.brk` file)
/// has already been emitted — `None` for everything resolved outright; `Some` only for the
/// one scale (`PercentOfAnaWindowCount`) that can't be computed until the real `.ana` file
/// exists.
enum DeferredParamKind {
    Arg { flag: Option<String>, percent: f64 },
    BrkFile { relative_name: String, points: Vec<(f64, f64)> },
}

struct ParamPlan {
    /// Fully-resolved argv token to append, in order; `None` for a false Toggle (contributes
    /// no token). For a deferred `PercentOfAnaWindowCount` param, this is a placeholder
    /// token/file the caller records via `deferred` for the runner to patch later.
    arg: Option<String>,
    deferred: Option<DeferredParamKind>,
}

fn plan_param(
    param: &super::def::ParamDef,
    value: &ParamValue,
    duration_secs: f64,
    pvoc: &PvocSettings,
    sample_rate: u32,
    brk_files: &mut Vec<(String, String)>,
    brk_index: usize,
) -> ParamPlan {
    match value {
        ParamValue::Toggle(false) => ParamPlan { arg: None, deferred: None },
        // `.filter(|f| !f.is_empty())`: a toggle with no flag has no meaningful argv shape
        // (an enabled toggle IS its flag token — the flag needn't start with `-`, so a bare
        // word is already expressible as `flag = "word"`). Emitting the old
        // `unwrap_or_default()` empty string instead produced a literal "" argv token that
        // shifted every later positional out of place. No built-in entry does this (a
        // catalog test enforces it), but user-authored catalogs can.
        ParamValue::Toggle(true) => ParamPlan {
            arg: param.flag.clone().filter(|f| !f.is_empty()),
            deferred: None,
        },
        ParamValue::Choice(index) => {
            let super::def::ParamKind::Choice { options, .. } = &param.kind else {
                unreachable!("Choice value paired with non-Choice ParamKind")
            };
            let text = options.get(*index).cloned().unwrap_or_default();
            ParamPlan { arg: format_arg(&param.flag, &text), deferred: None }
        }
        ParamValue::Number(raw) => {
            let super::def::ParamKind::Number { scale, .. } = &param.kind else {
                unreachable!("Number value paired with non-Number ParamKind")
            };
            match scale {
                NumberScale::PercentOfAnaWindowCount => ParamPlan {
                    arg: format_arg(&param.flag, "0"),
                    deferred: Some(DeferredParamKind::Arg { flag: param.flag.clone(), percent: *raw }),
                },
                other => {
                    let value = scale_number_value(*other, *raw, duration_secs, pvoc, sample_rate);
                    ParamPlan { arg: format_arg(&param.flag, &format_number(value)), deferred: None }
                }
            }
        }
        ParamValue::Breakpoints(points) => {
            let super::def::ParamKind::Number { scale, .. } = &param.kind else {
                unreachable!("Breakpoints value paired with non-Number ParamKind")
            };
            let relative_name = format!("brk_{brk_index}.txt");
            match scale {
                // Regression fix: an envelope on this scale used to write its raw 0-100
                // percent values straight into the .brk file — CDP then rejected them as
                // literal (and far too small) window counts. The real count isn't known
                // until the .ana file exists, so write a placeholder now and let the
                // runner rewrite every point's value once it is (`DeferredWindowTarget`).
                NumberScale::PercentOfAnaWindowCount => {
                    let placeholder =
                        points.iter().map(|(t, _)| format!("{t} 0")).collect::<Vec<_>>().join("\n");
                    brk_files.push((relative_name.clone(), placeholder));
                    ParamPlan {
                        arg: format_arg(&param.flag, &relative_name),
                        deferred: Some(DeferredParamKind::BrkFile {
                            relative_name,
                            points: points.clone(),
                        }),
                    }
                }
                other => {
                    let contents = points
                        .iter()
                        .map(|&(t, v)| format!("{t} {}", scale_number_value(*other, v, duration_secs, pvoc, sample_rate)))
                        .collect::<Vec<_>>()
                        .join("\n");
                    brk_files.push((relative_name.clone(), contents));
                    ParamPlan { arg: format_arg(&param.flag, &relative_name), deferred: None }
                }
            }
        }
        // A plain ordered list (no time axis) — one number per line, same "extra text file
        // written to the temp dir, argv token is its filename" mechanism `brk_files`
        // already provides for `Breakpoints`, just without the paired time column. None of
        // the catalog's `required_list` params today use `PercentOfAnaWindowCount`, so
        // unlike `Breakpoints` above this doesn't need a deferred-rewrite path — every
        // scale resolves outright via `scale_number_value`.
        ParamValue::List(values) => {
            let super::def::ParamKind::Number { scale, .. } = &param.kind else {
                unreachable!("List value paired with non-Number ParamKind")
            };
            let relative_name = format!("list_{brk_index}.txt");
            let contents = values
                .iter()
                .map(|&v| format_number(scale_number_value(*scale, v, duration_secs, pvoc, sample_rate)))
                .collect::<Vec<_>>()
                .join("\n");
            brk_files.push((relative_name.clone(), contents));
            ParamPlan { arg: format_arg(&param.flag, &relative_name), deferred: None }
        }
        // A multi-column datafile (`ParamKind::Table`, e.g. tapdelay's `time amp [pan]`
        // taps): one row per line, each row's values space-separated in column order,
        // each resolved through its own column's `NumberScale` — the same "extra text
        // file, argv token is its filename" mechanism `List`/`Breakpoints` already use,
        // just with more than one value per line. None of the catalog's table params use
        // `PercentOfAnaWindowCount`, so — like `List` — every column resolves outright.
        ParamValue::Table(rows) => {
            let super::def::ParamKind::Table { columns, transposed, .. } = &param.kind else {
                unreachable!("Table value paired with non-Table ParamKind")
            };
            let relative_name = format!("table_{brk_index}.txt");
            let cell = |col_index: usize, v: f64| {
                let scale = columns.get(col_index).map(|c| c.scale).unwrap_or(NumberScale::Plain);
                format_number(scale_number_value(scale, v, duration_secs, pvoc, sample_rate))
            };
            // Transposed (`tesselate`): one line per column, holding every row's value for
            // that column. Scaling is still looked up per *column*, exactly as in the normal
            // layout — only where the newlines fall changes. See `ParamKind::Table.transposed`.
            let contents = if *transposed {
                (0..columns.len())
                    .map(|c| {
                        rows.iter()
                            .filter_map(|row| row.get(c).copied())
                            .map(|v| cell(c, v))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                rows.iter()
                    .map(|row| {
                        row.iter()
                            .enumerate()
                            .map(|(c, &v)| cell(c, v))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            brk_files.push((relative_name.clone(), contents));
            ParamPlan { arg: format_arg(&param.flag, &relative_name), deferred: None }
        }
        // `focus freeze`'s bespoke shape: marker character concatenated directly onto the
        // time value with no separator (`"a0.3"`, never `"a 0.3"` — confirmed against the
        // real binary, which rejects the latter as an "unknown time flag"). None of the
        // catalog's marker-time-list params use `PercentOfAnaWindowCount`, so — like
        // `List`/`Table` — the time resolves outright via `scale_number_value`.
        ParamValue::MarkerTimeList(entries) => {
            let super::def::ParamKind::MarkerTimeList { scale, .. } = &param.kind else {
                unreachable!("MarkerTimeList value paired with non-MarkerTimeList ParamKind")
            };
            let relative_name = format!("marktime_{brk_index}.txt");
            let contents = entries
                .iter()
                .map(|&(marker, t)| format!("{marker}{}", format_number(scale_number_value(*scale, t, duration_secs, pvoc, sample_rate))))
                .collect::<Vec<_>>()
                .join("\n");
            brk_files.push((relative_name.clone(), contents));
            ParamPlan { arg: format_arg(&param.flag, &relative_name), deferred: None }
        }
        // `hilite band`'s bitflag-conditional shape: each line is `lofrq hifrq BITFLAG
        // [amp1] [amp2] [[+]transpose]` — the bitflag is a literal 4-character '0'/'1'
        // string (confirmed against the real binary), and each trailing value is present
        // only when its governing bit is set, in that fixed order. None of the catalog's
        // hilite band fields use `PercentOfAnaWindowCount`, so every numeric field
        // resolves outright via `scale_number_value`.
        ParamValue::HiliteBand(rows) => {
            let super::def::ParamKind::HiliteBand { lofrq, hifrq, amp1, amp2, transpose } = &param.kind else {
                unreachable!("HiliteBand value paired with non-HiliteBand ParamKind")
            };
            let relative_name = format!("hiliteband_{brk_index}.txt");
            let resolve = |col: &super::def::TableColumn, v: f64| {
                format_number(scale_number_value(col.scale, v, duration_secs, pvoc, sample_rate))
            };
            let contents = rows
                .iter()
                .map(|row| {
                    let mut tokens = vec![
                        resolve(lofrq, row.lofrq),
                        resolve(hifrq, row.hifrq),
                        format!(
                            "{}{}{}{}",
                            row.amp_bit as u8, row.ramp_bit as u8, row.transpose_bit as u8, row.add_bit as u8
                        ),
                    ];
                    if row.amp_bit {
                        tokens.push(resolve(amp1, row.amp1));
                    }
                    if row.ramp_bit {
                        tokens.push(resolve(amp2, row.amp2));
                    }
                    if row.transpose_bit {
                        let value = resolve(transpose, row.transpose_value);
                        tokens.push(if row.transpose_additive { format!("+{value}") } else { value });
                    }
                    tokens.join(" ")
                })
                .collect::<Vec<_>>()
                .join("\n");
            brk_files.push((relative_name.clone(), contents));
            ParamPlan { arg: format_arg(&param.flag, &relative_name), deferred: None }
        }
        // The buffer's actual bytes never flow through here — `ParamValue::FormantBufferRef`
        // carries no data (see its doc comment); the app layer injects the picked buffer's
        // bytes into `PlannedJob.binary_input_files` at this same `relative_name` after
        // `plan_job` returns. This arm only needs to emit the argv token itself, which is
        // always the catalog-declared `relative_name` verbatim (never flag-prefixed — every
        // real CDP process with this param shape, `formants put`/`oneform put`, takes it as
        // a bare positional filename).
        ParamValue::FormantBufferRef => {
            let super::def::ParamKind::FormantBufferRef { relative_name, .. } = &param.kind else {
                unreachable!("FormantBufferRef value paired with non-FormantBufferRef ParamKind")
            };
            ParamPlan { arg: format_arg(&param.flag, relative_name), deferred: None }
        }
        // The picked file already exists on disk at this absolute path — no temp file to
        // write, no bytes to inject, unlike `FormantBufferRef` (see `ParamKind::FilePath`'s
        // doc comment). Just emit the path itself as the argv token.
        ParamValue::FilePath(path) => ParamPlan { arg: format_arg(&param.flag, path), deferred: None },
        // Praat-only kinds. They reach here only if a hand-written user catalog puts one on a
        // CDP process; the built-in CDP catalog has none, and a Praat job never comes through
        // this planner at all (`model::praat::plan` handles those). Emitted as a plain token so
        // such an entry behaves predictably rather than silently dropping the value.
        ParamValue::Text(text) => ParamPlan { arg: format_arg(&param.flag, text), deferred: None },
        // `crystal rotate`'s two-section VDAT file (see `ParamKind::CrystalVdat`). Same
        // "extra text file in the temp dir, argv token is its filename" mechanism as every
        // other datafile kind; only the file's own layout is bespoke, and it's the one
        // layout in this catalog where *where the newlines fall* is load-bearing rather
        // than cosmetic — see `write_crystal_vdat`. No `NumberScale` is involved at all:
        // coordinates are unitless and the envelope's times are the generated event's own
        // duration, neither of which is a percentage of anything the planner knows.
        ParamValue::CrystalVdat(vdat) => {
            let relative_name = format!("vdat_{brk_index}.txt");
            brk_files.push((relative_name.clone(), write_crystal_vdat(vdat)));
            ParamPlan { arg: format_arg(&param.flag, &relative_name), deferred: None }
        }
    }
}

/// Serializes a `CrystalVdat` into the exact text `crystal rotate` parses (verified against
/// the real binary, not just its usage text): every vertex first, one `x y z` triple per
/// line, then every envelope breakpoint, **two numbers per line**.
///
/// The two-numbers-per-line rule is not stylistic. `crystal.c` splits the sections purely by
/// counting numbers per line — a line of exactly 3 is a vertex, and the *first* line with any
/// other count begins the envelope. Writing the envelope 3-to-a-line therefore makes CDP read
/// those numbers back as additional vertices and then fail with "No envelope data found",
/// which is exactly what happens when you try it. One `(time, value)` pair per line is both
/// the natural layout and, at 2 numbers, unambiguously not a vertex.
///
/// A leading comment naming the two sections: `;` starts a comment anywhere in the file and
/// the parser skips such lines entirely, so this is free, and it makes a job's temp directory
/// readable when debugging a rejected datafile.
fn write_crystal_vdat(vdat: &super::def::CrystalVdat) -> String {
    let mut out = String::from("; crystal vertices: x y z\n");
    for v in &vdat.vertices {
        out.push_str(&format!("{} {} {}\n", format_number(v[0]), format_number(v[1]), format_number(v[2])));
    }
    out.push_str("; event envelope: time value\n");
    for &(t, v) in &vdat.envelope {
        out.push_str(&format!("{} {}\n", format_number(t), format_number(v)));
    }
    out
}

/// Appends `def`'s positional args (subprog, mode) then param args, resolving scales
/// against `duration_secs`/`pvoc`. `brk_files` accumulates side effects that apply to the
/// whole job, not just this one invocation. The returned `Vec` holds one
/// `DeferredWindowTarget` per deferred (`PercentOfAnaWindowCount`) param this invocation's
/// args (or `.brk` files) reference — almost always 0 or 1 in practice (only one catalog
/// param uses that scale today), but a process could in principle carry more than one.
///
/// `brk_index_base` offsets the per-param index used to name generated `.brk`/list/table
/// files (`brk_{i}.txt`, etc.) — every existing single-process caller passes `0` (no change
/// from before this parameter existed). `plan_ana_chain` is the one caller that passes a
/// distinct nonzero base per process in a merged run: those all share one job/temp directory
/// (unlike every other planning function, which gives each process its own job), so two
/// different processes' own param 0 would otherwise both generate `brk_0.txt` and silently
/// clobber each other's file.
fn build_process_args(
    def: &ProcessDef,
    values: &[ParamValue],
    infiles: &[&str],
    outfile: &str,
    duration_secs: f64,
    pvoc: &PvocSettings,
    sample_rate: u32,
    head_tail_marks: &[usize],
    brk_files: &mut Vec<(String, String)>,
    brk_index_base: usize,
) -> Result<(Vec<String>, Vec<DeferredWindowTarget>), PlanError> {
    if values.len() != def.params.len() {
        return Err(PlanError::ParamCountMismatch { expected: def.params.len(), actual: values.len() });
    }

    let mut args = Vec::new();
    if let Some(subprog) = &def.subprog {
        args.push(subprog.clone());
    }
    if let Some(mode) = &def.mode {
        args.push(mode.clone());
    }
    let mut deferred = Vec::new();

    // `fastconv` parses its flags getopt-style, ahead of the filenames — see
    // `ProcessDef::flags_before_infile` for why trailing flags there fail silently rather
    // than erroring. Only flagged params move; bare positionals keep their usual slot.
    if def.flags_before_infile {
        for (i, (param, value)) in def.params.iter().zip(values).enumerate() {
            if param.flag.is_none() || def.is_ui_only_param(i) {
                continue;
            }
            let plan = plan_param(param, value, duration_secs, pvoc, sample_rate, brk_files, brk_index_base + i);
            if let Some(token) = plan.arg {
                match plan.deferred {
                    Some(DeferredParamKind::Arg { flag, percent }) => {
                        deferred.push(DeferredWindowTarget::Arg { arg_index: args.len(), flag, percent });
                    }
                    Some(DeferredParamKind::BrkFile { relative_name, points }) => {
                        deferred.push(DeferredWindowTarget::BrkFile { relative_name, points });
                    }
                    None => {}
                }
                args.push(token);
            }
        }
    }

    args.extend(infiles.iter().map(|s| s.to_string()));
    // Two passes rather than one: a handful of real CDP processes (`pitch altharms`/
    // `octmove`, `formants put`) place a required datafile *between* the input and output
    // filenames — `ParamDef.before_outfile` marks which param(s) need that. Emitting those
    // first, then `outfile`, then everything else (the overwhelmingly common case) gets
    // every process's real argv order right without the common case paying for it: a
    // process with no `before_outfile` params behaves exactly as it always has, since the
    // first pass simply emits nothing and the second pass is byte-identical to the old
    // single-pass loop.
    for (i, (param, value)) in def.params.iter().zip(values).enumerate() {
        if !param.before_outfile || (def.flags_before_infile && param.flag.is_some()) || def.is_ui_only_param(i) {
            continue;
        }
        let plan = plan_param(param, value, duration_secs, pvoc, sample_rate, brk_files, brk_index_base + i);
        if let Some(token) = plan.arg {
            match plan.deferred {
                Some(DeferredParamKind::Arg { flag, percent }) => {
                    deferred.push(DeferredWindowTarget::Arg { arg_index: args.len(), flag, percent });
                }
                Some(DeferredParamKind::BrkFile { relative_name, points }) => {
                    deferred.push(DeferredWindowTarget::BrkFile { relative_name, points });
                }
                None => {}
            }
            args.push(token);
        }
    }

    args.push(outfile.to_string());

    // The DISTMORE family's Head/Tail marklist (`ProcessDef.needs_head_tail_marks`) is a
    // positional datafile immediately after the outfile — `distmore bright 1-3 infile outfile
    // marklist [-s… -d]` — and before any of the flagged params below. It has no `ParamDef`
    // at all: the marks come from the document (`InputSpec.head_tail_marks`, already rebased
    // to the selection), not from a form field, so it's emitted here rather than by
    // `plan_param`. See `ProcessDef::needs_head_tail_marks` for why.
    if def.needs_head_tail_marks {
        // `scramble`'s cuts file reads the same marks as a plain list of boundaries rather
        // than as Head/Tail pairs — see `ProcessDef::head_tail_marks_unpaired`.
        let used: Vec<usize> = if def.head_tail_marks_unpaired {
            // A mark at the very start of the range rebases to time 0.0, which CDP rejects
            // outright ("Invalid time (0.000000) ... Must be greater than zero"), and it would
            // describe a cut with nothing before it anyway.
            let times: Vec<usize> = head_tail_marks.iter().copied().filter(|&m| m > 0).collect();
            if times.is_empty() {
                return Err(PlanError::MissingCutTimes { found: 0 });
            }
            times
        } else {
            let pairs = head_tail_marks.len() / 2;
            if pairs < MIN_HEAD_TAIL_PAIRS {
                return Err(PlanError::MissingHeadTailMarks { pairs });
            }
            // Truncated to whole pairs: CDP reads the list strictly two at a time, and a
            // trailing unpaired Head would leave it reading past the end of the segment list.
            head_tail_marks[..pairs * 2].to_vec()
        };
        let relative_name = "headstails.txt".to_string();
        brk_files.push((
            relative_name.clone(),
            crate::model::headstails::marks_to_text(&used, sample_rate),
        ));
        args.push(relative_name);
    }

    for (i, (param, value)) in def.params.iter().zip(values).enumerate() {
        // A ui-only param (the ChannelSplit toggle and its per-channel values) is read by
        // the app to decide *how* to run the binary, and has no argv token of its own.
        if param.before_outfile || (def.flags_before_infile && param.flag.is_some()) || def.is_ui_only_param(i) {
            continue;
        }
        let plan = plan_param(param, value, duration_secs, pvoc, sample_rate, brk_files, brk_index_base + i);
        if let Some(token) = plan.arg {
            match plan.deferred {
                Some(DeferredParamKind::Arg { flag, percent }) => {
                    deferred.push(DeferredWindowTarget::Arg { arg_index: args.len(), flag, percent });
                }
                Some(DeferredParamKind::BrkFile { relative_name, points }) => {
                    deferred.push(DeferredWindowTarget::BrkFile { relative_name, points });
                }
                None => {}
            }
            args.push(token);
        }
    }

    Ok((args, deferred))
}

fn channel_label(index: usize, total: usize) -> String {
    if total <= 1 {
        String::new()
    } else if total == 2 {
        format!(" ({})", if index == 0 { "L" } else { "R" })
    } else {
        format!(" ({})", index + 1)
    }
}

fn process_label(def: &ProcessDef) -> String {
    match &def.subprog {
        Some(subprog) => format!("{} {subprog}", def.bin),
        None => def.bin.clone(),
    }
}

/// Plans the full sequence of CDP invocations to apply `def` (with `values` in the same
/// order as `def.params`) to `inputs` — empty for a synthesis process, one entry for the
/// selection being processed, two for dual-input processes (the selection plus a second
/// whole buffer). Never spawns a process or touches the filesystem.
pub fn plan_job(
    def: &ProcessDef,
    values: &[ParamValue],
    inputs: &[InputSpec],
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    // Arity comes from `ProcessDef::input_arity` (min, optional max, must-be-even) rather
    // than a table local to this function, so the UI's own pre-Apply gate can consult
    // exactly the same rule — see that method's doc comment. `Curve` reports 0 here even
    // though it never reaches this function in practice (callers use `plan_curve_job`), so a
    // stray call falls through to the dispatch below's `UnsupportedInV1` rather than a
    // confusing `InputCountMismatch` first; `WavGlob` reports 1 for the same "keep the
    // dispatch below in charge of rejecting it" reason.
    let (min_inputs, max_inputs, even_only) = def.input_arity();
    if inputs.is_empty() && min_inputs > 0 {
        return Err(PlanError::MissingInput);
    }
    if max_inputs == Some(min_inputs) && !even_only {
        // Fixed arity (every non-variadic kind): one exact expected count, so the original
        // precise error is still the right one.
        if inputs.len() != min_inputs {
            return Err(PlanError::InputCountMismatch { expected: min_inputs, actual: inputs.len() });
        }
    } else if inputs.len() < min_inputs {
        return Err(PlanError::VariadicInputCount {
            reason: format!("this process needs at least {min_inputs}"),
            actual: inputs.len(),
        });
    } else if even_only && inputs.len() % 2 != 0 {
        // Channel-grouped (`repair repair`): channel-1 sources followed by an equal number
        // of channel-2 sources, so an odd count has no valid split. CDP rejects it too
        // ("NUMBER OF INPUT FILES IS NOT A MULTIPLE OF 2"), but saying it in terms of the
        // groups the UI actually shows is more use than restating the arithmetic.
        return Err(PlanError::VariadicInputCount {
            reason: "needs an equal number of channel-1 and channel-2 sources".into(),
            actual: inputs.len(),
        });
    }
    // Every extra input must match input 0's rate. Written as a scan over `inputs[1..]`
    // rather than the old two-element slice pattern so it covers a variadic list's 3rd,
    // 4th, … file too; for a `Dual*` process it is byte-for-byte the same check as before.
    if let Some(first) = inputs.first() {
        if let Some(other) = inputs[1..].iter().find(|i| i.sample_rate != first.sample_rate) {
            return Err(PlanError::SampleRateMismatch {
                first: first.sample_rate,
                second: other.sample_rate,
            });
        }
    }

    // Compound-datafile params carry cross-field rules that neither the type system nor the
    // UI's per-field validation can express — checked here, before any temp file is written
    // or a process spawned, so the user sees the real reason inline in the params dialog
    // rather than CDP's own message after a full job launch. See
    // `PlanError::InvalidParamData`.
    check_compound_param_data(def, values, inputs.len())?;

    // `WavGlob` (an unknown number of numbered output files) is a distinct enough result
    // shape — one mono lane always, no channel merging, no splice target — that it gets its
    // own planning function rather than threading a glob flag through `plan_wav`'s
    // stereo-lane-splitting logic. Checked on `def.output`, ahead of the `def.input`
    // dispatch below (which stays keyed on input arity as normal). A zero-input glob
    // process (a synthesis program using the numbered-output convention, e.g. `strands`
    // mode 2 — see catalog_extra.toml's removal note) is real but unsupported: erroring
    // here keeps a user-authored catalog entry declaring that combination from panicking
    // the plan (`inputs` is empty for `IoKind::None`, so `&inputs[0]` would).
    if def.output == IoKind::WavGlob {
        // A variadic-input glob process (`repair repair`: N mono files in, N/chans
        // interleaved files out) shares the glob *result* shape but none of `plan_wav_glob`'s
        // single-input assumptions, so it routes to the variadic planner instead — which
        // reads `def.output` itself to decide between one `out.wav` and a numbered set.
        if matches!(def.input, IoKind::VariadicWav | IoKind::GroupedWav) {
            return plan_variadic_wav(def, values, inputs, pvoc);
        }
        let Some(first) = inputs.first() else {
            return Err(PlanError::UnsupportedInV1 {
                reason: "a glob-output process without an audio input is not supported yet".into(),
            });
        };
        return plan_wav_glob(def, values, first, pvoc);
    }

    apply_clip_headroom(def, plan_job_inner(def, values, inputs, pvoc)?)
}

/// Attenuates every input file of a `CLIP_HEADROOM_PROCESSES` job and records the exact
/// inverse for the runner to undo — applied once here, after the dispatch, so all four audio
/// input kinds (`Wav`/`Ana`/`DualWav`/`DualAna`) are covered by one rule rather than four
/// copies of it.
///
/// Deliberately skipped when `matrix_gain_calibration` is set: that scheme already attenuates
/// the input itself and derives a gain from measuring the result, so layering this on top
/// would corrupt its measurement. The two never overlap in practice (`matrix_matrix_3` is on
/// the headroom list; the calibration only ever applies to modes 1 and 2), and this keeps that
/// true by construction rather than by coincidence.
fn apply_clip_headroom(def: &ProcessDef, mut job: PlannedJob) -> Result<PlannedJob, PlanError> {
    if !needs_clip_headroom(&def.key) || job.matrix_gain_calibration.is_some() {
        return Ok(job);
    }
    for spec in &mut job.input_files {
        spec.gain = Some(spec.gain.unwrap_or(1.0) * CLIP_HEADROOM_ATTENUATION);
    }
    job.clip_headroom_restore = Some(1.0 / CLIP_HEADROOM_ATTENUATION);
    Ok(job)
}

fn plan_job_inner(
    def: &ProcessDef,
    values: &[ParamValue],
    inputs: &[InputSpec],
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    match def.input {
        IoKind::None => plan_synthesis(def, values, pvoc),
        // `matrix_matrix_1`/`matrix_matrix_2` with "Auto Gain Reduction" on each get their
        // own two-pass planning function instead of the ordinary single-invocation
        // `plan_wav` — see `plan_matrix_with_gain_calibration`'s and
        // `plan_matrix_apply_with_gain_calibration`'s doc comments. Both return `None`
        // (falling through to `plan_wav` unchanged) for every other process, and for their
        // own process with the toggle off.
        IoKind::Wav => plan_matrix_with_gain_calibration(def, values, &inputs[0], pvoc)
            .or_else(|| plan_matrix_apply_with_gain_calibration(def, values, &inputs[0], pvoc))
            .unwrap_or_else(|| plan_wav(def, values, &inputs[0], pvoc)),
        IoKind::Ana => plan_ana(def, values, &inputs[0], pvoc),
        IoKind::DualWav => plan_dual_wav(def, values, &inputs[0], &inputs[1], pvoc),
        IoKind::DualAna => plan_dual_ana(def, values, &inputs[0], &inputs[1], pvoc),
        // Both variadic arities plan identically — the flat-vs-grouped distinction is
        // entirely about what the *order* means, which only the UI and the arity check
        // above care about (see `IoKind::VariadicWav`'s doc comment).
        IoKind::VariadicWav | IoKind::GroupedWav => plan_variadic_wav(def, values, inputs, pvoc),
        // Never valid as `def.input` (see `IoKind::WavGlob`'s doc comment) — a catalog bug
        // if reached, not a real plan to build.
        IoKind::WavGlob => Err(PlanError::UnsupportedInV1 {
            reason: "WavGlob is not a valid input kind".into(),
        }),
        // Curve processes carry no audio `InputSpec` at all — the caller must use
        // `plan_curve_job` directly instead of routing through this audio-only dispatch
        // (see `IoKind::Curve`'s doc comment).
        IoKind::Curve => Err(PlanError::UnsupportedInV1 {
            reason: "Curve processes must be planned via plan_curve_job, not plan_job".into(),
        }),
        // Praat-only, and planned by `praat::plan::plan_praat_job_with` — no CDP binary reads
        // an image. Reaching this is a catalog bug (a `photo` input on a CDP entry), not a
        // plan to build, so it is refused for the same reason `WavGlob` is.
        IoKind::Photo => Err(PlanError::UnsupportedInV1 {
            reason: "Photo input is a Praat-only kind and cannot be planned as a CDP job".into(),
        }),
    }
}

/// Structural checks for compound datafile params — the rules that span more than one field
/// of a single value, or that couple a value to the run's input-file count, and so have no
/// home in the per-field UI validation (`ui::app`'s `cdp_validate_fields`) or in a
/// `TableColumn`'s min/max.
///
/// **Vertex/input-count mismatch is pre-blocked here rather than passed through to CDP.**
/// `crystal.c` does reject it with a perfectly clear message of its own, but that message
/// only arrives after a full job launch (temp WAVs written for every picked buffer, a
/// subprocess spawned) and lands in the run-failure output viewer, detached from the two
/// controls that actually disagree. Both numbers are known here for free, the rule is exact
/// (not a heuristic), and every other input-count rule this app can state — `input_arity`'s
/// minimum, `GroupedWav`'s even-count split — is already pre-blocked the same way, so
/// passing this one through would be the odd one out. The check deliberately mirrors CDP's
/// own condition exactly, including its `infilecnt > 1` escape: with a single input file any
/// vertex count is legal (the one file is re-read, delayed and transposed, once per vertex),
/// so a solo-buffer run is never blocked on this.
fn check_compound_param_data(
    def: &ProcessDef,
    values: &[ParamValue],
    input_count: usize,
) -> Result<(), PlanError> {
    for (param, value) in def.params.iter().zip(values) {
        // A `rows_match_input_count` table must hold exactly one row per input file — CDP
        // checks it too ("No of data items (1) in 1st line of file table_0.txt doesn't
        // correspond to no of input files (5)"), but only after the job has been written out
        // and spawned. The UI keeps the two in step automatically
        // (`App::sync_cdp_table_to_input_count`), so reaching here means something bypassed
        // that — a hand-edited preset, most likely — and a named error beats CDP's.
        if param.rows_match_input_count {
            if let ParamValue::Table(rows) = value {
                if rows.len() != input_count {
                    return Err(PlanError::InvalidParamData {
                        param: param.name.clone(),
                        reason: format!(
                            "{} row(s) but {input_count} input file(s) — this table needs exactly one row per input",
                            rows.len()
                        ),
                    });
                }
            }
        }
        let ParamValue::CrystalVdat(vdat) = value else { continue };
        if let Err(reason) = vdat.validate() {
            return Err(PlanError::InvalidParamData { param: param.name.clone(), reason });
        }
        if input_count > 1 && vdat.vertices.len() != input_count {
            return Err(PlanError::InvalidParamData {
                param: param.name.clone(),
                reason: format!(
                    "{} input files but {} vertices — with more than one file CDP needs exactly one vertex per file",
                    input_count,
                    vdat.vertices.len()
                ),
            });
        }
    }
    Ok(())
}

/// Plans a glob-output process (`IoKind::WavGlob` — an unknown number of numbered mono
/// output files sharing a prefix, e.g. `distcut`/`envcut`). Always exactly one mono lane:
/// only the source's first channel is written to the temp input file (see
/// `GlobOutputSpec`'s doc comment for why stereo isn't supported here). `expected_output`
/// checks for `<prefix>0.wav` specifically — CDP numbers this family of outputs from 0.
///
/// `def.input == IoKind::Ana` gets the same single-lane `pvoc anal` pre-pass `plan_ana`
/// gives every other Ana-domain process — but no resynthesis step after, unlike `plan_ana`:
/// such a process's own numbered `.wav` outputs are taken to already BE the final result,
/// not `.ana` files still waiting on `pvoc synth`. Found missing (`plan_wav_glob`
/// unconditionally treated its input as a plain WAV) while cataloging `speculate` against
/// the real binary (2026-07-26) — every glob-output process cataloged before it took `Wav`
/// input.
///
/// **No catalog entry currently takes this branch.** `speculate`, the one that prompted it,
/// was removed on 2026-07-27 once its numbered outputs turned out to be pvoc *analysis*
/// files despite the `.wav` names, so the no-resynthesis assumption above was wrong for it
/// specifically (see the batch-3 note in `catalog_extra.toml`). The branch is kept because
/// it is the right shape for a genuinely audio-emitting Ana-input glob process, and its
/// unit test below pins the behavior; a future entry that needs per-output resynthesis
/// would be a *third* case, not a fix to this one.
fn plan_wav_glob(
    def: &ProcessDef,
    values: &[ParamValue],
    input: &InputSpec,
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let duration = input.duration_secs();
    let prefix = "cutout".to_string();

    let mut steps = Vec::new();
    let process_input: String = if def.input == IoKind::Ana {
        let ana_in = "in.ana".to_string();
        steps.push(Invocation {
            bin: "pvoc".into(),
            args: vec![
                "anal".into(),
                "1".into(),
                "in.wav".into(),
                ana_in.clone(),
                format!("-c{}", pvoc.points),
                format!("-o{}", pvoc.overlap),
            ],
            label: "pvoc anal".into(),
            expected_output: ana_in.clone(),
        });
        ana_in
    } else {
        "in.wav".into()
    };

    let process_step_index = steps.len();
    let (args, deferred) = build_process_args(
        def,
        values,
        &[process_input.as_str()],
        &prefix,
        duration,
        pvoc,
        input.sample_rate,
            &[],
        &mut brk_files,
        0,
    )?;
    // A `Wav`-input glob process does its own internal analysis (if any) with no separate
    // `.ana` file this pipeline ever produces, so a `PercentOfAnaWindowCount`-scaled param
    // there would have no real window count to resolve against — preserved from this
    // function's original (pre-Ana-input) form. An `Ana`-input one genuinely has a `.ana`
    // file now (`process_input` above), so its own deferred params, if any, resolve
    // normally as `plan_ana`'s already do.
    debug_assert!(
        def.input != IoKind::Wav || deferred.is_empty(),
        "wav-input glob-output processes never carry ana-window-count params"
    );
    let deferred_window_params = deferred
        .into_iter()
        .map(|target| DeferredWindowParam {
            ana_relative_name: process_input.clone(),
            step_index: process_step_index,
            target,
        })
        .collect();

    steps.push(Invocation {
        bin: def.bin.clone(),
        args,
        label: process_label(def),
        expected_output: format!("{prefix}0.wav"),
    });

    Ok(PlannedJob {
        steps,
        input_files: vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0], gain: None }],
        output_files: Vec::new(),
        glob_output: Some(GlobOutputSpec { prefix }),
        output_curve: None,
        output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
        brk_files,
        binary_input_files: Vec::new(),
        deferred_window_params,
        needs_simple_wav_input: def.requires_simple_wav_input, clip_headroom_restore: None,
    })
}

fn plan_synthesis(
    def: &ProcessDef,
    values: &[ParamValue],
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    // No real input to analyze, so no real sample rate either -- `HzCappedToAnalysisRange`
    // only makes sense for a process reading an actual `.ana` file, which a synthesis
    // process (no input at all) never does. Placeholder value is inert for every other
    // scale, and no catalog entry pairs this scale with an `IoKind::None` process.
    let (args, deferred) =
        build_process_args(def, values, &[], "out.wav", 0.0, pvoc, 44100, &[], &mut brk_files, 0)?;
    debug_assert!(deferred.is_empty(), "synthesis processes have no ana-window-count params");

    let dest_channels = if def.output_is_stereo { vec![0, 1] } else { vec![0] };
    Ok(PlannedJob {
        steps: vec![Invocation {
            bin: def.bin.clone(),
            args,
            label: process_label(def),
            expected_output: "out.wav".into(),
        }],
        input_files: Vec::new(),
        output_files: vec![OutputWavSpec { relative_name: "out.wav".into(), dest_channels }],
        brk_files,
        binary_input_files: Vec::new(),
        glob_output: None,
        output_curve: None,
        output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
        deferred_window_params: Vec::new(),
        needs_simple_wav_input: def.requires_simple_wav_input, clip_headroom_restore: None,
    })
}

/// `matrix matrix 1`'s random-matrix transform has no gain parameter of its own (confirmed
/// against the real binary's own usage text), but its output level scales with Analysis
/// Channels AND with the input's own content — every output spectral bin is a weighted sum
/// of every input bin, so more channels means more accumulated energy per bin, and how much
/// depends on what's actually in those bins. A fixed formula (this function's original,
/// now-replaced form: a calibrated `2.0 / sqrt(channels)` constant) held up against a
/// calibration sine tone but still clipped, just less, against real white-noise content in
/// manual testing (user report, 2026-07-26) — confirmed by re-testing the same formula
/// against white noise myself, it doesn't generalize across content types.
///
/// Replaced with an exact two-pass "measure-then-apply" scheme rather than a better-fitted
/// estimate, since the matrix multiply is linear for a FIXED matrix (confirmed empirically:
/// scaling the input by `k` scales the output by exactly `k`) — reusing the SAME generated
/// matrix across both passes (mode 1 generates and applies it once; mode 2 re-applies a
/// saved one) turns "what gain avoids clipping" from a guess into an exact computation
/// regardless of channel count or content:
///
///   Pass 1 (`steps[0]`, mode 1): run the source, pre-attenuated by a fixed, guaranteed-safe
///           `MATRIX_PREVIEW_ATTENUATION` (-100dB), through the real mode-1 transform. This
///           generates both the real matrix data (saved as this job's sidecar, same as
///           before "Auto Gain Reduction" existed) and a preview output whose peak, scaled
///           back up by `1 / MATRIX_PREVIEW_ATTENUATION`, is exactly what a full-scale input would
///           have produced through this same matrix.
///   Pass 2 (`steps[1..]`, mode 2, one invocation per channel lane): re-run the ORIGINAL
///           input — gained by the exact factor that brings that implied full-scale peak
///           down to `MATRIX_TARGET_PEAK` — through mode 2 with pass 1's saved matrix file,
///           guaranteeing (up to `MATRIX_TARGET_PEAK`'s headroom) no clipping.
///
/// `cdp::runner::resolve_matrix_gain_calibration` does the actual peak-measurement/gain-
/// compute/rewrite between the two passes, once pass 1's real output exists (see
/// `MatrixGainCalibration`'s own doc comment). `matrix` is never `stereo_native` (confirmed
/// in the catalog), so a stereo document still gets one independent mode-2 invocation per
/// channel lane here — every lane shares the one matrix and the one gain, both computed
/// once from lane 0's preview only: measuring each lane independently would need a separate
/// mode-1 preview per lane, each generating a DIFFERENT random matrix, defeating the point
/// of a single reusable "the" matrix as this result's sidecar.
///
/// Returns `None` (falling through to the ordinary single-invocation `plan_wav`) if the
/// process isn't `matrix_matrix_1`, the "Auto Gain Reduction" toggle is off, or the process
/// lacks `sidecar_extension` (needed to name the matrix file mode 2 reads back) — the last
/// case is a catalog-authoring bug if it ever happens, not a real runtime state.
// -40dB (0.01), tried first, still let the PREVIEW pass itself clip on dense white-noise
// content at high Analysis Channels (confirmed by hand, 2026-07-26: -40dB-attenuated noise
// through 1024 channels still overflowed to a real peak of ~2.27, not just close to the
// CDP-reported "peak 148 at full scale, 1024 channels" sine case this constant was
// originally picked against) -- a clipped preview corrupts the whole calibration, since the
// measured peak then understates the true implied full-scale peak, so the computed final
// gain undershoots and the FINAL pass clips too. -100dB left both a sine tone and
// full-scale-equivalent white noise unclipped even at the highest Analysis Channels option
// (16384) in manual testing, with enormous remaining margin (float32's exponent range makes
// attenuating this hard essentially free — no precision loss at either end of a
// `preview_peak / MATRIX_PREVIEW_ATTENUATION` division this small). Shared by both
// `matrix_matrix_1` (`plan_matrix_with_gain_calibration`) and `matrix_matrix_2`
// (`plan_matrix_apply_with_gain_calibration`) -- the same worst-case-content reasoning
// applies identically to both, and mode 2's own clipping (applying a saved matrix to an
// unrelated file) was confirmed worse in practice (peak ~229 vs. mode 1's own worst case).
const MATRIX_PREVIEW_ATTENUATION: f64 = 0.00001; // -100dB
const MATRIX_TARGET_PEAK: f64 = 0.95;

fn plan_matrix_with_gain_calibration(
    def: &ProcessDef,
    values: &[ParamValue],
    input: &InputSpec,
    pvoc: &PvocSettings,
) -> Option<Result<PlannedJob, PlanError>> {
    if def.key != "matrix_matrix_1" {
        return None;
    }
    let toggle_index = def.params.iter().position(|p| p.name == "Auto Gain Reduction")?;
    if !matches!(values.get(toggle_index), Some(ParamValue::Toggle(true))) {
        return None;
    }
    let sidecar_ext = def.sidecar_extension.as_ref()?;

    Some((|| {
        let mut brk_files = Vec::new();
        let duration = input.duration_secs();
        let channels = input.channels.max(1);

        let preview_input = "in_preview.wav".to_string();
        let preview_output = "preview_out.wav".to_string();
        let (preview_args, deferred) = build_process_args(
            def,
            values,
            &[preview_input.as_str()],
            &preview_output,
            duration,
            pvoc,
            input.sample_rate,
            &[],
            &mut brk_files,
            0,
        )?;
        debug_assert!(deferred.is_empty(), "matrix has no ana-window-count params");

        let matrix_file = format!("preview_out.{sidecar_ext}");

        let cyclic_on = def
            .params
            .iter()
            .position(|p| p.name == "Cyclic")
            .and_then(|i| values.get(i))
            .is_some_and(|v| matches!(v, ParamValue::Toggle(true)));

        let mut steps = vec![Invocation {
            bin: def.bin.clone(),
            args: preview_args,
            label: format!("{} (preview)", process_label(def)),
            expected_output: preview_output.clone(),
        }];
        let mut input_files = vec![TempWavSpec {
            relative_name: preview_input,
            input_index: 0,
            source_channels: vec![0],
            gain: Some(MATRIX_PREVIEW_ATTENUATION),
        }];
        let mut output_files = Vec::new();
        let mut final_inputs = Vec::new();

        for ch in 0..channels {
            let (infile, outfile) = if channels == 1 {
                ("in_final.wav".to_string(), "out.wav".to_string())
            } else {
                (format!("in_final_c{}.wav", ch + 1), format!("out_c{}.wav", ch + 1))
            };
            let mut final_args = vec![
                def.subprog.clone().unwrap_or_default(),
                "2".to_string(),
                infile.clone(),
                outfile.clone(),
                matrix_file.clone(),
            ];
            if cyclic_on {
                final_args.push("-c".to_string());
            }
            steps.push(Invocation {
                bin: def.bin.clone(),
                args: final_args,
                label: format!("{}{}", process_label(def), channel_label(ch, channels)),
                expected_output: outfile.clone(),
            });
            // Written at the same safe `MATRIX_PREVIEW_ATTENUATION` initially (a placeholder — the
            // real gain isn't known until steps[0] finishes); `resolve_matrix_gain_calibration`
            // overwrites every `final_inputs` entry's file in place once it is.
            let spec = TempWavSpec {
                relative_name: infile,
                input_index: 0,
                source_channels: vec![ch],
                gain: Some(MATRIX_PREVIEW_ATTENUATION),
            };
            input_files.push(spec.clone());
            final_inputs.push(spec);
            output_files.push(OutputWavSpec { relative_name: outfile, dest_channels: vec![ch] });
        }

        Ok(PlannedJob {
            steps,
            input_files,
            output_files,
            glob_output: None,
            output_curve: None,
            output_curve_binary_template: None,
            output_formant_buffer: None,
            output_sidecar: Some(matrix_file),
            matrix_gain_calibration: Some(MatrixGainCalibration {
                preview_output_relative_name: preview_output,
                preview_attenuation: MATRIX_PREVIEW_ATTENUATION,
                target_peak: MATRIX_TARGET_PEAK,
                final_inputs,
            }),
            brk_files,
            binary_input_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: def.requires_simple_wav_input, clip_headroom_restore: None,
        })
    })())
}

/// `matrix matrix 2` ("Apply Saved Matrix") shares mode 1's clipping problem and its fix
/// (`plan_matrix_with_gain_calibration`'s doc comment) — user report, 2026-07-26: applying a
/// saved matrix to a *different* file clips too, confirmed by hand against the real binary
/// (a matrix generated from a full-scale sine at 1024 channels, applied via mode 2 to
/// unrelated full-scale white noise: peak ~229, far worse than mode 1's own worst case,
/// since the saved matrix's energy characteristics have no relationship at all to the new
/// source it's being applied to).
///
/// Simpler than mode 1's scheme: there's no matrix to *generate* here (`Matrix File` is
/// already a real, fixed path the user picked — a `ParamKind::FilePath` value emitted
/// verbatim by the ordinary per-param loop in `build_process_args`), so both the preview and
/// the final pass are just two ordinary mode-2 invocations differing only in which temp
/// input file they read — no hand-built final args, no sidecar to capture. Same
/// `MATRIX_PREVIEW_ATTENUATION`/`MATRIX_TARGET_PEAK` constants as mode 1 (same worst-case-content
/// reasoning applies identically here).
fn plan_matrix_apply_with_gain_calibration(
    def: &ProcessDef,
    values: &[ParamValue],
    input: &InputSpec,
    pvoc: &PvocSettings,
) -> Option<Result<PlannedJob, PlanError>> {
    if def.key != "matrix_matrix_2" {
        return None;
    }
    let toggle_index = def.params.iter().position(|p| p.name == "Auto Gain Reduction")?;
    if !matches!(values.get(toggle_index), Some(ParamValue::Toggle(true))) {
        return None;
    }

    Some((|| {
        let mut brk_files = Vec::new();
        let duration = input.duration_secs();
        let channels = input.channels.max(1);

        let preview_input = "in_preview.wav".to_string();
        let preview_output = "preview_out.wav".to_string();
        let (preview_args, deferred) = build_process_args(
            def,
            values,
            &[preview_input.as_str()],
            &preview_output,
            duration,
            pvoc,
            input.sample_rate,
            &[],
            &mut brk_files,
            0,
        )?;
        debug_assert!(deferred.is_empty(), "matrix has no ana-window-count params");

        let mut steps = vec![Invocation {
            bin: def.bin.clone(),
            args: preview_args,
            label: format!("{} (preview)", process_label(def)),
            expected_output: preview_output.clone(),
        }];
        let mut input_files = vec![TempWavSpec {
            relative_name: preview_input,
            input_index: 0,
            source_channels: vec![0],
            gain: Some(MATRIX_PREVIEW_ATTENUATION),
        }];
        let mut output_files = Vec::new();
        let mut final_inputs = Vec::new();

        for ch in 0..channels {
            let (infile, outfile) = if channels == 1 {
                ("in_final.wav".to_string(), "out.wav".to_string())
            } else {
                (format!("in_final_c{}.wav", ch + 1), format!("out_c{}.wav", ch + 1))
            };
            let (final_args, deferred) = build_process_args(
                def,
                values,
                &[infile.as_str()],
                &outfile,
                duration,
                pvoc,
                input.sample_rate,
                &[],
                &mut brk_files,
                0,
            )?;
            debug_assert!(deferred.is_empty());
            steps.push(Invocation {
                bin: def.bin.clone(),
                args: final_args,
                label: format!("{}{}", process_label(def), channel_label(ch, channels)),
                expected_output: outfile.clone(),
            });
            let spec = TempWavSpec {
                relative_name: infile,
                input_index: 0,
                source_channels: vec![ch],
                gain: Some(MATRIX_PREVIEW_ATTENUATION),
            };
            input_files.push(spec.clone());
            final_inputs.push(spec);
            output_files.push(OutputWavSpec { relative_name: outfile, dest_channels: vec![ch] });
        }

        Ok(PlannedJob {
            steps,
            input_files,
            output_files,
            glob_output: None,
            output_curve: None,
            output_curve_binary_template: None,
            output_formant_buffer: None,
            output_sidecar: None,
            matrix_gain_calibration: Some(MatrixGainCalibration {
                preview_output_relative_name: preview_output,
                preview_attenuation: MATRIX_PREVIEW_ATTENUATION,
                target_peak: MATRIX_TARGET_PEAK,
                final_inputs,
            }),
            brk_files,
            binary_input_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: def.requires_simple_wav_input, clip_headroom_restore: None,
        })
    })())
}

fn plan_wav(
    def: &ProcessDef,
    values: &[ParamValue],
    input: &InputSpec,
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let duration = input.duration_secs();

    let split_active = def.channel_split_active(values, input.channels);
    // A process that declares `input_channels` states the channel count its binary demands,
    // and takes precedence over every other branch here: it runs once, on exactly those
    // channels, whatever the document's width. Lane-splitting a mono→8-channel spatialiser
    // across a 30-channel take would run it 30 times and then read channel 0 of each result;
    // handing the binary the whole take instead makes it exit 255 (see
    // `ProcessDef::input_channels`). `channel_split` is a per-channel-value toggle no process
    // in this family carries, so the two cannot both be set.
    let declared_channels = match def.input_source_channels(input.channels) {
        Some(Ok(channels)) => Some(channels),
        Some(Err(reason)) => return Err(PlanError::InputChannelCount { reason }),
        None => None,
    };
    if declared_channels.is_some() || ((input.channels <= 1 || def.stereo_native) && !split_active)
    {
        let source_channels: Vec<usize> =
            declared_channels.unwrap_or_else(|| (0..input.channels.max(1)).collect());
        let (args, deferred) = build_process_args(
            def,
            values,
            &["in.wav"],
            "out.wav",
            duration,
            pvoc,
            input.sample_rate,
            &input.head_tail_marks,
            &mut brk_files,
            0,
        )?;
        debug_assert!(deferred.is_empty(), "wav processes never carry ana-window-count params");
        // A `stereo_native` process's real output channel count is `def.output_is_stereo`,
        // not necessarily the *input's* channel count — e.g. `rmverb`/`reverb` always emit
        // stereo (their own `-cN` flag defaults to 2) even from a mono input, since a
        // reverb's two output channels are independently-generated room reflections, not a
        // copy of a single input channel. Read literally as `source_channels.clone()`
        // (this fn's original behavior, correct for the vastly more common case where a
        // channel-preserving process's input and output channel counts always match), a
        // mono input into `rmverb` silently dropped its whole right channel — `load_outputs`
        // only ever reads as many of the real output file's channels as `dest_channels` has
        // entries. For an already-stereo input this is a no-op (`source_channels` is
        // already `[0, 1]`, identical to what this produces).
        //
        // `output_channels` generalizes that same fix past 2: the multichannel family's real
        // output width is a parameter of the run (OUTCHANS), so it can be neither the input's
        // count nor a constant. Under-counting here doesn't error — it silently drops every
        // channel past the last entry, which for a spatialiser is the entire effect.
        let dest_channels = match def.output_channel_count(values) {
            Some(count) => (0..count).collect(),
            None if def.output_is_stereo => vec![0, 1],
            None => source_channels.clone(),
        };
        return Ok(PlannedJob {
            steps: vec![Invocation {
                bin: def.bin.clone(),
                args,
                label: process_label(def),
                expected_output: "out.wav".into(),
            }],
            input_files: vec![TempWavSpec {
                relative_name: "in.wav".into(),
                input_index: 0,
                source_channels, gain: None }],
            output_files: vec![OutputWavSpec { relative_name: "out.wav".into(), dest_channels }],
            brk_files,
            binary_input_files: Vec::new(),
            glob_output: None,
            output_curve: None,
            output_curve_binary_template: None, output_formant_buffer: None,
            output_sidecar: def.sidecar_extension.as_ref().map(|ext| format!("out.{ext}")),
        matrix_gain_calibration: None,
        deferred_window_params: Vec::new(),
        needs_simple_wav_input: def.requires_simple_wav_input, clip_headroom_restore: None,
        });
    }

    // Stereo doc, mono-only process: dual-mono lanes, split/merged in Rust.
    let mut steps = Vec::new();
    let mut input_files = Vec::new();
    let mut output_files = Vec::new();
    // A sidecar-producing process (e.g. `matrix matrix 1`) run per-lane generates an
    // independent one for *each* channel (mode 1's matrix is randomly generated fresh every
    // run) — there's no single file that represents "the" result across lanes the way
    // `output_files` (one real audio channel per lane) does. Rather than drop sidecar
    // capture entirely for a stereo document, only the first lane's is captured and offered
    // for save — the same "one mono lane, first channel" convention `GlobOutputSpec`'s own
    // doc comment already established for this codebase's other lane-per-channel limitation.
    let mut output_sidecar = None;
    for ch in 0..input.channels {
        let infile = format!("in_c{}.wav", ch + 1);
        let outfile = format!("out_c{}.wav", ch + 1);
        // A ChannelSplit process gives each lane its own value for the split param (see
        // `ProcessDef::channel_split_value_index`); every other process passes `values`
        // through untouched, so the clone only happens where it changes something.
        let lane_values: Vec<ParamValue> = match def.channel_split_value_index(values, ch) {
            Some(source) if split_active => {
                let mut lane = values.to_vec();
                let split_param = def.channel_split.as_ref().map(|s| s.param).unwrap_or(source);
                lane[split_param] = values[source].clone();
                lane
            }
            _ => values.to_vec(),
        };
        let (args, deferred) = build_process_args(
            def,
            &lane_values,
            &[infile.as_str()],
            &outfile,
            duration,
            pvoc,
            input.sample_rate,
            // Every lane writes the same marklist file under the same name, which is
            // harmless (identical content) and keeps the marks channel-independent — they
            // describe positions on the timeline, not per-channel data.
            &input.head_tail_marks,
            &mut brk_files,
            0,
        )?;
        debug_assert!(deferred.is_empty());
        let label = format!("{}{}", process_label(def), channel_label(ch, input.channels));
        steps.push(Invocation { bin: def.bin.clone(), args, label, expected_output: outfile.clone() });
        input_files.push(TempWavSpec { relative_name: infile, input_index: 0, source_channels: vec![ch], gain: None });
        output_files.push(OutputWavSpec { relative_name: outfile, dest_channels: vec![ch] });
        if ch == 0 {
            output_sidecar = def.sidecar_extension.as_ref().map(|ext| format!("out_c1.{ext}"));
        }
    }

    Ok(PlannedJob { steps, input_files, output_files, glob_output: None, output_curve: None, output_curve_binary_template: None, output_formant_buffer: None, output_sidecar, matrix_gain_calibration: None, brk_files, binary_input_files: Vec::new(), deferred_window_params: Vec::new(), needs_simple_wav_input: def.requires_simple_wav_input , clip_headroom_restore: None})
}

/// Dual-input time-domain process: `bin subprog [mode] inA inB out params...`. Lanes work
/// like `plan_wav`'s, but pairing channel N of the first input with channel N of the
/// second (a mono input's single channel is reused against every channel of a stereo one).
/// Duration-scaled params resolve against the *first* input (the selection being
/// processed) — the second input is contextual material, not the timeline being edited.
fn plan_dual_wav(
    def: &ProcessDef,
    values: &[ParamValue],
    a: &InputSpec,
    b: &InputSpec,
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let duration = a.duration_secs();
    let lanes = if def.stereo_native { 1 } else { a.channels.max(b.channels).max(1) };

    if lanes == 1 {
        let (args, deferred) = build_process_args(
            def,
            values,
            &["in_a.wav", "in_b.wav"],
            "out.wav",
            duration,
            pvoc,
            a.sample_rate,
            &[],
            &mut brk_files,
            0,
        )?;
        debug_assert!(deferred.is_empty());
        return Ok(PlannedJob {
            steps: vec![Invocation {
                bin: def.bin.clone(),
                args,
                label: process_label(def),
                expected_output: "out.wav".into(),
            }],
            input_files: vec![
                TempWavSpec {
                    relative_name: "in_a.wav".into(),
                    input_index: 0,
                    source_channels: (0..a.channels.max(1)).collect(), gain: None },
                TempWavSpec {
                    relative_name: "in_b.wav".into(),
                    input_index: 1,
                    source_channels: (0..b.channels.max(1)).collect(), gain: None },
            ],
            output_files: vec![OutputWavSpec {
                relative_name: "out.wav".into(),
                dest_channels: (0..a.channels.max(1)).collect(),
            }],
            brk_files,
            binary_input_files: Vec::new(),
            glob_output: None,
            output_curve: None,
            output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
        deferred_window_params: Vec::new(),
        needs_simple_wav_input: def.requires_simple_wav_input, clip_headroom_restore: None,
        });
    }

    let mut steps = Vec::new();
    let mut input_files = Vec::new();
    let mut output_files = Vec::new();
    for ch in 0..lanes {
        let infile_a = format!("in_a_c{}.wav", ch + 1);
        let infile_b = format!("in_b_c{}.wav", ch + 1);
        let outfile = format!("out_c{}.wav", ch + 1);
        let (args, deferred) = build_process_args(
            def,
            values,
            &[infile_a.as_str(), infile_b.as_str()],
            &outfile,
            duration,
            pvoc,
            a.sample_rate,
            &[],
            &mut brk_files,
            0,
        )?;
        debug_assert!(deferred.is_empty());
        let label = format!("{}{}", process_label(def), channel_label(ch, lanes));
        steps.push(Invocation { bin: def.bin.clone(), args, label, expected_output: outfile.clone() });
        input_files.push(TempWavSpec {
            relative_name: infile_a,
            input_index: 0,
            source_channels: vec![ch.min(a.channels.saturating_sub(1))], gain: None });
        input_files.push(TempWavSpec {
            relative_name: infile_b,
            input_index: 1,
            source_channels: vec![ch.min(b.channels.saturating_sub(1))], gain: None });
        output_files.push(OutputWavSpec { relative_name: outfile, dest_channels: vec![ch] });
    }

    Ok(PlannedJob { steps, input_files, output_files, glob_output: None, output_curve: None, output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None, brk_files, binary_input_files: Vec::new(), deferred_window_params: Vec::new(), needs_simple_wav_input: def.requires_simple_wav_input , clip_headroom_restore: None})
}

/// Variadic-input time-domain process: `bin subprog [mode] in_1.wav in_2.wav … out.wav
/// params...` (`IoKind::VariadicWav`/`GroupedWav` — `pulser multi`, `tesselate`, `crystal
/// rotate`, `repair repair`). One invocation, never per-channel lanes: every process in
/// this family rejects a non-mono input outright (confirmed against all four real
/// binaries), so each input contributes exactly its **first channel** to a mono temp file
/// — the same "one mono lane, first channel" convention `plan_wav_glob`/`GlobOutputSpec`
/// already established for this codebase's other mono-only family. Splitting a stereo
/// document into lanes the way `plan_dual_wav` does would be wrong here anyway: with N
/// inputs of possibly differing channel counts there is no meaningful lane pairing, and
/// these processes' outputs are spatial/generative results whose channel count comes from
/// their own parameters, not from the input's.
///
/// Duration-scaled params resolve against `inputs[0]` — the selection being processed,
/// exactly as in `plan_dual_wav`; every later input is contextual source material.
///
/// The result shape follows `def.output`: `IoKind::WavGlob` (only `repair repair` today)
/// produces a numbered set the runner scans for, everything else a single `out.wav` whose
/// channel count is `def.output_is_stereo`. CDP derives the glob names from the outfile
/// stem by inserting `_<n>` before the extension (`out.wav` → `out_0.wav`, `out_1.wav`, …,
/// confirmed against the real binary), which is exactly `GlobOutputSpec`'s
/// `<prefix><n>.wav` scan with `prefix = "out_"`.
fn plan_variadic_wav(
    def: &ProcessDef,
    values: &[ParamValue],
    inputs: &[InputSpec],
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let Some(first) = inputs.first() else { return Err(PlanError::MissingInput) };
    let duration = first.duration_secs();

    let names: Vec<String> = (0..inputs.len()).map(|i| format!("in_{}.wav", i + 1)).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (args, deferred) = build_process_args(
        def,
        values,
        &name_refs,
        "out.wav",
        duration,
        pvoc,
        first.sample_rate,
        &[],
        &mut brk_files,
        0,
    )?;
    debug_assert!(deferred.is_empty(), "variadic wav processes never carry ana-window-count params");

    let input_files = names
        .into_iter()
        .enumerate()
        .map(|(i, relative_name)| TempWavSpec {
            relative_name,
            input_index: i,
            source_channels: vec![0],
            gain: None,
        })
        .collect();

    let is_glob = def.output == IoKind::WavGlob;
    Ok(PlannedJob {
        steps: vec![Invocation {
            bin: def.bin.clone(),
            args,
            label: process_label(def),
            // A glob job's real first file is `out_0.wav`; `out.wav` itself is never
            // written, so checking for it would fail every run.
            expected_output: if is_glob { "out_0.wav".into() } else { "out.wav".into() },
        }],
        input_files,
        output_files: if is_glob {
            Vec::new()
        } else {
            vec![OutputWavSpec {
                relative_name: "out.wav".into(),
                dest_channels: if def.output_is_stereo { vec![0, 1] } else { vec![0] },
            }]
        },
        glob_output: is_glob.then(|| GlobOutputSpec { prefix: "out_".into() }),
        brk_files,
        binary_input_files: Vec::new(),
        output_curve: None,
        output_curve_binary_template: None,
        output_formant_buffer: None,
        output_sidecar: None,
        matrix_gain_calibration: None,
        deferred_window_params: Vec::new(),
        needs_simple_wav_input: def.requires_simple_wav_input, clip_headroom_restore: None,
    })
}

/// Dual-input spectral process: per channel lane, `pvoc anal` both inputs, run the process
/// on the two `.ana` files, `pvoc synth` the result back. Channel pairing follows
/// `plan_dual_wav` (mono reused against stereo); the deferred ana-window-count param can't
/// occur here (only `blur_blur` uses that scale and it's single-input).
fn plan_dual_ana(
    def: &ProcessDef,
    values: &[ParamValue],
    a: &InputSpec,
    b: &InputSpec,
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let duration = a.duration_secs();
    let lanes = a.channels.max(b.channels).max(1);

    // A `spec_grab_prepass` process consumes its first two params as grab positions (one per
    // input) rather than passing them to the binary, so the def/values handed to
    // `build_process_args` below have to be the *remainder*. Split once here rather than per
    // channel — the trimmed def is identical for every lane. See
    // `ProcessDef::spec_grab_prepass` for why each percentage resolves against its own input's
    // duration and can't be a `NumberScale`.
    let grab_times = if def.spec_grab_prepass {
        if def.params.len() < 2 || values.len() < 2 {
            return Err(PlanError::ParamCountMismatch { expected: 2, actual: values.len() });
        }
        let percent = |v: &ParamValue| match v {
            ParamValue::Number(n) => Ok(*n),
            _ => Err(PlanError::ParamCountMismatch { expected: 2, actual: values.len() }),
        };
        Some((percent(&values[0])? / 100.0 * a.duration_secs(), percent(&values[1])? / 100.0 * b.duration_secs()))
    } else {
        None
    };
    let trimmed_def;
    let (def, values) = if grab_times.is_some() {
        trimmed_def = ProcessDef { params: def.params[2..].to_vec(), ..def.clone() };
        (&trimmed_def, &values[2..])
    } else {
        (def, values)
    };

    let mut steps = Vec::new();
    let mut input_files = Vec::new();
    let mut output_files = Vec::new();
    for ch in 0..lanes {
        let label_suffix = channel_label(ch, lanes);
        let wav_a = format!("in_a_c{}.wav", ch + 1);
        let wav_b = format!("in_b_c{}.wav", ch + 1);
        let ana_a = format!("a_a{}.ana", ch + 1);
        let ana_b = format!("a_b{}.ana", ch + 1);
        let ana_out = format!("b{}.ana", ch + 1);
        let wav_out = format!("out_c{}.wav", ch + 1);

        input_files.push(TempWavSpec {
            relative_name: wav_a.clone(),
            input_index: 0,
            source_channels: vec![ch.min(a.channels.saturating_sub(1))], gain: None });
        input_files.push(TempWavSpec {
            relative_name: wav_b.clone(),
            input_index: 1,
            source_channels: vec![ch.min(b.channels.saturating_sub(1))], gain: None });

        for (wav_in, ana, which) in [(&wav_a, &ana_a, "A"), (&wav_b, &ana_b, "B")] {
            steps.push(Invocation {
                bin: "pvoc".into(),
                args: vec![
                    "anal".into(),
                    "1".into(),
                    wav_in.clone(),
                    ana.clone(),
                    format!("-c{}", pvoc.points),
                    format!("-o{}", pvoc.overlap),
                ],
                label: format!("pvoc anal {which}{label_suffix}"),
                expected_output: ana.clone(),
            });
        }

        // With a pre-pass, the process reads the single-window grabs, not the full analyses.
        let (proc_in_a, proc_in_b) = match grab_times {
            None => (ana_a.clone(), ana_b.clone()),
            Some((time_a, time_b)) => {
                let grab_a = format!("g_a{}.ana", ch + 1);
                let grab_b = format!("g_b{}.ana", ch + 1);
                for (src, dest, time, which) in [
                    (&ana_a, &grab_a, time_a, "A"),
                    (&ana_b, &grab_b, time_b, "B"),
                ] {
                    steps.push(Invocation {
                        bin: "spec".into(),
                        args: vec!["grab".into(), src.clone(), dest.clone(), format_number(time)],
                        label: format!("spec grab {which}{label_suffix}"),
                        expected_output: dest.clone(),
                    });
                }
                (grab_a, grab_b)
            }
        };

        let (args, deferred) = build_process_args(
            def,
            values,
            &[proc_in_a.as_str(), proc_in_b.as_str()],
            &ana_out,
            duration,
            pvoc,
            a.sample_rate,
            &[],
            &mut brk_files,
            0,
        )?;
        debug_assert!(deferred.is_empty(), "no dual-input process uses the ana-window-count scale");
        steps.push(Invocation {
            bin: def.bin.clone(),
            args,
            label: format!("{}{label_suffix}", process_label(def)),
            expected_output: ana_out.clone(),
        });

        steps.push(Invocation {
            bin: "pvoc".into(),
            args: vec!["synth".into(), ana_out, wav_out.clone()],
            label: format!("pvoc synth{label_suffix}"),
            expected_output: wav_out.clone(),
        });
        output_files.push(OutputWavSpec { relative_name: wav_out, dest_channels: vec![ch] });
    }

    Ok(PlannedJob { steps, input_files, output_files, glob_output: None, output_curve: None, output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None, brk_files, binary_input_files: Vec::new(), deferred_window_params: Vec::new(), needs_simple_wav_input: def.requires_simple_wav_input , clip_headroom_restore: None})
}

fn plan_ana(
    def: &ProcessDef,
    values: &[ParamValue],
    input: &InputSpec,
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let duration = input.duration_secs();
    let channels = input.channels.max(1);

    let mut steps = Vec::new();
    let mut input_files = Vec::new();
    let mut output_files = Vec::new();
    let mut deferred_window_params = Vec::new();

    for ch in 0..channels {
        let label_suffix = channel_label(ch, channels);
        let wav_in = format!("in_c{}.wav", ch + 1);
        let ana_in = format!("a{}.ana", ch + 1);
        let ana_out = format!("b{}.ana", ch + 1);
        let wav_out = format!("out_c{}.wav", ch + 1);

        input_files.push(TempWavSpec { relative_name: wav_in.clone(), input_index: 0, source_channels: vec![ch], gain: None });

        steps.push(Invocation {
            bin: "pvoc".into(),
            args: vec![
                "anal".into(),
                "1".into(),
                wav_in,
                ana_in.clone(),
                format!("-c{}", pvoc.points),
                format!("-o{}", pvoc.overlap),
            ],
            label: format!("pvoc anal{label_suffix}"),
            expected_output: ana_in.clone(),
        });

        let process_step_index = steps.len();
        let (args, deferred) = build_process_args(
            def,
            values,
            &[ana_in.as_str()],
            &ana_out,
            duration,
            pvoc,
            input.sample_rate,
            &[],
            &mut brk_files,
            0,
        )?;
        // Every lane analyzes its own .ana file, so each accumulates its own entry rather
        // than overwriting a job-wide slot (see DeferredWindowParam's doc comment).
        deferred_window_params.extend(deferred.into_iter().map(|target| DeferredWindowParam {
            ana_relative_name: ana_in.clone(),
            step_index: process_step_index,
            target,
        }));
        steps.push(Invocation {
            bin: def.bin.clone(),
            args,
            label: format!("{}{label_suffix}", process_label(def)),
            expected_output: ana_out.clone(),
        });

        steps.push(Invocation {
            bin: "pvoc".into(),
            args: vec!["synth".into(), ana_out, wav_out.clone()],
            label: format!("pvoc synth{label_suffix}"),
            expected_output: wav_out.clone(),
        });
        output_files.push(OutputWavSpec { relative_name: wav_out, dest_channels: vec![ch] });
    }

    Ok(PlannedJob { steps, input_files, output_files, glob_output: None, output_curve: None, output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None, brk_files, binary_input_files: Vec::new(), deferred_window_params, needs_simple_wav_input: def.requires_simple_wav_input , clip_headroom_restore: None})
}

/// Plans a run of 2+ consecutive single-input spectral (`IoKind::Ana`-in/`IoKind::Ana`-out)
/// CDP Chain steps as ONE analysis/resynthesis round trip instead of one per step: `pvoc
/// anal` runs once per channel, each process in `steps` reads the *previous* one's `.ana`
/// output directly — CDP's own normal PVOC-domain workflow (chaining spectral processes on
/// `.ana` files without resynthesizing audio in between, exactly how a CDP CLI script would
/// do it by hand: `anal` once, several `.ana`-to-`.ana` transforms, `synth` once) — and
/// `pvoc synth` runs once at the very end.
///
/// Used only by the CDP Chain execution engine (`ui/app.rs`'s `submit_current_chain_stage`,
/// which detects such a run); a lone `Ana` step still goes through the ordinary
/// single-process `plan_ana` above, completely unchanged — this function exists
/// side-by-side with it rather than replacing it so the single-process path (and its
/// existing tests/exact `.ana` filenames) carries zero risk from this change. Every `def` in
/// `steps` must have `input == IoKind::Ana` and `output == IoKind::Ana` (checked by the
/// caller, not re-validated here) — a dual-input (`DualAna`) step can't join a run this way,
/// since its secondary input never enters the run's single-buffer `.ana` chain and would
/// need its own anal regardless.
///
/// Every process in the run shares one job (one temp working directory), unlike every other
/// planning function here (one process = one job) — so each gets its own `brk_index_base`
/// (`step_idx * 1000`, comfortably more headroom than any real process's param count) when
/// building its args, or two different processes' own param 0 would both generate
/// `brk_0.txt` and silently clobber each other's file.
pub fn plan_ana_chain(
    steps: &[(&ProcessDef, &[ParamValue])],
    input: &InputSpec,
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    debug_assert!(steps.len() >= 2, "a single step should go through plan_ana instead");
    let mut brk_files = Vec::new();
    let duration = input.duration_secs();
    let channels = input.channels.max(1);

    let mut invocations = Vec::new();
    let mut input_files = Vec::new();
    let mut output_files = Vec::new();
    let mut deferred_window_params = Vec::new();

    for ch in 0..channels {
        let label_suffix = channel_label(ch, channels);
        let wav_in = format!("in_c{}.wav", ch + 1);
        let mut ana_cur = format!("chain_c{}_s0.ana", ch + 1);
        let wav_out = format!("out_c{}.wav", ch + 1);

        input_files.push(TempWavSpec { relative_name: wav_in.clone(), input_index: 0, source_channels: vec![ch], gain: None });

        invocations.push(Invocation {
            bin: "pvoc".into(),
            args: vec![
                "anal".into(),
                "1".into(),
                wav_in,
                ana_cur.clone(),
                format!("-c{}", pvoc.points),
                format!("-o{}", pvoc.overlap),
            ],
            label: format!("pvoc anal{label_suffix}"),
            expected_output: ana_cur.clone(),
        });

        for (step_idx, (def, values)) in steps.iter().enumerate() {
            let ana_next = format!("chain_c{}_s{}.ana", ch + 1, step_idx + 1);
            let process_step_index = invocations.len();
            let (args, deferred) = build_process_args(
                def,
                values,
                &[ana_cur.as_str()],
                &ana_next,
                duration,
                pvoc,
                input.sample_rate,
                &[],
                &mut brk_files,
                step_idx * 1000,
            )?;
            // Each step in the run reads the *previous* step's own `.ana` output as its
            // input — same "every lane accumulates its own deferred-param entry" reasoning
            // as `plan_ana` above, just generalized across steps too, not only channels.
            deferred_window_params.extend(deferred.into_iter().map(|target| DeferredWindowParam {
                ana_relative_name: ana_cur.clone(),
                step_index: process_step_index,
                target,
            }));
            invocations.push(Invocation {
                bin: def.bin.clone(),
                args,
                label: format!("{}{label_suffix}", process_label(def)),
                expected_output: ana_next.clone(),
            });
            ana_cur = ana_next;
        }

        invocations.push(Invocation {
            bin: "pvoc".into(),
            args: vec!["synth".into(), ana_cur, wav_out.clone()],
            label: format!("pvoc synth{label_suffix}"),
            expected_output: wav_out.clone(),
        });
        output_files.push(OutputWavSpec { relative_name: wav_out, dest_channels: vec![ch] });
    }

    Ok(PlannedJob {
        steps: invocations,
        input_files,
        output_files,
        glob_output: None,
        output_curve: None,
        output_curve_binary_template: None,
        output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
        brk_files,
        binary_input_files: Vec::new(),
        deferred_window_params,
        needs_simple_wav_input: steps.iter().any(|(def, _)| def.requires_simple_wav_input),
        // A chain runs several processes back to back over one analysis; attenuating for one
        // link would silently rescale every other link's result too. Chains are built from an
        // explicit user-assembled step list rather than the single-process dialog, so this is
        // left off rather than guessed at — see CLIP_HEADROOM_PROCESSES.
        clip_headroom_restore: None,
    })
}

/// Plans a curve-in/curve-out process (`IoKind::Curve` on both sides) — the `repitch`
/// family's pitch-curve transforms (`invert`, `smooth`, `quantise`, ..., CDP-Ext-Plan.md
/// Phase 4 "hard tier"). No audio anywhere, but — confirmed against the real binary the
/// hard way, after an earlier plain-text version of this function shipped un-runnable —
/// no plain text either: this whole family rejects a text pitchfile outright as its
/// "infile", even CDP's own `pchtotext` round-trip of one ("Application doesn't work with
/// this type of infile"). Only CDP's binary pitch-WAV format works.
///
/// Rather than trying to synthesize that format's header from nothing (`repitch generate`
/// was tried as a text→binary bridge and produced two unexplained anomalies — a silently
/// `.wav`-suffixed filename and a wildly oversized result — before this template approach
/// was found), this always starts from `binary_template`: a real CDP-produced pitchfile
/// (from `plan_extract_pitch_curve` or a prior transform's own result), confirmed to
/// tolerate having *every* one of its `data` chunk's float values replaced while every
/// other chunk (`fmt `, `PEAK`, `cue `, the `LIST`/`adtl`/`note` chunk carrying CDP's own
/// "is a pitch file" marker) stays untouched. `current_points` — this app's own, possibly
/// hand-edited, breakpoint representation — is resampled onto the template's exact
/// per-window time grid (`model::curve::pitch_wav_grid_times`/`resample_to_grid`) and
/// spliced in (`splice_pitch_wav_data`) before this job is even planned, so by the time a
/// real CDP invocation happens the "infile" is indistinguishable from one CDP itself wrote.
///
/// The whole family also *writes* results in this same binary format, with the same
/// `.wav`-auto-suffix quirk `plan_extract_pitch_curve` found for `getpitch` (confirmed
/// against the real binary: `repitch invert`'s own declared outfile got `.wav` appended
/// too) — so the raw result is always normalized through `repitch pchtotext` for display
/// text (`PlannedJob.output_curve`), while the raw bytes themselves become the curve's
/// *next* `binary_template` (`output_curve_binary_template`), so a chain of transforms
/// keeps working without ever re-deriving a template from scratch.
///
/// Curve params never need a duration- or sample-rate-dependent `NumberScale` (there's no
/// selection being processed, no `.ana` file, no real input length) — every param on a
/// catalog-authored `Curve` process must use `NumberScale::Plain`; the placeholder
/// `duration_secs = 0.0`/`sample_rate = 44100` passed to `build_process_args` only matters
/// for the other scales, which curve processes never use.
pub fn plan_curve_transform_job(
    def: &ProcessDef,
    values: &[ParamValue],
    binary_template: &[u8],
    current_points: &[(f64, f64)],
) -> Result<PlannedJob, PlanError> {
    if def.input != IoKind::Curve || def.output != IoKind::Curve {
        return Err(PlanError::UnsupportedInV1 {
            reason: "plan_curve_transform_job requires IoKind::Curve on both input and output".into(),
        });
    }
    let grid = crate::model::curve::pitch_wav_grid_times(binary_template).ok_or_else(|| {
        PlanError::UnsupportedInV1 { reason: "binary_template is not a valid CDP pitch WAV".into() }
    })?;
    let resampled = crate::model::curve::resample_to_grid(current_points, &grid);
    let spliced = crate::model::curve::splice_pitch_wav_data(binary_template, &resampled).ok_or_else(|| {
        PlanError::UnsupportedInV1 { reason: "curve point count doesn't match the template's grid".into() }
    })?;

    let mut brk_files = Vec::new();
    let raw_outfile = "curve_raw_out.pch";
    let (args, deferred) = build_process_args(
        def,
        values,
        &["curve_in.wav"],
        raw_outfile,
        0.0,
        &PvocSettings::default(),
        44100,
        &[],
        &mut brk_files,
        0,
    )?;
    debug_assert!(deferred.is_empty(), "curve processes never carry ana-window-count params");

    // CDP silently appends its own .wav suffix to any binary-pitch-data outfile, regardless
    // of the literal name given (see this fn's doc comment) — declared here so the runner's
    // post-step existence check looks for the file that will actually exist.
    let raw_outfile_actual = format!("{raw_outfile}.wav");
    let steps = vec![
        Invocation {
            bin: def.bin.clone(),
            args,
            label: process_label(def),
            expected_output: raw_outfile_actual.clone(),
        },
        Invocation {
            bin: "repitch".to_string(),
            args: vec!["pchtotext".to_string(), raw_outfile_actual.clone(), "curve_out.txt".to_string()],
            label: "repitch pchtotext".to_string(),
            expected_output: "curve_out.txt".to_string(),
        },
    ];

    Ok(PlannedJob {
        steps,
        input_files: Vec::new(),
        output_files: Vec::new(),
        glob_output: None,
        output_curve: Some("curve_out.txt".to_string()),
        output_curve_binary_template: Some(raw_outfile_actual),
        output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
        brk_files,
        binary_input_files: vec![("curve_in.wav".to_string(), spliced)],
        deferred_window_params: Vec::new(),
        needs_simple_wav_input: false, clip_headroom_restore: None,
    })
}

/// Plans the "Extract Pitch Curve" action (the *producing* end of Phase 4 "hard tier" —
/// unlike every process in the catalog, this one isn't a `ProcessDef` at all, since it's
/// the one asymmetric shape in this whole family: audio *in*, curve *out*. `repitch
/// getpitch` won't accept a plain WAV directly (confirmed against the real binary:
/// "Application doesn't work with this type of infile") — it needs a `.ana` file, so this
/// wraps the selection in `pvoc anal` first, exactly like `plan_ana` does for a real
/// catalog process.
///
/// Uses `repitch getpitch` **mode 1** (the binary pitchfile), not mode 2's plain text —
/// confirmed against the real binary that the whole curve-in/curve-out `repitch` family
/// (`invert`, `smooth`, `quantise`, ...) rejects plain text outright, even CDP's own
/// `pchtotext` round-trip of it ("Application doesn't work with this type of infile"); only
/// the binary format is ever a valid "infile" for a transform. This app still displays a
/// curve as plain text (`model::curve::PitchCurve.points`) — a `repitch pchtotext` step
/// converts the binary result to text for that — but keeps the *real* binary bytes too
/// (`output_curve_binary_template`) as `PitchCurve.binary_template`, the thing any later
/// transform actually runs against (see that field's doc comment for the whole scheme,
/// including why a hand-edit doesn't just get discarded).
///
/// `repitch getpitch` silently writes `<outfile>.wav`, ignoring the literal name given
/// (confirmed against the real binary — the same family of quirk as `strands` mode 2's
/// forced `0` suffix) — `expected_output`/the pchtotext step's input both account for this.
///
/// Only ever takes the *first* channel of a multi-channel selection — a pitch curve is one
/// melodic line, not a per-channel concept, so there is no stereo-lane-splitting the way
/// ordinary audio processes have.
pub fn plan_extract_pitch_curve(pvoc: &PvocSettings) -> PlannedJob {
    let steps = vec![
        Invocation {
            bin: "pvoc".into(),
            args: vec![
                "anal".into(),
                "1".into(),
                "in.wav".into(),
                "in.ana".into(),
                format!("-c{}", pvoc.points),
                format!("-o{}", pvoc.overlap),
            ],
            label: "pvoc anal".into(),
            expected_output: "in.ana".into(),
        },
        Invocation {
            bin: "repitch".into(),
            args: vec![
                "getpitch".into(),
                "1".into(),
                "in.ana".into(),
                "resynth.wav".into(),
                "pitch.pch".into(),
            ],
            label: "repitch getpitch".into(),
            expected_output: "pitch.pch.wav".into(),
        },
        Invocation {
            bin: "repitch".into(),
            args: vec!["pchtotext".into(), "pitch.pch.wav".into(), "pitch.txt".into()],
            label: "repitch pchtotext".into(),
            expected_output: "pitch.txt".into(),
        },
    ];
    PlannedJob {
        steps,
        input_files: vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0], gain: None }],
        output_files: Vec::new(),
        glob_output: None,
        output_curve: Some("pitch.txt".into()),
        output_curve_binary_template: Some("pitch.pch.wav".into()),
        output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
        brk_files: Vec::new(),
        binary_input_files: Vec::new(),
        deferred_window_params: Vec::new(),
        needs_simple_wav_input: false, clip_headroom_restore: None,
    }
}

/// `formants get`'s two mutually exclusive ways to size the extracted envelope (CDP's own
/// `-p`/`-f` flags — see `formants get`'s usage text) — `PitchWise(N)` is N pitch-bands per
/// octave (musically/log spaced), `FreqWise(N)` is 1 point per N equally-spaced (linear Hz)
/// frequency channels. Exposed as two separate menu actions (`Action::ExtractFormants`/
/// `ExtractFormantsFreqwise`) rather than one action with a mode toggle, mirroring
/// `formants_vocode`/`formants_vocode_freq`'s own catalog precedent for the identical
/// choice — see `plan_extract_formants`'s doc comment for why frequency-wise extraction
/// exists as a real, useful alternative rather than a rarely-touched knob: pitch-wise
/// sampling can leave large stretches of a voiced recording's lower pitch-bands reading
/// near-zero, simply because a harmonic series is sparser than the pitch-band grid down
/// there (user report, 2026-07-21 — confirmed against a real recording, not a rendering
/// bug), and equal-Hz spacing doesn't have that particular blind spot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormantExtractionMode {
    PitchWise(u32),
    FreqWise(u32),
}

impl FormantExtractionMode {
    fn cdp_flag(self) -> String {
        match self {
            FormantExtractionMode::PitchWise(n) => format!("-p{n}"),
            FormantExtractionMode::FreqWise(n) => format!("-f{n}"),
        }
    }
}

/// Plans the "Extract Formants" action (CDP-Ext-Plan.md Phase 5, the producing end of the
/// same asymmetric shape `plan_extract_pitch_curve` has — audio *in*, buffer *out*). `formants
/// get` won't accept a plain WAV directly (it needs a `.ana` file, same as `repitch getpitch`),
/// so this wraps the selection in `pvoc anal` first.
///
/// Unlike `repitch getpitch`, `formants get` does **not** silently append `.wav` to its
/// declared outfile (confirmed against the real binary: a run with `outfile = "out.for"`
/// produced literally `out.for`, not `out.for.wav`) — a different quirk from the pitch-curve
/// family that must be handled separately rather than assumed uniform.
///
/// `mode`'s own band/channel count is a fixed default (`8`, matching `formants_vocode`'s own
/// default for either flag) rather than a user-facing choice — mirrors
/// `plan_extract_pitch_curve`'s own zero-config simplicity (no dialog, just one action on
/// the current selection); only the pitch-wise-vs-frequency-wise choice itself is exposed,
/// as two separate menu actions (see `FormantExtractionMode`'s doc comment).
///
/// Only ever takes the *first* channel of a multi-channel selection, same rationale as
/// `plan_extract_pitch_curve`: a formant envelope is one spectral shape, not a per-channel
/// concept in this app's UI.
pub fn plan_extract_formants(pvoc: &PvocSettings, mode: FormantExtractionMode) -> PlannedJob {
    let steps = vec![
        Invocation {
            bin: "pvoc".into(),
            args: vec![
                "anal".into(),
                "1".into(),
                "in.wav".into(),
                "in.ana".into(),
                format!("-c{}", pvoc.points),
                format!("-o{}", pvoc.overlap),
            ],
            label: "pvoc anal".into(),
            expected_output: "in.ana".into(),
        },
        Invocation {
            bin: "formants".into(),
            args: vec!["get".into(), "in.ana".into(), "out.for".into(), mode.cdp_flag()],
            label: "formants get".into(),
            expected_output: "out.for".into(),
        },
    ];
    PlannedJob {
        steps,
        input_files: vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0], gain: None }],
        output_files: Vec::new(),
        glob_output: None,
        output_curve: None,
        output_curve_binary_template: None,
        output_formant_buffer: Some("out.for".into()), output_sidecar: None, matrix_gain_calibration: None,
        brk_files: Vec::new(),
        binary_input_files: Vec::new(),
        deferred_window_params: Vec::new(),
        needs_simple_wav_input: false, clip_headroom_restore: None,
    }
}

/// Plans `oneform get` — CDP-Ext-Plan.md Phase 5's "freeze snapshot" action, the second
/// asymmetric shape this family needs. Unlike `plan_extract_formants` (audio in, buffer out)
/// this one's input is itself a `[f]` buffer's raw bytes (`oneform get`'s own usage
/// text: `oneform get informantfile 1f-outfile time`) — there's no audio and no `pvoc anal`
/// step at all, just one CDP invocation with the caller-picked buffer spliced in as a plain
/// `binary_input_files` entry (the same "write raw bytes, argv token is the filename"
/// mechanism `plan_curve_transform_job` already uses for a pitch-curve template).
///
/// `oneform get` **does** silently append `.wav` to its declared outfile (confirmed against
/// the real binary — the opposite of `formants get`'s behavior, so this is not assumed
/// uniform across the family), hence `expected_output`/`output_formant_buffer` both naming
/// `moment.1f.wav` rather than the literal `moment.1f` passed on the command line.
pub fn plan_oneform_get(formant_buffer_bytes: &[u8], time_secs: f64) -> PlannedJob {
    let steps = vec![Invocation {
        bin: "oneform".into(),
        args: vec!["get".into(), "in.for".into(), "moment.1f".into(), format_number(time_secs)],
        label: "oneform get".into(),
        expected_output: "moment.1f.wav".into(),
    }];
    PlannedJob {
        steps,
        input_files: Vec::new(),
        output_files: Vec::new(),
        glob_output: None,
        output_curve: None,
        output_curve_binary_template: None,
        output_formant_buffer: Some("moment.1f.wav".into()), output_sidecar: None, matrix_gain_calibration: None,
        brk_files: Vec::new(),
        binary_input_files: vec![("in.for".to_string(), formant_buffer_bytes.to_vec())],
        deferred_window_params: Vec::new(),
        needs_simple_wav_input: false, clip_headroom_restore: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::def::{Category, ParamDef, ParamKind};

    fn number_param(name: &str, min: f64, max: f64, default: f64, scale: NumberScale) -> ParamDef {
        ParamDef {
            rows_match_input_count: false,
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            name: name.into(),
            description: String::new(),
            flag: None,
            automatable: false,
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
            before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
            kind: ParamKind::Number { min, max, step: 1.0, default, exponential: false, scale, integer: false },
        }
    }

    fn base_def(input: IoKind, output: IoKind) -> ProcessDef {
        ProcessDef {
            needs_head_tail_marks: false,
            head_tail_marks_unpaired: false,
            flags_before_infile: false,
            channel_split: None,
            spec_grab_prepass: false,
            preset_param: None,
            preset_custom_option: 0,
            script_presets: Vec::new(),
            key: "test_key".into(),
            bin: "modify".into(),
            subprog: Some("speed".into()),
            mode: Some("2".into()),
            title: "Speed".into(),
            category: Category::Time,
            subcategory: "pitch".into(),
            short_description: String::new(),
            description: String::new(),
            input,
            output,
            stereo_native: false,
            output_is_stereo: false,
            input_channels: None,
            output_channels: None,
            output_new_buffer: false,
            interactive: false,
            praat_form_locks: Vec::new(),
            praat_builtin: false,
            praat_python_rewrite: false,
            requires_simple_wav_input: false, sidecar_extension: None, min_inputs: None,
            params: vec![number_param("Speed", -96.0, 96.0, 0.0, NumberScale::Plain)],
            param_notes: Vec::new(),
        }
    }

    /// The real catalog entry, planned through the real planner: `fastconv`'s flags must come
    /// out ahead of the filenames and its dry/wet value after the outfile. A unit test on a
    /// synthetic def can't catch the entry itself losing `flags_before_infile`.
    #[test]
    fn the_real_fastconv_entry_emits_flags_before_the_filenames() {
        let (catalog, _) = crate::model::cdp::catalog::CdpCatalog::load(None);
        let def = catalog
            .processes
            .iter()
            .find(|p| p.key == "fastconv_fastconv")
            .expect("fastconv is in the built-in catalog");
        assert!(def.flags_before_infile, "the catalog entry must opt in");
        let values: Vec<ParamValue> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let input = InputSpec { channels: 1, sample_rate: 44_100, len_samples: 44_100, head_tail_marks: vec![] };
        let job = plan_job(def, &values, &[input.clone(), input], &PvocSettings::default()).expect("plans");
        assert_eq!(
            job.steps[0].args,
            vec!["-a1", "-f", "in_a.wav", "in_b.wav", "out.wav", "0"],
            "fastconv's own usage: fastconv [-aX][-f] infile impulsefile outfile [dry]"
        );
    }

    /// With its "process channels separately" toggle on, a `ChannelSplit` process runs once
    /// per channel — overriding `stereo_native` — with each lane taking its own value for the
    /// split param. Neither the toggle nor the extra value params ever reach argv.
    #[test]
    fn channel_split_runs_one_lane_per_channel_with_its_own_value() {
        let (catalog, _) = crate::model::cdp::catalog::CdpCatalog::load(None);
        let def = catalog
            .processes
            .iter()
            .find(|p| p.key == "housekeep_extract_4")
            .expect("Remove DC Offset is in the built-in catalog");
        assert!(def.stereo_native, "the premise: CDP itself handles stereo in one run");
        let input = InputSpec { channels: 2, sample_rate: 44_100, len_samples: 44_100, head_tail_marks: vec![] };

        let off = vec![ParamValue::Number(-0.004), ParamValue::Number(0.003), ParamValue::Toggle(false)];
        let job = plan_job(def, &off, std::slice::from_ref(&input), &PvocSettings::default()).unwrap();
        assert_eq!(job.steps.len(), 1, "with the toggle off it is one stereo run, as before");
        assert_eq!(
            job.steps[0].args,
            vec!["extract", "4", "in.wav", "out.wav", "-0.004"],
            "and neither ui-only param reaches argv"
        );

        let on = vec![ParamValue::Number(-0.004), ParamValue::Number(0.003), ParamValue::Toggle(true)];
        let job = plan_job(def, &on, std::slice::from_ref(&input), &PvocSettings::default()).unwrap();
        assert_eq!(job.steps.len(), 2, "with it on, one run per channel");
        assert_eq!(job.steps[0].args, vec!["extract", "4", "in_c1.wav", "out_c1.wav", "-0.004"]);
        assert_eq!(job.steps[1].args, vec!["extract", "4", "in_c2.wav", "out_c2.wav", "0.003"]);
        // Each lane reads one source channel and writes back to that same channel.
        assert_eq!(job.input_files[0].source_channels, vec![0]);
        assert_eq!(job.input_files[1].source_channels, vec![1]);
        assert_eq!(job.output_files[0].dest_channels, vec![0]);
        assert_eq!(job.output_files[1].dest_channels, vec![1]);
    }

    /// The toggle is inert on a mono file — there is nothing to separate, and splitting would
    /// mean planning a "lane" per channel for a single channel.
    #[test]
    fn channel_split_does_nothing_on_a_mono_input() {
        let (catalog, _) = crate::model::cdp::catalog::CdpCatalog::load(None);
        let def = catalog.processes.iter().find(|p| p.key == "housekeep_extract_4").unwrap();
        let input = InputSpec { channels: 1, sample_rate: 44_100, len_samples: 44_100, head_tail_marks: vec![] };
        let on = vec![ParamValue::Number(-0.004), ParamValue::Number(0.003), ParamValue::Toggle(true)];
        let job = plan_job(def, &on, &[input], &PvocSettings::default()).unwrap();
        assert_eq!(job.steps.len(), 1);
        assert_eq!(job.steps[0].args, vec!["extract", "4", "in.wav", "out.wav", "-0.004"]);
    }

    /// `scramble`'s per-segment modes take their cuts datafile from the Head/Tail marks, in
    /// the same argv slot the DISTMORE marklist uses (`scramble scramble 5 infile outfile
    /// cuts seed …`) — but every mark counts on its own, so a single one is a usable run
    /// where DISTMORE demands two complete pairs.
    #[test]
    fn scramble_per_segment_takes_its_cut_times_from_head_tail_marks() {
        let (catalog, _) = crate::model::cdp::catalog::CdpCatalog::load(None);
        let def = catalog
            .processes
            .iter()
            .find(|p| p.key == "scramble_scramble_5")
            .expect("scramble mode 5 is in the built-in catalog");
        assert!(def.needs_head_tail_marks && def.head_tail_marks_unpaired);
        assert!(
            !def.params.iter().any(|p| p.name == "Cut Times"),
            "the times come from the marks, not a form field"
        );

        let values: Vec<ParamValue> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let input = InputSpec {
            channels: 1,
            sample_rate: 10_000,
            len_samples: 40_000,
            head_tail_marks: vec![10_000, 25_000, 30_000],
        };
        let job = plan_job(def, &values, std::slice::from_ref(&input), &PvocSettings::default()).unwrap();
        let cuts_at = job.steps[0].args.iter().position(|a| a == "headstails.txt").expect("a cuts datafile");
        assert_eq!(
            job.steps[0].args[cuts_at - 1],
            "out.wav",
            "the cuts file sits directly after the outfile: {:?}",
            job.steps[0].args
        );
        let (_, contents) = job.brk_files.iter().find(|(n, _)| n == "headstails.txt").unwrap();
        let secs: Vec<f64> = contents.lines().map(|l| l.parse().unwrap()).collect();
        assert_eq!(secs, vec![1.0, 2.5, 3.0], "every mark is its own cut time, odd count and all");
    }

    /// A single mark is enough — the "two complete pairs" floor is DISTMORE's, and applying it
    /// here would reject a perfectly good one-cut run. Zero marks is the only failing case.
    #[test]
    fn scramble_per_segment_accepts_one_mark_and_rejects_none() {
        let (catalog, _) = crate::model::cdp::catalog::CdpCatalog::load(None);
        let def = catalog.processes.iter().find(|p| p.key == "scramble_scramble_5").unwrap();
        let values: Vec<ParamValue> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let with_one = InputSpec { channels: 1, sample_rate: 10_000, len_samples: 40_000, head_tail_marks: vec![10_000] };
        assert!(plan_job(def, &values, &[with_one], &PvocSettings::default()).is_ok());

        // A mark rebased to exactly 0 is dropped (CDP rejects a cut time of 0), which for a
        // lone mark leaves nothing to cut at.
        let at_zero = InputSpec { channels: 1, sample_rate: 10_000, len_samples: 40_000, head_tail_marks: vec![0] };
        assert!(matches!(
            plan_job(def, &values, &[at_zero], &PvocSettings::default()),
            Err(PlanError::MissingCutTimes { found: 0 })
        ));

        let none = InputSpec { channels: 1, sample_rate: 10_000, len_samples: 40_000, head_tail_marks: vec![] };
        assert!(matches!(
            plan_job(def, &values, &[none], &PvocSettings::default()),
            Err(PlanError::MissingCutTimes { found: 0 })
        ));
    }

    /// `focus step`'s time step is bounded at both ends by data — see
    /// `NumberScale::AnaFrameStepSeconds`. Values checked against what the real binary
    /// reports for the same settings, so a change to the decimation formula fails here
    /// rather than as an INCORRECT USE at run time.
    #[test]
    fn ana_frame_step_seconds_clamps_to_two_frames_and_the_input_duration() {
        let clamp = |raw: f64, points: u32, overlap: u32, rate: u32, duration: f64| {
            scale_number_value(
                NumberScale::AnaFrameStepSeconds,
                raw,
                duration,
                &PvocSettings { points, overlap },
                rate,
            )
        };
        // The reported case: 1.0 on a 0.85s selection, which CDP rejects outright.
        let capped = clamp(1.0, 1024, 3, 96_000, 0.85);
        assert!((capped - 0.84).abs() < 1e-9, "should cap just under the duration, got {capped}");
        // Floors measured against the real binary at four (points, overlap, rate) settings.
        for (points, overlap, rate, floor) in [
            (1024u32, 3u32, 44_100u32, 0.005805),
            (2048, 3, 44_100, 0.011610),
            (1024, 1, 48_000, 0.021333),
            (1024, 4, 48_000, 0.002667),
        ] {
            let got = clamp(0.0001, points, overlap, rate, 10.0);
            assert!(
                (got - floor).abs() < 1e-5,
                "{points}/{overlap}/{rate}: expected floor {floor}, got {got}"
            );
        }
        // A value already inside both bounds is passed through untouched.
        assert_eq!(clamp(0.5, 1024, 3, 44_100, 10.0), 0.5);
    }

    /// `fastconv` parses its flags getopt-style, *before* the filenames — with them trailing
    /// it silently ignores `-a`, `-f` and the positional dry value all at once, so every
    /// setting produced the same clipped result (user report). Bare positional params (its
    /// own `[dry]`) must still land after the outfile.
    #[test]
    fn flags_before_infile_puts_flagged_params_ahead_of_the_filenames() {
        let mut def = base_def(IoKind::DualWav, IoKind::Wav);
        def.bin = "fastconv".into();
        def.subprog = None;
        def.mode = None;
        def.flags_before_infile = true;
        def.params = vec![
            number_param("Dry/Wet Mix", 0.0, 1.0, 0.0, NumberScale::Plain),
            ParamDef { flag: Some("-a".into()), ..number_param("Amplitude Scale", 0.01, 10.0, 1.0, NumberScale::Plain) },
            ParamDef {
                flag: Some("-f".into()),
                kind: ParamKind::Toggle { default: true },
                ..number_param("Force Float Output", 0.0, 1.0, 1.0, NumberScale::Plain)
            },
        ];
        let input = InputSpec { channels: 1, sample_rate: 44_100, len_samples: 44_100, head_tail_marks: vec![] };

        let job = plan_job(
            &def,
            &[ParamValue::Number(0.5), ParamValue::Number(0.2), ParamValue::Toggle(true)],
            &[input.clone(), input],
            &PvocSettings::default(),
        )
        .expect("a dual-input wav job plans");

        let args = &job.steps[0].args;
        let infile_at = args.iter().position(|a| a.ends_with(".wav")).expect("an infile");
        assert!(
            args[..infile_at].iter().any(|a| a.starts_with("-a")) && args[..infile_at].iter().any(|a| a == "-f"),
            "both flags must precede the filenames: {args:?}"
        );
        assert_eq!(args.last().unwrap(), "0.5", "the bare positional dry value stays after the outfile: {args:?}");
    }

    /// The default shape is unchanged for every other process: flags stay after the outfile,
    /// which is what the shared CDP framework's own argument scanning expects.
    #[test]
    fn without_the_flag_flagged_params_still_follow_the_outfile() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.params = vec![ParamDef {
            flag: Some("-a".into()),
            ..number_param("Amplitude", 0.0, 10.0, 1.0, NumberScale::Plain)
        }];
        let input = InputSpec { channels: 1, sample_rate: 44_100, len_samples: 44_100, head_tail_marks: vec![] };
        let job = plan_job(&def, &[ParamValue::Number(2.0)], &[input], &PvocSettings::default()).unwrap();
        assert_eq!(job.steps[0].args, vec!["speed", "2", "in.wav", "out.wav", "-a2"]);
    }

    /// A DISTMORE-family process (`needs_head_tail_marks`) writes its marklist datafile from
    /// `InputSpec.head_tail_marks` and emits its filename as the positional argument directly
    /// after the outfile — the argv slot CDP's usage text specifies
    /// (`distmore bright 1-3 infile outfile marklist [-s… -d]`). Flagged params still follow.
    #[test]
    fn a_head_tail_marklist_is_written_and_placed_directly_after_the_outfile() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.bin = "distmore".into();
        def.subprog = Some("bright".into());
        def.mode = Some("2".into());
        def.needs_head_tail_marks = true;
        def.params = vec![ParamDef {
            flag: Some("-s".into()),
            ..number_param("Splice Length", 1.0, 100.0, 15.0, NumberScale::Plain)
        }];
        let input = InputSpec {
            channels: 1,
            sample_rate: 10_000,
            len_samples: 10_000,
            head_tail_marks: vec![1_000, 2_000, 4_000, 5_000],
        };

        let job = plan_job(&def, &[ParamValue::Number(15.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .expect("two complete pairs is enough");

        assert_eq!(
            job.steps[0].args,
            vec!["bright", "2", "in.wav", "out.wav", "headstails.txt", "-s15"],
            "the marklist sits between the outfile and the flagged params"
        );
        let (_, contents) = job
            .brk_files
            .iter()
            .find(|(name, _)| name == "headstails.txt")
            .expect("the marklist datafile is written");
        let secs: Vec<f64> = contents.lines().map(|l| l.parse().unwrap()).collect();
        assert_eq!(secs, vec![0.1, 0.2, 0.4, 0.5], "positions converted to seconds, in order");
    }

    /// Fewer than `MIN_HEAD_TAIL_PAIRS` complete pairs is rejected at plan time rather than
    /// left for CDP to fail on: the user needs to be told to place more marks, which CDP's own
    /// error can't say.
    #[test]
    fn too_few_head_tail_pairs_is_a_plan_error() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.needs_head_tail_marks = true;
        def.params = Vec::new();

        for (marks, expected_pairs) in
            [(vec![], 0), (vec![1_000], 0), (vec![1_000, 2_000], 1), (vec![1_000, 2_000, 4_000], 1)]
        {
            let input = InputSpec { channels: 1, sample_rate: 10_000, len_samples: 10_000, head_tail_marks: marks.clone() };
            let err = plan_job(&def, &[], std::slice::from_ref(&input), &PvocSettings::default())
                .expect_err("under two complete pairs must not plan");
            assert!(
                matches!(err, PlanError::MissingHeadTailMarks { pairs } if pairs == expected_pairs),
                "{marks:?} should report {expected_pairs} pairs, got {err:?}"
            );
        }
    }

    /// A trailing unpaired Head is dropped rather than written: CDP reads the list strictly two
    /// at a time, so an odd final entry would leave it reading past the end of the segment list.
    #[test]
    fn a_trailing_unpaired_head_is_truncated_out_of_the_marklist() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.needs_head_tail_marks = true;
        def.params = Vec::new();
        let input = InputSpec {
            channels: 1,
            sample_rate: 10_000,
            len_samples: 10_000,
            head_tail_marks: vec![1_000, 2_000, 4_000, 5_000, 7_000],
        };

        let job = plan_job(&def, &[], std::slice::from_ref(&input), &PvocSettings::default()).unwrap();
        let (_, contents) = job.brk_files.iter().find(|(n, _)| n == "headstails.txt").unwrap();
        assert_eq!(contents.lines().count(), 4, "only the two complete pairs are written");
    }

    /// A process that doesn't declare `needs_head_tail_marks` must ignore the field entirely,
    /// even when the document happens to carry marks — otherwise every other process in the
    /// catalog would gain a stray argv token the moment a user pressed `h`.
    #[test]
    fn a_process_that_does_not_need_marks_ignores_them_completely() {
        let def = base_def(IoKind::Wav, IoKind::Wav);
        let input = InputSpec {
            channels: 1,
            sample_rate: 10_000,
            len_samples: 10_000,
            head_tail_marks: vec![1_000, 2_000, 4_000, 5_000],
        };

        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();
        assert!(!job.steps[0].args.iter().any(|a| a.contains("headstails")));
        assert!(job.brk_files.is_empty());
    }

    #[test]
    fn mono_wav_single_lane_matches_modify_speed_2() {
        let def = base_def(IoKind::Wav, IoKind::Wav);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 1);
        assert_eq!(job.steps[0].bin, "modify");
        assert_eq!(job.steps[0].args, vec!["speed", "2", "in.wav", "out.wav", "3"]);
        assert_eq!(job.input_files, vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0], gain: None }]);
        assert_eq!(
            job.output_files,
            vec![OutputWavSpec { relative_name: "out.wav".into(), dest_channels: vec![0] }]
        );
    }

    /// `ProcessDef.sidecar_extension` (`matrix matrix 1`'s generated-matrix-data file,
    /// 2026-07-26) turns into `PlannedJob.output_sidecar` naming `"out.<ext>"` — the same
    /// fixed `"out.wav"` stem `plan_wav`'s mono/`stereo_native` branch already uses, just a
    /// different extension for the secondary file.
    #[test]
    fn sidecar_extension_becomes_a_named_output_sidecar_file() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.sidecar_extension = Some("txt".into());
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();
        assert_eq!(job.output_sidecar, Some("out.txt".to_string()));
    }

    #[test]
    fn no_sidecar_extension_means_no_output_sidecar() {
        let def = base_def(IoKind::Wav, IoKind::Wav);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();
        assert_eq!(job.output_sidecar, None);
    }

    /// `ParamDef.before_outfile` (the `pitch altharms`/`formants put` datafile-before-outfile
    /// gap) places that param's token between the infile(s) and `outfile`, while every other
    /// param stays after `outfile` in its normal declared order -- both groups keep their own
    /// relative order, mirroring how a real multi-param before/after mix would look.
    #[test]
    fn before_outfile_param_is_emitted_between_infile_and_outfile() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        let mut datafile_param = number_param("Datafile", 0.0, 1.0, 0.0, NumberScale::Plain);
        datafile_param.required_envelope = true;
        datafile_param.automatable = true;
        datafile_param.before_outfile = true;
        def.params = vec![datafile_param, number_param("After", -96.0, 96.0, 0.0, NumberScale::Plain)];

        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(
            &def,
            &[ParamValue::Breakpoints(vec![(0.0, 0.5)]), ParamValue::Number(10.0)],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();

        assert_eq!(job.steps[0].args, vec!["speed", "2", "in.wav", "brk_0.txt", "out.wav", "10"]);
    }

    #[test]
    fn stereo_wav_non_native_splits_into_dual_mono_lanes() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.stereo_native = false;
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 2);
        assert_eq!(job.steps[0].args, vec!["speed", "2", "in_c1.wav", "out_c1.wav", "3"]);
        assert_eq!(job.steps[1].args, vec!["speed", "2", "in_c2.wav", "out_c2.wav", "3"]);
        assert_eq!(job.input_files[0].source_channels, vec![0]);
        assert_eq!(job.input_files[1].source_channels, vec![1]);
        assert_eq!(job.output_files[0].dest_channels, vec![0]);
        assert_eq!(job.output_files[1].dest_channels, vec![1]);
    }

    /// Regression (user report, 2026-07-26): a sidecar-producing process (e.g. `matrix
    /// matrix 1`) run against a stereo document takes the dual-mono-lane branch, which
    /// originally didn't populate `output_sidecar` at all — "Save Matrix As" silently never
    /// appeared. Only the first lane's sidecar (`out_c1.<ext>`, matching that lane's own
    /// `out_c1.wav`) is captured, the same "one mono lane" scope limit `GlobOutputSpec`
    /// already has for this exact tension (each lane's matrix is independently generated,
    /// there's no single file representing both).
    #[test]
    fn stereo_wav_non_native_sidecar_captures_only_the_first_lane() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.stereo_native = false;
        def.sidecar_extension = Some("txt".into());
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();
        assert_eq!(job.output_sidecar, Some("out_c1.txt".to_string()));
    }

    /// `matrix_matrix_1`'s "Auto Gain Reduction" (2026-07-26) two-pass "measure-then-apply"
    /// gain calibration (`MatrixGainCalibration`'s doc comment, `plan_matrix_with_gain_calibration`)
    /// is planned only for this one process key, only when the toggle is on and
    /// `sidecar_extension` is set, and never for anything else (e.g. a process that happens
    /// to share the "Auto Gain Reduction"/"Cyclic" param names by coincidence).
    fn matrix_like_def(auto_gain_default: bool) -> ProcessDef {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.key = "matrix_matrix_1".into();
        def.subprog = Some("matrix".into());
        def.mode = Some("1".into());
        def.sidecar_extension = Some("txt".into());
        def.params = vec![
            ParamDef {
                rows_match_input_count: false,
                range_scales_with_input_duration: false,
                default_from_dc_offset: false,
                name: "Auto Gain Reduction".into(),
                description: String::new(),
                flag: None,
                automatable: false,
                required_envelope: false,
                required_list: false,
                list_is_time_sequence: false,
                before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
                kind: ParamKind::Toggle { default: auto_gain_default },
            },
            ParamDef {
                rows_match_input_count: false,
                range_scales_with_input_duration: false,
                default_from_dc_offset: false,
                name: "Cyclic".into(),
                description: String::new(),
                flag: Some("-c".into()),
                automatable: false,
                required_envelope: false,
                required_list: false,
                list_is_time_sequence: false,
                before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
                kind: ParamKind::Toggle { default: false },
            },
        ];
        def
    }

    #[test]
    fn matrix_gain_calibration_is_none_when_the_toggle_is_off() {
        let def = matrix_like_def(false);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(
            &def,
            &[ParamValue::Toggle(false), ParamValue::Toggle(false)],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();
        assert_eq!(job.matrix_gain_calibration, None);
        assert_eq!(job.steps.len(), 1, "toggle off falls through to the ordinary single-invocation plan_wav");
    }

    #[test]
    fn matrix_gain_calibration_is_none_for_any_other_process() {
        let mut def = matrix_like_def(true);
        // `matrix_matrix_2` is deliberately NOT used here (it's `matrix_matrix_1`'s own
        // real sibling with its own gain-calibration dispatch, `plan_matrix_apply_with_gain_calibration`
        // — see the tests below) -- picking a process key that gets NEITHER calibration is
        // what actually exercises "any other process".
        def.key = "matrix_matrix_3".into();
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(
            &def,
            &[ParamValue::Toggle(true), ParamValue::Toggle(false)],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();
        assert_eq!(job.matrix_gain_calibration, None);
    }

    #[test]
    fn plan_job_builds_a_two_pass_matrix_gain_calibration_job_for_mono() {
        let def = matrix_like_def(true);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(
            &def,
            &[ParamValue::Toggle(true), ParamValue::Toggle(false)],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();

        assert_eq!(job.steps.len(), 2, "one preview (mode 1) pass, one final (mode 2) pass");
        assert_eq!(job.steps[0].args, vec!["matrix", "1", "in_preview.wav", "preview_out.wav"]);
        assert_eq!(job.steps[1].args, vec!["matrix", "2", "in_final.wav", "out.wav", "preview_out.txt"]);
        assert_eq!(job.output_sidecar, Some("preview_out.txt".to_string()));
        assert_eq!(job.output_files.len(), 1);
        assert_eq!(job.output_files[0], OutputWavSpec { relative_name: "out.wav".into(), dest_channels: vec![0] });

        let cal = job.matrix_gain_calibration.expect("expected a calibration");
        assert_eq!(cal.preview_output_relative_name, "preview_out.wav");
        assert!(cal.preview_attenuation > 0.0 && cal.preview_attenuation < 1.0);
        assert!(cal.target_peak > 0.0 && cal.target_peak < 1.0);
        assert_eq!(cal.final_inputs.len(), 1);
        assert_eq!(cal.final_inputs[0].relative_name, "in_final.wav");
        assert_eq!(cal.final_inputs[0].source_channels, vec![0]);
        assert_eq!(cal.final_inputs[0].gain, Some(cal.preview_attenuation));
    }

    #[test]
    fn plan_job_builds_a_two_pass_matrix_gain_calibration_job_per_lane_for_stereo() {
        let def = matrix_like_def(true);
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(
            &def,
            &[ParamValue::Toggle(true), ParamValue::Toggle(true)], // Cyclic on this time
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();

        // One shared preview pass (one matrix, one gain for both lanes -- see
        // `plan_matrix_with_gain_calibration`'s doc comment for why), then one final pass
        // per channel, both reusing that same matrix file.
        assert_eq!(job.steps.len(), 3);
        // Cyclic (true here) is a real param on `def` itself, so the preview pass (built via
        // the normal `build_process_args`, unlike the hand-built final passes below) picks
        // it up automatically along with every other param.
        assert_eq!(job.steps[0].args, vec!["matrix", "1", "in_preview.wav", "preview_out.wav", "-c"]);
        assert_eq!(job.steps[1].args, vec!["matrix", "2", "in_final_c1.wav", "out_c1.wav", "preview_out.txt", "-c"]);
        assert_eq!(job.steps[2].args, vec!["matrix", "2", "in_final_c2.wav", "out_c2.wav", "preview_out.txt", "-c"]);
        assert_eq!(job.output_sidecar, Some("preview_out.txt".to_string()));

        assert_eq!(job.output_files.len(), 2);
        assert_eq!(job.output_files[0].dest_channels, vec![0]);
        assert_eq!(job.output_files[1].dest_channels, vec![1]);

        let cal = job.matrix_gain_calibration.expect("expected a calibration");
        assert_eq!(cal.final_inputs.len(), 2);
        assert_eq!(cal.final_inputs[0].source_channels, vec![0]);
        assert_eq!(cal.final_inputs[1].source_channels, vec![1]);
    }

    /// `matrix_matrix_2`'s own "Auto Gain Reduction" (`plan_matrix_apply_with_gain_calibration`,
    /// 2026-07-26 — user report: applying a saved matrix to a different file clips too).
    fn matrix_apply_like_def(auto_gain_default: bool) -> ProcessDef {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.key = "matrix_matrix_2".into();
        def.subprog = Some("matrix".into());
        def.mode = Some("2".into());
        def.params = vec![
            ParamDef {
                rows_match_input_count: false,
                range_scales_with_input_duration: false,
                default_from_dc_offset: false,
                name: "Matrix File".into(),
                description: String::new(),
                flag: None,
                automatable: false,
                required_envelope: false,
                required_list: false,
                list_is_time_sequence: false,
                before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
                kind: ParamKind::FilePath { extension: "matrix".into() },
            },
            ParamDef {
                rows_match_input_count: false,
                range_scales_with_input_duration: false,
                default_from_dc_offset: false,
                name: "Auto Gain Reduction".into(),
                description: String::new(),
                flag: None,
                automatable: false,
                required_envelope: false,
                required_list: false,
                list_is_time_sequence: false,
                before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
                kind: ParamKind::Toggle { default: auto_gain_default },
            },
            ParamDef {
                rows_match_input_count: false,
                range_scales_with_input_duration: false,
                default_from_dc_offset: false,
                name: "Cyclic".into(),
                description: String::new(),
                flag: Some("-c".into()),
                automatable: false,
                required_envelope: false,
                required_list: false,
                list_is_time_sequence: false,
                before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
                kind: ParamKind::Toggle { default: false },
            },
        ];
        def
    }

    #[test]
    fn matrix_apply_gain_calibration_is_none_when_the_toggle_is_off() {
        let def = matrix_apply_like_def(false);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let values = [
            ParamValue::FilePath("/tmp/some.matrix".into()),
            ParamValue::Toggle(false),
            ParamValue::Toggle(false),
        ];
        let job = plan_job(&def, &values, std::slice::from_ref(&input), &PvocSettings::default()).unwrap();
        assert_eq!(job.matrix_gain_calibration, None);
        assert_eq!(job.steps.len(), 1, "toggle off falls through to the ordinary single-invocation plan_wav");
    }

    #[test]
    fn plan_job_builds_a_two_pass_matrix_apply_gain_calibration_job_for_mono() {
        let def = matrix_apply_like_def(true);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let values = [
            ParamValue::FilePath("/tmp/some.matrix".into()),
            ParamValue::Toggle(true),
            ParamValue::Toggle(false),
        ];
        let job = plan_job(&def, &values, std::slice::from_ref(&input), &PvocSettings::default()).unwrap();

        assert_eq!(job.steps.len(), 2, "one preview pass, one final pass, both mode 2");
        assert_eq!(job.steps[0].args, vec!["matrix", "2", "in_preview.wav", "preview_out.wav", "/tmp/some.matrix"]);
        assert_eq!(job.steps[1].args, vec!["matrix", "2", "in_final.wav", "out.wav", "/tmp/some.matrix"]);
        assert_eq!(job.output_sidecar, None, "mode 2 doesn't generate a new matrix file");

        let cal = job.matrix_gain_calibration.expect("expected a calibration");
        assert_eq!(cal.preview_output_relative_name, "preview_out.wav");
        assert_eq!(cal.final_inputs.len(), 1);
        assert_eq!(cal.final_inputs[0].relative_name, "in_final.wav");
        assert_eq!(cal.final_inputs[0].gain, Some(cal.preview_attenuation));
    }

    #[test]
    fn plan_job_builds_a_two_pass_matrix_apply_gain_calibration_job_per_lane_for_stereo() {
        let def = matrix_apply_like_def(true);
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let values = [
            ParamValue::FilePath("/tmp/some.matrix".into()),
            ParamValue::Toggle(true),
            ParamValue::Toggle(true), // Cyclic on
        ];
        let job = plan_job(&def, &values, std::slice::from_ref(&input), &PvocSettings::default()).unwrap();

        assert_eq!(job.steps.len(), 3);
        assert_eq!(job.steps[0].args, vec!["matrix", "2", "in_preview.wav", "preview_out.wav", "/tmp/some.matrix", "-c"]);
        assert_eq!(job.steps[1].args, vec!["matrix", "2", "in_final_c1.wav", "out_c1.wav", "/tmp/some.matrix", "-c"]);
        assert_eq!(job.steps[2].args, vec!["matrix", "2", "in_final_c2.wav", "out_c2.wav", "/tmp/some.matrix", "-c"]);

        let cal = job.matrix_gain_calibration.expect("expected a calibration");
        assert_eq!(cal.final_inputs.len(), 2);
        assert_eq!(cal.final_inputs[0].source_channels, vec![0]);
        assert_eq!(cal.final_inputs[1].source_channels, vec![1]);
    }

    #[test]
    fn stereo_native_process_keeps_single_lane() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.stereo_native = true;
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 1);
        assert_eq!(job.input_files, vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0, 1], gain: None }]);
        assert_eq!(
            job.output_files,
            vec![OutputWavSpec { relative_name: "out.wav".into(), dest_channels: vec![0, 1] }]
        );
    }

    /// A `Mono` process takes channel 0 alone from a wide document and runs **once**, rather
    /// than lane-splitting. This is the case `stereo_native` could express neither way: `false`
    /// would run a mono→8-channel spatialiser 30 times over a 30-channel take and then read
    /// channel 0 of each result, and `true` would hand the binary all 30 channels, which it
    /// refuses outright ("must be mono").
    #[test]
    fn a_mono_input_process_takes_channel_zero_and_runs_once() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.input_channels = Some(crate::model::cdp::def::InputChannels::Mono);
        let input =
            InputSpec { channels: 30, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job =
            plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
                .unwrap();

        assert_eq!(job.steps.len(), 1, "one run, not one per channel");
        assert_eq!(job.input_files[0].source_channels, vec![0]);
    }

    /// The output width comes from the parameter the process declares, so a spatialiser's
    /// `dest_channels` covers every channel the real file holds. Under-counting here is silent:
    /// `read_outputs` reads exactly as many channels as there are entries and drops the rest.
    #[test]
    fn output_channel_count_comes_from_the_declared_param() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.input_channels = Some(crate::model::cdp::def::InputChannels::Mono);
        def.output_channels = Some(crate::model::cdp::def::OutputChannels::FromParam { param: 0 });
        def.params = vec![number_param("Output Channels", 2.0, 16.0, 8.0, NumberScale::Plain)];
        let input =
            InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        for count in [2.0, 8.0, 16.0] {
            let job = plan_job(
                &def,
                &[ParamValue::Number(count)],
                std::slice::from_ref(&input),
                &PvocSettings::default(),
            )
            .unwrap();
            assert_eq!(
                job.output_files[0].dest_channels,
                (0..count as usize).collect::<Vec<_>>(),
                "a run asking for {count} channels must read back {count}"
            );
        }
    }

    /// A `Multichannel` process preserves the width it was given: it declares no
    /// `output_channels`, so `dest_channels` falls back to the source channels. `mchshred`
    /// mode 2 is the real entry with this shape.
    #[test]
    fn a_multichannel_process_reads_back_every_channel_it_was_given() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.input_channels = Some(crate::model::cdp::def::InputChannels::Multichannel);
        let input =
            InputSpec { channels: 6, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job =
            plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
                .unwrap();

        assert_eq!(job.input_files[0].source_channels, (0..6).collect::<Vec<_>>());
        assert_eq!(job.output_files[0].dest_channels, (0..6).collect::<Vec<_>>());
    }

    /// A selection too narrow for what the binary demands is refused at plan time with a
    /// sentence, not passed through to CDP to fail as an exit code. `App::cdp_params_blocker`
    /// shows the same string the moment the dialog opens.
    #[test]
    fn too_few_channels_is_refused_before_a_binary_is_spawned() {
        for (declared, channels, expect) in [
            (crate::model::cdp::def::InputChannels::Stereo, 1, "stereo source"),
            (crate::model::cdp::def::InputChannels::Multichannel, 2, "more than 2 channels"),
        ] {
            let mut def = base_def(IoKind::Wav, IoKind::Wav);
            def.input_channels = Some(declared);
            let input = InputSpec {
                channels,
                sample_rate: 44100,
                len_samples: 44100,
                ..Default::default()
            };
            let err = plan_job(
                &def,
                &[ParamValue::Number(3.0)],
                std::slice::from_ref(&input),
                &PvocSettings::default(),
            )
            .expect_err("must refuse");
            let PlanError::InputChannelCount { reason } = err else {
                panic!("wrong error for {declared:?}: {err:?}");
            };
            assert!(reason.contains(expect), "{declared:?} said {reason:?}");
        }
    }

    /// The real catalog entries, planned through the real planner. A unit test on a synthetic
    /// def can't catch an entry losing `output_new_buffer` (splicing an 8-channel result over
    /// the source document) or pointing `output_channels` at the wrong param index — which
    /// would read back the wrong number of channels with no error at all.
    ///
    /// **CDP entries only.** This was originally every entry declaring `input_channels`,
    /// because when the field existed solely for CDP's MULTICHANNEL family, declaring a width
    /// and changing the channel count were the same thing — every member either narrows
    /// (`pairex`, 8 in and 2 out) or fans out, so `output_new_buffer` was rightly demanded of
    /// all of them. Airwindows separated the two: it declares
    /// [`InputChannels::MonoOrStereo`](crate::model::cdp::def::InputChannels::MonoOrStereo) to
    /// express a *constraint* — refuse anything past two channels — and for a stereo
    /// selection, which is the ordinary case, the count does not change at all. The one case
    /// that does change it widens 1 to 2, which is the direction `CdpProcessCommand` already
    /// supports in place and reverses on undo via `channels_before`; the hazard
    /// `output_new_buffer` exists to prevent is the *narrowing* splice, where `insert_range`
    /// would fill the channels the data doesn't cover from channel 0 and smear it across the
    /// rest. So the rule is narrowed here to the backend it was written about, rather than
    /// forcing every saturator to spawn a buffer it has no reason to.
    #[test]
    fn the_real_multichannel_entries_declare_a_coherent_channel_shape() {
        use crate::model::cdp::def::{Backend, InputChannels, OutputChannels};
        let (catalog, _) = crate::model::cdp::catalog::CdpCatalog::load(None);
        let mut seen = 0;
        for def in catalog
            .processes
            .iter()
            .filter(|p| p.input_channels.is_some() && p.backend() == Backend::Cdp)
        {
            seen += 1;
            assert!(
                def.output_new_buffer,
                "{}: a channel-count-changing process must open its result as a new buffer",
                def.key
            );
            // A `FromParam` index pointing at something that isn't a count resolves to `None`
            // and silently falls back to the input's width — the exact silent-drop this field
            // exists to prevent.
            if let Some(OutputChannels::FromParam { param }) = def.output_channels {
                let values: Vec<ParamValue> =
                    def.params.iter().map(|p| p.kind.default_value()).collect();
                let count = def
                    .output_channel_count(&values)
                    .unwrap_or_else(|| panic!("{}: param {param} doesn't resolve to a count", def.key));
                assert!(count >= 2, "{}: resolved to {count} channels", def.key);
            }
            // Only a mono input can be narrowed to one channel of a wider document; a stereo-
            // or multichannel-input process planned against a document too narrow is refused,
            // which is what `cdp_params_blocker` reports.
            let width = match def.input_channels {
                Some(InputChannels::Mono) => 1,
                Some(InputChannels::Stereo) => 2,
                _ => 6,
            };
            assert!(
                def.input_source_channels(width).is_some_and(|r| r.is_ok()),
                "{}: rejects the width it declares",
                def.key
            );
        }
        assert!(seen >= 11, "expected the multichannel batch to be present, found {seen}");
    }

    /// The Airwindows half of the rule above, stated positively rather than left to the
    /// carve-out. Every entry must declare the mono-or-stereo constraint, must accept both
    /// widths it names, and must refuse anything wider — that refusal is the only thing
    /// standing between a 56-channel take and a plugin that indexes `inputs[0]`/`inputs[1]`
    /// literally and would silently process two channels of it as though that were the file.
    #[test]
    fn every_airwindows_entry_takes_mono_or_stereo_and_refuses_wider() {
        use crate::model::cdp::def::{Backend, InputChannels};
        let (catalog, _) = crate::model::cdp::catalog::CdpCatalog::load(None);
        let mut seen = 0;
        for def in catalog.processes.iter().filter(|p| p.backend() == Backend::Airwindows) {
            seen += 1;
            assert_eq!(
                def.input_channels,
                Some(InputChannels::MonoOrStereo),
                "{}: every Airwindows plugin is hard-wired to two legs",
                def.key
            );
            for width in [1, 2] {
                assert!(
                    def.input_source_channels(width).is_some_and(|r| r.is_ok()),
                    "{}: rejects {width}-channel input",
                    def.key
                );
            }
            for width in [3, 6, 56] {
                assert!(
                    def.input_source_channels(width).is_some_and(|r| r.is_err()),
                    "{}: accepts {width}-channel input",
                    def.key
                );
            }
            // Two channels out, always — a mono selection is duplicated into both legs and the
            // stereo result kept, which is what widens the buffer.
            assert!(def.output_is_stereo, "{}: must declare stereo output", def.key);
        }
        assert!(seen > 400, "expected the Airwindows catalog to be present, found {seen}");
    }

    #[test]
    fn ana_input_wraps_with_pvoc_anal_and_synth() {
        let mut def = base_def(IoKind::Ana, IoKind::Ana);
        def.bin = "blur".into();
        def.subprog = Some("avrg".into());
        def.mode = None;
        def.params = vec![number_param("Channels", 1.0, 200.0, 6.0, NumberScale::Plain)];

        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 88200, ..Default::default() };
        let job = plan_job(&def, &[ParamValue::Number(6.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 3);
        assert_eq!(job.steps[0].bin, "pvoc");
        assert_eq!(job.steps[0].args, vec!["anal", "1", "in_c1.wav", "a1.ana", "-c1024", "-o3"]);
        assert_eq!(job.steps[1].bin, "blur");
        assert_eq!(job.steps[1].args, vec!["avrg", "a1.ana", "b1.ana", "6"]);
        assert_eq!(job.steps[2].bin, "pvoc");
        assert_eq!(job.steps[2].args, vec!["synth", "b1.ana", "out_c1.wav"]);
        assert_eq!(job.input_files.len(), 1);
        assert_eq!(job.output_files.len(), 1);
    }

    #[test]
    fn ana_input_stereo_produces_two_full_lanes() {
        let mut def = base_def(IoKind::Ana, IoKind::Ana);
        def.bin = "blur".into();
        def.subprog = Some("avrg".into());
        def.mode = None;
        def.params = vec![number_param("Channels", 1.0, 200.0, 6.0, NumberScale::Plain)];

        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 88200, ..Default::default() };
        let job = plan_job(&def, &[ParamValue::Number(6.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 6);
        assert_eq!(job.input_files.len(), 2);
        assert_eq!(job.output_files.len(), 2);
        assert_eq!(job.output_files[0].dest_channels, vec![0]);
        assert_eq!(job.output_files[1].dest_channels, vec![1]);
    }

    /// A mono 2-step spectral run must produce exactly ONE `pvoc anal` and ONE `pvoc synth`
    /// (not one pair per step) — 4 invocations total (anal, step1, step2, synth) — with each
    /// process reading the *previous* one's `.ana` output directly, never resynthesizing to
    /// audio in between. This is the whole point of `plan_ana_chain` over calling `plan_ana`
    /// twice.
    #[test]
    fn two_step_mono_run_shares_one_anal_and_one_synth() {
        let mut avrg = base_def(IoKind::Ana, IoKind::Ana);
        avrg.bin = "blur".into();
        avrg.subprog = Some("avrg".into());
        avrg.mode = None;
        avrg.params = vec![number_param("Channels", 1.0, 200.0, 6.0, NumberScale::Plain)];

        let mut freeze = base_def(IoKind::Ana, IoKind::Ana);
        freeze.bin = "focus".into();
        freeze.subprog = Some("freeze".into());
        freeze.mode = Some("1".into());
        freeze.params = vec![];

        let avrg_values = vec![ParamValue::Number(6.0)];
        let freeze_values: Vec<ParamValue> = vec![];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 88200, ..Default::default() };
        let job = plan_ana_chain(&[(&avrg, &avrg_values), (&freeze, &freeze_values)], &input, &PvocSettings::default())
            .unwrap();

        let anal_count = job.steps.iter().filter(|s| s.bin == "pvoc" && s.args.first().map(String::as_str) == Some("anal")).count();
        let synth_count = job.steps.iter().filter(|s| s.bin == "pvoc" && s.args.first().map(String::as_str) == Some("synth")).count();
        assert_eq!(anal_count, 1, "one process's own anal must not fire for a merged run");
        assert_eq!(synth_count, 1, "one process's own synth must not fire for a merged run");
        assert_eq!(job.steps.len(), 4, "anal, blur avrg, focus freeze, synth -- no intermediate resynthesis");

        assert_eq!(job.steps[0].args, vec!["anal", "1", "in_c1.wav", "chain_c1_s0.ana", "-c1024", "-o3"]);
        assert_eq!(job.steps[1].bin, "blur");
        assert_eq!(job.steps[1].args, vec!["avrg", "chain_c1_s0.ana", "chain_c1_s1.ana", "6"]);
        assert_eq!(job.steps[2].bin, "focus");
        assert_eq!(job.steps[2].args, vec!["freeze", "1", "chain_c1_s1.ana", "chain_c1_s2.ana"]);
        assert_eq!(job.steps[3].args, vec!["synth", "chain_c1_s2.ana", "out_c1.wav"]);
        assert_eq!(job.input_files.len(), 1);
        assert_eq!(job.output_files.len(), 1);
    }

    /// A stereo run must still only anal/synth once *per channel* (not per step): 2 channels
    /// x 2 steps = one anal + 2 processes + one synth per channel = 8 invocations total, not
    /// 2 channels x 2 steps x 3 (anal/process/synth each) = 12.
    #[test]
    fn stereo_run_anals_and_synths_once_per_channel_not_per_step() {
        let mut avrg = base_def(IoKind::Ana, IoKind::Ana);
        avrg.bin = "blur".into();
        avrg.subprog = Some("avrg".into());
        avrg.mode = None;
        avrg.params = vec![number_param("Channels", 1.0, 200.0, 6.0, NumberScale::Plain)];
        let values = vec![ParamValue::Number(6.0)];

        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 88200, ..Default::default() };
        let job = plan_ana_chain(&[(&avrg, &values), (&avrg, &values)], &input, &PvocSettings::default()).unwrap();

        assert_eq!(job.steps.len(), 8, "2 channels x (1 anal + 2 process steps + 1 synth)");
        let anal_count = job.steps.iter().filter(|s| s.bin == "pvoc" && s.args.first().map(String::as_str) == Some("anal")).count();
        let synth_count = job.steps.iter().filter(|s| s.bin == "pvoc" && s.args.first().map(String::as_str) == Some("synth")).count();
        assert_eq!(anal_count, 2, "one anal per channel, not per step");
        assert_eq!(synth_count, 2, "one synth per channel, not per step");
        assert_eq!(job.input_files.len(), 2);
        assert_eq!(job.output_files.len(), 2);
    }

    /// Two different processes in the same run each having their own `Breakpoints` param at
    /// local index 0 must not collide on the same `brk_0.txt` filename -- they share one job
    /// (one temp directory), unlike every other planning function's one-process-per-job
    /// convention, so without `brk_index_base` the second process's file would silently
    /// clobber the first's before either ever runs.
    #[test]
    fn distinct_processes_own_breakpoint_files_do_not_collide() {
        let mut first = base_def(IoKind::Ana, IoKind::Ana);
        first.bin = "blur".into();
        first.subprog = Some("avrg".into());
        first.mode = None;
        first.params = vec![number_param("Channels", 1.0, 200.0, 6.0, NumberScale::Plain)];
        let first_values = vec![ParamValue::Breakpoints(vec![(0.0, 4.0), (1.0, 8.0)])];

        let mut second = base_def(IoKind::Ana, IoKind::Ana);
        second.bin = "focus".into();
        second.subprog = Some("freeze".into());
        second.mode = Some("1".into());
        second.params = vec![number_param("Depth", 0.0, 1.0, 0.5, NumberScale::Plain)];
        let second_values = vec![ParamValue::Breakpoints(vec![(0.0, 0.2), (1.0, 0.9)])];

        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_ana_chain(&[(&first, &first_values), (&second, &second_values)], &input, &PvocSettings::default())
            .unwrap();

        let brk_names: Vec<&str> = job.brk_files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(brk_names.len(), 2, "each process's own .brk file must survive, not overwrite the other's");
        assert_ne!(brk_names[0], brk_names[1], "the two processes' own param-0 .brk files must not share a name");
        // Each invocation's own arg must reference its own distinct file, not the other's.
        assert!(job.steps[1].args.contains(&brk_names[0].to_string()));
        assert!(job.steps[2].args.contains(&brk_names[1].to_string()));
    }

    /// A `PercentOfAnaWindowCount` param on a *non-first* step in the run must be tracked
    /// against the `.ana` file *that step itself reads* (the previous step's output), not
    /// the run's very first anal -- the runner patches window-count placeholders by parsing
    /// `decfactor` out of whichever file `ana_relative_name` names, so getting this wrong
    /// would have it read the wrong (differently-windowed) file.
    #[test]
    fn deferred_window_param_on_a_later_step_points_at_its_own_input_ana_file() {
        let mut first = base_def(IoKind::Ana, IoKind::Ana);
        first.bin = "blur".into();
        first.subprog = Some("avrg".into());
        first.mode = None;
        first.params = vec![number_param("Channels", 1.0, 200.0, 6.0, NumberScale::Plain)];
        let first_values = vec![ParamValue::Number(6.0)];

        let mut second = base_def(IoKind::Ana, IoKind::Ana);
        second.bin = "some_bin".into();
        second.subprog = None;
        second.mode = None;
        second.params = vec![number_param("Window", 0.0, 100.0, 50.0, NumberScale::PercentOfAnaWindowCount)];
        let second_values = vec![ParamValue::Number(50.0)];

        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_ana_chain(&[(&first, &first_values), (&second, &second_values)], &input, &PvocSettings::default())
            .unwrap();

        assert_eq!(job.deferred_window_params.len(), 1);
        assert_eq!(
            job.deferred_window_params[0].ana_relative_name, "chain_c1_s1.ana",
            "must reference the second step's own input (the first step's output), not the initial anal's output"
        );
    }

    #[test]
    fn flagged_toggle_and_choice_params_format_correctly() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.params = vec![
            ParamDef {
                rows_match_input_count: false,
                range_scales_with_input_duration: false,
                default_from_dc_offset: false,
                name: "Omit".into(),
                description: String::new(),
                flag: Some("-x".into()),
                automatable: false,
                required_envelope: false,
                required_list: false,
                list_is_time_sequence: false,
            before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
                kind: ParamKind::Toggle { default: false },
            },
            ParamDef {
                rows_match_input_count: false,
                range_scales_with_input_duration: false,
                default_from_dc_offset: false,
                name: "Rate".into(),
                description: String::new(),
                flag: None,
                automatable: false,
                required_envelope: false,
                required_list: false,
                list_is_time_sequence: false,
            before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
                kind: ParamKind::Choice { options: vec!["44100".into(), "48000".into()], default: 0 },
            },
        ];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        let job_off = plan_job(
            &def,
            &[ParamValue::Toggle(false), ParamValue::Choice(1)],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();
        assert_eq!(job_off.steps[0].args, vec!["speed", "2", "in.wav", "out.wav", "48000"]);

        let job_on = plan_job(
            &def,
            &[ParamValue::Toggle(true), ParamValue::Choice(0)],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();
        assert_eq!(job_on.steps[0].args, vec!["speed", "2", "in.wav", "out.wav", "-x", "44100"]);
    }

    #[test]
    fn percent_of_input_duration_converts_to_seconds_with_100_percent_clamp() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.params = vec![number_param("At", 0.0, 100.0, 50.0, NumberScale::PercentOfInputDuration)];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 * 2, ..Default::default() }; // 2s

        let half = plan_job(&def, &[ParamValue::Number(50.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();
        assert_eq!(half.steps[0].args.last().unwrap(), "1");

        let full = plan_job(&def, &[ParamValue::Number(100.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();
        assert_eq!(full.steps[0].args.last().unwrap(), "1.9"); // duration(2) - 0.1
    }

    #[test]
    fn percent_of_fft_size_scales_against_pvoc_points() {
        let mut def = base_def(IoKind::Ana, IoKind::Ana);
        def.subprog = Some("suppress".into());
        def.mode = None;
        def.params = vec![number_param("Amount", 0.0, 100.0, 50.0, NumberScale::PercentOfFftSize)];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        let job = plan_job(
            &def,
            &[ParamValue::Number(50.0)],
            std::slice::from_ref(&input),
            &PvocSettings { points: 2048, overlap: 3 },
        )
        .unwrap();
        // args: [subprog, infile, outfile, param] -- the process step is steps[1]
        assert_eq!(job.steps[1].args.last().unwrap(), "1024");
    }

    /// `distort replim`'s `-f` HILIM: the accepted ceiling is the input's own Nyquist
    /// frequency, so the same catalog `max` has to resolve differently per sample rate. Only
    /// the ceiling moves — a value already under Nyquist is passed through untouched, and the
    /// catalog's own `min` (a fixed 440Hz floor CDP enforces at every rate) is not this
    /// scale's business. See `NumberScale::HzCappedToNyquist` (def.rs).
    #[test]
    fn hz_capped_to_nyquist_clamps_down_per_sample_rate() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.subprog = Some("replim".into());
        def.mode = None;
        def.params =
            vec![number_param("High Limit", 440.0, 22050.0, 2000.0, NumberScale::HzCappedToNyquist)];

        let at = |sample_rate: u32, raw: f64| {
            let input =
                InputSpec { channels: 1, sample_rate, len_samples: sample_rate as usize, ..Default::default() };
            plan_job(&def, &[ParamValue::Number(raw)], std::slice::from_ref(&input), &PvocSettings::default())
                .unwrap()
                .steps[0]
                .args
                .last()
                .unwrap()
                .clone()
        };

        // Under Nyquist at both rates -- passed through unchanged.
        assert_eq!(at(44100, 2000.0), "2000");
        assert_eq!(at(22050, 2000.0), "2000");
        // Over Nyquist at 22.05k but not at 44.1k -- clamped only where CDP would reject it.
        assert_eq!(at(44100, 22050.0), "22050");
        assert_eq!(at(22050, 22050.0), "11025");
        // A higher-rate file is *not* raised past what the catalog entry declares.
        assert_eq!(at(96000, 22050.0), "22050");
    }

    /// `distort pulsed` mode 3's START TIME is a sample count (modes 1-2 take the same
    /// argument in seconds), capped at the input's own length. See
    /// `NumberScale::CappedAtInputSamples` (def.rs).
    #[test]
    fn capped_at_input_samples_clamps_to_the_inputs_own_length() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.subprog = Some("pulsed".into());
        def.mode = Some("3".into());
        def.params =
            vec![number_param("Start Time", 0.0, 10_000_000.0, 0.0, NumberScale::CappedAtInputSamples)];

        let at = |len_samples: usize, raw: f64| {
            let input = InputSpec { channels: 1, sample_rate: 44100, len_samples, ..Default::default() };
            plan_job(&def, &[ParamValue::Number(raw)], std::slice::from_ref(&input), &PvocSettings::default())
                .unwrap()
                .steps[0]
                .args
                .last()
                .unwrap()
                .clone()
        };

        // Inside the file -- untouched.
        assert_eq!(at(132_300, 1000.0), "1000");
        // Past the end -- pulled back to the last usable sample, matching the real binary's
        // own ceiling (132299 accepted, 200000 rejected, on this exact 3s/44.1k length).
        assert_eq!(at(132_300, 200_000.0), "132299");
        // A longer file leaves the same value alone.
        assert_eq!(at(441_000, 200_000.0), "200000");
    }

    #[test]
    fn percent_of_ana_window_count_is_deferred_not_precomputed() {
        let mut def = base_def(IoKind::Ana, IoKind::Ana);
        def.bin = "blur".into();
        def.subprog = Some("blur".into());
        def.mode = None;
        def.params = vec![number_param("Blurring", 0.1, 100.0, 20.0, NumberScale::PercentOfAnaWindowCount)];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        let job = plan_job(&def, &[ParamValue::Number(20.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.deferred_window_params.len(), 1, "expected exactly one deferred window param");
        let deferred = &job.deferred_window_params[0];
        assert_eq!(deferred.ana_relative_name, "a1.ana");
        let DeferredWindowTarget::Arg { arg_index, flag, percent } = &deferred.target else {
            panic!("expected an Arg target for a constant Number value")
        };
        assert_eq!(*percent, 20.0);
        assert_eq!(*flag, None);
        assert_eq!(job.steps[deferred.step_index].args[*arg_index], "0");
    }

    /// Regression test for the bug behind "blur gives an error" on a stereo file: with two
    /// channel lanes, both must get their own resolved deferred param — not just the last
    /// lane, which a single-`Option` field silently produced (leaving lane 1's argv stuck
    /// on the unresolved "0" placeholder, which CDP rejects as out of range).
    #[test]
    fn percent_of_ana_window_count_produces_one_deferred_entry_per_stereo_lane() {
        let mut def = base_def(IoKind::Ana, IoKind::Ana);
        def.bin = "blur".into();
        def.subprog = Some("blur".into());
        def.mode = None;
        def.params = vec![number_param("Blurring", 0.1, 100.0, 20.0, NumberScale::PercentOfAnaWindowCount)];
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        let job = plan_job(&def, &[ParamValue::Number(20.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.deferred_window_params.len(), 2, "expected one deferred entry per channel lane");
        let names: Vec<&str> = job.deferred_window_params.iter().map(|d| d.ana_relative_name.as_str()).collect();
        assert_eq!(names, vec!["a1.ana", "a2.ana"]);
        // Both lanes' argv still carry the unresolved placeholder at plan time — the runner
        // patches each independently right before spawning that lane's process step.
        for deferred in &job.deferred_window_params {
            let DeferredWindowTarget::Arg { arg_index, .. } = &deferred.target else {
                panic!("expected an Arg target for a constant Number value")
            };
            assert_eq!(job.steps[deferred.step_index].args[*arg_index], "0");
        }
    }

    /// Regression test for the actual reported bug: an *automated* (envelope) value on
    /// `blur_blur`'s "Blurring" param used to write its raw 0-100 percent values straight
    /// into the `.brk` file — CDP then rejected them as literal (and far too small) window
    /// counts, e.g. "Value (0.100000) out of range (1.0 to 1632.0)". A `Breakpoints` value on
    /// this scale must defer too, targeting the `.brk` file rather than an argv token.
    #[test]
    fn percent_of_ana_window_count_breakpoints_defer_to_a_brk_file() {
        let mut def = base_def(IoKind::Ana, IoKind::Ana);
        def.bin = "blur".into();
        def.subprog = Some("blur".into());
        def.mode = None;
        def.params = vec![number_param("Blurring", 0.1, 100.0, 20.0, NumberScale::PercentOfAnaWindowCount)];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let points = vec![(0.0, 0.1), (1.0, 50.0)];

        let job = plan_job(
            &def,
            &[ParamValue::Breakpoints(points.clone())],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();

        assert_eq!(job.deferred_window_params.len(), 1);
        let deferred = &job.deferred_window_params[0];
        assert_eq!(deferred.ana_relative_name, "a1.ana");
        let DeferredWindowTarget::BrkFile { relative_name, points: deferred_points } = &deferred.target else {
            panic!("expected a BrkFile target for an automated (Breakpoints) value")
        };
        assert_eq!(deferred_points, &points, "raw percent points must be preserved for the runner to rescale");

        // The .brk file emitted at plan time is a placeholder — the runner rewrites it once
        // the real window count is known, so it must NOT hold the raw (out-of-range) percents.
        let (name, contents) = job.brk_files.iter().find(|(n, _)| n == relative_name).unwrap();
        assert_eq!(name, relative_name);
        assert!(!contents.contains("0.1") && !contents.contains("50"), "plan-time file must be a placeholder, not the real percents: {contents:?}");
    }

    #[test]
    fn breakpoints_emit_brk_file_and_reference_its_path() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.params = vec![ParamDef {
            rows_match_input_count: false,
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            name: "Gain".into(),
            description: String::new(),
            flag: Some("-f".into()),
            automatable: true,
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
            before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
            kind: ParamKind::Number {
                min: 0.0,
                max: 2.0,
                step: 0.01,
                default: 1.0,
                exponential: false,
                scale: NumberScale::Plain,
                integer: false,
            },
        }];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        let job = plan_job(
            &def,
            &[ParamValue::Breakpoints(vec![(0.0, 0.5), (1.0, 1.5)])],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();

        assert_eq!(job.brk_files.len(), 1);
        assert_eq!(job.brk_files[0].0, "brk_0.txt");
        assert_eq!(job.brk_files[0].1, "0 0.5\n1 1.5");
        assert_eq!(job.steps[0].args.last().unwrap(), "-fbrk_0.txt");
    }

    #[test]
    fn synthesis_process_needs_no_input_files() {
        let mut def = base_def(IoKind::None, IoKind::Wav);
        def.bin = "synth".into();
        def.subprog = Some("noise".into());
        def.mode = None;
        def.output_is_stereo = false;
        def.params = vec![];

        let job = plan_job(&def, &[], &[], &PvocSettings::default()).unwrap();
        assert!(job.input_files.is_empty());
        assert_eq!(job.steps[0].args, vec!["noise", "out.wav"]);
        assert_eq!(job.output_files[0].dest_channels, vec![0]);
    }

    /// A glob-output process (`IoKind::WavGlob`, e.g. distcut/envcut) plans a single mono
    /// lane with the shared prefix as its "outfile" argv token, `output_files` left empty
    /// (there's no single known result file), and `glob_output` populated instead —
    /// `expected_output` checks for `<prefix>0.wav` specifically, matching CDP's own
    /// 0-based numbering for this family of outputs.
    #[test]
    fn glob_output_process_uses_a_shared_prefix_and_no_output_files() {
        let mut def = base_def(IoKind::Wav, IoKind::WavGlob);
        def.bin = "distcut".into();
        def.subprog = Some("distcut".into());
        def.mode = Some("1".into());
        def.params = vec![
            number_param("Cycle Count", 1.0, 200.0, 10.0, NumberScale::Plain),
            number_param("Decay Shape", 0.1, 10.0, 1.0, NumberScale::Plain),
        ];
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        let job = plan_job(
            &def,
            &[ParamValue::Number(10.0), ParamValue::Number(1.0)],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();

        assert_eq!(job.steps.len(), 1);
        assert_eq!(job.steps[0].args, vec!["distcut", "1", "in.wav", "cutout", "10", "1"]);
        assert_eq!(job.steps[0].expected_output, "cutout0.wav");
        assert!(job.output_files.is_empty(), "glob-output jobs have no single known result file");
        let glob = job.glob_output.expect("expected a GlobOutputSpec");
        assert_eq!(glob.prefix, "cutout");
        // Always exactly one mono lane, using only the first channel — even though the
        // InputSpec above says the document is stereo (see GlobOutputSpec's doc comment for
        // why merging independently-numbered file sets across stereo lanes isn't supported).
        assert_eq!(job.input_files.len(), 1);
        assert_eq!(job.input_files[0].source_channels, vec![0]);
    }

    /// An `Ana`-input glob-output process gets a `pvoc anal` pre-pass first, reads that
    /// `.ana` file (not a plain `.wav`), and has no resynthesis step after — its own
    /// numbered outputs are taken to already be the final result. Found missing (this
    /// function's original form only ever wrote `in.wav` unconditionally) while cataloging
    /// `speculate` against the real binary (2026-07-26).
    ///
    /// `speculate` itself was removed from the catalog on 2026-07-27 (its numbered outputs
    /// are pvoc analysis files, not audio — see `plan_wav_glob`'s doc comment), so this
    /// test now defines the contract for a hypothetical future entry of that shape rather
    /// than mirroring a live one. Kept deliberately: deleting it would leave the branch
    /// untested and quietly rot.
    #[test]
    fn glob_output_with_ana_input_gets_an_anal_prepass_and_no_synth_step() {
        let mut def = base_def(IoKind::Ana, IoKind::WavGlob);
        def.bin = "speculate".into();
        def.subprog = Some("speculate".into());
        def.mode = None;
        def.params = vec![
            number_param("Min Frequency", 0.0, 20000.0, 200.0, NumberScale::Plain),
            number_param("Max Frequency", 0.0, 20000.0, 2000.0, NumberScale::Plain),
        ];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        let job = plan_job(
            &def,
            &[ParamValue::Number(200.0), ParamValue::Number(2000.0)],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();

        assert_eq!(job.steps.len(), 2, "expected a pvoc anal pre-pass, then the process itself");
        assert_eq!(job.steps[0].bin, "pvoc");
        assert_eq!(job.steps[0].args[0], "anal");
        assert_eq!(job.steps[0].expected_output, "in.ana");
        assert_eq!(job.steps[1].bin, "speculate");
        assert_eq!(job.steps[1].args, vec!["speculate", "in.ana", "cutout", "200", "2000"]);
        assert_eq!(job.steps[1].expected_output, "cutout0.wav");
        assert!(job.output_files.is_empty());
        assert_eq!(job.glob_output.expect("expected a GlobOutputSpec").prefix, "cutout");
        assert_eq!(job.input_files.len(), 1, "the pvoc anal pre-pass still needs a real in.wav on disk");
        assert_eq!(job.input_files[0].source_channels, vec![0]);
    }

    /// A glob-output process with no audio input at all (`input = "none"`, `output =
    /// "wav_glob"` — the shape `strands` mode 2 would need, see catalog_extra.toml's removal
    /// note) must fail with a clean `UnsupportedInV1`, not panic: the glob branch used to
    /// index `inputs[0]` before ever consulting `def.input`, and a user-authored catalog
    /// entry declaring this combination is enough to reach it.
    #[test]
    fn glob_output_with_no_input_errors_cleanly_instead_of_panicking() {
        let mut def = base_def(IoKind::None, IoKind::WavGlob);
        def.params = vec![];
        let err = plan_job(&def, &[], &[], &PvocSettings::default()).unwrap_err();
        assert!(matches!(err, PlanError::UnsupportedInV1 { .. }));
    }

    // -- Dual-input planning ---------------------------------------------------------------

    fn dual_inputs(a_channels: usize, b_channels: usize) -> [InputSpec; 2] {
        [
            InputSpec { channels: a_channels, sample_rate: 44100, len_samples: 44100, ..Default::default() },
            InputSpec { channels: b_channels, sample_rate: 44100, len_samples: 88200, ..Default::default() },
        ]
    }

    #[test]
    fn dual_wav_mono_pair_runs_a_single_lane_with_two_infiles() {
        let def = base_def(IoKind::DualWav, IoKind::Wav);
        let job = plan_job(&def, &[ParamValue::Number(3.0)], &dual_inputs(1, 1), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 1);
        // Mono + mono is the single-lane fast path: whole files, no per-channel suffixes.
        assert_eq!(job.steps[0].args, vec!["speed", "2", "in_a.wav", "in_b.wav", "out.wav", "3"]);
        assert_eq!(job.input_files.len(), 2);
        assert_eq!(job.input_files[0].input_index, 0);
        assert_eq!(job.input_files[1].input_index, 1);
    }

    #[test]
    fn dual_wav_stereo_native_uses_whole_multichannel_files() {
        let mut def = base_def(IoKind::DualWav, IoKind::Wav);
        def.stereo_native = true;
        let job = plan_job(&def, &[ParamValue::Number(3.0)], &dual_inputs(2, 1), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 1);
        assert_eq!(job.steps[0].args, vec!["speed", "2", "in_a.wav", "in_b.wav", "out.wav", "3"]);
        assert_eq!(job.input_files[0].source_channels, vec![0, 1]);
        assert_eq!(job.input_files[1].source_channels, vec![0]);
    }

    #[test]
    fn dual_wav_stereo_plus_mono_pairs_lanes_reusing_the_mono_channel() {
        let def = base_def(IoKind::DualWav, IoKind::Wav);
        let job = plan_job(&def, &[ParamValue::Number(3.0)], &dual_inputs(2, 1), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 2);
        // Lane 2 pairs the stereo input's channel 1 with the mono input's only channel.
        let lane2: Vec<_> = job.input_files.iter().filter(|f| f.relative_name.contains("c2")).collect();
        assert_eq!(lane2.len(), 2);
        assert_eq!(lane2[0].source_channels, vec![1]); // input A, channel 1
        assert_eq!(lane2[1].source_channels, vec![0]); // input B, mono reused
        assert_eq!(job.output_files.len(), 2);
    }

    #[test]
    fn dual_ana_wraps_both_inputs_in_pvoc_anal_per_lane() {
        let mut def = base_def(IoKind::DualAna, IoKind::Ana);
        def.bin = "combine".into();
        def.subprog = Some("sum".into());
        def.mode = None;
        def.params = vec![];

        let job = plan_job(&def, &[], &dual_inputs(1, 1), &PvocSettings::default()).unwrap();

        // anal A, anal B, combine, synth.
        assert_eq!(job.steps.len(), 4);
        assert_eq!(job.steps[0].args[0], "anal");
        assert_eq!(job.steps[1].args[0], "anal");
        assert_eq!(job.steps[2].bin, "combine");
        assert_eq!(job.steps[2].args, vec!["sum", "a_a1.ana", "a_b1.ana", "b1.ana"]);
        assert_eq!(job.steps[3].args[0], "synth");
        assert_eq!(job.input_files.len(), 2);
    }

    #[test]
    fn dual_input_sample_rate_mismatch_is_rejected_up_front() {
        let def = base_def(IoKind::DualWav, IoKind::Wav);
        let inputs = [
            InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() },
            InputSpec { channels: 1, sample_rate: 48000, len_samples: 48000, ..Default::default() },
        ];
        let err = plan_job(&def, &[ParamValue::Number(0.0)], &inputs, &PvocSettings::default())
            .unwrap_err();
        assert!(matches!(err, PlanError::SampleRateMismatch { first: 44100, second: 48000 }));
    }

    #[test]
    fn dual_input_process_with_one_input_is_a_count_mismatch() {
        let def = base_def(IoKind::DualWav, IoKind::Wav);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let err = plan_job(&def, &[ParamValue::Number(0.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap_err();
        assert!(matches!(err, PlanError::InputCountMismatch { expected: 2, actual: 1 }));
    }

    /// The `spec_grab_prepass` chain, which replaced `morph_glide`'s old blanket
    /// `UnsupportedInV1` rejection: two `spec grab` steps sit between the analyses and the
    /// process, the process reads the *grabbed* files rather than the full analyses, and the
    /// two position params are consumed by the grabs instead of reaching the binary.
    #[test]
    fn spec_grab_prepass_inserts_a_grab_per_input_and_consumes_the_position_params() {
        let mut def = base_def(IoKind::DualAna, IoKind::Ana);
        def.key = "morph_glide".into();
        def.bin = "morph".into();
        def.subprog = Some("glide".into());
        def.mode = None;
        def.spec_grab_prepass = true;
        def.params = vec![
            number_param("Window 1 Position", 0.0, 100.0, 10.0, NumberScale::Plain),
            number_param("Window 2 Position", 0.0, 100.0, 10.0, NumberScale::Plain),
            number_param("Output Duration", 1.0, 1000.0, 60.0, NumberScale::Plain),
        ];

        // Input A is 2s, input B is 4s -- deliberately different, since each position is a
        // percentage of its *own* input's duration.
        let inputs = vec![
            InputSpec { channels: 1, sample_rate: 44100, len_samples: 88_200, ..Default::default() },
            InputSpec { channels: 1, sample_rate: 44100, len_samples: 176_400, ..Default::default() },
        ];
        let job = plan_job(
            &def,
            &[ParamValue::Number(25.0), ParamValue::Number(50.0), ParamValue::Number(10.0)],
            &inputs,
            &PvocSettings::default(),
        )
        .unwrap();

        let grabs: Vec<_> = job.steps.iter().filter(|s| s.args.first().map(String::as_str) == Some("grab")).collect();
        assert_eq!(grabs.len(), 2, "one spec grab per input");
        assert_eq!(grabs[0].bin, "spec");
        // 25% of 2s and 50% of 4s -- each against its own input, not both against input 0.
        assert_eq!(grabs[0].args.last().unwrap(), "0.5");
        assert_eq!(grabs[1].args.last().unwrap(), "2");

        // The grabs run after the analyses and before the glide.
        let idx = |pred: &dyn Fn(&Invocation) -> bool| job.steps.iter().position(pred).unwrap();
        let last_anal = job.steps.iter().rposition(|s| s.args.first().map(String::as_str) == Some("anal")).unwrap();
        let glide = idx(&|s: &Invocation| s.bin == "morph");
        assert!(last_anal < idx(&|s: &Invocation| s.args.first().map(String::as_str) == Some("grab")));
        assert!(grabs.iter().all(|g| job.steps.iter().position(|s| std::ptr::eq(s, *g)).unwrap() < glide));

        // The glide reads the grabbed windows, and its only param is Output Duration -- the
        // two positions were consumed by the pre-pass and must not appear here.
        let glide_args = &job.steps[glide].args;
        assert!(glide_args.contains(&"g_a1.ana".to_string()), "reads grab A, got {glide_args:?}");
        assert!(glide_args.contains(&"g_b1.ana".to_string()), "reads grab B, got {glide_args:?}");
        assert!(!glide_args.iter().any(|a| a == "25" || a == "50"), "positions leaked: {glide_args:?}");
        assert_eq!(glide_args.last().unwrap(), "10");
    }

    /// The headroom attenuation is only lossless because the path is float end to end. A
    /// `requires_simple_wav_input` process is handed a 16-bit *integer* temp file instead,
    /// where −24 dB really would cost 4 bits of resolution — so the two must never coincide.
    /// None do today; this keeps it that way if either list grows.
    #[test]
    fn clip_headroom_never_applies_to_integer_input_processes() {
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let offenders: Vec<_> = CLIP_HEADROOM_PROCESSES
            .iter()
            .filter(|key| catalog.find(key).is_some_and(|d| d.requires_simple_wav_input))
            .collect();
        assert!(
            offenders.is_empty(),
            "these are on the headroom list but get 16-bit integer temp input, where the \
             attenuation is lossy: {offenders:?}"
        );
    }

    /// Every key on the headroom list must actually exist, so a catalog rename can't silently
    /// turn an entry's clipping fix back off.
    #[test]
    fn clip_headroom_process_keys_all_exist() {
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let missing: Vec<_> =
            CLIP_HEADROOM_PROCESSES.iter().filter(|key| catalog.find(key).is_none()).collect();
        assert!(missing.is_empty(), "headroom list names processes not in the catalog: {missing:?}");
    }

    /// A flagged process gets every input attenuated and records the exact inverse; an
    /// unflagged one is untouched.
    #[test]
    fn clip_headroom_attenuates_inputs_and_records_the_inverse() {
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("focus_accu").expect("focus_accu in catalog");
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let job =
            plan_job(def, &values, std::slice::from_ref(&input), &PvocSettings::default()).unwrap();

        assert_eq!(job.clip_headroom_restore, Some(16.0));
        assert!(!job.input_files.is_empty());
        for spec in &job.input_files {
            assert_eq!(spec.gain, Some(CLIP_HEADROOM_ATTENUATION));
        }

        // An unflagged process in the same family is left alone.
        let other = catalog.find("blur_blur").or_else(|| catalog.find("spec_gain")).unwrap();
        let ovalues: Vec<_> = other.params.iter().map(|p| p.kind.default_value()).collect();
        let ojob =
            plan_job(other, &ovalues, std::slice::from_ref(&input), &PvocSettings::default()).unwrap();
        assert_eq!(ojob.clip_headroom_restore, None);
        assert!(ojob.input_files.iter().all(|s| s.gain.is_none()));
    }

    /// Without the flag, a dual-ana process is planned exactly as before — no grab steps, and
    /// every param reaches the binary.
    #[test]
    fn dual_ana_without_the_prepass_flag_is_unchanged() {
        let mut def = base_def(IoKind::DualAna, IoKind::Ana);
        def.subprog = Some("bridge".into());
        def.params = vec![number_param("Amount", 0.0, 100.0, 50.0, NumberScale::Plain)];
        let job =
            plan_job(&def, &[ParamValue::Number(50.0)], &dual_inputs(1, 1), &PvocSettings::default()).unwrap();
        assert!(!job.steps.iter().any(|s| s.args.first().map(String::as_str) == Some("grab")));
    }

    #[test]
    fn missing_input_for_wav_process_is_an_error() {
        let def = base_def(IoKind::Wav, IoKind::Wav);
        let err = plan_job(&def, &[ParamValue::Number(0.0)], &[], &PvocSettings::default())
            .unwrap_err();
        assert!(matches!(err, PlanError::MissingInput));
    }

    #[test]
    fn param_count_mismatch_is_rejected() {
        let def = base_def(IoKind::Wav, IoKind::Wav);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let err = plan_job(&def, &[], std::slice::from_ref(&input), &PvocSettings::default()).unwrap_err();
        assert!(matches!(err, PlanError::ParamCountMismatch { expected: 1, actual: 0 }));
    }

    // -- plan_curve_transform_job (IoKind::Curve, Phase 4 "hard tier") -------------------

    fn curve_def() -> ProcessDef {
        let mut def = base_def(IoKind::Curve, IoKind::Curve);
        def.bin = "repitch".into();
        def.subprog = Some("invert".into());
        def.mode = Some("1".into());
        def
    }

    /// A minimal but structurally real CDP binary pitchfile — `fmt ` (IEEE float, mono,
    /// `arate` as the sample-rate field) + `data` (`values` as float32 LE) — enough for
    /// `plan_curve_transform_job` to read a grid from and splice into. Mirrors
    /// `model::curve::tests::fake_pitch_wav` (duplicated rather than shared across module
    /// boundaries for a handful of lines).
    fn fake_binary_template(arate: u32, values: &[f32]) -> Vec<u8> {
        let mut fmt_body = Vec::new();
        fmt_body.extend_from_slice(&3u16.to_le_bytes());
        fmt_body.extend_from_slice(&1u16.to_le_bytes());
        fmt_body.extend_from_slice(&arate.to_le_bytes());
        fmt_body.extend_from_slice(&(arate * 4).to_le_bytes());
        fmt_body.extend_from_slice(&4u16.to_le_bytes());
        fmt_body.extend_from_slice(&32u16.to_le_bytes());
        let mut data_body = Vec::new();
        for &v in values {
            data_body.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&(fmt_body.len() as u32).to_le_bytes());
        out.extend_from_slice(&fmt_body);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_body.len() as u32).to_le_bytes());
        out.extend_from_slice(&data_body);
        out
    }

    #[test]
    fn plan_curve_transform_job_splices_the_curve_into_the_binary_template() {
        let def = curve_def();
        let template = fake_binary_template(2, &[219.7, 219.7]); // arate=2 -> grid [0.0, 0.5]
        let points = vec![(0.0, 220.0), (0.5, 440.0)];
        let job = plan_curve_transform_job(&def, &[ParamValue::Number(0.0)], &template, &points).unwrap();

        assert_eq!(job.steps.len(), 2);
        assert_eq!(job.steps[0].bin, "repitch");
        assert_eq!(job.steps[0].args, vec!["invert", "1", "curve_in.wav", "curve_raw_out.pch", "0"]);
        // CDP forces its own .wav suffix onto any binary-pitch-data outfile.
        assert_eq!(job.steps[0].expected_output, "curve_raw_out.pch.wav");
        assert_eq!(job.steps[1].bin, "repitch");
        assert_eq!(job.steps[1].args, vec!["pchtotext", "curve_raw_out.pch.wav", "curve_out.txt"]);
        assert_eq!(job.output_curve, Some("curve_out.txt".to_string()));
        assert_eq!(job.output_curve_binary_template, Some("curve_raw_out.pch.wav".to_string()));

        let (name, spliced) = job.binary_input_files.first().expect("spliced binary input file");
        assert_eq!(name, "curve_in.wav");
        assert_eq!(spliced.len(), template.len(), "splicing must never change the file's size");
        let data_offset = spliced.len() - 8; // this fixture's data chunk payload starts here
        let vals: Vec<f32> = spliced[data_offset..]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(vals, vec![220.0, 440.0], "the curve's own points should replace the template's");
    }

    #[test]
    fn plan_curve_transform_job_rejects_a_process_not_declared_as_curve_both_sides() {
        let def = base_def(IoKind::Wav, IoKind::Curve);
        let template = fake_binary_template(2, &[219.7, 219.7]);
        let err = plan_curve_transform_job(&def, &[ParamValue::Number(0.0)], &template, &[(0.0, 1.0)]).unwrap_err();
        assert!(matches!(err, PlanError::UnsupportedInV1 { .. }));
    }

    #[test]
    fn plan_curve_transform_job_rejects_a_template_that_isnt_a_valid_pitch_wav() {
        let def = curve_def();
        let err = plan_curve_transform_job(&def, &[ParamValue::Number(0.0)], b"not a riff file", &[(0.0, 1.0)])
            .unwrap_err();
        assert!(matches!(err, PlanError::UnsupportedInV1 { .. }));
    }

    #[test]
    fn plan_job_rejects_a_curve_process_directing_the_caller_to_plan_curve_transform_job() {
        let def = curve_def();
        let err = plan_job(&def, &[ParamValue::Number(0.0)], &[], &PvocSettings::default()).unwrap_err();
        assert!(matches!(err, PlanError::UnsupportedInV1 { .. }));
    }

    // -- plan_extract_pitch_curve ("Extract Pitch Curve" action, the asymmetric ana-in/
    //    curve-out shape `plan_curve_job` doesn't cover) ----------------------------------

    #[test]
    fn plan_extract_pitch_curve_wraps_in_pvoc_anal_then_repitch_getpitch_mode_1_then_pchtotext() {
        let job = plan_extract_pitch_curve(&PvocSettings::default());

        assert_eq!(job.steps.len(), 3);
        assert_eq!(job.steps[0].bin, "pvoc");
        assert_eq!(job.steps[0].args, vec!["anal", "1", "in.wav", "in.ana", "-c1024", "-o3"]);
        assert_eq!(job.steps[1].bin, "repitch");
        assert_eq!(job.steps[1].args, vec!["getpitch", "1", "in.ana", "resynth.wav", "pitch.pch"]);
        // getpitch silently writes <name>.wav regardless of the literal name given.
        assert_eq!(job.steps[1].expected_output, "pitch.pch.wav");
        assert_eq!(job.steps[2].bin, "repitch");
        assert_eq!(job.steps[2].args, vec!["pchtotext", "pitch.pch.wav", "pitch.txt"]);
        assert_eq!(job.output_curve, Some("pitch.txt".to_string()));
        assert_eq!(job.output_curve_binary_template, Some("pitch.pch.wav".to_string()));
        assert_eq!(job.output_files, Vec::new());
        assert_eq!(
            job.input_files,
            vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0], gain: None }]
        );
    }

    #[test]
    fn plan_extract_pitch_curve_only_takes_the_first_channel() {
        let job = plan_extract_pitch_curve(&PvocSettings::default());
        assert_eq!(job.input_files[0].source_channels, vec![0], "pitch is one melodic line, not per-channel");
    }

    // -- plan_extract_formants ("Extract Formants" action, CDP-Ext-Plan.md Phase 5's own
    //    asymmetric ana-in/buffer-out shape) -----------------------------------------------

    #[test]
    fn plan_extract_formants_wraps_in_pvoc_anal_then_formants_get() {
        let job = plan_extract_formants(&PvocSettings::default(), FormantExtractionMode::PitchWise(8));

        assert_eq!(job.steps.len(), 2);
        assert_eq!(job.steps[0].bin, "pvoc");
        assert_eq!(job.steps[0].args, vec!["anal", "1", "in.wav", "in.ana", "-c1024", "-o3"]);
        assert_eq!(job.steps[1].bin, "formants");
        assert_eq!(job.steps[1].args, vec!["get", "in.ana", "out.for", "-p8"]);
        // Unlike getpitch, formants get does NOT append .wav to its declared outfile.
        assert_eq!(job.steps[1].expected_output, "out.for");
        assert_eq!(job.output_formant_buffer, Some("out.for".to_string()));
        assert_eq!(job.output_curve, None);
        assert_eq!(job.output_files, Vec::new());
        assert_eq!(
            job.input_files,
            vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0], gain: None }]
        );
    }

    #[test]
    fn plan_extract_formants_only_takes_the_first_channel() {
        let job = plan_extract_formants(&PvocSettings::default(), FormantExtractionMode::PitchWise(8));
        assert_eq!(job.input_files[0].source_channels, vec![0], "a formant envelope is one spectral shape, not per-channel");
    }

    #[test]
    fn plan_extract_formants_freqwise_uses_the_f_flag() {
        let job = plan_extract_formants(&PvocSettings::default(), FormantExtractionMode::FreqWise(8));
        assert_eq!(job.steps[1].args, vec!["get", "in.ana", "out.for", "-f8"]);
    }

    // -- plan_oneform_get ("freeze snapshot" action, the Formant-buffer-in/Snapshot-buffer-out
    //    shape — no audio, no pvoc anal step at all) --------------------------------------

    #[test]
    fn plan_oneform_get_splices_the_buffer_in_and_names_the_wav_suffixed_output() {
        let job = plan_oneform_get(b"fake formant buffer bytes", 0.5);

        assert_eq!(job.steps.len(), 1);
        assert_eq!(job.steps[0].bin, "oneform");
        assert_eq!(job.steps[0].args, vec!["get", "in.for", "moment.1f", "0.5"]);
        // Unlike formants get, oneform get DOES append .wav to its declared outfile.
        assert_eq!(job.steps[0].expected_output, "moment.1f.wav");
        assert_eq!(job.output_formant_buffer, Some("moment.1f.wav".to_string()));
        assert_eq!(
            job.binary_input_files,
            vec![("in.for".to_string(), b"fake formant buffer bytes".to_vec())]
        );
        assert!(job.input_files.is_empty(), "no audio input at all for this shape");
    }

    // -- .ana decfactor header parsing (Phase 0 spike S5) --------------------------------

    /// Builds a minimal fake `.ana` byte buffer with a RIFF `note` LIST chunk containing the
    /// given key/value (hex) pairs, matching the format captured from real CDP8 output.
    fn fake_ana_note_chunk(pairs: &[(&str, u32)]) -> Vec<u8> {
        let mut body = String::new();
        for (key, value) in pairs {
            body.push_str(key);
            body.push('\n');
            body.push_str(&hex::encode_le_u32(*value));
            body.push('\n');
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"note");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body.as_bytes());
        out
    }

    mod hex {
        pub fn encode_le_u32(v: u32) -> String {
            v.to_le_bytes().iter().map(|b| format!("{b:02X}")).collect()
        }
    }

    #[test]
    fn parses_decfactor_from_ana_note_chunk() {
        let data = fake_ana_note_chunk(&[
            ("original sample rate", 44100),
            ("arate", 344),
            ("analwinlen", 1024),
            ("decfactor", 128),
        ]);
        assert_eq!(parse_ana_decfactor(&data), Some(128));
    }

    #[test]
    fn missing_note_chunk_returns_none() {
        assert_eq!(parse_ana_decfactor(b"RIFF....WAVEfmt "), None);
    }

    #[test]
    fn window_count_matches_observed_default_overlap_math() {
        // 2 seconds @ 44100Hz, decfactor 128 (points=1024, overlap=3 default -- verified
        // against real CDP output in the Phase 0 spike).
        assert_eq!(window_count_from_decfactor(88200, 128), 690);
    }

    // ---- crystal rotate's compound VDAT datafile -------------------------------------

    fn crystal_like_def() -> ProcessDef {
        let mut def = base_def(IoKind::VariadicWav, IoKind::Wav);
        def.key = "crystal_rotate_1".into();
        def.bin = "crystal".into();
        def.subprog = Some("rotate".into());
        def.mode = Some("1".into());
        def.params = vec![ParamDef {
            rows_match_input_count: false,
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            name: "Crystal Data".into(),
            description: String::new(),
            flag: None,
            automatable: false,
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
            before_outfile: false,
            praat_pause_block: None,
            praat_directory_var: None,
            key_value_group: None,
            key_value_key: None,
            kind: ParamKind::CrystalVdat,
        }];
        def
    }

    fn crystal_value(vertices: Vec<[f64; 3]>) -> ParamValue {
        ParamValue::CrystalVdat(crate::model::cdp::CrystalVdat {
            vertices,
            envelope: vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)],
        })
    }

    /// The datafile's exact bytes, not just "a file was written". Line shape is what CDP's
    /// own parser uses to tell the two sections apart, so this pins vertices at 3 numbers per
    /// line and envelope points at 2 — the byte-level property `write_crystal_vdat`'s doc
    /// comment explains and the real-binary test in `cdp::runner` then confirms end to end.
    #[test]
    fn crystal_vdat_datafile_writes_vertices_three_per_line_and_envelope_two_per_line() {
        let def = crystal_like_def();
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        let job = plan_job(
            &def,
            &[crystal_value(vec![[0.5, 0.25, -0.1]])],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap();

        assert_eq!(job.steps[0].args, vec!["rotate", "1", "in_1.wav", "out.wav", "vdat_0.txt"]);
        assert_eq!(job.brk_files.len(), 1);
        let (name, contents) = &job.brk_files[0];
        assert_eq!(name, "vdat_0.txt");

        let data_lines: Vec<&str> = contents.lines().filter(|l| !l.trim_start().starts_with(';')).collect();
        assert_eq!(data_lines, vec!["0.5 0.25 -0.1", "0 0", "0.5 1", "1 0"]);
        // Every non-comment line before the envelope must have exactly 3 numbers and every
        // envelope line exactly 2 — the rule the parser actually applies.
        assert_eq!(data_lines[0].split_whitespace().count(), 3);
        for line in &data_lines[1..] {
            assert_eq!(line.split_whitespace().count(), 2, "envelope line {line:?} must not look like a vertex");
        }
    }

    /// The vertex/input-count rule is pre-blocked rather than left to CDP — see
    /// `check_compound_param_data`'s doc comment for why. It mirrors CDP's condition exactly,
    /// including the single-input escape hatch.
    #[test]
    fn crystal_vertex_count_must_match_the_input_count_only_when_more_than_one_file() {
        let def = crystal_like_def();
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };

        // One input file: any vertex count is legal (the file is re-read once per vertex).
        for vertices in [vec![[0.0, 0.0, 0.0]], vec![[0.1, 0.0, 0.0], [-0.1, 0.0, 0.0], [0.0, 0.2, 0.0]]] {
            assert!(
                plan_job(&def, &[crystal_value(vertices)], std::slice::from_ref(&input), &PvocSettings::default())
                    .is_ok(),
                "a single input accepts any vertex count"
            );
        }

        // Two input files, three vertices: blocked before anything is spawned.
        let err = plan_job(
            &def,
            &[crystal_value(vec![[0.1, 0.0, 0.0], [-0.1, 0.0, 0.0], [0.0, 0.2, 0.0]])],
            &[input.clone(), input.clone()],
            &PvocSettings::default(),
        )
        .unwrap_err();
        let PlanError::InvalidParamData { param, reason } = err else {
            panic!("expected InvalidParamData, got {err:?}");
        };
        assert_eq!(param, "Crystal Data");
        assert!(reason.contains("2 input files but 3 vertices"), "{reason}");

        // Two input files, two vertices: fine.
        assert!(plan_job(
            &def,
            &[crystal_value(vec![[0.1, 0.0, 0.0], [-0.1, 0.0, 0.0]])],
            &[input.clone(), input.clone()],
            &PvocSettings::default(),
        )
        .is_ok());
    }

    /// `CrystalVdat::validate`'s structural rules must be surfaced by `plan_job` too, not
    /// only by the UI — a value can reach here from a saved preset that predates the check.
    #[test]
    fn crystal_vdat_structural_errors_block_planning_with_the_real_reason() {
        let def = crystal_like_def();
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100, ..Default::default() };
        // Each coordinate is inside -1..1 but the vector length is >1 — the constraint CDP's
        // `get_vectorlen` really does enforce.
        let err = plan_job(
            &def,
            &[crystal_value(vec![[0.9, 0.9, 0.9]])],
            std::slice::from_ref(&input),
            &PvocSettings::default(),
        )
        .unwrap_err();
        let PlanError::InvalidParamData { reason, .. } = err else { panic!("expected InvalidParamData") };
        assert!(reason.contains("unit sphere"), "{reason}");
    }
}

