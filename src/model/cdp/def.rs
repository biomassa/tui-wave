//! Data model for a CDP (Composer's Desktop Project) process definition: the catalog entry
//! that describes how to invoke one CDP command-line program and what parameters it takes.
//! Pure data — no process spawning, no UI — so the catalog can be parsed and unit-tested in
//! isolation. See `catalog.rs` for loading, `pipeline.rs` for turning a `ProcessDef` plus
//! concrete `ParamValue`s into actual command invocations.

use serde::{Deserialize, Serialize};

/// Broad process family, mirrors CDP's own split between time-domain and spectral
/// (phase-vocoder) processing. `pipeline.rs` uses this to decide whether a process needs
/// wrapping in `pvoc anal`/`pvoc synth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Time,
    Pvoc,
}

/// What kind of file a process reads/writes on one side. `Dual*` processes take two input
/// files (e.g. combine/morph) — modeled but not yet supported by the v1 UI (see
/// `pipeline::PlanError::UnsupportedInV1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoKind {
    None,
    Wav,
    Ana,
    DualWav,
    DualAna,
}

/// How a `Number` parameter's raw slider value (0-100 for percentage-based scales) maps to
/// the value actually passed on the CDP command line. Resolved at pipeline-planning time,
/// except `PercentOfAnaWindowCount` — see CDP-PLAN.md Phase 0 spike finding S5: CDP
/// recalculates the true analysis window length from the requested overlap factor in a way
/// that can't be predicted before `pvoc anal` actually runs, so that scale is resolved by the
/// runner after the analysis step completes, not by `pipeline::plan_job`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberScale {
    Plain,
    PercentOfInputDuration,
    PercentOfFftSize,
    PercentOfAnaWindowCount,
    OutputDurationSeconds,
}

/// A concrete value for one parameter, as edited in the UI. Also the shape a saved CDP
/// preset's per-param values take (`model::cdp::preset`) — `Serialize`/`Deserialize` exist
/// for that, not for the catalog itself (which only ever deserializes `ParamKind`). Default
/// (externally tagged) enum representation, not `ParamKind`'s internally-tagged one — these
/// are tuple variants (`Number(f64)`, not `Number { .. }`), which internal tagging can't
/// represent (there's no map to merge a "kind" field into).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamValue {
    Number(f64),
    Toggle(bool),
    Choice(usize),
    Breakpoints(Vec<(f64, f64)>),
}

/// The shape of one parameter: its range/default for a slider, or its set of named options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParamKind {
    Number {
        min: f64,
        max: f64,
        step: f64,
        default: f64,
        exponential: bool,
        scale: NumberScale,
    },
    Toggle {
        default: bool,
    },
    Choice {
        options: Vec<String>,
        default: usize,
    },
}

impl ParamKind {
    /// Test-only: what `cdp::runner`'s catalog smoke test drives every process with, since
    /// it's the one value guaranteed to already be inside the param's own declared range.
    /// The UI's own "value a fresh dialog opens with" path is `CdpField::from_default`
    /// (`ui/app.rs`), which builds a `CdpField` directly rather than going through this.
    #[cfg(test)]
    pub fn default_value(&self) -> ParamValue {
        match self {
            ParamKind::Number { default, .. } => ParamValue::Number(*default),
            ParamKind::Toggle { default } => ParamValue::Toggle(*default),
            ParamKind::Choice { default, .. } => ParamValue::Choice(*default),
        }
    }
}

/// One parameter of a CDP process: what to call it, how to edit it, and how it's placed on
/// the command line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamDef {
    pub name: String,
    pub description: String,
    /// `Some("-x")` means the value is emitted as a single argv token `-x<value>`; `None`
    /// means it's a bare positional argument (or, for a `Toggle`, a bare flag with no
    /// prefix — CDP flags themselves always start with `-`, so this is rare for toggles).
    pub flag: Option<String>,
    /// Whether CDP supports driving this parameter with a breakpoint (`.brk`) envelope file
    /// instead of a constant — a V2 UI capability; `pipeline.rs` supports it today.
    pub automatable: bool,
    #[serde(flatten)]
    pub kind: ParamKind,
}

/// One CDP process: which binary to invoke, what its parameters are, and how it fits into
/// the wav/ana pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDef {
    /// Stable identifier, e.g. `"blur_avrg"`, `"modify_speed_2"` — matches (a lightly
    /// cleaned form of) the SoundThread key it was ported from, so re-running the converter
    /// doesn't churn IDs. User catalog files override a built-in definition by reusing its
    /// key.
    pub key: String,
    /// The CDP binary name, e.g. `"blur"`, `"modify"`, `"rmverb"` — must exist in the
    /// configured CDP directory.
    pub bin: String,
    /// The first positional argument after the binary, e.g. `"avrg"`, `"speed"` — `None`
    /// for single-purpose binaries invoked as `bin infile outfile params...` (e.g.
    /// `rmverb`).
    pub subprog: Option<String>,
    /// The mode number, e.g. `"2"` in `modify speed 2 ...` — a separate positional argument
    /// after `subprog`, `None` when the process takes no mode number.
    pub mode: Option<String>,
    pub title: String,
    pub category: Category,
    pub subcategory: String,
    pub short_description: String,
    pub description: String,
    pub input: IoKind,
    pub output: IoKind,
    /// Whether this process handles a stereo `Wav` input natively. When `false` and the
    /// input is stereo, `pipeline.rs` splits it into two mono lanes and runs the process
    /// once per channel.
    pub stereo_native: bool,
    pub output_is_stereo: bool,
    /// Ordered — this order is exactly the order these values appear as positional
    /// arguments on the CDP command line (flagged params are still emitted in this order,
    /// just as `-x<value>` tokens instead of bare ones). A process with no parameters emits
    /// no `params` field at all in TOML (there's no syntax for an empty array-of-tables),
    /// hence `#[serde(default)]`.
    #[serde(default)]
    pub params: Vec<ParamDef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_number() -> ParamDef {
        ParamDef {
            name: "Cycle Count".into(),
            description: "Number of cycles over which to average".into(),
            flag: None,
            automatable: true,
            kind: ParamKind::Number {
                min: 2.0,
                max: 64.0,
                step: 1.0,
                default: 5.0,
                exponential: false,
                scale: NumberScale::Plain,
            },
        }
    }

    #[test]
    fn process_def_round_trips_through_toml() {
        let def = ProcessDef {
            key: "blur_avrg".into(),
            bin: "blur".into(),
            subprog: Some("avrg".into()),
            mode: None,
            title: "Average".into(),
            category: Category::Time,
            subcategory: "distort".into(),
            short_description: "Average the waveshape".into(),
            description: "Full description.".into(),
            input: IoKind::Wav,
            output: IoKind::Wav,
            stereo_native: false,
            output_is_stereo: false,
            params: vec![sample_number()],
        };

        let text = toml::to_string(&def).expect("serialize");
        let back: ProcessDef = toml::from_str(&text).expect("deserialize");
        assert_eq!(def, back);
    }

    #[test]
    fn toggle_and_choice_params_round_trip() {
        let toggle = ParamDef {
            name: "Omit Inharmonic Partials".into(),
            description: "Removes inharmonic partials from the sound".into(),
            flag: Some("-x".into()),
            automatable: false,
            kind: ParamKind::Toggle { default: false },
        };
        let choice = ParamDef {
            name: "Sample Rate".into(),
            description: "Output sample rate".into(),
            flag: None,
            automatable: false,
            kind: ParamKind::Choice {
                options: vec!["44100".into(), "48000".into()],
                default: 0,
            },
        };
        let def = ProcessDef {
            key: "synth_wave_1".into(),
            bin: "synth".into(),
            subprog: Some("wave".into()),
            mode: Some("1".into()),
            title: "Wave".into(),
            category: Category::Time,
            subcategory: "synthesis".into(),
            short_description: "Generate a waveform".into(),
            description: "Full description.".into(),
            input: IoKind::None,
            output: IoKind::Wav,
            stereo_native: false,
            output_is_stereo: false,
            params: vec![toggle, choice],
        };

        let text = toml::to_string(&def).expect("serialize");
        let back: ProcessDef = toml::from_str(&text).expect("deserialize");
        assert_eq!(def, back);
    }
}
