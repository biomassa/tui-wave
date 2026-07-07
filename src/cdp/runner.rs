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

#[derive(Debug)]
pub struct JobOutput {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
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
    let temp_dir =
        std::env::temp_dir().join(format!("tui-wave-cdp-{}-{}", std::process::id(), job.id));
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
    for spec in &job.planned.input_files {
        let source = job.inputs.get(spec.input_index).map(Vec::as_slice).unwrap_or(&[]);
        let channels: Vec<Vec<f32>> = spec
            .source_channels
            .iter()
            .map(|&ch| source.get(ch).cloned().unwrap_or_default())
            .collect();
        let doc = Document { channels, sample_rate: job.input_sample_rate, ..Default::default() };
        let path = temp_dir.join(&spec.relative_name);
        save_wav_with(&doc, &path, BitDepth::Float32, false).map_err(|e| CdpError::Spawn {
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

/// Patches the placeholder token for the `PercentOfAnaWindowCount` param (see
/// CDP-PLAN.md Phase 0 spike S5) with the real value, computed from the `.ana` file the
/// preceding `pvoc anal` step just produced. A no-op for every job except the one process
/// in the catalog that uses this scale (`blur_blur`'s "Blurring" param).
fn resolve_deferred_window_param(
    job: &Job,
    step_index: usize,
    temp_dir: &Path,
    args: &mut [String],
) -> Result<(), CdpError> {
    let Some(deferred) = &job.planned.deferred_window_param else { return Ok(()) };
    if deferred.step_index != step_index {
        return Ok(());
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
    let scaled = (f64::from(window_count) * deferred.percent / 100.0).max(1.0).round();
    let value_text = format!("{scaled}");
    args[deferred.arg_index] = match &deferred.flag {
        Some(flag) => format!("{flag}{value_text}"),
        None => value_text,
    };
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

    Ok(JobOutput { channels, sample_rate })
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

    fn empty_planned_job(steps: Vec<Invocation>, output_relative_name: &str) -> PlannedJob {
        PlannedJob {
            steps,
            input_files: vec![TempWavSpec { relative_name: "in.wav".into(), input_index: 0, source_channels: vec![0] }],
            output_files: vec![OutputWavSpec {
                relative_name: output_relative_name.into(),
                dest_channels: vec![0],
            }],
            brk_files: Vec::new(),
            deferred_window_param: None,
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
        assert_eq!(output.channels.len(), 1);
        assert_eq!(output.channels[0].len(), 4);
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
        assert_eq!(output.channels.len(), 1);
        let ratio = output.channels[0].len() as f64 / len_samples as f64;
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
        let ratio = output.channels[0].len() as f64 / len_samples as f64;
        assert!((ratio - 1.0).abs() < 0.1, "expected ~same duration after pvoc round-trip, got ratio {ratio}");
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
            id: 102,
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
            id: 103,
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
        let ratio = output.channels[0].len() as f64 / len_samples as f64;
        assert!((ratio - 2.0).abs() < 0.05, "joining a file to itself should ~double duration, got ratio {ratio}");
    }
}
