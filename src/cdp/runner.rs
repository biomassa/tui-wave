//! Runs a `PlannedJob` (see `model::cdp::pipeline`) on a dedicated thread, mirroring
//! `audio::engine::AudioEngine`'s thread + crossbeam-channel pattern: the UI submits jobs
//! and polls `events` once per frame, never blocking on a CDP subprocess. This is the piece
//! the codebase didn't already have a template for — everything else (`Command`, dialogs,
//! temp-WAV I/O) had a precedent to follow.

use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::model::cdp::pipeline::{parse_ana_decfactor, CLIP_HEADROOM_TARGET_PEAK, window_count_from_decfactor, PlannedJob, TempWavSpec};
use crate::model::document::Document;
use crate::model::io::{load_wav, save_wav_with, BitDepth};

/// How often the runner polls a spawned child for exit while also checking for
/// cancellation. Cheap and frequent enough that Esc feels instant.
const POLL_INTERVAL: Duration = Duration::from_millis(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobPurpose {
    Apply,
    Preview,
}

/// Everything the runner needs to execute a plan: the plan itself, the real source audio
/// (one deinterleaved channel set per input — `inputs[0]` is the selection being
/// processed, `inputs[1]` the second buffer of a dual-input process; the runner slices
/// them per `TempWavSpec.input_index`/`source_channels` when writing temp files), and
/// where to find the CDP binaries.
pub struct Job {
    pub id: u64,
    pub cdp_dir: PathBuf,
    pub planned: PlannedJob,
    pub inputs: Vec<Vec<Vec<f32>>>,
    pub input_sample_rate: u32,
    pub purpose: JobPurpose,
}

/// The audio a finished job produced. `results` holds one deinterleaved channel-set per
/// output *buffer* — almost always exactly one (the normal case: one process applied to
/// one selection). More than one only for a glob-output process
/// (`model::cdp::pipeline::GlobOutputSpec`, e.g. `distcut`/`envcut`): each numbered file it
/// produced becomes its own entry here, and the UI opens each as a separate new buffer
/// instead of splicing a single result into the current selection.
#[derive(Debug)]
pub struct JobOutput {
    pub results: Vec<Vec<Vec<f32>>>,
    pub sample_rate: u32,
    /// `Some(db)` (always negative) when `restore_clip_headroom` had to pull the result down
    /// to stay inside full scale — i.e. the process genuinely outputs louder than its input,
    /// not merely that headroom was reserved. `None` on every job that needed no reduction,
    /// including every process not on the headroom list. The UI reports it so the level change
    /// is visible rather than silent.
    pub clip_headroom_reduction_db: Option<f32>,
    /// `Some` only for a curve job (`PlannedJob.output_curve` — `IoKind::Curve`, the
    /// `repitch` pitch-curve transforms); `results` is always empty in that case, mirroring
    /// how `glob_output`/`output_files` are already mutually exclusive result shapes. The
    /// caller (UI layer) replaces an open `model::curve::PitchCurve`'s points with this
    /// rather than splicing anything into an audio `Document`.
    pub curve_points: Option<Vec<(f64, f64)>>,
    /// `Some` only when `PlannedJob.output_curve_binary_template` was set — the raw bytes
    /// of a real CDP binary pitchfile the caller should keep as the curve's new
    /// `PitchCurve.binary_template` (for chaining into a further transform, or for baking a
    /// later hand-edit back into via `model::curve::splice_pitch_wav_data`).
    pub curve_binary_template: Option<Vec<u8>>,
    /// `Some` only for a job producing a `model::formant::FormantBuffer`
    /// (`PlannedJob.output_formant_buffer` — CDP-Ext-Plan.md Phase 5's `formants get`/
    /// `oneform get`); `results` is always empty in that case, same mutual-exclusivity as
    /// `curve_points`. The raw bytes of the named temp file, verbatim — there's no
    /// text/binary split to make here the way `curve_points`/`curve_binary_template` have,
    /// since formant data has no plain-text representation at all.
    pub formant_buffer_bytes: Option<Vec<u8>>,
    /// `Some` only when `PlannedJob.output_sidecar` was set (e.g. `matrix matrix 1`'s
    /// generated-matrix-data file) — unlike `curve_points`/`formant_buffer_bytes`, this
    /// coexists with real `results`: it's a *secondary* file alongside the job's normal wav
    /// output, not the whole result. Raw bytes, verbatim; the app layer decides what to do
    /// with them (`App::tick_cdp`'s Save-As prompt).
    pub sidecar_bytes: Option<Vec<u8>>,
    /// How many samples of silence the channel-lane merge had to pad the shortest channel
    /// with to match the longest — 0 whenever the lanes agreed (the overwhelmingly common
    /// case, and always so for a `stereo_native` process, which never splits into lanes).
    ///
    /// A process CDP only implements for mono runs as one job per channel, and a
    /// length-changing one whose output length depends on the *content* (the waveset family:
    /// each channel has its own wavecycle boundaries, so "delete the quietest cycle in each
    /// group of four" removes a different number of samples from each) returns lanes of
    /// genuinely different lengths — 172157 vs 169457 samples on a two-second stereo test
    /// signal, not the sample or two of rounding the merge was originally written for. The
    /// padding keeps every sample CDP produced, but a silent tail in one channel and not the
    /// other reads as the process having broken (user report: "after process there's zeroed
    /// data in the right channel only"), so the UI names it in the command label instead of
    /// leaving it to be discovered in the waveform.
    pub lane_pad_samples: usize,
}

#[derive(Debug)]
pub enum CdpError {
    Spawn { step: String, message: String },
    NonZeroExit { step: String, code: Option<i32>, output: String },
    NoOutput { step: String },
    OutputRead { path: String, message: String },
    Cancelled,
}

pub enum CdpEvent {
    StepStarted { job: u64, index: usize, total: usize, label: String },
    Finished { job: u64, purpose: JobPurpose, result: Result<JobOutput, CdpError> },
}

/// Owns the CDP worker thread. The UI thread only ever submits jobs (fire-and-forget) and
/// drains `events` with `try_recv()` once per frame — it never blocks on a subprocess, and
/// a slow/hung CDP process never blocks the terminal.
pub struct CdpRunner {
    job_tx: Sender<Job>,
    pub events: Receiver<CdpEvent>,
    cancel: Arc<AtomicBool>,
}

impl CdpRunner {
    pub fn new() -> Self {
        let (job_tx, job_rx) = unbounded::<Job>();
        let (event_tx, event_rx) = unbounded::<CdpEvent>();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();

        thread::spawn(move || {
            for job in job_rx {
                cancel_for_thread.store(false, Ordering::Relaxed);
                let id = job.id;
                let purpose = job.purpose;
                let result = run_job(&job, &event_tx, &cancel_for_thread);
                let _ = event_tx.send(CdpEvent::Finished { job: id, purpose, result });
            }
        });

        Self { job_tx, events: event_rx, cancel }
    }

    /// Submits a job to run. Only one job should be in flight at a time in v1 (the UI shows
    /// a hard-modal "Running" dialog for the duration) — jobs queue rather than overlap if
    /// more than one is submitted, but nothing currently does that.
    pub fn submit(&self, job: Job) {
        let _ = self.job_tx.send(job);
    }

    /// Requests cancellation of the currently running job. Best-effort: takes effect at the
    /// next poll tick (`POLL_INTERVAL`), not instantly.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Default for CdpRunner {
    fn default() -> Self {
        Self::new()
    }
}

fn run_job(job: &Job, events: &Sender<CdpEvent>, cancel: &AtomicBool) -> Result<JobOutput, CdpError> {
    // The process-wide counter (not just `job.id`) keeps concurrent runners' temp dirs
    // distinct even when two jobs share an id — job ids are only unique per `App`, and the
    // test suite runs many runners in one process, where two tests reusing an id made each
    // delete the other's working files mid-run (NoOutput failures only under a parallel
    // `cargo test`, never single-threaded).
    static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!("{TEMP_DIR_PREFIX}{}-{}-{seq}", std::process::id(), job.id));
    if std::fs::create_dir_all(&temp_dir).is_err() {
        return Err(CdpError::Spawn {
            step: "setup".into(),
            message: format!("failed to create temp dir {}", temp_dir.display()),
        });
    }

    // A guard rather than a call after `run_job_body`, which is what this was. Every ordinary
    // failure inside the body is a `?`, so the plain call already covered errors and cancellation
    // — but not a panic, which unwound straight past it, leaked the directory *and* killed the
    // runner thread, after which `submit` silently no-ops for the rest of the session.
    let _guard = TempDirGuard(temp_dir.clone());
    run_job_body(job, events, cancel, &temp_dir)
}

/// Removes its directory when dropped, including while a panic unwinds.
struct TempDirGuard(std::path::PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Names every CDP job directory, so the sweep below can recognise one.
const TEMP_DIR_PREFIX: &str = "tui-wave-cdp-";

/// Deletes CDP job directories left behind by a previous run of *this* PID.
///
/// A directory only outlives its job if the process died without unwinding — a kill, a power loss,
/// a hard crash. `create_dir_all` succeeds on an existing directory, so once the OS recycles that
/// PID, a fresh run whose `job.id` and sequence counter have both restarted at 0 lands *inside*
/// the dead run's directory and can read its leftovers as its own output: `load_glob_outputs`
/// scans `out_0.wav`, `out_1.wav`, … and would happily pick up a stale one.
///
/// Matching on our own PID makes the check exact rather than heuristic — no age threshold, and no
/// chance of deleting a directory belonging to another instance that is still running.
///
/// **Call this once, at process startup, before any job can have run.** That precondition is the
/// whole basis for "anything with our PID is stale", and it is why this is called from `main` and
/// not from `CdpRunner::new`: the test suite builds many runners in one process, so a runner
/// sweeping on construction would delete the live job directories of every other runner — the
/// same PID-collision hazard that `RUN_SEQ` above exists to avoid.
pub fn sweep_stale_temp_dirs() {
    sweep_stale_temp_dirs_in(&std::env::temp_dir());
}

/// [`sweep_stale_temp_dirs`] against an explicit directory, so it can be tested without pointing
/// it at the real `$TMPDIR` — where it would delete the job directories of CDP tests running
/// concurrently in this very process. (Mirrors `Config::backup_path`'s split for the same reason.)
fn sweep_stale_temp_dirs_in(dir: &Path) {
    let own = format!("{TEMP_DIR_PREFIX}{}-", std::process::id());
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&own) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn run_job_body(
    job: &Job,
    events: &Sender<CdpEvent>,
    cancel: &AtomicBool,
    temp_dir: &Path,
) -> Result<JobOutput, CdpError> {
    write_inputs(job, temp_dir)?;
    write_brk_files(job, temp_dir)?;
    write_binary_input_files(job, temp_dir)?;

    let total = job.planned.steps.len();
    for (index, step) in job.planned.steps.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(CdpError::Cancelled);
        }

        let mut args = step.args.clone();
        resolve_deferred_window_param(job, index, temp_dir, &mut args)?;
        if index == 1 {
            resolve_matrix_gain_calibration(job, temp_dir)?;
        }

        let _ = events.send(CdpEvent::StepStarted {
            job: job.id,
            index,
            total,
            label: step.label.clone(),
        });

        run_step(&job.cdp_dir, &step.bin, &args, &step.label, temp_dir, cancel)?;

        let output_path = temp_dir.join(&step.expected_output);
        match std::fs::metadata(&output_path) {
            Ok(meta) if meta.len() > 0 => {}
            _ => return Err(CdpError::NoOutput { step: step.label.clone() }),
        }
    }

    let mut output = load_outputs(job, temp_dir)?;
    restore_clip_headroom(job, &mut output);
    Ok(output)
}

/// Undoes the `CLIP_HEADROOM_ATTENUATION` a `CLIP_HEADROOM_PROCESSES` job's inputs were
/// written with, and keeps the result inside full scale.
///
/// Two stages, and the order matters:
///
/// 1. **Restore.** Multiply by the exact inverse the planner recorded. Both factors are powers
///    of two and the samples are `f32`, so this is bit-exact — a process that never needed the
///    headroom comes back byte-identical to what it would have produced untouched. That is the
///    whole reason the attenuation can be this generous without costing dynamic range, and it
///    holds for integer source files too: `model::io::load_wav` normalizes 16/24-bit PCM to
///    `f32` by dividing by a power of two, so those sample values are exactly representable
///    and survive the round trip unchanged (verified across every representable 16- and
///    24-bit value).
///
/// 2. **Normalize only if still over.** If the true peak exceeds full scale even after
///    restoring — the process really does output louder than its input — scale down to
///    `CLIP_HEADROOM_TARGET_PEAK` and report the reduction so it is visible rather than
///    silent. A result that already fits is left at its natural level, deliberately: silently
///    normalizing every run would break level-matching against the untouched parts of the
///    document, and the point of the pre-attenuation is to stop CDP destroying peaks, not to
///    take over gain-staging.
fn restore_clip_headroom(job: &Job, output: &mut JobOutput) {
    let Some(restore) = job.planned.clip_headroom_restore else {
        return;
    };
    let restore = restore as f32;
    let mut peak: f32 = 0.0;
    for buf in &mut output.results {
        for ch in buf.iter_mut() {
            for s in ch.iter_mut() {
                *s *= restore;
                peak = peak.max(s.abs());
            }
        }
    }
    if peak > CLIP_HEADROOM_TARGET_PEAK {
        let reduction = CLIP_HEADROOM_TARGET_PEAK / peak;
        for buf in &mut output.results {
            for ch in buf.iter_mut() {
                for s in ch.iter_mut() {
                    *s *= reduction;
                }
            }
        }
        output.clip_headroom_reduction_db = Some(20.0 * reduction.log10());
    }
}

fn write_inputs(job: &Job, temp_dir: &Path) -> Result<(), CdpError> {
    // `hound` (our WAV library) writes the WAVE_FORMAT_EXTENSIBLE header for any file with
    // `bits_per_sample > 16` — i.e. every input file this app normally sends CDP, since
    // Float32 is the working format. A few older binaries can't correctly parse that header
    // (`ProcessDef.requires_simple_wav_input`'s doc comment has the full story — found via
    // `rmverb` silently corrupting audio, not erroring); for those, write plain 16-bit
    // integer PCM instead, which is exactly the condition under which hound uses the
    // simple, non-extensible `fmt ` chunk (`channels <= 2 && bits_per_sample <= 16`, true
    // for every job this app ever plans — mono or stereo).
    let bit_depth =
        if job.planned.needs_simple_wav_input { BitDepth::Int16 } else { BitDepth::Float32 };
    for spec in &job.planned.input_files {
        write_temp_wav_spec(spec, job, temp_dir, bit_depth)?;
    }
    Ok(())
}

/// Writes one `TempWavSpec` (a subset/reordering of `job.inputs`' channels, optionally
/// gain-scaled) to `temp_dir` — factored out of `write_inputs` so
/// `resolve_matrix_gain_calibration` can reuse it to rewrite the final pass's input file in
/// place, once the real gain is known, without duplicating the channel-selection logic.
fn write_temp_wav_spec(
    spec: &TempWavSpec,
    job: &Job,
    temp_dir: &Path,
    bit_depth: BitDepth,
) -> Result<(), CdpError> {
    let source = job.inputs.get(spec.input_index).map(Vec::as_slice).unwrap_or(&[]);
    let mut channels: Vec<Vec<f32>> =
        spec.source_channels.iter().map(|&ch| source.get(ch).cloned().unwrap_or_default()).collect();
    if let Some(gain) = spec.gain {
        let gain = gain as f32;
        for channel in &mut channels {
            for sample in channel {
                *sample *= gain;
            }
        }
    }
    let doc = Document { channels, sample_rate: job.input_sample_rate, ..Default::default() };
    let path = temp_dir.join(&spec.relative_name);
    save_wav_with(&doc, &path, bit_depth, false)
        .map_err(|e| CdpError::Spawn { step: format!("write {}", spec.relative_name), message: e.to_string() })
}

fn write_brk_files(job: &Job, temp_dir: &Path) -> Result<(), CdpError> {
    for (name, contents) in &job.planned.brk_files {
        std::fs::write(temp_dir.join(name), contents).map_err(|e| CdpError::Spawn {
            step: format!("write {name}"),
            message: e.to_string(),
        })?;
    }
    Ok(())
}

/// Writes a curve-transform job's raw-byte input file(s) — a binary pitch WAV already
/// spliced with a (possibly hand-edited) curve's points by `plan_curve_transform_job`
/// before this job was ever submitted. Parallel to `write_brk_files`, just for bytes
/// instead of text.
fn write_binary_input_files(job: &Job, temp_dir: &Path) -> Result<(), CdpError> {
    for (name, contents) in &job.planned.binary_input_files {
        std::fs::write(temp_dir.join(name), contents).map_err(|e| CdpError::Spawn {
            step: format!("write {name}"),
            message: e.to_string(),
        })?;
    }
    Ok(())
}

/// Patches the placeholder(s) for `PercentOfAnaWindowCount` params with their real values,
/// computed from the `.ana` file each entry's preceding `pvoc anal` step produced.
///
/// Deferred to here rather than resolved at planning time because CDP recalculates the actual
/// analysis window length from the requested overlap factor, in a way that cannot be predicted
/// before `pvoc anal` has run — so the real count only exists once that step has finished. A no-op for every job except the one process in the catalog
/// that uses this scale (`blur_blur`'s "Blurring" param). Iterates every entry matching
/// `step_index` rather than a single slot — a stereo file produces one entry per channel
/// lane (each analyzing its own `.ana` file), and patching only one of them was the bug
/// behind "blur gives an error" on stereo input: the other channel's argv kept the
/// unresolved "0" placeholder, which CDP rejects as out of range.
///
/// A constant value (`DeferredWindowTarget::Arg`) patches one argv token; an automated
/// value (`DeferredWindowTarget::BrkFile`) instead rewrites the `.brk` file's per-point
/// values in place — that file was written with placeholder values at plan time since the
/// real window count wasn't known yet. Regression fix: an envelope on this param used to
/// leave the `.brk` file holding raw 0-100 percent values, which CDP rejected as literal
/// (and far too small) window counts.
fn resolve_deferred_window_param(
    job: &Job,
    step_index: usize,
    temp_dir: &Path,
    args: &mut [String],
) -> Result<(), CdpError> {
    for deferred in &job.planned.deferred_window_params {
        if deferred.step_index != step_index {
            continue;
        }

        let ana_path = temp_dir.join(&deferred.ana_relative_name);
        let bytes = std::fs::read(&ana_path).map_err(|e| CdpError::OutputRead {
            path: ana_path.display().to_string(),
            message: e.to_string(),
        })?;
        let decfactor = parse_ana_decfactor(&bytes).ok_or_else(|| CdpError::OutputRead {
            path: ana_path.display().to_string(),
            message: "could not find decfactor in .ana header".into(),
        })?;
        let len_samples =
            job.inputs.first().and_then(|chs| chs.first()).map(|c| c.len()).unwrap_or(0);
        let window_count = window_count_from_decfactor(len_samples, decfactor);
        let scale_percent = |percent: f64| (f64::from(window_count) * percent / 100.0).max(1.0).round();

        match &deferred.target {
            crate::model::cdp::pipeline::DeferredWindowTarget::Arg { arg_index, flag, percent } => {
                let value_text = format!("{}", scale_percent(*percent));
                args[*arg_index] = match flag {
                    Some(flag) => format!("{flag}{value_text}"),
                    None => value_text,
                };
            }
            crate::model::cdp::pipeline::DeferredWindowTarget::BrkFile { relative_name, points } => {
                let contents = points
                    .iter()
                    .map(|&(t, percent)| format!("{t} {}", scale_percent(percent)))
                    .collect::<Vec<_>>()
                    .join("\n");
                let brk_path = temp_dir.join(relative_name);
                std::fs::write(&brk_path, contents).map_err(|e| CdpError::Spawn {
                    step: format!("rewrite {relative_name}"),
                    message: e.to_string(),
                })?;
            }
        }
    }
    Ok(())
}

/// `matrix_matrix_1`'s "Auto Gain Reduction" two-pass scheme (`MatrixGainCalibration`'s doc
/// comment) — called right before `steps[1]` (the first final, correctly-gained pass) runs,
/// once `steps[0]`'s safely-attenuated preview has already produced its real output.
/// Measures that preview's actual peak, computes the exact linear gain that would have
/// brought a full-scale input to `target_peak` through this same (now-fixed,
/// mode-2-reused) matrix, and rewrites every entry in `final_inputs` in place with the
/// original samples scaled by that gain — replacing the safe placeholder `write_inputs`
/// wrote there initially (before the real gain was knowable). One shared gain covers every
/// lane (mono has one entry, stereo has two independent per-channel final-pass invocations
/// sharing the one calibration — see `plan_matrix_with_gain_calibration`). A no-op whenever
/// `matrix_gain_calibration` is `None`, i.e. every job except this one process's two-pass jobs.
fn resolve_matrix_gain_calibration(job: &Job, temp_dir: &Path) -> Result<(), CdpError> {
    let Some(cal) = &job.planned.matrix_gain_calibration else { return Ok(()) };

    let preview_path = temp_dir.join(&cal.preview_output_relative_name);
    let preview = load_wav(&preview_path).map_err(|e| CdpError::OutputRead {
        path: preview_path.display().to_string(),
        message: e.to_string(),
    })?;
    let preview_peak = preview
        .channels
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |max, &s| max.max(s.abs())) as f64;

    // A silent (or all-but-silent) preview has nothing to correct for -- fall back to no
    // attenuation rather than dividing by (near) zero.
    let final_gain = if preview_peak < 1e-9 {
        1.0
    } else {
        let implied_full_scale_peak = preview_peak / cal.preview_attenuation;
        (cal.target_peak / implied_full_scale_peak).min(1.0)
    };

    let bit_depth = if job.planned.needs_simple_wav_input { BitDepth::Int16 } else { BitDepth::Float32 };
    for spec in &cal.final_inputs {
        let spec = TempWavSpec { gain: Some(final_gain), ..spec.clone() };
        write_temp_wav_spec(&spec, job, temp_dir, bit_depth)?;
    }
    Ok(())
}

fn run_step(
    cdp_dir: &Path,
    bin: &str,
    args: &[String],
    label: &str,
    temp_dir: &Path,
    cancel: &AtomicBool,
) -> Result<(), CdpError> {
    let bin_path = cdp_dir.join(super::bin_filename(bin));
    let mut child = StdCommand::new(&bin_path)
        .args(args)
        .current_dir(temp_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CdpError::Spawn { step: label.to_string(), message: e.to_string() })?;

    // Drained on helper threads so a chatty program can't deadlock us by filling a pipe
    // buffer while we're busy polling `try_wait` instead of reading.
    use std::io::Read;
    let stdout_handle = child.stdout.take().map(|mut s| {
        thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut s| {
        thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });

    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CdpError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(e) => {
                return Err(CdpError::Spawn { step: label.to_string(), message: e.to_string() })
            }
        }
    };

    let stdout_text = stdout_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr_text = stderr_handle.and_then(|h| h.join().ok()).unwrap_or_default();

    if !status.success() {
        return Err(CdpError::NonZeroExit {
            step: label.to_string(),
            code: status.code(),
            output: format!("{stdout_text}{stderr_text}"),
        });
    }
    Ok(())
}

fn load_outputs(job: &Job, temp_dir: &Path) -> Result<JobOutput, CdpError> {
    if let Some(glob) = &job.planned.glob_output {
        return load_glob_outputs(glob, job.input_sample_rate, temp_dir);
    }
    if let Some(relative_name) = &job.planned.output_curve {
        let path = temp_dir.join(relative_name);
        let text = std::fs::read_to_string(&path).map_err(|e| CdpError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        let points = crate::model::curve::parse_breakpoints(&text).map_err(|e| CdpError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        let curve_binary_template = match &job.planned.output_curve_binary_template {
            Some(relative_name) => {
                let path = temp_dir.join(relative_name);
                Some(std::fs::read(&path).map_err(|e| CdpError::OutputRead {
                    path: path.display().to_string(),
                    message: e.to_string(),
                })?)
            }
            None => None,
        };
        return Ok(JobOutput {
            clip_headroom_reduction_db: None,
            results: Vec::new(),
            sample_rate: job.input_sample_rate,
            curve_points: Some(points),
            curve_binary_template,
            formant_buffer_bytes: None,
            sidecar_bytes: None,
            lane_pad_samples: 0,
        });
    }
    if let Some(relative_name) = &job.planned.output_formant_buffer {
        let path = temp_dir.join(relative_name);
        let bytes = std::fs::read(&path).map_err(|e| CdpError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        return Ok(JobOutput {
            clip_headroom_reduction_db: None,
            results: Vec::new(),
            sample_rate: job.input_sample_rate,
            curve_points: None,
            curve_binary_template: None,
            formant_buffer_bytes: Some(bytes),
            sidecar_bytes: None,
            lane_pad_samples: 0,
        });
    }

    let max_channel = job
        .planned
        .output_files
        .iter()
        .flat_map(|spec| spec.dest_channels.iter().copied())
        .max()
        .unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); max_channel + 1];
    let mut sample_rate = job.input_sample_rate;

    for spec in &job.planned.output_files {
        let path = temp_dir.join(&spec.relative_name);
        let doc = load_wav(&path).map_err(|e| CdpError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        sample_rate = doc.sample_rate;
        for (i, &dest_ch) in spec.dest_channels.iter().enumerate() {
            channels[dest_ch] = doc.channels.get(i).cloned().unwrap_or_default();
        }
    }

    // CDP's per-channel outputs can differ — by a sample or two from rounding on most
    // processes, by a great deal more on a content-dependent length-changing one (see
    // `JobOutput::lane_pad_samples`). Pad the shorter channels with silence rather than
    // leaving the channels out of sync, and report how much was added.
    let max_len = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    let min_len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    for c in &mut channels {
        c.resize(max_len, 0.0);
    }
    let lane_pad_samples = max_len - min_len;

    let sidecar_bytes = match &job.planned.output_sidecar {
        Some(relative_name) => {
            let path = temp_dir.join(relative_name);
            Some(std::fs::read(&path).map_err(|e| CdpError::OutputRead {
                path: path.display().to_string(),
                message: e.to_string(),
            })?)
        }
        None => None,
    };

    Ok(JobOutput { clip_headroom_reduction_db: None, results: vec![channels], sample_rate, curve_points: None, curve_binary_template: None, formant_buffer_bytes: None, sidecar_bytes, lane_pad_samples })
}

/// Loads every `<prefix>N.wav` (N = 0, 1, 2, …) found in `temp_dir`, in numeric order, as
/// its own separate result — the glob-output counterpart of the normal single-result path
/// above. Stops at the first missing index (0, 1, 2, … until a gap) rather than doing a
/// directory scan + sort, since CDP always numbers this family of outputs contiguously
/// from 0 and `run_job_body` already confirmed index 0 exists before calling here.
fn load_glob_outputs(
    glob: &crate::model::cdp::pipeline::GlobOutputSpec,
    fallback_sample_rate: u32,
    temp_dir: &Path,
) -> Result<JobOutput, CdpError> {
    let mut results = Vec::new();
    let mut sample_rate = fallback_sample_rate;
    for index in 0.. {
        let path = temp_dir.join(format!("{}{index}.wav", glob.prefix));
        if !path.exists() {
            break;
        }
        let doc = load_wav(&path).map_err(|e| CdpError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        sample_rate = doc.sample_rate;
        results.push(doc.channels);
    }
    if results.is_empty() {
        return Err(CdpError::NoOutput { step: format!("{}0.wav", glob.prefix) });
    }
    Ok(JobOutput { clip_headroom_reduction_db: None, results, sample_rate, curve_points: None, curve_binary_template: None, formant_buffer_bytes: None, sidecar_bytes: None, lane_pad_samples: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::pipeline::{Invocation, OutputWavSpec, TempWavSpec};
    use std::time::Instant;

    /// The startup sweep removes this PID's leftover job directories and nothing else.
    ///
    /// Run against a scratch directory rather than the real `$TMPDIR`: the sweep matches on our
    /// own PID, and the CDP tests in this very process are creating live job directories under
    /// that same PID as this runs.
    #[test]
    fn the_startup_sweep_removes_only_this_processs_own_leftovers() {
        let scratch = std::env::temp_dir()
            .join(format!("tui_wave_sweep_test_{}_{:p}", std::process::id(), &"sweep"));
        std::fs::create_dir_all(&scratch).unwrap();

        // Ours, from a hypothetical earlier run of this PID — must go.
        let stale = scratch.join(format!("{TEMP_DIR_PREFIX}{}-7-3", std::process::id()));
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("out.wav"), b"leftover").unwrap();

        // Another instance's, still running — must survive, or a sweep on one instance's startup
        // would pull the floor out from under another's job.
        let other = scratch.join(format!("{TEMP_DIR_PREFIX}{}-0-0", std::process::id() + 1));
        std::fs::create_dir_all(&other).unwrap();
        // And something that simply is not ours.
        let unrelated = scratch.join("someone-elses-work");
        std::fs::create_dir_all(&unrelated).unwrap();

        sweep_stale_temp_dirs_in(&scratch);

        assert!(!stale.exists(), "our own leftover job directory must be removed");
        assert!(other.exists(), "another live instance's directory must be left alone");
        assert!(unrelated.exists(), "unrelated directories must be left alone");

        std::fs::remove_dir_all(&scratch).ok();
    }

    fn recv_finished(runner: &CdpRunner, timeout: Duration) -> CdpEvent {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(event) = runner.events.try_recv() {
                if matches!(event, CdpEvent::Finished { .. }) {
                    return event;
                }
                // StepStarted events are fine to skip past in these tests.
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for CdpEvent::Finished");
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Runs one job synchronously to completion and returns its `JobOutput` — used by
    /// `catalog_smoke_test` to produce a real Formant/Snapshot buffer up front (via
    /// `plan_extract_formants`/`plan_oneform_get`) before driving any catalog entry with a
    /// `FormantBufferRef` param, since a fake byte blob would fail as an unparseable formant
    /// file rather than exercising the argv shape the smoke test actually cares about.
    fn run_smoke_prereq_job(
        runner: &CdpRunner,
        cdp_dir: &Path,
        planned: PlannedJob,
        inputs: Vec<Vec<Vec<f32>>>,
        sample_rate: u32,
        id: u64,
    ) -> JobOutput {
        runner.submit(Job {
            id,
            cdp_dir: cdp_dir.to_path_buf(),
            planned,
            inputs,
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(runner, Duration::from_secs(30)) else {
            unreachable!()
        };
        result.expect("smoke-test prerequisite formant/snapshot extraction job should succeed")
    }

    fn empty_planned_job(steps: Vec<Invocation>, output_relative_name: &str) -> PlannedJob {
        PlannedJob {
            steps,
            input_files: vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0], gain: None }],
            output_files: vec![OutputWavSpec {
                relative_name: output_relative_name.into(),
                dest_channels: vec![0],
            }],
            glob_output: None,
            output_curve: None,
            output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
            brk_files: Vec::new(),
            binary_input_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: false, clip_headroom_restore: None,
        }
    }

    #[test]
    fn fake_copy_step_round_trips_audio() {
        // Uses /bin/cp as a stand-in for a real CDP binary -- validates spawn/poll/exit/
        // output-loading without depending on the actual CDP install.
        let steps = vec![Invocation {
            bin: "cp".into(),
            args: vec!["in.wav".into(), "out.wav".into()],
            label: "copy".into(),
            expected_output: "out.wav".into(),
        }];
        let planned = empty_planned_job(steps, "out.wav");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 1,
            cdp_dir: PathBuf::from("/bin"),
            planned,
            inputs: vec![vec![vec![0.1, 0.2, -0.3, 0.4]]],
            input_sample_rate: 44100,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(5))
        else {
            unreachable!()
        };
        let output = result.expect("job should succeed");
        assert_eq!(output.sample_rate, 44100);
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0][0].len(), 4);
    }

    /// A glob-output job (`PlannedJob.glob_output`, e.g. distcut/envcut) loads every
    /// numbered `<prefix>N.wav` it finds, in order, as its own separate `results` entry —
    /// exercised with a fake shell step that writes three numbered copies of the input
    /// (standing in for CDP writing an unpredictable number of segments) rather than
    /// depending on a real CDP install, matching `fake_copy_step_round_trips_audio`'s own
    /// "no real CDP needed" precedent for pure runner-mechanics tests.
    #[test]
    fn glob_output_job_loads_every_numbered_file_as_a_separate_result() {
        let steps = vec![Invocation {
            bin: "sh".into(),
            args: vec![
                "-c".into(),
                "cp in.wav cutout0.wav && cp in.wav cutout1.wav && cp in.wav cutout2.wav".into(),
            ],
            label: "fake distcut".into(),
            expected_output: "cutout0.wav".into(),
        }];
        let planned = PlannedJob {
            steps,
            input_files: vec![TempWavSpec {
                relative_name: "in.wav".into(),
                input_index: 0,
                source_channels: vec![0],
                gain: None,
            }],
            output_files: Vec::new(),
            glob_output: Some(crate::model::cdp::pipeline::GlobOutputSpec { prefix: "cutout".into() }),
            output_curve: None,
            output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
            brk_files: Vec::new(),
            binary_input_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: false, clip_headroom_restore: None,
        };

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 5,
            cdp_dir: PathBuf::from("/bin"),
            planned,
            inputs: vec![vec![vec![0.1, 0.2, -0.3, 0.4]]],
            input_sample_rate: 44100,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(5))
        else {
            unreachable!()
        };
        let output = result.expect("job should succeed");
        assert_eq!(output.sample_rate, 44100);
        assert_eq!(output.results.len(), 3, "expected one result per numbered file");
        for segment in &output.results {
            assert_eq!(segment[0].len(), 4, "each copied segment should round-trip the same 4 samples");
        }
    }

    /// A curve job (`PlannedJob.output_curve`, e.g. `repitch invert`) writes its curve as a
    /// plain text file (via `brk_files`, same mechanism envelope params already use) and
    /// reads the result back as points rather than audio -- exercised with a fake shell
    /// step (standing in for a real `repitch` invocation) that just copies the input file
    /// to the expected output name, matching this file's established "no real CDP needed
    /// for pure runner-mechanics tests" precedent.
    #[test]
    fn curve_job_reads_the_result_back_as_points_not_audio() {
        let steps = vec![Invocation {
            bin: "cp".into(),
            args: vec!["curve_in.txt".into(), "curve_out.txt".into()],
            label: "fake repitch invert".into(),
            expected_output: "curve_out.txt".into(),
        }];
        let planned = PlannedJob {
            steps,
            input_files: Vec::new(),
            output_files: Vec::new(),
            glob_output: None,
            output_curve: Some("curve_out.txt".into()),
            output_curve_binary_template: None, output_formant_buffer: None, output_sidecar: None, matrix_gain_calibration: None,
            brk_files: vec![("curve_in.txt".into(), "0 220\n1 440".into())],
            binary_input_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: false, clip_headroom_restore: None,
        };

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 6,
            cdp_dir: PathBuf::from("/bin"),
            planned,
            inputs: Vec::new(),
            input_sample_rate: 44100,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(5))
        else {
            unreachable!()
        };
        let output = result.expect("job should succeed");
        assert!(output.results.is_empty(), "a curve job never produces spliceable audio");
        assert_eq!(output.curve_points, Some(vec![(0.0, 220.0), (1.0, 440.0)]));
    }

    #[test]
    fn missing_binary_reports_spawn_error() {
        let steps = vec![Invocation {
            bin: "this-binary-does-not-exist".into(),
            args: vec!["in.wav".into(), "out.wav".into()],
            label: "missing".into(),
            expected_output: "out.wav".into(),
        }];
        let planned = empty_planned_job(steps, "out.wav");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 2,
            cdp_dir: PathBuf::from("/nonexistent-cdp-dir"),
            planned,
            inputs: vec![vec![vec![0.0; 4]]],
            input_sample_rate: 44100,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(5))
        else {
            unreachable!()
        };
        assert!(matches!(result, Err(CdpError::Spawn { .. })));
    }

    #[test]
    fn nonzero_exit_is_reported_with_captured_output() {
        // /bin/sh -c 'exit 1' always fails regardless of args, standing in for a CDP
        // binary that rejects out-of-range parameters.
        let steps = vec![Invocation {
            bin: "false".into(),
            args: vec![],
            label: "deliberately fails".into(),
            expected_output: "out.wav".into(),
        }];
        let planned = empty_planned_job(steps, "out.wav");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 3,
            cdp_dir: PathBuf::from("/bin"),
            planned,
            inputs: vec![vec![vec![0.0; 4]]],
            input_sample_rate: 44100,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(5))
        else {
            unreachable!()
        };
        assert!(matches!(result, Err(CdpError::NonZeroExit { .. })));
    }

    #[test]
    fn cancel_stops_a_long_running_step() {
        let steps = vec![Invocation {
            bin: "sleep".into(),
            args: vec!["30".into()],
            label: "sleeping".into(),
            expected_output: "out.wav".into(),
        }];
        let planned = empty_planned_job(steps, "out.wav");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 4,
            cdp_dir: PathBuf::from("/bin"),
            planned,
            inputs: vec![vec![vec![0.0; 4]]],
            input_sample_rate: 44100,
            purpose: JobPurpose::Apply,
        });

        // Give the job a moment to actually spawn the sleeping child before cancelling.
        thread::sleep(Duration::from_millis(100));
        runner.cancel();

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(5))
        else {
            unreachable!()
        };
        assert!(matches!(result, Err(CdpError::Cancelled)));
    }

    // -- Gated integration tests against real CDP binaries -------------------------------
    //
    // This is a binary-only crate (no `lib.rs`, so no external `tests/*.rs` can link against
    // it) -- every test in the project is inline like this, referencing `tests/fixtures/`
    // by relative path. These are gated on finding a real CDP install rather than `#[ignore]`
    // so they still run automatically whenever the `cdp/` directory is present (as it is in
    // this checkout), while staying green on any other machine/CI without it.

    fn real_cdp_dir() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("TUI_WAVE_CDP_DIR") {
            let path = PathBuf::from(dir);
            if crate::cdp::validate_cdp_dir(&path).is_ok() {
                return Some(path);
            }
        }
        let fallback = Path::new(env!("CARGO_MANIFEST_DIR")).join("cdp");
        crate::cdp::validate_cdp_dir(&fallback).ok().map(|_| fallback)
    }

    macro_rules! require_cdp {
        () => {
            match real_cdp_dir() {
                Some(dir) => dir,
                None => {
                    eprintln!(
                        "skipping: no real CDP install found (set TUI_WAVE_CDP_DIR or place binaries in ./cdp)"
                    );
                    return;
                }
            }
        };
    }

    fn mono_sine_channels() -> (Vec<Vec<f32>>, u32) {
        let doc = crate::model::io::load_wav("tests/fixtures/mono_sine.wav").unwrap();
        (doc.channels, doc.sample_rate)
    }

    #[test]
    fn modify_speed_2_transposes_by_semitones_end_to_end() {
        // `modify speed 2` is semitone transposition, not a speed multiplier -- mode 1
        // (plain multiplier) isn't in the SoundThread-derived catalog. Duration scales as
        // 2^(-semitones/12); +12 semitones (one octave up) gives an exact half-duration,
        // discovered by the Phase 0 spike getting a non-obvious ratio (0.891 for 2
        // semitones) that only made sense once re-read against CDP's own usage text.
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, warnings) = crate::model::cdp::CdpCatalog::load(None);
        assert!(warnings.is_empty());
        let def = catalog.find("modify_speed_2").expect("modify_speed_2 in catalog");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &[crate::model::cdp::ParamValue::Number(12.0)],
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 100,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("modify speed 2 should succeed on a real CDP install");
        assert_eq!(output.results.len(), 1);
        let ratio = output.results[0][0].len() as f64 / len_samples as f64;
        assert!((ratio - 0.5).abs() < 0.05, "expected ~half duration at +12 semitones, got ratio {ratio}");
    }

    /// Real end-to-end coverage for the flat variadic-input kind (`IoKind::VariadicWav`):
    /// `plan_variadic_wav` -> `CdpRunner` -> the real `pulser` binary, with **two distinct**
    /// inputs (a sine and a detuned copy) rather than one buffer duplicated. `pulser multi`
    /// is `MANY_SNDFILES` internally, so a single file is rejected outright ("Insufficient
    /// input files for this process") -- which makes this the one thing a single-input fake
    /// job could never have caught: that `plan_variadic_wav` emits every picked file, in
    /// order, as its own `in_N.wav`.
    #[test]
    fn pulser_multi_1_runs_with_two_real_inputs_end_to_end() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();
        // A second, genuinely different source: the same sine resampled by nearest-neighbour
        // to shift its pitch. Cheap, and enough that CDP is reading two real files rather
        // than the same bytes twice.
        let detuned: Vec<Vec<f32>> = vec![(0..len_samples)
            .map(|i| channels[0][(i * 3 / 2).min(len_samples - 1)])
            .collect()];

        let (catalog, warnings) = crate::model::cdp::CdpCatalog::load(None);
        assert!(warnings.is_empty(), "catalog failed to parse: {warnings:?}");
        let def = catalog.find("pulser_multi_1").expect("pulser_multi_1 in catalog");
        assert_eq!(def.input, crate::model::cdp::IoKind::VariadicWav);
        assert_eq!(def.input_arity().0, 2, "pulser multi rejects fewer than 2 input files");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            &[input.clone(), input.clone()],
            &crate::model::cdp::PvocSettings::default(),
        )
        .expect("plan_job should accept two inputs for a variadic process");

        // The argv shape is the thing under test as much as the exit code: both inputs must
        // appear, in order, before the outfile.
        let args = &planned.steps[0].args;
        let in1 = args.iter().position(|a| a == "in_1.wav").expect("in_1.wav in argv");
        let in2 = args.iter().position(|a| a == "in_2.wav").expect("in_2.wav in argv");
        let out = args.iter().position(|a| a == "out.wav").expect("out.wav in argv");
        assert!(in1 < in2 && in2 < out, "expected in_1 in_2 out ordering, got {args:?}");
        assert_eq!(planned.input_files.len(), 2);
        assert_eq!(planned.input_files[1].input_index, 1, "second temp file reads input 1");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 140,
            cdp_dir,
            planned,
            inputs: vec![channels, detuned],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
        else {
            unreachable!()
        };
        let output = result.expect("pulser multi 1 should succeed with two real mono inputs");
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].len(), 1, "pulser multi 1 emits mono");
        assert!(!output.results[0][0].is_empty(), "output should contain audio");
    }

    /// `fastconv`'s settings must actually reach the binary. Its flags are parsed
    /// getopt-style *before* the filenames (`ProcessDef.flags_before_infile`), and with them
    /// trailing it silently ignored `-a`, `-f` **and** the positional dry/wet value all at
    /// once -- so every combination of settings produced a byte-identical, clipped, integer
    /// result. That is exactly what the user reported ("still heavily clips and sounds the
    /// same no matter what the settings are"), and nothing errored, so only running the real
    /// binary and comparing two differently-configured outputs catches it.
    #[test]
    fn fastconv_settings_actually_change_the_convolved_output() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();
        // A decaying noise burst -- a plausible reverb IR, and deliberately *not* derived
        // from the sine: convolving a sine with a sine-shaped impulse just returns a sine, so
        // the wet and dry signals would have nearly the same shape and the comparison below
        // could not tell a working dry/wet mix from a broken one. Seeded LCG, so the test is
        // deterministic.
        let mut rng: u32 = 0x1234_5678;
        let impulse: Vec<Vec<f32>> = vec![(0..len_samples / 4)
            .map(|i| {
                rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (rng >> 8) as f32 / (1 << 23) as f32 - 1.0;
                noise * (-6.0 * i as f32 / (len_samples / 4) as f32).exp() * 0.5
            })
            .collect()];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("fastconv_fastconv").expect("fastconv in catalog");
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };

        let run = |dry: f64, amp: f64, id: u64| -> Vec<f32> {
            let mut values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
            values[0] = crate::model::cdp::ParamValue::Number(dry);
            values[1] = crate::model::cdp::ParamValue::Number(amp);
            let planned = crate::model::cdp::plan_job(
                def,
                &values,
                &[input.clone(), input.clone()],
                &crate::model::cdp::PvocSettings::default(),
            )
            .expect("plans");
            let first_file = planned.steps[0].args.iter().position(|a| a.ends_with(".wav")).unwrap();
            assert!(
                planned.steps[0].args[..first_file].iter().all(|a| a.starts_with('-')),
                "flags must precede the filenames: {:?}",
                planned.steps[0].args
            );
            let runner = CdpRunner::new();
            runner.submit(Job {
                id,
                cdp_dir: cdp_dir.clone(),
                planned,
                inputs: vec![channels.clone(), impulse.clone()],
                input_sample_rate: sample_rate,
                purpose: JobPurpose::Apply,
            });
            let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
            else {
                unreachable!()
            };
            result.expect("fastconv should run").results[0][0].clone()
        };

        let wet = run(0.0, 1.0, 310);
        let half = run(0.9, 1.0, 311);
        assert!(!wet.is_empty() && !half.is_empty(), "both runs produce audio");

        // Mixing in nearly all dry signal has to change the result. Compared on the
        // normalized shape rather than raw level, so the assertion can't be satisfied by the
        // headroom stage's own gain scaling alone.
        let normalize = |v: &[f32]| -> Vec<f32> {
            let peak = v.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(f32::MIN_POSITIVE);
            v.iter().map(|s| s / peak).collect()
        };
        let (a, b) = (normalize(&wet), normalize(&half));
        let n = a.len().min(b.len());
        let worst = (0..n).fold(0.0f32, |m, i| m.max((a[i] - b[i]).abs()));
        assert!(worst > 0.05, "the dry/wet mix must change the output shape (worst diff {worst})");

        // And the result stays inside full scale -- the clipping half of the same report.
        let peak = wet.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= 1.0001, "convolution output must not exceed full scale (peak {peak})");
    }

    /// A mono-only, content-dependent length-changing process run on stereo returns lanes of
    /// genuinely different lengths, and the merge pads the shorter one with silence — the
    /// user-visible symptom being "after process there's zeroed data in the right channel
    /// only" (Waveset Thin). The padding itself is deliberate (it keeps every sample CDP
    /// produced); what this pins down is that the amount is *reported*, so the UI can name it
    /// rather than leaving a silent tail to be discovered in the waveform.
    #[test]
    fn a_waveset_process_on_stereo_reports_how_much_lane_padding_it_needed() {
        let cdp_dir = require_cdp!();
        let sr = 44100u32;
        let n = (sr * 2) as usize;
        // The two channels must be genuinely different material: identical channels produce
        // identical wavecycle boundaries and therefore identical lane lengths, which is
        // exactly the case this bug hides in.
        let mut seed: u32 = 7;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
        };
        let tone = |hz: f32, i: usize| 0.4 * (2.0 * std::f32::consts::PI * hz * i as f32 / sr as f32).sin();
        let left: Vec<f32> = (0..n).map(|i| tone(220.0, i) + 0.05 * noise()).collect();
        let right: Vec<f32> = (0..n).map(|i| tone(331.0, i) + 0.05 * noise()).collect();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("distort_delete_3").expect("Waveset Thin (Drop Weakest) in catalog");
        assert!(!def.stereo_native, "the premise: CDP only runs this mono, so the app splits into lanes");
        let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let input = crate::model::cdp::InputSpec { channels: 2, sample_rate: sr, len_samples: n, ..Default::default() };
        let planned = crate::model::cdp::plan_job(def, &values, &[input], &crate::model::cdp::PvocSettings::default()).unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 320,
            cdp_dir,
            planned,
            inputs: vec![vec![left, right]],
            input_sample_rate: sr,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(120))
        else {
            unreachable!()
        };
        let out = result.expect("Waveset Thin should run on both lanes");

        let channels = &out.results[0];
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].len(), channels[1].len(), "merged channels must stay the same length");
        // One channel really does end early: its silent tail is the padding, and its length
        // must be exactly what the job reported.
        let audible = |ch: &[f32]| ch.iter().rposition(|s| *s != 0.0).map(|p| p + 1).unwrap_or(0);
        let pad = channels[0].len() - audible(&channels[0]).min(audible(&channels[1]));
        assert_eq!(out.lane_pad_samples, pad, "the reported padding must match the real silent tail");
        assert!(out.lane_pad_samples > 0, "this material is meant to make the lanes diverge");
    }

    /// Remove DC Offset with "process channels separately" on, against the real binary: each
    /// channel must come back with *its own* offset removed. A stereo file whose channels are
    /// offset in opposite directions is the case a single whole-file shift cannot fix, and the
    /// one the option exists for.
    #[test]
    fn dc_offset_per_channel_removes_each_channels_own_offset() {
        let cdp_dir = require_cdp!();
        let (mono, sample_rate) = mono_sine_channels();
        let n = mono[0].len();
        // Opposite offsets: no single shift removes both, and their average is ~0, so a
        // whole-file run would leave both channels almost exactly as they started.
        let left: Vec<f32> = mono[0].iter().map(|s| s * 0.5 + 0.02).collect();
        let right: Vec<f32> = mono[0].iter().map(|s| s * 0.5 - 0.03).collect();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("housekeep_extract_4").expect("Remove DC Offset in catalog");
        let mean = |ch: &[f32]| ch.iter().map(|s| *s as f64).sum::<f64>() / ch.len() as f64;
        let values = vec![
            crate::model::cdp::ParamValue::Number(-mean(&left)),
            crate::model::cdp::ParamValue::Number(-mean(&right)),
            crate::model::cdp::ParamValue::Toggle(true),
        ];
        let input = crate::model::cdp::InputSpec { channels: 2, sample_rate, len_samples: n, ..Default::default() };
        let planned = crate::model::cdp::plan_job(def, &values, &[input], &crate::model::cdp::PvocSettings::default()).unwrap();
        assert_eq!(planned.steps.len(), 2, "one CDP run per channel");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 330,
            cdp_dir,
            planned,
            inputs: vec![vec![left.clone(), right.clone()]],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
        else {
            unreachable!()
        };
        let out = result.expect("Remove DC Offset should run per channel");
        let channels = &out.results[0];
        assert_eq!(channels.len(), 2);
        for (i, (before, after)) in [(&left, &channels[0]), (&right, &channels[1])].iter().enumerate() {
            let before_offset = mean(before).abs();
            let after_offset = mean(after).abs();
            assert!(
                after_offset < before_offset / 10.0,
                "channel {i}: offset {before_offset:.5} should be largely removed, got {after_offset:.5}"
            );
        }
    }

    /// `scramble`'s per-segment modes take their cuts datafile from the Head/Tail marks — the
    /// real binary must accept the file this produces, in that argv slot, with an odd number
    /// of times in it (every mark is its own cut, unlike the DISTMORE marklist).
    #[test]
    fn scramble_per_segment_runs_with_cut_times_from_head_tail_marks() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("scramble_scramble_5").expect("scramble mode 5 in catalog");
        let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        // Three marks: an odd count, which the paired DISTMORE reading would truncate.
        let input = crate::model::cdp::InputSpec {
            channels: 1,
            sample_rate,
            len_samples,
            head_tail_marks: vec![len_samples / 4, len_samples / 2, len_samples * 3 / 4],
        };
        let planned = crate::model::cdp::plan_job(def, &values, &[input], &crate::model::cdp::PvocSettings::default())
            .expect("three marks is three cut times");
        let (_, cuts) = planned.brk_files.iter().find(|(n, _)| n == "headstails.txt").unwrap();
        assert_eq!(cuts.lines().count(), 3, "all three marks reach CDP: {cuts:?}");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 340,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
        else {
            unreachable!()
        };
        let out = result.expect("scramble per-segment should accept a cuts file from the marks");
        assert!(!out.results[0][0].is_empty(), "output should contain audio");
    }

    /// Real end-to-end coverage for `tesselate` -- the one process needing a `transposed`
    /// table (`ParamKind::Table.transposed`). Its datafile must have exactly two lines with
    /// one entry per input file; getting that wrong is silent in a fake job and a hard
    /// "doesn't correspond to no of input files" error against the real binary, so this runs
    /// with two inputs and a two-row table to actually exercise the transpose.
    #[test]
    fn tesselate_transposed_datafile_matches_input_count_end_to_end() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();
        let other: Vec<Vec<f32>> = vec![channels[0].iter().rev().copied().collect()];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("tesselate_tesselate").expect("tesselate_tesselate in catalog");
        assert_eq!(def.input, crate::model::cdp::IoKind::VariadicWav);

        // Two rows (one per input file); entry delays must differ and stay under Cycle
        // Duration, both of which CDP enforces.
        let mut values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        values[0] = crate::model::cdp::ParamValue::Table(vec![vec![4.0, 0.0], vec![4.0, 0.2]]);

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            &[input.clone(), input.clone()],
            &crate::model::cdp::PvocSettings::default(),
        )
        .expect("plan_job should accept two inputs");

        // Two lines, two entries each -- the transpose, not the row-per-line default.
        let (_, table) = planned
            .brk_files
            .iter()
            .find(|(name, _)| name.starts_with("table_"))
            .expect("tesselate writes a table datafile");
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 2, "tesselate's datafile is exactly two lines: {table:?}");
        assert_eq!(lines[0], "4 4", "line 1 is every source's resync count");
        assert_eq!(lines[1], "0 0.2", "line 2 is every source's entry delay");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 141,
            cdp_dir,
            planned,
            inputs: vec![channels, other],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
        else {
            unreachable!()
        };
        let output = result.expect("tesselate should succeed with a matching 2-row datafile");
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].len(), 2, "tesselate at chans=2 emits stereo");
    }

    /// Three real inputs through `tesselate` with an auto-sized Sources table — the shape the
    /// UI now produces for a three-buffer pick. Before the table tracked the pick, this failed
    /// with "No of data items (1) in 1st line of file table_0.txt doesn't correspond to no of
    /// input files (3)" for every pick above one source, i.e. the process could never do the
    /// thing it exists for (user report, 2026-07-27).
    #[test]
    fn tesselate_runs_with_three_inputs_and_a_matching_auto_sized_table() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();
        let reversed: Vec<Vec<f32>> = vec![channels[0].iter().rev().copied().collect()];
        let quiet: Vec<Vec<f32>> = vec![channels[0].iter().map(|s| s * 0.4).collect()];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("tesselate_tesselate").expect("tesselate_tesselate in catalog");
        let sources = def
            .params
            .iter()
            .find(|p| p.name == "Sources")
            .expect("a Sources param");
        assert!(sources.rows_match_input_count, "its row count tracks the input count");

        // Exactly what `App::sync_cdp_table_to_input_count` builds for three inputs: the
        // column defaults, with the `must_be_distinct` Entry Delay staggered by one step.
        let mut values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        values[0] = crate::model::cdp::ParamValue::Table(vec![
            vec![4.0, 0.0],
            vec![4.0, 0.01],
            vec![4.0, 0.02],
        ]);

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            &[input.clone(), input.clone(), input.clone()],
            &crate::model::cdp::PvocSettings::default(),
        )
        .expect("three inputs with three rows should plan");

        let (_, table) = planned
            .brk_files
            .iter()
            .find(|(name, _)| name.starts_with("table_"))
            .expect("tesselate writes a table datafile");
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 2, "always exactly two lines: {table:?}");
        for line in &lines {
            assert_eq!(
                line.split_whitespace().count(),
                3,
                "one entry per input file, which is what CDP checks: {table:?}"
            );
        }

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 420,
            cdp_dir,
            planned,
            inputs: vec![channels, reversed, quiet],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
        else {
            unreachable!()
        };
        let output = result.expect("tesselate should accept three inputs and a three-row table");
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].len(), 2, "chans=2 emits stereo");
        assert!(output.results[0][0].iter().any(|s| s.abs() > 1e-3), "and real audio");
    }

    /// Real end-to-end coverage for the channel-grouped variadic kind
    /// (`IoKind::GroupedWav`) plus the variadic *glob* output path: `repair repair` writes
    /// `out_0.wav`, `out_1.wav`, … rather than one `out.wav`, so this checks both that the
    /// numbered set is what `GlobOutputSpec`'s `"out_"` prefix scans for and that each
    /// numbered file really is the interleaving of its two positionally-paired mono sources.
    #[test]
    fn repair_grouped_inputs_interleave_into_numbered_stereo_files_end_to_end() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();
        // Distinguishable right-channel source: the same sine at half amplitude. Same length
        // as the left source, which `repair` requires ("FILES 0 AND 1 ARE NOT THE SAME SIZE").
        let quiet: Vec<Vec<f32>> = vec![channels[0].iter().map(|s| s * 0.25).collect()];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("repair_repair").expect("repair_repair in catalog");
        assert_eq!(def.input, crate::model::cdp::IoKind::GroupedWav);
        assert_eq!(def.output, crate::model::cdp::IoKind::WavGlob);

        let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };

        // An odd count has no valid channel-1/channel-2 split -- rejected before CDP is even
        // spawned, with the group wording rather than CDP's "not a multiple of 2".
        let odd = crate::model::cdp::plan_job(def, &values, &[input.clone()], &crate::model::cdp::PvocSettings::default());
        assert!(
            matches!(odd, Err(crate::model::cdp::PlanError::VariadicInputCount { .. })),
            "one input file is not a valid grouped pick: {odd:?}"
        );

        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            &[input.clone(), input.clone()],
            &crate::model::cdp::PvocSettings::default(),
        )
        .expect("two inputs is one channel-1 source and one channel-2 source");
        assert_eq!(
            planned.glob_output.as_ref().map(|g| g.prefix.as_str()),
            Some("out_"),
            "repair's numbered outputs are out_0.wav, out_1.wav, ..."
        );
        assert!(planned.output_files.is_empty(), "a glob job has no single output file");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 142,
            cdp_dir,
            planned,
            inputs: vec![channels.clone(), quiet.clone()],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
        else {
            unreachable!()
        };
        let output = result.expect("repair should join two mono sources into one stereo file");
        assert_eq!(output.results.len(), 1, "one group of two sources produces one output file");
        let joined = &output.results[0];
        assert_eq!(joined.len(), 2, "the joined file is stereo");
        // The whole point of `repair` -- so assert the actual interleaving, not just the
        // channel count. Left is the first channel-1 source, right the first channel-2 one.
        let n = joined[0].len().min(64);
        for i in 0..n {
            assert!((joined[0][i] - channels[0][i]).abs() < 1e-3, "left channel at {i}");
            assert!((joined[1][i] - quiet[0][i]).abs() < 1e-3, "right channel at {i}");
        }
    }

    /// Builds a `crystal_rotate_*` value list from the catalog's own defaults with the
    /// handful of params these tests pin overridden by name — the datafile, both rotation
    /// speeds (zeroed, so the crystal is stationary and the result is exactly periodic), the
    /// time width (minimised, so a group's events are effectively simultaneous), the time
    /// step, the output duration, and a near-unison pitch range (CDP rejects an exactly zero
    /// range). Overriding by name rather than by index keeps these tests from silently
    /// testing the wrong parameter if the catalog entry ever gains or reorders one.
    fn crystal_values(
        def: &crate::model::cdp::ProcessDef,
        vertices: Vec<[f64; 3]>,
        envelope: Vec<(f64, f64)>,
        duration: f64,
    ) -> Vec<crate::model::cdp::ParamValue> {
        use crate::model::cdp::ParamValue;
        def.params
            .iter()
            .map(|p| match p.name.as_str() {
                "Crystal Data" => ParamValue::CrystalVdat(crate::model::cdp::CrystalVdat {
                    vertices: vertices.clone(),
                    envelope: envelope.clone(),
                }),
                "Rotation XY" | "Rotation XZ" => ParamValue::Number(0.0),
                "Time Width" => ParamValue::Number(0.01),
                "Time Step" => ParamValue::Number(1.0),
                "Output Duration" => ParamValue::Number(duration),
                "Min Pitch" => ParamValue::Number(60.0),
                "Max Pitch" => ParamValue::Number(61.0),
                _ => p.kind.default_value(),
            })
            .collect()
    }

    /// The compound VDAT datafile (`ParamKind::CrystalVdat`) end to end against the real
    /// `crystal` binary, asserting the *shape of the generated sound* rather than an exit
    /// code — which is the only way to know both halves of that one datafile were parsed the
    /// way this app intends.
    ///
    /// With the crystal held still (both rotation speeds 0), a single vertex at the origin,
    /// a minimal time width and a 1-second time step, the process must emit one event per
    /// second, each shaped by the 0.5-second envelope in section 2 of the datafile. So the
    /// output is an exactly periodic pulse train: loud a quarter-second into every second
    /// (the envelope's peak) and *silent* three-quarters of the way in (past the end of that
    /// event, before the next). That pattern is impossible unless the vertex section, the
    /// envelope section and the boundary between them were all read correctly — in
    /// particular, writing the envelope 3 numbers to a line instead of 2 makes CDP read it
    /// back as extra vertices and fail outright.
    #[test]
    fn crystal_rotate_mono_generates_an_enveloped_event_train_from_the_real_vdat_file() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, warnings) = crate::model::cdp::CdpCatalog::load(None);
        assert!(warnings.is_empty(), "catalog failed to parse: {warnings:?}");
        let def = catalog.find("crystal_rotate_1").expect("crystal_rotate_1 in catalog");
        assert_eq!(def.input, crate::model::cdp::IoKind::VariadicWav);
        assert!(!def.output_is_stereo, "mode 1 is the mono mode");

        let values = crystal_values(def, vec![[0.0, 0.0, 0.0]], vec![(0.0, 0.0), (0.25, 1.0), (0.5, 0.0)], 5.0);
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .expect("one input file is enough for crystal (ONE_OR_MANY_SNDFILES)");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 160,
            cdp_dir,
            planned,
            inputs: vec![channels.clone()],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60)) else {
            unreachable!()
        };
        let output = result.expect("crystal rotate 1 should accept the generated vdat file");
        assert_eq!(output.results.len(), 1);
        let mono = &output.results[0];
        assert_eq!(mono.len(), 1, "mode 1 writes a mono file");

        let sr = sample_rate as usize;
        let peak_at = |secs: f64| {
            let start = (secs * sr as f64) as usize;
            mono[0][start..(start + sr / 20).min(mono[0].len())]
                .iter()
                .fold(0.0f32, |acc, s| acc.max(s.abs()))
        };
        // Four full one-second periods fit inside the 5-second output (the last event starts
        // at t=4 and its envelope runs out at 4.5).
        for k in 0..4 {
            let loud = peak_at(k as f64 + 0.25);
            let quiet = peak_at(k as f64 + 0.75);
            assert!(loud > 0.1, "event {k} should peak a quarter-second in, got {loud}");
            assert!(quiet < 1e-4, "the gap after event {k} should be silent, got {quiet}");
        }
    }

    /// The DISTMORE family end-to-end against the real binaries, for both argv shapes in it:
    /// `distmore bright` (marklist *plus* trailing flagged params) and `distmore segsbkwd`
    /// (marklist and nothing else). Both take the marklist as a positional immediately after
    /// the outfile — `distmore bright 1-3 infile outfile marklist [-s… -d]` — which is the one
    /// thing most likely to be silently wrong, since a misplaced filename token just makes CDP
    /// read some other argument as its datafile.
    ///
    /// The marks come from `InputSpec.head_tail_marks` rather than any parameter (see
    /// `ProcessDef::needs_head_tail_marks`), so this also pins that whole path: document →
    /// `InputSpec` → `headstails::marks_to_text` → temp datafile → CDP.
    #[test]
    fn distmore_reads_a_head_tail_marklist_written_from_the_documents_marks() {
        let cdp_dir = require_cdp!();
        // A constant sine has no varying zero-crossing rate for `bright` to sort by, but it
        // doesn't need one to prove the marklist was read: what's being checked is that CDP
        // accepts the file and produces real audio, not what it reorders it into.
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, warnings) = crate::model::cdp::CdpCatalog::load(None);
        assert!(warnings.is_empty(), "catalog failed to parse: {warnings:?}");

        // Three complete pairs, comfortably above `MIN_HEAD_TAIL_PAIRS`.
        let marks = vec![
            len_samples / 10,
            len_samples * 2 / 10,
            len_samples * 4 / 10,
            len_samples * 5 / 10,
            len_samples * 7 / 10,
            len_samples * 8 / 10,
        ];

        for (job_id, key) in [(400u64, "distmore_bright_2"), (401, "distmore_segsbkwd_3")] {
            let def = catalog.find(key).unwrap_or_else(|| panic!("{key} in catalog"));
            assert!(def.needs_head_tail_marks, "{key} is a marklist process");
            assert!(
                !def.params.iter().any(|p| p.name.contains("Head/Tail")),
                "{key} must take its marks from the document, not a form field"
            );

            let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
            let input = crate::model::cdp::InputSpec {
                channels: 1,
                sample_rate,
                len_samples,
                head_tail_marks: marks.clone(),
            };
            let planned = crate::model::cdp::plan_job(
                def,
                &values,
                std::slice::from_ref(&input),
                &crate::model::cdp::PvocSettings::default(),
            )
            .unwrap_or_else(|e| panic!("{key} should plan with 3 pairs: {e:?}"));

            // The marklist must be written as a real datafile, and its filename must be the
            // argv token immediately after the outfile.
            let (_, marklist) = planned
                .brk_files
                .iter()
                .find(|(name, _)| name == "headstails.txt")
                .unwrap_or_else(|| panic!("{key} should write a headstails datafile"));
            assert_eq!(
                marklist.lines().count(),
                marks.len(),
                "{key}: one timemark per line, in seconds"
            );
            let args = &planned.steps[0].args;
            let out_at = args.iter().position(|a| a == "out.wav").expect("an outfile token");
            assert_eq!(
                args[out_at + 1],
                "headstails.txt",
                "{key}: the marklist is positional, directly after the outfile — got {args:?}"
            );

            let runner = CdpRunner::new();
            runner.submit(Job {
                id: job_id,
                cdp_dir: cdp_dir.clone(),
                planned,
                inputs: vec![channels.clone()],
                input_sample_rate: sample_rate,
                purpose: JobPurpose::Apply,
            });
            let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
            else {
                unreachable!()
            };
            let output = result.unwrap_or_else(|e| panic!("{key} should accept the marklist: {e:?}"));
            assert_eq!(output.results.len(), 1, "{key} produces one result");
            let audio = &output.results[0];
            assert!(!audio[0].is_empty(), "{key} produced no samples");
            assert!(
                audio[0].iter().any(|s| s.abs() > 1e-3),
                "{key} produced silence, so nothing was really segmented"
            );
        }
    }

    /// Mode 2 (stereo) plus the variadic input ordering in one check: two mono sources, one
    /// loud and one a tenth of its amplitude, driving two vertices placed at opposite ends of
    /// the X axis. X is both the time offset *and* (in this mode only) the stereo position,
    /// so the loud source's vertex must land hard left and the quiet source's hard right —
    /// which means the left channel has to be dramatically louder than the right. Anything
    /// that mixed the two files up, ignored the pick order, or dropped a vertex would fail
    /// this rather than merely produce a differently-shaped file.
    ///
    /// Also pins the pre-Apply vertex/input-count block (`check_compound_param_data`), since
    /// this is the only shipped process where that rule can fire at all.
    #[test]
    fn crystal_rotate_stereo_pans_each_input_file_by_its_own_vertex_x() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();
        let quiet: Vec<Vec<f32>> = vec![channels[0].iter().map(|s| s * 0.1).collect()];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("crystal_rotate_2").expect("crystal_rotate_2 in catalog");
        assert!(def.output_is_stereo, "mode 2 is the stereo mode");

        let envelope = vec![(0.0, 0.0), (0.25, 1.0), (0.5, 0.0)];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };

        // Two files but three vertices: rejected here, before any temp file is written or
        // any process spawned, naming both counts.
        let mismatched = crystal_values(
            def,
            vec![[-0.9, 0.0, 0.0], [0.9, 0.0, 0.0], [0.0, 0.5, 0.0]],
            envelope.clone(),
            5.0,
        );
        let err = crate::model::cdp::plan_job(
            def,
            &mismatched,
            &[input.clone(), input.clone()],
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap_err();
        assert!(
            matches!(&err, crate::model::cdp::PlanError::InvalidParamData { reason, .. }
                if reason.contains("2 input files but 3 vertices")),
            "a vertex/file mismatch must be pre-blocked: {err:?}"
        );

        let values = crystal_values(def, vec![[-0.9, 0.0, 0.0], [0.9, 0.0, 0.0]], envelope, 5.0);
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            &[input.clone(), input.clone()],
            &crate::model::cdp::PvocSettings::default(),
        )
        .expect("two files and two vertices agree");
        assert_eq!(
            planned.steps[0].args[..4],
            ["rotate".to_string(), "2".to_string(), "in_1.wav".to_string(), "in_2.wav".to_string()],
            "both input files must be emitted, in pick order, before the outfile"
        );

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 161,
            cdp_dir,
            planned,
            inputs: vec![channels.clone(), quiet.clone()],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60)) else {
            unreachable!()
        };
        let output = result.expect("crystal rotate 2 should accept two mono sources");
        let stereo = &output.results[0];
        assert_eq!(stereo.len(), 2, "mode 2 writes a stereo file");

        let mean = |ch: &Vec<f32>| ch.iter().map(|s| s.abs() as f64).sum::<f64>() / ch.len() as f64;
        let (left, right) = (mean(&stereo[0]), mean(&stereo[1]));
        assert!(
            left > right * 4.0,
            "the loud source's vertex sits at x = -0.9 so it must dominate the left channel (L {left}, R {right})"
        );
        assert!(right > 0.0, "the quiet source's vertex still contributes to the right channel");
    }

    #[test]
    fn blur_avrg_pvoc_round_trip_preserves_duration() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("blur_avrg").expect("blur_avrg in catalog");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &[crate::model::cdp::ParamValue::Number(6.0)],
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 101,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("blur avrg should succeed on a real CDP install");
        assert_eq!(output.sample_rate, sample_rate);
        let ratio = output.results[0][0].len() as f64 / len_samples as f64;
        assert!((ratio - 1.0).abs() < 0.1, "expected ~same duration after pvoc round-trip, got ratio {ratio}");
    }

    /// Deterministic full-scale white noise -- no `rand` dependency, just a fixed-seed
    /// xorshift so the test is reproducible. `matrix matrix 1`'s clipping (see
    /// `plan_matrix_with_gain_calibration`'s doc comment) turned out to be content-dependent,
    /// not just channel-count-dependent -- the original formula-based fix held up against a
    /// sine tone but still clipped against noise (user report, 2026-07-26), which is exactly
    /// why this test exercises both content types against the real binary.
    fn white_noise_channels(len_samples: usize) -> Vec<f32> {
        let mut state: u32 = 0x9E3779B9;
        (0..len_samples)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    /// Regression coverage for the two-pass "measure-then-apply" gain calibration
    /// (`MatrixGainCalibration`'s doc comment): confirms it holds against the real binary at
    /// a high Analysis Channels setting (1024 -- confirmed by hand to clip badly enough that
    /// CDP's own CLI suggests cutting the source by ~93%; the max option, 16384, is a real
    /// worst case too but takes ~90 CPU-seconds per single mode-1 run on this machine, too
    /// slow for a test run 4x over) on both a pure sine tone and full-scale white noise --
    /// the exact combination that broke the original formula-based attenuation (it
    /// under-attenuated noise while over-attenuating the sine, since a fixed
    /// channel-count-only formula can't account for how energy actually accumulates across
    /// content types). No assertion on the *result* other than "peak output amplitude is
    /// within TARGET_PEAK's headroom" -- what the matrix transform actually sounds like is
    /// intentionally unpredictable (per the process description).
    #[test]
    fn matrix_gain_calibration_avoids_clipping_on_sine_and_white_noise() {
        let cdp_dir = require_cdp!();
        let (sine_channels, sample_rate) = mono_sine_channels();
        let len_samples = sine_channels[0].len();
        let noise_channels = vec![white_noise_channels(len_samples)];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("matrix_matrix_1").expect("matrix_matrix_1 in catalog");
        let channels_index = def.params.iter().position(|p| p.name == "Analysis Channels").unwrap();
        let choice_1024 = match &def.params[channels_index].kind {
            crate::model::cdp::def::ParamKind::Choice { options, .. } => {
                options.iter().position(|o| o == "1024").expect("1024 is a valid Analysis Channels option")
            }
            _ => unreachable!("Analysis Channels is always a Choice"),
        };
        let values = [
            crate::model::cdp::ParamValue::Choice(choice_1024),
            crate::model::cdp::ParamValue::Number(2.0),
            crate::model::cdp::ParamValue::Toggle(true),
            crate::model::cdp::ParamValue::Toggle(false),
        ];

        for (label, source_channels) in [("sine", sine_channels), ("white noise", noise_channels)] {
            let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
            let planned = crate::model::cdp::plan_job(
                def,
                &values,
                std::slice::from_ref(&input),
                &crate::model::cdp::PvocSettings::default(),
            )
            .unwrap();
            assert!(planned.matrix_gain_calibration.is_some(), "{label}: expected the two-pass job shape");

            let runner = CdpRunner::new();
            runner.submit(Job {
                id: 200,
                cdp_dir: cdp_dir.clone(),
                planned,
                inputs: vec![source_channels],
                input_sample_rate: sample_rate,
                purpose: JobPurpose::Apply,
            });

            let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30)) else {
                unreachable!()
            };
            let output = result.unwrap_or_else(|e| panic!("{label}: matrix matrix 1 should succeed, got {e:?}"));
            let peak = output.results[0][0].iter().fold(0.0f32, |max, &s| max.max(s.abs()));
            assert!(peak <= 0.96, "{label}: expected no clipping (peak <= ~TARGET_PEAK), got peak {peak}");
        }
    }

    /// Regression coverage for `matrix_matrix_2`'s own "Auto Gain Reduction"
    /// (`plan_matrix_apply_with_gain_calibration`) — user report, 2026-07-26: applying a
    /// *saved* matrix to a different file clips too, confirmed by hand against the real
    /// binary (a matrix generated from a full-scale sine, applied via mode 2 to unrelated
    /// full-scale white noise: peak ~229, far worse than mode 1's own worst case, since the
    /// saved matrix's energy characteristics have no relationship to the new source). This
    /// generates a real matrix file from one sound (mode 1, gain reduction off — just need a
    /// real `.matrix` file, not exercising mode 1's own calibration here), then applies it
    /// (mode 2, gain reduction on) to a *different* sound, confirming no clipping end to end.
    #[test]
    fn matrix_apply_gain_calibration_avoids_clipping_on_a_mismatched_source() {
        let cdp_dir = require_cdp!();
        let (sine_channels, sample_rate) = mono_sine_channels();
        let len_samples = sine_channels[0].len();
        let noise_channels = vec![white_noise_channels(len_samples)];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let gen_def = catalog.find("matrix_matrix_1").expect("matrix_matrix_1 in catalog");
        let channels_index = gen_def.params.iter().position(|p| p.name == "Analysis Channels").unwrap();
        let choice_1024 = match &gen_def.params[channels_index].kind {
            crate::model::cdp::def::ParamKind::Choice { options, .. } => {
                options.iter().position(|o| o == "1024").unwrap()
            }
            _ => unreachable!("Analysis Channels is always a Choice"),
        };
        let gen_values = [
            crate::model::cdp::ParamValue::Choice(choice_1024),
            crate::model::cdp::ParamValue::Number(2.0),
            crate::model::cdp::ParamValue::Toggle(false), // gain reduction irrelevant here -- just need a real matrix file
            crate::model::cdp::ParamValue::Toggle(false),
        ];
        let gen_input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let gen_planned = crate::model::cdp::plan_job(
            gen_def,
            &gen_values,
            std::slice::from_ref(&gen_input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        let gen_runner = CdpRunner::new();
        gen_runner.submit(Job {
            id: 300,
            cdp_dir: cdp_dir.clone(),
            planned: gen_planned,
            inputs: vec![sine_channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&gen_runner, Duration::from_secs(30)) else {
            unreachable!()
        };
        let matrix_bytes =
            result.unwrap().sidecar_bytes.expect("matrix matrix 1 should produce a matrix sidecar");

        let matrix_path = std::env::temp_dir()
            .join(format!("tui_wave_test_matrix_{}.matrix", std::process::id()));
        std::fs::write(&matrix_path, &matrix_bytes).unwrap();

        let apply_def = catalog.find("matrix_matrix_2").expect("matrix_matrix_2 in catalog");
        let apply_values = [
            crate::model::cdp::ParamValue::FilePath(matrix_path.to_string_lossy().into_owned()),
            crate::model::cdp::ParamValue::Toggle(true),
            crate::model::cdp::ParamValue::Toggle(false),
        ];
        let apply_input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let apply_planned = crate::model::cdp::plan_job(
            apply_def,
            &apply_values,
            std::slice::from_ref(&apply_input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        assert!(apply_planned.matrix_gain_calibration.is_some(), "expected the two-pass job shape");

        let apply_runner = CdpRunner::new();
        apply_runner.submit(Job {
            id: 301,
            cdp_dir,
            planned: apply_planned,
            inputs: vec![noise_channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });
        let CdpEvent::Finished { result, .. } = recv_finished(&apply_runner, Duration::from_secs(30)) else {
            unreachable!()
        };
        let _ = std::fs::remove_file(&matrix_path);
        let output = result.unwrap_or_else(|e| panic!("matrix matrix 2 should succeed, got {e:?}"));
        let peak = output.results[0][0].iter().fold(0.0f32, |max, &s| max.max(s.abs()));
        assert!(peak <= 0.96, "expected no clipping (peak <= ~TARGET_PEAK) on a mismatched source, got peak {peak}");
    }

    /// Regression test for a real bug found by manual testing: `grain_reposition`'s (and
    /// its sibling grain processes') "Max Inter-Grain Time"/"Min Hole Duration"/"Gate
    /// Tracking Window" params have valid ranges CDP computes from the actual input's
    /// duration at runtime, not the fixed literal ranges the catalog originally declared —
    /// confirmed by hand against the real binary (e.g. "-b1.0" rejected as "out of range
    /// (0.100000 to 0.200000)" against a genuinely short ~0.2s selection). The 1-second
    /// fixture every other smoke test in this file uses happened to land right at the edge
    /// of validity for the old static range, masking the bug — this one deliberately uses a
    /// much shorter slice to actually exercise it, through the real pipeline/runner, not
    /// just a manual CDP CLI probe.
    #[test]
    fn grain_reposition_succeeds_on_a_genuinely_short_selection() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        // This exact length ("out of range (0.1 to 0.2)") reproduced the bug being
        // regression-tested here.
        let short_len = (sample_rate as f64 * 0.2) as usize;
        let short_channels: Vec<Vec<f32>> =
            channels.into_iter().map(|c| c[..short_len.min(c.len())].to_vec()).collect();
        let len_samples = short_channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("grain_reposition").expect("grain_reposition in catalog");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let values: Vec<_> = def
            .params
            .iter()
            .map(|p| {
                if p.required_list {
                    let crate::model::cdp::ParamKind::Number { default, .. } = &p.kind else {
                        unreachable!()
                    };
                    crate::model::cdp::ParamValue::List(vec![*default])
                } else {
                    p.kind.default_value()
                }
            })
            .collect();
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 102,
            cdp_dir,
            planned,
            inputs: vec![short_channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(10)) else {
            unreachable!()
        };
        result.expect("grain_reposition should succeed on a genuinely short selection with its own default field values");
    }

    /// Regression test for a real, silent-data-loss bug found while researching a *new*
    /// catalog entry (`reverb`) but affecting an already-shipped one (`rmverb`, SoundThread-
    /// derived): both processes' own `-cN` flag defaults to `N=2`, meaning they emit a real
    /// stereo output *even from a mono input* — confirmed against the real binary. `plan_wav`
    /// used to set a `stereo_native` process's destination channel count to always match the
    /// *source's* channel count (`dest_channels = source_channels.clone()`), so a mono
    /// input's `dest_channels` was `[0]` — `load_outputs` then only ever read that one
    /// channel back out of a genuinely 2-channel result file, silently discarding the whole
    /// right channel with no error. Fixed by keying `dest_channels` off `def.output_is_stereo`
    /// instead. This test drives `rmverb` on a mono fixture and asserts the real output has
    /// both channels with actual (non-silent) content in each.
    #[test]
    fn rmverb_on_mono_input_returns_both_channels_of_its_real_stereo_output() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("rmverb").expect("rmverb in catalog");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 103,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30)) else {
            unreachable!()
        };
        let output = result.expect("rmverb should succeed on a real CDP install");
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].len(), 2, "rmverb's real output is stereo even from a mono input");
        for (i, channel) in output.results[0].iter().enumerate() {
            assert!(
                channel.iter().any(|&s| s.abs() > 0.001),
                "channel {i} should have real audio, not be silently dropped/left empty"
            );
            // Guards against the `requires_simple_wav_input` regression this test was written
            // for: rmverb misreading our WAVE_FORMAT_EXTENSIBLE float32 input as raw int32
            // samples, which didn't fail the run (exit 0, non-empty output) but silently
            // corrupted it into a DC-step-then-flatline pattern -- caught here as an
            // implausibly large single-sample jump plus a channel that's mostly flat, neither
            // of which a real reverb tail on a smooth sine input produces.
            let max_delta = channel.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
            assert!(
                max_delta < 0.5,
                "channel {i} has an implausibly large sample-to-sample jump ({max_delta}), looks like corrupted/misdecoded audio"
            );
            let flat_fraction =
                channel.windows(2).filter(|w| w[0] == w[1]).count() as f64 / channel.len() as f64;
            assert!(
                flat_fraction < 0.5,
                "channel {i} is mostly flat ({:.0}% unchanged samples), looks like corrupted/misdecoded audio",
                flat_fraction * 100.0
            );
        }
    }

    /// Regression test for a real bug found by manual testing: `strange glis` mode 2's
    /// "Spacing" (`hzstep`) param rejected its own unchanged catalog default at 96kHz with
    /// "Value (50.0) out of range (93.75 to 24000.0)" -- SoundThread's own catalog data
    /// declared a fixed 50-200 Hz range, but the real range is `[sample_rate/analysis_
    /// points, sample_rate/4]` per the binary's own usage text ("Range: FROM channel-frq-
    /// width TO nyquist/2"), confirmed by reproducing the exact reported error against a
    /// synthesized 96kHz sine. Fixed via `NumberScale::HzCappedToAnalysisRange` (a new
    /// scale, since the real range depends on sample rate, which no existing scale had
    /// access to) plus a `PARAM_OVERRIDE` entry in the converter script correcting the
    /// catalog's min/max/default. This test deliberately uses a 96kHz fixture rather than
    /// the file's usual 44.1kHz one -- at 44.1kHz the old fixed 50-200 range happened to
    /// overlap the real dynamic range enough to mask the bug, exactly the kind of
    /// fixture-masks-a-real-bug case `grain_reposition`'s own regression test above was
    /// written to catch a version of.
    #[test]
    fn strange_glis_succeeds_at_its_own_default_values_on_a_96khz_file() {
        let cdp_dir = require_cdp!();
        let sample_rate = 96_000u32;
        let len_samples = sample_rate as usize; // 1 second
        let channels = vec![(0..len_samples)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sample_rate as f32).sin() * 0.5)
            .collect()];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("strange_glis_2").expect("strange_glis_2 in catalog");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 104,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30)) else {
            unreachable!()
        };
        result.expect(
            "strange_glis_2 should succeed at its own unchanged default values on a 96kHz file",
        );
    }

    fn stereo_sine_channels() -> (Vec<Vec<f32>>, u32) {
        let doc = crate::model::io::load_wav("tests/fixtures/stereo_sine.wav").unwrap();
        (doc.channels, doc.sample_rate)
    }

    /// Regression test for the real bug behind "blur gives an error": `blur_blur` is the
    /// one catalog process using `PercentOfAnaWindowCount`, which can't be resolved until
    /// each channel lane's own `.ana` file exists (Phase 0 spike S5). On a stereo file this
    /// used to leave every lane but the last with an unresolved "0" placeholder — CDP
    /// rejects a blurring count of 0 as out of range — so this specifically exercises two
    /// lanes against the real binary, not just one.
    #[test]
    fn blur_blur_on_stereo_input_resolves_every_lanes_window_count() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = stereo_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("blur_blur").expect("blur_blur in catalog");

        let input = crate::model::cdp::InputSpec { channels: 2, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &[crate::model::cdp::ParamValue::Number(20.0)],
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        assert_eq!(planned.deferred_window_params.len(), 2, "expected one deferred entry per channel");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 106,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("blur blur should succeed on both stereo lanes");
        assert_eq!(output.results[0].len(), 2);
    }

    /// Regression test for the actual reported bug: automating (enveloping) `blur_blur`'s
    /// "Blurring" param used to reject with "Value (0.100000) out of range (1.0 to 1632.0)"
    /// — the `.brk` file held the raw 0-100 percent values verbatim instead of being scaled
    /// to real window counts the way a constant value already was. Deliberately includes
    /// 0.1 (the exact value from the report) as a breakpoint value to pin this down against
    /// the real binary, not just the planning logic.
    #[test]
    fn blur_blur_with_an_automated_blurring_value_resolves_the_brk_file() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("blur_blur").expect("blur_blur in catalog");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &[crate::model::cdp::ParamValue::Breakpoints(vec![(0.0, 0.1), (1.0, 50.0)])],
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        assert_eq!(planned.deferred_window_params.len(), 1);

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 105,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        result.expect("blur blur should succeed with an automated Blurring value, not reject 0.1 as an out-of-range window count");
    }

    #[test]
    fn out_of_range_param_yields_nonzero_exit_with_captured_output() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("modify_speed_2").expect("modify_speed_2 in catalog");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        // Speed's real range is [-96, 96] semitones; 999999 is deliberately out of range so
        // CDP itself rejects it (matches the Phase 0 spike S4 finding).
        let planned = crate::model::cdp::plan_job(
            def,
            &[crate::model::cdp::ParamValue::Number(999_999.0)],
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 107,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        match result {
            Err(CdpError::NonZeroExit { output, .. }) => {
                assert!(!output.is_empty(), "expected CDP's error text to be captured");
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    #[test]
    fn dual_input_sfedit_join_appends_two_files_end_to_end() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("sfedit_join").expect("sfedit_join in catalog");

        let inputs_spec = [
            crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() },
            crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() },
        ];
        let planned = crate::model::cdp::plan_job(
            def,
            &[],
            &inputs_spec,
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 108,
            cdp_dir,
            planned,
            inputs: vec![channels.clone(), channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("sfedit join should succeed on a real CDP install");
        let ratio = output.results[0][0].len() as f64 / len_samples as f64;
        assert!((ratio - 2.0).abs() < 0.05, "joining a file to itself should ~double duration, got ratio {ratio}");
    }

    /// Exercises the first shipped `ParamKind::Table` process end-to-end: a real multi-row
    /// tap table (3 taps, ascending times, varied amp/pan) through the actual pipeline/
    /// runner — the catalog smoke test only ever drives a table param with its single
    /// default-seeded row, so a bug specific to multiple rows (argv/datafile shape, or the
    /// per-column `NumberScale` resolution) would get through it untested. Also pins the
    /// `requires_simple_wav_input` fix: tapdelay failed ("unable to open outfile") against
    /// the float32 WAVE_FORMAT_EXTENSIBLE input this app would otherwise send it, the same
    /// root cause `rmverb`/`reverb` hit before.
    #[test]
    fn tapdelay_with_a_multi_row_tap_table_produces_real_stereo_output() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("tapdelay_tapdelay").expect("tapdelay_tapdelay in catalog");

        use crate::model::cdp::ParamValue as V;
        let values = vec![
            V::Number(0.25), // Tap Gain
            V::Number(0.2),  // Feedback
            V::Number(0.4),  // Mix
            V::Table(vec![
                vec![0.05, 0.8, -1.0], // time, amp, pan (hard left)
                vec![0.15, 0.5, 0.0],  // centre
                vec![0.30, 0.3, 1.0],  // hard right
            ]),
            V::Number(0.5), // Trail Time
        ];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        // Positional argv: infile, outfile, tapgain, feedback, mix, taps-datafile, trailtime.
        let args = &planned.steps[0].args;
        assert_eq!(args[0], "in.wav");
        assert_eq!(args[1], "out.wav");
        assert_eq!(args[2], "0.25");
        assert_eq!(args[3], "0.2");
        assert_eq!(args[4], "0.4");
        assert_eq!(args[6], "0.5");
        let (_, table_contents) = planned.brk_files.first().expect("a table datafile");
        assert_eq!(table_contents, "0.05 0.8 -1\n0.15 0.5 0\n0.3 0.3 1");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 115,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("tapdelay should succeed with a multi-row tap table");
        assert_eq!(output.results[0].len(), 2, "panned taps must produce real stereo output");
        let expected_secs = len_samples as f64 / sample_rate as f64 + 0.5; // + trail time
        let actual_secs = output.results[0][0].len() as f64 / output.sample_rate as f64;
        assert!(
            (actual_secs - expected_secs).abs() < 0.1,
            "expected ~{expected_secs:.2}s (source + trail time), got {actual_secs:.2}s"
        );
        for (i, channel) in output.results[0].iter().enumerate() {
            assert!(channel.iter().any(|&s| s.abs() > 0.001), "channel {i} should have real audio");
        }
    }

    /// Exercises `repeater`'s Table param with multiple *overlapping and out-of-order*
    /// segments (row 2 starts earlier in the source than row 1) — the one catalog table
    /// with no ascending-order constraint at all (unlike tapdelay's time column), so this
    /// specifically confirms the app never enforces one where the real binary doesn't
    /// require it, and that a real multi-row segment table (not just the single-row
    /// smoke-test default) runs correctly end to end. ("Backtrack," per the binary's own
    /// usage text, means later *rows* may read earlier source material than prior rows —
    /// not that a single row's own end may precede its own start, which the real binary
    /// rejects as a negative-duration segment.)
    #[test]
    fn repeater_with_overlapping_and_out_of_order_segments_succeeds() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("repeater_repeater_1").expect("repeater_repeater_1 in catalog");

        use crate::model::cdp::ParamValue as V;
        let values = vec![
            V::Table(vec![
                vec![0.5, 0.6, 3.0, 0.05],  // forward segment
                vec![0.1, 0.3, 2.0, 0.05],  // starts earlier in the source than row 1
                vec![0.4, 0.65, 2.0, 0.05], // overlaps both of the above
            ]),
            V::Number(1.0), // Randomize Delay: none
            V::Number(0.0), // Randomize Pitch: none
        ];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        let (_, table_contents) = planned.brk_files.first().expect("a table datafile");
        assert_eq!(table_contents, "0.5 0.6 3 0.05\n0.1 0.3 2 0.05\n0.4 0.65 2 0.05");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 116,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("repeater should accept overlapping/backward segments without any client-side order rejection");
        assert!(!output.results[0][0].is_empty());
    }

    /// Exercises `repeater` mode 3's extra positional params (Acceleration/Warp/Fade Shape,
    /// which come *after* the table datafile in argv) plus the real repeat-count edge case
    /// found by hand: 0 means "no repeat" and succeeds, but the real binary specifically
    /// rejects 1 ("Repeat value less than 2") while accepting any integer >= 2 — this table
    /// uses 0 on one row to pin that down against the real binary, not just the smoke
    /// test's single-row default.
    #[test]
    fn repeater_mode_3_dimming_with_a_zero_repeat_count_row() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("repeater_repeater_3").expect("repeater_repeater_3 in catalog");

        use crate::model::cdp::ParamValue as V;
        let values = vec![
            V::Table(vec![
                vec![0.1, 0.2, 0.0, 0.05], // 0 repeats: play the segment once, untouched
                vec![0.4, 0.5, 3.0, 0.05],
            ]),
            V::Number(2.0), // Acceleration
            V::Number(1.0), // Warp
            V::Number(1.0), // Fade Shape
            V::Number(1.0), // Randomize Delay: none
            V::Number(0.0), // Randomize Pitch: none
        ];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        // Positional argv: subprog, mode, infile, outfile, table-datafile, accel, warp, fade.
        let args = &planned.steps[0].args;
        assert_eq!(args[0], "repeater");
        assert_eq!(args[1], "3");
        assert_eq!(args[2], "in.wav");
        assert_eq!(args[3], "out.wav");
        assert_eq!(args[5], "2");
        assert_eq!(args[6], "1");
        assert_eq!(args[7], "1");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 117,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("repeater mode 3 should accept a 0 repeat-count row and its own accel/warp/fade params");
        assert!(!output.results[0][0].is_empty());
    }

    /// Exercises `focus freeze`'s `ParamKind::MarkerTimeList` end-to-end with multiple
    /// entries — the smoke test only ever drives it with a single default entry, which
    /// trivially satisfies both real constraints found by hand (strictly ascending times,
    /// and never an 'a' marker followed later by a 'b' one). This pins down the datafile's
    /// exact format (marker concatenated directly onto the time, no separator) against a
    /// real multi-line file, using only 'a'-then-'a' and 'b'-then-'a' transitions (both
    /// confirmed valid) to stay clear of the 'a'-then-'b' "Impossible time sequence" quirk.
    #[test]
    fn focus_freeze_with_multiple_marked_times_succeeds() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("focus_freeze_1").expect("focus_freeze_1 in catalog");

        use crate::model::cdp::ParamValue as V;
        let values = vec![V::MarkerTimeList(vec![('b', 0.2), ('a', 0.5), ('a', 0.8)])];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        let (_, datafile_contents) = planned.brk_files.first().expect("a marker-time datafile");
        assert_eq!(datafile_contents, "b0.2\na0.5\na0.8", "marker must be concatenated directly onto the time, no separator");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 118,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("focus freeze should succeed with multiple marked times");
        assert!(!output.results[0][0].is_empty());
    }

    /// Exercises `hilite band`'s bitflag-conditional rows end-to-end with several distinct
    /// bit combinations in one table — the smoke test only ever drives it with the single
    /// default row (`amp_bit` alone), which can't catch a datafile-shape bug specific to a
    /// different combination or to multiple rows together. Covers: amp-only (bit 1), ramp
    /// with both amp1/amp2 (bits 1+2), plain-multiplier transpose (bit 3), and transpose
    /// with additive Hz + add-in (bits 3+4, `+` prefix) — one of each conditional shape the
    /// datafile format supports.
    #[test]
    fn hilite_band_with_varied_bit_combinations_succeeds() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("hilite_band").expect("hilite_band in catalog");

        use crate::model::cdp::{HiliteBandRow, ParamValue as V};
        let rows = vec![
            HiliteBandRow {
                lofrq: 100.0,
                hifrq: 800.0,
                amp_bit: true,
                ramp_bit: false,
                transpose_bit: false,
                add_bit: false,
                amp1: 0.5,
                amp2: 1.0,
                transpose_value: 1.0,
                transpose_additive: false,
            },
            HiliteBandRow {
                lofrq: 800.0,
                hifrq: 2000.0,
                amp_bit: true,
                ramp_bit: true,
                transpose_bit: false,
                add_bit: false,
                amp1: 0.3,
                amp2: 0.9,
                transpose_value: 1.0,
                transpose_additive: false,
            },
            HiliteBandRow {
                lofrq: 2000.0,
                hifrq: 4000.0,
                amp_bit: false,
                ramp_bit: false,
                transpose_bit: true,
                add_bit: false,
                amp1: 1.0,
                amp2: 1.0,
                transpose_value: 1.5,
                transpose_additive: false,
            },
            HiliteBandRow {
                lofrq: 4000.0,
                hifrq: 8000.0,
                amp_bit: false,
                ramp_bit: false,
                transpose_bit: true,
                add_bit: true,
                amp1: 1.0,
                amp2: 1.0,
                transpose_value: 50.0,
                transpose_additive: true,
            },
        ];
        let values = vec![V::HiliteBand(rows)];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        let (_, datafile_contents) = planned.brk_files.first().expect("a hilite band datafile");
        assert_eq!(
            datafile_contents,
            "100 800 1000 0.5\n800 2000 1100 0.3 0.9\n2000 4000 0010 1.5\n4000 8000 0011 +50"
        );

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 119,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("hilite band should succeed with varied bit combinations across multiple rows");
        assert!(!output.results[0][0].is_empty());
    }

    /// Exercises a `required_list` time-sequence param with a real multi-entry ascending
    /// list plus engaged flag params — the smoke test only ever drives such params with a
    /// single default entry and every flag at its (unemitted) default, so an argv-ordering
    /// or datafile-shape bug that only manifests with several slice times or with `-s`/`-a`
    /// style tokens present would get through it. `motor` mode 5 is one of the new
    /// hand-authored entries; its Duration param directly sets the output length, which
    /// gives a real correctness assertion beyond "exit 0".
    #[test]
    fn motor_5_with_a_multi_entry_slice_time_list_and_engaged_flags() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("motor_motor_5").expect("motor_motor_5 in catalog");

        use crate::model::cdp::ParamValue as V;
        // In catalog param order: slice times (multi-entry, ascending), duration 1s, then
        // the positional defaults, then real values for the flagged params -j/-s and the
        // bare -a toggle.
        let values = vec![
            V::List(vec![0.05, 0.3, 0.6]), // Slice Times
            V::Number(1.0),                // Duration
            V::Number(10.0),               // Inner Pulse Rate
            V::Number(2.0),                // Outer Pulse Rate
            V::Number(0.5),                // Inner On/Off Ratio
            V::Number(0.5),                // Outer On/Off Ratio
            V::Number(0.5),                // Symmetry
            V::Number(0.0),                // Freq Randomize (-f)
            V::Number(0.0),                // Pulse Randomize (-p)
            V::Number(0.5),                // Jitter (-j) — deliberately non-default
            V::Number(0.0),                // Tremor (-t)
            V::Number(0.0),                // Shift (-y)
            V::Number(0.0),                // Edge (-e)
            V::Number(3.0),                // Bite (-b)
            V::Number(0.0),                // Vary (-v)
            V::Number(1.0),                // Seed (-s) — deliberately non-default
            V::Toggle(true),               // Advance By Fixed Step (-a)
        ];

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        // The list datafile must be referenced positionally before the numeric params, and
        // the engaged flags must appear as single tokens.
        let args = &planned.steps[0].args;
        assert_eq!(args[..4], ["motor".to_string(), "5".into(), "in.wav".into(), "out.wav".into()]);
        assert_eq!(args[4], "list_0.txt");
        assert!(args.contains(&"-j0.5".to_string()), "flagged jitter missing: {args:?}");
        assert!(args.contains(&"-s1".to_string()), "flagged seed missing: {args:?}");
        assert!(args.contains(&"-a".to_string()), "bare toggle missing: {args:?}");
        let (_, list_contents) =
            planned.brk_files.iter().find(|(n, _)| n == "list_0.txt").expect("list datafile");
        assert_eq!(list_contents, "0.05\n0.3\n0.6");

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 110,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("motor 5 should succeed with a multi-entry slice-time list");
        let duration = output.results[0][0].len() as f64 / output.sample_rate as f64;
        // motor ends at the last complete outer pulse rather than padding out the requested
        // duration — probed by hand against the real binary: at outer rate 2.0 / on-off 0.5
        // it consistently emits requested − 0.25s (the trailing off-phase) for any requested
        // length. Allow up to one outer-pulse period (0.5s at rate 2.0) of shortfall.
        assert!(
            duration > 0.5 && duration <= 1.05,
            "Duration param was 1.0s (outer pulse period 0.5s) but output is {duration:.2}s"
        );
    }

    /// The headroom scheme's core claim, checked against the real binaries rather than
    /// asserted: attenuating the input and scaling the result back must reproduce what the
    /// process would have produced untouched — apart from the clipping it avoids.
    ///
    /// Runs each process twice, once as planned (attenuated, then restored) and once with the
    /// headroom stripped back out of the very same plan, and compares. Where the unattenuated
    /// run did not clip, the two must match closely; where it did clip, they must differ, since
    /// the whole point is that the clipped version lost data.
    ///
    /// `specfnu_specfnu_10` is in the sample deliberately: it is pitch-based, and CDP's pitch
    /// trackers can carry absolute amplitude floors that a −24 dB shift could fall through.
    /// This is what rules that out for the entries actually on the list.
    #[test]
    fn clip_headroom_restores_what_the_process_would_have_produced() {
        let cdp_dir = require_cdp!();
        let (mut channels, sample_rate) = mono_sine_channels();
        // The stock fixture peaks at 0.5; push it to a realistic level so the flagged
        // processes are actually driven into the range where CDP would clamp them.
        for ch in &mut channels {
            for s in ch.iter_mut() {
                *s *= 1.9;
            }
        }
        let len_samples = channels[0].len();
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);

        for key in ["focus_accu", "specfnu_specfnu_10", "repitch_transpose_1", "strange_glis_1"] {
            let def = catalog.find(key).unwrap_or_else(|| panic!("{key} in catalog"));
            assert!(
                crate::model::cdp::pipeline::needs_clip_headroom(key),
                "{key} should be on the headroom list"
            );
            let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
            let input =
                crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
            let planned = crate::model::cdp::plan_job(
                def,
                &values,
                std::slice::from_ref(&input),
                &crate::model::cdp::PvocSettings::default(),
            )
            .unwrap();

            // The same plan with the headroom removed — i.e. exactly what this job was before.
            let mut bare = planned.clone();
            bare.clip_headroom_restore = None;
            for spec in &mut bare.input_files {
                spec.gain = None;
            }

            let runner = CdpRunner::new();
            let run = |id: u64, plan: crate::model::cdp::pipeline::PlannedJob| {
                runner.submit(Job {
                    id,
                    cdp_dir: cdp_dir.clone(),
                    planned: plan,
                    inputs: vec![channels.clone()],
                    input_sample_rate: sample_rate,
                    purpose: JobPurpose::Apply,
                });
                let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
                else {
                    unreachable!()
                };
                result.unwrap_or_else(|e| panic!("{key} should run: {e:?}"))
            };

            let with = run(200, planned);
            let without = run(201, bare);

            let a = &with.results[0][0];
            let b = &without.results[0][0];
            let n = a.len().min(b.len());
            assert!(n > 0, "{key} produced no samples");

            let bare_peak = b.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let bare_clipped = bare_peak >= 0.9999;
            let worst = (0..n).fold(0.0f32, |m, i| m.max((a[i] - b[i]).abs()));

            if bare_clipped {
                // The unattenuated run hit CDP's clamp, so the headroom version must differ --
                // that difference is precisely the data the clamp was destroying.
                assert!(
                    worst > 1e-4,
                    "{key}: unattenuated output clipped (peak {bare_peak:.4}) yet the headroom \
                     version is identical -- the headroom is doing nothing"
                );
            } else {
                // Nothing was clipped, so restoring must reproduce the original almost exactly.
                // Not bit-exact end to end: CDP re-runs its own float maths on a rescaled input,
                // so tiny numerical differences are expected even though the scaling itself is
                // lossless.
                assert!(
                    worst < 1e-3,
                    "{key}: no clipping to avoid (peak {bare_peak:.4}) but the headroom version \
                     differs by {worst:.6} -- the process is level-dependent and does not belong \
                     on the headroom list"
                );
            }
        }
    }

    /// The two processes originally reported as clipping (2026-07-28) must now come back
    /// inside full scale, driven at a level that made both clip before the headroom scheme.
    /// `fastconv` is the harder case: convolution against a tonal impulse measured +32.7 dB,
    /// far past what attenuation alone covers, so it also depends on its `-f` float-output
    /// default keeping the true peak alive for the gain stage to scale down.
    #[test]
    fn reported_clipping_processes_come_back_within_full_scale() {
        let cdp_dir = require_cdp!();
        let (mut channels, sample_rate) = mono_sine_channels();
        for ch in &mut channels {
            for s in ch.iter_mut() {
                *s *= 1.9;
            }
        }
        let len_samples = channels[0].len();
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);

        for key in ["focus_accu", "fastconv_fastconv"] {
            let def = catalog.find(key).unwrap_or_else(|| panic!("{key} in catalog"));
            let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
            let input =
                crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
            let (arity, ..) = def.input_arity();
            let planned = crate::model::cdp::plan_job(
                def,
                &values,
                &vec![input.clone(); arity],
                &crate::model::cdp::PvocSettings::default(),
            )
            .unwrap();
            assert_eq!(planned.clip_headroom_restore, Some(16.0), "{key} should reserve headroom");

            let runner = CdpRunner::new();
            runner.submit(Job {
                id: 300,
                cdp_dir: cdp_dir.clone(),
                planned,
                inputs: vec![channels.clone(); arity],
                input_sample_rate: sample_rate,
                purpose: JobPurpose::Apply,
            });
            let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
            else {
                unreachable!()
            };
            let out = result.unwrap_or_else(|e| panic!("{key} should run: {e:?}"));

            let mut peak = 0.0f32;
            let mut worst_run = 0usize;
            for ch in &out.results[0] {
                let mut run = 0usize;
                for &s in ch {
                    peak = peak.max(s.abs());
                    if s.abs() >= 0.9999 {
                        run += 1;
                        worst_run = worst_run.max(run);
                    } else {
                        run = 0;
                    }
                }
            }
            assert!(peak <= 1.0, "{key}: peak {peak:.4} still exceeds full scale");
            assert!(worst_run < 4, "{key}: {worst_run} consecutive samples pinned at full scale");
        }
    }

    /// End-to-end proof of the `spec_grab_prepass` chain against the real binaries — the one
    /// that matters, since the unit test only inspects planned argv. `morph glide` was
    /// unrunnable before this (`PlanError::UnsupportedInV1`), so this is also the regression
    /// guard for the process working at all. Its Output Duration param sets the result length
    /// directly, which gives a real assertion beyond "exit 0" and proves the two grabbed
    /// single-window analyses actually reached the glide: with a full multi-window analysis on
    /// either side the binary rejects the run outright rather than producing a short file.
    #[test]
    fn morph_glide_runs_end_to_end_via_the_spec_grab_prepass() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("morph_glide").expect("morph_glide in catalog");
        assert!(def.spec_grab_prepass, "morph_glide must carry the pre-pass flag");

        use crate::model::cdp::ParamValue as V;
        // Window 1/2 Position (percentages, consumed by the grabs) then Output Duration.
        let values = vec![V::Number(25.0), V::Number(75.0), V::Number(3.0)];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            &[input.clone(), input.clone()],
            &crate::model::cdp::PvocSettings::default(),
        )
        .expect("morph glide plans now that the pre-pass exists");

        let grabs = planned.steps.iter().filter(|s| s.bin == "spec").count();
        assert_eq!(grabs, 2, "expected one spec grab per input: {:?}", planned.steps);

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 111,
            cdp_dir,
            planned,
            inputs: vec![channels.clone(), channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
        else {
            unreachable!()
        };
        let output = result.expect("morph glide should succeed via the spec-grab pre-pass");
        let duration = output.results[0][0].len() as f64 / output.sample_rate as f64;
        assert!(
            (duration - 3.0).abs() < 0.2,
            "Output Duration param was 3.0s but the result is {duration:.2}s"
        );
    }

    /// Exercises a synthesis process (`IoKind::None`) whose first param is a
    /// `required_list` of *values* (MIDI pitches — no time axis, no ordering constraint)
    /// followed by two `Choice` params — the full "no input buffer at all, output becomes
    /// an insert at the cursor" path with a real multi-note chord, asserting the declared
    /// `output_is_stereo` and the sample rate actually chosen via the Choice param rather
    /// than just exit 0.
    #[test]
    fn synth_chord_produces_a_stereo_chord_at_the_chosen_sample_rate() {
        let cdp_dir = require_cdp!();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("synth_chord_1").expect("synth_chord_1 in catalog");

        use crate::model::cdp::ParamValue as V;
        let values = vec![
            V::List(vec![60.0, 64.0, 67.0]), // Pitches: C major triad
            V::Choice(4),                    // Sample Rate: "44100"
            V::Choice(0),                    // Output Channels: "2"
            V::Number(1.0),                  // Duration
            V::Number(0.5),                  // Amplitude (-a)
        ];
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            &[],
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 111,
            cdp_dir,
            planned,
            inputs: vec![],
            input_sample_rate: 48_000, // deliberately NOT the chosen rate
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("synth chord should succeed with a 3-note pitch list");
        assert_eq!(output.sample_rate, 44_100, "sample rate must come from the Choice param's real output file, not the submitting document");
        assert_eq!(output.results[0].len(), 2, "synth chord declares output_is_stereo");
        for (i, channel) in output.results[0].iter().enumerate() {
            assert!(
                (channel.len() as f64 / 44_100.0 - 1.0).abs() < 0.1,
                "channel {i}: expected ~1s at 44.1kHz, got {} samples",
                channel.len()
            );
            assert!(channel.iter().any(|&s| s.abs() > 0.01), "channel {i} is silent");
        }
    }

    /// Exercises a real glob-output run end-to-end: `distcut` on a 1-second sine with a
    /// 20-cycle segment size must produce *several* numbered `cutout N.wav` files, each
    /// loading as its own separate result buffer — the existing glob test fakes the
    /// numbered files with `sh -c 'cp …'`, so nothing yet proved a real CDP binary's own
    /// numbering/format round-trips through `load_glob_outputs`.
    #[test]
    fn distcut_on_a_real_sine_returns_multiple_segments_as_separate_buffers() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("distcut_distcut_1").expect("distcut_distcut_1 in catalog");

        use crate::model::cdp::ParamValue as V;
        let values = vec![
            V::Number(20.0), // Cycle Count: cut every 20 wavecycles
            V::Number(1.0),  // Decay Shape
            V::Number(70.0), // Limit (-c)
        ];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 112,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("distcut should succeed on a real sine");
        assert!(
            output.results.len() >= 2,
            "a 1s sine cut every 20 cycles should produce several segments, got {}",
            output.results.len()
        );
        let total: usize = output.results.iter().map(|r| r[0].len()).sum();
        assert!(total > 0);
        for (i, segment) in output.results.iter().enumerate() {
            assert!(!segment[0].is_empty(), "segment {i} is empty");
        }
    }

    /// Exercises the dual-`Ana` lane-pairing path with *mismatched channel counts*: a
    /// stereo selection against a mono second buffer must run two full
    /// anal/anal/process/synth lanes, reusing the mono input's only channel in both — the
    /// existing dual-input test (`sfedit_join`) is mono+mono and stereo-native, so the
    /// mono-reuse pairing in `plan_dual_ana` had no end-to-end coverage at all.
    #[test]
    fn dual_ana_stereo_selection_with_mono_second_input_pairs_lanes() {
        let cdp_dir = require_cdp!();
        let (stereo, sample_rate) = stereo_sine_channels();
        let (mono, _) = mono_sine_channels();
        let len_samples = stereo[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("combine_diff").expect("combine_diff in catalog");

        let inputs_spec = [
            crate::model::cdp::InputSpec { channels: 2, sample_rate, len_samples, ..Default::default() },
            crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples: mono[0].len(), ..Default::default() },
        ];
        let planned = crate::model::cdp::plan_job(
            def,
            &[crate::model::cdp::ParamValue::Number(1.0)],
            &inputs_spec,
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        // Two lanes of anal A + anal B + combine + synth.
        assert_eq!(planned.steps.len(), 8);

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 113,
            cdp_dir,
            planned,
            inputs: vec![stereo, mono],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(60))
        else {
            unreachable!()
        };
        let output = result.expect("combine diff should succeed on stereo-vs-mono lanes");
        assert_eq!(output.results[0].len(), 2, "expected a stereo result, one lane per selection channel");
        for (i, channel) in output.results[0].iter().enumerate() {
            assert!(!channel.is_empty(), "lane {i} produced no audio");
        }
    }

    /// Exercises an *automated* (`Breakpoints`) value on a `PercentOfInputDuration`-scaled
    /// param end-to-end: each point's 0-100 percent value must be rescaled into real
    /// seconds in the emitted `.brk` file (the same class of bug `blur_blur`'s deferred
    /// window-count envelope had — raw percents written verbatim — but on the plan-time
    /// path, which nothing exercised with an envelope + non-plain scale before).
    #[test]
    fn envelope_on_a_percent_of_input_duration_param_scales_points_to_seconds() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();
        let duration = len_samples as f64 / sample_rate as f64;

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("extend_drunk_1").expect("extend_drunk_1 in catalog");

        use crate::model::cdp::ParamValue as V;
        let values = vec![
            V::Number(1.0), // Minimum Output Duration (seconds — keeps the run fast)
            V::Breakpoints(vec![(0.0, 0.0), (duration, 50.0)]), // Location: 0% -> 50%
            V::Number(2.0),  // Ambitus (percent)
            V::Number(0.5),  // Maximum Step (percent)
            V::Number(0.05), // Clock
        ];
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples, ..Default::default() };
        let planned = crate::model::cdp::plan_job(
            def,
            &values,
            std::slice::from_ref(&input),
            &crate::model::cdp::PvocSettings::default(),
        )
        .unwrap();
        // Plan-level: the .brk file must hold seconds (50% of the ~1s fixture ≈ 0.5), never
        // the raw percent values.
        let (_, brk) = planned.brk_files.first().expect("a .brk file for the Location envelope");
        let last_value: f64 = brk.lines().last().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        assert!(
            (last_value - duration / 2.0).abs() < 0.01,
            "expected ~{:.3}s for the 50% point, .brk holds {last_value} (raw percent leak?)",
            duration / 2.0
        );

        let runner = CdpRunner::new();
        runner.submit(Job {
            id: 114,
            cdp_dir,
            planned,
            inputs: vec![channels],
            input_sample_rate: sample_rate,
            purpose: JobPurpose::Apply,
        });

        let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
        else {
            unreachable!()
        };
        let output = result.expect("extend drunk should accept an enveloped Location");
        assert!(
            output.results[0][0].len() as f64 / sample_rate as f64 >= 0.9,
            "output should honor the 1s minimum duration"
        );
    }

    /// Runs every catalog entry once, at its own declared defaults, against a short mono
    /// sine and asserts it succeeds — the bulk-authoring safety net `CDP-Ext-Plan.md`'s Tier
    /// 0 depends on: a hand-typed `[[process]]` entry can have the wrong argv shape (subprog
    /// misspelled, params in the wrong order, a mode string CDP doesn't recognise) even when
    /// it parses as valid TOML, and that only shows up by actually running it. Deliberately
    /// separate from `TUI_WAVE_CDP_DIR`'s always-on gating (`require_cdp!`) — iterating the
    /// whole catalog takes real wall-clock time (a CDP invocation per entry, several needing
    /// a `pvoc anal`/`synth` wrap), which is fine for a manually-triggered check but not for
    /// every `cargo test`. A dual-input process gets the same mono input on both sides
    /// (self-processing is always valid for the argv shapes we care about here); a
    /// `PlanError::UnsupportedInV1` is a known, accepted gap, not a smoke-test failure —
    /// though as of 2026-07-28 no catalog entry produces one any more (`morph_glide`, the only
    /// entry that ever did, now plans via `ProcessDef::spec_grab_prepass`), so that branch is
    /// a safety net for the non-catalog `IoKind`s rather than something exercised in practice.
    /// Collects every failure before asserting, so one bad entry's
    /// error doesn't hide every other one behind it.
    #[test]
    fn catalog_smoke_test() {
        if std::env::var("TUI_WAVE_CDP_SMOKE").ok().as_deref() != Some("1") {
            eprintln!("skipping: set TUI_WAVE_CDP_SMOKE=1 to run the full catalog smoke test");
            return;
        }
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();
        // Four evenly-spread head/tail marks — two complete pairs, `MIN_HEAD_TAIL_PAIRS` —
        // so the DISTMORE family's marklist (`ProcessDef.needs_head_tail_marks`) is seeded
        // with something CDP will actually accept. This is what took all thirteen of those
        // entries *out* of `KNOWN_FIXTURE_FAILURES`: while the marklist was a `required_list`
        // param, the generic seeding below could only ever produce one entry, which CDP
        // rejects outright ("must be at least 2 pairs"). Now the marks come from the document
        // rather than a form field, the harness can supply a real pair set. Every other
        // process ignores this field entirely.
        let head_tail_marks =
            vec![len_samples / 8, len_samples / 4, len_samples / 2, len_samples * 3 / 4];
        let input = crate::model::cdp::InputSpec {
            channels: 1,
            sample_rate,
            len_samples,
            head_tail_marks,
        };

        // Entries that fail against this harness's specific test fixture (a 1-second, full-
        // level, constant sine tone) for reasons that have nothing to do with the catalog
        // entry's own argv shape or ranges — a real recording wouldn't trip these. Documented
        // rather than silently dropped, per usual policy for a bounded/known exclusion list:
        //   envspeak_envspeak_{1,2,5,6}: needs audio with real amplitude troughs to find; a
        //     constant tone has none.
        //   gate_gate_{1,2}: a noise gate needs a mix of loud and quiet passages to have
        //     anything meaningful to do; against a constant-level tone, any fixed threshold
        //     either gates nothing ("No signal is gateable") or everything ("Entire signal
        //     would be gated") — the catalog default (-40dB) is a sensible choice for real
        //     audio with an actual noise floor.
        //   grainex_extend, grain_reverse, grain_align: "NO PEAKS IN THE FILE" / "No grains
        //     found" — grain-finding needs amplitude variation (peaks and troughs) to find
        //     grains between; a constant-level tone has none by definition. grain_align
        //     (added 2026-07-22, CDP-WASM-SUITE-gaps.md's "Align grains") hits this from both
        //     of its two inputs at once (fed the same constant-tone fixture on each side, per
        //     this harness's single-input-duplicated convention for dual-input processes).
        //   (housekeep_extract_4 used to be listed here as "content-dependent". That was a
        //     misdiagnosis: its catalog default was 0.0, and `housekeep extract 4` rejects a
        //     zero shift outright with "NO CHANGE to original sound file" — so it failed on
        //     *every* input, not just this fixture, and in the app too (user report,
        //     2026-07-27). It now defaults to the negated measured DC offset
        //     (`ParamDef::default_from_dc_offset`), which this harness mirrors above, and it
        //     runs for real here — 2026-07-27.)
        //   modify_space_2, modify_space_4, tostereo_tostereo, spin_stereo_1: explicitly
        //     stereo-only ("MIRROR/NARROW only works with STEREO input files"; tostereo:
        //     "must be stereo"; spin stereo: "File in.wav is not of correct type (must be
        //     stereo)") — this harness only ever exercises mono input (see
        //     `input_count`/`inputs` above), so any process that hard-requires stereo will
        //     always fail here regardless of catalog correctness. Each verified correct by
        //     hand against `tests/fixtures/stereo_sine.wav` (real exit-0 runs), not a bug to
        //     chase.
        //   specfnu_specfnu_19: the CDP binary itself crashes ("double free or corruption")
        //     on this input — a CDP bug, nothing tui-wave's plan/argv can work around.
        //   (The thirteen `distmore_*` entries used to be listed here: their Head/Tail
        //     marklist was a `required_list` param, and the generic one-entry seeding below
        //     could never satisfy CDP's "at least 2 pairs". They now take their marks from
        //     `InputSpec.head_tail_marks`, which this harness seeds with two real pairs, so
        //     they run for real here — 2026-07-27.)
        //   matrix_matrix_2: its `FilePath` param (`ParamKind::FilePath`) has no catalog
        //     default at all — `ParamKind::default_value` can only supply an empty string
        //     ("Cannot open datafile"), same "no real value to seed with" situation as
        //     `FormantBufferRef` params below. Verified correct by hand: generated a real
        //     `.txt` matrix file via `matrix matrix 1` and fed it straight back through
        //     `matrix matrix 2 ... -c`, exit 0 (2026-07-26).
        // And two pre-existing (not catalog_extra.toml's) bugs found the same way: the
        // machine-generated catalog.toml (regenerate via
        // scripts/convert_soundthread_catalog.py, don't hand-edit) has a default outside
        // CDP's actually-enforced range for extend_scramble_1 (0.02 vs 0.031-0.985) and
        // modify_brassage_4 (2500 vs 0-2000).
        // Entries that legitimately produce *no* output against this harness's fixture while
        // working fine on real material -- the empty-output counterpart to
        // KNOWN_FIXTURE_FAILURES above, and bounded/documented for the same reason:
        //   distort_filter_1 ("omit cycles below FREQ", 1000Hz default) and distort_filter_3
        //     ("omit below FREQ1 and above FREQ2", 500-2000Hz default band): this harness's
        //     fixture is a single low-frequency sine, so every wavecycle in it sits below both
        //     cutoffs and is correctly omitted. Confirmed to be the fixture and not the
        //     entries (2026-07-28): filter 1 at 100Hz returns all 132100 frames, and filter 3
        //     at its shipped 500-2000 default returns 88156 frames from a 1kHz tone.
        // Investigating this list is what turned up a real bug in filter_3 alongside the
        // artifact -- it used to default to Low = High = 1000.0, which passes only wavecycles
        // sitting exactly on the boundary frequency and discards everything else regardless of
        // input. Fixed in the entry (see its note in catalog_extra.toml); it stays listed here
        // because the fixture still can't exercise it.
        const KNOWN_EMPTY_OUTPUT: &[&str] = &["distort_filter_1", "distort_filter_3"];

        const KNOWN_FIXTURE_FAILURES: &[&str] = &[
            "envspeak_envspeak_1",
            "envspeak_envspeak_2",
            "envspeak_envspeak_5",
            "envspeak_envspeak_6",
            "extend_scramble_1",
            "gate_gate_1",
            "gate_gate_2",
            "grain_align",
            "grain_reverse",
            "grainex_extend",
            "matrix_matrix_2",
            "modify_brassage_4",
            "modify_space_2",
            "modify_space_4",
            "specfnu_specfnu_19",
            "spin_stereo_1",
            "tostereo_tostereo",
            // psow_interp requires each input be a pre-grabbed single grain (e.g. via
            // psow_grab with duration 0) -- fed an ordinary recording, the real binary
            // hard-rejects it: "File 1 is not a valid pitch-sync grain file". A
            // fixture-content issue, not a catalog bug (found while cataloging the psow
            // family, 2026-07-15).
            "psow_interp",
            // The multichannel batch (2026-08-03). This fixture is one mono channel, and
            // `ProcessDef.input_channels` is a hard demand of the real binary rather than a
            // preference, so these four are refused by `plan_job` before a binary is ever
            // spawned — the same "needs real stereo input" situation `spin_stereo_1` and
            // `tostereo_tostereo` are listed above for, just reported as a clean
            // `InputChannelCount` plan error instead of a CDP exit code. Each was confirmed by
            // hand against the real binary and a file of the right width, all from float32
            // input written by this app's own writer: pairex 8ch -> a 2ch file, mchshred mode 2
            // 6ch -> a 6ch file, spin stereo modes 2/3 stereo -> 8ch files.
            "mchshred_shred_2",
            "pairex_pairex",
            "spin_stereo_2",
            "spin_stereo_3",
            // Needs audio with real silences between events to find any ("NO SILENCES FOUND IN
            // FILE") — a constant-level tone has none, exactly the `envspeak_*` situation
            // above. Its two siblings (`mchanpan_mchanpan_4`/`_9`) take the same input and pass
            // here, which is what shows this to be the fixture's content and not the entry's
            // argv shape.
            "mchanpan_mchanpan_3",
        ];

        let (catalog, warnings) = crate::model::cdp::CdpCatalog::load(None);
        assert!(warnings.is_empty(), "catalog failed to parse: {warnings:?}");

        let runner = CdpRunner::new();
        let mut failures = Vec::new();
        // `ParamValue::FormantBufferRef` (CDP-Ext-Plan.md Phase 5) carries no data of its
        // own (see its doc comment) — same "app layer injects the real bytes after plan_job"
        // scheme as production (`App::cdp_run`, once built), just done here instead against
        // a real Formant/Snapshot buffer extracted from this fixture via the real binaries,
        // since a fake byte blob would fail immediately as an unparseable formant file
        // rather than exercising the argv shape this test actually cares about. Computed
        // lazily (`Option`, filled on first need) and cached rather than up front, so a
        // catalog with no `FormantBufferRef` params at all (true today, before this Phase 5
        // work) pays zero extra cost.
        let mut formant_buffer_bytes: Option<Vec<u8>> = None;
        let mut snapshot_buffer_bytes: Option<Vec<u8>> = None;
        for (i, def) in catalog.processes.iter().enumerate() {
            if KNOWN_FIXTURE_FAILURES.contains(&def.key.as_str()) {
                continue;
            }
            // A `required_envelope` param has no meaningful `ParamValue::Number` default —
            // its argv token must always be a breakpoint textfile path (see
            // `ParamDef::required_envelope`'s doc comment) — so drive it with a 2-point line
            // at the param's own default value, spanning this fixture's real duration (an
            // arbitrary/mismatched duration, e.g. the placeholder `1.0` the UI's own never-
            // opened-editor state would use, is exactly the kind of thing this smoke test
            // exists to catch before a real user does). 3 points with a middle bump rather
            // than a straight 2-point line — mirrors `App::open_cdp_envelope_editor`'s own
            // starting shape (see that fn's doc comment): at least one real CDP process
            // (`fractal wave`/`spectrum`'s Shape) hangs indefinitely on *any* straight
            // 2-point line, so testing with one here would just as easily hang the smoke
            // test itself.
            let duration_secs = len_samples as f64 / sample_rate as f64;
            let values: Vec<_> = def
                .params
                .iter()
                .map(|p| {
                    if p.required_envelope {
                        let crate::model::cdp::ParamKind::Number { default, min, max, step, .. } = &p.kind else {
                            panic!("{}: required_envelope param {:?} is not a Number kind", def.key, p.name);
                        };
                        let bumped = if default + step <= *max { default + step } else { default - step };
                        crate::model::cdp::ParamValue::Breakpoints(vec![
                            (0.0, *default),
                            (duration_secs / 2.0, bumped.clamp(*min, *max)),
                            (duration_secs, *default),
                        ])
                    } else if p.required_list {
                        // Mirrors `App::open_cdp_list_editor`'s own never-opened seeding: a
                        // single entry at the param's own default value — plain lists have
                        // no known analogue of the required_envelope hang above (no reports,
                        // no interpolation to go pathological on), so unlike the branch
                        // above there's no reason to seed more than one entry here.
                        let crate::model::cdp::ParamKind::Number { default, .. } = &p.kind else {
                            panic!("{}: required_list param {:?} is not a Number kind", def.key, p.name);
                        };
                        crate::model::cdp::ParamValue::List(vec![*default])
                    } else if p.range_scales_with_input_duration {
                        // `min`/`max`/`default` are multipliers of the input duration here,
                        // not seconds — the app scales them in `App::cdp_fields_for`, so the
                        // harness has to do the same or it would send a raw multiplier as a
                        // duration and test nothing real.
                        let crate::model::cdp::ParamKind::Number { default, min, max, .. } = &p.kind
                        else {
                            panic!("{}: range_scales_with_input_duration param {:?} is not a Number kind", def.key, p.name);
                        };
                        let lo = min * duration_secs + 0.01;
                        let hi = (max * duration_secs - 0.01).max(lo);
                        crate::model::cdp::ParamValue::Number((default * duration_secs).clamp(lo, hi))
                    } else if p.default_from_dc_offset {
                        // The app pre-fills this with the negated mean of the real selection;
                        // this fixture is a symmetric sine, so that rounds to zero and the
                        // fallback (one `step`) is what it would actually send. Zero is the
                        // one value `housekeep extract 4` refuses outright.
                        let crate::model::cdp::ParamKind::Number { step, .. } = &p.kind else {
                            panic!("{}: default_from_dc_offset param {:?} is not a Number kind", def.key, p.name);
                        };
                        crate::model::cdp::ParamValue::Number(*step)
                    } else {
                        p.kind.default_value()
                    }
                })
                .collect();
            // The declared minimum, which for every non-variadic kind *is* the exact arity
            // (`ProcessDef::input_arity`). Deliberately the minimum and not more for a
            // variadic process: several of them cross-check the file count against a
            // companion datafile's own row count (`tesselate`'s two data lines, `crystal
            // rotate`'s vertex triples), and `ParamKind::default_value` seeds exactly one
            // row — so any count above the floor would fail on that mismatch rather than on
            // anything this test is trying to exercise. Every input is the same fixture
            // buffer, which also satisfies `repair repair`'s "paired files must be the same
            // size" check for free.
            let (input_count, ..) = def.input_arity();
            let inputs_spec = vec![input.clone(); input_count];

            let mut planned = match crate::model::cdp::plan_job(
                def,
                &values,
                &inputs_spec,
                &crate::model::cdp::PvocSettings::default(),
            ) {
                Ok(planned) => planned,
                Err(crate::model::cdp::PlanError::UnsupportedInV1 { .. }) => continue,
                Err(e) => {
                    failures.push(format!("{}: plan_job failed: {e:?}", def.key));
                    continue;
                }
            };

            for param in &def.params {
                let crate::model::cdp::ParamKind::FormantBufferRef { buffer_kind, relative_name } = &param.kind
                else {
                    continue;
                };
                let bytes = match buffer_kind {
                    crate::model::formant::FormantBufferKind::Formant => formant_buffer_bytes
                        .get_or_insert_with(|| {
                            run_smoke_prereq_job(
                                &runner,
                                &cdp_dir,
                                crate::model::cdp::plan_extract_formants(&crate::model::cdp::PvocSettings::default(), crate::model::cdp::FormantExtractionMode::PitchWise(8)),
                                vec![channels.clone()],
                                sample_rate,
                                8_000,
                            )
                            .formant_buffer_bytes
                            .expect("formants get should produce formant_buffer_bytes")
                        })
                        .clone(),
                    crate::model::formant::FormantBufferKind::Snapshot => {
                        if snapshot_buffer_bytes.is_none() {
                            let formant_bytes = formant_buffer_bytes.get_or_insert_with(|| {
                                run_smoke_prereq_job(
                                    &runner,
                                    &cdp_dir,
                                    crate::model::cdp::plan_extract_formants(&crate::model::cdp::PvocSettings::default(), crate::model::cdp::FormantExtractionMode::PitchWise(8)),
                                    vec![channels.clone()],
                                    sample_rate,
                                    8_000,
                                )
                                .formant_buffer_bytes
                                .expect("formants get should produce formant_buffer_bytes")
                            });
                            snapshot_buffer_bytes = Some(
                                run_smoke_prereq_job(
                                    &runner,
                                    &cdp_dir,
                                    crate::model::cdp::plan_oneform_get(formant_bytes, duration_secs / 2.0),
                                    Vec::new(),
                                    sample_rate,
                                    8_001,
                                )
                                .formant_buffer_bytes
                                .expect("oneform get should produce formant_buffer_bytes"),
                            );
                        }
                        snapshot_buffer_bytes.clone().unwrap()
                    }
                };
                planned.binary_input_files.push((relative_name.clone(), bytes));
            }

            runner.submit(Job {
                id: 10_000 + i as u64,
                cdp_dir: cdp_dir.clone(),
                planned,
                inputs: vec![channels.clone(); input_count],
                input_sample_rate: sample_rate,
                purpose: JobPurpose::Apply,
            });
            let CdpEvent::Finished { result, .. } = recv_finished(&runner, Duration::from_secs(30))
            else {
                unreachable!()
            };
            match result {
                Err(e) => failures.push(format!("{}: {e:?}", def.key)),
                // Exit 0 alone used to be the whole assertion, which let an entry that runs
                // cleanly and produces *nothing* pass unnoticed -- that is how `spec_grab`
                // (a single grabbed analysis window always synthesises to zero frames) sat in
                // the catalog as a user-facing process. Checking the result is non-empty
                // closes that hole; see KNOWN_EMPTY_OUTPUT for the one legitimate exception.
                Ok(out) if !KNOWN_EMPTY_OUTPUT.contains(&def.key.as_str()) => {
                    let has_audio = out.results.iter().any(|buf| buf.iter().any(|ch| !ch.is_empty()));
                    if !has_audio && out.curve_points.is_none() && out.formant_buffer_bytes.is_none() {
                        failures.push(format!("{}: ran clean but produced no output at all", def.key));
                    }
                }
                Ok(_) => {}
            }
        }

        assert!(
            failures.is_empty(),
            "{} of {} catalog entries failed:\n{}",
            failures.len(),
            catalog.processes.len(),
            failures.join("\n")
        );
    }
}

