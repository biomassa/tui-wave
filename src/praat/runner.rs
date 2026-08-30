//! Runs a `PraatPlannedJob` (see `model::praat::plan`) on a dedicated thread, mirroring
//! `cdp::runner`'s thread + crossbeam-channel pattern: the UI submits a job and polls `events`
//! once per frame, never blocking on the Praat subprocess.
//!
//! Deliberately a parallel implementation rather than a shared abstraction over
//! `cdp::runner`. The two differ in enough small ways — one binary instead of one per process,
//! a generated script instead of argv, a single step instead of a chain, and a hard timeout
//! that CDP has no need for (and that one class of process must be exempt from) — that
//! factoring out the common half would leave a base with a
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
//!
//! The one exemption is a process that opens **its own window and waits for the user** — the
//! `py` group's Tk editors (see `ProcessDef::interactive`). A person drawing a spatial
//! trajectory is indistinguishable from a wedged script to a wall-clock limit, so those run
//! with no timeout at all and are stopped by Esc instead, which the same poll loop already
//! handles. They are not the `beginPause` hazard: that window belongs to Praat and segfaults
//! under `--run`, while these belong to a separate Python process and genuinely work.

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

/// Wall-clock limit for a run that also draws its visualization.
///
/// Headroom rather than a measured need: across a sample of the plugin's drawing processes the
/// figure cost about 0.1s on top of a median 0.5s run, and the slowest was 5s end to end. But
/// those were measured on one second of audio, the drawing code is the least-exercised part of
/// these scripts (nothing ran it before this feature existed), and a spectrogram panel scales
/// with the selection. Doubling the budget costs nothing when nothing hangs, and the alternative
/// — killing a run that was 8s from finishing — throws away the audio too.
pub const DRAWING_TIMEOUT: Duration = Duration::from_secs(240);

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
    /// The PNG an `IoKind::Photo` process reads, as the user picked it. `Some` exactly when
    /// `planned.photo_input` is set.
    ///
    /// Passed straight through to Praat rather than staged into the job's temp directory like
    /// the input WAVs are, because there is nothing to convert: Praat reads PNG natively and
    /// the picker only offers PNGs, so a copy would be pure I/O on a file that may be tens of
    /// megabytes. It is therefore the one input path the runner does not own — and the one it
    /// must not delete, which `TempDirGuard` never sees.
    pub photo_path: Option<PathBuf>,
    pub input_sample_rate: u32,
    pub purpose: JobPurpose,
    pub timeout: Duration,
    /// A Praat preferences directory this app owns, containing a `plugin_AudioTools` symlink to
    /// the checkout — see `prepare_prefs_dir`. `None` runs Praat against its own default
    /// preferences folder, which is fine for every process that does not chain siblings.
    pub prefs_dir: Option<PathBuf>,
    /// The app-owned Python venv's `bin`, prepended to the child's `PATH` — see
    /// [`python_venv_bin`]. `None` inherits `PATH` unchanged.
    pub python_venv_bin: Option<PathBuf>,
}

/// The audio a finished Praat job produced.
#[derive(Debug)]
pub struct PraatJobOutput {
    pub result: Vec<Vec<f32>>,
    /// Read back from the output file rather than assumed: plenty of the plugin's scripts
    /// resample, and a few synthesise at a fixed rate regardless of the input's.
    pub sample_rate: u32,
    /// What the script drew into Praat's Picture window, cropped and scaled for display, when
    /// the run asked for a drawing and produced one.
    ///
    /// `None` covers four different things on purpose — the run drew nothing, the save failed,
    /// the file was unreadable, the canvas came back blank — because the caller does the same
    /// thing in all four: carries on with the audio and offers no picture. A drawing is a
    /// bonus, never a result, so none of them is an error.
    pub picture: Option<image::RgbaImage>,
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

/// Turns a spawn failure into something actionable rather than an OS-phrased error. `praat`
/// (the default `praat_bin`, resolved by a bare-name PATH lookup — see `praat_bin_for`) is by
/// far the most common cause: nothing here installs Praat itself on any platform, only the
/// praatAudioTools scripts and, for the `py` group, a Python venv. Unlike `NotFound`, every
/// other `io::Error` kind (permission denied, not executable, ...) names something concrete
/// enough that the raw OS message is already the useful answer.
fn spawn_error_message(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "Praat not found. Install it (https://www.fon.hum.uva.nl/praat/) and either add it to \
         your PATH or set praat_bin in your config to point at it."
            .to_string()
    } else {
        e.to_string()
    }
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
    let picture_path = job.planned.picture_name.as_ref().map(|name| temp_dir.join(name));

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

    // A built-in process's script ships with the app rather than living in the submodule, so
    // there is no file to read — the text travels in the plan and is written here beside the
    // driver, which calls it by bare filename. See `model::praat::builtin` for why it is
    // embedded rather than installed.
    if let Some(builtin) = &job.planned.builtin_source {
        let path = temp_dir.join(&builtin.script_name);
        std::fs::write(&path, &builtin.source).map_err(|e| PraatError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
    }

    // Two independent reasons to run a rewritten *copy* of the script rather than the submodule's
    // own file, and a script may need both — `Semantic_timbre_retrieval` is a `py`-group script
    // whose corpus folder is hoisted out of a `chooseDirectory$` call. So the passes are applied
    // in sequence to one text, written once, under the one filename both plans name. They touch
    // disjoint lines (interpreter assignments; dialog statements), so the order is immaterial;
    // what would not be immaterial is writing the file twice and having the second copy win.
    if job.planned.python_rewrite.is_some() || job.planned.pause_rewrite.is_some() {
        let mut source = std::fs::read_to_string(&job.planned.script_path).map_err(|e| {
            PraatError::OutputRead {
                path: job.planned.script_path.display().to_string(),
                message: e.to_string(),
            }
        })?;
        let fail = |message: String| PraatError::OutputRead {
            path: job.planned.script_path.display().to_string(),
            message,
        };

        // A `py`-group script picks its own interpreter, and on macOS picks an absolute path that
        // `PATH` cannot influence — so the app-owned venv is bypassed and numpy/scipy/soundfile
        // are simply not there. See `model::praat::python`; the count is checked because a script
        // that resolves its interpreter some way the rewriter does not recognise would otherwise
        // fail later, on an import, with nothing pointing at the cause.
        if let Some(rewrite) = &job.planned.python_rewrite {
            // `defaultDirectory$` is pinned to the original script's folder as well, because the
            // copy runs from the job's temp directory and 33 of these scripts locate their `.py`
            // helper relative to themselves — `defaultDirectory$ + "/spat_binaural_bridge.py"`.
            // Without it the copy runs and then reports the helper missing.
            let original_dir = job
                .planned
                .script_path
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (rewritten, replaced) = crate::model::praat::python::rewrite_for_venv(
                &source,
                &rewrite.interpreter,
                &original_dir,
            );
            if replaced == 0 {
                return Err(fail(
                    "this script was expected to select a Python interpreter but none was \
                     found to repoint; it may need numpy/scipy/soundfile on the system \
                     interpreter instead"
                        .to_string(),
                ));
            }
            source = rewritten;
        }

        // A process whose settings live in a Praat `beginPause` dialog, or whose folder it would
        // ask for with `chooseDirectory$`, runs against the copy instead — neither modal can be
        // shown under `--run` at all, they segfault Praat outright. See `model::praat::rewrite`
        // for the substitution rules.
        if let Some(rewrite) = &job.planned.pause_rewrite {
            source = crate::model::praat::rewrite::rewrite_pause_blocks(
                &source,
                &rewrite.blocks,
                &rewrite.form_locks,
                rewrite.button_override,
            )
            .map_err(|e| fail(e.to_string()))?;
            source = crate::model::praat::rewrite::rewrite_directory_choosers(
                &source,
                &rewrite.directories,
            )
            .map_err(|e| fail(e.to_string()))?;
            // Unconditional, and a no-op on every script that does not carry the defect: it is
            // detected in the text rather than declared, so there is nothing to gate it on. See
            // `rewrite::repair_branch_scoped_variables`.
            source = crate::model::praat::rewrite::repair_branch_scoped_variables(&source);
        }

        let name = match (&job.planned.pause_rewrite, &job.planned.python_rewrite) {
            (Some(rewrite), _) => &rewrite.script_name,
            (None, Some(rewrite)) => &rewrite.script_name,
            (None, None) => unreachable!("guarded by the condition above"),
        };
        let path = temp_dir.join(name);
        std::fs::write(&path, source).map_err(|e| PraatError::OutputRead {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
    }

    std::fs::write(&driver_path, &job.planned.driver_source).map_err(|e| PraatError::OutputRead {
        path: driver_path.display().to_string(),
        message: e.to_string(),
    })?;

    run_praat(
        job,
        &driver_path,
        &input_paths,
        &output_path,
        picture_path.as_deref(),
        temp_dir,
        cancel,
    )?;

    match std::fs::metadata(&output_path) {
        Ok(meta) if meta.len() > 0 => {}
        _ => return Err(PraatError::NoOutput),
    }
    let out = load_wav(&output_path).map_err(|e| PraatError::OutputRead {
        path: output_path.display().to_string(),
        message: e.to_string(),
    })?;

    // Read *here*, not by handing the path back: the caller's `TempDirGuard` deletes this
    // directory the moment `run_job` returns. Every step is fallible-and-ignored — the audio is
    // already in hand, and nothing about a drawing is worth failing it over.
    let picture = picture_path
        .as_deref()
        .filter(|path| path.exists())
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| crate::praat::picture::decode_for_display(&bytes));

    Ok(PraatJobOutput { sample_rate: out.sample_rate, result: out.channels, picture })
}

fn run_praat(
    job: &PraatJob,
    driver_path: &Path,
    input_paths: &[PathBuf],
    output_path: &Path,
    picture_path: Option<&Path>,
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
    // Praat 7.0 sandboxes a script's own file-writing/system-command actions by default and
    // refuses them without this flag — every driver here does exactly that (saves its result
    // to `output_path`, and the `py` group shells out to a venv interpreter), so without it
    // every job fails immediately on Praat 7+. Safe to grant unconditionally: `driver_path` is
    // never a user-supplied script, it is generated by `driver.rs` into a `temp_dir` this app
    // created for the one job, and it only ever touches paths inside that directory.
    command.arg("--FULL-TRUST");
    // Point `preferencesDirectory$` at a directory we own (see `prepare_prefs_dir`). A handful
    // of scripts — the Vector Chain family, which chain several processes together — locate
    // their sibling scripts through that variable, and without this they fail with
    // `Cannot open file ".../.praat-dir/plugin_AudioTools/..."`. Redirecting is what lets those
    // work *without* installing anything into the user's own Praat preferences folder, where a
    // `plugin_AudioTools` of their own may already live.
    if let Some(prefs) = &job.prefs_dir {
        command.arg(format!("--pref-dir={}", prefs.display()));
    }
    // The `py` group's scripts invoke bare `python3`, so the interpreter they get is whatever
    // `PATH` resolves — which is the whole hook needed to point them at an app-owned venv. Only
    // set when one exists; otherwise the child inherits our `PATH` untouched.
    if let Some(path) = path_with_venv(job.python_venv_bin.as_deref()) {
        command.env("PATH", path);
    }
    command.arg(driver_path).args(input_paths);
    // Between the inputs and the output, matching where `driver_script` puts `infile
    // Photo_file` in the form. Same lockstep rule as the picture below — Praat fills a form
    // strictly by position *and count*.
    if let Some(path) = &job.photo_path {
        command.arg(path);
    }
    command.arg(output_path);
    // Conditional in lockstep with the driver's own `outfile Picture_file` field: Praat fills a
    // form strictly by position *and count*, so one without the other is an immediate exit 255.
    if let Some(path) = picture_path {
        command.arg(path);
    }
    let mut child = command
        .current_dir(temp_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PraatError::Spawn {
            bin: job.praat_bin.display().to_string(),
            message: spawn_error_message(&e),
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
/// The app-owned directory holding Praat-side state: the redirected preferences folder and the
/// Python venv. One definition, used by both the app and the real-binary sweep — the sweep
/// needs the very same venv, or every `py` process fails there on missing imports while working
/// perfectly in the app.
pub fn state_dir() -> PathBuf {
    // Shared with `Config::path` rather than resolved again here. The two had identical copies
    // of this, which is exactly the shape that lets a fix land in one of them and not the other.
    crate::config::config_home().join("tui-wave").join("praat")
}

/// The app-owned Python virtual environment the `py` process group runs in, if it exists.
///
/// Those scripts shell out to a sibling `.py` and need `numpy`, `scipy` and `soundfile`. On
/// Linux they resolve the interpreter as bare `python3` from `PATH`, so putting a venv's `bin`
/// at the front of `PATH` for the Praat child is enough to supply all three — no script edits,
/// no `pythonCmd$` patching, and **nothing installed into the system Python**, which on Arch and
/// Debian is externally managed and rejects `pip install` outright (PEP 668).
///
/// `None` when the directory does not exist, and the caller then leaves `PATH` alone: someone
/// who already has the packages on their system interpreter should not be forced into a venv to
/// keep working. Verified both directions — with only the venv on `PATH` a py-group process
/// runs, and with the venv absent and `~/.local` hidden the same process fails.
pub fn python_venv_bin(state_dir: &Path) -> Option<PathBuf> {
    // The layout `python -m venv` produces.
    let bin = state_dir.join("pyenv").join("bin");
    bin.is_dir().then_some(bin)
}

/// The venv's interpreter itself, for scripts that resolve one by absolute path.
///
/// [`python_venv_bin`] covers the scripts that ask `PATH`; this covers the ones that do not, and
/// on macOS that is all of them — they probe `/opt/homebrew/bin/python3` and friends before ever
/// consulting `PATH`, so the venv was reachable on Linux and invisible on a Mac. Both mechanisms
/// stay: the rewrite fixes the interpreter, and `PATH` still matters for anything the helper
/// itself shells out to.
pub fn python_venv_interpreter(state_dir: &Path) -> Option<PathBuf> {
    let bin = python_venv_bin(state_dir)?;
    let exe = bin.join("python3");
    exe.is_file().then_some(exe)
}

/// `PATH` for the Praat child with `venv_bin` at the front, or `None` to inherit unchanged.
fn path_with_venv(venv_bin: Option<&Path>) -> Option<std::ffi::OsString> {
    let venv_bin = venv_bin?;
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![venv_bin.to_path_buf()];
    entries.extend(std::env::split_paths(&existing));
    std::env::join_paths(entries).ok()
}

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
    #[cfg(not(unix))]
    let created = false;

    created.then_some(prefs)
}

/// Check that `dir` looks like a praatAudioTools checkout.
///
/// Distinguishes "empty" from "wrong", because the overwhelmingly likely first-run failure is a
/// submodule that was never initialised — a directory that exists and is empty. Telling someone
/// to check their path when the fix is one `git submodule` command would send them looking in
/// the wrong place entirely.
/// The praatAudioTools commit this build's process catalog was generated from, read from the
/// header the converter writes into `praat_catalog.toml` (which is `include_str!`d, so this is a
/// compile-time constant in practice).
///
/// `None` only if that header is ever reworded — the callers then skip the staleness check
/// rather than inventing an answer.
pub fn catalog_commit() -> Option<&'static str> {
    crate::model::cdp::catalog::praat_catalog_source()
        .lines()
        .take(10)
        .find_map(|line| line.split("at commit ").nth(1))
        .map(|rest| rest.trim_end_matches('.').trim())
        .filter(|sha| sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The commit a praatAudioTools checkout is currently on, or `None` when it is not a git
/// checkout at all.
///
/// Reads `.git` directly rather than shelling out to `git`: this runs on a UI path, `git` may
/// not be installed, and the two files involved are trivial to parse. A detached HEAD — which is
/// what `setup-environment.sh` produces, since this is a pinned dependency — holds the SHA
/// itself; a branch holds `ref: refs/heads/...`, which is one more file read.
pub fn checkout_commit(dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(dir.join(".git").join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref: ") {
        // Loose ref first; a packed-refs lookup is the fallback for a freshly cloned checkout.
        if let Ok(sha) = std::fs::read_to_string(dir.join(".git").join(reference)) {
            return Some(sha.trim().to_string());
        }
        let packed = std::fs::read_to_string(dir.join(".git").join("packed-refs")).ok()?;
        return packed.lines().find_map(|line| {
            let (sha, name) = line.split_once(' ')?;
            (name.trim() == reference).then(|| sha.trim().to_string())
        });
    }
    (head.len() == 40 && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| head.to_string())
}

/// `Some((expected, found))` when the checkout is on a different commit than the catalog was
/// generated from.
///
/// **Why this is worth detecting.** The catalog carries each script's parameter names, types,
/// order and count, and Praat fills a script's `form` **positionally**. Upstream rewrites
/// scripts constantly and without warning, so a checkout a few commits off does not fail — it
/// silently hands arguments to fields that have moved, and produces plausible, wrong audio.
/// Nothing else in `validate_audiotools_dir` can see that: the directory exists, is non-empty
/// and has the sentinel folders, so it passes every other check.
///
/// `None` whenever the question cannot be answered — not a git checkout, unreadable, or the
/// catalog header reworded. A user pointing at their own working copy is a legitimate thing to
/// do, and this must not nag them about it.
pub fn checkout_staleness(dir: &Path) -> Option<(String, String)> {
    let expected = catalog_commit()?;
    let found = checkout_commit(dir)?;
    (found != expected).then(|| (expected.to_string(), found))
}

/// What to tell a user who has no praatAudioTools scripts. Named once so every path that can
/// report the condition says the same thing.
pub const SETUP_HINT: &str = "About 439 of this app's processes are scripts from the \
praatAudioTools project, which the packages do not bundle. Run setup-environment.sh to fetch \
them: it is in /usr/share/tui-wave/ if you installed a .deb or .rpm, beside the binary if you \
unpacked a tarball, and attached to every release at \
https://github.com/biomassa/tui-wave/releases. Or set praat_audiotools_dir in the config to \
your own checkout.";

pub fn validate_audiotools_dir(dir: &Path) -> Result<(), String> {
    // An empty path renders as nothing at all, so the generic message below became the
    // unreadable `is not a directory` with no subject. Callers should route through
    // `Config::praat_audiotools_path`, which substitutes the bundled submodule — but say
    // something useful rather than nothing if one ever does not.
    if dir.as_os_str().is_empty() {
        // The state a *downloaded* build starts in: the packages carry the binary and none of
        // the scripts, and the executable-relative fallback cannot resolve from /usr/bin. Naming
        // the remedy rather than the config key is the difference between a dead end and a
        // one-line fix — the key alone left the user to discover both that the scripts are a
        // separate project and where to put them.
        return Err(format!("no praatAudioTools scripts found.\n\n{SETUP_HINT}"));
    }
    if !dir.is_dir() {
        return Err(format!("{} is not a directory.\n\n{SETUP_HINT}", dir.display()));
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
                picture_name: None,
                photo_input: false,
                script_path: PathBuf::from("/unused"),
                pause_rewrite: None,
                builtin_source: None,
                python_rewrite: None,
                label: "test".into(),
            },
            inputs: vec![vec![vec![0.0f32; 1000], vec![0.0f32; 1000]]],
            photo_path: None,
            input_sample_rate: 44_100,
            purpose: JobPurpose::Apply,
            timeout,
            prefs_dir: None,
            python_venv_bin: None,
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

    /// A missing binary is an `io::ErrorKind::NotFound`, and by far the most likely cause is
    /// that Praat itself was never installed — nothing here installs it on any platform, only
    /// the scripts and (for `py`) a venv. The message should say so and name the fix rather
    /// than surface the raw OS string ("No such file or directory (os error 2)"), which names a
    /// symptom a user who has never heard of `praat_bin` can't act on.
    #[test]
    fn a_missing_binary_names_praat_and_the_fix_not_a_raw_os_error() {
        let mut job = job_with("writeInfoLine: 1\n", DEFAULT_TIMEOUT);
        job.praat_bin = PathBuf::from("/nonexistent/praat-does-not-exist");
        match run(&job) {
            Err(err @ PraatError::Spawn { .. }) => {
                let text = err.to_string();
                assert!(text.contains("Install it"), "{text}");
                assert!(text.contains("fon.hum.uva.nl"), "{text}");
                assert!(text.contains("praat_bin"), "{text}");
                assert!(!text.contains("os error"), "should not leak the raw OS message: {text}");
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

    /// A driver that draws is handed a third argv path, and what it painted comes back decoded.
    /// This is the whole seam end to end: the extra form field, the extra argument, Praat's
    /// batch-mode Picture window, the PNG, and the crop — anything broken in that chain shows up
    /// as `picture: None` rather than as a failure, so nothing weaker would catch it.
    #[test]
    fn a_driver_that_draws_returns_its_picture() {
        require_praat!();
        let mut job = job_with(
            "form Driver\n    infile Input_file\n    outfile Output_file\n    \
             outfile Picture_file\nendform\n\
             snd = Read from file: input_file$\n\
             selectObject: snd\n\
             Save as 32-bit WAV file: output_file$\n\
             Erase all\n\
             Select outer viewport: 0, 6, 0, 4\n\
             Axes: 0, 1, 0, 1\n\
             Paint rectangle: \"Black\", 0.2, 0.8, 0.2, 0.8\n\
             nocheck Select outer viewport: 0, 12, 0, 12\n\
             nocheck Save as 300-dpi PNG file: picture_file$\n",
            DEFAULT_TIMEOUT,
        );
        job.planned.picture_name = Some(crate::model::praat::plan::PICTURE_PNG.into());
        let out = run(&job).expect("run");
        let picture = out.picture.expect("a picture came back");
        let (width, height) = picture.dimensions();
        assert!(width > 0 && height > 0);
        assert!(
            width <= crate::praat::picture::MAX_DIMENSION
                && height <= crate::praat::picture::MAX_DIMENSION,
            "{width}x{height} was not scaled down"
        );
        // The rectangle is 3:2, and the crop keeps that; a full 12x12 canvas would be square,
        // which is what proves the white margin was actually removed.
        assert!(width > height, "the canvas was not cropped to the drawing: {width}x{height}");
    }

    /// The mirror image, and the reason the blank check exists: a run that asks for a picture
    /// but paints nothing gets a full white canvas back from Praat, which must read as "no
    /// picture" rather than as a popup full of nothing.
    #[test]
    fn a_driver_that_draws_nothing_returns_no_picture() {
        require_praat!();
        let mut job = job_with(
            "form Driver\n    infile Input_file\n    outfile Output_file\n    \
             outfile Picture_file\nendform\n\
             snd = Read from file: input_file$\n\
             selectObject: snd\n\
             Save as 32-bit WAV file: output_file$\n\
             nocheck Select outer viewport: 0, 12, 0, 12\n\
             nocheck Save as 300-dpi PNG file: picture_file$\n",
            DEFAULT_TIMEOUT,
        );
        job.planned.picture_name = Some(crate::model::praat::plan::PICTURE_PNG.into());
        let out = run(&job).expect("run");
        assert!(out.picture.is_none(), "a blank canvas was mistaken for a picture");
        assert_eq!(out.result.len(), 2, "the audio must be unaffected either way");
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

    /// End-to-end proof that the redirect actually unlocks the chained scripts: `chain_3` locates
    /// its siblings through `preferencesDirectory$` and fails outright without this.
    ///
    /// Was `chain_2` until the 2026-08-30 bump, where upstream rewrote its third stage's call to
    /// a "compact form" of `8-channel_speed_deviations` -- collapsing eight channel-speed reals
    /// into one space-separated sentence and the two pitch bounds into another -- while shipping
    /// that script **unchanged**, with its original 22-field form. The chain now passes 14
    /// arguments to a 22-field form and dies with "Found 14 arguments but expected more". Three
    /// chains carry the same edit (`chain_2`, `Composition_2`, `Live_2`);
    /// `docs/vector-chain-stages.txt` flags all three, which is what that report is for.
    ///
    /// `chain_3` is the replacement rather than `chain_1` because this test's fixture is a plain
    /// tone: `chain_1` opens on `MDS_Space_Navigator`, which segments its input into words and
    /// refuses one with fewer than two. That is a property of the fixture, not of the chain.
    #[test]
    fn a_chained_script_finds_its_siblings_through_the_redirected_prefs_dir() {
        require_praat!();
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        let script = checkout.join("Vector Chain/chain_3.praat");
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
            crate::model::praat::driver::DriverOptions::default(),
        )
        .unwrap();

        match run(&job) {
            Ok(out) => assert!(!out.result.is_empty(), "chained script produced no audio"),
            Err(err) => panic!("chained script failed: {err}"),
        }
        let _ = std::fs::remove_dir_all(&state);
    }

    /// `setup-environment.sh` pins the praatAudioTools commit it checks out, and that pin must
    /// equal the one this build's catalog was generated from.
    ///
    /// The catalog carries every script's parameter order, and Praat fills a `form`
    /// **positionally** — so a script fetched at the wrong commit does not fail, it hands
    /// arguments to fields that have moved and produces plausible, wrong audio. A submodule bump
    /// that forgets the script would otherwise ship exactly that.
    #[test]
    fn praat_setup_commit_matches_the_catalog() {
        let expected = catalog_commit().expect("the catalog header names its source commit");
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("setup-environment.sh"),
        )
        .expect("setup-environment.sh ships with the repo");
        let pinned = script
            .lines()
            .find_map(|l| l.trim().strip_prefix("PINNED_COMMIT="))
            .map(|v| v.trim().trim_matches('"').to_string())
            .expect("setup-environment.sh declares PINNED_COMMIT");
        assert_eq!(
            pinned, expected,
            "setup-environment.sh checks out {pinned}, but this build's catalog was generated \
             from {expected} — re-run update-praat-scripts.sh and update PINNED_COMMIT together"
        );
    }

    /// The bundled submodule is on the commit the catalog names, so a source checkout reports no
    /// staleness. This is also what proves `checkout_commit` can read a real `.git` — the
    /// submodule's is a `.git` *file* pointing into the parent's modules directory, not a
    /// directory, which a naive implementation gets wrong.
    #[test]
    fn the_bundled_checkout_is_not_reported_as_stale() {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        if !checkout.join("Distortion").is_dir() {
            eprintln!("skipping: submodule not initialised");
            return;
        }
        match checkout_staleness(&checkout) {
            None => {}
            Some((expected, found)) => panic!(
                "the bundled submodule is at {found} but the catalog expects {expected}"
            ),
        }
    }

    /// A directory that is not a git checkout answers "cannot tell" rather than "stale" — a user
    /// pointing at their own unpacked copy is legitimate and must not be nagged.
    #[test]
    fn a_non_git_directory_reports_no_staleness() {
        let dir = std::env::temp_dir().join(format!("tui-wave-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(checkout_staleness(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two seconds of mono material with real structure — a 220 Hz tone under a slow tremolo,
    /// plus a deterministic pseudo-noise layer.
    ///
    /// A constant full-level sine is the wrong fixture for this collection: many of these
    /// scripts analyse pitch, envelope or dynamics, and Praat rejects a signal whose loudest
    /// and softest parts differ by 0.0002 dB outright. The variation here is what makes a
    /// failure mean "the catalog entry is wrong" rather than "the fixture was degenerate".
    ///
    /// Paired with [`smoke_fixture_stereo`]; the sweep drives every entry at both widths.
    fn smoke_fixture() -> (Vec<Vec<f32>>, u32) {
        const RATE: u32 = 44_100;
        (vec![smoke_leg(220.0, 0.0, 0)], RATE)
    }

    /// The stereo companion to [`smoke_fixture`], with the two channels genuinely decorrelated.
    ///
    /// **Why a second width at all.** Most of these scripts fold their input to mono before
    /// analysing it, and against a mono fixture that fold is a no-op — so a sweep of mono alone
    /// never exercises any script's stereo handling, and a defect there passes unseen. That is
    /// not hypothetical: an upstream Reich Generator fix (2026-08-25) added a cancellation-safe
    /// mono analysis gated on `channels > 1`, and the sweep went green on it without reaching a
    /// line of the new code. Both widths run because neither subsumes the other — a script can
    /// just as easily break on a mono input it assumed was stereo.
    ///
    /// **Decorrelated, not phase-opposed.** Inverting one channel would exercise the
    /// cancellation path head-on, but it folds to digital silence, so every script analysing the
    /// fold would be reading a dead signal and its failure would be the fixture's fault rather
    /// than the catalog's — the same degeneracy the tremolo in [`smoke_fixture`] exists to
    /// avoid. The right leg is detuned 3 Hz with its tremolo in quadrature and its own noise
    /// seed instead: the fold stays healthy while the two channels are never interchangeable.
    fn smoke_fixture_stereo() -> (Vec<Vec<f32>>, u32) {
        const RATE: u32 = 44_100;
        let left = smoke_leg(220.0, 0.0, 0);
        let right = smoke_leg(223.0, std::f32::consts::FRAC_PI_2, 0x9E37_79B9);
        (vec![left, right], RATE)
    }

    /// One channel of smoke-fixture material. `seed` decorrelates the noise layer between legs;
    /// the jitter is a deterministic hash rather than an RNG so a failure is reproducible.
    fn smoke_leg(hz: f32, tremolo_phase: f32, seed: u32) -> Vec<f32> {
        const RATE: u32 = 44_100;
        (0..RATE * 2)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let tremolo = 0.55 + 0.45 * (t * std::f32::consts::TAU * 1.7 + tremolo_phase).sin();
                let tone = (t * hz * std::f32::consts::TAU).sin();
                let grit = ((i.wrapping_add(seed).wrapping_mul(2_654_435_761) >> 9) as f32
                    / 4_194_304.0
                    - 1.0)
                    * 0.05;
                (tone * tremolo + grit) * 0.7
            })
            .collect()
    }

    /// A PNG for the `IoKind::Photo` processes to sonify, written under `state` and returned by
    /// path.
    ///
    /// Deliberately generated rather than checked in as a fixture: the four scripts read
    /// brightness and the red/blue balance per column, so what matters is that both vary
    /// across the image — a flat or grey picture would sonify to something the smoke test could
    /// not tell apart from a broken run. The gradients here put a different value in every
    /// column and a red→blue sweep across the width, which exercises the pan mapping too.
    ///
    /// Small on purpose (160x120). These scripts scan every pixel of every analysis column, and
    /// the sweep runs them at their catalog defaults on a timeout.
    fn smoke_photo_fixture(state: &Path) -> PathBuf {
        let (width, height) = (160u32, 120u32);
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            let across = x as f32 / width as f32;
            let down = y as f32 / height as f32;
            image::Rgb([
                (255.0 * (1.0 - across)) as u8,
                (255.0 * down) as u8,
                (255.0 * across) as u8,
            ])
        });
        let path = state.join("photo.png");
        std::fs::create_dir_all(state).expect("photo fixture dir");
        image.save(&path).expect("photo fixture");
        path
    }

    /// Runs every Praat catalog entry once at its declared defaults and reports which fail.
    ///
    /// This is what turns "82.5% of a 120-script sample worked" into a known-good shipped set.
    /// The catalog is machine-generated from upstream scripts of uneven quality, and a bad
    /// entry — a form this converter misparsed, a script that needs data we cannot supply —
    /// only shows up by actually running it.
    ///
    /// Env-gated like `cdp::runner::catalog_smoke_test`, and for the same reason: this spawns
    /// one Praat per entry across 350+ entries, which is minutes of wall clock. Failures are
    /// collected rather than asserted one at a time, so one bad entry cannot hide the rest.
    #[test]
    fn praat_catalog_smoke_test() {
        if std::env::var("TUI_WAVE_PRAAT_SMOKE").ok().as_deref() != Some("1") {
            eprintln!("skipping: set TUI_WAVE_PRAAT_SMOKE=1 to run the full Praat smoke test");
            return;
        }
        require_praat!();
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        if validate_audiotools_dir(&checkout).is_err() {
            eprintln!("skipping: submodule not initialised");
            return;
        }
        let state = std::env::temp_dir().join(format!("praat-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let prefs = prepare_prefs_dir(&state, &checkout);

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let (channels, sample_rate) = smoke_fixture();
        let (stereo_channels, _) = smoke_fixture_stereo();
        // Every entry is driven at both widths — see `smoke_fixture_stereo` for why neither
        // subsumes the other. Mono first, so a script that breaks at both reports the simpler
        // case first.
        let widths: [(&str, &Vec<Vec<f32>>); 2] =
            [("mono", &channels), ("stereo", &stereo_channels)];
        // Built once even if nothing asks for them: a minute of audio is cheap to synthesise and
        // the alternative is building it inside the entry loop, once per width per entry.
        let (long_channels, _) = long_fixture();
        let (long_stereo, _) = long_fixture_stereo();
        let long_widths: [(&str, &Vec<Vec<f32>>); 2] =
            [("mono", &long_channels), ("stereo", &long_stereo)];

        // A folder of real sounds for `ParamKind::FolderPath` params, which have no catalog
        // default because there is no sensible directory to invent. Leaving one empty is not a
        // neutral choice: the scripts that take one fall back to Praat's `chooseFolder$`, which
        // under `--run` opens a window and **segfaults**, so the sweep would take Praat down
        // three times per run and report it as an unexplained non-zero exit. Seeding is also
        // the only way these processes get covered at all.
        let corpus = state.join("corpus");
        std::fs::create_dir_all(&corpus).expect("corpus dir");
        for i in 0..6 {
            let doc = Document {
                channels: vec![channels[0]
                    .iter()
                    .enumerate()
                    .map(|(n, s)| s * (1.0 + n as f32 * 0.0001 * (i + 1) as f32).sin())
                    .collect()],
                sample_rate,
                ..Default::default()
            };
            save_wav_with(&doc, &corpus.join(format!("clip{i}.wav")), BitDepth::Int16, false)
                .expect("corpus clip");
        }
        let corpus_path = corpus.to_string_lossy().into_owned();
        let photo = smoke_photo_fixture(&state);

        // Narrows the sweep to entries whose key contains this substring, e.g.
        // `TUI_WAVE_PRAAT_SMOKE_FILTER=praat_py_` for the Python group alone. The full sweep is
        // minutes of wall clock and plays audio the whole time, so re-testing one group after
        // touching it was otherwise all-or-nothing.
        let filter = std::env::var("TUI_WAVE_PRAAT_SMOKE_FILTER").unwrap_or_default();
        let matches = |d: &crate::model::cdp::ProcessDef| {
            d.backend() == crate::model::cdp::def::Backend::Praat
                && (filter.is_empty() || d.key.contains(&filter))
        };

        let mut failures: Vec<String> = Vec::new();
        let mut ran = 0usize;
        let total = catalog.processes.iter().filter(|d| matches(d)).count();

        // The sweep writes its own log, at a fixed path announced before the first entry runs.
        //
        // stderr alone was not enough. The per-entry name exists so that whoever is listening can
        // tell which of the 32 unconditionally-playing scripts is making the noise — and that only
        // works if the line is visible *while* it plays. Any wrapper that pipes the run (a CI
        // capture, `| tail`, a test harness collecting output) buffers it to the end and the names
        // arrive after every sound has already played, which defeats the entire point of printing
        // them early. A file the sweep writes itself cannot be intercepted that way: `tail -f` on
        // it shows each name as it happens no matter how the run was launched.
        //
        // Fixed name rather than timestamped, because it has to be typed into a *second* terminal,
        // often before this one has printed anything. Truncated on open, so a `tail -f` started
        // beforehand follows the new run rather than showing the last one's tail.
        // A fixed name is only safe because the sweep holds an exclusive lock on it. Two sweeps at
        // once used to shred each other silently: the second one's truncate reset the length to
        // zero while the first still held a descriptor positioned tens of kilobytes in, so the
        // kernel filled the gap with a sparse hole. The result was a log interleaving both runs
        // and carrying thousands of NUL bytes — which makes `grep` treat it as binary and print
        // *nothing at all*, so the file looked empty of failures it plainly contained.
        //
        // Lock first, truncate second, and never the other way around: `File::create` truncates
        // as it opens, which is the destructive half happening before anyone has established the
        // right to do it. A loser falls back to a pid-suffixed path rather than refusing to run,
        // since a sweep is thirteen minutes and failing it over a log file would be the more
        // annoying outcome — the banner names whichever path won.
        let requested = std::env::var("TUI_WAVE_PRAAT_SMOKE_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("tui-wave-praat-smoke.log"));
        let open_locked = |path: &Path| {
            let file = std::fs::OpenOptions::new().write(true).create(true).open(path).ok()?;
            file.try_lock().ok()?;
            let _ = file.set_len(0);
            Some(file)
        };
        let (mut log, log_path, contended) = match open_locked(&requested) {
            Some(file) => (Some(file), requested, false),
            None => {
                let stem = requested.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                let busy = requested.with_file_name(format!("{stem}.{}.log", std::process::id()));
                let file = open_locked(&busy);
                (file, busy, true)
            }
        };
        // Every line goes to both, and the file is flushed per line — a buffered log is exactly
        // the failure being fixed, only one directory further along.
        macro_rules! sweep_log {
            ($($arg:tt)*) => {{
                let line = format!($($arg)*);
                eprint!("{line}");
                if let Some(f) = log.as_mut() {
                    use std::io::Write;
                    let _ = write!(f, "{line}");
                    let _ = f.flush();
                }
            }};
        }
        // Announced on **both** streams, before anything else runs. stdout as well as stderr
        // because the two get captured independently — a harness that swallows one usually still
        // shows the other, and this banner is worthless if it is the half that went missing. It
        // is the one line whose whole job is to be read by a person before the sweep gets going.
        let banner = format!(
            "\n\
             ==================================================================\n\
             praat smoke sweep: {total} entries — live log:\n  \
               {}\n\
             follow it from another terminal with:\n  \
               tail -f {}\n\
             {}\
             ==================================================================\n",
            log_path.display(),
            log_path.display(),
            if contended {
                "NOTE: another sweep holds the usual log, so this run took the path above\n"
            } else {
                ""
            },
        );
        println!("{banner}");
        eprintln!("{banner}");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        sweep_log!("praat smoke sweep: {total} entries x {} widths\n", widths.len());
        if !filter.is_empty() {
            sweep_log!("filter {filter:?}: {total} of {} Praat entries\n", catalog.processes.iter().filter(|d| d.backend() == crate::model::cdp::def::Backend::Praat).count());
        }
        for (index, def) in catalog.processes.iter().filter(|d| matches(d)).enumerate() {
            let values: Vec<_> = def
                .params
                .iter()
                .map(|p| match p.kind {
                    crate::model::cdp::ParamKind::FolderPath => {
                        crate::model::cdp::ParamValue::Text(corpus_path.clone())
                    }
                    _ => p.kind.default_value(),
                })
                .collect();
            // An interactive process opens a window and waits for a person, so it has no
            // timeout (see `ProcessDef::interactive`) and would hang this sweep forever. There
            // is nothing to check automatically about one anyway.
            if def.interactive {
                sweep_log!("[{:>3}/{total}] {} — skipped (interactive)\n", index + 1, def.key);
                continue;
            }
            // Printed *before* the run, not after, and that ordering is the point: 32 of these
            // scripts call `Play` unconditionally, so the sweep audibly plays audio and there
            // was no way to tell which process was responsible. stderr is unbuffered, so the
            // name is on screen before the sound starts.
            // Name, then elapsed time and a running failure count once it returns. Printed in
            // two halves on purpose: the name has to be on screen *before* the run, because 32
            // of these scripts call `Play` unconditionally and there was otherwise no way to
            // tell which one was making noise. The timing has to come after, and without it a
            // slow process and a wedged one look identical for however long the timeout is —
            // which is exactly how a sweep reads as a hang.
            // The width is part of the printed line because a failure that reproduces at one
            // width and not the other is the whole reason both run — a bare key would leave the
            // reader unable to tell which input produced it.
            // A handful of entries correctly refuse two seconds of continuous tone; they get a
            // minute of segmented material instead. See `LONG_FIXTURE_ENTRIES`.
            let long_material = LONG_FIXTURE_ENTRIES.contains(&def.key.as_str());
            let entry_widths = if long_material { long_widths } else { widths };
            // Thirty times the audio is thirty times the analysis, and `vector_chain_chain_1`
            // spends 46s on the mono leg alone — under the ordinary budget it would fail as a
            // timeout, which reads as a wedged script rather than as a slow one. Raised only for
            // the entries actually handed the long fixture, so a genuine hang anywhere else is
            // still caught in a minute.
            let budget =
                if long_material { LONG_FIXTURE_TIMEOUT } else { Duration::from_secs(60) };
            for (width, width_channels) in entry_widths {
                sweep_log!("[{:>3}/{total}] {:<52} {:<7}", index + 1, def.key, width);
                let started = std::time::Instant::now();
                // Re-planned per width rather than planned once and reused: a plan is derived
                // from the def and its values and not from the audio, but re-deriving keeps the
                // two runs independent, so neither can inherit anything the other left behind.
                let planned = match crate::model::praat::plan_praat_job_with(def, &values, &checkout, python_venv_interpreter(&crate::ui::app::praat_state_dir()).as_deref(), 1) {
                    Ok(planned) => planned,
                    Err(err) => {
                        failures.push(format!("{} [{width}]: plan failed: {err}", def.key));
                        sweep_log!("   plan failed\n");
                        continue;
                    }
                };
                // A two-Sound process gets the same fixture on both sides; self-processing is a
                // valid shape for everything being checked here.
                let input_count = planned.input_names.len();
                let job = PraatJob {
                    id: ran as u64,
                    praat_bin: praat_bin_for(""),
                    inputs: vec![width_channels.clone(); input_count],
                    // An `IoKind::Photo` process `exitScript`s without one, so the sweep would
                    // report four confusing "produced no Sound object" failures rather than
                    // covering them at all — the same reasoning as the corpus folder above.
                    photo_path: planned.photo_input.then(|| photo.clone()),
                    planned,
                    input_sample_rate: sample_rate,
                    purpose: JobPurpose::Apply,
                    timeout: budget,
                    prefs_dir: prefs.clone(),
                    // The sweep exercises the `py` group too, so it needs the same venv the app
                    // uses — otherwise every Python-backed process fails on missing imports here
                    // while working perfectly in the app.
                    python_venv_bin: python_venv_bin(&state_dir()),
                };
                ran += 1;
                let outcome = run(&job);
                let elapsed = started.elapsed();
                if let Err(err) = outcome {
                    let detail = match &err {
                        PraatError::NonZeroExit { output, .. } => output
                            .lines()
                            .find(|l| l.starts_with("Error:"))
                            .unwrap_or("(no Error: line)")
                            .to_string(),
                        other => other.to_string(),
                    };
                    failures.push(format!("{} [{width}]: {detail}", def.key));
                    sweep_log!(" {:>6.1}s  FAIL ({} so far)\n", elapsed.as_secs_f32(), failures.len());
                } else {
                    sweep_log!(" {:>6.1}s  ok\n", elapsed.as_secs_f32());
                }
            }
        }
        let _ = std::fs::remove_dir_all(&state);

        sweep_log!("praat smoke: {} ran, {} failed\n", ran, failures.len());
        for failure in &failures {
            sweep_log!("  {failure}\n");
        }
        assert!(ran > 0, "no Praat entries found in the catalog");
    }

    /// Entries whose script refuses [`smoke_fixture`] on its merits, needing a **long, segmented**
    /// recording instead of two seconds of continuous tone.
    ///
    /// Not a workaround and not a baseline: each of these states its requirement plainly and
    /// correctly — "Sound too short (< 20 s). Need longer material for structural analysis",
    /// "Need at least 2 word segments! Found: 1", "Too few events detected (1)". A two-second
    /// unbroken tone is one event and one segment however long you stare at it, so the sweep was
    /// asserting nothing about these four beyond the fact that they can read a header.
    ///
    /// Kept as a named list rather than given to everything, because the ordinary fixture is what
    /// makes a 936-run sweep cheap: a minute of audio costs thirty times the analysis on every
    /// entry that never needed it. Same reasoning as `melody_fixture`, which exists for the one
    /// script that wants discrete pitched notes.
    const LONG_FIXTURE_ENTRIES: &[&str] = &[
        "praat_time_granular_dramaturgical_structure_composer",
        "praat_time_granular_mds_space_navigator",
        "praat_vector_chain_chain_1",
        "praat_py_phasespacecomposer",
    ];

    /// What [`LONG_FIXTURE_ENTRIES`] get instead of the sweep's ordinary 60s budget.
    ///
    /// A timeout and a failure are not the same report — one says the script is wrong, the other
    /// says it did not finish — and handing an entry thirty times the audio without adjusting the
    /// clock turns the second into the first. `vector_chain_chain_1` takes 46s on the mono leg
    /// and more on stereo, having taken 0.1s to *reject* the short fixture outright.
    const LONG_FIXTURE_TIMEOUT: Duration = Duration::from_secs(240);

    /// Sixty seconds of discrete, silence-separated events in three sections — what
    /// [`LONG_FIXTURE_ENTRIES`] need and what [`smoke_fixture`] deliberately is not.
    ///
    /// Three properties, one per failure it answers. It is **a minute long**, clearing the 20 s
    /// floor structural analysis asks for. Its events are **separated by real gaps** — a noise
    /// floor near -72 dBFS rather than digital silence, so a silence-based segmenter finds
    /// boundaries where a continuous tone offers none, and an onset detector finds forty-odd
    /// events rather than one. And it is **sectioned**: register, density and brightness all step
    /// twice across the minute, so a script looking for *structure* has some to find rather than
    /// one texture repeated for sixty seconds, which would satisfy the duration check and then
    /// fail the analysis it exists for.
    ///
    /// Deterministic like every other fixture here — the jitter is a hash of the event index, so
    /// a failure reproduces exactly. `seed` decorrelates the stereo legs, as in
    /// [`smoke_fixture_stereo`].
    fn long_fixture_leg(seed: u32) -> Vec<f32> {
        const RATE: u32 = 44_100;
        const SECONDS: usize = 60;
        let total = RATE as usize * SECONDS;
        let mut out = vec![0.0f32; total];

        // A hash-like unit value for event `k`, stable across runs.
        let jitter = |k: u32, salt: u32| {
            ((k.wrapping_add(seed).wrapping_add(salt).wrapping_mul(2_246_822_519) >> 8) % 1_000)
                as f32
                / 1_000.0
        };

        // Low noise floor everywhere: real recordings are never digitally silent, and a segmenter
        // tuned for speech behaves differently against true zeros.
        for (i, sample) in out.iter_mut().enumerate() {
            *sample = (jitter(i as u32, 0x5F35) - 0.5) * 0.0005;
        }

        let mut at = RATE as usize / 4;
        let mut event = 0u32;
        while at < total {
            // Three sections across the minute, stepping register, brightness and density.
            let section = (at * 3 / total).min(2);
            let (root, brightness, gap_scale) =
                [(150.0f32, 0.25f32, 1.0f32), (260.0, 0.55, 0.7), (420.0, 0.85, 1.4)][section];

            let steps = [0i32, 3, 5, 7, 10, 12];
            let semitone = steps[(event as usize + section) % steps.len()] as f32;
            let freq = root * 2f32.powf(semitone / 12.0);

            let dur = ((0.45 + jitter(event, 1) * 0.40) * RATE as f32) as usize;
            let len = dur.min(total - at);
            let fade = (RATE as usize / 40).min(len / 2).max(1);
            for i in 0..len {
                let t = i as f32 / RATE as f32;
                let envelope = if i < fade {
                    // Raised cosine, so an onset detector sees an edge without a click.
                    0.5 - 0.5 * (std::f32::consts::PI * i as f32 / fade as f32).cos()
                } else if i + fade > len {
                    0.5 - 0.5 * (std::f32::consts::PI * (len - i) as f32 / fade as f32).cos()
                } else {
                    1.0
                };
                let phase = t * freq * std::f32::consts::TAU;
                let tone = phase.sin()
                    + brightness * 0.5 * (phase * 2.0).sin()
                    + brightness * 0.25 * (phase * 3.0).sin();
                let breath = (jitter(event * 977 + i as u32, 0x9E37) - 0.5) * 0.06;
                out[at + i] += (tone / 1.75 + breath) * envelope * 0.6;
            }

            let gap = (((0.28 + jitter(event, 2) * 0.45) * gap_scale) * RATE as f32) as usize;
            at += len + gap.max(RATE as usize / 8);
            event += 1;
        }
        out
    }

    /// Mono [`long_fixture_leg`]; see it for what this is for.
    fn long_fixture() -> (Vec<Vec<f32>>, u32) {
        (vec![long_fixture_leg(0)], 44_100)
    }

    /// Stereo [`long_fixture_leg`], decorrelated the way [`smoke_fixture_stereo`] is.
    fn long_fixture_stereo() -> (Vec<Vec<f32>>, u32) {
        (vec![long_fixture_leg(0), long_fixture_leg(0x9E37_79B9)], 44_100)
    }

    /// A melody of discrete pitched notes — what the note-extracting scripts actually need, and
    /// what `smoke_fixture`'s steady 220 Hz tone with tremolo deliberately is not (it is there to
    /// exercise every entry cheaply, and a script that refuses it on its merits is not a defect).
    ///
    /// Eight notes over a major-ish scale, each 300 ms — comfortably past the 80 ms minimum note
    /// duration `OT_Grammar` defaults to — with a short raised-cosine fade on each so Praat's
    /// pitch tracker sees note boundaries rather than one continuous glide.
    fn melody_fixture(offset: usize) -> (Vec<Vec<f32>>, u32) {
        const RATE: u32 = 44_100;
        const NOTE: usize = RATE as usize * 3 / 10;
        let steps = [0i32, 2, 4, 5, 7, 9, 11, 12];
        let mut samples = Vec::with_capacity(NOTE * steps.len());
        for (n, step) in steps.iter().enumerate() {
            // The offset walks the melody through the scale, so the good/ and bad/ corpora hold
            // genuinely different files rather than eight copies of one.
            let semitones = step + ((n + offset) % 3) as i32;
            let freq = 220.0 * 2f32.powf(semitones as f32 / 12.0);
            let fade = RATE as usize / 200;
            for i in 0..NOTE {
                let t = i as f32 / RATE as f32;
                let envelope = if i < fade {
                    i as f32 / fade as f32
                } else if i > NOTE - fade {
                    (NOTE - i) as f32 / fade as f32
                } else {
                    1.0
                };
                // Two harmonics: a pure sine is tracked fine, but the overtone makes the
                // fixture's spectrum less degenerate for anything measuring timbre.
                let tone = (t * freq * std::f32::consts::TAU).sin()
                    + 0.3 * (t * freq * 2.0 * std::f32::consts::TAU).sin();
                samples.push(tone * envelope * 0.5);
            }
        }
        (vec![samples], RATE)
    }

    /// `OT_Grammar_Learning_from_Audio` in **both** GEN modes, against the real binary.
    ///
    /// This is the test the directory hoist exists for. The entry shipped before it with a
    /// `chooseDirectory$` call sitting in the `else` of its GEN-mode menu: mode 1 worked, and
    /// mode 2 opened a modal under `--run`, which does not block — it **segfaults** (exit 139).
    /// So the mode was not merely unusable, it took Praat down, and nothing in the catalog said
    /// so. Both modes are run because passing in mode 1 proves nothing about the branch that
    /// changed, and running only mode 2 would not notice the hoist breaking the other one.
    ///
    /// The corpus is built here rather than reusing the sweep's: this script extracts a *melody*,
    /// and eight discrete notes are the minimum it will accept.
    #[test]
    fn ot_grammar_runs_in_both_gen_modes() {
        require_praat!();
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        if validate_audiotools_dir(&checkout).is_err() {
            return; // submodule not initialised
        }
        let state = std::env::temp_dir().join(format!("praat-otgrammar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let prefs = prepare_prefs_dir(&state, &checkout);

        // `pairRoot$ + "/good"` and `+ "/bad"` — the script names both subfolders itself, so the
        // hoisted param is the folder *holding* them, not either one.
        let pairs = state.join("pairs");
        for (kind, base) in [("good", 0usize), ("bad", 1usize)] {
            let dir = pairs.join(kind);
            std::fs::create_dir_all(&dir).expect("corpus dir");
            for i in 0..3 {
                let (channels, sample_rate) = melody_fixture(base * 3 + i);
                let doc = Document { channels, sample_rate, ..Default::default() };
                save_wav_with(&doc, &dir.join(format!("m{i}.wav")), BitDepth::Int16, false)
                    .expect("corpus clip");
            }
        }

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog
            .find("praat_analysis_ot_grammar_learning_from_audio")
            .expect("entry exists");
        let gen_mode = def.params.iter().position(|p| p.name == "GEN_mode").expect("GEN_mode");
        let folder = def
            .params
            .iter()
            .position(|p| p.praat_directory_var.as_deref() == Some("pairRoot$"))
            .expect("the hoisted folder param");
        let (channels, sample_rate) = melody_fixture(0);

        for (mode, label) in [(0usize, "Neighbor-GEN"), (1, "Pair-corpus")] {
            let mut values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
            values[gen_mode] = crate::model::cdp::ParamValue::Choice(mode);
            values[folder] =
                crate::model::cdp::ParamValue::Text(pairs.to_string_lossy().into_owned());
            let planned = crate::model::praat::plan::plan_praat_job(def, &values, &checkout)
                .unwrap_or_else(|e| panic!("{label}: plan failed: {e}"));
            let job = PraatJob {
                id: mode as u64,
                praat_bin: praat_bin_for(""),
                inputs: vec![channels.clone(); planned.input_names.len()],
                photo_path: None,
                planned,
                input_sample_rate: sample_rate,
                purpose: JobPurpose::Apply,
                timeout: Duration::from_secs(120),
                prefs_dir: prefs.clone(),
                python_venv_bin: None,
            };
            match run(&job) {
                Ok(out) => assert!(!out.result[0].is_empty(), "{label}: empty output"),
                Err(err) => panic!("{label}: {err}"),
            }
        }
        let _ = std::fs::remove_dir_all(&state);
    }

    /// The same sweep, but with every drawing toggle turned **on** — the number that says
    /// whether the picture feature is honest.
    ///
    /// It exists because the drawing blocks are the least-exercised code in the plugin: nothing
    /// ever ran them before this feature, `praat_catalog_smoke_test` drives every entry at its
    /// catalog default (which the converter forces to *off* for exactly these toggles), and
    /// there are roughly 475 of them. A `Draw:` against an object a script only creates on
    /// another branch fails here and nowhere else.
    ///
    /// Reports three buckets and asserts none of them, because the useful signal is the
    /// proportion rather than a threshold — and because a script refusing the fixture on its
    /// merits ("No loops found. Try adjusting parameters." on two seconds of tone) is not a
    /// defect in this feature. Separately gated from `TUI_WAVE_PRAAT_SMOKE` so the ordinary
    /// The nine `Universal Convolution Generator` variants must render **nine different**
    /// impulse responses, not nine copies of one.
    ///
    /// Upstream reworked that script into a wizard with no `form` at all: an outer
    /// `while sessionActive = 1`, an inner `while wizardDone = 0`, and nine settings pages each
    /// ending `endPause: "< Back", "Apply", "OK", 2`. Honouring that declared default sets
    /// `uiAction = 2`, which the script documents as "keeps the session alive" — so every variant
    /// ran to the runner's timeout, the worst failure shape there is: no error, just a job that
    /// never returns. `praat_pause_button = 3` ("OK — applies once and closes the session") is the
    /// fix, and the smoke sweep can only say each variant now *finishes*.
    ///
    /// Finishing is not the property worth pinning. A wizard reached through hoisted pause blocks
    /// could just as easily complete while every page selected the same algorithm, and the sweep
    /// would report nine passes for nine identical renders. So this compares the actual audio:
    /// pairwise-distinct outputs are what proves the algorithm choice reaches the script.
    #[test]
    fn the_nine_convolution_generators_render_nine_different_impulse_responses() {
        require_praat!();
        let checkout =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        if crate::praat::validate_audiotools_dir(&checkout).is_err() {
            return; // submodule not initialised
        }
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let defs: Vec<_> = catalog
            .processes
            .iter()
            .filter(|d| d.key.starts_with("praat_reverb_universal_convolution_generator_"))
            .collect();
        assert_eq!(defs.len(), 9, "nine algorithm variants");

        let state = std::env::temp_dir().join(format!("praat-ucg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let prefs = prepare_prefs_dir(&state, &checkout);
        let sample_rate = 44_100u32;
        let tone: Vec<f32> = (0..sample_rate as usize)
            .map(|i| (i as f32 * 220.0 * std::f32::consts::TAU / sample_rate as f32).sin() * 0.5)
            .collect();

        let mut rendered: Vec<(String, Vec<f32>)> = Vec::new();
        for def in &defs {
            let values: Vec<_> = def.params.iter().map(|p| p.kind.default_value()).collect();
            let planned = crate::model::praat::plan_praat_job_with(
                def,
                &values,
                &checkout,
                python_venv_interpreter(&crate::ui::app::praat_state_dir()).as_deref(),
                1,
            )
            .unwrap_or_else(|e| panic!("{}: plan failed: {e}", def.key));
            assert_eq!(
                planned.pause_rewrite.as_ref().and_then(|r| r.button_override),
                Some(3),
                "{}: the override must reach the plan, or this test proves nothing",
                def.key
            );
            let input_count = planned.input_names.len();
            let job = PraatJob {
                id: 0,
                praat_bin: praat_bin_for(""),
                inputs: vec![vec![tone.clone()]; input_count],
                photo_path: None,
                planned,
                input_sample_rate: sample_rate,
                purpose: JobPurpose::Apply,
                // Well under the 60s the hang used to consume, so a regression to the session
                // loop fails here quickly rather than stalling the suite.
                timeout: Duration::from_secs(30),
                prefs_dir: prefs.clone(),
                python_venv_bin: python_venv_bin(&state_dir()),
            };
            match run(&job) {
                Ok(out) => {
                    let first = out.result.first().cloned().unwrap_or_default();
                    assert!(!first.is_empty(), "{}: produced no audio", def.key);
                    // Non-empty is not enough: nine distinct near-silences would satisfy the
                    // pairwise check below while meaning the generator produced nothing usable.
                    let peak = first.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                    assert!(
                        peak > 0.01,
                        "{}: peak {peak} — rendered essentially silence",
                        def.key
                    );
                    eprintln!(
                        "  {:52} {:>7} samples  peak {peak:.3}",
                        def.key.trim_start_matches("praat_reverb_universal_convolution_generator_"),
                        first.len()
                    );
                    rendered.push((def.key.clone(), first));
                }
                Err(err) => panic!("{}: {err}", def.key),
            }
        }
        let _ = std::fs::remove_dir_all(&state);

        for (i, (key_a, a)) in rendered.iter().enumerate() {
            for (key_b, b) in rendered.iter().skip(i + 1) {
                assert!(
                    a != b,
                    "{key_a} and {key_b} rendered identical audio — the algorithm choice is not \
                     reaching the script"
                );
            }
        }
    }

    /// sweep does not double in cost.
    #[test]
    fn praat_draw_smoke_test() {
        if std::env::var("TUI_WAVE_PRAAT_DRAW_SMOKE").ok().as_deref() != Some("1") {
            eprintln!("skipping: set TUI_WAVE_PRAAT_DRAW_SMOKE=1 to run the Praat drawing sweep");
            return;
        }
        require_praat!();
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        if validate_audiotools_dir(&checkout).is_err() {
            eprintln!("skipping: submodule not initialised");
            return;
        }
        let state = std::env::temp_dir().join(format!("praat-draw-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let prefs = prepare_prefs_dir(&state, &checkout);

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let (channels, sample_rate) = smoke_fixture();
        let photo = smoke_photo_fixture(&state);

        let (mut drew, mut blank) = (0usize, 0usize);
        let mut failures: Vec<String> = Vec::new();
        let mut index = 0usize;
        for def in catalog
            .processes
            .iter()
            .filter(|d| d.backend() == crate::model::cdp::def::Backend::Praat)
        {
            // Defaults, except every drawing toggle forced on. `draws_picture` then decides
            // whether this entry has one at all, so the sweep covers exactly the entries the
            // feature applies to and nothing else.
            let values: Vec<_> = def
                .params
                .iter()
                .map(|p| match &p.kind {
                    crate::model::cdp::def::ParamKind::Toggle { .. } => {
                        crate::model::cdp::ParamValue::Toggle(true)
                    }
                    kind => kind.default_value(),
                })
                .collect();
            // Re-derived rather than assumed: `values` turns *every* toggle on, including
            // `Play_result`, and only the drawing ones make this a picture run.
            if !crate::model::praat::plan::draws_picture(def, &values) {
                continue;
            }
            // ...so put the play toggles back, or the sweep spends the fixture's duration in
            // real time on each of 257 entries.
            let values: Vec<_> = def
                .params
                .iter()
                .zip(values)
                .map(|(param, value)| {
                    if param.name.to_ascii_lowercase().starts_with("play") {
                        crate::model::cdp::ParamValue::Toggle(false)
                    } else {
                        value
                    }
                })
                .collect();
            let planned = match crate::model::praat::plan_praat_job_with(def, &values, &checkout, python_venv_interpreter(&crate::ui::app::praat_state_dir()).as_deref(), 1) {
                Ok(planned) => planned,
                Err(err) => {
                    failures.push(format!("{}: plan failed: {err}", def.key));
                    continue;
                }
            };
            let input_count = planned.input_names.len();
            let job = PraatJob {
                id: 0,
                praat_bin: praat_bin_for(""),
                inputs: vec![channels.clone(); input_count],
                // The image sonifiers all draw, so they reach this sweep too and need the same
                // fixture the main one supplies.
                photo_path: planned.photo_input.then(|| photo.clone()),
                planned,
                input_sample_rate: sample_rate,
                purpose: JobPurpose::Apply,
                // Not `DRAWING_TIMEOUT`. That figure is headroom for one interactive run the
                // user is waiting on; here it is 300 of them in a row, where a single script
                // that never returns would look indistinguishable from a hung sweep for four
                // minutes. 60s is well past the slowest measured drawing run.
                timeout: Duration::from_secs(60),
                prefs_dir: prefs.clone(),
                // The sweep exercises the `py` group too, so it needs the same venv the app
                // uses — otherwise every Python-backed process fails on missing imports here
                // while working perfectly in the app.
                python_venv_bin: python_venv_bin(&state_dir()),
            };
            // Printed before the run, not after: this is the line that tells you which entry
            // the sweep is *currently* on when it stops moving.
            index += 1;
            eprintln!("[{index}] {}", def.key);
            match run(&job) {
                Ok(out) if out.picture.is_some() => drew += 1,
                Ok(_) => {
                    blank += 1;
                    eprintln!("  no picture: {}", def.key);
                }
                Err(err) => {
                    let detail = match &err {
                        PraatError::NonZeroExit { output, .. } => output
                            .lines()
                            .find(|l| l.starts_with("Error:"))
                            .unwrap_or("(no Error: line)")
                            .to_string(),
                        other => other.to_string(),
                    };
                    failures.push(format!("{}: {detail}", def.key));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&state);

        eprintln!(
            "praat draw smoke: {drew} drew, {blank} produced nothing, {} failed",
            failures.len()
        );
        for failure in &failures {
            eprintln!("  {failure}");
        }
        assert!(drew + blank + failures.len() > 0, "no drawing entries found in the catalog");
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

    /// A checkout sitting beside the executable is found with no configuration at all — see
    /// `config::default_praat_audiotools_dir`, whose walk up from the executable is how a
    /// `cargo run` or `cargo build` tree has always resolved the scripts (`target/release/tui-wave`
    /// sits two levels below the repository root).
    ///
    /// Two properties are pinned. That the walk resolves a sibling `third_party/praat-audiotools`,
    /// and that `validate_audiotools_dir` needs only `Distortion/` and `Reverb/` rather than the
    /// full set of category directories — so a partial checkout that is missing, say, `Max-MSP/`
    /// (7.2 MB of Max patches this app never reads) still validates.
    #[test]
    fn a_checkout_beside_the_executable_validates() {
        let root = std::env::temp_dir().join(format!("praat-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bundled = root.join("third_party").join("praat-audiotools");
        // Every category directory the real submodule has except Max-MSP, abbreviated to the
        // two the validator actually looks for plus one more, so this is not merely the
        // sentinel pair echoed back.
        for c in ["Distortion", "Reverb", "Generative & Synthesis"] {
            std::fs::create_dir_all(bundled.join(c)).unwrap();
        }
        assert!(
            validate_audiotools_dir(&bundled).is_ok(),
            "a checkout without Max-MSP/ must still validate"
        );

        // And the executable-relative walk finds it from a binary sitting beside it:
        // `<root>/tui-wave` alongside `<root>/third_party/praat-audiotools`.
        let exe = root.join("tui-wave");
        let found = exe
            .ancestors()
            .skip(1)
            .take(4)
            .map(|a| a.join("third_party/praat-audiotools"))
            .find(|c| c.is_dir());
        assert_eq!(found.as_deref(), Some(bundled.as_path()), "the bundled layout must resolve");

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod venv_tests {
    use super::*;

    /// No venv means `PATH` is inherited untouched. Someone who already has numpy/scipy/
    /// soundfile on their system interpreter must not be forced to create one to keep working.
    #[test]
    fn without_a_venv_the_child_inherits_path_unchanged() {
        assert_eq!(path_with_venv(None), None);
    }

    /// With one, its `bin` goes to the **front**, so a bare `python3` — which is exactly how
    /// these scripts resolve their interpreter on Linux — finds the venv's before the system's.
    #[test]
    fn a_venv_goes_to_the_front_of_path() {
        let venv = Path::new("/state/pyenv/bin");
        let joined = path_with_venv(Some(venv)).expect("a PATH is built");
        let first = std::env::split_paths(&joined).next().expect("at least one entry");
        assert_eq!(first, venv, "the venv must win over anything already on PATH");
        // And nothing is dropped: whatever was there is still reachable behind it.
        let existing = std::env::var_os("PATH").unwrap_or_default();
        assert_eq!(
            std::env::split_paths(&joined).count(),
            std::env::split_paths(&existing).count() + 1
        );
    }

    /// The venv is only reported when it is really there — `python_venv_bin` returning a path
    /// for a missing directory would put a nonexistent entry on `PATH` and mask the real one.
    #[test]
    fn a_missing_venv_is_reported_as_absent() {
        let empty = std::env::temp_dir().join(format!("tui-wave-venv-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        assert_eq!(python_venv_bin(&empty), None);

        let bin = empty.join("pyenv").join("bin");
        std::fs::create_dir_all(&bin).expect("create probe venv");
        assert_eq!(python_venv_bin(&empty), Some(bin));
        let _ = std::fs::remove_dir_all(&empty);
    }
}
