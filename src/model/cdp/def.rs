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
///
/// `Curve` is unlike every other variant: it carries no audio at all. Both sides of a
/// `repitch` pitch-curve-to-pitch-curve transform (`invert`, `smooth`, `quantise`, ...,
/// CDP-Ext-Plan.md Phase 4 "hard tier") always declare `input = "curve"` and
/// `output = "curve"` together — the real "infile" is CDP's own binary pitch-WAV format,
/// spliced from a `model::curve::PitchCurve`'s `binary_template` and current points, never
/// an open audio `Document`; the result replaces that curve's points and template rather
/// than being spliced into any buffer (see `pipeline::plan_curve_transform_job`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoKind {
    None,
    Wav,
    Ana,
    DualWav,
    DualAna,
    WavGlob,
    Curve,
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
    /// A frequency value (Hz) whose valid range is `[sample_rate / pvoc.points,
    /// sample_rate / 4]` — the width of one analysis channel up to half the Nyquist
    /// frequency. Found via a user manually testing `strange glis`'s "Spacing" (`hzstep`)
    /// at its catalog default (50 Hz): CDP rejected it with "Value (50.0) out of range
    /// (93.75 to 24000.0)" against a 96kHz file at the default 1024-point analysis —
    /// 93.75 = 96000/1024 (one channel's width) and 24000 = 96000/4 (nyquist/2), confirming
    /// the binary's own usage text verbatim ("Range: FROM channel-frq-width TO nyquist/2")
    /// rather than the fixed 50-200 range SoundThread's own catalog data declared. Depends
    /// on the real input's sample rate, not just its duration, so this needed a new scale
    /// rather than reusing `CappedAtInputDuration`.
    HzCappedToAnalysisRange,
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
    /// A multi-column datafile: one row per line, each row a fixed number of
    /// space-separated values matching `ParamKind::Table`'s `columns` — e.g. tapdelay's
    /// `time amp [pan]` taps, or repeater's `start end repeat-count delay` segments. Each
    /// inner `Vec<f64>`'s length always equals the param's column count.
    Table(Vec<Vec<f64>>),
    /// A time list where each entry additionally carries a single-character marker
    /// concatenated directly onto the time with no separator (e.g. `"a0.3"`, never `"a
    /// 0.3"`) — `focus freeze`'s bespoke datafile shape (CDP-Ext-Plan.md Tier 1b), confirmed
    /// against the real binary: a space between marker and time is rejected as an "unknown
    /// time flag." Genuinely a different shape from `Table` (which always writes
    /// whitespace-separated columns), so it gets its own variant rather than a special case
    /// bolted onto that one.
    MarkerTimeList(Vec<(char, f64)>),
    /// `hilite band`'s bitflag-conditional per-row shape (CDP-Ext-Plan.md Tier 1b) — see
    /// `HiliteBandRow`'s own doc comment for the row shape.
    HiliteBand(Vec<HiliteBandRow>),
}

/// One row of `hilite band`'s per-band data: a frequency band (`lofrq`/`hifrq`) plus up to
/// three independently-gated adjustments. `amp_bit`/`ramp_bit`/`transpose_bit`/`add_bit`
/// mirror the datafile's 4-bit flag exactly — confirmed against the real binary: `add_bit`
/// is only ever meaningful (and only ever written) when `transpose_bit` is also set
/// ("Cannot add_in partials without first transposing"), and `ramp_bit` needs no
/// `amp_bit` (a `ramp_bit`-alone row ramps from the band's own original level to `amp2`).
/// `amp1`/`amp2`/`transpose_value`/`transpose_additive` are always present in memory —
/// never lost when their governing bit toggles off in the editor — but only the ones whose
/// bit is currently set are ever written to the datafile (`model::cdp::pipeline`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HiliteBandRow {
    pub lofrq: f64,
    pub hifrq: f64,
    pub amp_bit: bool,
    pub ramp_bit: bool,
    pub transpose_bit: bool,
    pub add_bit: bool,
    pub amp1: f64,
    pub amp2: f64,
    pub transpose_value: f64,
    /// The `+` prefix on the datafile's transpose value — additive Hz instead of a
    /// multiplier. Only meaningful (and only ever written) when `transpose_bit` is set.
    pub transpose_additive: bool,
}

/// One column of a `ParamKind::Table` param — the per-column counterpart to `Number`'s own
/// min/max/step/default/scale, since a table has no single set of bounds covering every
/// column (e.g. tapdelay's `time`/`amp`/`pan` columns each have their own real-world range).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableColumn {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub default: f64,
    pub scale: NumberScale,
    /// True for a column CDP requires to be a whole number (e.g. repeater's Repeat Count —
    /// confirmed by hand: "Non-integer repeat value" — or `blur weave`'s step list —
    /// "Invalid character in weave file" on a decimal). Distinct from range clamping (which
    /// every column already gets): a value like `2.5` can sit well inside `min`/`max` and
    /// still be rejected by CDP for not being an integer at all, so this is checked and
    /// rounded separately at commit time in the UI. `#[serde(default)]` so existing catalog
    /// entries (where no column needs this) don't need updating.
    #[serde(default)]
    pub integer: bool,
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
        /// True for a param CDP requires to be a whole number — see `TableColumn.integer`'s
        /// doc comment for the full rationale; this is the same flag, just for a plain
        /// `Number` param (and, via `ParamDef.required_list`'s reuse of this `ParamKind`,
        /// for a `required_list` field's entries too). `#[serde(default)]` so existing
        /// catalog entries don't need updating.
        #[serde(default)]
        integer: bool,
    },
    Toggle {
        default: bool,
    },
    Choice {
        options: Vec<String>,
        default: usize,
    },
    /// A multi-column datafile param (CDP-Ext-Plan.md Tier 1b's "bespoke multi-column"
    /// shape, e.g. tapdelay's `time amp [pan]` taps or repeater's `start end repeat-count
    /// delay` segments) — always required (there's no bare-constant alternative, the same
    /// way `ParamDef.required_list`/`required_envelope` fields work, but expressed as its
    /// own `ParamKind` rather than a flag bolted onto `Number` since no single set of
    /// min/max/step/default covers more than one column). `time_column` is the index of
    /// the column that must stay strictly ascending across rows (mirrors
    /// `ParamDef.list_is_time_sequence`, e.g. tapdelay's `time` column), or `None` when row
    /// order is unconstrained (e.g. repeater's segments, which may overlap or run backward
    /// in the source).
    Table {
        columns: Vec<TableColumn>,
        /// `#[serde(default)]`: most `Table` params (e.g. repeater's) have no ordering
        /// constraint at all, so a catalog entry can omit this key entirely rather than
        /// writing `time_column = false` — TOML has no `null`, so without this attribute a
        /// missing key would be a hard deserialize error, not an implicit `None`.
        #[serde(default)]
        time_column: Option<usize>,
    },
    /// `focus freeze`'s bespoke marker-prefixed time list (CDP-Ext-Plan.md Tier 1b) — always
    /// required, same rationale as `Table`. `markers` is the catalog-declared set of valid
    /// marker characters (`['a', 'b']` for every process that uses this today, but not
    /// hardcoded in case a future one uses a different alphabet); entries must stay strictly
    /// ascending by time across rows (confirmed against the real binary: "Time values out of
    /// sequence"), so unlike `Table` there's no `time_column`/`None` choice to make — time
    /// ordering always applies.
    MarkerTimeList {
        markers: Vec<char>,
        min: f64,
        max: f64,
        step: f64,
        default: f64,
        scale: NumberScale,
    },
    /// `hilite band`'s bitflag-conditional per-row shape (CDP-Ext-Plan.md Tier 1b) — see
    /// `HiliteBandRow`'s doc comment for the row semantics. Each field reuses `TableColumn`
    /// for its bounds rather than introducing yet another bounds struct, even though there's
    /// only ever one column of each here — the shape (name/min/max/step/default/scale) is
    /// identical to what a table column already needs.
    HiliteBand {
        lofrq: TableColumn,
        hifrq: TableColumn,
        amp1: TableColumn,
        amp2: TableColumn,
        transpose: TableColumn,
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
            // One row, each column at its own default — mirrors how the UI seeds a
            // never-yet-configured table field (`App::open_cdp_table_editor`).
            ParamKind::Table { columns, .. } => {
                ParamValue::Table(vec![columns.iter().map(|c| c.default).collect()])
            }
            // One entry, at the param's own default time and its first declared marker —
            // mirrors how the UI seeds a never-yet-configured field
            // (`App::open_cdp_marker_time_list_editor`).
            ParamKind::MarkerTimeList { markers, default, .. } => {
                ParamValue::MarkerTimeList(vec![(*markers.first().unwrap_or(&'a'), *default)])
            }
            // One row with `amp_bit` set (the simplest always-valid starting state — CDP
            // itself rejects an all-bits-off row as a "Zero bitflag"), at each numeric
            // field's own catalog default.
            ParamKind::HiliteBand { lofrq, hifrq, amp1, amp2, transpose } => {
                ParamValue::HiliteBand(vec![HiliteBandRow {
                    lofrq: lofrq.default,
                    hifrq: hifrq.default,
                    amp_bit: true,
                    ramp_bit: false,
                    transpose_bit: false,
                    add_bit: false,
                    amp1: amp1.default,
                    amp2: amp2.default,
                    transpose_value: transpose.default,
                    transpose_additive: false,
                }])
            }
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
    /// True for a param whose real CDP argv position is *before* `outfile` rather than
    /// after it (e.g. `pitch altharms infile pitchfile outfile`, `formants put mode infile
    /// fmntfile outfile` — the required datafile sits between the input and output
    /// filenames). Every `required_envelope`/`required_list` param this catalog shipped
    /// before this field existed had its datafile positioned *after* `outfile` in the real
    /// argv, which is why `pipeline::build_process_args` always assumed that shape — this
    /// is the first time that assumption needed an escape hatch. At most one param on a
    /// given process is expected to need this (CDP's own datafile-before-outfile processes
    /// only ever have one such datafile), but `build_process_args` handles any number by
    /// emitting every `true`-flagged param (in declared order) before `outfile`, then every
    /// other param (in declared order) after it — the same relative ordering either group
    /// would already get on its own.
    #[serde(default)]
    pub before_outfile: bool,
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
            before_outfile: false,
            kind: ParamKind::Number {
                min: 2.0,
                max: 64.0,
                step: 1.0,
                default: 5.0,
                exponential: false,
                scale: NumberScale::Plain,
                integer: false,
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
            before_outfile: false,
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
            before_outfile: false,
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

    /// `ParamKind::Table`'s nested `columns` (an array of `TableColumn` structs) must
    /// survive a TOML round-trip cleanly — validated in isolation before any UI/pipeline
    /// code is built on top of it, since a doubly-nested array-of-tables under
    /// `#[serde(flatten)]`'s tag is exactly the kind of shape that can surprise a TOML
    /// serializer.
    #[test]
    fn table_param_with_multiple_columns_round_trips_through_toml() {
        let table = ParamDef {
            name: "Taps".into(),
            description: "Delay taps".into(),
            flag: None,
            automatable: false,
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
            before_outfile: false,
            kind: ParamKind::Table {
                columns: vec![
                    TableColumn {
                        name: "Time".into(),
                        min: 0.0,
                        max: 60.0,
                        step: 0.01,
                        default: 0.1,
                        scale: NumberScale::Plain,
                        integer: false,
                    },
                    TableColumn {
                        name: "Amp".into(),
                        min: 0.0,
                        max: 1.0,
                        step: 0.01,
                        default: 0.5,
                        scale: NumberScale::Plain,
                        integer: false,
                    },
                    TableColumn {
                        name: "Pan".into(),
                        min: -1.0,
                        max: 1.0,
                        step: 0.01,
                        default: 0.0,
                        scale: NumberScale::Plain,
                        integer: false,
                    },
                ],
                time_column: Some(0),
            },
        };
        let def = ProcessDef {
            key: "tapdelay_tapdelay".into(),
            bin: "tapdelay".into(),
            subprog: None,
            mode: None,
            title: "Tap Delay".into(),
            category: Category::Time,
            subcategory: "delay".into(),
            short_description: "Multi-tap delay".into(),
            description: "Full description.".into(),
            input: IoKind::Wav,
            output: IoKind::Wav,
            stereo_native: false,
            output_is_stereo: true,
            requires_simple_wav_input: false,
            params: vec![table],
        };

        let text = toml::to_string(&def).expect("serialize");
        let back: ProcessDef = toml::from_str(&text).expect("deserialize");
        assert_eq!(def, back);
    }

    /// `ParamKind::MarkerTimeList`'s `markers: Vec<char>` must survive a TOML round-trip —
    /// validated in isolation like the `Table` schema above, since `char` isn't a native
    /// TOML type (it round-trips as a one-character string) and is worth confirming before
    /// any UI/pipeline code depends on it.
    #[test]
    fn marker_time_list_param_round_trips_through_toml() {
        let param = ParamDef {
            name: "Freeze Times".into(),
            description: "Times at which the spectrum is frozen".into(),
            flag: None,
            automatable: false,
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
            before_outfile: false,
            kind: ParamKind::MarkerTimeList {
                markers: vec!['a', 'b'],
                min: 0.0,
                max: 7200.0,
                step: 0.01,
                default: 0.1,
                scale: NumberScale::CappedAtInputDuration,
            },
        };
        let def = ProcessDef {
            key: "focus_freeze_1".into(),
            bin: "focus".into(),
            subprog: Some("freeze".into()),
            mode: Some("1".into()),
            title: "Freeze (Amplitude)".into(),
            category: Category::Pvoc,
            subcategory: "spectrum".into(),
            short_description: "Freeze spectral amplitudes".into(),
            description: "Full description.".into(),
            input: IoKind::Ana,
            output: IoKind::Ana,
            stereo_native: false,
            output_is_stereo: false,
            requires_simple_wav_input: false,
            params: vec![param],
        };

        let text = toml::to_string(&def).expect("serialize");
        let back: ProcessDef = toml::from_str(&text).expect("deserialize");
        assert_eq!(def, back);
    }

    /// `ParamKind::HiliteBand`'s five `TableColumn`-shaped fields must survive a TOML
    /// round-trip — validated in isolation before any UI/pipeline code depends on it, same
    /// discipline as the `Table`/`MarkerTimeList` schemas above.
    #[test]
    fn hilite_band_param_round_trips_through_toml() {
        let bounds = |name: &str, min, max, default| TableColumn {
            name: name.into(),
            min,
            max,
            step: 0.1,
            default,
            scale: NumberScale::Plain,
            integer: false,
        };
        let param = ParamDef {
            name: "Bands".into(),
            description: "Frequency bands to process independently".into(),
            flag: None,
            automatable: false,
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
            before_outfile: false,
            kind: ParamKind::HiliteBand {
                lofrq: bounds("Lo Freq", 20.0, 20000.0, 200.0),
                hifrq: bounds("Hi Freq", 20.0, 20000.0, 2000.0),
                amp1: bounds("Amp 1", 0.0, 10.0, 1.0),
                amp2: bounds("Amp 2", 0.0, 10.0, 1.0),
                transpose: bounds("Transpose", -10000.0, 10000.0, 1.0),
            },
        };
        let def = ProcessDef {
            key: "hilite_band".into(),
            bin: "hilite".into(),
            subprog: Some("band".into()),
            mode: None,
            title: "Band".into(),
            category: Category::Pvoc,
            subcategory: "spectrum".into(),
            short_description: "Split spectrum into bands".into(),
            description: "Full description.".into(),
            input: IoKind::Ana,
            output: IoKind::Ana,
            stereo_native: false,
            output_is_stereo: false,
            requires_simple_wav_input: false,
            params: vec![param],
        };

        let text = toml::to_string(&def).expect("serialize");
        let back: ProcessDef = toml::from_str(&text).expect("deserialize");
        assert_eq!(def, back);
    }
}
