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
/// `pipeline::PlanError::UnsupportedInV1`). `WavGlob` is output-only (never valid as
/// `input`): a process that produces an unknown number of numbered mono output files
/// sharing a prefix (e.g. `distcut`'s `cutout0.wav`, `cutout1.wav`, …) instead of one
/// result — each file becomes its own new buffer instead of being spliced into the
/// selection (see `pipeline::plan_wav_glob`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoKind {
    None,
    Wav,
    Ana,
    DualWav,
    DualAna,
    WavGlob,
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
    /// The raw literal value (already in the param's real unit — seconds, unlike
    /// `PercentOfInputDuration`'s 0-100 slider), clamped down to just under the real
    /// selection's duration if it would otherwise exceed it — the catalog's own `min`/`max`
    /// stay literal (a genuine fixed floor CDP enforces independent of duration, and a
    /// generous outer safety cap respectively), only the *effective ceiling* tightens per
    /// selection. Found via a user manually testing `grain reposition`'s "Max Inter-Grain
    /// Time" (`-b`): CDP rejects any value greater than the actual input's duration with
    /// "Value (...) out of range (0.1 to <duration>)" — a genuinely data-dependent
    /// constraint (confirmed against the real binary across several file lengths: the
    /// upper bound tracked the file's own duration exactly every time, unrelated to the
    /// catalog's static `max`), not a fixed range this catalog can declare once and reuse
    /// unchanged across every selection the way `Plain` params can.
    CappedAtInputDuration,
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
    /// A plain ordered list of numbers, one per line in the datafile CDP reads — no time
    /// axis, unlike `Breakpoints` (see `ParamDef::required_list`'s doc comment for the
    /// real processes this covers: a list of grain-onset *times*, or a list of per-grain
    /// transposition/multiplier *values* — mechanically the same file shape either way,
    /// differing only in what the numbers mean, which lives in the param's own
    /// name/description rather than the type).
    List(Vec<f64>),
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
    /// True for a parameter whose CDP argument syntax is *always* a breakpoint textfile —
    /// never a bare constant (e.g. `iterline`'s TDATA, `fractal wave`'s SHAPE — CDP-Ext-Plan.md
    /// Phase 3/"Tier 1b"). Distinct from `automatable`, which additionally allows a plain
    /// constant as one valid alternative: every `required_envelope` param must also set
    /// `automatable = true` (so the existing 'e'-key/envelope-editor machinery applies
    /// unchanged — `pipeline.rs` needs no changes at all, since it already turns any
    /// `ParamValue::Breakpoints` into a `.brk`-shaped datafile for any `Number`-kind param),
    /// but the UI never offers a way *back* to a constant: `CdpField` starts such a field
    /// with no envelope yet (`App::open_cdp_envelope_editor`'s existing "no envelope yet"
    /// fallback already builds a sensible real-duration-scaled starting shape), validation
    /// blocks Apply/Preview until the user has actually opened the editor and set one
    /// (`App::cdp_validate_fields`), and the envelope editor's 'c' ("commit as constant")
    /// key is a no-op for it (`App::handle_cdp_envelope_key`).
    #[serde(default)]
    pub required_envelope: bool,
    /// True for a parameter whose CDP argument syntax is *always* a plain ordered-list
    /// datafile (one number per line, no time axis) — never a bare constant. Covers two
    /// real shapes that happen to share one file format: a list of *times* (e.g. `grain
    /// reposition`'s TIMEFILE, `stutter`'s DATAFILE) and a list of per-element *values*
    /// (e.g. `grain repitch`'s TRANSPFILE, `grain rerhythm`'s MULTFILE) — see
    /// CDP-Ext-Plan.md Phase 3's "plain time-list"/"plain value-list" shapes. Mutually
    /// exclusive with `required_envelope` on the same param (one param is either a
    /// breakpoint-pairs field or a plain-list field, never both) — mirrors that flag's
    /// shape exactly: every `required_list` param must also set `automatable = true`
    /// (reusing the existing 'e'-key gate, this time to open the list editor instead of
    /// the envelope editor — `App::open_cdp_list_editor`), starts with no list yet
    /// (`CdpField::List`'s `values` empty), and blocks Apply/Preview until the user has
    /// set at least one entry (`App::cdp_validate_fields`).
    #[serde(default)]
    pub required_list: bool,
    /// Only meaningful when `required_list` is also true: whether the list's entries are
    /// audio-position *times* that CDP requires to stay strictly ascending (e.g. `grain
    /// reposition`'s TIMEFILE, `stutter`'s DATAFILE — confirmed against the real binary,
    /// which rejects an out-of-order list with "Sync times out of sequence") as opposed to
    /// per-element *values* with no ordering constraint (e.g. `grain repitch`'s
    /// TRANSPFILE — transpositions applied to successive grains in whatever order the user
    /// wants). When true, `App::handle_cdp_list_key`'s Up/Down nudge clamps a time entry
    /// between its immediate neighbors (mirroring the envelope editor's neighbor-clamped
    /// time-move) instead of the field's full `min`/`max`, 'n' inserts a new entry at the
    /// midpoint between the selected entry and its neighbor (instead of a flat duplicate,
    /// which would create two equal — also rejected — times), and the practical nudge
    /// range/step is bound by the actual selection's duration rather than the catalog's
    /// own (necessarily generous, e.g. "up to 2 hours") `max` — the catalog `max` stays a
    /// hard safety cap, but the *usable* range for a specific selection is almost always
    /// far smaller than that cap, and a coarse nudge step sized off the cap alone (as a
    /// non-time value-list's is) produces jumps of hundreds of seconds that are useless for
    /// picking a real position in a short file.
    #[serde(default)]
    pub list_is_time_sequence: bool,
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
    /// True for a process whose binary can't correctly read the `WAVE_FORMAT_EXTENSIBLE`
    /// WAV header `hound` (this project's WAV library) writes for any file with
    /// `bits_per_sample > 16` — which is every input file this app ever sends CDP, since
    /// the runner's normal working format is 32-bit float. Found by hand (`rmverb`,
    /// SoundThread-derived, already shipped): the *symptom* wasn't a clean error — the
    /// binary silently misread the float samples' raw bytes as if they were 32-bit
    /// integers, producing wildly wrong ("distorted") audio with no error at all, discovered
    /// by dumping and comparing raw sample values between our pipeline's output and a
    /// direct CDP CLI run on a plain 16-bit input (which produced a clean, correct result).
    /// `reverb` (a sibling, never-shipped process — see `catalog_extra.toml`'s removal
    /// note) hit the same root cause but failed loudly instead ("cannot open output file"),
    /// which is how the incompatibility was first found. Most of the catalog's ~200 other
    /// processes tolerate the extensible header fine (confirmed via the smoke test, though
    /// that only checks exit code — it can't catch *silent* corruption the way this one
    /// slipped through), so this is a per-process opt-in rather than a global format
    /// change: `App`/`cdp::runner`'s `write_inputs` writes this process's input as plain
    /// 16-bit integer PCM instead (channels ≤ 2 and bits ≤ 16 are exactly the condition
    /// under which `hound` uses the simple, non-extensible `fmt ` chunk), trading a small,
    /// CDP-processing-scale amount of precision for correctness on the processes that need
    /// it, without touching the float32 precision every other process still gets.
    #[serde(default)]
    pub requires_simple_wav_input: bool,
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
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
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
            requires_simple_wav_input: false,
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
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
            kind: ParamKind::Toggle { default: false },
        };
        let choice = ParamDef {
            name: "Sample Rate".into(),
            description: "Output sample rate".into(),
            flag: None,
            automatable: false,
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
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
            requires_simple_wav_input: false,
            params: vec![toggle, choice],
        };

        let text = toml::to_string(&def).expect("serialize");
        let back: ProcessDef = toml::from_str(&text).expect("deserialize");
        assert_eq!(def, back);
    }
}
