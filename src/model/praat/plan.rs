//! Turning a Praat `ProcessDef` plus concrete `ParamValue`s into everything the runner needs
//! to execute one job. Pure — no filesystem access, no spawning; it returns file *contents* and
//! *names*, and `src/praat/runner.rs` is what writes and runs them.
//!
//! This is the Praat counterpart of `model::cdp::pipeline::plan_job`, and deliberately a
//! separate type rather than a reuse of `PlannedJob`. That struct carries roughly a dozen
//! CDP-only result shapes (`output_curve`, `output_formant_buffer`, `glob_output`,
//! `matrix_gain_calibration`, …) which a Praat job can never produce; threading `None` through
//! all of them would say less than a three-field struct that says exactly what happens.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use super::driver::{driver_script, DriverArg, DriverError, DriverOptions};
use super::rewrite::Assignment;
use crate::model::cdp::def::{Backend, IoKind, ParamDef, ParamKind, ParamValue, ProcessDef};

/// Temp-file names inside the job's own directory. Fixed rather than generated: the directory
/// is per-job and disposable, so there is nothing to collide with, and a fixed name makes a
/// failed run's leftovers readable when debugging. Inputs are numbered by `input_wav_name`.
pub const OUTPUT_WAV: &str = "out.wav";
pub const DRIVER_SCRIPT: &str = "driver.praat";
pub const PICTURE_PNG: &str = "picture.png";
/// Name the pause-rewritten copy of a plugin script is written under. See [`PauseRewrite`].
pub const REWRITTEN_SCRIPT: &str = "process.praat";

/// Everything the runner must materialise and execute for one Praat job.
#[derive(Debug, Clone, PartialEq)]
pub struct PraatPlannedJob {
    /// Filename to write `driver_source` to, relative to the job's temp directory.
    pub driver_name: String,
    /// The generated driver script's full text.
    pub driver_source: String,
    /// Temp WAVs the runner writes the inputs to, in order, relative to the job's temp
    /// directory. One entry for an ordinary process; two for a `DualWav` one, where the script
    /// reads them as `selected("Sound", 1)` and `selected("Sound", 2)`.
    pub input_names: Vec<String>,
    /// Temp WAV the driver saves its result to, which the runner reads back afterwards.
    pub output_name: String,
    /// Temp PNG the driver saves Praat's Picture window to, relative to the job's temp
    /// directory, or `None` when this run draws nothing (see [`draws_picture`]). Doubles as the
    /// signal to the runner: `Some` means pass a third path on argv and try to read it back.
    pub picture_name: Option<String>,
    /// Whether the driver expects a `Photo_file` path on argv — true exactly for an
    /// `IoKind::Photo` process. The *path* is not here: unlike every other file this struct
    /// names, it is one the **user** already has on disk rather than one the runner
    /// materialises, so it travels on `PraatJob.photo_path` beside the input audio, and this
    /// module stays free of any real filesystem path it did not derive itself.
    pub photo_input: bool,
    /// Absolute path to the plugin script being run — resolved here so the runner never has to
    /// know how the submodule is laid out.
    pub script_path: PathBuf,
    /// Short human-readable label for the progress display.
    pub label: String,
    /// `Some` when this process's settings live in a Praat `beginPause` dialog, which cannot be
    /// shown under `--run` (it segfaults). The runner writes a rewritten copy of the script into
    /// the job's temp directory and the driver calls *that* instead. See `praat::rewrite`.
    pub pause_rewrite: Option<PauseRewrite>,
    /// `Some` for a process whose script tui-wave ships itself — the runner writes this text
    /// into the job's temp directory and the driver calls it. See `praat::builtin`.
    pub builtin_source: Option<BuiltinScript>,
    /// `Some` when the script picks its own Python interpreter and we have one to point it at.
    /// The runner writes a copy with every interpreter assignment repointed. See
    /// `praat::python` for why `PATH` alone cannot do this on macOS.
    pub python_rewrite: Option<PythonRewrite>,
}

/// Repointing a `py`-group script's interpreter.
#[derive(Debug, Clone, PartialEq)]
pub struct PythonRewrite {
    /// Name to write the rewritten copy under, inside the job's temp directory — relative for
    /// the same reason [`PauseRewrite::script_name`] is.
    pub script_name: String,
    /// Absolute path to the interpreter every assignment is repointed at.
    pub interpreter: String,
}

/// A script the app carries rather than reads from the submodule.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinScript {
    /// Name to write it under inside the job's temp directory. Relative for the same reason
    /// [`PauseRewrite::script_name`] is: `runScript:` resolves a relative path against the
    /// calling script's folder, and the driver sits in that same directory.
    pub script_name: String,
    /// The script's full text.
    pub source: String,
}

/// What the runner must do to turn a pause-dialog script into a runnable one.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseRewrite {
    /// Name to write the rewritten copy under, inside the job's temp directory. Relative on
    /// purpose: `runScript:` resolves a relative path against the *calling* script's folder,
    /// and the generated driver sits in that same directory, so the two always agree however
    /// the temp directory is named.
    pub script_name: String,
    /// Assignments replacing each pause block, keyed by the block's index in the original
    /// script. A block with no entry is deleted outright — see `rewrite_pause_blocks`.
    pub blocks: BTreeMap<usize, Vec<Assignment>>,
    /// `boolean` form fields to delete from the script and assign instead, as (label, value).
    /// For a switch that exists only to gate a hoisted block: once its parameters are in this
    /// app's dialog the switch controls nothing, so it is removed rather than shown as a
    /// checkbox that refuses to be clicked.
    pub form_locks: Vec<(String, f64)>,
    /// Folders picked in this app's own dialog, standing in for the `chooseDirectory$` modals the
    /// script would otherwise open, as (variable including its `$`, absolute path). See
    /// `rewrite::rewrite_directory_choosers`.
    pub directories: Vec<(String, String)>,
}

/// The assignment standing in for one hoisted param, or `None` for a param that travels as an
/// ordinary `runScript:` argument.
///
/// Mirrors `argument_for`'s typing rules, with one addition that `argument_for` never needs: an
/// `optionmenu` sets a numeric variable *and* a `$` one, because the scripts read both.
fn assignment_for(param: &ParamDef, value: &ParamValue) -> Result<Assignment, PraatPlanError> {
    let label = param.name.clone();
    match (&param.kind, value) {
        (ParamKind::Number { .. }, ParamValue::Number(v)) => {
            Ok(Assignment::Number { label, value: *v })
        }
        (ParamKind::Toggle { .. }, ParamValue::Toggle(on)) => {
            Ok(Assignment::Number { label, value: if *on { 1.0 } else { 0.0 } })
        }
        (ParamKind::Choice { options, .. }, ParamValue::Choice(i)) => options
            .get(*i)
            .map(|text| Assignment::Choice { label, index: *i + 1, text: text.clone() })
            .ok_or_else(|| PraatPlanError::ChoiceOutOfRange {
                param: param.name.clone(),
                index: *i,
                options: options.len(),
            }),
        (ParamKind::Text { .. }, ParamValue::Text(text))
        | (ParamKind::FolderPath, ParamValue::Text(text)) => {
            Ok(Assignment::Text { label, value: text.clone() })
        }
        (ParamKind::FilePath { .. }, ParamValue::FilePath(path)) => {
            Ok(Assignment::Text { label, value: path.clone() })
        }
        (ParamKind::NumberList { separator, .. }, ParamValue::List(values)) => {
            let joined: Vec<String> = values.iter().map(|v| format!("{v}")).collect();
            Ok(Assignment::Text { label, value: joined.join(separator) })
        }
        (ParamKind::Number { .. }, _)
        | (ParamKind::Toggle { .. }, _)
        | (ParamKind::Choice { .. }, _) => {
            Err(PraatPlanError::ParamTypeMismatch { param: param.name.clone() })
        }
        _ => Err(PraatPlanError::UnsupportedParamKind { param: param.name.clone() }),
    }
}

/// Why a Praat job could not be planned.
#[derive(Debug, Clone, PartialEq)]
pub enum PraatPlanError {
    /// The definition is a CDP process; it belongs in `pipeline::plan_job`.
    NotAPraatProcess,
    /// `values` did not line up with `def.params`. Praat matches a script's `form` fields to
    /// arguments strictly by position and count — a mismatch is not a warning there, it is
    /// `Found N arguments but expected more.` and exit 255 — so it is caught here instead.
    ParamCountMismatch { expected: usize, got: usize },
    /// A `ParamValue` whose variant does not match its `ParamKind`.
    ParamTypeMismatch { param: String },
    /// A `ParamKind` with no Praat argument spelling. Praat forms only ever declare scalar
    /// fields, so the datafile-shaped kinds CDP needs (`Table`, `MarkerTimeList`, `HiliteBand`,
    /// `CrystalVdat`, …) can never appear on a converted entry; this exists so a hand-written
    /// user catalog entry gets a clear message rather than a silently wrong argv.
    UnsupportedParamKind { param: String },
    /// A `Choice` index outside the option list.
    ChoiceOutOfRange { param: String, index: usize, options: usize },
    /// A value with no Praat spelling (see `DriverError`).
    Driver(DriverError),
}

impl std::fmt::Display for PraatPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PraatPlanError::NotAPraatProcess => write!(f, "not a Praat process"),
            PraatPlanError::ParamCountMismatch { expected, got } => {
                write!(f, "process takes {expected} parameter(s) but {got} were supplied")
            }
            PraatPlanError::ParamTypeMismatch { param } => {
                write!(f, "parameter “{param}” was given a value of the wrong type")
            }
            PraatPlanError::UnsupportedParamKind { param } => {
                write!(f, "parameter “{param}” has no Praat equivalent")
            }
            PraatPlanError::ChoiceOutOfRange { param, index, options } => write!(
                f,
                "parameter “{param}” selected option {index} but only {options} exist"
            ),
            PraatPlanError::Driver(err) => write!(f, "{err}"),
        }
    }
}

/// Map one parameter's value onto the argument Praat's `form` expects for it.
///
/// The `ParamKind` decides the *spelling*, not the `ParamValue`: a `Toggle` becomes the numeric
/// `0`/`1` a Praat `boolean` field reads, and a `Choice` becomes its option **label**, because
/// Praat matches an `optionmenu` by label text and rejects a bare index.
fn argument_for(def: &ProcessDef, index: usize, value: &ParamValue) -> Result<DriverArg, PraatPlanError> {
    let param = &def.params[index];
    let name = || param.name.clone();
    match (&param.kind, value) {
        (ParamKind::Number { .. }, ParamValue::Number(v)) => Ok(DriverArg::Number(*v)),
        (ParamKind::Toggle { .. }, ParamValue::Toggle(on)) => {
            Ok(DriverArg::Number(if *on { 1.0 } else { 0.0 }))
        }
        (ParamKind::Choice { options, .. }, ParamValue::Choice(i)) => options
            .get(*i)
            .map(|label| DriverArg::Text(label.clone()))
            .ok_or_else(|| PraatPlanError::ChoiceOutOfRange {
                param: name(),
                index: *i,
                options: options.len(),
            }),
        // Joined with the script's *own* delimiter, never a normalised one: the receiving
        // script splits on exactly this and nothing else, and across the eleven scripts of
        // this shape it is variously a space, a comma, an underscore, or nothing at all
        // (`BPM_Panning`'s accent grid is one digit per sixteenth, `1010100110101001`).
        //
        // Entries are formatted the same way a `Number` argument is, so `2` reaches the script
        // as `2` and not `2.0000000000000004` — the field's own `integer` flag has already
        // rounded where it applies, and this only has to avoid re-introducing float noise.
        // Free text and a picked folder are both just strings to Praat — a `sentence`/`word`/
        // `text` field takes one, and a folder path is a `sentence` field the app fills with a
        // browser instead of the keyboard.
        (ParamKind::Text { .. }, ParamValue::Text(text))
        | (ParamKind::FolderPath, ParamValue::Text(text)) => Ok(DriverArg::Text(text.clone())),
        // A picked file is a path like any other string. Missed on the first pass, which left
        // `SPEAR_Par-Text-Frame_Format_Parser` unable to plan at all ("has no Praat
        // equivalent") — caught by the real-binary sweep, not by any unit test, because
        // nothing else in the catalog pairs a `FilePath` param with the Praat backend.
        (ParamKind::FilePath { .. }, ParamValue::FilePath(path)) => Ok(DriverArg::Text(path.clone())),
        (ParamKind::NumberList { separator, .. }, ParamValue::List(values)) => {
            // `{v}` on an f64 is what `driver::praat_number_literal` uses for a numeric
            // argument, and prints 2.0 as `2` — the list editor clamps every entry into the
            // param's own bounds, so nothing non-finite can reach here.
            let joined: Vec<String> = values.iter().map(|v| format!("{v}")).collect();
            Ok(DriverArg::Text(joined.join(separator)))
        }
        (ParamKind::Number { .. }, _)
        | (ParamKind::Toggle { .. }, _)
        | (ParamKind::Choice { .. }, _) => Err(PraatPlanError::ParamTypeMismatch { param: name() }),
        _ => Err(PraatPlanError::UnsupportedParamKind { param: name() }),
    }
}

/// Plan one Praat job.
///
/// `audiotools_dir` is the root of the praatAudioTools checkout; `def.bin` is a path relative to
/// it (e.g. `Distortion/Wavefolder__Foldback_.praat`), so the two join to the script to run.
/// Test-only now that every production caller supplies an interpreter. Kept because the ~25
/// tests below are about parameters, drivers and pause hoisting, none of which involve Python,
/// and threading a `None` through all of them would say nothing.
#[cfg(test)]
pub fn plan_praat_job(
    def: &ProcessDef,
    values: &[ParamValue],
    audiotools_dir: &Path,
) -> Result<PraatPlannedJob, PraatPlanError> {
    plan_praat_job_with(def, values, audiotools_dir, None)
}

/// [`plan_praat_job`] with an explicit Python interpreter for the `py` group.
///
/// A separate entry point rather than a fourth parameter on the original because the
/// interpreter is irrelevant to all but 34 of the 435 catalogued processes, and every existing
/// caller and test that has nothing to do with Python should not have to say so. `None` leaves
/// each script's own discovery in place, which is right when no app-owned venv exists: someone
/// with the packages on their system interpreter should keep working.
pub fn plan_praat_job_with(
    def: &ProcessDef,
    values: &[ParamValue],
    audiotools_dir: &Path,
    python_interpreter: Option<&Path>,
) -> Result<PraatPlannedJob, PraatPlanError> {
    if def.backend() != Backend::Praat {
        return Err(PraatPlanError::NotAPraatProcess);
    }
    if values.len() != def.params.len() {
        return Err(PraatPlanError::ParamCountMismatch {
            expected: def.params.len(),
            got: values.len(),
        });
    }

    // A hoisted param has no slot in the script's `form` -- the whole reason it lives in a
    // pause dialog is that Praat allows only one form -- so it is assigned inside the rewritten
    // copy instead of being passed positionally. Splitting here keeps the argument list exactly
    // what the script's own form declares, which is what Praat matches by position and count.
    let mut args = Vec::new();
    let mut blocks: BTreeMap<usize, Vec<Assignment>> = BTreeMap::new();
    // Same split, for the folder a script would otherwise ask for with `chooseDirectory$`: the
    // form has no slot for it either, so it is assigned in the copy rather than passed.
    let mut directories: Vec<(String, String)> = Vec::new();
    // Numbers split out of one `key=value` field rejoin into a single argument at the position
    // of the first — see `ParamDef::key_value_group`. They are emitted consecutively by the
    // converter, so tracking just the run in progress is enough.
    let mut open_group: Option<(String, usize)> = None;
    for (i, value) in values.iter().enumerate() {
        let param = &def.params[i];
        if let (Some(group), Some(key)) = (&param.key_value_group, &param.key_value_key) {
            let ParamValue::Number(number) = value else {
                return Err(PraatPlanError::ParamTypeMismatch { param: param.name.clone() });
            };
            let pair = format!("{key}={number}");
            match &open_group {
                // Same field as the previous param: append to the argument already placed.
                Some((open, at)) if open == group => {
                    if let Some(DriverArg::Text(text)) = args.get_mut(*at) {
                        text.push(' ');
                        text.push_str(&pair);
                    }
                }
                _ => {
                    open_group = Some((group.clone(), args.len()));
                    args.push(DriverArg::Text(pair));
                }
            }
            continue;
        }
        open_group = None;
        if let Some(variable) = &param.praat_directory_var {
            let ParamValue::Text(path) = value else {
                return Err(PraatPlanError::ParamTypeMismatch { param: param.name.clone() });
            };
            directories.push((variable.clone(), path.clone()));
            continue;
        }
        match param.praat_pause_block {
            Some(block) => blocks.entry(block).or_default().push(assignment_for(param, value)?),
            None => args.push(argument_for(def, i, value)?),
        }
    }

    let script_path = audiotools_dir.join(&def.bin);
    let form_locks: Vec<(String, f64)> = def
        .praat_form_locks
        .iter()
        .map(|(label, on)| (label.clone(), if *on { 1.0 } else { 0.0 }))
        .collect();
    // A script with no dialog to rewrite still needs a copy when it carries the branch-scoped
    // variable defect — see `rewrite::repair_branch_scoped_variables`. Detected by reading the
    // script here rather than recorded in the catalog, so it is re-derived every run and an
    // upstream fix takes effect immediately, with nothing here to go stale. Best-effort: an
    // unreadable script is not this function's failure to report, and the runner reads it again
    // and reports properly.
    let needs_branch_repair = std::fs::read_to_string(&script_path)
        .map(|source| {
            crate::model::praat::rewrite::repair_branch_scoped_variables(&source) != source
        })
        .unwrap_or(false);
    let pause_rewrite = (!blocks.is_empty()
        || !form_locks.is_empty()
        || !directories.is_empty()
        || needs_branch_repair)
        .then(|| PauseRewrite {
            script_name: REWRITTEN_SCRIPT.to_string(),
            blocks,
            form_locks,
            directories,
        });
    let input_count = praat_input_count(def);
    let save_picture = draws_picture(def, values);
    // A built-in ships with the app rather than living in the submodule, so there is nothing at
    // `script_path` to run — the runner writes the embedded text beside the driver instead. Same
    // shape as the pause rewrite below it, and mutually exclusive with it: a built-in has no
    // pause dialog to rewrite, being written here.
    let builtin_source = def
        .praat_builtin
        .then(|| super::builtin::source_for(&def.key))
        .flatten()
        .map(|source| BuiltinScript {
            script_name: super::builtin::BUILTIN_SCRIPT.to_string(),
            source: source.to_string(),
        });
    // A `py`-group script resolves its own interpreter, and on macOS resolves it to an absolute
    // path that `PATH` cannot influence — so the venv the app installed is bypassed and every
    // import fails. Repointing happens in a copy, like the pause rewrite, and under the same
    // filename: the runner applies whichever passes are asked for to **one** copy, in sequence.
    // That composition is not hypothetical — `Semantic_timbre_retrieval` is a `py`-group script
    // whose corpus folder is hoisted out of a `chooseDirectory$` call, so it needs both.
    let python_rewrite = def
        .praat_python_rewrite
        .then_some(python_interpreter)
        .flatten()
        .map(|interpreter| PythonRewrite {
            script_name: REWRITTEN_SCRIPT.to_string(),
            interpreter: interpreter.to_string_lossy().into_owned(),
        });
    // Both a rewritten copy and a built-in are siblings of the driver, so a bare filename
    // resolves — see `PauseRewrite::script_name`.
    let called_script = match (&builtin_source, &pause_rewrite, &python_rewrite) {
        (Some(builtin), _, _) => builtin.script_name.clone(),
        (None, Some(rewrite), _) => rewrite.script_name.clone(),
        (None, None, Some(rewrite)) => rewrite.script_name.clone(),
        (None, None, None) => script_path.to_string_lossy().into_owned(),
    };
    let photo_input = praat_needs_photo(def);
    let driver_source = driver_script(
        &called_script,
        &args,
        DriverOptions { input_count, save_picture, photo_input },
    )
    .map_err(PraatPlanError::Driver)?;

    Ok(PraatPlannedJob {
        driver_name: DRIVER_SCRIPT.to_string(),
        driver_source,
        input_names: (1..=input_count).map(input_wav_name).collect(),
        output_name: OUTPUT_WAV.to_string(),
        picture_name: save_picture.then(|| PICTURE_PNG.to_string()),
        photo_input,
        script_path,
        label: def.title.clone(),
        pause_rewrite,
        builtin_source,
        python_rewrite,
    })
}

/// Whether this run will paint something into Praat's Picture window — i.e. whether the driver
/// should save it and the app should offer to show it.
///
/// True when any **toggle** whose name reads as a drawing switch is on. The names come from the
/// plugin's own `form` blocks and are wildly inconsistent (`Draw_visualization` on 267 entries,
/// `Show_visualization` on 14, then `Draw_response`, `Draw_spectrogram`, `Visualize`, …), so a
/// prefix match is the only thing that covers them without a 300-line table that the next
/// submodule bump would invalidate.
///
/// The prefixes are `scripts/convert_praat_audiotools.py`'s `SILENCE_RE` **minus**
/// `play|demo|open_|export`, and the exclusions are the interesting part: `play` costs the
/// selection's duration in real time and draws nothing; `demo` is Praat's Demo window, which
/// cannot be saved and which only two scripts touch anyway; `open_` never matched a real toggle
/// (only `Open_phase`, a glottal-source *number*, which cannot match here since the kind is
/// checked); and `export` writes its own files. Keep the two lists in step — if `SILENCE_RE`
/// grows a prefix, decide here whether it draws.
///
/// **Deliberately over-inclusive**, with no exclusion list for Info-window-only toggles like
/// `Show_info`. A false positive costs one wasted `Save as PNG` whose all-white result is
/// discarded by the blank check in `praat::picture`; a false negative silently loses a picture
/// the user asked for and gives them nothing to look at. Those are not comparable costs.
///
/// Checking `ParamKind::Toggle` and not just the name is what keeps `Visualization_delay` and
/// `Show_every_n_frames` — numbers, not switches — from matching.
pub fn draws_picture(def: &ProcessDef, values: &[ParamValue]) -> bool {
    def.params.iter().zip(values).any(|(param, value)| {
        matches!(param.kind, ParamKind::Toggle { .. })
            && matches!(value, ParamValue::Toggle(true))
            && is_picture_toggle_name(&param.name)
    })
}

/// Whether a parameter *name* reads as a drawing switch. Public because the UI greys these out
/// when the terminal cannot show a picture, and must agree exactly with what the planner will
/// actually act on.
pub fn is_picture_toggle_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["draw", "show", "visuali"].iter().any(|prefix| name.starts_with(prefix))
}

/// How many Sound objects this process expects selected, read off its declared `input`.
///
/// `IoKind::DualWav` is CDP's existing "two input files" kind and carries over unchanged: the
/// meaning ("this process needs a second buffer, and the UI must offer a picker for it") is the
/// same on both backends even though the mechanism differs entirely.
pub fn praat_input_count(def: &ProcessDef) -> usize {
    match def.input {
        IoKind::DualWav => 2,
        // A process that creates its Sound rather than transforming one — Record, today. The
        // driver emits no `infile` field and no `selectObject:` for this, and the runner writes
        // no temp WAV, so such a process runs with no document open at all. That is the point of
        // it: needing something already loaded before you could record would defeat the feature.
        //
        // `Photo` counts zero for the same reason: the image sonifiers *generate* a Sound from a
        // picture. The picture is still selected before `runScript:`, but it is not a Sound and
        // so takes no slot here — the driver's `photo_input` handles it separately.
        IoKind::None | IoKind::Photo => 0,
        _ => 1,
    }
}

/// Whether this process wants a Praat `Photo` object selected — the four image-sonification
/// scripts, which `exitScript` immediately without one ("Please select a Photo object first.").
///
/// Read off `def.input` rather than sniffed from the params, so a hand-written user catalog
/// entry opts in the same way the generated ones do.
pub fn praat_needs_photo(def: &ProcessDef) -> bool {
    def.input == IoKind::Photo
}

/// Temp filename for the nth input (1-based), matching the driver's `Input_file_N` form field.
pub fn input_wav_name(index: usize) -> String {
    format!("in_{index}.wav")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::def::{Category, NumberScale, ParamDef};


    fn number(name: &str, default: f64) -> ParamDef {
        ParamDef {
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
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            rows_match_input_count: false,
            kind: ParamKind::Number {
                min: 0.0,
                max: 10.0,
                step: 0.1,
                default,
                exponential: false,
                scale: NumberScale::Plain,
                integer: false,
            },
        }
    }

    fn with_kind(name: &str, kind: ParamKind) -> ParamDef {
        ParamDef { kind, ..number(name, 0.0) }
    }

    fn praat_def(params: Vec<ParamDef>) -> ProcessDef {
        ProcessDef {
            key: "praat_distortion_wavefolder".into(),
            bin: "Distortion/Wavefolder__Foldback_.praat".into(),
            subprog: None,
            mode: None,
            title: "Wavefolder (Foldback)".into(),
            category: Category::Praat,
            subcategory: String::new(),
            short_description: String::new(),
            description: String::new(),
            input: IoKind::Wav,
            output: IoKind::Wav,
            stereo_native: true,
            output_is_stereo: false,
            input_channels: None,
            output_channels: None,
            output_new_buffer: false,
            interactive: false,
            praat_form_locks: Vec::new(),
            praat_builtin: false,
            praat_python_rewrite: false,
            requires_simple_wav_input: false,
            sidecar_extension: None,
            min_inputs: None,
            needs_head_tail_marks: false,
            head_tail_marks_unpaired: false,
            flags_before_infile: false,
            channel_split: None,
            spec_grab_prepass: false,
            preset_param: None,
            preset_custom_option: 0,
            script_presets: Vec::new(),
            params,
            param_notes: Vec::new(),
        }
    }

    #[test]
    fn the_script_path_is_the_checkout_joined_with_the_relative_bin() {
        let def = praat_def(vec![]);
        let job = plan_praat_job(&def, &[], Path::new("/opt/audiotools")).unwrap();
        assert_eq!(
            job.script_path,
            PathBuf::from("/opt/audiotools/Distortion/Wavefolder__Foldback_.praat")
        );
        assert!(job.driver_source.contains("/opt/audiotools/Distortion/Wavefolder__Foldback_.praat"));
    }

    #[test]
    fn a_number_param_becomes_a_bare_numeric_argument() {
        let def = praat_def(vec![number("Threshold", 0.5)]);
        let job = plan_praat_job(&def, &[ParamValue::Number(0.25)], Path::new("/p")).unwrap();
        assert!(job.driver_source.contains(".praat\", 0.25\n"));
    }

    /// A Praat `boolean` field reads `0`/`1`, not `no`/`yes` — confirmed empirically against
    /// the real binary during the feasibility probe.
    #[test]
    fn a_toggle_becomes_zero_or_one() {
        let def = praat_def(vec![with_kind("Play_result", ParamKind::Toggle { default: false })]);
        let off = plan_praat_job(&def, &[ParamValue::Toggle(false)], Path::new("/p")).unwrap();
        assert!(off.driver_source.contains(".praat\", 0\n"));
        let on = plan_praat_job(&def, &[ParamValue::Toggle(true)], Path::new("/p")).unwrap();
        assert!(on.driver_source.contains(".praat\", 1\n"));
    }

    /// The whole reason `ParamKind::Choice` fits Praat: it already stores labels, and Praat
    /// matches an `optionmenu` by label rather than by index.
    #[test]
    fn a_choice_becomes_its_option_label_not_its_index() {
        let def = praat_def(vec![with_kind(
            "Preset",
            ParamKind::Choice {
                options: vec!["Custom (use settings below)".into(), "Hard Fold".into()],
                default: 0,
            },
        )]);
        let job = plan_praat_job(&def, &[ParamValue::Choice(1)], Path::new("/p")).unwrap();
        assert!(job.driver_source.contains(".praat\", \"Hard Fold\"\n"));
        assert!(!job.driver_source.contains(".praat\", 1\n"));
    }

    #[test]
    fn params_are_emitted_in_declaration_order() {
        let def = praat_def(vec![
            with_kind(
                "Preset",
                ParamKind::Choice { options: vec!["Custom".into()], default: 0 },
            ),
            number("Threshold", 0.5),
            with_kind("Bipolar", ParamKind::Toggle { default: true }),
        ]);
        let job = plan_praat_job(
            &def,
            &[ParamValue::Choice(0), ParamValue::Number(0.5), ParamValue::Toggle(true)],
            Path::new("/p"),
        )
        .unwrap();
        assert!(job.driver_source.contains(".praat\", \"Custom\", 0.5, 1\n"));
    }

    /// Praat fills a form strictly by position and count, so a mismatch has to be caught before
    /// it becomes an opaque `Found N arguments but expected more.` at exit 255.
    #[test]
    fn too_few_values_is_refused_before_running() {
        let def = praat_def(vec![number("a", 1.0), number("b", 2.0)]);
        assert_eq!(
            plan_praat_job(&def, &[ParamValue::Number(1.0)], Path::new("/p")),
            Err(PraatPlanError::ParamCountMismatch { expected: 2, got: 1 })
        );
    }

    #[test]
    fn a_choice_index_past_the_options_is_refused() {
        let def = praat_def(vec![with_kind(
            "Preset",
            ParamKind::Choice { options: vec!["Only".into()], default: 0 },
        )]);
        assert_eq!(
            plan_praat_job(&def, &[ParamValue::Choice(3)], Path::new("/p")),
            Err(PraatPlanError::ChoiceOutOfRange {
                param: "Preset".into(),
                index: 3,
                options: 1,
            })
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused() {
        let def = praat_def(vec![number("Threshold", 0.5)]);
        assert_eq!(
            plan_praat_job(&def, &[ParamValue::Toggle(true)], Path::new("/p")),
            Err(PraatPlanError::ParamTypeMismatch { param: "Threshold".into() })
        );
    }

    /// A datafile-shaped kind cannot be spelled as a Praat form argument. Converted entries
    /// never produce one, but a hand-written user entry could.
    #[test]
    fn a_datafile_shaped_param_is_refused_with_its_name() {
        let def = praat_def(vec![with_kind(
            "Taps",
            ParamKind::Table { columns: vec![], time_column: None, transposed: false },
        )]);
        assert_eq!(
            plan_praat_job(&def, &[ParamValue::Table(vec![])], Path::new("/p")),
            Err(PraatPlanError::UnsupportedParamKind { param: "Taps".into() })
        );
    }

    /// `IoKind::DualWav` carries over from CDP unchanged: it means "needs a second buffer" on
    /// both backends, even though CDP passes a second filename on argv while Praat selects a
    /// second Sound object.
    #[test]
    fn a_dual_wav_process_plans_two_input_files() {
        let mut def = praat_def(vec![]);
        def.input = IoKind::DualWav;
        let job = plan_praat_job(&def, &[], Path::new("/p")).unwrap();
        assert_eq!(job.input_names, vec!["in_1.wav", "in_2.wav"]);
        assert!(job.driver_source.contains("selectObject: snd1, snd2"));
    }

    #[test]
    fn an_ordinary_process_plans_exactly_one_input_file() {
        let job = plan_praat_job(&praat_def(vec![]), &[], Path::new("/p")).unwrap();
        assert_eq!(job.input_names, vec!["in_1.wav"]);
    }

    fn toggle(name: &str) -> ParamDef {
        with_kind(name, ParamKind::Toggle { default: false })
    }

    /// The whole point: the toggle that 267 catalog entries carry now reaches the driver as a
    /// request for a picture rather than as a switch with no observable effect.
    #[test]
    fn a_draw_visualization_toggle_that_is_on_plans_a_picture() {
        let def = praat_def(vec![toggle("Draw_visualization")]);
        let job = plan_praat_job(&def, &[ParamValue::Toggle(true)], Path::new("/p")).unwrap();
        assert_eq!(job.picture_name.as_deref(), Some(PICTURE_PNG));
        assert!(job.driver_source.contains("Save as 300-dpi PNG file:"));
    }

    #[test]
    fn the_same_toggle_turned_off_plans_no_picture() {
        let def = praat_def(vec![toggle("Draw_visualization")]);
        let job = plan_praat_job(&def, &[ParamValue::Toggle(false)], Path::new("/p")).unwrap();
        assert_eq!(job.picture_name, None);
        assert!(!job.driver_source.contains("PNG"));
    }

    /// The plugin spells this switch a dozen different ways, so the prefix match has to cover
    /// all of them — a table of exact names would rot at the next submodule bump.
    #[test]
    fn every_spelling_of_the_drawing_switch_counts() {
        for name in [
            "Draw_visualization",
            "Show_visualization",
            "Draw_response",
            "Draw_spectrogram",
            "Visualize",
            "show_ANALYSIS", // matched case-insensitively
        ] {
            let def = praat_def(vec![toggle(name)]);
            let job = plan_praat_job(&def, &[ParamValue::Toggle(true)], Path::new("/p")).unwrap();
            assert!(job.picture_name.is_some(), "{name} was not treated as a drawing toggle");
        }
    }

    /// `Play_result` is on 257 entries and is the one switch that must *not* count: it draws
    /// nothing and costs the selection's duration in real time.
    #[test]
    fn a_play_toggle_alone_plans_no_picture() {
        let def = praat_def(vec![toggle("Play_result")]);
        let job = plan_praat_job(&def, &[ParamValue::Toggle(true)], Path::new("/p")).unwrap();
        assert_eq!(job.picture_name, None);
    }

    /// Checking the *kind* and not just the name is what keeps a number like
    /// `Visualization_delay` from being read as a request to draw.
    #[test]
    fn a_number_whose_name_starts_like_a_drawing_switch_is_not_one() {
        let def = praat_def(vec![number("Visualization_delay", 0.25)]);
        let job = plan_praat_job(&def, &[ParamValue::Number(0.25)], Path::new("/p")).unwrap();
        assert_eq!(job.picture_name, None);
    }

    /// A real entry carries several toggles at once; one drawing switch anywhere in the list is
    /// enough, whatever its position.
    #[test]
    fn one_drawing_toggle_among_many_params_is_enough() {
        let def = praat_def(vec![
            number("Threshold", 0.5),
            toggle("Play_result"),
            toggle("Draw_visualization"),
        ]);
        let job = plan_praat_job(
            &def,
            &[ParamValue::Number(0.5), ParamValue::Toggle(false), ParamValue::Toggle(true)],
            Path::new("/p"),
        )
        .unwrap();
        assert!(job.picture_name.is_some());
    }

    /// The two backends must not accept each other's definitions.
    #[test]
    fn a_cdp_process_is_refused() {
        let mut def = praat_def(vec![]);
        def.category = Category::Time;
        def.bin = "blur".into();
        assert_eq!(
            plan_praat_job(&def, &[], Path::new("/p")),
            Err(PraatPlanError::NotAPraatProcess)
        );
    }
}

#[cfg(test)]
mod pause_hoist_tests {
    use super::*;
    use crate::model::cdp::CdpCatalog;

    fn checkout() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools")
    }

    /// The whole point, end to end but without Praat: take a real catalog entry, plan it with
    /// real values, apply the rewrite to the real script, and check the values are *in* the
    /// script that would run. A rewrite that dropped them would still exit 0 and still produce
    /// audio — just the default sound every time — so the smoke test cannot catch this.
    #[test]
    fn a_hoisted_value_reaches_the_script_that_will_run() {
        let Ok(source) =
            std::fs::read_to_string(checkout().join("Distortion/Sidechain_Feedback_VCA.praat"))
        else {
            return; // submodule not initialised
        };
        let (catalog, _) = CdpCatalog::load(None);
        let def = catalog.find("praat_distortion_sidechain_feedback_vca").expect("entry exists");

        // Defaults everywhere, except the two hoisted fields set to something distinctive.
        let mut values: Vec<ParamValue> =
            def.params.iter().map(|p| p.kind.default_value()).collect();
        let gain = def.params.iter().position(|p| p.name == "Output_Gain").expect("Output_Gain");
        let mode = def.params.iter().position(|p| p.name == "Spatial_Mode").expect("Spatial_Mode");
        values[gain] = ParamValue::Number(0.375);
        values[mode] = ParamValue::Choice(3); // "Pseudo-Binaural (Delay/Filter)", the 4th option

        assert!(
            def.params[gain].praat_pause_block.is_some(),
            "Output_Gain must be a hoisted param, or this test proves nothing"
        );

        let job = plan_praat_job(def, &values, &checkout()).expect("plans");
        let rewrite = job.pause_rewrite.as_ref().expect("this entry rewrites");
        let script = crate::model::praat::rewrite::rewrite_pause_blocks(&source, &rewrite.blocks, &[])
            .expect("rewrites");

        // The exact variable names the script reads further down — note `output_Gain` keeps its
        // capital G, and the optionmenu sets both forms.
        assert!(script.contains("output_Gain = 0.375"), "the typed gain must reach the script");
        assert!(script.contains("spatial_Mode = 4"));
        assert!(script.contains("spatial_Mode$ = \"Pseudo-Binaural (Delay/Filter)\""));

        // A hoisted param must NOT also be passed as a runScript: argument — the script's form
        // has no slot for it, and Praat matches arguments by position and count.
        let form_params = def.params.iter().filter(|p| p.praat_pause_block.is_none()).count();
        assert_eq!(
            job.driver_source.matches(", ").count() >= form_params,
            true,
            "driver should carry the form's params only"
        );
        assert!(
            !job.driver_source.contains("0.375"),
            "a hoisted value must travel in the script, not on the argument list"
        );
    }

    /// The same end-to-end check for a hoisted *folder*, and it needs to be its own test for the
    /// same reason: a rewrite that dropped the path would still exit 0. It would just run
    /// against whatever `pairRoot$` happened to be — the empty string, so
    /// `Create Strings as file list: … "/good/*.wav"` would find nothing and the run would fail
    /// somewhere far from the cause.
    #[test]
    fn a_hoisted_folder_reaches_the_script_that_will_run() {
        let Ok(source) = std::fs::read_to_string(
            checkout().join("Analysis/OT_Grammar_Learning_from_Audio.praat"),
        ) else {
            return; // submodule not initialised
        };
        let (catalog, _) = CdpCatalog::load(None);
        let def = catalog
            .find("praat_analysis_ot_grammar_learning_from_audio")
            .expect("entry exists");

        let folder = def
            .params
            .iter()
            .position(|p| p.praat_directory_var.as_deref() == Some("pairRoot$"))
            .expect("the folder must be a hoisted param, or this test proves nothing");
        let mut values: Vec<ParamValue> =
            def.params.iter().map(|p| p.kind.default_value()).collect();
        values[folder] = ParamValue::Text("/corpora/ot pairs".into());

        let job = plan_praat_job(def, &values, &checkout()).expect("plans");
        let rewrite = job.pause_rewrite.as_ref().expect("this entry rewrites");
        let script =
            crate::model::praat::rewrite::rewrite_directory_choosers(&source, &rewrite.directories)
                .expect("rewrites");

        assert!(script.contains("pairRoot$ = \"/corpora/ot pairs\""), "the folder must be assigned");
        // And, like a hoisted pause param, must not *also* travel as a runScript: argument —
        // the script's form has no slot for it, and Praat matches arguments by position.
        assert!(
            !job.driver_source.contains("/corpora/ot pairs"),
            "a hoisted folder must travel in the script, not on the argument list"
        );
    }

    /// Every param kind the Praat catalog actually uses must have an argument spelling.
    ///
    /// This is the test that was missing when `ParamKind::FilePath` reached the Praat catalog
    /// for the first time: nothing else pairs that kind with this backend, so the gap only
    /// surfaced in the real-binary sweep, as `SPEAR_Par-Text-Frame_Format_Parser` failing to
    /// plan at all with "has no Praat equivalent". Planning every entry at its defaults costs a
    /// fraction of a second and closes the whole class.
    #[test]
    fn every_praat_entry_plans_at_its_defaults() {
        use crate::model::cdp::def::Backend;
        let (catalog, _) = CdpCatalog::load(None);
        let mut failures = Vec::new();
        for def in catalog.processes.iter().filter(|p| p.backend() == Backend::Praat) {
            let values: Vec<ParamValue> =
                def.params.iter().map(|p| p.kind.default_value()).collect();
            if let Err(err) = plan_praat_job(def, &values, &checkout()) {
                failures.push(format!("{}: {err}", def.key));
            }
        }
        assert!(failures.is_empty(), "entries that cannot be planned:\n  {}", failures.join("\n  "));
    }

    /// Numbers split out of a `key=value` field must rebuild exactly one argument holding every
    /// key, and must not each become an argument of their own — Praat matches a script's form by
    /// position and count, so a stray extra argument shifts everything after it.
    #[test]
    fn key_value_numbers_rejoin_into_one_argument() {
        let (catalog, _) = CdpCatalog::load(None);
        let def = catalog
            .find("praat_spatial_surround_physics_based_stereo_dynamics")
            .expect("entry exists");
        let mut values: Vec<ParamValue> =
            def.params.iter().map(|p| p.kind.default_value()).collect();
        // Change one key, so the test proves the typed value travels rather than the default.
        let grav = def.params.iter().position(|p| p.name == "Physics_grav").expect("Physics_grav");
        values[grav] = ParamValue::Number(1.62); // lunar gravity
        let job = plan_praat_job(def, &values, &checkout()).expect("plans");

        assert!(
            job.driver_source.contains("\"h0=1.2 v0=6 grav=1.62 rest=0.75 bounces=8\""),
            "the five Physics keys must arrive as one argument:\n{}",
            job.driver_source
        );
        // Three separate fields, three separate arguments — not one merged blob.
        assert!(job.driver_source.contains("\"start=-0.9 end=0.9 cycles=2\""), "Pan_path");
        assert!(job.driver_source.contains("\"width=3 listener=4 ref=1\""), "Geometry");
        // And the argument count still matches the script's own form, which is what Praat
        // checks: one argument per *field*, not per key.
        let groups: std::collections::BTreeSet<_> =
            def.params.iter().filter_map(|p| p.key_value_group.clone()).collect();
        let split_params = def.params.iter().filter(|p| p.key_value_group.is_some()).count();
        assert_eq!(groups.len(), 3);
        assert_eq!(split_params, 11, "eleven numbers collapsing into three arguments");
    }

    /// The nine split entries differ in exactly the way they should: each pins `algorithm$` to
    /// its own algorithm, which is what every one of the script's `if algorithm$ = "…"` guards
    /// tests.
    #[test]
    fn each_convolver_variant_pins_its_own_algorithm() {
        let Ok(source) =
            std::fs::read_to_string(checkout().join("Reverb/Universal Convolution Generator.praat"))
        else {
            return;
        };
        let (catalog, _) = CdpCatalog::load(None);
        let variants: Vec<_> = catalog
            .processes
            .iter()
            .filter(|p| p.key.starts_with("praat_reverb_universal_convolution_generator"))
            .collect();
        assert_eq!(variants.len(), 9, "one entry per algorithm");

        for def in variants {
            let values: Vec<ParamValue> =
                def.params.iter().map(|p| p.kind.default_value()).collect();
            let job = plan_praat_job(def, &values, &checkout()).expect("plans");
            let rewrite = job.pause_rewrite.as_ref().expect("rewrites");
            let script =
                crate::model::praat::rewrite::rewrite_pause_blocks(&source, &rewrite.blocks, &[])
                    .expect("rewrites");
            // Read from the entry's own locked Algorithm choice, not parsed out of the title:
            // one of the algorithms is literally "Fibonacci (Mono)", so splitting the title on
            // its parentheses picks up "Mono".
            let algorithm = def
                .params
                .iter()
                .find(|p| p.name == "Algorithm")
                .expect("every variant keeps its Algorithm param");
            let ParamKind::Choice { options, .. } = &algorithm.kind else {
                panic!("{}: Algorithm must stay a choice", def.key)
            };
            assert_eq!(options.len(), 1, "{}: locked to exactly one algorithm", def.key);
            let label = &options[0];
            assert!(
                script.contains(&format!("algorithm$ = \"{label}\"")),
                "{}: must pin algorithm$ to {label:?}",
                def.key
            );
            // This script declares no `form`, so nothing travels on the argument list at all.
            assert!(
                job.driver_source.contains(&format!("runScript: \"{REWRITTEN_SCRIPT}\"\n")),
                "{}: must call the rewritten copy with no arguments",
                def.key
            );
        }
    }
}

#[cfg(test)]
mod interactive_tests {
    use super::*;
    use crate::model::cdp::CdpCatalog;

    /// A process that opens its own window is marked, and it is the Python-GUI kind rather than
    /// the Praat-`beginPause` kind — those two look alike and behave nothing alike. The marker
    /// is what stops the runner killing a run mid-edit, which is how "broken pipe" on Apply
    /// happened.
    #[test]
    fn a_python_gui_process_is_marked_interactive() {
        let (catalog, _) = CdpCatalog::load(None);
        let interactive: Vec<_> = catalog.processes.iter().filter(|p| p.interactive).collect();
        assert!(!interactive.is_empty(), "the py group ships at least one Tk editor");
        for def in &interactive {
            assert!(
                def.bin.starts_with("py/"),
                "{}: only the Python-helper group can open a window that works — Praat's own \
                 beginPause segfaults under --run and must stay excluded, not marked",
                def.key
            );
            // It still has to be a plannable process, not a special case that skips the pipeline.
            let values: Vec<ParamValue> =
                def.params.iter().map(|p| p.kind.default_value()).collect();
            plan_praat_job(def, &values, Path::new("/plugins"))
                .unwrap_or_else(|e| panic!("{}: {e}", def.key));
        }
    }

    /// The four image sonifiers, as the catalog actually ships them.
    ///
    /// Each assertion is a thing that would fail *silently* if wrong: a Sound input would make
    /// the runner write a temp WAV the script never reads (and, with no document open, refuse
    /// to run at all); a missing `photo_input` would give the driver no `Photo_file` field
    /// while the runner still put a path on argv, which Praat answers with a bare exit 255.
    #[test]
    fn the_image_sonifiers_ask_for_a_photo_and_no_sound() {
        let (catalog, _) = CdpCatalog::load(None);
        let photo: Vec<_> =
            catalog.processes.iter().filter(|p| p.input == IoKind::Photo).collect();
        assert_eq!(photo.len(), 4, "the four image sonifiers, no more and no fewer");
        for def in &photo {
            assert_eq!(praat_input_count(def), 0, "{}: reads a picture, not a buffer", def.key);
            assert!(praat_needs_photo(def), "{}", def.key);
            let values: Vec<ParamValue> =
                def.params.iter().map(|p| p.kind.default_value()).collect();
            let planned = plan_praat_job(def, &values, Path::new("/plugins"))
                .unwrap_or_else(|e| panic!("{}: {e}", def.key));
            assert!(planned.photo_input, "{}: the driver was not told to read one", def.key);
            assert!(planned.input_names.is_empty(), "{}: no temp WAV to write", def.key);
            assert!(
                planned.driver_source.contains("infile Photo_file"),
                "{}: driver has no photo field",
                def.key
            );
        }
    }

    /// The inverse, across the whole catalog: no other process may claim a photo. `photo_input`
    /// drives an extra argv slot, so a stray one would break that process's runs entirely.
    #[test]
    fn no_other_process_asks_for_a_photo() {
        let (catalog, _) = CdpCatalog::load(None);
        for def in catalog.processes.iter().filter(|p| p.input != IoKind::Photo) {
            assert!(!praat_needs_photo(def), "{}", def.key);
        }
    }
}
