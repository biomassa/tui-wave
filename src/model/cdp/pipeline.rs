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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputSpec {
    pub channels: usize,
    pub sample_rate: u32,
    pub len_samples: usize,
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
    /// `.ana`/`.wav` or the job's final output (see CDP-PLAN.md §7: CDP never creates an
    /// output file on failure, but a defensive existence check is cheap and catches any
    /// exit-0-but-no-output edge case).
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
}

/// A temp output file the runner must read after the job completes, and which destination
/// channel(s) of the final result its content fills.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputWavSpec {
    pub relative_name: String,
    pub dest_channels: Vec<usize>,
}

/// A `PercentOfAnaWindowCount` parameter can't be resolved until the real `.ana` file
/// exists — see CDP-PLAN.md Phase 0 spike finding S5: CDP recalculates the actual analysis
/// window length from the requested overlap factor in a way that can't be predicted before
/// `pvoc anal` runs. The runner parses `ana_relative_name`'s header for `decfactor` after
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
    pub brk_files: Vec<(String, String)>,
    pub deferred_window_params: Vec<DeferredWindowParam>,
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
    /// Dual-input processing requires both inputs at the same sample rate — CDP itself
    /// rejects mismatched-rate inputs, so this is caught up front with a clearer message.
    SampleRateMismatch { first: u32, second: u32 },
}

/// Parses the `decfactor` field out of a `.ana` file's RIFF `note` chunk (hex-encoded
/// little-endian ints, one `key\nhex\n` pair per line — verified against real CDP 7.1
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
fn scale_number_value(scale: NumberScale, raw: f64, duration_secs: f64, pvoc: &PvocSettings) -> f64 {
    match scale {
        NumberScale::Plain | NumberScale::OutputDurationSeconds => raw,
        NumberScale::PercentOfInputDuration => {
            if raw >= 100.0 { duration_secs - 0.1 } else { duration_secs * raw / 100.0 }
        }
        NumberScale::PercentOfFftSize => (pvoc.points as f64 * raw / 100.0).max(1.0).round(),
        NumberScale::PercentOfAnaWindowCount => {
            unreachable!("PercentOfAnaWindowCount is deferred, never resolved here")
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
    brk_files: &mut Vec<(String, String)>,
    brk_index: usize,
) -> ParamPlan {
    match value {
        ParamValue::Toggle(false) => ParamPlan { arg: None, deferred: None },
        ParamValue::Toggle(true) => ParamPlan {
            arg: Some(param.flag.clone().unwrap_or_default()),
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
                    let value = scale_number_value(*other, *raw, duration_secs, pvoc);
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
                        .map(|&(t, v)| format!("{t} {}", scale_number_value(*other, v, duration_secs, pvoc)))
                        .collect::<Vec<_>>()
                        .join("\n");
                    brk_files.push((relative_name.clone(), contents));
                    ParamPlan { arg: format_arg(&param.flag, &relative_name), deferred: None }
                }
            }
        }
    }
}

/// Appends `def`'s positional args (subprog, mode) then param args, resolving scales
/// against `duration_secs`/`pvoc`. `brk_files` accumulates side effects that apply to the
/// whole job, not just this one invocation. The returned `Vec` holds one
/// `DeferredWindowTarget` per deferred (`PercentOfAnaWindowCount`) param this invocation's
/// args (or `.brk` files) reference — almost always 0 or 1 in practice (only one catalog
/// param uses that scale today), but a process could in principle carry more than one.
fn build_process_args(
    def: &ProcessDef,
    values: &[ParamValue],
    infiles: &[&str],
    outfile: &str,
    duration_secs: f64,
    pvoc: &PvocSettings,
    brk_files: &mut Vec<(String, String)>,
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
    args.extend(infiles.iter().map(|s| s.to_string()));
    args.push(outfile.to_string());

    let mut deferred = Vec::new();
    for (i, (param, value)) in def.params.iter().zip(values).enumerate() {
        let plan = plan_param(param, value, duration_secs, pvoc, brk_files, i);
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
    // morph_glide needs a `spec grab` pre-pass extracting one window from each input before
    // the glide itself (its two position params are percentages into each file, not plain
    // args) — SoundThread special-cases it the same way; not generalizable, not built.
    if def.key == "morph_glide" {
        return Err(PlanError::UnsupportedInV1 {
            reason: "morph glide needs a spec-grab pre-pass, not built yet".into(),
        });
    }

    let expected_inputs = match def.input {
        IoKind::None => 0,
        // `WavGlob` is output-only (see its doc comment) and never valid as `def.input` — a
        // catalog bug, not a real input arity, but the match must stay exhaustive.
        IoKind::Wav | IoKind::Ana | IoKind::WavGlob => 1,
        IoKind::DualWav | IoKind::DualAna => 2,
    };
    if inputs.len() != expected_inputs {
        if expected_inputs > 0 && inputs.is_empty() {
            return Err(PlanError::MissingInput);
        }
        return Err(PlanError::InputCountMismatch { expected: expected_inputs, actual: inputs.len() });
    }
    if let [first, second] = inputs {
        if first.sample_rate != second.sample_rate {
            return Err(PlanError::SampleRateMismatch {
                first: first.sample_rate,
                second: second.sample_rate,
            });
        }
    }

    // `WavGlob` (an unknown number of numbered output files) is a distinct enough result
    // shape — one mono lane always, no channel merging, no splice target — that it gets its
    // own planning function rather than threading a glob flag through `plan_wav`'s
    // stereo-lane-splitting logic. Checked on `def.output`, ahead of the `def.input`
    // dispatch below (which stays keyed on input arity as normal).
    if def.output == IoKind::WavGlob {
        return plan_wav_glob(def, values, &inputs[0], pvoc);
    }

    match def.input {
        IoKind::None => plan_synthesis(def, values, pvoc),
        IoKind::Wav => plan_wav(def, values, &inputs[0], pvoc),
        IoKind::Ana => plan_ana(def, values, &inputs[0], pvoc),
        IoKind::DualWav => plan_dual_wav(def, values, &inputs[0], &inputs[1], pvoc),
        IoKind::DualAna => plan_dual_ana(def, values, &inputs[0], &inputs[1], pvoc),
        // Never valid as `def.input` (see `IoKind::WavGlob`'s doc comment) — a catalog bug
        // if reached, not a real plan to build.
        IoKind::WavGlob => Err(PlanError::UnsupportedInV1 {
            reason: "WavGlob is not a valid input kind".into(),
        }),
    }
}

/// Plans a glob-output process (`IoKind::WavGlob` — an unknown number of numbered mono
/// output files sharing a prefix, e.g. `distcut`/`envcut`). Always exactly one mono lane:
/// only the source's first channel is written to the temp input file (see
/// `GlobOutputSpec`'s doc comment for why stereo isn't supported here). `expected_output`
/// checks for `<prefix>0.wav` specifically — CDP numbers this family of outputs from 0.
fn plan_wav_glob(
    def: &ProcessDef,
    values: &[ParamValue],
    input: &InputSpec,
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let duration = input.duration_secs();
    let prefix = "cutout".to_string();

    let (args, deferred) =
        build_process_args(def, values, &["in.wav"], &prefix, duration, pvoc, &mut brk_files)?;
    debug_assert!(deferred.is_empty(), "glob-output processes never carry ana-window-count params");

    Ok(PlannedJob {
        steps: vec![Invocation {
            bin: def.bin.clone(),
            args,
            label: process_label(def),
            expected_output: format!("{prefix}0.wav"),
        }],
        input_files: vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0] }],
        output_files: Vec::new(),
        glob_output: Some(GlobOutputSpec { prefix }),
        brk_files,
        deferred_window_params: Vec::new(),
    })
}

fn plan_synthesis(
    def: &ProcessDef,
    values: &[ParamValue],
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let (args, deferred) =
        build_process_args(def, values, &[], "out.wav", 0.0, pvoc, &mut brk_files)?;
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
        glob_output: None,
        deferred_window_params: Vec::new(),
    })
}

fn plan_wav(
    def: &ProcessDef,
    values: &[ParamValue],
    input: &InputSpec,
    pvoc: &PvocSettings,
) -> Result<PlannedJob, PlanError> {
    let mut brk_files = Vec::new();
    let duration = input.duration_secs();

    if input.channels <= 1 || def.stereo_native {
        let source_channels: Vec<usize> = (0..input.channels.max(1)).collect();
        let (args, deferred) =
            build_process_args(def, values, &["in.wav"], "out.wav", duration, pvoc, &mut brk_files)?;
        debug_assert!(deferred.is_empty(), "wav processes never carry ana-window-count params");
        let dest_channels = source_channels.clone();
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
                source_channels,
            }],
            output_files: vec![OutputWavSpec { relative_name: "out.wav".into(), dest_channels }],
            brk_files,
            glob_output: None,
        deferred_window_params: Vec::new(),
        });
    }

    // Stereo doc, mono-only process: dual-mono lanes, split/merged in Rust.
    let mut steps = Vec::new();
    let mut input_files = Vec::new();
    let mut output_files = Vec::new();
    for ch in 0..input.channels {
        let infile = format!("in_c{}.wav", ch + 1);
        let outfile = format!("out_c{}.wav", ch + 1);
        let (args, deferred) = build_process_args(
            def,
            values,
            &[infile.as_str()],
            &outfile,
            duration,
            pvoc,
            &mut brk_files,
        )?;
        debug_assert!(deferred.is_empty());
        let label = format!("{}{}", process_label(def), channel_label(ch, input.channels));
        steps.push(Invocation { bin: def.bin.clone(), args, label, expected_output: outfile.clone() });
        input_files.push(TempWavSpec { relative_name: infile, input_index: 0, source_channels: vec![ch] });
        output_files.push(OutputWavSpec { relative_name: outfile, dest_channels: vec![ch] });
    }

    Ok(PlannedJob { steps, input_files, output_files, glob_output: None, brk_files, deferred_window_params: Vec::new() })
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
            &mut brk_files,
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
                    source_channels: (0..a.channels.max(1)).collect(),
                },
                TempWavSpec {
                    relative_name: "in_b.wav".into(),
                    input_index: 1,
                    source_channels: (0..b.channels.max(1)).collect(),
                },
            ],
            output_files: vec![OutputWavSpec {
                relative_name: "out.wav".into(),
                dest_channels: (0..a.channels.max(1)).collect(),
            }],
            brk_files,
            glob_output: None,
        deferred_window_params: Vec::new(),
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
            &mut brk_files,
        )?;
        debug_assert!(deferred.is_empty());
        let label = format!("{}{}", process_label(def), channel_label(ch, lanes));
        steps.push(Invocation { bin: def.bin.clone(), args, label, expected_output: outfile.clone() });
        input_files.push(TempWavSpec {
            relative_name: infile_a,
            input_index: 0,
            source_channels: vec![ch.min(a.channels.saturating_sub(1))],
        });
        input_files.push(TempWavSpec {
            relative_name: infile_b,
            input_index: 1,
            source_channels: vec![ch.min(b.channels.saturating_sub(1))],
        });
        output_files.push(OutputWavSpec { relative_name: outfile, dest_channels: vec![ch] });
    }

    Ok(PlannedJob { steps, input_files, output_files, glob_output: None, brk_files, deferred_window_params: Vec::new() })
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
            source_channels: vec![ch.min(a.channels.saturating_sub(1))],
        });
        input_files.push(TempWavSpec {
            relative_name: wav_b.clone(),
            input_index: 1,
            source_channels: vec![ch.min(b.channels.saturating_sub(1))],
        });

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

        let (args, deferred) = build_process_args(
            def,
            values,
            &[ana_a.as_str(), ana_b.as_str()],
            &ana_out,
            duration,
            pvoc,
            &mut brk_files,
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

    Ok(PlannedJob { steps, input_files, output_files, glob_output: None, brk_files, deferred_window_params: Vec::new() })
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

        input_files.push(TempWavSpec { relative_name: wav_in.clone(), input_index: 0, source_channels: vec![ch] });

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
            &mut brk_files,
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

    Ok(PlannedJob { steps, input_files, output_files, glob_output: None, brk_files, deferred_window_params })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::def::{Category, ParamDef, ParamKind};

    fn number_param(name: &str, min: f64, max: f64, default: f64, scale: NumberScale) -> ParamDef {
        ParamDef {
            name: name.into(),
            description: String::new(),
            flag: None,
            automatable: false,
            kind: ParamKind::Number { min, max, step: 1.0, default, exponential: false, scale },
        }
    }

    fn base_def(input: IoKind, output: IoKind) -> ProcessDef {
        ProcessDef {
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
            params: vec![number_param("Speed", -96.0, 96.0, 0.0, NumberScale::Plain)],
        }
    }

    #[test]
    fn mono_wav_single_lane_matches_modify_speed_2() {
        let def = base_def(IoKind::Wav, IoKind::Wav);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 };
        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 1);
        assert_eq!(job.steps[0].bin, "modify");
        assert_eq!(job.steps[0].args, vec!["speed", "2", "in.wav", "out.wav", "3"]);
        assert_eq!(job.input_files, vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0] }]);
        assert_eq!(
            job.output_files,
            vec![OutputWavSpec { relative_name: "out.wav".into(), dest_channels: vec![0] }]
        );
    }

    #[test]
    fn stereo_wav_non_native_splits_into_dual_mono_lanes() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.stereo_native = false;
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100 };
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

    #[test]
    fn stereo_native_process_keeps_single_lane() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.stereo_native = true;
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100 };
        let job = plan_job(&def, &[ParamValue::Number(3.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 1);
        assert_eq!(job.input_files, vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0, 1] }]);
        assert_eq!(
            job.output_files,
            vec![OutputWavSpec { relative_name: "out.wav".into(), dest_channels: vec![0, 1] }]
        );
    }

    #[test]
    fn ana_input_wraps_with_pvoc_anal_and_synth() {
        let mut def = base_def(IoKind::Ana, IoKind::Ana);
        def.bin = "blur".into();
        def.subprog = Some("avrg".into());
        def.mode = None;
        def.params = vec![number_param("Channels", 1.0, 200.0, 6.0, NumberScale::Plain)];

        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 88200 };
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

        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 88200 };
        let job = plan_job(&def, &[ParamValue::Number(6.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap();

        assert_eq!(job.steps.len(), 6);
        assert_eq!(job.input_files.len(), 2);
        assert_eq!(job.output_files.len(), 2);
        assert_eq!(job.output_files[0].dest_channels, vec![0]);
        assert_eq!(job.output_files[1].dest_channels, vec![1]);
    }

    #[test]
    fn flagged_toggle_and_choice_params_format_correctly() {
        let mut def = base_def(IoKind::Wav, IoKind::Wav);
        def.params = vec![
            ParamDef {
                name: "Omit".into(),
                description: String::new(),
                flag: Some("-x".into()),
                automatable: false,
                kind: ParamKind::Toggle { default: false },
            },
            ParamDef {
                name: "Rate".into(),
                description: String::new(),
                flag: None,
                automatable: false,
                kind: ParamKind::Choice { options: vec!["44100".into(), "48000".into()], default: 0 },
            },
        ];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 };

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
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 * 2 }; // 2s

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
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 };

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

    #[test]
    fn percent_of_ana_window_count_is_deferred_not_precomputed() {
        let mut def = base_def(IoKind::Ana, IoKind::Ana);
        def.bin = "blur".into();
        def.subprog = Some("blur".into());
        def.mode = None;
        def.params = vec![number_param("Blurring", 0.1, 100.0, 20.0, NumberScale::PercentOfAnaWindowCount)];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 };

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
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100 };

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
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 };
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
            name: "Gain".into(),
            description: String::new(),
            flag: Some("-f".into()),
            automatable: true,
            kind: ParamKind::Number {
                min: 0.0,
                max: 2.0,
                step: 0.01,
                default: 1.0,
                exponential: false,
                scale: NumberScale::Plain,
            },
        }];
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 };

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
        let input = InputSpec { channels: 2, sample_rate: 44100, len_samples: 44100 };

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

    // -- Dual-input planning ---------------------------------------------------------------

    fn dual_inputs(a_channels: usize, b_channels: usize) -> [InputSpec; 2] {
        [
            InputSpec { channels: a_channels, sample_rate: 44100, len_samples: 44100 },
            InputSpec { channels: b_channels, sample_rate: 44100, len_samples: 88200 },
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
            InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 },
            InputSpec { channels: 1, sample_rate: 48000, len_samples: 48000 },
        ];
        let err = plan_job(&def, &[ParamValue::Number(0.0)], &inputs, &PvocSettings::default())
            .unwrap_err();
        assert!(matches!(err, PlanError::SampleRateMismatch { first: 44100, second: 48000 }));
    }

    #[test]
    fn dual_input_process_with_one_input_is_a_count_mismatch() {
        let def = base_def(IoKind::DualWav, IoKind::Wav);
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 };
        let err = plan_job(&def, &[ParamValue::Number(0.0)], std::slice::from_ref(&input), &PvocSettings::default())
            .unwrap_err();
        assert!(matches!(err, PlanError::InputCountMismatch { expected: 2, actual: 1 }));
    }

    #[test]
    fn morph_glide_stays_unsupported() {
        let mut def = base_def(IoKind::DualAna, IoKind::Ana);
        def.key = "morph_glide".into();
        let err = plan_job(&def, &[ParamValue::Number(0.0)], &dual_inputs(1, 1), &PvocSettings::default())
            .unwrap_err();
        assert!(matches!(err, PlanError::UnsupportedInV1 { .. }));
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
        let input = InputSpec { channels: 1, sample_rate: 44100, len_samples: 44100 };
        let err = plan_job(&def, &[], std::slice::from_ref(&input), &PvocSettings::default()).unwrap_err();
        assert!(matches!(err, PlanError::ParamCountMismatch { expected: 1, actual: 0 }));
    }

    // -- .ana decfactor header parsing (Phase 0 spike S5) --------------------------------

    /// Builds a minimal fake `.ana` byte buffer with a RIFF `note` LIST chunk containing the
    /// given key/value (hex) pairs, matching the format captured from real CDP 7.1 output.
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
}
