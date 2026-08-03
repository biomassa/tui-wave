//! Data model for a CDP (Composer's Desktop Project) process definition: the catalog entry
//! that describes how to invoke one CDP command-line program and what parameters it takes.
//! Pure data — no process spawning, no UI — so the catalog can be parsed and unit-tested in
//! isolation. See `catalog.rs` for loading, `pipeline.rs` for turning a `ProcessDef` plus
//! concrete `ParamValue`s into actual command invocations.

use serde::{Deserialize, Serialize};

/// Broad process family, mirrors CDP's own split between time-domain and spectral
/// (phase-vocoder) processing. `pipeline.rs` uses this to decide whether a process needs
/// wrapping in `pvoc anal`/`pvoc synth`.
///
/// `Praat` is not a CDP family at all — it marks an entry that runs through Praat
/// (`model::praat`) instead of a CDP binary. It lives on this enum rather than in a separate
/// field because the browser's Domain column is literally `CdpDomainRow::Domain(Category)`, so
/// a new variant *is* the new domain row; `ProcessDef::backend` then derives the execution
/// backend from it, keeping one field that cannot disagree with itself (see `backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Time,
    Pvoc,
    Praat,
}

/// Which external program actually runs a `ProcessDef` — derived from `Category`, never
/// stored (see `ProcessDef::backend`).
///
/// The two backends could not be less alike in how they are invoked (CDP: one binary per
/// process, arguments straight on argv; Praat: one binary for everything, driven by a
/// generated script), but they are alike in every way the *UI* cares about, which is why a
/// Praat process can be an ordinary `ProcessDef`. Only planning and running branch on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cdp,
    Praat,
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
///
/// `VariadicWav`/`GroupedWav` are the open-ended input arities (CDP-Release8-TODO.md's
/// "variadic input" batch): CDP's `infile1 [infile2 infile3 ...] outfile ...` shape, which
/// `Dual*`'s fixed 2 can't express. Both always resolve to input 0 = the active selection
/// plus N additional whole buffers the user picked (`ui::app`'s `CdpVariadicInput`), so a
/// process is never picker-only — exactly the role `Dual*`'s second input already plays,
/// generalized past two. They differ only in what the ordered list *means*, which is a UI
/// and validation concern rather than a planning one (`pipeline::plan_variadic_wav` handles
/// both identically — it just emits every input in list order):
///
/// - `VariadicWav`: a flat list, minimum 1 file. Order is significant to the process
///   (`crystal rotate`'s Nth file drives the Nth crystal vertex) but carries no grouping.
/// - `GroupedWav`: the list is two equal-length channel-role groups concatenated — every
///   channel-1 source in order, then every channel-2 source in order (`repair repair`'s
///   documented ordering, confirmed against the real binary: it reads
///   `infiles[n]`/`infiles[n + count/2]` as one output file's two channels). Minimum 2
///   files, always an even count. Only ever stereo here: CDP supports 4/5/7/8/16-channel
///   groupings too, deliberately not exposed (this app's audio path is stereo-only — the
///   same exclusion `mchanpan`/`abfpan`/`panorama`/… already got).
///
/// Every process with either input kind takes **mono** input files only (confirmed against
/// all four real binaries: "File x.wav is not of correct type (must be mono)"), so
/// `plan_variadic_wav` always writes one mono temp file per input rather than splitting a
/// stereo document into per-channel lanes the way `plan_wav`/`plan_dual_wav` do.
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
    VariadicWav,
    GroupedWav,
}

/// The channel count a process's binary demands of its input file, and which of the
/// document's channels `pipeline::plan_wav` therefore writes into the temp WAV. See
/// [`ProcessDef::input_channels`] for why `stereo_native: bool` couldn't express this and for
/// the real binaries' own refusals.
///
/// `Mono` deliberately takes **channel 0 alone** from a wider document rather than running a
/// lane per channel. A spatialiser turns one source into a room; running it 30 times over a
/// 30-channel take would produce 30 unrelated 8-channel renderings, not one. Same call, and
/// the same wording, as `plan_curve`'s "only ever takes the first channel of a multi-channel
/// selection" — and each such process says so in its own catalog description, since the
/// choice is invisible in the dialog otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputChannels {
    /// Exactly one channel — channel 0 of the selection, whatever its width.
    Mono,
    /// Exactly two — channels 0 and 1. Unsatisfiable by a mono document, which is what
    /// `App::cdp_params_blocker` says before Apply can be pressed.
    Stereo,
    /// Every channel of the selection, and at least three of them (`pairex`'s "must have more
    /// than two channels", `mchshred` mode 2's "correct number of channels for this mode").
    Multichannel,
}

/// Where a process's output channel count comes from, when it is neither the input's count
/// nor a flat 2. See [`ProcessDef::output_channels`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputChannels {
    /// Read from the param at this index — the OUTCHANS the user picked. A `Number` param
    /// reads as itself; a `Choice` param reads its selected option's text as a number, so a
    /// count locked to one value can stay the locked-`Choice` shape the catalog already uses
    /// for `repair`/`tesselate`'s channel counts.
    FromParam { param: usize },
    /// A count the binary always writes regardless of parameters — `crumble sound 1` is
    /// always 8 channels and mode 2 always 16, `crystal rotate` modes 3-9 always 8.
    Fixed { count: usize },
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
    /// A frequency value (Hz) whose valid range runs up to the input's own Nyquist frequency
    /// (`sample_rate / 2`). Confirmed against `distort replim`'s `-f` HILIM (2026-07-28): the
    /// binary reports "Value (439.0) out of range (440.000000 to 22050.000000)" on a 44.1kHz
    /// file and "(440.000000 to 11025.000000)" on a 22.05kHz one — the floor is a fixed 440Hz
    /// (the catalog's own `min` covers that), only the ceiling tracks the file.
    ///
    /// Distinct from `HzCappedToAnalysisRange`, which caps at `sample_rate / 4` and additionally
    /// floors at one analysis channel's width: that scale describes a *spectral* param bounded
    /// by the pvoc window, this one a time-domain param bounded only by the sample rate, so
    /// they can't share an arm. The catalog's static `max` still applies as an outer safety cap
    /// — this only ever clamps *down*, never raises a value past what the entry declares.
    HzCappedToNyquist,
    /// A position expressed as a **sample count** rather than seconds, clamped down to the
    /// real input's length. `distort pulsed` mode 3's STIME is the only such param so far —
    /// its own usage text calls this out explicitly ("In mode 3, time as samplecnt") while
    /// modes 1 and 2 take the same argument in seconds, which is why it can't just reuse
    /// `CappedAtInputDuration`: the two differ in unit, not only in ceiling. Confirmed
    /// against the real binary (2026-07-28): 132299 accepted and 200000 rejected on a
    /// 3-second 44.1kHz file, i.e. the ceiling is the file's own sample count.
    ///
    /// Clamps to `len_samples - 1` for the same reason `CappedAtInputDuration` subtracts a
    /// small margin: a start position exactly at the end leaves no audio to work with.
    CappedAtInputSamples,
    /// A time step (seconds) bounded at *both* ends by data: at least two analysis frames
    /// long, and no longer than the input itself.
    ///
    /// `focus step`'s own usage text states the floor ("Must be >= duration of 2 analysis
    /// frames") without saying what a frame is worth; measuring the real binary across
    /// sample rates, window sizes and overlaps gives one frame = `points / 2^overlap`
    /// samples, i.e. the pvoc decimation factor:
    ///
    /// | points | overlap | rate  | reported floor | 2·points/2^overlap/rate |
    /// |--------|---------|-------|----------------|-------------------------|
    /// | 1024   | 3       | 44100 | 0.005805       | 0.005805                |
    /// | 2048   | 3       | 44100 | 0.011610       | 0.011610                |
    /// | 1024   | 1       | 48000 | 0.021333       | 0.021333                |
    /// | 1024   | 4       | 48000 | 0.002667       | 0.002667                |
    ///
    /// The ceiling is the input's own duration (measured slightly *above* it — a 4.0s file
    /// reports 4.025760 — so clamping to just under the duration is safely inside).
    ///
    /// Found by a user hitting "Parameter[1] Value (1.000000) out of range (0.002667 to
    /// 0.852000)" on a short selection: the catalog declared a flat `[0.01 – 1.0]`, so its own
    /// `max` was unusable on any selection under a second, and its `min` is below CDP's real
    /// floor at every sample rate under 25.6kHz. Neither bound is expressible as a fixed
    /// range, and unlike `CappedAtInputDuration` the *floor* moves too — with the analysis
    /// window, not the file — which is why this can't reuse that scale.
    AnaFrameStepSeconds,
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
    /// Carries no data itself — unlike every other variant, "which buffer" is a runtime UI
    /// selection (`CdpField::FormantBufferRef`'s picked index into `App.formant_buffers`),
    /// not a portable value that belongs in a saved job. `plan_param` always emits the
    /// paired `ParamKind::FormantBufferRef`'s `relative_name` literally; the actual bytes
    /// are injected into `PlannedJob.binary_input_files` by the app layer after `plan_job`
    /// returns, the same bypass `ParamKind::FormantBufferRef`'s own doc comment describes.
    FormantBufferRef,
    /// An absolute path to a real file on disk the user picked via a file browser (e.g.
    /// `matrix matrix 2`'s `inmatrixfile`, `ParamKind::FilePath`) — unlike
    /// `FormantBufferRef`, the file already exists wherever the user saved it and CDP can
    /// open it directly by that path (`cdp::runner` spawns every job with the temp job dir
    /// as CWD, but an absolute path works regardless of CWD), so there's no bytes-injection
    /// bypass needed: `plan_param` just emits this string as the argv token verbatim.
    FilePath(String),
    /// `crystal rotate`'s two-section VDAT datafile — see `ParamKind::CrystalVdat` and
    /// `CrystalVdat`'s own doc comments for the shape and why it isn't two separate params.
    CrystalVdat(CrystalVdat),
}

/// `crystal rotate`'s VDAT companion datafile, as edited in the UI and written by
/// `pipeline::plan_param`: a list of crystal vertices followed by one amplitude envelope
/// imposed on every sound event the crystal generates. One value rather than two params
/// because CDP reads it as **one** argv token naming **one** file whose two sections are
/// delimited only by line shape (a line of exactly 3 numbers is a vertex; the first line
/// with any other count starts the envelope), so the two halves can never be written or
/// validated independently — see `ParamKind::CrystalVdat`.
///
/// Every constraint below is enforced by `crystal.c`'s own `handle_the_special_data`, i.e.
/// it is a hard parse error from the real binary, not a preference (all reproduced against
/// the real binary while building this):
/// - `vertices` must be non-empty, each coordinate in `[-1, 1]` ("Crystal X-coord (%lf) out
///   of range"), and each vertex must additionally lie **inside the unit sphere**
///   (`sqrt(x²+y²+z²) <= 1`, "vertex N lies outside the unit sphere"). The usage text
///   presents that second rule as advice; `get_vectorlen` really does enforce it, so a
///   coordinate triple can be individually in range and still be rejected.
/// - `envelope` is `(time_secs, value)` pairs: at least 2 of them, times strictly
///   increasing and starting at exactly 0, values in `[0, 1]` with the **first and last
///   exactly 0** ("First envelope value (%lf) is not zero", "Last envelope value ...").
///   The final time is the duration of each generated sound event.
///
/// Vertex count also has to equal the input-file count whenever more than one file is
/// supplied (`if(dz->infilecnt > 1 && vertexcnt != dz->infilecnt)`) — checked in
/// `pipeline::plan_job` instead of `validate` here, since only the planner knows how many
/// input files a given run actually has.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CrystalVdat {
    /// Initial X/Y/Z coordinates, one per crystal vertex, in the same order as the input
    /// files (the Nth file drives the Nth vertex). X maps to event time/stereo position, Y
    /// to pitch, Z to brightness.
    pub vertices: Vec<[f64; 3]>,
    /// `(time_secs, value)` breakpoints, in the exact shape `ParamValue::Breakpoints`
    /// already uses — deliberately, so the existing graphical envelope editor
    /// (`ui::app`'s `CdpEnvelopeEdit`) can edit this section unchanged. Empty means "never
    /// configured": there is no catalog default that could know the real event duration, so
    /// the UI seeds it from the selection on first open and blocks Apply until then, exactly
    /// as a `ParamDef::required_envelope` field does.
    pub envelope: Vec<(f64, f64)>,
}

impl CrystalVdat {
    /// Inclusive bounds on one vertex coordinate. Also the per-axis bounds the vertex table
    /// editor clamps typed cells to — the unit-sphere rule is a separate, cross-axis check
    /// (`validate`) that no single-column clamp can express.
    pub const COORD_MIN: f64 = -1.0;
    pub const COORD_MAX: f64 = 1.0;
    /// Inclusive bounds on one envelope value. Fixed by CDP, not derived from any catalog
    /// param — which is exactly why the envelope editor needed a way to get its value-axis
    /// bounds from something other than a `ParamKind::Number` (`ui::app`'s
    /// `CdpEnvelopeTarget`).
    pub const ENVELOPE_MIN: f64 = 0.0;
    pub const ENVELOPE_MAX: f64 = 1.0;
    /// Nudge step for the envelope's value axis, standing in for the `step` the editor
    /// would otherwise read off a `ParamKind::Number` — 1/100th of the fixed 0-1 range.
    pub const ENVELOPE_STEP: f64 = 0.01;

    /// Every structural rule the real binary enforces while parsing this file, checked
    /// before a temp file is written or a process spawned. `Err` carries a message written
    /// to be shown verbatim to the user (`pipeline::PlanError::InvalidParamData`), so it
    /// names the offending row/point rather than restating the rule abstractly.
    ///
    /// Lives here rather than in the UI's own `cdp_validate_fields` because it is a property
    /// of the *value*, not of any dialog: the same check has to hold for a value loaded from
    /// a saved preset, and it needs to be unit-testable without a terminal.
    pub fn validate(&self) -> Result<(), String> {
        if self.vertices.is_empty() {
            return Err("needs at least one crystal vertex".into());
        }
        for (i, v) in self.vertices.iter().enumerate() {
            if v.iter().any(|c| *c < Self::COORD_MIN || *c > Self::COORD_MAX) {
                return Err(format!("vertex {} has a coordinate outside -1 to 1", i + 1));
            }
            // `sqrt(x²+y²+z²) > 1`, written without the square root so the comparison is
            // exact for the boundary case (a vertex exactly on the unit sphere is accepted
            // by CDP: it rejects only `> 1.0`).
            let len_sq = v.iter().map(|c| c * c).sum::<f64>();
            if len_sq > 1.0 {
                return Err(format!(
                    "vertex {} lies outside the unit sphere (x²+y²+z² = {:.3}, must be ≤ 1)",
                    i + 1,
                    len_sq
                ));
            }
        }
        if self.envelope.len() < 2 {
            return Err("the event envelope needs at least 2 breakpoints".into());
        }
        if self.envelope[0].0 != 0.0 {
            return Err("the event envelope's first breakpoint must be at time 0".into());
        }
        if self.envelope.windows(2).any(|w| w[1].0 <= w[0].0) {
            return Err("the event envelope's times must strictly increase".into());
        }
        if self.envelope.iter().any(|&(_, v)| v < Self::ENVELOPE_MIN || v > Self::ENVELOPE_MAX) {
            return Err("every event-envelope value must be between 0 and 1".into());
        }
        if self.envelope[0].1 != 0.0 || self.envelope[self.envelope.len() - 1].1 != 0.0 {
            return Err("the event envelope's first and last values must both be 0".into());
        }
        Ok(())
    }
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
    /// True for a column whose values CDP requires to be **all different from each other**
    /// (`tesselate`'s Entry Delay: "All vals must be different. (With same value, 2 sources
    /// would collapse into one double-src)"). Only meaningful on a table that
    /// `rows_match_input_count` auto-sizes — that is where rows appear without the user
    /// typing them, so seeding a new row with this column's plain `default` would produce a
    /// duplicate every time. New rows instead get a value staggered past the largest one
    /// already present (see `App::sync_cdp_table_to_input_count`).
    #[serde(default)]
    pub must_be_distinct: bool,
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
        /// Write the table **transposed**: one line per *column*, each holding every row's
        /// value for that column, instead of the usual one line per row. `tesselate`'s
        /// datafile is exactly this shape (confirmed against the real binary's own parser: it
        /// insists on exactly two lines — line 1 every source's resync count, line 2 every
        /// source's onset delay — and rejects any other line count outright), and it is the
        /// only process that needs it so far.
        ///
        /// A flag on `Table` rather than a whole new `ParamKind` because nothing else about
        /// the shape differs: the editor, the per-column bounds, and the row semantics ("one
        /// row per input file") are identical, and only the final `\n`/`" "` placement in
        /// `pipeline::plan_param` changes. `#[serde(default)]` so no existing table entry
        /// needs updating.
        #[serde(default)]
        transposed: bool,
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
    /// A required reference to a stored, opaque `model::formant::FormantBuffer` (CDP-Ext-Plan.md
    /// Phase 5) — `formants put`'s `fmntfile`, `oneform put`'s `1f-infile`. Unlike every
    /// other datafile-shaped param, there is no hand-drawable alternative at all (confirmed:
    /// `formants`/`oneform` offer no way to author this data except by extracting it from a
    /// real sound) — the UI always resolves this to "pick one of my open Formant/Snapshot
    /// buffers" (`ui/app.rs`'s `FormantBufferPicker`), never a breakpoint editor.
    /// `buffer_kind` says which of the two buffer shapes this param needs (they're never
    /// interchangeable — `formants put` always wants a whole curve, `oneform put` always
    /// wants a frozen snapshot). `relative_name` is the fixed temp filename this param's
    /// argv token always resolves to (`pipeline::plan_param` just emits it literally, every
    /// time — the actual *bytes* written there come from whichever buffer the user picked,
    /// injected directly into `PlannedJob.binary_input_files` by the app layer, bypassing
    /// `ParamValue` entirely since "which buffer" is a runtime UI selection, not a portable
    /// value).
    FormantBufferRef {
        buffer_kind: crate::model::formant::FormantBufferKind,
        relative_name: String,
    },
    /// A required reference to a real file on disk, picked via a file browser rather than
    /// typed or hand-drawn — `matrix matrix 2`'s `inmatrixfile` (a machine-generated
    /// matrix-data file, `matrix matrix 1`'s own `sidecar_extension` output, saved by the
    /// user under `.matrix` via a Save-As prompt). Unlike `FormantBufferRef` this carries no
    /// in-app storage of its own: the picked file already exists wherever the user saved it,
    /// so the UI's file browser (`ui/app.rs`, mirrors `Dialog::LoadCurve`'s picker) just
    /// resolves to a real absolute path, which `ParamValue::FilePath` carries directly.
    /// `extension` (no leading dot) is what the picker filters to.
    FilePath { extension: String },
    /// `crystal rotate`'s VDAT datafile — a **compound** param, the first in this catalog:
    /// one argv token, one file, two structurally unrelated sections inside it (XYZ vertex
    /// triples, then a time/value amplitude envelope). See `ParamValue::CrystalVdat` and
    /// `CrystalVdat` for the shape and the rules CDP enforces on it.
    ///
    /// Deliberately not two params (a `Table` for the triples plus a `required_envelope`
    /// `Number` for the envelope), even though the halves would each fit an existing kind:
    /// CDP takes exactly **one** filename here, and the two sections are delimited only by
    /// line shape — the first line whose number count isn't 3 ends the vertex section and
    /// begins the envelope. Splitting them into two params would mean two datafiles for one
    /// argv slot, which `plan_param` (one `ParamPlan` per param, one token per plan) has no
    /// way to express, and would let a user configure one half and not the other with no
    /// single place to validate the pairing. The editor still presents them as two sections
    /// — it just does that inside one field (`ui::app`'s `CdpCrystalSection`), reusing the
    /// existing table and graphical-envelope editors polymorphically rather than by
    /// splitting the data model.
    ///
    /// Carries no configuration at all, unlike every other datafile kind here: every bound
    /// (coords -1..1, envelope values 0..1, times ascending from 0) is fixed by
    /// `crystal.c`, identical for both in-scope modes, and not something a catalog author
    /// could meaningfully vary — so they live as `CrystalVdat` associated constants where
    /// the writer, the validator, and the editor all read the same copy, rather than being
    /// restated (and able to drift) per catalog entry.
    CrystalVdat,
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
            // Real coverage needs an actual formant/snapshot buffer's bytes, which this
            // value-less variant can't supply — the catalog smoke test special-cases
            // `FormantBufferRef` params instead of driving them through this helper (see
            // `cdp::runner`'s `KNOWN_FIXTURE_FAILURES`-adjacent handling).
            ParamKind::FormantBufferRef { .. } => ParamValue::FormantBufferRef,
            // Same rationale as `FormantBufferRef` above: a real path needs a real file on
            // disk to point at, which this helper can't manufacture — the catalog smoke
            // test special-cases `FilePath` params the same way.
            ParamKind::FilePath { .. } => ParamValue::FilePath(String::new()),
            // The simplest value that satisfies every rule in `CrystalVdat::validate`: one
            // vertex at the origin (dead centre of the crystal — no time offset, mid pitch,
            // neutral brightness) and a symmetric 1-second rise/fall event envelope. Unlike
            // the UI's own seeding (`App::open_cdp_crystal_envelope_editor`, which stretches
            // the envelope across the real selection's duration), this helper has no
            // selection to measure, so it uses a fixed 1s — fine for the smoke test, whose
            // point is that the argv/datafile shape is accepted, not that the timing is
            // musically apt.
            ParamKind::CrystalVdat => ParamValue::CrystalVdat(CrystalVdat {
                vertices: vec![[0.0, 0.0, 0.0]],
                envelope: vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)],
            }),
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
    /// True for a `Number` param whose real CDP-enforced range is a **multiple of the input's
    /// duration** rather than a fixed span of seconds. `min`/`max`/`default` are then read as
    /// multipliers, and `App::cdp_fields_for` turns them into absolute seconds against the
    /// selection actually being processed, so both the value the field starts at and the range
    /// the dialog displays are right for that selection.
    ///
    /// Found on `distmore segszig` mode 3's `dur` (user report, 2026-07-27): the catalog
    /// declared a flat `[0.1 – 600.0]` with a default of 2.0, and CDP rejected it with
    /// "Value (2.000000) out of range (7.940000 to 254.080000)". Verified against the real
    /// binary at three input lengths — 1s → `[2, 64]`, 2s → `[4, 128]`, 4s → `[8, 256]` — so
    /// the rule is exactly 2× to 64× the input duration, and independent of every other
    /// parameter (the zigzag count doesn't move it). A flat range can't express that: the
    /// default was below the floor for *any* input longer than a second, i.e. the process
    /// could never run at its own defaults on realistic material.
    ///
    /// Distinct from `NumberScale::CappedAtInputDuration`, which clamps the *value* at plan
    /// time while leaving the catalog's literal range on display. Here the range itself is
    /// what's data-dependent, and showing the real one is the whole point — a clamp alone
    /// would silently rewrite what the user typed.
    #[serde(default)]
    pub range_scales_with_input_duration: bool,
    /// True for a `Number` param that should default to the **negative of the input's mean
    /// sample value** — i.e. the shift that cancels its DC offset.
    ///
    /// Exists for `housekeep extract 4` ("Remove DC Offset"), whose catalog default of 0.0
    /// made it fail every single time: CDP rejects a zero shift outright with "CANNOT ACHIEVE
    /// TASK: NO CHANGE to original sound file" (user report, 2026-07-27). The binary asks for
    /// a shift amount and *adds* it to every sample — verified by hand: a file with a +0.1
    /// mean given `shift = +0.1` comes back with a +0.2 mean, and given `-0.1` comes back at
    /// 0.0 — so the value that actually removes the offset is the negated mean. Measuring it
    /// in `App::cdp_fields_for` turns a process that could never run at its defaults into one
    /// that does the thing its title promises.
    ///
    /// Falls back to one `step` when the measured offset rounds to zero, since zero is the
    /// one value CDP refuses.
    #[serde(default)]
    pub default_from_dc_offset: bool,
    /// True for a `Table` param on a variadic-input process whose datafile must hold
    /// **exactly one row per input file**. The row count then isn't a free choice at all, so
    /// the UI keeps it in step with the buffer pick rather than making the user maintain it
    /// (`App::sync_cdp_table_to_input_count`).
    ///
    /// Exists for `tesselate`, whose datafile CDP describes as "two lines, with the same
    /// number of entries per line, and the number of entries corresponds to the number of
    /// input files". Its table shipped with a single default row, so picking any number of
    /// buffers other than one failed with "No of data items (1) in 1st line of file
    /// table_0.txt doesn't correspond to no of input files (5)" — i.e. it could never run
    /// with more than one source, which is the entire point of the process (user report,
    /// 2026-07-27).
    #[serde(default)]
    pub rows_match_input_count: bool,
    #[serde(flatten)]
    pub kind: ParamKind,
}

/// Lets one process offer a "process both channels separately" option: a toggle that, when
/// on, runs the (otherwise `stereo_native`) binary once per channel with a *different* value
/// for one of its params.
///
/// Exists for Remove DC Offset (`housekeep extract 4`), which shifts the whole file by a
/// single value — so on a stereo file with different offsets per channel, no single value
/// removes both. CDP has no per-channel mode of its own; the split is the app running the
/// binary once per channel, which the lane machinery in `pipeline::plan_wav` already does for
/// every mono-only process.
///
/// The toggle and the extra value params are **never emitted to argv** (`ProcessDef::is_ui_only_param`):
/// they exist to be filled in by the user and read here. Everything else about the process is
/// unchanged, including its behaviour with the toggle off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelSplit {
    /// Index of the `Toggle` param that turns the split on. Off, or on a mono input, the
    /// process runs exactly as it did before this existed.
    pub toggle: usize,
    /// Index of the param whose value becomes per-channel. Channel 0 keeps using it.
    pub param: usize,
    /// Params supplying channels 1, 2, … in order (channel 0 uses `param`). A channel past
    /// the end of this list reuses the last entry, so a 4-channel file never fails outright
    /// on a declaration written for stereo.
    pub extra: Vec<usize>,
}

/// One preset a Praat script defines for itself: which option of the process's preset menu it
/// is, and the parameter values that option sets.
///
/// `params` and `values` are parallel arrays rather than a list of pairs purely so the
/// generated TOML stays homogeneous (`params = [1, 3]`, `values = [1.5, 0.25]`) — a mixed
/// `[[1, 1.5]]` array is legal TOML but needlessly fragile across parsers, and the catalog
/// carries ten thousand of these. `ScriptPreset::pairs` is the accessor everything reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptPreset {
    /// Index into the preset param's `options`.
    pub option: usize,
    /// Indices into `ProcessDef::params`.
    pub params: Vec<usize>,
    /// One value per entry of `params`.
    pub values: Vec<f64>,
}

impl ScriptPreset {
    /// The (param index, value) pairs this preset sets, ignoring any trailing entry that has
    /// no counterpart — a defensive truncation so a hand-edited user catalog with mismatched
    /// array lengths cannot panic the dialog.
    pub fn pairs(&self) -> impl Iterator<Item = (usize, f64)> + '_ {
        self.params.iter().copied().zip(self.values.iter().copied())
    }
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
    /// The old hand-assigned *semantic* taxonomy (`distort`, `texture`, `filter`, …). No longer
    /// what the browser groups by — that is now CDP's own grouping, derived from `bin` in
    /// `model::cdp::group`. Kept because `catalog.toml` is machine-generated and still carries
    /// the field, and because it remains a reasonable secondary descriptor; nothing reads it
    /// for navigation. See `group.rs`'s module doc for why the switch happened.
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
    /// `Some` for a process whose binary *hard-rejects* an input that isn't the channel count
    /// it wants — the multichannel-spatialisation family (`mchanpan`, `mchanrev`, `mchiter`,
    /// `mchzig`, `mchshred`, `crumble`, `madrid`, `spin`, `pairex`). `None` (every process
    /// before this existed) keeps the old behaviour exactly: `stereo_native` decides between
    /// one whole-document run and one mono lane per channel.
    ///
    /// The pair `stereo_native: bool` could not express these. `false` lane-splits, which
    /// runs a mono→8-channel spatialiser once per input channel and then reads only channel
    /// 0 of each 8-channel result; `true` hands the binary the whole document, which it
    /// refuses outright. Verified against the real binaries — each states its own demand and
    /// exits 255:
    ///
    /// ```text
    /// mchanrev   stereo in → "File stereo.wav is not of correct type (must be mono)"
    /// crumble    stereo in → "File stereo.wav is not a mono soundfile"
    /// mchshred 1 stereo in → "does not have correct number of channels for this mode"
    /// spin stereo 2, mono in → "File mono.wav is not of correct type (must be stereo)"
    /// pairex     stereo in → "must have more than two channels"
    /// ```
    ///
    /// So this is what the *binary* demands, and `pipeline::plan_wav` selects the channels to
    /// satisfy it (see [`InputChannels`] for what each variant takes from the document, and
    /// `App::cdp_params_blocker` for the case where the document can't satisfy it at all).
    #[serde(default)]
    pub input_channels: Option<InputChannels>,
    /// `Some` for a process whose output channel count is neither the input's nor a flat 2 —
    /// again the multichannel family, whose OUTCHANS is an ordinary parameter of the process.
    /// `None` (every process before this existed) keeps `output_is_stereo`'s meaning
    /// unchanged: stereo when set, same-as-input otherwise.
    ///
    /// Needed because `output_is_stereo: bool` can only say "2", and `dest_channels` has to
    /// carry one entry per channel the real output file holds — `cdp::runner`'s `read_outputs`
    /// reads exactly as many channels as there are entries, so an under-count silently
    /// discards the rest of the spatialisation, the same failure `output_is_stereo` was itself
    /// introduced to fix for `rmverb` (see `plan_wav`'s comment on that).
    #[serde(default)]
    pub output_channels: Option<OutputChannels>,
    /// True for a process whose result becomes its **own new buffer** rather than being
    /// spliced over the selection.
    ///
    /// Every multichannel process changes the channel count, and splicing an 8-channel result
    /// into the document would rewrite the document's own width — `CdpProcessCommand` widens
    /// for that case, but the result is that spatialising a mono take silently turns the take
    /// itself into an 8-channel buffer. A new buffer leaves the source alone, which is what
    /// these processes are for: the input is material, the output is a new spatial rendering
    /// of it. It is also what makes the *narrowing* direction safe (`pairex`, 8 channels in
    /// and 2 out) — `Document::insert_range` fills any channel the data doesn't cover from
    /// channel 0, so an in-place narrowing splice would smear channel 0 across the rest.
    ///
    /// Same shape a glob-output process (`distcut`, `envcut`) already uses for its numbered
    /// results, just reached by declaration rather than by result count.
    #[serde(default)]
    pub output_new_buffer: bool,
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
    /// `Some(ext)` for a process whose real binary writes a second, secondary output file
    /// alongside its normal `Wav` result — same base name, a different extension (e.g.
    /// `matrix matrix 1` writes both `out.wav` and `out.txt`, confirmed against the real
    /// binary: the `.txt` file holds the generated matrix data). `pipeline::plan_wav`'s
    /// mono/`stereo_native` branch turns this into `PlannedJob.output_sidecar` (`"out.{ext}"`,
    /// same fixed `"out.wav"` stem every process in that branch already uses); `cdp::runner`
    /// reads its bytes back before the job's temp directory is cleaned up, for the app layer
    /// to offer a Save-As prompt on. `None` (the default — every process before
    /// `matrix_matrix_1`) means no secondary file exists to capture.
    #[serde(default)]
    pub sidecar_extension: Option<String>,
    /// Smallest input-file count a variadic-input process (`IoKind::VariadicWav`/
    /// `GroupedWav`) will actually run with; meaningless for every other `input` kind, whose
    /// arity the `IoKind` itself already fixes. `None` (the default, and every process
    /// before the variadic-input batch) means 1 — the natural floor, since input 0 is always
    /// the active selection.
    ///
    /// Needed because CDP's real floors differ per process and are *not* derivable from the
    /// usage text: `pulser multi` and `repair repair` both declare `MANY_SNDFILES`
    /// internally, which hard-rejects anything under 2 ("Insufficient input files for this
    /// process") in **all** modes — including `pulser multi 1`, whose usage line reads
    /// `infile1 [infile2 ......]` as though one would do. `tesselate`/`crystal rotate`
    /// declare `ONE_OR_MANY_SNDFILES` instead and genuinely accept a single file. Verified
    /// by running each binary at every count around its boundary.
    #[serde(default)]
    pub min_inputs: Option<usize>,
    /// True for a process that takes a **Head/Tail marklist** datafile — the DISTMORE family
    /// (`distmore bright` modes 1-3, `distmore segsbkwd` modes 1-9, `distmore segszig` mode
    /// 1), and nothing else in the catalog.
    ///
    /// The marklist is a plain text file of timemarks in seconds, in increasing order,
    /// alternating Head then Tail (the first is always a Head), needing **at least two
    /// complete pairs**. CDP takes it as a positional argument immediately after the outfile:
    /// `distmore bright 1-3 infile outfile marklist [-s… -d]`.
    ///
    /// It is deliberately **not** a `ParamDef` with `required_list = true`, which is how these
    /// processes were first cataloged. Hand-typing a list of times into a dialog field is
    /// unusable for the thing it describes — the marks are positions in the waveform the user
    /// is looking at. So the content comes from `Document.head_tail_marks` (placed with `h`,
    /// dragged with the mouse, persisted in a `.headstails` sidecar) and reaches the pipeline
    /// via `InputSpec.head_tail_marks`, already rebased to the selection. `pipeline` writes
    /// the datafile and inserts its filename in the right argv slot; there is no form field
    /// for it at all.
    #[serde(default)]
    pub needs_head_tail_marks: bool,
    /// Refines `needs_head_tail_marks`: the marks are a plain list of **times**, not
    /// Head/Tail *pairs*, so every mark counts on its own and one is enough.
    ///
    /// `scramble`'s eight per-segment and up-then-down modes take a "cuts" datafile —
    /// "Textfile of (increasing) times in src: process in each separate segment" — in exactly
    /// the argv slot the DISTMORE marklist occupies (`scramble scramble 5-8 infile outfile
    /// cuts seed …`), and in the same one-time-per-line format. What differs is only the
    /// reading: DISTMORE consumes marks two at a time as segment starts and ends, while a cut
    /// time is just a boundary, so the even/odd Head/Tail roles carry no meaning here and the
    /// "at least two complete pairs" floor would reject a perfectly good single cut.
    ///
    /// Typing those times into a dialog field was the alternative, and is unusable for the
    /// same reason it was for DISTMORE: they describe positions in the waveform in front of
    /// you.
    #[serde(default)]
    pub head_tail_marks_unpaired: bool,
    /// True for a process whose flagged params must be emitted **before the input filenames**
    /// rather than after the outfile.
    ///
    /// Nearly every CDP program is built on the shared CDP framework, which scans for flags
    /// *after* the positional filenames — the shape `build_process_args` emits by default.
    /// `fastconv` is not: it's one of RWD's standalone programs, parsing flags getopt-style
    /// with a leading `while (argv[1][0] == '-')` loop, so its own usage text reads
    /// `fastconv [-aX][-f] infile impulsefile outfile [dry]`.
    ///
    /// Getting this wrong fails *silently and completely*, which is why it needs a flag
    /// rather than being left to chance: with the flags trailing, `fastconv` never sees them
    /// **and** stops reading the positional `[dry]` too (it only accepts the dry value when
    /// the argument count is exactly right), so amplitude scaling, float output and dry/wet
    /// mix are all ignored at once and every setting produces a byte-identical, clipped,
    /// integer-quantised result — exactly the user report ("still heavily clips and sounds
    /// the same no matter what the settings are") that turned this up. Verified against the
    /// real binary: `src ir out 0.5 -a0.2 -f` gives peak 0.0417 with a 16-bit file, while
    /// `-a0.2 -f src ir out 0.5` gives peak 0.3558 with a float one.
    ///
    /// Only params that actually carry a `flag` move; bare positional params (fastconv's own
    /// `[dry]`) keep their place after the outfile. A sweep of every binary in this catalog
    /// with a flagged param (59 of them) found `fastconv` to be the only one whose usage puts
    /// flags ahead of the infile, so this stays an explicit opt-in rather than a heuristic.
    #[serde(default)]
    pub flags_before_infile: bool,
    /// `Some` for a process offering a "process both channels separately" toggle — see
    /// [`ChannelSplit`]. `None` (the default) for every other process, which behaves exactly
    /// as it did before this existed.
    #[serde(default)]
    pub channel_split: Option<ChannelSplit>,
    /// True for a process whose analysis input(s) must each contain **exactly one** window,
    /// which CDP produces only via a separate `spec grab` run. The planner emits that grab as
    /// a pre-pass step per input, between `pvoc anal` and the process itself, and feeds the
    /// process the grabbed files rather than the full analyses.
    ///
    /// When set, the **first N params are the grab positions**, one per input, in input order
    /// — each a percentage of *that* input's own duration (which is why the conversion can't
    /// be a `NumberScale`: `PercentOfInputDuration` resolves everything against input 0, and
    /// input 1's percentage has to resolve against input 1). The remaining params are the
    /// process's own, and only those are passed to it.
    ///
    /// Two processes need this and they need different counts, which is why it's a flag read
    /// against the input arity rather than a hardcoded key check: `morph glide` takes two
    /// single-window analyses ("INTERPOLATE, LINEARLY, BETWEEN 2 SINGLE ANALYSIS WINDOWS
    /// EXTRACTED WITH spec grab" — its own usage text names the pre-pass), and
    /// `get_partials harmonic` modes 1-2 take one ("file must have only a single analysis
    /// window"; modes 3-4 take a TIME argument instead and do their own grabbing, so they
    /// must NOT set this).
    ///
    /// Not to be confused with the several processes that *sound* like they need it and don't
    /// — `spec magnify`, `focus freeze`, `extend freeze`, `psow sustain` all take an ordinary
    /// multi-window analysis and freeze internally.
    #[serde(default)]
    pub spec_grab_prepass: bool,
    /// Index of the `Choice` param that selects one of the script's **own** presets, for a
    /// Praat process that has one. `None` for every CDP process and for a Praat script whose
    /// preset chain could not be read.
    ///
    /// praatAudioTools scripts implement presets *inside themselves*, as an
    /// `if preset = 2 … elsif preset = 3 …` chain that overwrites the other form variables. So
    /// choosing one already changes the sound correctly — but the dialog went on showing the
    /// manual values, so the user could neither see what a preset had chosen nor adjust it.
    /// [`ProcessDef::script_presets`] carries those values back out so the form can show them.
    #[serde(default)]
    pub preset_param: Option<usize>,
    /// Which option of `preset_param` means "use the values shown in the form" — the script's
    /// own Custom/Manual entry. Selecting a preset fills the fields and then switches the
    /// param back to *this* option, so the script leaves those values alone and what runs is
    /// exactly what the dialog shows.
    #[serde(default)]
    pub preset_custom_option: usize,
    /// The values each of `preset_param`'s options sets, extracted from the script.
    #[serde(default)]
    pub script_presets: Vec<ScriptPreset>,
    /// Ordered — this order is exactly the order these values appear as positional
    /// arguments on the CDP command line (flagged params are still emitted in this order,
    /// just as `-x<value>` tokens instead of bare ones). A process with no parameters emits
    /// no `params` field at all in TOML (there's no syntax for an empty array-of-tables),
    /// hence `#[serde(default)]`.
    #[serde(default)]
    pub params: Vec<ParamDef>,
}

impl ProcessDef {
    /// Which external program runs this process. Derived from `category` rather than stored
    /// alongside it, so the two can never drift apart — the same reasoning that keeps
    /// `group::cdp_group` derived from `bin` instead of being a catalog column.
    pub fn backend(&self) -> Backend {
        match self.category {
            Category::Praat => Backend::Praat,
            Category::Time | Category::Pvoc => Backend::Cdp,
        }
    }

    /// Smallest and largest input-file counts this process accepts, and whether the count
    /// must be even. One place both `pipeline::plan_job`'s arity check and the UI's
    /// "can Apply run yet?" gate read, so the two can never disagree about whether a given
    /// pick is runnable. `None` for a max means unbounded (every variadic process — CDP
    /// imposes no ceiling beyond memory).
    /// Whether param `index` exists only to be filled in by the user and read by the app,
    /// never emitted as an argv token — today exactly the [`ChannelSplit`] toggle and its
    /// per-channel value params. CDP has no flag for either; they describe *how the app runs
    /// the binary*, not something the binary is told.
    pub fn is_ui_only_param(&self, index: usize) -> bool {
        self.channel_split
            .as_ref()
            .is_some_and(|split| split.toggle == index || split.extra.contains(&index))
    }

    /// The per-channel value params from [`ChannelSplit`], resolved for `channel` — `None`
    /// when the process has no split, the toggle is off, or the input is mono. `values` is
    /// the full param list in catalog order, as everywhere else.
    ///
    /// Returns the *param index* to read this channel's value from, so the caller can
    /// substitute it into the split param's slot.
    pub fn channel_split_value_index(&self, values: &[ParamValue], channel: usize) -> Option<usize> {
        let split = self.channel_split.as_ref()?;
        if !matches!(values.get(split.toggle), Some(ParamValue::Toggle(true))) {
            return None;
        }
        if channel == 0 {
            return Some(split.param);
        }
        // A channel past the declared list reuses the last entry rather than failing: a
        // declaration written for stereo shouldn't make a 4-channel file unrunnable.
        split.extra.get(channel - 1).or_else(|| split.extra.last()).copied()
    }

    /// Whether this run should split into per-channel lanes because of its [`ChannelSplit`]
    /// toggle, overriding `stereo_native`. Mono inputs never split — there is nothing to
    /// separate.
    pub fn channel_split_active(&self, values: &[ParamValue], channels: usize) -> bool {
        channels > 1 && self.channel_split_value_index(values, 0).is_some()
    }

    /// Which of a `channels`-wide selection's channels go into this process's input file, or
    /// `None` for a process that declares no [`InputChannels`] and so keeps the old
    /// `stereo_native`-driven behaviour.
    ///
    /// `Err` when the selection cannot satisfy the binary at all — too narrow for `Stereo`,
    /// or not wide enough for `Multichannel`. The message is user-facing: it is what
    /// `App::cdp_params_blocker` shows under the dialog, so it names the shortfall rather
    /// than the enum variant.
    pub fn input_source_channels(&self, channels: usize) -> Option<Result<Vec<usize>, String>> {
        Some(match self.input_channels? {
            // Never a shortfall: every document has a channel 0.
            InputChannels::Mono => Ok(vec![0]),
            InputChannels::Stereo if channels >= 2 => Ok(vec![0, 1]),
            InputChannels::Stereo => {
                Err("this process needs a stereo source; the selection is mono".to_string())
            }
            InputChannels::Multichannel if channels > 2 => Ok((0..channels).collect()),
            InputChannels::Multichannel => Err(format!(
                "this process needs more than 2 channels; the selection has {channels}"
            )),
        })
    }

    /// How many channels this process's output file holds, given the parameter values a run
    /// is about to use — `None` for a process that declares no [`OutputChannels`], leaving
    /// `output_is_stereo`'s existing meaning in force.
    ///
    /// A count that doesn't resolve to a sane positive number (a hand-edited user catalog
    /// pointing `FromParam` at a param that isn't a count) falls back to `None` rather than
    /// planning a job with zero destination channels, which would read nothing back at all.
    pub fn output_channel_count(&self, values: &[ParamValue]) -> Option<usize> {
        let count = match self.output_channels? {
            OutputChannels::Fixed { count } => count,
            OutputChannels::FromParam { param } => match values.get(param)? {
                ParamValue::Number(n) => n.round() as usize,
                // A `Choice` holds an index; the option's own text is the count, which is what
                // a channel count locked to a single value already looks like in the catalog.
                ParamValue::Choice(i) => match &self.params.get(param)?.kind {
                    ParamKind::Choice { options, .. } => options.get(*i)?.trim().parse().ok()?,
                    _ => return None,
                },
                _ => return None,
            },
        };
        (count > 0).then_some(count)
    }

    pub fn input_arity(&self) -> (usize, Option<usize>, bool) {
        match self.input {
            IoKind::None | IoKind::Curve => (0, Some(0), false),
            IoKind::Wav | IoKind::Ana | IoKind::WavGlob => (1, Some(1), false),
            IoKind::DualWav | IoKind::DualAna => (2, Some(2), false),
            IoKind::VariadicWav => (self.min_inputs.unwrap_or(1).max(1), None, false),
            // Two equal-length channel-role groups, so at least one source per channel and
            // always an even total — see `IoKind::GroupedWav`'s doc comment.
            IoKind::GroupedWav => (self.min_inputs.unwrap_or(2).max(2), None, true),
        }
    }
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
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            rows_match_input_count: false,
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
            input_channels: None,
            output_channels: None,
            output_new_buffer: false,
            requires_simple_wav_input: false, sidecar_extension: None, min_inputs: None,
            needs_head_tail_marks: false,
            head_tail_marks_unpaired: false,
            flags_before_infile: false,
            channel_split: None,
            spec_grab_prepass: false,
            preset_param: None,
            preset_custom_option: 0,
            script_presets: Vec::new(),
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
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            rows_match_input_count: false,
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
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            rows_match_input_count: false,
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
            input_channels: None,
            output_channels: None,
            output_new_buffer: false,
            requires_simple_wav_input: false, sidecar_extension: None, min_inputs: None,
            needs_head_tail_marks: false,
            head_tail_marks_unpaired: false,
            flags_before_infile: false,
            channel_split: None,
            spec_grab_prepass: false,
            preset_param: None,
            preset_custom_option: 0,
            script_presets: Vec::new(),
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
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            rows_match_input_count: false,
            before_outfile: false,
            kind: ParamKind::Table {
                columns: vec![
                    TableColumn {
                        must_be_distinct: false,
                        name: "Time".into(),
                        min: 0.0,
                        max: 60.0,
                        step: 0.01,
                        default: 0.1,
                        scale: NumberScale::Plain,
                        integer: false,
                    },
                    TableColumn {
                        must_be_distinct: false,
                        name: "Amp".into(),
                        min: 0.0,
                        max: 1.0,
                        step: 0.01,
                        default: 0.5,
                        scale: NumberScale::Plain,
                        integer: false,
                    },
                    TableColumn {
                        must_be_distinct: false,
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
                transposed: false,
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
            input_channels: None,
            output_channels: None,
            output_new_buffer: false,
            requires_simple_wav_input: false, sidecar_extension: None, min_inputs: None,
            needs_head_tail_marks: false,
            head_tail_marks_unpaired: false,
            flags_before_infile: false,
            channel_split: None,
            spec_grab_prepass: false,
            preset_param: None,
            preset_custom_option: 0,
            script_presets: Vec::new(),
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
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            rows_match_input_count: false,
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
            input_channels: None,
            output_channels: None,
            output_new_buffer: false,
            requires_simple_wav_input: false, sidecar_extension: None, min_inputs: None,
            needs_head_tail_marks: false,
            head_tail_marks_unpaired: false,
            flags_before_infile: false,
            channel_split: None,
            spec_grab_prepass: false,
            preset_param: None,
            preset_custom_option: 0,
            script_presets: Vec::new(),
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
            must_be_distinct: false,
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
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            rows_match_input_count: false,
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
            input_channels: None,
            output_channels: None,
            output_new_buffer: false,
            requires_simple_wav_input: false, sidecar_extension: None, min_inputs: None,
            needs_head_tail_marks: false,
            head_tail_marks_unpaired: false,
            flags_before_infile: false,
            channel_split: None,
            spec_grab_prepass: false,
            preset_param: None,
            preset_custom_option: 0,
            script_presets: Vec::new(),
            params: vec![param],
        };

        let text = toml::to_string(&def).expect("serialize");
        let back: ProcessDef = toml::from_str(&text).expect("deserialize");
        assert_eq!(def, back);
    }

    fn crystal_param() -> ParamDef {
        ParamDef {
            name: "Crystal Data".into(),
            description: "Vertices and event envelope".into(),
            flag: None,
            automatable: false,
            required_envelope: false,
            required_list: false,
            list_is_time_sequence: false,
            range_scales_with_input_duration: false,
            default_from_dc_offset: false,
            rows_match_input_count: false,
            // The real argv is `crystal rotate <mode> fi [fi2..] fo vdat ...` — the datafile
            // sits *after* the outfile, so this is the ordinary (default) placement.
            before_outfile: false,
            kind: ParamKind::CrystalVdat,
        }
    }

    fn crystal_def(params: Vec<ParamDef>) -> ProcessDef {
        ProcessDef {
            key: "crystal_rotate_1".into(),
            bin: "crystal".into(),
            subprog: Some("rotate".into()),
            mode: Some("1".into()),
            title: "Crystal (Mono)".into(),
            category: Category::Time,
            subcategory: "texture".into(),
            short_description: "Rotate a crystal".into(),
            description: "Full description.".into(),
            input: IoKind::VariadicWav,
            output: IoKind::Wav,
            stereo_native: false,
            output_is_stereo: false,
            input_channels: None,
            output_channels: None,
            output_new_buffer: false,
            requires_simple_wav_input: false,
            sidecar_extension: None,
            needs_head_tail_marks: false,
            head_tail_marks_unpaired: false,
            flags_before_infile: false,
            channel_split: None,
            spec_grab_prepass: false,
            preset_param: None,
            preset_custom_option: 0,
            script_presets: Vec::new(),
            min_inputs: None,
            params,
        }
    }

    /// `ParamKind::CrystalVdat` is the catalog's first **unit** variant under the
    /// `#[serde(tag = "kind")]` + `#[serde(flatten)]` combination every other variant uses
    /// as a struct variant — worth confirming in isolation, exactly like the `Table`/
    /// `MarkerTimeList`/`HiliteBand` schema tests above, since a flattened internally-tagged
    /// *unit* variant is a distinct serde code path (a bare `kind = "..."` key with no
    /// sibling data) that could plausibly fail to round-trip through TOML.
    #[test]
    fn crystal_vdat_param_round_trips_through_toml() {
        let def = crystal_def(vec![crystal_param()]);
        let text = toml::to_string(&def).expect("serialize");
        assert!(text.contains("kind = \"crystal_vdat\""), "the tag must survive flattening: {text}");
        let back: ProcessDef = toml::from_str(&text).expect("deserialize");
        assert_eq!(def, back);
    }

    /// `default_value` must produce something `validate` accepts — the catalog smoke test
    /// drives every process through it, so a default that violates CDP's own parse rules
    /// would fail as a confusing datafile error rather than a real argv-shape check.
    #[test]
    fn crystal_vdat_default_value_is_already_valid() {
        let ParamValue::CrystalVdat(vdat) = ParamKind::CrystalVdat.default_value() else {
            panic!("wrong value kind");
        };
        assert_eq!(vdat.validate(), Ok(()));
    }

    #[test]
    fn crystal_vdat_validate_rejects_a_vertex_outside_the_unit_sphere() {
        // Every coordinate is individually inside -1..1, but the vector length is ~1.56 —
        // exactly the case the usage text presents as advice and the binary actually
        // rejects ("vertex 1 lies outside the unit sphere").
        let vdat = CrystalVdat {
            vertices: vec![[0.9, 0.9, 0.9]],
            envelope: vec![(0.0, 0.0), (1.0, 0.0)],
        };
        assert!(vdat.validate().unwrap_err().contains("unit sphere"));

        // Exactly on the sphere is accepted (CDP rejects only `> 1.0`).
        let on_sphere = CrystalVdat {
            vertices: vec![[1.0, 0.0, 0.0]],
            envelope: vec![(0.0, 0.0), (1.0, 0.0)],
        };
        assert_eq!(on_sphere.validate(), Ok(()));
    }

    #[test]
    fn crystal_vdat_validate_enforces_the_envelope_contract() {
        let with = |envelope: Vec<(f64, f64)>| CrystalVdat { vertices: vec![[0.0, 0.0, 0.0]], envelope };

        assert!(with(vec![(0.0, 0.0)]).validate().unwrap_err().contains("at least 2"));
        assert!(with(vec![(0.1, 0.0), (1.0, 0.0)]).validate().unwrap_err().contains("time 0"));
        assert!(with(vec![(0.0, 0.0), (0.5, 0.5), (0.5, 0.0)]).validate().unwrap_err().contains("strictly increase"));
        assert!(with(vec![(0.0, 0.0), (0.5, 1.5), (1.0, 0.0)]).validate().unwrap_err().contains("between 0 and 1"));
        assert!(with(vec![(0.0, 0.2), (1.0, 0.0)]).validate().unwrap_err().contains("first and last"));
        assert!(with(vec![(0.0, 0.0), (1.0, 0.3)]).validate().unwrap_err().contains("first and last"));
        assert_eq!(with(vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]).validate(), Ok(()));
    }

    #[test]
    fn crystal_vdat_validate_rejects_an_empty_vertex_list() {
        let vdat = CrystalVdat { vertices: Vec::new(), envelope: vec![(0.0, 0.0), (1.0, 0.0)] };
        assert!(vdat.validate().unwrap_err().contains("at least one crystal vertex"));
    }
}

