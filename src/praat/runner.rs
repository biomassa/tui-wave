//! Runs a `PraatPlannedJob` (see `model::praat::plan`) on a dedicated thread, mirroring
//! `cdp::runner`'s thread + crossbeam-channel pattern: the UI submits a job and polls `events`
//! once per frame, never blocking on the Praat subprocess.
//!
//! Deliberately a parallel implementation rather than a shared abstraction over
//! `cdp::runner`. The two differ in enough small ways — one binary instead of one per process,
//! a generated script instead of argv, a single step instead of a chain, and a hard timeout
//! that CDP has no need for — that factoring out the common half would leave a base with a
//! parameter for each difference, and would put the working CDP path at risk for the benefit of
//! code that is a few dozen lines either way.
//!
//! ## The timeout is the material difference
//!
//! CDP binaries exit on their own. Praat scripts do not necessarily: 32 of the plugin's scripts
//! call `Play` unconditionally, which blocks for the audio's real-time duration, and a script
//! that reaches an interactive construct in batch mode can block forever. There is **no safe
//! way to suppress this from outside the process** — pointing `PULSE_SERVER` at a dead socket
//! makes Praat hang rather than fail, which is worse — so the guard has to be a wall-clock
//! limit here, plus passing `0` to each script's own Play/Draw form fields (which the catalog
//! does, by defaulting those parameters off).

use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::cdp::runner::JobPurpose;
use crate::model::document::Document;
use crate::model::io::{load_wav, save_wav_with, BitDepth};
use crate::model::praat::plan::PraatPlannedJob;

/// How often a spawned Praat is polled for exit, cancellation and timeout. Same cadence as the
/// CDP runner, for the same reason: frequent enough that Esc feels instant, cheap enough to
/// ignore.
const POLL_INTERVAL: Duration = Duration::from_millis(30);

/// Wall-clock limit for one Praat run before it is killed.
///
/// Generous on purpose. A granular or convolution script on a long selection legitimately takes
/// tens of seconds, and a script that plays its result adds the selection's own duration on top;
/// the limit exists to catch a script that will *never* return, not to bound slow ones.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// The executable name looked up on `PATH` when no explicit path is configured. Unlike CDP —
/// whose ~250 binaries are not on anyone's `PATH` and which therefore *requires* a configured
/// directory — Praat is packaged on Arch, Debian/Ubuntu and Homebrew, so the overwhelmingly
/// common case needs no configuration at all.
pub const DEFAULT_PRAAT_BIN: &str = "praat";

/// Everything the runner needs to execute one Praat job.
pub struct PraatJob {
    pub id: u64,
    /// The Praat executable — either a configured absolute path or the bare
    /// [`DEFAULT_PRAAT_BIN`], which `Command` resolves against `PATH`.
    pub praat_bin: PathBuf,
    pub planned: PraatPlannedJob,
    /// One deinterleaved channel set per input, in the order the script expects them selected.
    /// `inputs[0]` is always the selection being processed; a `DualWav` process adds a second
    /// whole buffer the user picked.
    pub inputs: Vec<Vec<Vec<f32>>>,
    pub input_sample_rate: u32,
    pub purpose: JobPurpose,
    pub timeout: Duration,
    /// A Praat preferences directory this app owns, containing a `plugin_AudioTools` symlink to
    /// the checkout — see `prepare_prefs_dir`. `None` runs Praat against its own default
    /// preferences folder, which is fine for every process that does not chain siblings.
    pub prefs_dir: Option<PathBuf>,
}

/// The audio a finished Praat job produced.
#[derive(Debug)]
pub struct PraatJobOutput {
    pub result: Vec<Vec<f32>>,
    /// Read back from the output file rather than assumed: plenty of the plugin's scripts
    /// resample, and a few synthesise at a fixed rate regardless of the input's.
    pub sample_rate: u32,
}

#[derive(Debug)]
pub enum PraatError {
    /// Praat could not be started at all — almost always "not installed" or a wrong configured
    /// path, which is why the message names the binary that was tried.
    Spawn { bin: String, message: String },
    /// Praat exited non-zero. It uses **255 for every script error**, so the code carries no
    /// information and the captured stderr — which does name the failing line and its source
    /// text — is the whole diagnostic.
    NonZeroExit { code: Option<i32>, output: String },
    /// Praat exited 0 but wrote no output file, or an empty one.
    NoOutput,
    OutputRead { path: String, message: String },
    Cancelled,
    TimedOut { seconds: u64 },
}

impl std::fmt::Display for PraatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PraatError::Spawn { bin, message } => {
                write!(f, "could not start Praat ({bin}): {message}")
            }
            PraatError::NonZeroExit { code, output } => {
                let code = code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
                write!(f, "Praat exited {code}:\n{output}")
            }
            PraatError::NoOutput => write!(f, "the script produced no output file"),
            PraatError::OutputRead { path, message } => {
                write!(f, "could not read {path}: {message}")
            }
            PraatError::Cancelled => write!(f, "cancelled"),
            PraatError::TimedOut { seconds } => {
                write!(f, "the script did not finish within {seconds}s and was stopped")
            }
        }
    }
}

pub enum PraatEvent {
    Started { job: u64, label: String },
    Finished { job: u64, purpose: JobPurpose, result: Result<PraatJobOutput, PraatError> },
}

/// Owns the Praat worker thread.
pub struct PraatRunner {
    job_tx: Sender<PraatJob>,
    pub events: Receiver<PraatEvent>,
    cancel: Arc<AtomicBool>,
}

impl PraatRunner {
    pub fn new() -> Self {
        let (job_tx, job_rx) = unbounded::<PraatJob>();
        let (event_tx, event_rx) = unbounded::<PraatEvent>();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();

        thread::spawn(move || {
            for job in job_rx {
                cancel_for_thread.store(false, Ordering::Relaxed);
                let id = job.id;
                let purpose = job.purpose;
                let _ = event_tx.send(PraatEvent::Started {
                    job: id,
                    label: job.planned.label.clone(),
                });
                let result = run_job(&job, &cancel_for_thread);
                let _ = event_tx.send(PraatEvent::Finished { job: id, purpose, result });
            }
        });

        Self { job_tx, events: event_rx, cancel }
    }

    pub fn submit(&self, job: PraatJob) {
        let _ = self.job_tx.send(job);
    }

    /// Best-effort cancellation of the running job; takes effect at the next poll tick.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Default for PraatRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Names every Praat job directory, so the sweep below can recognise one. Distinct from the CDP
/// runner's prefix so neither sweep can delete the other's live working directories.
const TEMP_DIR_PREFIX: &str = "tui-wave-praat-";

/// Deletes Praat job directories left behind by a previous run of *this* PID — see
/// `cdp::runner::sweep_stale_temp_dirs` for the PID-recycling hazard this closes.
pub fn sweep_stale_temp_dirs() {
    sweep_stale_temp_dirs_in(&std::env::temp_dir());
}

fn sweep_stale_temp_dirs_in(dir: &Path) {
    let own = format!("{TEMP_DIR_PREFIX}{}-", std::process::id());
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&own) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes its directory when dropped, including while a panic unwinds.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_job(job: &PraatJob, cancel: &AtomicBool) -> Result<PraatJobOutput, PraatError> {
    // The process-wide counter (not just `job.id`) keeps concurrent runners' temp dirs distinct
    // even when two jobs share an id — ids are only unique per `App`, and the test suite runs
    // many runners in one process.
    static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!(
        "{TEMP_DIR_PREFIX}{}-{}-{seq}",
        std::process::id(),
        job.id
    ));
    if std::fs::create_dir_all(&temp_dir).is_err() {
        return Err(PraatError::Spawn {
            bin: job.praat_bin.display().to_string(),
            message: format!("failed to create temp dir {}", temp_dir.display()),
        });
    }
    let _guard = TempDirGuard(temp_dir.clone());
    run_job_body(job, cancel, &temp_dir)
}

fn run_job_body(
    job: &PraatJob,
    cancel: &AtomicBool,
    temp_dir: &Path,
) -> Result<PraatJobOutput, PraatError> {
    let output_path = temp_dir.join(&job.planned.output_name);
    let driver_path = temp_dir.join(&job.planned.driver_name);

    // Float32 in: Praat reads IEEE float WAV natively, so the input leg is lossless and the
    // working buffer needs no conversion. (Only the return leg quantizes — Praat's writer
    // cannot emit float at all.)
    let mut input_paths = Vec::with_capacity(job.planned.input_names.len());
    for (index, name) in job.planned.input_names.iter().enumerate() {
        let path = temp_dir.join(name);
        let channels = job.inputs.get(index).cloned().unwrap_or_default();
        let doc = Document {
            channels,
            sample_rate: job.input_sample_rate,
            ..Default::default()
        };
        save_wav_with(&doc, &path, BitDepth::Float32, false).map_err(|e| PraatError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        input_paths.push(path);
    }

    std::fs::write(&driver_path, &job.planned.driver_source).map_err(|e| PraatError::OutputRead {
        path: driver_path.display().to_string(),
        message: e.to_string(),
    })?;

    run_praat(job, &driver_path, &input_paths, &output_path, temp_dir, cancel)?;

    match std::fs::metadata(&output_path) {
        Ok(meta) if meta.len() > 0 => {}
        _ => return Err(PraatError::NoOutput),
    }
    let out = load_wav(&output_path).map_err(|e| PraatError::OutputRead {
        path: output_path.display().to_string(),
        message: e.to_string(),
    })?;

    Ok(PraatJobOutput { sample_rate: out.sample_rate, result: out.channels })
}

fn run_praat(
    job: &PraatJob,
    driver_path: &Path,
    input_paths: &[PathBuf],
    output_path: &Path,
    temp_dir: &Path,
    cancel: &AtomicBool,
) -> Result<(), PraatError> {
    // `--run` is mandatory, not merely conventional: Praat's own manual says that without it
    // the behaviour is unspecified when output is redirected, and it may start the GUI and
    // *open* the script rather than run it.
    //
    // `--no-pref-files` keeps a headless run from reading or rewriting the user's own Praat
    // preferences, and `--no-plugins` stops any plugin they have installed from executing its
    // `setup.praat` on every single job — including praatAudioTools itself, which this
    // integration reaches by absolute path rather than by installing.
    let mut command = StdCommand::new(&job.praat_bin);
    command.arg("--run").arg("--no-pref-files").arg("--no-plugins");
    // Point `preferencesDirectory$` at a directory we own (see `prepare_prefs_dir`). A handful
    // of scripts — the Vector Chain family, which chain several processes together — locate
    // their sibling scripts through that variable, and without this they fail with
    // `Cannot open file ".../.praat-dir/plugin_AudioTools/..."`. Redirecting is what lets those
    // work *without* installing anything into the user's own Praat preferences folder, where a
    // `plugin_AudioTools` of their own may already live.
    if let Some(prefs) = &job.prefs_dir {
        command.arg(format!("--pref-dir={}", prefs.display()));
    }
    let mut child = command
        .arg(driver_path)
        .args(input_paths)
        .arg(output_path)
        .current_dir(temp_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PraatError::Spawn {
            bin: job.praat_bin.display().to_string(),
            message: e.to_string(),
        })?;

    // Drained on helper threads so a chatty script can't deadlock us by filling a pipe buffer
    // while we're polling `try_wait` instead of reading. Praat is chatty by default: most of
    // these scripts write a progress report to the Info window, which lands on stdout.
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

    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PraatError::Cancelled);
        }
        if started.elapsed() >= job.timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PraatError::TimedOut { seconds: job.timeout.as_secs() });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(e) => {
                return Err(PraatError::Spawn {
                    bin: job.praat_bin.display().to_string(),
                    message: e.to_string(),
                })
            }
        }
    };

    let stdout_text = stdout_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr_text = stderr_handle.and_then(|h| h.join().ok()).unwrap_or_default();

    if !status.success() {
        return Err(PraatError::NonZeroExit {
            code: status.code(),
            // stderr last: it carries the actual error, its line number and the offending
            // source text, so it should be the part nearest the end of a truncated display.
            output: format!("{stdout_text}{stderr_text}"),
        });
    }
    Ok(())
}

/// The Praat executable to use for a configured setting — the setting itself when it is set,
/// otherwise the bare name for `PATH` lookup.
pub fn praat_bin_for(configured: &str) -> PathBuf {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        PathBuf::from(DEFAULT_PRAAT_BIN)
    } else {
        PathBuf::from(trimmed)
    }
}

/// Check that Praat can actually be run, returning its version banner.
///
/// `praat --version` prints one line and exits 0, which makes it the cheapest possible probe —
/// the same role `AudioEngine::try_new`'s device probe plays, and for the same reason: Praat
/// support is optional, so this must report a clean failure rather than being allowed to blow
/// up a code path that assumed it was there.
pub fn probe_praat(configured: &str) -> Result<String, String> {
    let bin = praat_bin_for(configured);
    let output = StdCommand::new(&bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", bin.display()))?;
    if !output.status.success() {
        return Err(format!("{} --version exited {:?}", bin.display(), output.status.code()));
    }
    let banner = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if banner.is_empty() {
        return Err(format!("{} --version printed nothing", bin.display()));
    }
    Ok(banner)
}

/// The plugin folder name Praat expects inside a preferences directory. Fixed by Praat's own
/// convention (`plugin_` prefix) and by the scripts, which build sibling paths as
/// `preferencesDirectory$ + "/plugin_AudioTools/..."`.
const PLUGIN_DIR_NAME: &str = "plugin_AudioTools";

/// Build an app-owned Praat preferences directory whose `plugin_AudioTools` entry points at
/// `audiotools_dir`, and return it for `--pref-dir`.
///
/// Exists for the Vector Chain scripts, which call sibling scripts through
/// `preferencesDirectory$` and therefore only work when the plugin is reachable under that
/// name. The obvious fix is to install a symlink into the user's real preferences folder
/// (`~/.praat-dir` on Linux); this deliberately does not, because that folder belongs to their
/// Praat installation and may already hold a `plugin_AudioTools` of their own — a different
/// version, or one they edited. Redirecting Praat to a directory we own gets the same result
/// and cannot collide with anything.
///
/// Best-effort: on failure the caller runs without `--pref-dir`, which costs only the chained
/// processes rather than the whole backend. Symlinks are not portable everywhere, so a failure
/// to create one is not treated as fatal.
pub fn prepare_prefs_dir(state_dir: &Path, audiotools_dir: &Path) -> Option<PathBuf> {
    let prefs = state_dir.join("praat-prefs");
    std::fs::create_dir_all(&prefs).ok()?;
    let link = prefs.join(PLUGIN_DIR_NAME);

    // Re-point a stale link rather than leaving it: the configured checkout can move between
    // runs, and a link to a path that no longer exists is worse than none at all.
    let current = std::fs::read_link(&link).ok();
    if current.as_deref() == Some(audiotools_dir) {
        return Some(prefs);
    }
    if link.exists() || current.is_some() {
        let _ = std::fs::remove_file(&link);
    }

    #[cfg(unix)]
    let created = std::os::unix::fs::symlink(audiotools_dir, &link).is_ok();
    #[cfg(windows)]
    let created = std::os::windows::fs::symlink_dir(audiotools_dir, &link).is_ok();
    #[cfg(not(any(unix, windows)))]
    let created = false;

    created.then_some(prefs)
}

/// Check that `dir` looks like a praatAudioTools checkout.
///
/// Distinguishes "empty" from "wrong", because the overwhelmingly likely first-run failure is a
/// submodule that was never initialised — a directory that exists and is empty. Telling someone
/// to check their path when the fix is one `git submodule` command would send them looking in
/// the wrong place entirely.
pub fn validate_audiotools_dir(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let empty = std::fs::read_dir(dir).map(|mut e| e.next().is_none()).unwrap_or(false);
    if empty {
        return Err(format!(
            "{} is empty — if it is the bundled submodule, run: \
             git submodule update --init",
            dir.display()
        ));
    }
    // Two of the plugin's category directories, chosen the way `cdp::validate_cdp_dir` picks
    // sentinel binaries: present in every real checkout, and unlikely together by accident.
    for sentinel in ["Distortion", "Reverb"] {
        if !dir.join(sentinel).is_dir() {
            return Err(format!(
                "{} does not look like a praatAudioTools checkout (no {sentinel}/ directory)",
                dir.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::praat::plan::{input_wav_name, DRIVER_SCRIPT, OUTPUT_WAV};

    /// Skips a test that needs a real Praat rather than failing it, mirroring
    /// `cdp::runner`'s `require_cdp!`.
    macro_rules! require_praat {
        () => {
            if probe_praat("").is_err() {
                eprintln!("skipping: no praat on PATH");
                return;
            }
        };
    }

    fn job_with(driver_source: &str, timeout: Duration) -> PraatJob {
        PraatJob {
            id: 0,
            praat_bin: praat_bin_for(""),
            planned: PraatPlannedJob {
                driver_name: DRIVER_SCRIPT.into(),
                driver_source: driver_source.into(),
                input_names: vec![input_wav_name(1)],
                output_name: OUTPUT_WAV.into(),
                script_path: PathBuf::from("/unused"),
                label: "test".into(),
            },
            inputs: vec![vec![vec![0.0f32; 1000], vec![0.0f32; 1000]]],
            input_sample_rate: 44_100,
            purpose: JobPurpose::Apply,
            timeout,
            prefs_dir: None,
        }
    }

    fn run(job: &PraatJob) -> Result<PraatJobOutput, PraatError> {
        run_job(job, &AtomicBool::new(false))
    }

    #[test]
    fn praat_bin_falls_back_to_a_path_lookup_when_unset() {
        assert_eq!(praat_bin_for(""), PathBuf::from("praat"));
        assert_eq!(praat_bin_for("   "), PathBuf::from("praat"));
        assert_eq!(praat_bin_for("/opt/praat/praat"), PathBuf::from("/opt/praat/praat"));
    }

    #[test]
    fn a_missing_binary_reports_a_spawn_error_naming_it() {
        let mut job = job_with("writeInfoLine: 1\n", DEFAULT_TIMEOUT);
        job.praat_bin = PathBuf::from("/nonexistent/praat-does-not-exist");
        match run(&job) {
            Err(PraatError::Spawn { bin, .. }) => {
                assert!(bin.contains("praat-does-not-exist"), "{bin}");
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    /// The round trip that matters: f32 in, int32 back, same length and channel count.
    #[test]
    fn a_pass_through_driver_round_trips_the_audio() {
        require_praat!();
        let job = job_with(
            "form Driver\n    infile Input_file\n    outfile Output_file\nendform\n\
             snd = Read from file: input_file$\n\
             selectObject: snd\n\
             Save as 32-bit WAV file: output_file$\n",
            DEFAULT_TIMEOUT,
        );
        let out = run(&job).expect("round trip");
        assert_eq!(out.sample_rate, 44_100);
        assert_eq!(out.result.len(), 2);
        assert_eq!(out.result[0].len(), 1000);
    }

    /// Praat uses 255 for every script error, so the captured stderr is the whole diagnostic —
    /// this pins that we actually keep it.
    #[test]
    fn a_script_error_is_reported_with_its_captured_stderr() {
        require_praat!();
        let job = job_with(
            "form Driver\n    infile Input_file\n    outfile Output_file\nendform\n\
             Read from file: \"/definitely/not/here.wav\"\n",
            DEFAULT_TIMEOUT,
        );
        match run(&job) {
            Err(PraatError::NonZeroExit { code, output }) => {
                assert_eq!(code, Some(255));
                assert!(output.contains("Error"), "{output}");
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    /// Exit 0 with nothing written must not be mistaken for success.
    #[test]
    fn exiting_cleanly_without_writing_output_is_an_error() {
        require_praat!();
        let job = job_with(
            "form Driver\n    infile Input_file\n    outfile Output_file\nendform\n\
             writeInfoLine: \"did nothing\"\n",
            DEFAULT_TIMEOUT,
        );
        assert!(matches!(run(&job), Err(PraatError::NoOutput)));
    }

    /// The guard that CDP's runner has no equivalent of. A script that never returns must be
    /// killed rather than hanging the worker thread forever.
    #[test]
    fn a_script_that_never_finishes_is_killed_by_the_timeout() {
        require_praat!();
        let job = job_with(
            "form Driver\n    infile Input_file\n    outfile Output_file\nendform\n\
             while 1 = 1\n    x = 1\nendwhile\n",
            Duration::from_secs(2),
        );
        let started = Instant::now();
        match run(&job) {
            Err(PraatError::TimedOut { seconds }) => assert_eq!(seconds, 2),
            other => panic!("expected TimedOut, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(30), "timeout did not fire promptly");
    }

    #[test]
    fn cancelling_stops_a_long_running_script() {
        require_praat!();
        let job = job_with(
            "form Driver\n    infile Input_file\n    outfile Output_file\nendform\n\
             while 1 = 1\n    x = 1\nendwhile\n",
            DEFAULT_TIMEOUT,
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            flag.store(true, Ordering::Relaxed);
        });
        assert!(matches!(run_job(&job, &cancel), Err(PraatError::Cancelled)));
    }

    /// The temp directory must not outlive the job, however the job ended — including while a
    /// panic unwinds, which is the case a plain "remove it at the end of `run_job`" misses.
    ///
    /// Tests the guard directly rather than counting directories before and after a job: the
    /// suite runs these tests in parallel within one process, so every test's job directory
    /// shares this PID's prefix and a process-wide count races its siblings.
    #[test]
    fn the_temp_directory_is_removed_even_while_a_panic_unwinds() {
        let dir = std::env::temp_dir()
            .join(format!("{TEMP_DIR_PREFIX}{}-guard-test", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("leftover")).unwrap();

        let unwound = std::panic::catch_unwind({
            let dir = dir.clone();
            move || {
                let _guard = TempDirGuard(dir);
                panic!("simulated failure mid-job");
            }
        });

        assert!(unwound.is_err(), "the panic should have propagated");
        assert!(!dir.exists(), "temp dir survived a panic: {}", dir.display());
    }

    #[test]
    fn the_sweep_only_removes_this_processes_directories() {
        let dir = std::env::temp_dir().join(format!("praat-sweep-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mine = dir.join(format!("{TEMP_DIR_PREFIX}{}-1-0", std::process::id()));
        let theirs = dir.join(format!("{TEMP_DIR_PREFIX}999999-1-0"));
        let unrelated = dir.join("something-else");
        for d in [&mine, &theirs, &unrelated] {
            std::fs::create_dir_all(d).unwrap();
        }
        sweep_stale_temp_dirs_in(&dir);
        assert!(!mine.exists(), "own directory should be swept");
        assert!(theirs.exists(), "another PID's directory must be left alone");
        assert!(unrelated.exists(), "unrelated directory must be left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_uninitialised_submodule_is_reported_as_empty_not_as_wrong() {
        let dir = std::env::temp_dir().join(format!("praat-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = validate_audiotools_dir(&dir).unwrap_err();
        assert!(err.contains("git submodule update --init"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_without_the_plugin_layout_is_rejected() {
        let dir = std::env::temp_dir().join(format!("praat-wrong-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("unrelated")).unwrap();
        let err = validate_audiotools_dir(&dir).unwrap_err();
        assert!(err.contains("does not look like"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The symlink is what makes `preferencesDirectory$ + "/plugin_AudioTools/..."` resolve for
    /// the Vector Chain scripts, and it must land in a directory we own rather than the user's
    /// own Praat preferences folder.
    #[test]
    fn the_prefs_dir_links_the_plugin_under_the_name_praat_expects() {
        let state = std::env::temp_dir().join(format!("praat-prefs-{}", std::process::id()));
        let checkout = state.join("checkout");
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&checkout).unwrap();

        let prefs = prepare_prefs_dir(&state, &checkout).expect("prefs dir");
        let link = prefs.join(PLUGIN_DIR_NAME);
        assert!(link.exists(), "no plugin link at {}", link.display());
        assert_eq!(std::fs::read_link(&link).unwrap(), checkout);
        let _ = std::fs::remove_dir_all(&state);
    }

    /// The configured checkout can move between runs; a link left pointing at the old path
    /// would silently break every chained process.
    #[test]
    fn a_stale_plugin_link_is_repointed() {
        let state = std::env::temp_dir().join(format!("praat-restale-{}", std::process::id()));
        let old = state.join("old");
        let new = state.join("new");
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();

        prepare_prefs_dir(&state, &old).unwrap();
        let prefs = prepare_prefs_dir(&state, &new).unwrap();
        assert_eq!(std::fs::read_link(prefs.join(PLUGIN_DIR_NAME)).unwrap(), new);
        let _ = std::fs::remove_dir_all(&state);
    }

    /// End-to-end proof that the redirect actually unlocks the chained scripts: `chain_2` locates
    /// its siblings through `preferencesDirectory$` and fails outright without this.
    #[test]
    fn a_chained_script_finds_its_siblings_through_the_redirected_prefs_dir() {
        require_praat!();
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        let script = checkout.join("Vector Chain/chain_2.praat");
        if !script.is_file() {
            eprintln!("skipping: submodule not initialised");
            return;
        }
        let state = std::env::temp_dir().join(format!("praat-chain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let prefs = prepare_prefs_dir(&state, &checkout).expect("prefs dir");

        let mut job = job_with("", Duration::from_secs(90));
        job.prefs_dir = Some(prefs);
        // Two seconds of a real tone, not the tiny silent buffer the other tests use: this chain
        // runs a pitch analysis, and Praat refuses one on a buffer shorter than a few periods.
        let tone: Vec<f32> = (0..88_200)
            .map(|i| (i as f32 * 220.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.5)
            .collect();
        job.inputs = vec![vec![tone]];
        job.planned.driver_source = crate::model::praat::driver::driver_script(
            &script.to_string_lossy(),
            &[],
            1,
        )
        .unwrap();

        match run(&job) {
            Ok(out) => assert!(!out.result.is_empty(), "chained script produced no audio"),
            Err(err) => panic!("chained script failed: {err}"),
        }
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn a_real_checkout_validates() {
        let dir = std::env::temp_dir().join(format!("praat-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for c in ["Distortion", "Reverb"] {
            std::fs::create_dir_all(dir.join(c)).unwrap();
        }
        assert!(validate_audiotools_dir(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
