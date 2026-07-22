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

use crate::model::cdp::pipeline::{parse_ana_decfactor, window_count_from_decfactor, PlannedJob};
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
    let temp_dir = std::env::temp_dir()
        .join(format!("tui-wave-cdp-{}-{}-{seq}", std::process::id(), job.id));
    if std::fs::create_dir_all(&temp_dir).is_err() {
        return Err(CdpError::Spawn {
            step: "setup".into(),
            message: format!("failed to create temp dir {}", temp_dir.display()),
        });
    }

    let result = run_job_body(job, events, cancel, &temp_dir);
    let _ = std::fs::remove_dir_all(&temp_dir);
    result
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

    load_outputs(job, temp_dir)
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
        let source = job.inputs.get(spec.input_index).map(Vec::as_slice).unwrap_or(&[]);
        let channels: Vec<Vec<f32>> = spec
            .source_channels
            .iter()
            .map(|&ch| source.get(ch).cloned().unwrap_or_default())
            .collect();
        let doc = Document { channels, sample_rate: job.input_sample_rate, ..Default::default() };
        let path = temp_dir.join(&spec.relative_name);
        save_wav_with(&doc, &path, bit_depth, false).map_err(|e| CdpError::Spawn {
            step: format!("write {}", spec.relative_name),
            message: e.to_string(),
        })?;
    }
    Ok(())
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

/// Patches the placeholder(s) for `PercentOfAnaWindowCount` params (see CDP-PLAN.md Phase 0
/// spike S5) with their real values, computed from the `.ana` file each entry's preceding
/// `pvoc anal` step produced. A no-op for every job except the one process in the catalog
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

fn run_step(
    cdp_dir: &Path,
    bin: &str,
    args: &[String],
    label: &str,
    temp_dir: &Path,
    cancel: &AtomicBool,
) -> Result<(), CdpError> {
    let bin_path = cdp_dir.join(bin);
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
            results: Vec::new(),
            sample_rate: job.input_sample_rate,
            curve_points: Some(points),
            curve_binary_template,
            formant_buffer_bytes: None,
        });
    }
    if let Some(relative_name) = &job.planned.output_formant_buffer {
        let path = temp_dir.join(relative_name);
        let bytes = std::fs::read(&path).map_err(|e| CdpError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        return Ok(JobOutput {
            results: Vec::new(),
            sample_rate: job.input_sample_rate,
            curve_points: None,
            curve_binary_template: None,
            formant_buffer_bytes: Some(bytes),
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

    // CDP's per-channel outputs can differ by a sample or two from rounding; pad shorter
    // channels with silence rather than leaving channels out of sync.
    let max_len = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    for c in &mut channels {
        c.resize(max_len, 0.0);
    }

    Ok(JobOutput { results: vec![channels], sample_rate, curve_points: None, curve_binary_template: None, formant_buffer_bytes: None })
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
    Ok(JobOutput { results, sample_rate, curve_points: None, curve_binary_template: None, formant_buffer_bytes: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::pipeline::{Invocation, OutputWavSpec, TempWavSpec};
    use std::time::Instant;

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
            input_files: vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0] }],
            output_files: vec![OutputWavSpec {
                relative_name: output_relative_name.into(),
                dest_channels: vec![0],
            }],
            glob_output: None,
            output_curve: None,
            output_curve_binary_template: None, output_formant_buffer: None,
            brk_files: Vec::new(),
            binary_input_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: false,
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
            }],
            output_files: Vec::new(),
            glob_output: Some(crate::model::cdp::pipeline::GlobOutputSpec { prefix: "cutout".into() }),
            output_curve: None,
            output_curve_binary_template: None, output_formant_buffer: None,
            brk_files: Vec::new(),
            binary_input_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: false,
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
            output_curve_binary_template: None, output_formant_buffer: None,
            brk_files: vec![("curve_in.txt".into(), "0 220\n1 440".into())],
            binary_input_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: false,
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

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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

    #[test]
    fn blur_avrg_pvoc_round_trip_preserves_duration() {
        let cdp_dir = require_cdp!();
        let (channels, sample_rate) = mono_sine_channels();
        let len_samples = channels[0].len();

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("blur_avrg").expect("blur_avrg in catalog");

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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

        let input = crate::model::cdp::InputSpec { channels: 2, sample_rate, len_samples };
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

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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
            crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples },
            crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples },
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
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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

        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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
            crate::model::cdp::InputSpec { channels: 2, sample_rate, len_samples },
            crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples: mono[0].len() },
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
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };
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
    /// `PlanError::UnsupportedInV1` (currently only `morph_glide`) is a known, accepted gap,
    /// not a smoke-test failure. Collects every failure before asserting, so one bad entry's
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
        let input = crate::model::cdp::InputSpec { channels: 1, sample_rate, len_samples };

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
        //   housekeep_extract_4: "NO CHANGE to original sound file" against this specific
        //     mono fixture — content-dependent, not an argv-shape problem.
        //   modify_space_2, modify_space_4, tostereo_tostereo: explicitly stereo-only
        //     ("MIRROR/NARROW only works with STEREO input files"; tostereo: "must be
        //     stereo") — this harness only ever exercises mono input (see
        //     `input_count`/`inputs` above), so any process that hard-requires stereo will
        //     always fail here regardless of catalog correctness. Each verified correct by
        //     hand against `tests/fixtures/stereo_sine.wav` (real exit-0 runs), not a bug to
        //     chase.
        //   specfnu_specfnu_19: the CDP binary itself crashes ("double free or corruption")
        //     on this input — a CDP bug, nothing tui-wave's plan/argv can work around.
        // And two pre-existing (not catalog_extra.toml's) bugs found the same way: the
        // machine-generated catalog.toml (regenerate via
        // scripts/convert_soundthread_catalog.py, don't hand-edit) has a default outside
        // CDP's actually-enforced range for extend_scramble_1 (0.02 vs 0.031-0.985) and
        // modify_brassage_4 (2500 vs 0-2000).
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
            "housekeep_extract_4",
            "modify_brassage_4",
            "modify_space_2",
            "modify_space_4",
            "specfnu_specfnu_19",
            "tostereo_tostereo",
            // psow_interp requires each input be a pre-grabbed single grain (e.g. via
            // psow_grab with duration 0) -- fed an ordinary recording, the real binary
            // hard-rejects it: "File 1 is not a valid pitch-sync grain file". A
            // fixture-content issue, not a catalog bug (found while cataloging the psow
            // family, 2026-07-15).
            "psow_interp",
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
                    } else {
                        p.kind.default_value()
                    }
                })
                .collect();
            let input_count = match def.input {
                crate::model::cdp::IoKind::None | crate::model::cdp::IoKind::Curve => 0,
                crate::model::cdp::IoKind::Wav
                | crate::model::cdp::IoKind::Ana
                | crate::model::cdp::IoKind::WavGlob => 1,
                crate::model::cdp::IoKind::DualWav | crate::model::cdp::IoKind::DualAna => 2,
            };
            let inputs_spec = vec![input; input_count];

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
            if let Err(e) = result {
                failures.push(format!("{}: {e:?}", def.key));
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
