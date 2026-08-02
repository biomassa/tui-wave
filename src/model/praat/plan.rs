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

use super::driver::{driver_script, DriverArg, DriverError};
use crate::model::cdp::def::{Backend, IoKind, ParamKind, ParamValue, ProcessDef};

/// Temp-file names inside the job's own directory. Fixed rather than generated: the directory
/// is per-job and disposable, so there is nothing to collide with, and a fixed name makes a
/// failed run's leftovers readable when debugging. Inputs are numbered by `input_wav_name`.
pub const OUTPUT_WAV: &str = "out.wav";
pub const DRIVER_SCRIPT: &str = "driver.praat";

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
    /// Absolute path to the plugin script being run — resolved here so the runner never has to
    /// know how the submodule is laid out.
    pub script_path: PathBuf,
    /// Short human-readable label for the progress display.
    pub label: String,
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
pub fn plan_praat_job(
    def: &ProcessDef,
    values: &[ParamValue],
    audiotools_dir: &Path,
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

    let args = values
        .iter()
        .enumerate()
        .map(|(i, value)| argument_for(def, i, value))
        .collect::<Result<Vec<_>, _>>()?;

    let script_path = audiotools_dir.join(&def.bin);
    let input_count = praat_input_count(def);
    let driver_source = driver_script(&script_path.to_string_lossy(), &args, input_count)
        .map_err(PraatPlanError::Driver)?;

    Ok(PraatPlannedJob {
        driver_name: DRIVER_SCRIPT.to_string(),
        driver_source,
        input_names: (1..=input_count).map(input_wav_name).collect(),
        output_name: OUTPUT_WAV.to_string(),
        script_path,
        label: def.title.clone(),
    })
}

/// How many Sound objects this process expects selected, read off its declared `input`.
///
/// `IoKind::DualWav` is CDP's existing "two input files" kind and carries over unchanged: the
/// meaning ("this process needs a second buffer, and the UI must offer a picker for it") is the
/// same on both backends even though the mechanism differs entirely.
pub fn praat_input_count(def: &ProcessDef) -> usize {
    match def.input {
        IoKind::DualWav => 2,
        _ => 1,
    }
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
