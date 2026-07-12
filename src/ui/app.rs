use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::audio::engine::AudioEngine;
use crate::config::Config;
use crate::commands::cut::cut_command;
use crate::commands::delete::delete_command;
use crate::commands::fade::{fade_command, technical_fades_command, FadeCurve};
use crate::commands::gain::gain_command;
use crate::commands::marker::{
    auto_insert_markers_command, delete_marker_command, insert_marker_command, move_marker_command, rename_marker_command,
};
use crate::commands::paste::paste_command;
use crate::commands::normalize::normalize_command;
use crate::commands::resample::resample_command;
use crate::commands::reverse::reverse_command;
use crate::commands::trim::trim_command;
use crate::model::clipboard::Clipboard;
use crate::model::document::{Document, Marker};
use crate::model::dsp;
use crate::model::history::History;
use crate::model::io::{save_wav, save_wav_with, BitDepth};
use crate::model::selection::Selection;

use super::buffer_panel::BufferPanel;
use super::file_panel::{EntryKind as FileEntryKind, FilePanel};
use super::keymap::{build_action_display_map, build_key_map, fill_missing_keybindings, map_key, Action};
use super::layout::split_chrome;
use super::menu::MenuBar;
use super::terminal::Tui;
use super::text_input::TextInput;
use super::theme;
use super::toolbar::Toolbar;
use super::viewport::Viewport;
use super::waveform_cache::WaveformCache;
use super::widgets::db_scale::{DbScaleWidget, DB_GUTTER_WIDTH};
use super::widgets::cdp_envelope_image::{self, interp_cdp_envelope};
use super::widgets::statusbar::StatusBar;
use super::widgets::waveform::WaveformWidget;
use super::widgets::waveform_image;

/// Focus indices and row geometry for the Export Regions dialog, shared by every site that
/// needs to know "which focus index is which": input routing (`dialog_input`), the char
/// filter (`dialog_accepts`), Enter validation, Space toggles, Tab wrapping, mouse row
/// clicks, and the render fn. Wire any new field through these constants — the mapping
/// used to be repeated as bare literals at each of those sites, and a missed edit
/// compiled cleanly while toggling the wrong checkbox.
mod er_focus {
    pub const SUBFOLDER: usize = 0;
    pub const BASE_NAME: usize = 1;
    pub const FORMAT: usize = 2;
    pub const DITHER: usize = 3;
    pub const LIMIT_CB: usize = 4;
    pub const LIMIT_MS: usize = 5;
    pub const NORMALIZE_CB: usize = 6;
    pub const NORMALIZE_DB: usize = 7;
    pub const FADE_IN_CB: usize = 8;
    pub const FADE_IN_MS: usize = 9;
    pub const FADE_OUT_CB: usize = 10;
    pub const FADE_OUT_MS: usize = 11;
    pub const COUNT: usize = 12;

    /// First index (into `dialog_row_rects`) of the four checkbox+value rows — the rows
    /// before it (subfolder, base name, format, dither) have focus index == row index.
    pub const FIRST_CHECKBOX_ROW: usize = 4;

    /// Maps a checkbox+value mouse row (`FIRST_CHECKBOX_ROW`..=7) to its
    /// (checkbox, value field) focus-index pair.
    pub fn checkbox_row_focus(row: usize) -> (usize, usize) {
        let cb = LIMIT_CB + (row - FIRST_CHECKBOX_ROW) * 2;
        (cb, cb + 1)
    }

    /// Width the checkbox rows' labels are padded to, so all four value fields start in
    /// the same column.
    pub const ROW_LABEL_WIDTH: usize = 16;

    /// Column within a checkbox+value row's Rect where the value text starts:
    /// `"   [X] "` (7 chars) + the padded label + one space. Clicks left of this toggle
    /// the checkbox; clicks on/after it focus the value field for editing.
    pub const VALUE_COL: u16 = (3 + 3 + 1 + ROW_LABEL_WIDTH + 1) as u16;
}

/// Optional per-region processing for [`App::export_regions`], one field per Export
/// Regions dialog checkbox: `None` means the option is off, `Some(value)` enables it.
/// (The dialog validates values before building this, but `export_regions` still guards
/// against nonsensical ones — tests call it directly.)
struct RegionExportOptions {
    /// Cap each region's duration at this many milliseconds (must be > 0 to apply).
    limit_length_ms: Option<f32>,
    /// Normalize each region's peak to this dBFS target, independently per region.
    normalize_db: Option<f32>,
    /// Exp² fade-in over this many milliseconds at each region's start.
    fade_in_ms: Option<f32>,
    /// Exp² fade-out over this many milliseconds at each region's end.
    fade_out_ms: Option<f32>,
}

/// Number of samples covered by `ms` milliseconds at `sample_rate` (rounded).
fn ms_to_samples(ms: f32, sample_rate: u32) -> usize {
    ((ms / 1000.0) * sample_rate as f32).round() as usize
}

/// One-line message for a `PlanError`, shown inline in `Dialog::CdpParams`. Every case
/// here is otherwise prevented by the UI (the browser hides dual-input processes, a
/// document is always available once the dialog is open) except `ParamCountMismatch`,
/// which would indicate a catalog/UI mismatch bug rather than a user-fixable condition.
fn cdp_plan_error_message(err: &crate::model::cdp::PlanError) -> String {
    use crate::model::cdp::PlanError;
    match err {
        PlanError::UnsupportedInV1 { reason } => format!("not supported yet: {reason}"),
        PlanError::MissingInput => "no audio to process".into(),
        PlanError::ParamCountMismatch { expected, actual } => {
            format!("internal error: expected {expected} params, got {actual}")
        }
        PlanError::InputCountMismatch { expected, actual } => {
            format!("internal error: expected {expected} inputs, got {actual}")
        }
        PlanError::SampleRateMismatch { first, second } => {
            format!("second input is {second} Hz, selection is {first} Hz — resample one first")
        }
    }
}

/// Splits a `CdpError` into display lines for `Dialog::CdpOutput`: a short summary line
/// followed by the captured process output (if any), one line per line of output so the
/// dialog can scroll it.
fn cdp_error_lines(err: &crate::cdp::CdpError) -> Vec<String> {
    use crate::cdp::CdpError;
    match err {
        CdpError::Spawn { step, message } => {
            vec![format!("Failed to start '{step}': {message}")]
        }
        CdpError::NonZeroExit { step, code, output } => {
            let mut lines = vec![format!(
                "'{step}' exited with {}",
                code.map(|c| c.to_string()).unwrap_or_else(|| "no exit code (killed?)".into())
            )];
            lines.extend(output.lines().map(str::to_string));
            lines
        }
        CdpError::NoOutput { step } => vec![format!("'{step}' produced no output file")],
        CdpError::OutputRead { path, message } => {
            vec![format!("Failed to read '{path}': {message}")]
        }
        CdpError::Cancelled => vec!["Cancelled.".into()],
    }
}

/// One editable field in the `Dialog::CdpParams` form, mirroring a `ParamKind` from the
/// catalog. Built fresh (from each param's default value) whenever `CdpParams` opens for a
/// process, or loaded from a saved preset (`CdpField::from_value`).
#[derive(Clone)]
enum CdpField {
    /// `min`/`max`/`step` are cloned from the `ParamKind::Number` at construction time so
    /// Up/Down nudging (`cdp_nudge_number`) never needs a catalog lookup — same rationale
    /// as `Choice::options` below. `envelope` is `Some` exactly when this field has been
    /// switched from a constant value to time-varying automation via the envelope editor
    /// (`Dialog::CdpParams.envelope`, `App::open_cdp_envelope_editor`) — `to_value` returns
    /// `ParamValue::Breakpoints` instead of parsing `input` whenever it's set. `input` keeps
    /// whatever constant value was last live so switching back to constant mode (the
    /// editor's 'c' key) doesn't lose it.
    Number { input: TextInput, min: f64, max: f64, step: f64, envelope: Option<Vec<(f64, f64)>> },
    Toggle { on: bool },
    /// `options` is cloned from the `ParamKind::Choice` at construction time so cycling it
    /// (Left/Right) never needs a catalog lookup back through the current selection.
    Choice { options: Vec<String>, selected: usize },
    /// A `ParamDef.required_list` field (`CDP-Ext-Plan.md` Phase 3's plain-list shape) — a
    /// distinct variant, not a `Number` mode like `envelope`, because there's no constant
    /// fallback to fall back *to*: the argv token is always a list-file path. `values`
    /// starts empty ("not set yet") until `App::open_cdp_list_editor` commits at least one
    /// entry — no wrapping `Option` needed, an empty `Vec` already means "unset."
    /// `min`/`max`/`step` bound each entry, same rationale as `Number`'s.
    List { values: Vec<f64>, min: f64, max: f64, step: f64 },
}

impl CdpField {
    fn from_default(param: &crate::model::cdp::ParamDef) -> Self {
        use crate::model::cdp::ParamKind;
        if param.required_list {
            let ParamKind::Number { min, max, step, .. } = &param.kind else {
                panic!("required_list param {:?} is not a Number kind", param.name);
            };
            return CdpField::List { values: Vec::new(), min: *min, max: *max, step: *step };
        }
        match &param.kind {
            ParamKind::Number { default, min, max, step, .. } => {
                CdpField::Number {
                    input: TextInput::fresh(format_cdp_float_for_display(*default)),
                    min: *min,
                    max: *max,
                    step: *step,
                    envelope: None,
                }
            }
            ParamKind::Toggle { default } => CdpField::Toggle { on: *default },
            ParamKind::Choice { options, default } => {
                CdpField::Choice { options: options.clone(), selected: *default }
            }
        }
    }

    /// Builds a field from a saved preset's value rather than the catalog default —
    /// `App::cdp_params_cycle_preset`'s load path. Falls back to `from_default` on a
    /// kind/value mismatch (the catalog def changed a param's *type*, not just its count,
    /// since the file was saved — `preset::load_presets` already filters out a *count*
    /// mismatch, but a same-length type change would slip through that check) rather than
    /// panicking on a saved preset that no longer lines up with the live catalog.
    fn from_value(param: &crate::model::cdp::ParamDef, value: &crate::model::cdp::ParamValue) -> Self {
        use crate::model::cdp::{ParamKind, ParamValue};
        match (&param.kind, value) {
            (ParamKind::Number { min, max, step, .. }, ParamValue::Number(v)) => CdpField::Number {
                input: TextInput::fresh(format_cdp_float_for_display(*v)),
                min: *min,
                max: *max,
                step: *step,
                envelope: None,
            },
            (ParamKind::Number { min, max, step, .. }, ParamValue::Breakpoints(points)) => CdpField::Number {
                input: TextInput::fresh(format_cdp_float_for_display(
                    points.first().map(|&(_, v)| v).unwrap_or(0.0),
                )),
                min: *min,
                max: *max,
                step: *step,
                envelope: Some(points.clone()),
            },
            (ParamKind::Number { min, max, step, .. }, ParamValue::List(values)) => {
                CdpField::List { values: values.clone(), min: *min, max: *max, step: *step }
            }
            (ParamKind::Toggle { .. }, ParamValue::Toggle(on)) => CdpField::Toggle { on: *on },
            (ParamKind::Choice { options, .. }, ParamValue::Choice(i)) => CdpField::Choice {
                options: options.clone(),
                selected: (*i).min(options.len().saturating_sub(1)),
            },
            _ => CdpField::from_default(param),
        }
    }

    fn to_value(&self) -> crate::model::cdp::ParamValue {
        use crate::model::cdp::ParamValue;
        match self {
            CdpField::Number { envelope: Some(points), .. } => ParamValue::Breakpoints(points.clone()),
            CdpField::Number { input, .. } => {
                ParamValue::Number(input.value().trim().parse::<f64>().unwrap_or(0.0))
            }
            CdpField::Toggle { on } => ParamValue::Toggle(*on),
            CdpField::Choice { selected, .. } => ParamValue::Choice(*selected),
            CdpField::List { values, .. } => ParamValue::List(values.clone()),
        }
    }
}

/// Formats a float for a CDP number field's *programmatically set* text (the initial
/// default, or the result of an Up/Down nudge) so it always carries a decimal point —
/// `1` reads as "did this actually load?" in a field the user hasn't touched yet, where
/// "1.0" unambiguously reads as a float default. Rust's plain `{v}` (used for the argv
/// CDP itself receives, in `model::cdp::pipeline`, which is a separate concern — CDP
/// accepts "5" and "5.0" identically) omits the trailing ".0" for whole numbers, so this
/// checks for and adds it back rather than reusing that formatter.
fn format_cdp_float_for_display(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// Nudges a focused `CdpField::Number` by `sign * step`, clamped to its range — Up/Down's
/// role in the params dialog (Left/Right is reserved for the field's own text cursor, or
/// for cycling a `Choice`). No-op for any other field kind.
fn cdp_nudge_number(field: Option<&mut CdpField>, sign: f64) {
    let Some(CdpField::Number { input, min, max, step, .. }) = field else { return };
    let current = input.value().trim().parse::<f64>().unwrap_or(*min);
    let next = (current + sign * *step).clamp(*min, *max);
    *input = TextInput::new(format_cdp_float_for_display(next));
}

enum Dialog {
    Normalize { input: TextInput },
    /// `input` holds the single overall gain when `per_channel` is off, or the Left
    /// channel's gain when it's on; `right_input` holds the Right channel's gain and is
    /// only used/shown when both `is_stereo` and `per_channel` are true. `is_stereo` is
    /// captured once at dialog-open time from the active document's channel count (the
    /// document can't change while the dialog is up), since `render_dialog` has no document
    /// access to recompute it.
    Gain {
        input: TextInput,
        right_input: TextInput,
        tanh_clip: bool,
        per_channel: bool,
        is_stereo: bool,
        focused: usize,
    },
    FadeIn { curve: FadeCurve },
    FadeOut { curve: FadeCurve },
    Resample { input: TextInput, current_rate: u32 },
    RenameMarker { position: usize, input: TextInput },
    OpenDirectory { input: TextInput },
    RenameBuffer { index: usize, input: TextInput },
    /// Rename the file at `path` on disk (Files panel `r`). Esc cancels.
    RenameFile { path: PathBuf, input: TextInput },
    /// Mix-to-mono: one TextInput per source channel (dB gain, or the literal "-inf" for
    /// silence). `focused` is the index of the currently-active field; Tab cycles through.
    /// `tanh_clip` enables a tanh soft-limiter on the mixed output (same as Gain's option).
    MixToMono { inputs: Vec<TextInput>, focused: usize, tanh_clip: bool },
    /// Export Regions to Subfolder — chops file at markers and saves each region.
    /// `focused` is one of the [`er_focus`] indices (subfolder, base name, format, dither,
    /// then a checkbox + value-field pair for each of limit length/normalize/fade in/fade
    /// out).
    /// Per-region processing order (see `App::export_regions`) is limit length, then
    /// normalize, then fades — trimming before normalizing means the peak measurement
    /// reflects the audio that's actually kept, and fading last means the envelope taper
    /// is never itself included in that peak measurement.
    ExportRegions {
        folder_input: TextInput,
        base_name_input: TextInput,
        depth: BitDepth,
        dither: bool,
        limit_length: bool,
        limit_length_input: TextInput,
        normalize: bool,
        normalize_input: TextInput,
        fade_in: bool,
        fade_in_input: TextInput,
        fade_out: bool,
        fade_out_input: TextInput,
        focused: usize,
    },
    /// First-run prompt for the CDP (Composer's Desktop Project) binaries directory —
    /// opened whenever `Action::CdpProcess` fires and `config.cdp_dir` is unset or fails
    /// `cdp::validate_cdp_dir`. Enter re-validates and, on success, proceeds straight to
    /// `CdpBrowser`; the menu entry stays always-enabled rather than being conditionally
    /// greyed out, so a bad/missing path is discovered here rather than silently.
    CdpSetup { input: TextInput, error: Option<String> },
    /// Searchable, group-filterable list of CDP processes — three columns
    /// (`render_cdp_browser_dialog`): groups, the process list, and the highlighted
    /// process's full `description`. Deliberately a *fixed* size regardless of scroll
    /// position or which process is highlighted: params live in the separate `CdpParams`
    /// dialog now, so nothing here varies per process — this is what stops the dialog
    /// resizing itself as you browse (the thing the browser/params split exists to fix;
    /// they used to be one merged dialog).
    ///
    /// `groups` is `["All", "Recent", ...every real `subcategory` value in the catalog,
    /// alphabetically]` (`App::cdp_groups`), computed once at open time since the catalog
    /// doesn't change while the dialog is up. `group_selected` indexes into `groups`;
    /// `recent` is a snapshot of `model::cdp::recent::load_recent()` taken at open time
    /// (most-recent-first — the *order* `entries` should show when "Recent" is highlighted,
    /// not just a membership filter, so it can't be re-derived from `entries` itself).
    /// `entries` are indices into the loaded `CdpCatalog::processes`, filtered by BOTH the
    /// highlighted group and `search`'s text (case-insensitive substring over
    /// key/title/short_description) — `App::refresh_cdp_browser_filter` recomputes it
    /// whenever either changes; `selected` indexes into `entries`, not the catalog directly.
    ///
    /// `group_focus` says which column Up/Down/PageUp/PageDown act on — Tab/Shift+Tab
    /// toggles it (`App::cycle_dialog_focus`), mirroring `CdpParams`'s Tab-cycle convention
    /// rather than inventing a new one. Search stays typable from either column (typing
    /// always narrows the currently-highlighted group; "All" is what makes it search the
    /// whole catalog) so there's no separate "focus the search box" step. Enter or a mouse
    /// click on a process entry opens `Dialog::CdpParams` for it (`App::open_cdp_params`,
    /// via `handle_dialog_row_click` for the mouse path) regardless of which column has
    /// focus — only the highlighted *process* row matters for that, not `group_focus`.
    CdpBrowser {
        search: TextInput,
        groups: Vec<String>,
        group_selected: usize,
        group_focus: bool,
        recent: Vec<String>,
        entries: Vec<usize>,
        selected: usize,
    },
    /// The parameter-editing form for one CDP process, opened from `Dialog::CdpBrowser`
    /// (`App::open_cdp_params`). Esc closes this dialog outright — there is no "back to the
    /// browser," cancelling means cancelling the whole flow, matching how every other CDP
    /// dialog's Esc behaves. `catalog_index` is stable for this dialog's lifetime (the
    /// catalog doesn't change while it's open). `fields` are index-parallel to
    /// `catalog_processes[catalog_index].params`, built once at open time — there's no
    /// per-process rebuilding to worry about since this dialog only ever shows one process.
    /// Sized to fit every field, scrolling (`scroll`) if the terminal is too short to show
    /// them all at once, with column widths computed per-process from the actual longest
    /// label/range text (`cdp_params_column_widths`) rather than a fixed guess — the fixed
    /// guess was what let a long param name collide with its own range/value.
    ///
    /// `focus` is `CDP_PRESET_FOCUS` (0) for the preset row, or `field_index + 1` for the
    /// field at that index in `fields`, continuing past them with a second-input picker row
    /// (dual-input processes only), then Preview, then Apply — see
    /// `cdp_params_focus_second_input`/`cdp_params_focus_preview`/`cdp_params_focus_apply`
    /// (thin `+1`-shifted wrappers around the plain field-index-space helpers below, so both
    /// spaces share one source of truth for the trailing rows' positions). `second_input` is
    /// `Some` exactly when the process is dual-input (`IoKind::DualWav`/`DualAna`). `error`
    /// is a validation message shown inline (e.g. "value out of range") *before* any process
    /// runs; a failure *during* a run instead replaces this dialog outright with
    /// `CdpOutput`/`Info`, mirroring how `export_regions` replaces `ExportRegions` with
    /// `Info` on a failure rather than returning to it. `preview` caches the most recent
    /// successful Preview run so Apply can splice it straight in without re-running CDP when
    /// parameters haven't changed since — see `App::cdp_preview_matches`. `envelope` is
    /// `Some` while the ASCII/bitmap breakpoint-curve editor (`render_cdp_envelope_editor`)
    /// is open for one of `fields`' automatable Number params — it takes over the whole
    /// popup and all key handling until committed ('c'/Enter) or cancelled (Esc).
    ///
    /// `presets`/`preset_selected` are this process's saved presets (`model::cdp::preset`),
    /// loaded once when the dialog opens; Left/Right on the preset row (`focus ==
    /// CDP_PRESET_FOCUS`) cycles `preset_selected` and immediately loads that preset's
    /// values into `fields`. `save_prompt`, when `Some`, takes over key handling the same
    /// way `envelope` does — typing a name and pressing Enter saves the current field values
    /// under that name (prefilled with `preset_selected`'s name, if any, so re-saving over
    /// the same preset is just Enter); Esc cancels without saving.
    CdpParams {
        catalog_index: usize,
        fields: Vec<CdpField>,
        second_input: Option<CdpSecondInput>,
        focus: usize,
        error: Option<String>,
        preview: Option<CdpPreview>,
        envelope: Option<CdpEnvelopeEdit>,
        /// `Some` while the plain-list editor (`render_cdp_list_editor`) is open for one of
        /// `fields`' `required_list` params — mutually exclusive with `envelope` (a field is
        /// either breakpoint-shaped or list-shaped, never both), same take-over-all-key-
        /// handling shape.
        list_edit: Option<CdpListEdit>,
        presets: Vec<crate::model::cdp::preset::CdpPreset>,
        preset_selected: Option<usize>,
        save_prompt: Option<TextInput>,
        scroll: usize,
    },
    /// Hard-modal progress display while a `CdpRunner` job is in flight — deliberately
    /// blocks all other input (Esc cancels) because the job captured a snapshot of the
    /// active document's range/samples at launch; any concurrent edit/undo/buffer-close
    /// would leave nothing sane for the eventual splice to land in. Esc requests
    /// cancellation but does *not* itself close the dialog — only `App::tick_cdp` does,
    /// once the runner's `Finished(Err(Cancelled))` event actually arrives, so the modal
    /// never lies about a job still technically in flight.
    CdpRunning {
        job_id: u64,
        title: String,
        step_label: String,
        step_index: usize,
        step_total: usize,
        started: std::time::Instant,
        purpose: crate::cdp::JobPurpose,
    },
    /// Scrollable viewer for a CDP process's captured stdout+stderr after a failed run.
    CdpOutput { title: String, lines: Vec<String>, scroll: usize },
    /// Generic single-message info/error popup with an Enter/Esc-to-dismiss button.
    Info { message: String },
}

/// The second-input picker state for a dual-input CDP process (combine/morph/vocode/...):
/// which open buffer provides the second file. `doc_indices`/`names` are captured at
/// dialog-open time — safe because the dialog is modal, so buffers can't be opened, closed,
/// or renamed while it's up — and include every open buffer (processing a selection against
/// its own document, e.g. self-convolution, is legitimate).
#[derive(Clone)]
struct CdpSecondInput {
    doc_indices: Vec<usize>,
    names: Vec<String>,
    selected: usize,
}

impl CdpSecondInput {
    fn selected_doc_index(&self) -> Option<usize> {
        self.doc_indices.get(self.selected).copied()
    }
    fn selected_name(&self) -> &str {
        self.names.get(self.selected).map(String::as_str).unwrap_or("")
    }
}

/// State for the ASCII breakpoint-curve editor, active while `Dialog::CdpParams.envelope`
/// is `Some`. `points` are `(time_secs, value)` pairs, always kept sorted by time and with
/// at least 2 entries (a single point isn't a meaningful automation curve) — this is the
/// exact shape `ParamValue::Breakpoints` and the `.brk`-file writer in
/// `model::cdp::pipeline` already expect, so committing just clones `points` into the
/// field's `envelope`. `selected` indexes into `points`. `original` snapshots the field's
/// envelope as it was when the editor opened (`None` if the field was still a constant), so
/// Esc can discard every edit made this session and restore exactly that, not just "turn
/// automation off". `time_max` is the selection's duration in seconds — the field's own
/// `min`/`max`/`step` (looked up from `fields[field_index]` at render/key time rather than
/// duplicated here) bound the value axis.
struct CdpEnvelopeEdit {
    field_index: usize,
    points: Vec<(f64, f64)>,
    selected: usize,
    original: Option<Vec<(f64, f64)>>,
    time_max: f64,
    /// The same sample range `time_max` was derived from — kept alongside it so the
    /// graphics-mode renderer can look up the actual audio for that span to draw as a pale
    /// reference waveform behind the curve, without recomputing the selection.
    range: (usize, usize),
}

/// State for the plain-list editor, active while `Dialog::CdpParams.list_edit` is `Some` —
/// the `required_list` counterpart to `CdpEnvelopeEdit` (`CDP-Ext-Plan.md` Phase 3's plain
/// time/value-list shape), simpler since there's no interpolation or graphics-mode curve to
/// render: just an ordered list of numbers. `values` starts as a clone of the field's
/// current list (empty for a never-configured field); always kept with at least 1 entry
/// once the user has added one — unlike `CdpEnvelopeEdit`'s 2-point minimum, a
/// single-entry list is perfectly meaningful here (e.g. "freeze the spectrum at this one
/// time"). `original` snapshots the field's list as it was when the editor opened, so Esc
/// can discard every edit made this session.
///
/// `is_time_sequence` mirrors `ParamDef.list_is_time_sequence` (looked up once at open
/// time rather than re-checked per keystroke): when true, `App::handle_cdp_list_key`
/// constrains Up/Down and 'n' to keep `values` strictly ascending (CDP's own requirement
/// for e.g. `grain reposition`'s TIMEFILE — confirmed by hand: a submitted list with times
/// out of order fails with "Sync times out of sequence", found via the user manually
/// testing this exact editor). `time_max` is the *practical* nudge/clamp bound for a
/// time-sequence field: the real selection's duration in seconds, not the catalog's own
/// (necessarily generous) `max` — the same reasoning `CdpEnvelopeEdit.time_max` already
/// uses for its time axis, extended here since a time-sequence list's entries are exactly
/// the same kind of value. Unused (left at the catalog `max`) for a non-time-sequence list.
struct CdpListEdit {
    field_index: usize,
    values: Vec<f64>,
    selected: usize,
    original: Vec<f64>,
    is_time_sequence: bool,
    time_max: f64,
}

/// A cached successful Preview run, kept alongside `Dialog::CdpParams` so an unchanged-
/// parameter Apply can splice the already-computed audio instead of re-running CDP.
/// Invalidated (set back to `None`) by any field edit.
struct CdpPreview {
    values: Vec<crate::model::cdp::ParamValue>,
    range: (usize, usize),
    channels: Vec<Vec<f32>>,
    sample_rate: u32,
}

/// `Dialog::CdpBrowser`'s control focus range is dynamic (one index per param field, then a
/// second-input picker row for dual-input processes, then two trailing action buttons)
/// rather than a fixed `er_focus`-style constant set, since the field count varies per
/// process. These helpers compute the trailing indices from `fields.len()` +
/// `second_input.is_some()` at every call site so they can't drift out of sync with each
/// other. They operate in plain 0-based field-index space; `CDP_PRESET_FOCUS` and the
/// `cdp_browser_*_focus` wrappers below shift into the dialog's actual `focus` space, which
/// reserves index 0 for the process list itself.
fn cdp_params_second_input_focus(field_count: usize) -> usize {
    field_count
}
fn cdp_params_preview_focus(field_count: usize, has_second_input: bool) -> usize {
    field_count + has_second_input as usize
}
fn cdp_params_apply_focus(field_count: usize, has_second_input: bool) -> usize {
    cdp_params_preview_focus(field_count, has_second_input) + 1
}

/// `Dialog::CdpParams.focus` reserves 0 for the preset row; a field's own focus value is
/// `field_index + 1`. These three wrap the plain field-index-space helpers above with that
/// `+1` shift so both spaces share one source of truth for the trailing rows' positions.
const CDP_PRESET_FOCUS: usize = 0;
fn cdp_params_focus_second_input(field_count: usize) -> usize {
    cdp_params_second_input_focus(field_count) + 1
}
fn cdp_params_focus_preview(field_count: usize, has_second_input: bool) -> usize {
    cdp_params_preview_focus(field_count, has_second_input) + 1
}
fn cdp_params_focus_apply(field_count: usize, has_second_input: bool) -> usize {
    cdp_params_apply_focus(field_count, has_second_input) + 1
}

/// PageUp/PageDown step size for `Dialog::CdpBrowser`'s process list.
const CDP_BROWSER_PAGE_SIZE: usize = 10;

/// `Dialog::CdpBrowser`'s always-first group: no group filter, `search` spans the whole
/// catalog — the pre-Phase-7 behavior. Not a real `subcategory` value, so it can't collide
/// with one.
const CDP_GROUP_ALL: &str = "All";
/// `Dialog::CdpBrowser`'s always-second group: the entries in `Dialog::CdpBrowser.recent`,
/// most-recently-used first (not catalog order — see `App::cdp_filter_entries`).
const CDP_GROUP_RECENT: &str = "Recent";
/// Width of `Dialog::CdpBrowser`'s Groups column (`render_cdp_browser_dialog`) — shared
/// with `App::handle_dialog_row_click`'s Groups-vs-Processes hit-test so the two can't
/// disagree about which column an `x_in_row` falls in.
const CDP_GROUP_COL_WIDTH: u16 = 18;

/// Focus-order layout for the Gain dialog's interactive rows. The Gain/Left field is always
/// focus index 0. On a stereo document, a "Per-channel gain" checkbox and (when checked) a
/// Right field follow it, in that order, before the Tanh checkbox. Centralizing the
/// arithmetic here keeps the five call sites that need to know "which focus index is
/// which" — input routing, Enter, Space, Tab, mouse click, and render — from drifting out
/// of sync with each other.
///
/// This is a *focus* order, not a visual line order: the dialog's line layout is fixed
/// (see `render_gain_dialog`) so toggling per-channel never resizes or reflows the popup —
/// the Right field simply appears in a line that's blank when per-channel is off. Only the
/// Right field's presence in the focus cycle depends on `per_channel`.
struct GainRows {
    checkbox: Option<usize>,
    right: Option<usize>,
    tanh: usize,
    total: usize,
}

impl GainRows {
    fn new(is_stereo: bool, per_channel: bool) -> Self {
        let mut next = 1; // index 0 is always the Gain/Left field.
        let mut take = || {
            let row = next;
            next += 1;
            row
        };
        let right = (is_stereo && per_channel).then(&mut take);
        let checkbox = is_stereo.then(&mut take);
        let tanh = take();
        GainRows { checkbox, right, tanh, total: next }
    }
}

/// Which panel currently has focus — the single source of truth for the modal command
/// panel, contextual key handling, and the accent on the active panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Waveform,
    Files,
    Buffers,
}

/// How long the Files-panel selection must sit still on a file before Audition decodes
/// and plays it — long enough that arrowing quickly through a list doesn't trigger a
/// decode-and-play per keystroke, short enough to still feel immediate when browsing.
const AUDITION_DEBOUNCE: Duration = Duration::from_millis(200);

/// Clamp range for the Next Rising Edge transient threshold (`+`/`-`), in dB.
const TRANSIENT_THRESHOLD_MIN_DB: f32 = 1.0;
const TRANSIENT_THRESHOLD_MAX_DB: f32 = 24.0;

/// A pending y/n confirmation modal. Generalizes the old quit-only prompt so closing a
/// dirty buffer can reuse the same flow.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirm {
    Quit,
    CloseBuffer(usize),
    ResetConfig,
    /// Delete this file from disk (Files panel `Del`). Irreversible, hence the confirm.
    DeleteFile(PathBuf),
}

/// What to do once `App::save_as_queue` (buffers waiting for a filename before some other
/// action can proceed) is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveAsQueueThen {
    Quit,
    CloseBuffer(usize),
}


pub struct App {
    pub should_quit: bool,
    /// Set once at startup via `set_picker` (queried in `terminal::init`, which needs raw
    /// mode already enabled — before `App::new` runs) if the terminal supports a real
    /// image-graphics protocol (kitty, Sixel, or iTerm2's). `None` on any terminal that
    /// doesn't, including inside a detected multiplexer. Never re-queried after startup.
    /// Config-derived key dispatch map, built at startup from `Config.keybindings`.
    /// Consulted first in `handle_key`; `map_key` is the fallback for any key not in here.
    key_map: std::collections::HashMap<KeyEvent, Action>,
    picker: Option<ratatui_image::picker::Picker>,
    /// When true (and `picker` is `Some`), render the waveform via the detected graphics
    /// protocol instead of character glyphs. Persisted, defaults to `true` — see
    /// `Config.graphics_mode`. Toggled with `Action::ToggleGraphicsMode`.
    pub graphics_mode: bool,
    /// Per-channel graphics-mode image state, index-parallel to the active document's
    /// channels. Rebuilt fresh every frame from the live `viewport`/`selection`/`cursor`/
    /// `playhead` (via `Picker::new_resize_protocol`, the crate's intended way to swap in
    /// new image content — there's no in-place "update this image" method on
    /// `StatefulProtocol`), since the waveform's pixel content genuinely changes on
    /// essentially every redraw during scrolling/zooming/playback. Cleared whenever the
    /// channel count changes (e.g. switching to a document with a different channel
    /// count), so stale per-channel state from a previous document is never reused.
    graphics_protocols: Vec<ratatui_image::protocol::StatefulProtocol>,
    /// The CDP envelope editor's own graphics-mode protocol slot — deliberately separate
    /// from `graphics_protocols` (the per-channel waveform ones): the waveform's own
    /// graphics rendering is skipped entirely while any dialog is open (`overlay_active`,
    /// see the waveform's render block), so this never competes with it for a slot, and a
    /// stale image here is harmless (it's simply not drawn) rather than needing the same
    /// channel-count-driven `truncate` the waveform's Vec does.
    cdp_envelope_graphics_protocol: Option<ratatui_image::protocol::StatefulProtocol>,
    /// All open documents (buffers). Index 0 is always the first file loaded; subsequent
    /// entries are created by "Copy to New" or loading additional files.
    pub documents: Vec<Document>,
    /// Index into `documents` for the currently-active buffer.
    pub active_document: usize,
    pub viewport: Option<Viewport>,
    pub audio: Option<AudioEngine>,
    /// Sample rate the current audio engine was built with. The engine captures the rate at
    /// construction, so an operation that changes `Document.sample_rate` (resample, and its
    /// undo/redo) must rebuild the engine rather than just `reload` it.
    audio_sample_rate: Option<u32>,
    /// One undo/redo stack per open document, kept index-parallel to `documents`. Undo
    /// must never cross buffers — each `Command` stores sample data from the document it
    /// was applied to, so replaying it against a different document would corrupt it.
    pub histories: Vec<History>,
    pub clipboard: Clipboard,
    pub menu: MenuBar,
    pub toolbar: Toolbar,
    /// One precomputed min/max cache per channel, rebuilt whenever the document's sample
    /// data changes. Keeps waveform render cost bounded by screen width instead of file
    /// length — see `ui::waveform_cache`.
    pub waveform_caches: Vec<WaveformCache>,
    /// Width/area of the waveform content as of the last render; navigation/zoom/mouse
    /// actions need this and re-reading it from the terminal on every input would require
    /// a redraw, so it's cached here instead.
    pub content_width: u16,
    pub waveform_area: Rect,
    /// Rendered area of the Files panel, for mouse-click focus hit-testing.
    file_panel_area: Rect,
    /// Rendered area of the Buffers panel, for mouse-click focus hit-testing.
    buffer_panel_area: Rect,
    /// A pending y/n confirmation (quit, or closing a dirty buffer). Intercepts the next
    /// keypress as a confirmation instead of routing it through the normal keymap.
    confirm: Option<Confirm>,
    /// Sample position where the current mouse-down started (for drag-to-select).
    mouse_down_anchor: Option<usize>,
    /// Index of the marker currently being dragged with the mouse, if any.
    dragging_marker: Option<usize>,
    /// The dragged marker's position when the drag started, so the whole gesture (not each
    /// intermediate mouse-move) becomes a single undoable `MoveMarkerCommand` at drag-end.
    dragging_marker_start_position: Option<usize>,
    /// Rendered marker-label rects (label box + marker index) for mouse hit-testing.
    marker_label_rects: Vec<(Rect, usize)>,
    /// Time/cell of the last left mouse-down, used to detect double-clicks.
    last_click: Option<(Instant, u16, u16)>,
    /// Time/cell of the last left mouse-down *in the waveform background* (not on a marker
    /// label, which has its own double-click-to-rename handling via `last_click`) — used to
    /// detect a double-click that should select the region between adjacent markers.
    last_waveform_click: Option<(Instant, u16, u16)>,
    /// Time/cell of the last left mouse-down in the CDP envelope editor's grid — its own
    /// field (not `last_click`/`last_waveform_click`) since it's a distinct click-target
    /// class with its own double-click meaning (insert a point, not rename/select-region).
    last_cdp_envelope_click: Option<(Instant, u16, u16)>,
    /// Index into `CdpEnvelopeEdit.points` currently being dragged with the mouse, if any.
    dragging_cdp_point: Option<usize>,
    /// `(anchor_mouse_col, anchor_mouse_row, anchor_point_time, anchor_point_value)` captured
    /// when a drag starts, so `Shift`-drag can scale the *delta* from this anchor down for
    /// finer control instead of mapping the raw cursor position directly (which has no
    /// natural notion of "finer" since it's already continuous) — mirrors the coarse/fine
    /// split on keyboard Up/Down, just measured in mouse pixels instead of key presses.
    dragging_cdp_point_anchor: Option<(u16, u16, f64, f64)>,
    /// File panel on the left showing WAV files in the current directory.
    pub file_panel: FilePanel,
    /// Buffer panel showing all open documents.
    pub buffer_panel: BufferPanel,
    /// When true, the user is typing a Save-As path in a prompt overlay.
    pub save_as_active: bool,
    /// The Save-As filename field being edited.
    save_as_input: TextInput,
    /// Output bit depth for the pending Save As (Tab cycles it in the prompt).
    pub save_as_depth: BitDepth,
    /// Whether to dither the pending Save As (Space toggles when dither row focused).
    pub save_as_dither: bool,
    /// Which row has keyboard focus in the Save As dialog (0=filename, 1=format, 2=dither).
    save_as_focused: usize,
    /// Clickable row rects from the last dialog render, used for mouse hit-testing.
    dialog_row_rects: Vec<Rect>,
    dialog_n_interactive: usize,
    /// Buffer indices still waiting for a Save-As filename before `save_as_queue_then` can
    /// run — e.g. quitting with several never-saved buffers walks through one Save As
    /// prompt per buffer rather than silently skipping (and losing) them. Popped from the
    /// back, so it's pushed already reversed (see `queue_save_as`).
    save_as_queue: Vec<usize>,
    /// What to do once `save_as_queue` is empty. `None` means the current Save-As prompt
    /// (if any) is just a plain one-off, not part of a queued sequence.
    save_as_queue_then: Option<SaveAsQueueThen>,
    /// When true, destructive operations snap selection boundaries to zero crossings.
    pub snap_to_zero: bool,
    /// When true, playback loops — the full file if no selection, or the selection range.
    pub loop_playback: bool,
    /// When true, arrows (and Shift+arrows) move/extend by a single sample instead of a whole
    /// column. Toggled with `~` — a modifier-free fine-step mode, since every Ctrl/Alt+arrow
    /// combo is intercepted by some terminal or desktop before the app sees it.
    pub fine_mode: bool,
    /// When true, navigating to a file in the Files panel (Up/Down or a single click)
    /// previews it by playing straight from disk, without loading it into a buffer.
    /// Toggled with `p`.
    pub audition: bool,
    /// When true, pausing playback (Space while playing) snaps the insertion point to
    /// wherever playback stopped, scrolling it into view. Toggled with `i`.
    pub cursor_follows_playback: bool,
    /// When true, once the playhead reaches the right edge of the view during playback,
    /// the viewport recenters on it and keeps scrolling so the playhead stays in view for
    /// the rest of that playback run. Toggled with `f`.
    pub viewport_follows_playback: bool,
    /// Sticky flag: once the playhead has reached the right edge during the current
    /// playback run, the viewport keeps recentering on it every frame rather than waiting
    /// for the edge to be hit again (which would otherwise produce a jumpy step-scroll
    /// instead of a continuous one). Reset whenever playback stops.
    viewport_following: bool,
    /// dB threshold a frame's level must rise above the recent background by to count as a
    /// transient for "Next Rising Edge" (`/`). Adjusted with `+`/`-`, persisted.
    pub transient_threshold_db: f32,
    /// The audition playback engine, separate from `audio` (the active document's engine)
    /// since auditioning must not disturb whatever's actually loaded/playing. `None` when
    /// nothing is being auditioned.
    audition_audio: Option<AudioEngine>,
    /// Path of the file `audition_audio` is currently playing, if any.
    audition_playing_path: Option<PathBuf>,
    /// A file waiting to start auditioning once `AUDITION_DEBOUNCE` has elapsed since the
    /// selection landed on it — avoids decoding/playing every file the user arrows past
    /// while skimming the list quickly.
    audition_pending: Option<(PathBuf, Instant)>,
    /// Time/cell of the last left mouse-down on a file-panel entry, used to detect a
    /// double-click (which opens the file) versus a single click (which only selects it,
    /// auditioning it if Audition is on).
    last_file_click: Option<(Instant, u16, u16)>,
    /// Persisted toggles, loaded at startup and rewritten whenever one changes. The
    /// snapshot here is what gets written to disk — see `save_config`.
    config: Config,
    /// The nav action currently building up a fast-repeat streak (see `nav_step_multiplier`).
    nav_hold_action: Option<Action>,
    /// How many consecutive repeats of `nav_hold_action` have landed less than
    /// `NAV_FAST_REPEAT_GAP` apart. This — not elapsed wall-clock time — is what
    /// acceleration ramps on, specifically because elapsed time can't tell a held key from
    /// someone tapping it steadily for a while: both rack up the same wall-clock duration.
    /// A tight per-event gap requirement is what only a genuine hold (terminal auto-repeat
    /// fires every ~20-50ms) can sustain for many consecutive events; manual tapping can't.
    nav_repeat_count: u32,
    /// Time of the most recent nav-step keypress, used to measure the gap to the next one.
    last_nav_time: Option<Instant>,
    /// Active parameter dialog (Normalize, Gain, or one of the CDP dialogs), if any.
    dialog: Option<Dialog>,
    /// The current playback position, set from `AudioEngine.position` during playback.
    /// `None` when playback is stopped. This is the visual playhead only — the cursor
    /// (insertion point) lives on `Document.cursor`.
    playhead_position: Option<usize>,
    /// CDP process catalog, loaded once at startup (built-ins plus any user-authored
    /// `$XDG_CONFIG_HOME/tui-wave/cdp/*.toml` overrides/additions).
    cdp_catalog: crate::model::cdp::CdpCatalog,
    /// Parse warnings from loading `cdp_catalog`'s user-directory files, surfaced once via
    /// an `Info` dialog on startup rather than blocking it (mirrors `Config::load`).
    cdp_catalog_warnings: Vec<String>,
    /// Background worker for running CDP binaries — see `cdp::runner`. Always present
    /// (spawning its thread can't fail); only one job is ever in flight at a time in v1
    /// since `Dialog::CdpRunning` is hard-modal.
    cdp_runner: crate::cdp::CdpRunner,
    /// Monotonic id for the next submitted `cdp::Job`, so a `CdpEvent` can be matched back
    /// to the run that produced it.
    cdp_next_job_id: u64,
    /// Context for the in-flight CDP job, stashed at submit time so `tick_cdp` knows what
    /// to do with the result: which document/range to splice into (Apply) or audition
    /// (Preview), and the label for the eventual undo entry.
    cdp_pending: Option<CdpPending>,
    /// Second, independent audio engine for auditioning a `Preview` result without
    /// disturbing whatever's loaded/playing — mirrors `audition_audio`.
    cdp_preview_audio: Option<AudioEngine>,
}

/// Context for the CDP job currently running in `cdp_runner`, stashed when it's submitted
/// so `App::tick_cdp` knows what to do once the matching `CdpEvent::Finished` arrives.
/// `catalog_index`/`fields`/`second_input`/`focus`/`presets`/`preset_selected` are only
/// needed for a `Preview` job: the dialog is `Dialog::CdpRunning` (not `CdpParams`) while
/// the job is in flight, so this is the only place that state survives long enough to
/// rebuild `CdpParams` (with the new preview attached) once the job completes.
struct CdpPending {
    doc_index: usize,
    range: (usize, usize),
    label: String,
    catalog_index: usize,
    fields: Vec<CdpField>,
    second_input: Option<CdpSecondInput>,
    focus: usize,
    presets: Vec<crate::model::cdp::preset::CdpPreset>,
    preset_selected: Option<usize>,
}

impl App {
    pub fn new(document: Option<Document>, directory: Option<PathBuf>) -> Self {
        let mut app = Self::new_with_config(document, directory, Config::load());
        // Write the merged config on every launch: creates the file on first launch so
        // all keybindings are immediately visible, and on subsequent launches after an
        // upgrade appends any newly-added default bindings to the existing file without
        // touching the user's custom entries (fill_missing_keybindings only inserts).
        app.config.save();
        // A malformed user CDP catalog file shouldn't be silently swallowed (the user would
        // otherwise have no way to discover why a hand-authored process definition never
        // showed up in the browser), but it also shouldn't block startup — surfaced once,
        // here, rather than failing to load.
        if !app.cdp_catalog_warnings.is_empty() {
            app.dialog = Some(Dialog::Info {
                message: format!("CDP catalog warnings:\n{}", app.cdp_catalog_warnings.join("\n")),
            });
        }
        app
    }

    /// Sets the graphics-protocol capability detected by `terminal::init()` — called once
    /// from `main` right after construction, since the detection query itself needs raw
    /// mode already enabled (done in `terminal::init`, which runs before `App::new`).
    pub fn set_picker(&mut self, picker: Option<ratatui_image::picker::Picker>) {
        self.picker = picker;
    }

    /// The real constructor body, parameterized on `Config` so tests can pass
    /// `Config::default()` instead of `Config::load()` — tests must never depend on
    /// whatever happens to be in the user's real `~/.config/tui-wave/config.toml` (or race
    /// against other tests that temporarily redirect `XDG_CONFIG_HOME`).
    fn new_with_config(document: Option<Document>, directory: Option<PathBuf>, mut config: Config) -> Self {
        fill_missing_keybindings(&mut config.keybindings);
        let key_map = build_key_map(&config.keybindings);

        let dir = directory
            .or_else(|| document.as_ref().and_then(|d| d.path.as_ref()).and_then(|p| p.parent().map(|p| p.to_path_buf())))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let mut file_panel = FilePanel::new(dir);
        // The Files panel starts focused so the first thing a user does is pick a file to
        // load, rather than landing on an empty waveform view with nothing to act on.
        file_panel.focused = true;

        let documents = match document {
            Some(doc) => vec![doc],
            None => Vec::new(),
        };
        let audio = documents.first()
            .and_then(|doc| AudioEngine::try_new(doc.channels.clone(), doc.sample_rate));
        let audio_sample_rate = documents.first().map(|doc| doc.sample_rate);
        let waveform_caches = documents.first()
            .map(|doc| doc.channels.iter().map(|c| WaveformCache::build(c)).collect())
            .unwrap_or_default();
        let histories = documents.iter().map(|_| History::new()).collect();
        let menu_shortcuts = build_action_display_map(&config.keybindings, false);
        let toolbar_shortcuts = build_action_display_map(&config.keybindings, true);
        let user_cdp_dir = crate::model::cdp::CdpCatalog::user_dir();
        let (cdp_catalog, cdp_catalog_warnings) =
            crate::model::cdp::CdpCatalog::load(Some(&user_cdp_dir));
        Self {
            should_quit: false,
            key_map,
            picker: None,
            graphics_mode: config.graphics_mode,
            graphics_protocols: Vec::new(),
            cdp_envelope_graphics_protocol: None,
            documents,
            active_document: 0,
            viewport: None,
            audio,
            audio_sample_rate,
            histories,
            clipboard: Clipboard::default(),
            menu: MenuBar::new(&menu_shortcuts),
            toolbar: Toolbar::new(&toolbar_shortcuts),
            waveform_caches,
            content_width: 1,
            waveform_area: Rect::default(),
            file_panel_area: Rect::default(),
            buffer_panel_area: Rect::default(),
            confirm: None,
            mouse_down_anchor: None,
            dragging_marker: None,
            dragging_marker_start_position: None,
            marker_label_rects: Vec::new(),
            last_click: None,
            last_waveform_click: None,
            last_cdp_envelope_click: None,
            dragging_cdp_point: None,
            dragging_cdp_point_anchor: None,
            file_panel,
            buffer_panel: BufferPanel::new(),
            save_as_active: false,
            save_as_input: TextInput::new(""),
            save_as_depth: BitDepth::Float32,
            save_as_dither: false,
            save_as_focused: 0,
            dialog_row_rects: Vec::new(),
            dialog_n_interactive: 0,
            save_as_queue: Vec::new(),
            save_as_queue_then: None,
            snap_to_zero: config.snap_to_zero,
            loop_playback: config.loop_playback,
            fine_mode: config.fine_mode,
            audition: config.audition,
            cursor_follows_playback: config.cursor_follows_playback,
            viewport_follows_playback: config.viewport_follows_playback,
            viewport_following: false,
            transient_threshold_db: config.transient_threshold_db,
            audition_audio: None,
            audition_playing_path: None,
            audition_pending: None,
            last_file_click: None,
            config,
            nav_hold_action: None,
            nav_repeat_count: 0,
            last_nav_time: None,
            dialog: None,
            playhead_position: None,
            cdp_catalog,
            cdp_catalog_warnings,
            cdp_runner: crate::cdp::CdpRunner::new(),
            cdp_next_job_id: 0,
            cdp_pending: None,
            cdp_preview_audio: None,
        }
    }

    fn active_doc(&self) -> Option<&Document> {
        self.documents.get(self.active_document)
    }

    fn active_doc_mut(&mut self) -> Option<&mut Document> {
        self.documents.get_mut(self.active_document)
    }

    /// Pushes a freshly-opened document and its (empty) history, keeping the two vecs
    /// index-parallel, and makes it the active buffer.
    fn push_document(&mut self, document: Document) {
        self.documents.push(document);
        self.histories.push(History::new());
        self.active_document = self.documents.len() - 1;
    }

    /// Display name for buffer `idx`: its file name, or `_NEW_NNN` for a never-saved buffer
    /// (NNN is its 1-based position). No dirty marker — callers add that where appropriate.
    /// Shared by the Buffers panel and the waveform header so the two can't drift apart.
    fn buffer_name(&self, idx: usize) -> String {
        match self.documents.get(idx).and_then(|d| d.path.as_ref()) {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "untitled".to_string()),
            None => format!("_NEW_{:03}", idx + 1),
        }
    }

    fn buffer_names(&self) -> Vec<String> {
        (0..self.documents.len())
            .map(|i| {
                let prefix = if self.documents[i].dirty { "*" } else { "" };
                format!("{}{}", prefix, self.buffer_name(i))
            })
            .collect()
    }

    fn switch_to_buffer(&mut self, index: usize) {
        if index >= self.documents.len() || index == self.active_document {
            return;
        }
        self.active_document = index;
        if self.active_doc().is_some() {
            self.rebuild_audio();
            self.rebuild_waveform_caches();
            self.viewport = None;
        }
    }

    /// Document indices whose buffer name passes the Buffers-panel search filter.
    fn filtered_buffer_indices(&self) -> Vec<usize> {
        let names = self.buffer_names();
        (0..names.len())
            .filter(|&i| self.buffer_panel.matches(&names[i]))
            .collect()
    }

    /// Moves the Buffers-panel selection cursor by `delta` within the filtered subset.
    fn move_buffer_selection(&mut self, delta: isize) {
        let filtered = self.filtered_buffer_indices();
        if filtered.is_empty() {
            return;
        }
        let cur = filtered
            .iter()
            .position(|&i| i == self.buffer_panel.selected)
            .unwrap_or(0);
        let next = (cur as isize + delta).clamp(0, filtered.len() as isize - 1) as usize;
        self.buffer_panel.selected = filtered[next];
        // Navigating to a buffer loads it immediately — like the mouse click handler
        // already does — so Up/Down previews audio without a separate Enter to commit.
        self.switch_to_buffer(self.buffer_panel.selected);
    }

    /// After the buffer filter changes, keep the selection on a still-visible buffer.
    fn snap_buffer_selection_to_filter(&mut self) {
        let filtered = self.filtered_buffer_indices();
        if !filtered.contains(&self.buffer_panel.selected) {
            self.buffer_panel.selected = filtered.first().copied().unwrap_or(0);
        }
    }

    /// Returns the playback loop range: the current selection if one exists, or the full
    /// document if nothing is selected. Returns `None` when loop playback is disabled.
    fn loop_range(&self) -> Option<(usize, usize)> {
        if !self.loop_playback {
            return None;
        }
        self.active_doc().map(|doc| {
            doc.selection
                .map(|sel| sel.normalized())
                .unwrap_or((0, doc.len_samples()))
        })
    }

    /// Highest peak within the current visible window. Computed from the precomputed cache
    /// so it's cheap enough to call every frame.
    fn visible_peak(&self) -> f32 {
        visible_peak_raw(
            self.active_doc(),
            self.viewport.as_ref(),
            &self.waveform_caches,
            self.content_width,
        )
    }

    fn rebuild_waveform_caches(&mut self) {
        self.waveform_caches = self
            .active_doc()
            .map(|doc| doc.channels.iter().map(|c| WaveformCache::build(c)).collect())
            .unwrap_or_default();
    }

    pub fn run(&mut self, terminal: &mut Tui) -> color_eyre::Result<()> {
        // Redraw only when something actually changed, rather than unconditionally every
        // ~60fps tick. When idle the loop still wakes every 16ms (to poll input and advance
        // the playhead), but skips the expensive `render` — which matters most in graphics
        // mode, where every draw re-rasterizes the waveform bitmap and re-transmits it to the
        // terminal (there is no cell-diffing for the embedded image; see `render`). A redraw
        // is requested on: the initial frame, ANY input event (keys, mouse, and crucially
        // resize/focus/paste via the catch-all arm — a resize previously repainted only
        // because the draw was unconditional), and a change to the playhead position (which
        // is what animates playback; `tick_viewport_follow` only scrolls while the playhead
        // is advancing, so it is covered by the same signal). `tick_audition` mutates only
        // off-screen audio state and so deliberately does not request a redraw.
        let mut needs_redraw = true;
        while !self.should_quit {
            if needs_redraw {
                terminal.draw(|frame| self.render(frame))?;
                needs_redraw = false;
            }
            if event::poll(Duration::from_millis(16))? {
                needs_redraw = true;
                match event::read()? {
                    Event::Key(key) => self.handle_key(key),
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    _ => {}
                }
                // Drain any additional events that arrived while rendering. Without this,
                // a slow render (e.g. kitty graphics transmission on macOS) lets key-repeat
                // events queue up faster than they're consumed, so the cursor keeps moving
                // for a visible moment after the key is released. Draining here means every
                // queued event is processed before the next expensive redraw, and the cursor
                // stops the instant the queue empties (i.e. as soon as release takes effect).
                while !self.should_quit && event::poll(Duration::from_millis(0))? {
                    match event::read()? {
                        Event::Key(key) => self.handle_key(key),
                        Event::Mouse(mouse) => self.handle_mouse(mouse),
                        _ => {}
                    }
                }
            }
            let playhead_before = self.playhead_position;
            self.sync_playhead_from_audio();
            self.tick_audition();
            // A drained CDP event may have replaced/closed the dialog (job finished) with
            // no input event to trigger the repaint — without this the finished dialog
            // sits stale on screen until the next keypress.
            if self.tick_cdp() {
                needs_redraw = true;
            }
            self.tick_viewport_follow();
            if self.playhead_position != playhead_before {
                needs_redraw = true;
            }
            // The `CdpRunning` spinner and step label are time-/event-driven, not input-
            // driven, so (unlike the rest of this idle tick) they need an unconditional
            // redraw every loop iteration to actually animate while the user isn't typing.
            if matches!(self.dialog, Some(Dialog::CdpRunning { .. })) {
                needs_redraw = true;
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.confirm.is_some() {
            self.handle_confirm_key(key);
            return;
        }
        if self.menu.is_open() {
            self.handle_menu_key(key);
            return;
        }
        if self.save_as_active {
            self.handle_save_as_key(key);
            return;
        }
        if self.dialog.is_some() {
            self.handle_dialog_key(key);
            return;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(c) = key.code {
                if self.menu.open_by_mnemonic(c) {
                    return;
                }
            }
        }
        if key.code == KeyCode::F(10) {
            self.menu.open_first();
            return;
        }
        // File panel filtering — arrows still navigate the filtered sublist.
        if self.file_panel.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.file_panel.filtering = false;
                    self.file_panel.filter.clear();
                }
                KeyCode::Enter => {
                    self.open_selected_file();
                }
                KeyCode::Up => self.file_panel.move_up(),
                KeyCode::Down => self.file_panel.move_down(),
                KeyCode::Home => self.file_panel.move_top(),
                KeyCode::End => self.file_panel.move_bottom(),
                KeyCode::PageUp => self.file_panel.move_page_up(),
                KeyCode::PageDown => self.file_panel.move_page_down(),
                KeyCode::Backspace => {
                    self.file_panel.filter.pop();
                    self.file_panel.selected = 0;
                }
                KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                    self.file_panel.filter.push(c);
                    self.file_panel.selected = 0;
                }
                _ => {}
            }
            return;
        }
        // File panel keyboard focus
        if self.file_panel.focused {
            let handled = match key.code {
                KeyCode::Up => { self.file_panel.move_up(); true }
                KeyCode::Down => { self.file_panel.move_down(); true }
                KeyCode::Home => { self.file_panel.move_top(); true }
                KeyCode::End => { self.file_panel.move_bottom(); true }
                KeyCode::PageUp => { self.file_panel.move_page_up(); true }
                KeyCode::PageDown => { self.file_panel.move_page_down(); true }
                KeyCode::Enter => { self.open_selected_file(); true }
                KeyCode::Char('/') => {
                    self.file_panel.filtering = true;
                    self.file_panel.filter.clear();
                    true
                }
                // ^o opens the directory dialog (in waveform focus ^o is Fade Out).
                KeyCode::Char('o') | KeyCode::Char('O')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.handle_action(Action::OpenDirectory);
                    true
                }
                // Plain 'a' toggles Audition here (in waveform focus, plain 'a' is Auto
                // Vertical Zoom instead) — the same contextual-override pattern as ^o above.
                KeyCode::Char('a') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.handle_action(Action::ToggleAudition);
                    true
                }
                // ^r renames the selected file on disk, Del deletes it (both no-ops unless a
                // .wav row is selected). Contextual to the Files panel — in waveform focus ^r
                // is Reverse and Del is Delete-selection.
                KeyCode::Char('r') | KeyCode::Char('R')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.handle_action(Action::RenameFile);
                    true
                }
                KeyCode::Delete => {
                    self.handle_action(Action::DeleteFile);
                    true
                }
                // Shift+Tab cycles backward (Files → Waveform); Tab forward (Files → Buffers).
                // Shift+Tab arrives as BackTab on legacy terminals, or Tab+SHIFT under the
                // kitty keyboard protocol — accept both.
                KeyCode::BackTab => { self.file_panel.focused = false; true }
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.file_panel.focused = false;
                    true
                }
                KeyCode::Tab => {
                    self.file_panel.focused = false;
                    self.buffer_panel.focused = true;
                    true
                }
                KeyCode::Esc => { self.file_panel.focused = false; true }
                _ => false,
            };
            if handled {
                return;
            }
        }
        // Buffer panel filtering — arrows still navigate the filtered sublist.
        if self.buffer_panel.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.buffer_panel.filtering = false;
                    self.buffer_panel.filter.clear();
                }
                KeyCode::Enter => {
                    self.handle_action(Action::SwitchBuffer);
                    self.buffer_panel.filtering = false;
                    self.buffer_panel.filter.clear();
                    self.buffer_panel.focused = false;
                }
                KeyCode::Up => self.move_buffer_selection(-1),
                KeyCode::Down => self.move_buffer_selection(1),
                KeyCode::Backspace => {
                    self.buffer_panel.filter.pop();
                    self.snap_buffer_selection_to_filter();
                }
                KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                    self.buffer_panel.filter.push(c);
                    self.snap_buffer_selection_to_filter();
                }
                _ => {}
            }
            return;
        }
        // Buffer panel keyboard focus
        if self.buffer_panel.focused {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let handled = match key.code {
                // Up/Dn move the selection cursor and immediately switch to it (loading the
                // buffer's audio as you navigate); Enter is a no-op once already switched,
                // kept as a binding so it still does something sensible if pressed.
                KeyCode::Up => { self.move_buffer_selection(-1); true }
                KeyCode::Down => { self.move_buffer_selection(1); true }
                // Enter commits the selection and hands focus to the waveform, since picking
                // a buffer is almost always followed by editing it (unlike the Files panel's
                // Enter, which stays focused so browsing to open several files in a row
                // doesn't require re-focusing between each one).
                KeyCode::Enter => {
                    self.handle_action(Action::SwitchBuffer);
                    self.buffer_panel.focused = false;
                    true
                }
                KeyCode::Char('/') => { self.handle_action(Action::SearchBuffers); true }
                // Contextual buffer commands (^r/^a differ from the global Reverse/SaveAll).
                KeyCode::Char('s') | KeyCode::Char('S') if ctrl => { self.handle_action(Action::Save); true }
                KeyCode::Char('w') | KeyCode::Char('W') if ctrl => { self.handle_action(Action::CloseBuffer); true }
                KeyCode::Char('r') | KeyCode::Char('R') if ctrl => { self.handle_action(Action::RenameBuffer); true }
                KeyCode::Char('a') | KeyCode::Char('A') if ctrl => { self.handle_action(Action::SaveAll); true }
                // Shift+Tab cycles backward (Buffers → Files); Tab forward (Buffers → Waveform).
                KeyCode::BackTab => { self.buffer_panel.focused = false; self.file_panel.focused = true; true }
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.buffer_panel.focused = false;
                    self.file_panel.focused = true;
                    true
                }
                KeyCode::Tab => { self.buffer_panel.focused = false; true }
                KeyCode::Esc => { self.buffer_panel.focused = false; true }
                _ => false,
            };
            if handled {
                return;
            }
        }
        // Tab when nothing is focused → focus the file panel (forward); Shift+Tab → the buffer
        // panel (backward), so the reverse cycle is Waveform → Buffers → Files → Waveform.
        if key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
        {
            self.buffer_panel.focused = true;
            return;
        }
        if key.code == KeyCode::Tab {
            self.file_panel.focused = true;
            return;
        }
        // Normalise kind/state before lookup so key-repeat events hit the same entry as
        // initial presses (map_key already ignores kind/state in its match arms).
        let normalised = KeyEvent {
            code: key.code,
            modifiers: key.modifiers,
            kind: ratatui::crossterm::event::KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        };
        let action = self.key_map.get(&normalised).copied().or_else(|| map_key(key));
        if let Some(action) = action {
            self.handle_action(action);
        }
    }

    fn handle_menu_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.menu.move_left(),
            KeyCode::Right => self.menu.move_right(),
            KeyCode::Up => self.menu.move_up(),
            KeyCode::Down => self.menu.move_down(),
            KeyCode::Enter => {
                if let Some(action) = self.menu.activate() {
                    self.handle_action(action);
                }
            }
            KeyCode::Esc => self.menu.close(),
            _ => {}
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) {
        if self.confirm.is_none() {
            return;
        }
        let save = matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'));
        let proceed = save || matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y'));
        if !proceed {
            // Any other key cancels.
            self.confirm = None;
            return;
        }
        let Some(confirm) = self.confirm.take() else { return };
        match confirm {
            Confirm::Quit => {
                // (s)ave saves every dirty buffer with a path first, then walks any
                // never-saved ones through a Save As prompt each before actually quitting;
                // (y) quits regardless, discarding unsaved changes.
                if save {
                    self.begin_save_all_then_quit();
                } else {
                    self.should_quit = true;
                }
            }
            Confirm::CloseBuffer(idx) => {
                if save {
                    if self.documents.get(idx).is_some_and(|d| d.path.is_none()) {
                        // Never saved — needs a filename before it can actually be saved,
                        // so defer closing until that Save As prompt is done.
                        self.queue_save_as(vec![idx], SaveAsQueueThen::CloseBuffer(idx));
                        return;
                    }
                    self.save_buffer(idx);
                }
                self.close_buffer(idx);
            }
            Confirm::ResetConfig => {
                self.reset_config_to_defaults();
            }
            Confirm::DeleteFile(path) => {
                self.delete_file(&path);
            }
        }
    }

    fn handle_save_as_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                // Ensure a .wav extension before resolving the path.
                let name = ensure_wav_extension(self.save_as_input.value().trim());
                if !name.is_empty() {
                    let path = PathBuf::from(&name);
                    let path = if path.is_absolute() {
                        path
                    } else {
                        self.file_panel.directory.join(&name)
                    };
                    let depth = self.save_as_depth;
                    let dither = self.save_as_dither && depth.supports_dither();
                    if let Some(document) = self.active_doc_mut() {
                        if save_wav_with(document, &path, depth, dither).is_ok() {
                            document.path = Some(path.clone());
                            document.dirty = false;
                            self.file_panel.mark_dirty(&path, false);
                            self.file_panel.scan();
                        }
                    }
                }
                // Plain one-off Save As (no pending queue) just closes; mid-queue, this
                // moves on to the next never-saved buffer, or finishes (e.g. actually quits).
                self.advance_save_as_queue();
            }
            KeyCode::Esc => {
                // Backing out cancels the whole pending sequence, not just this one buffer
                // — if the user meant to quit/close anyway, (y)/(s) without saving is right
                // there in the confirmation that started this.
                self.save_as_active = false;
                self.save_as_queue.clear();
                self.save_as_queue_then = None;
            }
            // Tab cycles focus forward (0=filename, 1=format, 2=dither); Shift+Tab backward.
            KeyCode::BackTab => self.save_as_focused = (self.save_as_focused + 2) % 3,
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.save_as_focused = (self.save_as_focused + 2) % 3
            }
            KeyCode::Tab => self.save_as_focused = (self.save_as_focused + 1) % 3,
            KeyCode::Char(' ') => {
                if self.save_as_focused == 2 && self.save_as_depth.supports_dither() {
                    self.save_as_dither = !self.save_as_dither;
                }
            }
            KeyCode::Left => match self.save_as_focused {
                0 => self.save_as_input.left(),
                1 => self.save_as_depth = self.save_as_depth.prev(),
                _ => {}
            },
            KeyCode::Right => match self.save_as_focused {
                0 => self.save_as_input.right(),
                1 => self.save_as_depth = self.save_as_depth.next(),
                _ => {}
            },
            KeyCode::Home => self.save_as_input.home(),
            KeyCode::End => self.save_as_input.end(),
            KeyCode::Backspace => self.save_as_input.backspace(),
            KeyCode::Delete => self.save_as_input.delete(),
            KeyCode::Char(c) if self.save_as_focused == 0
                && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.save_as_input.insert(c);
            }
            _ => {}
        }
    }

    /// `&mut TextInput` for the active dialog, if it's a text-bearing one.
    fn dialog_input(&mut self) -> Option<&mut TextInput> {
        match self.dialog.as_mut()? {
            Dialog::Normalize { input }
            | Dialog::Resample { input, .. }
            | Dialog::RenameMarker { input, .. }
            | Dialog::OpenDirectory { input }
            | Dialog::RenameBuffer { input, .. }
            | Dialog::RenameFile { input, .. } => Some(input),
            Dialog::Gain { input, right_input, focused, per_channel, is_stereo, .. } => {
                let rows = GainRows::new(*is_stereo, *per_channel);
                let f = *focused;
                if f == 0 {
                    Some(input)
                } else if Some(f) == rows.right {
                    Some(right_input)
                } else {
                    None
                }
            }
            Dialog::FadeIn { .. } | Dialog::FadeOut { .. } => None,
            Dialog::MixToMono { inputs, focused, .. } => {
                let f = *focused;
                inputs.get_mut(f)
            }
            Dialog::ExportRegions {
                folder_input, base_name_input, limit_length_input, normalize_input,
                fade_in_input, fade_out_input, focused, ..
            } => {
                match *focused {
                    er_focus::SUBFOLDER => Some(folder_input),
                    er_focus::BASE_NAME => Some(base_name_input),
                    er_focus::LIMIT_MS => Some(limit_length_input),
                    er_focus::NORMALIZE_DB => Some(normalize_input),
                    er_focus::FADE_IN_MS => Some(fade_in_input),
                    er_focus::FADE_OUT_MS => Some(fade_out_input),
                    _ => None,
                }
            }
            Dialog::CdpSetup { input, .. } => Some(input),
            Dialog::CdpBrowser { search, .. } => Some(search),
            Dialog::CdpParams { save_prompt: Some(input), .. } => Some(input),
            Dialog::CdpParams { fields, focus, .. } => {
                if *focus == CDP_PRESET_FOCUS {
                    None
                } else {
                    match fields.get_mut(*focus - 1) {
                        Some(CdpField::Number { input, .. }) => Some(input),
                        _ => None,
                    }
                }
            }
            Dialog::CdpRunning { .. } | Dialog::CdpOutput { .. } => None,
            Dialog::Info { .. } => None,
        }
    }

    /// Whether a typed `c` is accepted by the active dialog (numeric dialogs restrict input).
    fn dialog_accepts(&self, c: char) -> bool {
        match &self.dialog {
            Some(Dialog::Normalize { .. }) | Some(Dialog::Gain { .. }) => {
                c.is_ascii_digit() || c == '-' || c == '.'
            }
            Some(Dialog::Resample { .. }) => c.is_ascii_digit(),
            Some(Dialog::MixToMono { .. }) => {
                c.is_ascii_digit() || matches!(c, '-' | '.' | 'i' | 'n' | 'f')
            }
            Some(Dialog::ExportRegions { focused, .. }) => {
                match *focused {
                    // ms fields: no sign
                    er_focus::LIMIT_MS | er_focus::FADE_IN_MS | er_focus::FADE_OUT_MS => {
                        c.is_ascii_digit() || c == '.'
                    }
                    // dB: can be negative
                    er_focus::NORMALIZE_DB => c.is_ascii_digit() || c == '-' || c == '.',
                    _ => true, // folder / base name: free text
                }
            }
            Some(Dialog::CdpParams { save_prompt: Some(_), .. }) => true, // preset name: free text
            Some(Dialog::CdpParams { fields, focus, .. }) if *focus != CDP_PRESET_FOCUS => {
                match fields.get(*focus - 1) {
                    Some(CdpField::Number { .. }) => c.is_ascii_digit() || c == '-' || c == '.',
                    // Toggle/Choice fields aren't typed into, and the trailing
                    // second-input/Preview/Apply pseudo-fields accept nothing either.
                    _ => false,
                }
            }
            // CdpParams with focus == CDP_PRESET_FOCUS: the preset row has no free-text
            // field of its own ('s'/'d' are dedicated key arms in handle_dialog_key,
            // checked before the generic accepts-a-char path this function gates).
            Some(Dialog::CdpParams { .. }) => false,
            // CdpBrowser is always search-typable regardless of focus: falls through to the
            // catch-all below.
            Some(Dialog::CdpRunning { .. }) | Some(Dialog::CdpOutput { .. }) => false,
            Some(Dialog::Info { .. }) => false,
            _ => true, // rename / directory / CDP setup / CDP search: free text
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) {
        if let Some(Dialog::CdpParams { envelope: Some(_), .. }) = &self.dialog {
            self.handle_cdp_envelope_key(key);
            return;
        }
        if let Some(Dialog::CdpParams { list_edit: Some(_), .. }) = &self.dialog {
            self.handle_cdp_list_key(key);
            return;
        }
        if let Some(Dialog::CdpParams { save_prompt: Some(_), .. }) = &self.dialog {
            self.handle_cdp_preset_save_prompt_key(key);
            return;
        }
        match key.code {
            KeyCode::Enter => match self.dialog.take() {
                Some(Dialog::Normalize { input }) => {
                    let db = input.value().parse::<f32>().unwrap_or(-1.0).min(0.0);
                    self.apply_normalize(db);
                }
                Some(Dialog::Gain { input, right_input, tanh_clip, per_channel, is_stereo, .. }) => {
                    let gains = if is_stereo && per_channel {
                        let left = input.value().parse::<f32>().unwrap_or(0.0);
                        let right = right_input.value().parse::<f32>().unwrap_or(0.0);
                        vec![left, right]
                    } else {
                        let db = input.value().parse::<f32>().unwrap_or(0.0);
                        let n_channels = self.active_doc().map(|d| d.channels.len()).unwrap_or(1).max(1);
                        vec![db; n_channels]
                    };
                    self.apply_gain(gains, tanh_clip);
                }
                Some(Dialog::FadeIn { curve }) => self.apply_fade(true, 100.0, curve),
                Some(Dialog::FadeOut { curve }) => self.apply_fade(false, 100.0, curve),
                Some(Dialog::Resample { input, current_rate }) => {
                    let rate = input.value().trim().parse::<u32>().unwrap_or(current_rate);
                    self.apply_resample(rate);
                }
                Some(Dialog::RenameMarker { position, input }) => {
                    let idx = self.active_document;
                    if let Some(document) = self.documents.get_mut(idx) {
                        let new_label = input.value().to_string();
                        self.histories[idx].apply(rename_marker_command(position, new_label), document);
                        if let Some(path) = document.path.clone() {
                            self.file_panel.mark_dirty(&path, true);
                        }
                    }
                }
                Some(Dialog::OpenDirectory { input }) => self.open_directory(input.value()),
                Some(Dialog::RenameBuffer { index, input }) => {
                    self.rename_buffer(index, &ensure_wav_extension(input.value().trim()));
                }
                Some(Dialog::RenameFile { path, input }) => {
                    self.rename_file(&path, &ensure_wav_extension(input.value().trim()));
                }
                Some(Dialog::MixToMono { inputs, tanh_clip, .. }) => {
                    let inputs_snapshot = inputs.clone();
                    let clip = tanh_clip;
                    self.apply_mix_to_mono(&inputs_snapshot, clip);
                }
                Some(Dialog::ExportRegions {
                    folder_input, base_name_input, depth, dither,
                    limit_length, limit_length_input, normalize, normalize_input,
                    fade_in, fade_in_input, fade_out, fade_out_input, focused,
                }) => {
                    let folder = folder_input.value().trim().to_string();
                    let base_name = base_name_input.value().trim().to_string();
                    let parse = |input: &TextInput| input.value().trim().parse::<f32>().ok();
                    let limit_ms = parse(&limit_length_input).map(|v| v.max(0.0));
                    let norm_db = parse(&normalize_input).map(|v| v.min(0.0));
                    let fi_ms = parse(&fade_in_input).map(|v| v.max(0.0));
                    let fo_ms = parse(&fade_out_input).map(|v| v.max(0.0));
                    // A checked option whose value field is empty/unparseable (or a limit
                    // of 0 ms) blocks the submit and focuses the offending field. Silent
                    // fallbacks here were destructive: a blank Normalize field used to
                    // become 0 dBFS (boost every region to full scale), and a blank limit
                    // silently disabled a cap the checkbox said was on.
                    let invalid_field = if limit_length && !limit_ms.is_some_and(|v| v > 0.0) {
                        Some(er_focus::LIMIT_MS)
                    } else if normalize && norm_db.is_none() {
                        Some(er_focus::NORMALIZE_DB)
                    } else if fade_in && fi_ms.is_none() {
                        Some(er_focus::FADE_IN_MS)
                    } else if fade_out && fo_ms.is_none() {
                        Some(er_focus::FADE_OUT_MS)
                    } else {
                        None
                    };
                    // "Do!" is inactive until both the subfolder and base name are filled —
                    // a blank either leaves the dialog open (re-created unchanged) and does
                    // nothing, matching the dimmed Enter hint the dialog already renders.
                    if folder.is_empty() || base_name.is_empty() || invalid_field.is_some() {
                        self.dialog = Some(Dialog::ExportRegions {
                            folder_input, base_name_input, depth, dither,
                            limit_length, limit_length_input, normalize, normalize_input,
                            fade_in, fade_in_input, fade_out, fade_out_input,
                            focused: invalid_field.unwrap_or(focused),
                        });
                        return;
                    }
                    self.export_regions(
                        &folder, &base_name, depth, dither,
                        RegionExportOptions {
                            limit_length_ms: limit_ms.filter(|_| limit_length),
                            normalize_db: norm_db.filter(|_| normalize),
                            fade_in_ms: fi_ms.filter(|_| fade_in),
                            fade_out_ms: fo_ms.filter(|_| fade_out),
                        },
                    );
                }
                Some(Dialog::CdpSetup { input, .. }) => {
                    self.confirm_cdp_setup(input.value().to_string());
                }
                Some(dlg @ Dialog::CdpBrowser { .. }) => {
                    // No matches: nothing to open, just leave the dialog as-is. Opens the
                    // highlighted process regardless of `group_focus` — only the highlighted
                    // *process* row matters here, not which column currently has focus.
                    let Dialog::CdpBrowser { entries, selected, .. } = &dlg else { unreachable!() };
                    let catalog_index = entries.get(*selected).copied();
                    self.dialog = Some(dlg);
                    if let Some(catalog_index) = catalog_index {
                        self.open_cdp_params(catalog_index);
                    }
                }
                Some(Dialog::CdpParams {
                    catalog_index, fields, second_input, focus, error, preview, envelope, list_edit,
                    presets, preset_selected, save_prompt, scroll,
                }) => {
                    // Enter's default action is Apply (from anywhere, including the preset
                    // row — a highlighted preset's values, or the process's defaults, are
                    // always valid); it's Preview only when the user has explicitly tabbed
                    // to the Preview button. `cdp_run` re-takes the dialog itself, so it's
                    // restored here first rather than passed directly.
                    let purpose = if focus == cdp_params_focus_preview(fields.len(), second_input.is_some()) {
                        crate::cdp::JobPurpose::Preview
                    } else {
                        crate::cdp::JobPurpose::Apply
                    };
                    self.dialog = Some(Dialog::CdpParams {
                        catalog_index, fields, second_input, focus, error, preview, envelope, list_edit,
                        presets, preset_selected, save_prompt, scroll,
                    });
                    self.cdp_run(purpose);
                }
                Some(d @ Dialog::CdpRunning { .. }) => {
                    // Hard-modal: a job is in flight, Enter does nothing (only Esc/cancel
                    // has any effect while this dialog is showing).
                    self.dialog = Some(d);
                }
                Some(Dialog::CdpOutput { .. }) => {} // just dismiss
                Some(Dialog::Info { .. }) => {} // just dismiss
                None => {}
            },
            KeyCode::Esc => {
                if let Some(Dialog::CdpRunning { .. }) = &self.dialog {
                    // Cancellation is best-effort and takes a poll tick to land; the modal
                    // stays up (and `Esc` stays a no-op beyond re-requesting cancel) until
                    // `tick_cdp` sees the runner's `Finished(Err(Cancelled))` event, so the
                    // dialog never claims a job has stopped before it actually has.
                    self.cdp_runner.cancel();
                } else {
                    self.stop_cdp_preview_audio();
                    self.dialog = None;
                }
            }
            KeyCode::Left => {
                if let Some(Dialog::ExportRegions { focused, depth, .. }) = self.dialog.as_mut() {
                    if *focused == er_focus::FORMAT { *depth = depth.prev(); }
                    else if let Some(input) = self.dialog_input() { input.left(); }
                } else if let Some(Dialog::CdpBrowser { group_focus, .. }) = self.dialog.as_mut() {
                    // Left out of Processes moves focus back into Groups — the mirror image
                    // of Right's "step right, into the next column" (see its own arm below).
                    // Left while already in Groups has no column further left to step into,
                    // so it stays search-cursor movement, matching how Right-in-Processes
                    // falls through the same way for the same reason.
                    if !*group_focus {
                        *group_focus = true;
                    } else if let Some(input) = self.dialog_input() {
                        input.left();
                    }
                } else if let Some(Dialog::CdpParams { .. }) = self.dialog.as_mut() {
                    self.cdp_params_cycle_left_right(false);
                } else if let Some(input) = self.dialog_input() {
                    input.left();
                } else {
                    self.cycle_dialog_curve(false);
                }
            }
            KeyCode::Right => {
                if let Some(Dialog::ExportRegions { focused, depth, .. }) = self.dialog.as_mut() {
                    if *focused == er_focus::FORMAT { *depth = depth.next(); }
                    else if let Some(input) = self.dialog_input() { input.right(); }
                } else if let Some(Dialog::CdpBrowser { group_focus, .. }) = self.dialog.as_mut() {
                    // Right out of the Groups column moves focus into Processes — the
                    // natural "step right, into the next column" reading of the key,
                    // distinct from Tab's plain toggle. Mirrors nothing else in this file
                    // since Groups is the only column with a column to its right; Processes'
                    // own Right stays search-cursor movement (there's no column further
                    // right for it to step into — Description is display-only).
                    if *group_focus {
                        *group_focus = false;
                    } else if let Some(input) = self.dialog_input() {
                        input.right();
                    }
                } else if let Some(Dialog::CdpParams { .. }) = self.dialog.as_mut() {
                    self.cdp_params_cycle_left_right(true);
                } else if let Some(input) = self.dialog_input() {
                    input.right();
                } else {
                    self.cycle_dialog_curve(true);
                }
            }
            // Groups list is fully visible at once (no scrolling — see `Dialog::CdpBrowser`'s
            // doc comment), so a page-step there would be indistinguishable from a
            // single-step one; PageUp/PageDown only ever act on the process list, regardless
            // of `group_focus`.
            KeyCode::PageUp => {
                if let Some(Dialog::CdpBrowser { group_focus: false, selected, .. }) = self.dialog.as_mut() {
                    *selected = selected.saturating_sub(CDP_BROWSER_PAGE_SIZE);
                } else if let Some(Dialog::CdpOutput { scroll, .. }) = self.dialog.as_mut() {
                    *scroll = scroll.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if let Some(Dialog::CdpBrowser { group_focus: false, entries, selected, .. }) = self.dialog.as_mut() {
                    if !entries.is_empty() {
                        *selected = (*selected + CDP_BROWSER_PAGE_SIZE).min(entries.len() - 1);
                    }
                } else if let Some(Dialog::CdpOutput { scroll, lines, .. }) = self.dialog.as_mut() {
                    *scroll = (*scroll + 10).min(lines.len().saturating_sub(1));
                }
            }
            KeyCode::Up => {
                if matches!(self.dialog, Some(Dialog::CdpBrowser { group_focus: true, .. })) {
                    self.cdp_browser_move_group(-1);
                    return;
                }
                match self.dialog.as_mut() {
                    Some(Dialog::CdpBrowser { selected, .. }) => {
                        *selected = selected.saturating_sub(1);
                    }
                    Some(Dialog::CdpParams { fields, focus, .. }) if *focus != CDP_PRESET_FOCUS => {
                        cdp_nudge_number(fields.get_mut(*focus - 1), 1.0);
                    }
                    Some(Dialog::CdpOutput { scroll, .. }) => *scroll = scroll.saturating_sub(1),
                    _ => {}
                }
            }
            KeyCode::Down => {
                if matches!(self.dialog, Some(Dialog::CdpBrowser { group_focus: true, .. })) {
                    self.cdp_browser_move_group(1);
                    return;
                }
                match self.dialog.as_mut() {
                    Some(Dialog::CdpBrowser { entries, selected, .. }) => {
                        if !entries.is_empty() {
                            *selected = (*selected + 1).min(entries.len() - 1);
                        }
                    }
                    Some(Dialog::CdpParams { fields, focus, .. }) if *focus != CDP_PRESET_FOCUS => {
                        cdp_nudge_number(fields.get_mut(*focus - 1), -1.0);
                    }
                    Some(Dialog::CdpOutput { scroll, lines, .. }) => {
                        *scroll = (*scroll + 1).min(lines.len().saturating_sub(1));
                    }
                    _ => {}
                }
            }
            KeyCode::Home => {
                if let Some(input) = self.dialog_input() {
                    input.home();
                }
            }
            KeyCode::End => {
                if let Some(input) = self.dialog_input() {
                    input.end();
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.dialog_input() {
                    input.backspace();
                }
                self.refresh_cdp_browser_filter();
            }
            KeyCode::Delete => {
                // In MixToMono, Delete is a shortcut for "-inf" (silence that channel).
                // Only applies when a channel field is focused, not the tanh toggle row.
                if let Some(Dialog::MixToMono { inputs, focused, .. }) = self.dialog.as_mut() {
                    let f = *focused;
                    if f < inputs.len() {
                        inputs[f] = TextInput::new("-inf");
                    }
                } else if let Some(input) = self.dialog_input() {
                    input.delete();
                }
                self.refresh_cdp_browser_filter();
            }
            KeyCode::Char(' ') => {
                match self.dialog.as_mut() {
                    Some(Dialog::MixToMono { inputs, focused, tanh_clip }) => {
                        if *focused == inputs.len() { *tanh_clip = !*tanh_clip; }
                    }
                    Some(Dialog::Gain { focused, tanh_clip, per_channel, is_stereo, .. }) => {
                        let rows = GainRows::new(*is_stereo, *per_channel);
                        if Some(*focused) == rows.checkbox {
                            *per_channel = !*per_channel;
                        } else if *focused == rows.tanh {
                            *tanh_clip = !*tanh_clip;
                        }
                    }
                    Some(Dialog::ExportRegions {
                        focused, dither, depth, limit_length, normalize, fade_in, fade_out, ..
                    }) => {
                        match *focused {
                            er_focus::DITHER => { if depth.supports_dither() { *dither = !*dither; } }
                            er_focus::LIMIT_CB => *limit_length = !*limit_length,
                            er_focus::NORMALIZE_CB => *normalize = !*normalize,
                            er_focus::FADE_IN_CB => *fade_in = !*fade_in,
                            er_focus::FADE_OUT_CB => *fade_out = !*fade_out,
                            _ => {}
                        }
                    }
                    Some(Dialog::CdpParams { fields, focus, .. }) if *focus != CDP_PRESET_FOCUS => {
                        if let Some(CdpField::Toggle { on }) = fields.get_mut(*focus - 1) {
                            *on = !*on;
                        }
                    }
                    _ => {}
                }
            }
            // Tab cycles dialog focus forward; Shift+Tab (Tab+SHIFT under the kitty protocol,
            // or BackTab on legacy terminals) cycles it backward.
            KeyCode::BackTab => self.cycle_dialog_focus(false),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.cycle_dialog_focus(false)
            }
            KeyCode::Tab => self.cycle_dialog_focus(true),
            // 'e' opens the breakpoint-curve editor for a focused automatable Number field,
            // or the plain-list editor for a focused automatable List field (only one of
            // the two ever applies to a given field — `open_cdp_envelope_editor` already
            // returns `false` for a List field since it pattern-matches `CdpField::Number`)
            // — checked before the generic accepts-a-char arm below since a focused Number
            // field's `dialog_accepts` normally rejects 'e' anyway (digit/minus/dot only),
            // leaving this free to repurpose without a conflict.
            KeyCode::Char('e')
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if !self.open_cdp_envelope_editor() && !self.open_cdp_list_editor() && self.dialog_accepts('e') {
                    if let Some(input) = self.dialog_input() {
                        input.insert('e');
                    }
                    self.refresh_cdp_browser_filter();
                }
            }
            // 's' opens the preset-name prompt (prefilled with the currently-loaded
            // preset's name, if any); 'd' deletes it. Both are `Dialog::CdpParams`-only and,
            // like 'e' above, free to repurpose since a focused Number field's
            // `dialog_accepts` already rejects letters.
            KeyCode::Char('s')
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if !self.open_cdp_preset_save_prompt() && self.dialog_accepts('s') {
                    if let Some(input) = self.dialog_input() {
                        input.insert('s');
                    }
                    self.refresh_cdp_browser_filter();
                }
            }
            KeyCode::Char('d')
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if !self.delete_selected_cdp_preset() && self.dialog_accepts('d') {
                    if let Some(input) = self.dialog_input() {
                        input.insert('d');
                    }
                    self.refresh_cdp_browser_filter();
                }
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.dialog_accepts(c) =>
            {
                if let Some(input) = self.dialog_input() {
                    input.insert(c);
                }
                self.refresh_cdp_browser_filter();
            }
            _ => {}
        }
    }

    /// Re-filters `Dialog::CdpBrowser`'s entries against its current search text AND
    /// highlighted group, resetting `selected` to 0 (the old index's meaning doesn't survive
    /// a changed filter). Called after either input changes — a search keystroke or a group
    /// move — so both stay in sync with what's displayed. A no-op for every other dialog, so
    /// it's safe to call unconditionally after any text edit.
    fn refresh_cdp_browser_filter(&mut self) {
        let Some(Dialog::CdpBrowser { search, groups, group_selected, recent, .. }) = &self.dialog else { return };
        let query = search.value().to_string();
        let group = groups.get(*group_selected).cloned().unwrap_or_else(|| CDP_GROUP_ALL.to_string());
        let recent = recent.clone();
        let new_entries = self.cdp_filter_entries(&query, &group, &recent);
        if let Some(Dialog::CdpBrowser { entries, selected, .. }) = &mut self.dialog {
            *entries = new_entries;
            *selected = 0;
        }
    }

    /// Moves `Dialog::CdpBrowser`'s highlighted group by `delta` (clamped, no wraparound —
    /// wrapping between "All" and the last real group past the ends felt more disorienting
    /// than useful for a one-row-at-a-time list), then re-filters `entries` against the new
    /// group so the process list never shows stale results for the group that used to be
    /// highlighted.
    fn cdp_browser_move_group(&mut self, delta: isize) {
        if let Some(Dialog::CdpBrowser { groups, group_selected, .. }) = self.dialog.as_mut() {
            let n = groups.len();
            if n == 0 {
                return;
            }
            *group_selected = (*group_selected as isize + delta).clamp(0, n as isize - 1) as usize;
        }
        self.refresh_cdp_browser_filter();
    }

    fn cycle_dialog_curve(&mut self, forward: bool) {
        if let Some(Dialog::FadeIn { curve }) | Some(Dialog::FadeOut { curve }) = self.dialog.as_mut() {
            *curve = if forward { curve.next() } else { curve.prev() };
        }
    }

    /// Moves dialog focus to the next (`forward`) or previous field, wrapping. Shared by Tab
    /// and Shift+Tab so the two directions can never disagree on the field order.
    fn cycle_dialog_focus(&mut self, forward: bool) {
        // Step a 0..n index one slot in either direction, wrapping (n - 1 == one back).
        let step = |i: usize, n: usize| if forward { (i + 1) % n } else { (i + n - 1) % n };
        match self.dialog.as_mut() {
            Some(Dialog::MixToMono { inputs, focused, .. }) => {
                *focused = step(*focused, inputs.len() + 1);
            }
            Some(Dialog::Gain { focused, per_channel, is_stereo, .. }) => {
                let rows = GainRows::new(*is_stereo, *per_channel);
                *focused = step(*focused, rows.total);
            }
            // Tab cycles the curve here (←→ is the documented way); Shift+Tab steps it back.
            Some(Dialog::FadeIn { curve }) | Some(Dialog::FadeOut { curve }) => {
                *curve = if forward { curve.next() } else { curve.prev() };
            }
            Some(Dialog::ExportRegions { focused, .. }) => *focused = step(*focused, er_focus::COUNT),
            // Only two columns receive keyboard focus (Groups, Processes — the description
            // column is display-only), so Tab and Shift+Tab both just flip it.
            Some(Dialog::CdpBrowser { group_focus, .. }) => *group_focus = !*group_focus,
            // The preset row, then fields, then the second-input picker (dual-input
            // processes only), then Preview, then Apply (see
            // cdp_params_focus_{second_input,preview,apply}). `render_cdp_params_dialog`
            // computes the visible scroll window fresh from `focus` every frame (mirroring
            // `Dialog::CdpBrowser`'s own on-the-fly `scroll_top` from `selected`), so there's
            // nothing to update here beyond `focus` itself.
            Some(Dialog::CdpParams { fields, second_input, focus, .. }) => {
                *focus = step(*focus, cdp_params_focus_apply(fields.len(), second_input.is_some()) + 1);
            }
            _ => {}
        }
    }

    /// Called when the user left-clicks a row in an open dialog popup. `row` is a 0-based
    /// index into `dialog_row_rects` — clicking a row focuses it, and clicking a checkbox
    /// row also toggles it. `x_in_row` is the click's column within that row's Rect, used
    /// by rows that hold both a checkbox and a value field to tell the two targets apart.
    fn handle_dialog_row_click(&mut self, row: usize, x_in_row: u16) {
        // The hints/apply bar is appended as the last element of dialog_row_rects.
        // Clicking it (or anything past the interactive rows) submits the dialog.
        if row >= self.dialog_n_interactive {
            let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            if self.save_as_active {
                self.handle_save_as_key(enter);
            } else {
                self.handle_dialog_key(enter);
            }
            return;
        }

        if self.save_as_active {
            match row {
                0 | 1 => self.save_as_focused = row,
                2 if self.save_as_depth.supports_dither() => {
                    self.save_as_focused = 2;
                    self.save_as_dither = !self.save_as_dither;
                }
                _ => {}
            }
            return;
        }
        match self.dialog.as_mut() {
            Some(Dialog::Gain { focused, tanh_clip, per_channel, is_stereo, .. }) => {
                let rows = GainRows::new(*is_stereo, *per_channel);
                if row >= rows.total { return; }
                *focused = row;
                if Some(row) == rows.checkbox {
                    *per_channel = !*per_channel;
                } else if row == rows.tanh {
                    *tanh_clip = !*tanh_clip;
                }
            }
            Some(Dialog::MixToMono { inputs, focused, tanh_clip }) => {
                let n = inputs.len();
                if row < n {
                    *focused = row;
                } else if row == n {
                    *focused = n;
                    *tanh_clip = !*tanh_clip;
                }
            }
            Some(Dialog::ExportRegions {
                focused, dither, depth, limit_length, normalize, fade_in, fade_out, ..
            }) => {
                match row {
                    0..=3 => {
                        *focused = row;
                        if row == er_focus::DITHER && depth.supports_dither() { *dither = !*dither; }
                    }
                    4..=7 => {
                        let (cb, val) = er_focus::checkbox_row_focus(row);
                        if x_in_row >= er_focus::VALUE_COL {
                            // A click on the value text focuses the field for editing;
                            // it must not flip the checkbox the same row happens to hold.
                            *focused = val;
                        } else {
                            *focused = cb;
                            match cb {
                                er_focus::LIMIT_CB => *limit_length = !*limit_length,
                                er_focus::NORMALIZE_CB => *normalize = !*normalize,
                                er_focus::FADE_IN_CB => *fade_in = !*fade_in,
                                _ => *fade_out = !*fade_out,
                            }
                        }
                    }
                    _ => {}
                }
            }
            // `row_rects` (`render_cdp_browser_dialog`) spans BOTH the Groups and Processes
            // columns on one combined `Rect` per row, so `row` alone (0-based, no scroll
            // offset — the groups list is always fully visible) means the same thing in
            // either column; `x_in_row` disambiguates which one was actually clicked.
            // A groups click moves the highlight there and re-filters (no separate "open"
            // step — see `App::cdp_browser_move_group`'s doc comment for why arrow-key moves
            // work the same way). A click on a process name selects *and* opens it in one
            // step (matching Enter's behavior on the currently-selected entry) — clicking is
            // how a mouse user "commits" a choice, there's no separate confirm step the way
            // there is for e.g. a checkbox row elsewhere in this function. `row` is 0-based
            // into the *visible* window (`render_cdp_browser_dialog`'s own scroll_top math,
            // mirrored here so the two can't disagree about which entry a given row means).
            Some(Dialog::CdpBrowser { groups, group_selected, group_focus, entries, selected, .. }) => {
                if x_in_row < CDP_GROUP_COL_WIDTH {
                    if row < groups.len() {
                        *group_focus = true;
                        *group_selected = row;
                        self.refresh_cdp_browser_filter();
                    }
                } else {
                    *group_focus = false;
                    let scroll_top = selected.saturating_sub(CDP_BROWSER_LIST_ROWS.saturating_sub(1));
                    let clicked = scroll_top + row;
                    if clicked < entries.len() {
                        *selected = clicked;
                        self.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    }
                }
            }
            _ => {}
        }
    }

    /// The sample range an operation should act on: the current selection if one exists,
    /// otherwise the whole document. Optionally snapped to zero crossings. Returns `None`
    /// for an empty document or a degenerate (empty) range.
    fn operation_range(&self, idx: usize, snap: bool) -> Option<(usize, usize)> {
        let doc = self.documents.get(idx)?;
        let total_len = doc.len_samples();
        if total_len == 0 {
            return None;
        }
        let (start, end) = doc
            .selection
            .map(|sel| sel.normalized())
            .unwrap_or((0, total_len));
        let (start, end) = if snap {
            doc.snap_range_to_zero_crossing(start, end)
        } else {
            (start, end)
        };
        (start < end).then_some((start, end))
    }

    /// The sample range a fade should act on: the current selection if one exists, otherwise
    /// a fade-direction-specific default — fade in runs from the start of the file to the
    /// cursor, fade out from the cursor to the end of the file — rather than
    /// [`Self::operation_range`]'s whole-file default, which would fade the entire document
    /// regardless of where the cursor is. Optionally snapped to zero crossings; returns `None`
    /// for an empty document or a degenerate (empty) range.
    fn fade_operation_range(&self, idx: usize, fade_in: bool, snap: bool) -> Option<(usize, usize)> {
        let doc = self.documents.get(idx)?;
        let total_len = doc.len_samples();
        if total_len == 0 {
            return None;
        }
        let (start, end) = doc.selection.map(|sel| sel.normalized()).unwrap_or_else(|| {
            if fade_in { (0, doc.cursor) } else { (doc.cursor, total_len) }
        });
        let (start, end) = if snap {
            doc.snap_range_to_zero_crossing(start, end)
        } else {
            (start, end)
        };
        (start < end).then_some((start, end))
    }

    /// Shared tail for every operation that mutates sample data on `idx` (which is always
    /// the active document): mark the file dirty, hand the new buffer to the audio engine,
    /// rebuild the waveform caches, and re-fit auto vertical zoom if it's on.
    fn after_sample_mutation(&mut self, idx: usize) {
        if self.documents[idx].dirty {
            if let Some(path) = self.documents[idx].path.clone() {
                self.file_panel.mark_dirty(&path, true);
            }
        }
        // A rate change (resample, or its undo/redo) needs a fresh engine since the rate is
        // captured at construction; otherwise a cheap data reload is enough.
        if self.audio_sample_rate != Some(self.documents[idx].sample_rate) {
            self.rebuild_audio();
        } else if let Some(audio) = &self.audio {
            audio.reload(self.documents[idx].channels.clone());
        }
        self.rebuild_waveform_caches();
        if self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom) {
            let peak = self.visible_peak();
            if peak > 0.0001 {
                if let Some(viewport) = self.viewport.as_mut() {
                    viewport.set_amplitude_scale(0.95 / peak);
                }
            }
        }
    }

    /// Entry point for `Action::CdpProcess` (Ctrl+P / Process menu): opens the setup prompt
    /// if `config.cdp_dir` is unset or no longer validates, otherwise goes straight to the
    /// process browser. The menu entry is always enabled rather than conditionally greyed
    /// out — an invalid path is discovered here, on demand, instead of silently.
    fn open_cdp_entry(&mut self) {
        let dir = std::path::PathBuf::from(&self.config.cdp_dir);
        if self.config.cdp_dir.is_empty() || crate::cdp::validate_cdp_dir(&dir).is_err() {
            self.dialog = Some(Dialog::CdpSetup {
                input: TextInput::new(self.config.cdp_dir.clone()),
                error: None,
            });
        } else {
            self.open_cdp_browser();
        }
    }

    /// Re-validates the path typed into `Dialog::CdpSetup`. On success, persists it and
    /// proceeds straight to the browser; on failure, reopens the setup prompt with the
    /// reason shown inline.
    fn confirm_cdp_setup(&mut self, path: String) {
        let trimmed = path.trim().to_string();
        match crate::cdp::validate_cdp_dir(std::path::Path::new(&trimmed)) {
            Ok(()) => {
                self.config.cdp_dir = trimmed;
                self.save_config();
                self.open_cdp_browser();
            }
            Err(reason) => {
                self.dialog = Some(Dialog::CdpSetup { input: TextInput::new(trimmed), error: Some(reason) });
            }
        }
    }

    /// Entries matching `query` (case-insensitive substring over key/title — the process's
    /// *name*, not its description) AND `group`, as indices into `cdp_catalog.processes`.
    ///
    /// `group == CDP_GROUP_ALL` skips the group filter entirely (the pre-Phase-7 behavior);
    /// `group == CDP_GROUP_RECENT` returns entries in `recent`'s own most-recently-used
    /// order instead of catalog order — order carries meaning there (it's *why* the group
    /// exists) in a way it doesn't for the other groups. Every other `group` value matches
    /// `ProcessDef::subcategory` exactly. For every case but Recent, catalog order
    /// (alphabetical by key, from the conversion script) is preserved rather than re-sorted,
    /// so results don't reshuffle confusingly as the user types.
    fn cdp_filter_entries(&self, query: &str, group: &str, recent: &[String]) -> Vec<usize> {
        let query = query.to_lowercase();
        // Name only — `key` (the internal identifier, e.g. "blur_avrg") and `title` (the
        // displayed name, e.g. "Blur Average"). `short_description`/`description` used to be
        // included too, but that surfaced processes whose *description* happened to mention
        // a search term while their actual name didn't, which read as the search matching
        // almost anything.
        let matches_query = |p: &crate::model::cdp::ProcessDef| {
            query.is_empty()
                || p.key.to_lowercase().contains(&query)
                || p.title.to_lowercase().contains(&query)
        };
        if group == CDP_GROUP_RECENT {
            return recent
                .iter()
                .filter_map(|key| self.cdp_catalog.processes.iter().position(|p| &p.key == key))
                .filter(|&i| matches_query(&self.cdp_catalog.processes[i]))
                .collect();
        }
        self.cdp_catalog
            .processes
            .iter()
            .enumerate()
            .filter(|(_, p)| (group == CDP_GROUP_ALL || p.subcategory == group) && matches_query(p))
            .map(|(i, _)| i)
            .collect()
    }

    /// `Dialog::CdpBrowser`'s group list: `CDP_GROUP_ALL`, `CDP_GROUP_RECENT`, then every
    /// real `subcategory` value in the catalog, alphabetically — the taxonomy
    /// `scripts/convert_soundthread_catalog.py`'s `resolve_subcategory` reconciles down to
    /// one clean set (see CDP-Ext-Plan.md Phase 7), used verbatim rather than re-derived
    /// here.
    fn cdp_groups(&self) -> Vec<String> {
        let mut subcategories: Vec<String> =
            self.cdp_catalog.processes.iter().map(|p| p.subcategory.clone()).collect();
        subcategories.sort();
        subcategories.dedup();
        let mut groups = vec![CDP_GROUP_ALL.to_string(), CDP_GROUP_RECENT.to_string()];
        groups.extend(subcategories);
        groups
    }

    /// Opens the CDP process browser: a plain searchable, group-filterable list, no controls
    /// — see `Dialog::CdpBrowser`'s doc comment for why that's now a fixed-size dialog
    /// rather than something that grows/shrinks per process. Starts on "All" (group index 0)
    /// with the process list focused, matching the pre-Phase-7 behavior for anyone who never
    /// touches the groups column.
    fn open_cdp_browser(&mut self) {
        let groups = self.cdp_groups();
        let recent = crate::model::cdp::recent::load_recent();
        let entries = self.cdp_filter_entries("", CDP_GROUP_ALL, &recent);
        self.dialog = Some(Dialog::CdpBrowser {
            search: TextInput::new(""),
            groups,
            group_selected: 0,
            group_focus: false,
            recent,
            entries,
            selected: 0,
        });
    }

    /// Opens `Dialog::CdpParams` for the process at `catalog_index`, building fresh
    /// default-valued fields and loading any presets saved for it from disk.
    fn open_cdp_params(&mut self, catalog_index: usize) {
        let Some(def) = self.cdp_catalog.processes.get(catalog_index) else { return };
        let (fields, second_input) = self.cdp_fields_for(catalog_index);
        let presets = crate::model::cdp::preset::load_presets(&def.key, def.params.len());
        self.dialog = Some(Dialog::CdpParams {
            catalog_index,
            fields,
            second_input,
            focus: CDP_PRESET_FOCUS,
            error: None,
            preview: None,
            envelope: None,
            list_edit: None,
            presets,
            preset_selected: None,
            save_prompt: None,
            scroll: 0,
        });
    }

    /// Builds fresh default-valued `fields`/`second_input` for the catalog process at
    /// `catalog_index` — used by `open_cdp_params` to seed a freshly opened dialog.
    fn cdp_fields_for(&self, catalog_index: usize) -> (Vec<CdpField>, Option<CdpSecondInput>) {
        use crate::model::cdp::IoKind;
        let Some(def) = self.cdp_catalog.processes.get(catalog_index) else {
            return (Vec::new(), None);
        };
        let mut fields: Vec<CdpField> = def.params.iter().map(CdpField::from_default).collect();
        // Synthesis processes carry a Sample Rate choice — preselect the option matching
        // the active document so the generated audio splices in at the right speed by
        // default (Apply hard-rejects a mismatch; see tick_cdp). Matched generically by
        // option text rather than param name so hand-authored defs benefit too.
        if def.input == IoKind::None {
            if let Some(doc_rate) = self.active_doc().map(|d| d.sample_rate.to_string()) {
                for field in &mut fields {
                    if let CdpField::Choice { options, selected } = field {
                        if let Some(pos) = options.iter().position(|o| *o == doc_rate) {
                            *selected = pos;
                        }
                    }
                }
            }
        }
        let second_input = matches!(def.input, IoKind::DualWav | IoKind::DualAna).then(|| {
            let doc_indices: Vec<usize> = (0..self.documents.len()).collect();
            let names = doc_indices.iter().map(|&i| self.buffer_name(i)).collect();
            // Default to the first buffer that isn't the one being processed — the common
            // case is combining against different material; processing against itself
            // (self-convolution etc.) stays one Left-press away.
            let selected = doc_indices
                .iter()
                .position(|&i| i != self.active_document)
                .unwrap_or(0);
            CdpSecondInput { doc_indices, names, selected }
        });
        (fields, second_input)
    }

    /// Rebuilds `Dialog::CdpBrowser`'s `fields`/`second_input` for whatever process
    /// `list_selected` currently points at, resetting `focus` back to the process list,
    /// and clearing any stale validation error or Preview cache from the previously
    /// selected process. Called after every change to `entries`/`list_selected` (arrow
    /// navigation, a search edit that reshuffles the filtered list) — a no-op if no dialog
    /// is open or it isn't `CdpBrowser`.
    /// Opens the breakpoint-curve editor for whichever `CdpField::Number` is currently
    /// focused in `Dialog::CdpBrowser` ('e' key), if that's actually possible right now —
    /// returns `false` (a no-op) when there's no such dialog, focus isn't on a field, the
    /// field isn't a Number, or the process's own catalog metadata marks that param as not
    /// `automatable` (CDP's own constraint on which params accept a `.brk` file). The 'e'
    /// key handler falls through to typing 'e' as ordinary text when this returns `false`,
    /// so every one of these guards has to hold before touching the dialog.
    fn open_cdp_envelope_editor(&mut self) -> bool {
        let Some(Dialog::CdpParams { catalog_index, fields, focus, .. }) = &self.dialog else {
            return false;
        };
        if *focus == CDP_PRESET_FOCUS {
            return false;
        }
        let field_index = *focus - 1;
        let Some(CdpField::Number { input, min, max, step, envelope, .. }) = fields.get(field_index) else {
            return false;
        };
        let Some(def) = self.cdp_catalog.processes.get(*catalog_index) else {
            return false;
        };
        let Some(param) = def.params.get(field_index) else {
            return false;
        };
        if !param.automatable {
            return false;
        }

        let current_value = input.value().trim().parse::<f64>().unwrap_or(*min).clamp(*min, *max);
        let original = envelope.clone();
        let idx = self.active_document;
        let range = self.operation_range(idx, self.snap_to_zero).unwrap_or((0, 0));
        let time_max = self
            .documents
            .get(idx)
            .map(|doc| (range.1 - range.0) as f64 / doc.sample_rate as f64)
            .filter(|&d| d > 0.0)
            .unwrap_or(1.0);
        // A flat 2-point starting line is the right placeholder for an *optional*
        // automatable field — it exactly reproduces the constant it's replacing. But a
        // `required_envelope` field has no constant it's replacing, and at least one real
        // CDP process (`fractal wave`/`spectrum`'s Shape) hangs indefinitely — no error, it
        // just never returns — on *any* straight 2-point line, regardless of whether the two
        // points' values are equal or different (confirmed against the real binary: a
        // 2-point ramp of any size hangs, but a 3-point line with even a barely-perceptible
        // bend in the middle completes in milliseconds — the fractal algorithm's recursive
        // self-similarity check apparently never terminates against an input shape that's
        // itself perfectly self-similar at every scale, i.e. a straight line). So a required
        // field's never-yet-opened starting shape seeds 3 points with a small symmetric bump
        // in the middle rather than 2, sidestepping that trap by construction (for every
        // required-envelope process, not just fractal — a harmless, barely-visible starting
        // curve either way) rather than asking every affected process to work around it.
        let points = original.clone().unwrap_or_else(|| {
            if param.required_envelope {
                let bumped = if current_value + *step <= *max { current_value + *step } else { current_value - *step };
                vec![(0.0, current_value), (time_max / 2.0, bumped.clamp(*min, *max)), (time_max, current_value)]
            } else {
                vec![(0.0, current_value), (time_max, current_value)]
            }
        });

        let Some(Dialog::CdpParams { envelope: dialog_envelope, .. }) = self.dialog.as_mut() else {
            return false;
        };
        *dialog_envelope = Some(CdpEnvelopeEdit { field_index, points, selected: 0, original, time_max, range });
        true
    }

    /// Opens the plain-list editor for whichever `CdpField::List` is currently focused in
    /// `Dialog::CdpParams` ('e' key) — the `required_list` counterpart to
    /// `open_cdp_envelope_editor`. Returns `false` (a no-op, falls through to typing 'e' as
    /// ordinary text) when there's no such dialog, focus isn't on a field, the field isn't a
    /// List, or the process's own catalog metadata marks that param as not `automatable`.
    fn open_cdp_list_editor(&mut self) -> bool {
        let Some(Dialog::CdpParams { catalog_index, fields, focus, .. }) = &self.dialog else {
            return false;
        };
        if *focus == CDP_PRESET_FOCUS {
            return false;
        }
        let field_index = *focus - 1;
        let Some(CdpField::List { values, .. }) = fields.get(field_index) else {
            return false;
        };
        let Some(def) = self.cdp_catalog.processes.get(*catalog_index) else {
            return false;
        };
        let Some(param) = def.params.get(field_index) else {
            return false;
        };
        if !param.automatable {
            return false;
        }

        let is_time_sequence = param.list_is_time_sequence;
        let idx = self.active_document;
        let range = self.operation_range(idx, self.snap_to_zero).unwrap_or((0, 0));
        let time_max = self
            .documents
            .get(idx)
            .map(|doc| (range.1 - range.0) as f64 / doc.sample_rate as f64)
            .filter(|&d| d > 0.0)
            .unwrap_or(1.0);

        let original = values.clone();
        // Never-yet-configured (empty) list: seed one entry at the param's own default
        // rather than opening on a genuinely empty editor with nothing to select/nudge —
        // clamped to the real selection's duration for a time-sequence field, so the seed
        // itself is never already out of the practical range the user is about to nudge
        // within.
        let seeded = if original.is_empty() {
            let crate::model::cdp::ParamKind::Number { default, min, .. } = &param.kind else {
                unreachable!("required_list param {:?} is not a Number kind", param.name)
            };
            let seed_value = if is_time_sequence { default.clamp(*min, time_max) } else { *default };
            vec![seed_value]
        } else {
            original.clone()
        };

        let Some(Dialog::CdpParams { list_edit: dialog_list_edit, .. }) = self.dialog.as_mut() else {
            return false;
        };
        *dialog_list_edit =
            Some(CdpListEdit { field_index, values: seeded, selected: 0, original, is_time_sequence, time_max });
        true
    }

    /// All key handling while `Dialog::CdpParams.list_edit` is `Some` — the `required_list`
    /// counterpart to `handle_cdp_envelope_key`, dispatched the same way (checked at the top
    /// of `handle_dialog_key`, mutually exclusive with the envelope path since a field is
    /// never both). Left/Right selects an entry; Del/Backspace removes it (kept at a
    /// minimum of 1 — unlike the envelope editor's 2-point minimum, a single-entry list is
    /// perfectly meaningful here); Esc discards every edit made this session, Enter commits.
    ///
    /// Up/Down and 'n' branch on `edit.is_time_sequence` (`ParamDef.list_is_time_sequence`'s
    /// doc comment has the full "why," found via a user manually testing this exact editor
    /// against `grain_reposition` and hitting CDP's real "Sync times out of sequence"
    /// rejection): for a time-sequence field they're constrained the same way the envelope
    /// editor's own time-move already is — Up/Down clamps a value between its immediate
    /// neighbors (never past them, so `values` can't go out of order no matter how it's
    /// nudged) using the real selection's duration as the practical range instead of the
    /// catalog's own generous `max`, and 'n' inserts at the midpoint between the selected
    /// entry and its neighbor (or, with only one entry so far, offset by one `step` from it)
    /// rather than a flat duplicate, which would create two equal — also rejected — times.
    /// A non-time-sequence field (e.g. `grain repitch`'s per-grain transpositions) keeps the
    /// original unconstrained behavior: any order is fine, so Up/Down only clamps to the
    /// catalog's own `min`/`max` and 'n' duplicates the selected value.
    fn handle_cdp_list_key(&mut self, key: KeyEvent) {
        let Some(Dialog::CdpParams { list_edit: Some(edit), fields, .. }) = self.dialog.as_mut() else { return };
        let field_index = edit.field_index;
        let Some(CdpField::List { min, max, step, .. }) = fields.get(field_index) else { return };
        let (min, max, step) = (*min, *max, *step);
        let committed_values = edit.values.clone();
        let original = edit.original.clone();
        // A time-sequence field's practical range is the real selection's duration, not
        // the catalog's own safety-cap `max` (e.g. "up to 2 hours") — see this fn's doc
        // comment and `CdpListEdit.time_max`'s.
        let effective_max = if edit.is_time_sequence { max.min(edit.time_max) } else { max };
        // Minimum gap enforced between neighboring time-sequence entries so they can never
        // become exactly equal (also rejected by CDP as "out of sequence") — same constant
        // the envelope editor's own neighbor-clamped time-move already uses.
        const MIN_GAP: f64 = 0.001;

        match key.code {
            KeyCode::Left => {
                edit.selected = edit.selected.saturating_sub(1);
                return;
            }
            KeyCode::Right => {
                edit.selected = (edit.selected + 1).min(edit.values.len().saturating_sub(1));
                return;
            }
            KeyCode::Up => {
                let i = edit.selected;
                let value_step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    step
                } else {
                    ((effective_max - min) / 40.0).max(step)
                };
                if edit.is_time_sequence {
                    let upper =
                        if i + 1 == edit.values.len() { effective_max } else { edit.values[i + 1] - MIN_GAP };
                    edit.values[i] = (edit.values[i] + value_step).min(upper.max(edit.values[i])).max(min);
                } else {
                    edit.values[i] = (edit.values[i] + value_step).clamp(min, max);
                }
                return;
            }
            KeyCode::Down => {
                let i = edit.selected;
                let value_step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    step
                } else {
                    ((effective_max - min) / 40.0).max(step)
                };
                if edit.is_time_sequence {
                    let lower = if i == 0 { min } else { edit.values[i - 1] + MIN_GAP };
                    edit.values[i] = (edit.values[i] - value_step).max(lower.min(edit.values[i])).min(effective_max);
                } else {
                    edit.values[i] = (edit.values[i] - value_step).clamp(min, max);
                }
                return;
            }
            KeyCode::Char('n') => {
                let i = edit.selected;
                if edit.is_time_sequence {
                    if edit.values.len() == 1 {
                        let new_time = if edit.values[0] + step <= effective_max {
                            edit.values[0] + step
                        } else {
                            (edit.values[0] - step).max(min)
                        };
                        if new_time >= edit.values[0] {
                            edit.values.push(new_time);
                            edit.selected = 1;
                        } else {
                            edit.values.insert(0, new_time);
                            edit.selected = 0;
                        }
                    } else {
                        let (lo_i, hi_i) = if i + 1 < edit.values.len() { (i, i + 1) } else { (i - 1, i) };
                        let mid = (edit.values[lo_i] + edit.values[hi_i]) / 2.0;
                        edit.values.insert(hi_i, mid);
                        edit.selected = hi_i;
                    }
                } else {
                    let v = edit.values[i];
                    edit.values.insert(i + 1, v);
                    edit.selected = i + 1;
                }
                return;
            }
            KeyCode::Delete | KeyCode::Backspace => {
                if edit.values.len() > 1 {
                    edit.values.remove(edit.selected);
                    edit.selected = edit.selected.min(edit.values.len() - 1);
                }
                return;
            }
            _ => {}
        }

        // Only the closing actions (Esc/Enter) reach here — `edit`/`fields` are no longer
        // borrowed past this point, so `self.dialog` can be freely re-borrowed.
        match key.code {
            KeyCode::Esc => {
                if let Some(Dialog::CdpParams { fields, list_edit, .. }) = self.dialog.as_mut() {
                    if let Some(CdpField::List { values: field_values, .. }) = fields.get_mut(field_index) {
                        *field_values = original;
                    }
                    *list_edit = None;
                }
            }
            KeyCode::Enter => {
                if let Some(Dialog::CdpParams { fields, list_edit, .. }) = self.dialog.as_mut() {
                    if let Some(CdpField::List { values: field_values, .. }) = fields.get_mut(field_index) {
                        *field_values = committed_values;
                    }
                    *list_edit = None;
                }
            }
            _ => {}
        }
    }

    /// All key handling while `Dialog::CdpParams.envelope` is `Some` — a completely
    /// separate routing path from the rest of `handle_dialog_key` (dispatched at its very
    /// top), so the two never interleave. See `CdpEnvelopeEdit`'s doc comment for what each
    /// action does; `Esc`/`c`/`Enter` all *close* the editor and so need the mutable borrow
    /// of `edit` to have ended before they touch `Dialog::CdpParams.envelope` itself
    /// (can't null out the `Option` while something still borrows its contents) — every
    /// value they need is captured into plain owned locals up front for exactly that reason.
    fn handle_cdp_envelope_key(&mut self, key: KeyEvent) {
        let Some(Dialog::CdpParams { envelope: Some(edit), fields, catalog_index, .. }) = self.dialog.as_mut()
        else {
            return;
        };
        let field_index = edit.field_index;
        let Some(CdpField::Number { min, max, step, .. }) = fields.get(field_index) else { return };
        let (min, max, step) = (*min, *max, *step);
        let committed_points = edit.points.clone();
        let original = edit.original.clone();
        let required_envelope = self
            .cdp_catalog
            .processes
            .get(*catalog_index)
            .and_then(|d| d.params.get(field_index))
            .is_some_and(|p| p.required_envelope);

        match key.code {
            KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
                let i = edit.selected;
                let time_step = (edit.time_max / 40.0).max(0.001);
                let lower = if i == 0 { 0.0 } else { edit.points[i - 1].0 + 0.001 };
                edit.points[i].0 = (edit.points[i].0 - time_step).max(lower);
                return;
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
                let i = edit.selected;
                let time_step = (edit.time_max / 40.0).max(0.001);
                let upper = if i + 1 == edit.points.len() { edit.time_max } else { edit.points[i + 1].0 - 0.001 };
                edit.points[i].0 = (edit.points[i].0 + time_step).min(upper.max(edit.points[i].0));
                return;
            }
            KeyCode::Left => {
                edit.selected = edit.selected.saturating_sub(1);
                return;
            }
            KeyCode::Right => {
                edit.selected = (edit.selected + 1).min(edit.points.len().saturating_sub(1));
                return;
            }
            // Plain Up/Down is the coarse move — scaled to the param's own min/max range so
            // it always visibly shifts a breakpoint regardless of how fine that param's
            // catalog `step` is (e.g. Blurring's step of 0.01 across a 0.1-100.0 range is
            // imperceptible on a 16-row grid; 1/40th of the range always moves at least one
            // row). Shift+Up/Down is the fine move, using the catalog step directly — the
            // same coarse/plain-vs-fine/shift split as the time axis below.
            KeyCode::Up => {
                let i = edit.selected;
                let value_step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    step
                } else {
                    ((max - min) / 40.0).max(step)
                };
                edit.points[i].1 = (edit.points[i].1 + value_step).clamp(min, max);
                return;
            }
            KeyCode::Down => {
                let i = edit.selected;
                let value_step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    step
                } else {
                    ((max - min) / 40.0).max(step)
                };
                edit.points[i].1 = (edit.points[i].1 - value_step).clamp(min, max);
                return;
            }
            KeyCode::Char('n') => {
                let i = edit.selected;
                let (lo_i, hi_i) = if i + 1 < edit.points.len() { (i, i + 1) } else { (i - 1, i) };
                let (t0, v0) = edit.points[lo_i];
                let (t1, v1) = edit.points[hi_i];
                let mid_t = (t0 + t1) / 2.0;
                let ratio = if t1 > t0 { (mid_t - t0) / (t1 - t0) } else { 0.5 };
                let mid_v = v0 + (v1 - v0) * ratio;
                edit.points.insert(hi_i, (mid_t, mid_v));
                edit.selected = hi_i;
                return;
            }
            KeyCode::Delete | KeyCode::Backspace => {
                if edit.points.len() > 2 {
                    edit.points.remove(edit.selected);
                    edit.selected = edit.selected.min(edit.points.len() - 1);
                }
                return;
            }
            _ => {}
        }

        // Only the closing actions (Esc/'c'/Enter) reach here — `edit`/`fields` are no
        // longer borrowed past this point, so `self.dialog` can be freely re-borrowed.
        match key.code {
            KeyCode::Esc => {
                if let Some(Dialog::CdpParams { fields, envelope, .. }) = self.dialog.as_mut() {
                    if let Some(CdpField::Number { envelope: field_env, .. }) = fields.get_mut(field_index) {
                        *field_env = original;
                    }
                    *envelope = None;
                }
            }
            // No-op for a `required_envelope` field — it has no valid constant
            // representation to revert to (see `ParamDef::required_envelope`'s doc comment).
            KeyCode::Char('c') if required_envelope => {}
            KeyCode::Char('c') => {
                if let Some(Dialog::CdpParams { fields, envelope, .. }) = self.dialog.as_mut() {
                    if let Some(CdpField::Number { envelope: field_env, .. }) = fields.get_mut(field_index) {
                        *field_env = None;
                    }
                    *envelope = None;
                }
            }
            KeyCode::Enter => {
                if let Some(Dialog::CdpParams { fields, envelope, .. }) = self.dialog.as_mut() {
                    if let Some(CdpField::Number { envelope: field_env, .. }) = fields.get_mut(field_index) {
                        *field_env = Some(committed_points);
                    }
                    *envelope = None;
                }
            }
            _ => {}
        }
    }

    /// Left (`forward = false`)/Right (`forward = true`) within `Dialog::CdpParams`: on the
    /// preset row, cycles through saved presets (`cdp_params_cycle_preset`); on the second-
    /// input row, cycles which open buffer is selected; on a focused `Choice` field, cycles
    /// its option; otherwise, moves that field's text cursor. Each case returns as soon as
    /// it's handled so `Dialog::CdpParams`'s borrow from the initial match never overlaps
    /// with the final case's `self.dialog_input()` call (a fresh, separate borrow of
    /// `self.dialog`) — see this file's other `handle_*_key` functions for the same pattern.
    fn cdp_params_cycle_left_right(&mut self, forward: bool) {
        let Some(Dialog::CdpParams { fields, second_input, focus, .. }) = self.dialog.as_mut() else { return };
        let focus_val = *focus;
        if focus_val == CDP_PRESET_FOCUS {
            self.cdp_params_cycle_preset(forward);
            return;
        }
        if focus_val == cdp_params_focus_second_input(fields.len()) {
            if let Some(second) = second_input {
                second.selected = if forward {
                    (second.selected + 1).min(second.doc_indices.len().saturating_sub(1))
                } else {
                    second.selected.saturating_sub(1)
                };
            }
            return;
        }
        if let Some(CdpField::Choice { options, selected }) = fields.get_mut(focus_val - 1) {
            *selected = if forward {
                (*selected + 1).min(options.len().saturating_sub(1))
            } else {
                selected.saturating_sub(1)
            };
            return;
        }
        if let Some(input) = self.dialog_input() {
            if forward { input.right(); } else { input.left(); }
        }
    }

    /// Cycles `preset_selected` through the saved presets for the process currently open in
    /// `Dialog::CdpParams` (wrapping; starting from `None`, Right lands on the first preset
    /// and Left on the last), loading the newly selected preset's values into `fields`
    /// immediately — see `Dialog::CdpParams`'s doc comment. A no-op if there are no saved
    /// presets.
    fn cdp_params_cycle_preset(&mut self, forward: bool) {
        let (catalog_index, new_index) = {
            let Some(Dialog::CdpParams { catalog_index, presets, preset_selected, .. }) = &self.dialog else {
                return;
            };
            if presets.is_empty() {
                return;
            }
            let len = presets.len();
            let new_index = match (*preset_selected, forward) {
                (None, true) => 0,
                (None, false) => len - 1,
                (Some(i), true) => (i + 1) % len,
                (Some(i), false) => (i + len - 1) % len,
            };
            (*catalog_index, new_index)
        };
        let Some(def) = self.cdp_catalog.processes.get(catalog_index) else { return };
        let params = def.params.clone();

        let Some(Dialog::CdpParams { presets, preset_selected, fields, .. }) = self.dialog.as_mut() else {
            return;
        };
        let Some(preset) = presets.get(new_index) else { return };
        *fields = params.iter().zip(preset.values.iter()).map(|(p, v)| CdpField::from_value(p, v)).collect();
        *preset_selected = Some(new_index);
    }

    /// Opens the preset-name prompt ('s' key), prefilled with the currently-loaded preset's
    /// name if any (so re-saving over it is just Enter) — returns `false` (a no-op, falling
    /// through to typing 's' as ordinary text) when there's no `Dialog::CdpParams` open, or
    /// another sub-mode (envelope editor, an already-open save prompt) is active.
    fn open_cdp_preset_save_prompt(&mut self) -> bool {
        let Some(Dialog::CdpParams { presets, preset_selected, envelope, save_prompt, .. }) =
            self.dialog.as_mut()
        else {
            return false;
        };
        if envelope.is_some() || save_prompt.is_some() {
            return false;
        }
        let prefill = preset_selected.and_then(|i| presets.get(i)).map(|p| p.name.clone()).unwrap_or_default();
        *save_prompt = Some(TextInput::fresh(prefill));
        true
    }

    /// All key handling while `Dialog::CdpParams.save_prompt` is `Some` — mirrors
    /// `handle_cdp_envelope_key`'s isolation (dispatched at the very top of
    /// `handle_dialog_key`, before anything else sees the key). Enter commits: saves the
    /// current field values under the typed name (overwriting an existing preset of that
    /// name), reloads the preset list from disk, and selects the newly saved preset. An
    /// empty/whitespace-only name is treated as "cancel" (nothing worth saving under no
    /// name), matching Esc.
    fn handle_cdp_preset_save_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if let Some(Dialog::CdpParams { save_prompt, .. }) = self.dialog.as_mut() {
                    *save_prompt = None;
                }
            }
            KeyCode::Enter => {
                let Some(Dialog::CdpParams { catalog_index, fields, save_prompt, .. }) = self.dialog.as_mut()
                else {
                    return;
                };
                let name = save_prompt.as_ref().map(|i| i.value().trim().to_string()).unwrap_or_default();
                if name.is_empty() {
                    *save_prompt = None;
                    return;
                }
                let values: Vec<_> = fields.iter().map(CdpField::to_value).collect();
                let catalog_index = *catalog_index;
                let Some(def) = self.cdp_catalog.processes.get(catalog_index) else { return };
                let key_str = def.key.clone();
                let param_count = def.params.len();
                crate::model::cdp::preset::save_preset(
                    &key_str,
                    crate::model::cdp::preset::CdpPreset { name: name.clone(), values },
                );
                let new_presets = crate::model::cdp::preset::load_presets(&key_str, param_count);
                let new_selected = new_presets.iter().position(|p| p.name == name);
                if let Some(Dialog::CdpParams { presets, preset_selected, save_prompt, .. }) =
                    self.dialog.as_mut()
                {
                    *presets = new_presets;
                    *preset_selected = new_selected;
                    *save_prompt = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(Dialog::CdpParams { save_prompt: Some(input), .. }) = self.dialog.as_mut() {
                    input.backspace();
                }
            }
            KeyCode::Delete => {
                if let Some(Dialog::CdpParams { save_prompt: Some(input), .. }) = self.dialog.as_mut() {
                    input.delete();
                }
            }
            KeyCode::Left => {
                if let Some(Dialog::CdpParams { save_prompt: Some(input), .. }) = self.dialog.as_mut() {
                    input.left();
                }
            }
            KeyCode::Right => {
                if let Some(Dialog::CdpParams { save_prompt: Some(input), .. }) = self.dialog.as_mut() {
                    input.right();
                }
            }
            KeyCode::Home => {
                if let Some(Dialog::CdpParams { save_prompt: Some(input), .. }) = self.dialog.as_mut() {
                    input.home();
                }
            }
            KeyCode::End => {
                if let Some(Dialog::CdpParams { save_prompt: Some(input), .. }) = self.dialog.as_mut() {
                    input.end();
                }
            }
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                if let Some(Dialog::CdpParams { save_prompt: Some(input), .. }) = self.dialog.as_mut() {
                    input.insert(c);
                }
            }
            _ => {}
        }
    }

    /// Deletes the currently-selected preset ('d' key) from disk and the in-memory list,
    /// leaving the field values untouched (only the "this came from a saved preset" label
    /// goes away) — a no-op (returning `false`, falling through to typing 'd' as text) when
    /// there's no `Dialog::CdpParams` open, another sub-mode is active, or no preset is
    /// currently selected.
    fn delete_selected_cdp_preset(&mut self) -> bool {
        let Some(Dialog::CdpParams { catalog_index, presets, preset_selected, envelope, save_prompt, .. }) =
            self.dialog.as_mut()
        else {
            return false;
        };
        if envelope.is_some() || save_prompt.is_some() {
            return false;
        }
        let Some(index) = *preset_selected else { return false };
        let Some(name) = presets.get(index).map(|p| p.name.clone()) else { return false };
        let catalog_index = *catalog_index;
        let Some(def) = self.cdp_catalog.processes.get(catalog_index) else { return false };
        let key = def.key.clone();
        let param_count = def.params.len();
        crate::model::cdp::preset::delete_preset(&key, &name);
        let new_presets = crate::model::cdp::preset::load_presets(&key, param_count);
        if let Some(Dialog::CdpParams { presets, preset_selected, .. }) = self.dialog.as_mut() {
            *presets = new_presets;
            *preset_selected = None;
        }
        true
    }

    /// Whether `preview` was computed from exactly `values`/`range` (at the document's
    /// current sample rate) and can be spliced in directly instead of re-running CDP.
    /// Structural equality on `ParamValue` — a `TextInput` re-parsed to the same number
    /// counts as "unchanged" even if e.g. trailing whitespace differed, which is the right
    /// call since it's the *value* that matters. The sample-rate check guards against the
    /// (currently impossible, since no v1 CDP process changes rate) case of the document's
    /// rate having changed since Preview ran, which would otherwise splice audio recorded
    /// at the wrong speed.
    fn cdp_preview_matches(
        preview: &CdpPreview,
        values: &[crate::model::cdp::ParamValue],
        range: (usize, usize),
        sample_rate: u32,
    ) -> bool {
        preview.range == range && preview.values == values && preview.sample_rate == sample_rate
    }

    /// Validates every field against its `ParamKind` range, returning the index of the
    /// first invalid one. `None` means all fields are in range and safe to run.
    ///
    /// A `required_envelope` field additionally requires `envelope.is_some()` — its
    /// `input` never becomes the submitted value (see `CdpField::to_value` and
    /// `ParamDef::required_envelope`'s doc comment), so validating that stale text would
    /// pass or fail for reasons unrelated to whether the process can actually run.
    fn cdp_validate_fields(
        def: &crate::model::cdp::ProcessDef,
        fields: &[CdpField],
    ) -> Option<usize> {
        use crate::model::cdp::ParamKind;
        for (i, (param, field)) in def.params.iter().zip(fields).enumerate() {
            if let (ParamKind::Number { min, max, .. }, CdpField::Number { input, envelope, .. }) =
                (&param.kind, field)
            {
                if param.required_envelope {
                    if envelope.is_none() {
                        return Some(i);
                    }
                    continue;
                }
                match input.value().trim().parse::<f64>() {
                    Ok(v) if v >= *min && v <= *max => {}
                    _ => return Some(i),
                }
            }
            // A `required_list` field's `values` is the actual submitted value (`to_value`)
            // — same reasoning as `required_envelope` above, just for `CdpField::List`
            // instead: empty means "never configured," which must block Apply/Preview
            // rather than silently submitting an empty datafile. `handle_cdp_list_key`
            // already keeps a time-sequence field's `values` ascending by construction, but
            // a saved preset from before that constraint existed (or a hand-edited config)
            // could still carry an out-of-order list — checked here too as defense in
            // depth, since CDP rejects it outright ("Sync times out of sequence").
            if let CdpField::List { values, .. } = field {
                if values.is_empty() {
                    return Some(i);
                }
                if param.list_is_time_sequence && values.windows(2).any(|w| w[0] >= w[1]) {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Runs (or re-splices a matching cached Preview for) the process currently shown in
    /// `Dialog::CdpParams`. Shared by both the Preview and Apply actions — they differ only
    /// in `purpose` and in what `tick_cdp` does once the job finishes.
    fn cdp_run(&mut self, purpose: crate::cdp::JobPurpose) {
        use crate::model::cdp::IoKind;
        let Some(Dialog::CdpParams {
            catalog_index, fields, second_input, focus, preview, presets, preset_selected, ..
        }) = self.dialog.take()
        else {
            return;
        };
        let Some(def) = self.cdp_catalog.processes.get(catalog_index).cloned() else { return };

        // Every early-return below re-creates the dialog with an error; this closure keeps
        // those sites from each hand-copying the full field list (a missed field there
        // would silently drop e.g. the second-input selection on a validation error).
        // `envelope`/`save_prompt` are always `None` here — `handle_dialog_key` intercepts
        // every key while either sub-mode is open, so `cdp_run` can never be reached mid-edit.
        let reopen = |focus: usize, error: String, fields: Vec<CdpField>, second_input: Option<CdpSecondInput>, preview: Option<CdpPreview>, presets: Vec<crate::model::cdp::preset::CdpPreset>, preset_selected: Option<usize>| {
            Dialog::CdpParams {
                catalog_index, fields, second_input, focus, error: Some(error), preview,
                envelope: None, list_edit: None, presets, preset_selected, save_prompt: None, scroll: 0,
            }
        };

        if let Some(bad) = Self::cdp_validate_fields(&def, &fields) {
            self.dialog = Some(reopen(bad + 1, "value out of range".into(), fields, second_input, preview, presets, preset_selected));
            return;
        }

        let values: Vec<_> = fields.iter().map(CdpField::to_value).collect();
        let idx = self.active_document;
        let Some(doc) = self.documents.get(idx) else { return };

        let range = if def.input == IoKind::None {
            (doc.cursor, doc.cursor)
        } else {
            match self.operation_range(idx, self.snap_to_zero) {
                Some(r) => r,
                None => {
                    self.dialog = Some(reopen(focus, "no audio to process".into(), fields, second_input, preview, presets, preset_selected));
                    return;
                }
            }
        };

        // An unchanged-parameter Apply after a successful Preview splices the cached
        // result instead of re-running CDP.
        if matches!(purpose, crate::cdp::JobPurpose::Apply) {
            if let Some(cached) = &preview {
                if Self::cdp_preview_matches(cached, &values, range, doc.sample_rate) {
                    let label = format!("CDP: {}", def.title);
                    let channels = cached.channels.clone();
                    self.histories[idx].apply(
                        crate::commands::cdp::cdp_process_command(label, range, channels),
                        &mut self.documents[idx],
                    );
                    self.viewport = None;
                    self.after_sample_mutation(idx);
                    crate::model::cdp::recent::record_used(&def.key);
                    return;
                }
            }
        }

        // The second input (dual-input processes) is another open buffer, used whole — it's
        // source material, not a timeline being edited, so its selection is ignored.
        let second_doc_index = second_input.as_ref().and_then(CdpSecondInput::selected_doc_index);
        let mut input_specs = Vec::new();
        if def.input != IoKind::None {
            input_specs.push(crate::model::cdp::InputSpec {
                channels: doc.channel_count(),
                sample_rate: doc.sample_rate,
                len_samples: range.1 - range.0,
            });
        }
        if matches!(def.input, IoKind::DualWav | IoKind::DualAna) {
            let Some(doc_b) = second_doc_index.and_then(|i| self.documents.get(i)) else {
                self.dialog = Some(reopen(focus, "no second input buffer".into(), fields, second_input, preview, presets, preset_selected));
                return;
            };
            input_specs.push(crate::model::cdp::InputSpec {
                channels: doc_b.channel_count(),
                sample_rate: doc_b.sample_rate,
                len_samples: doc_b.len_samples(),
            });
        }

        let planned = crate::model::cdp::plan_job(&def, &values, &input_specs, &crate::model::cdp::PvocSettings::default());
        let planned = match planned {
            Ok(p) => p,
            Err(err) => {
                self.dialog = Some(reopen(focus, cdp_plan_error_message(&err), fields, second_input, preview, presets, preset_selected));
                return;
            }
        };

        let cdp_dir = std::path::PathBuf::from(&self.config.cdp_dir);
        if crate::cdp::validate_cdp_dir(&cdp_dir).is_err() {
            self.dialog = Some(Dialog::CdpSetup { input: TextInput::new(self.config.cdp_dir.clone()), error: Some("CDP directory no longer valid".into()) });
            return;
        }

        let mut inputs = Vec::new();
        if def.input != IoKind::None {
            inputs.push(doc.slice(range.0..range.1));
        }
        if let Some(doc_b) = second_doc_index.and_then(|i| self.documents.get(i)) {
            if matches!(def.input, IoKind::DualWav | IoKind::DualAna) {
                inputs.push(doc_b.channels.clone());
            }
        }
        let input_sample_rate = doc.sample_rate;
        let job_id = self.cdp_next_job_id;
        self.cdp_next_job_id += 1;
        let step_total = planned.steps.len();

        if let Some(audio) = self.audio.as_ref() {
            if audio.is_playing() {
                audio.pause();
            }
        }

        self.cdp_pending = Some(CdpPending {
            doc_index: idx,
            range,
            label: format!("CDP: {}", def.title),
            catalog_index,
            fields: fields.clone(),
            second_input,
            focus,
            presets,
            preset_selected,
        });
        self.cdp_runner.submit(crate::cdp::Job {
            id: job_id,
            cdp_dir,
            planned,
            inputs,
            input_sample_rate,
            purpose,
        });
        self.dialog = Some(Dialog::CdpRunning {
            job_id,
            title: def.title.clone(),
            step_label: "Starting…".into(),
            step_index: 0,
            step_total,
            started: std::time::Instant::now(),
            purpose,
        });
    }

    /// Drops the preview audition engine, if any — mirrors `stop_audition`. Called whenever
    /// the CDP dialog closes or a field edit invalidates the cached preview.
    fn stop_cdp_preview_audio(&mut self) {
        self.cdp_preview_audio = None;
    }

    /// Drains completed/in-progress events from `cdp_runner`, advancing `Dialog::CdpRunning`
    /// or (on completion) applying/auditioning the result. Called once per frame from the
    /// main loop, alongside `tick_audition`/`sync_playhead_from_audio` — the same pattern
    /// every other background-thread integration in this app uses to avoid blocking the UI
    /// thread on external work.
    ///
    /// Returns whether any event was processed — i.e. whether dialog/document state may
    /// have just changed without any input event. The run loop must redraw on `true`:
    /// its other redraw triggers are all input- or playhead-driven, so the frame where a
    /// finished job replaces `CdpRunning` with the result would otherwise sit stale on
    /// screen until the next keypress.
    fn tick_cdp(&mut self) -> bool {
        let mut processed_any = false;
        while let Ok(event) = self.cdp_runner.events.try_recv() {
            processed_any = true;
            match event {
                crate::cdp::CdpEvent::StepStarted { job, index, total, label } => {
                    if let Some(Dialog::CdpRunning { job_id, step_label, step_index, step_total, .. }) =
                        self.dialog.as_mut()
                    {
                        if *job_id == job {
                            *step_label = label;
                            *step_index = index;
                            *step_total = total;
                        }
                    }
                }
                crate::cdp::CdpEvent::Finished { job, purpose, result } => {
                    let Some(pending) = self.cdp_pending.take() else { continue };
                    // Only act on the job the currently-shown `CdpRunning` dialog is
                    // actually waiting on — guards against a stray event arriving after the
                    // dialog has already moved on for any reason.
                    if !matches!(self.dialog, Some(Dialog::CdpRunning { job_id, .. }) if job_id == job) {
                        continue;
                    }

                    // Only a successful Apply counts as "used" for `Dialog::CdpBrowser`'s
                    // Recent group — Preview is an audition, not a commitment. Looked up
                    // once here since both Apply arms below need it.
                    let recent_key = self.cdp_catalog.processes.get(pending.catalog_index).map(|d| d.key.clone());

                    match result {
                        Ok(mut output) => match purpose {
                            crate::cdp::JobPurpose::Apply if output.results.len() > 1 => {
                                // A glob-output process (e.g. distcut/envcut): each numbered
                                // file it produced becomes its own new buffer, the same "one
                                // new buffer per result" shape Action::NewFromLeft/NewFromRight
                                // already use, rather than being spliced into the selection —
                                // there's no single "the result" to splice.
                                let bits_per_sample = self
                                    .documents
                                    .get(pending.doc_index)
                                    .map(|d| d.bits_per_sample)
                                    .unwrap_or(32);
                                for channels in output.results.drain(..) {
                                    self.push_document(Document {
                                        channels,
                                        sample_rate: output.sample_rate,
                                        bits_per_sample,
                                        selection: None,
                                        cursor: 0,
                                        dirty: true,
                                        path: None,
                                        markers: Vec::new(),
                                        bext: None,
                                    });
                                }
                                self.viewport = None;
                                self.rebuild_audio();
                                self.rebuild_waveform_caches();
                                self.dialog = None;
                                if let Some(key) = &recent_key {
                                    crate::model::cdp::recent::record_used(key);
                                }
                            }
                            crate::cdp::JobPurpose::Apply => {
                                // Splicing raw samples at a different rate would play the
                                // result at the wrong speed — only reachable if the user
                                // overrode a synthesis process's Sample Rate away from the
                                // document's (no processing job changes rate).
                                let doc_rate =
                                    self.documents.get(pending.doc_index).map(|d| d.sample_rate);
                                if doc_rate != Some(output.sample_rate) {
                                    self.dialog = Some(Dialog::Info {
                                        message: format!(
                                            "Output is {} Hz but the document is {} Hz — set the process's sample rate to match.",
                                            output.sample_rate,
                                            doc_rate.unwrap_or(0)
                                        ),
                                    });
                                    continue;
                                }
                                let channels = output.results.into_iter().next().unwrap_or_default();
                                self.histories[pending.doc_index].apply(
                                    crate::commands::cdp::cdp_process_command(pending.label, pending.range, channels),
                                    &mut self.documents[pending.doc_index],
                                );
                                self.viewport = None;
                                self.after_sample_mutation(pending.doc_index);
                                self.dialog = None;
                                if let Some(key) = &recent_key {
                                    crate::model::cdp::recent::record_used(key);
                                }
                            }
                            crate::cdp::JobPurpose::Preview => {
                                // A glob-output process can't be meaningfully previewed as one
                                // audio stream (there's no single "the result"); play the
                                // first produced segment as a representative sample.
                                let channels = output.results.into_iter().next().unwrap_or_default();
                                self.cdp_preview_audio =
                                    AudioEngine::try_new(channels.clone(), output.sample_rate);
                                if let Some(audio) = &self.cdp_preview_audio {
                                    audio.play(0);
                                }
                                let values = pending.fields.iter().map(CdpField::to_value).collect();
                                self.dialog = Some(Dialog::CdpParams {
                                    catalog_index: pending.catalog_index,
                                    fields: pending.fields,
                                    second_input: pending.second_input,
                                    focus: pending.focus,
                                    error: None,
                                    preview: Some(CdpPreview {
                                        values,
                                        range: pending.range,
                                        channels,
                                        sample_rate: output.sample_rate,
                                    }),
                                    envelope: None,
                                    list_edit: None,
                                    presets: pending.presets,
                                    preset_selected: pending.preset_selected,
                                    save_prompt: None,
                                    scroll: 0,
                                });
                            }
                        },
                        Err(err) => {
                            self.dialog = Some(Dialog::CdpOutput {
                                title: "CDP Error".into(),
                                lines: cdp_error_lines(&err),
                                scroll: 0,
                            });
                        }
                    }
                }
            }
        }
        processed_any
    }

    /// Resets keybindings to factory defaults, saves, and rebuilds the key map + UI chrome.
    /// All other settings (snap, zoom, etc.) are preserved — only the `[keybindings]` table
    /// is replaced.
    fn reset_config_to_defaults(&mut self) {
        // Snapshot the current file to `<path>.bak` before overwriting it, so the reset is
        // recoverable.
        Config::backup_existing();
        let mut keybindings = std::collections::HashMap::new();
        fill_missing_keybindings(&mut keybindings);
        self.config.keybindings = keybindings.clone();
        self.save_config(); // persists; also snapshots current toggle state
        self.key_map = build_key_map(&keybindings);
        let menu_shortcuts = build_action_display_map(&keybindings, false);
        let toolbar_shortcuts = build_action_display_map(&keybindings, true);
        self.menu = MenuBar::new(&menu_shortcuts);
        // Preserve runtime toolbar state across the rebuild.
        let playing = self.toolbar.is_playing;
        let threshold = self.toolbar.transient_threshold_db;
        let active = std::mem::take(&mut self.toolbar.active_actions);
        self.toolbar = Toolbar::new(&toolbar_shortcuts);
        self.toolbar.is_playing = playing;
        self.toolbar.transient_threshold_db = threshold;
        self.toolbar.active_actions = active;
    }

    /// Snapshots the current toggle state into `self.config` and writes it to disk.
    /// Called right after any toggle action so the persisted file never lags behind
    /// what's actually in effect.
    fn save_config(&mut self) {
        let keybindings = self.config.keybindings.clone();
        let cdp_dir = self.config.cdp_dir.clone();
        self.config = Config {
            snap_to_zero: self.snap_to_zero,
            auto_vertical_zoom: self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom),
            fine_mode: self.fine_mode,
            loop_playback: self.loop_playback,
            audition: self.audition,
            cursor_follows_playback: self.cursor_follows_playback,
            viewport_follows_playback: self.viewport_follows_playback,
            transient_threshold_db: self.transient_threshold_db,
            graphics_mode: self.graphics_mode,
            cdp_dir,
            keybindings,
        };
        self.config.save();
    }

    /// Step multiplier for a held arrow key. Ramps on the *count* of consecutive repeats
    /// landing less than `NAV_FAST_REPEAT_GAP` apart — not on elapsed wall-clock time, which
    /// can't tell a held key apart from someone tapping it steadily: both rack up the same
    /// duration if the gaps just happen to all be short enough. A real hold's terminal
    /// auto-repeat fires every ~20-50ms and easily clears `NAV_ACCEL_START_REPS` within a
    /// fraction of a second; manual tapping can't sustain that many sub-gap repeats in a
    /// row, so it never accumulates enough count to ramp. Any repeat with a longer gap (or a
    /// different action) resets the count to 0. Always 1x in fine mode — fine stepping is
    /// for slow, precise movement, not covering ground quickly.
    fn nav_step_multiplier(&mut self, action: Action) -> f64 {
        const NAV_FAST_REPEAT_GAP: Duration = Duration::from_millis(120);
        const NAV_ACCEL_START_REPS: u32 = 5;
        const NAV_ACCEL_RAMP_REPS: u32 = 20;
        const NAV_MAX_MULTIPLIER: f64 = 8.0;

        let now = Instant::now();
        let is_fast_repeat = self.nav_hold_action == Some(action)
            && self.last_nav_time.is_some_and(|t| now.duration_since(t) < NAV_FAST_REPEAT_GAP);
        if is_fast_repeat {
            self.nav_repeat_count = self.nav_repeat_count.saturating_add(1);
        } else {
            self.nav_hold_action = Some(action);
            self.nav_repeat_count = 0;
        }
        self.last_nav_time = Some(now);

        if self.fine_mode || self.nav_repeat_count < NAV_ACCEL_START_REPS {
            return 1.0;
        }
        let t = ((self.nav_repeat_count - NAV_ACCEL_START_REPS) as f64 / NAV_ACCEL_RAMP_REPS as f64).min(1.0);
        1.0 + t * (NAV_MAX_MULTIPLIER - 1.0)
    }

    /// The panel that currently has focus — the single source of truth for the modal
    /// command panel, contextual keys, and the active-panel accent.
    fn focus(&self) -> Focus {
        if self.file_panel.focused {
            Focus::Files
        } else if self.buffer_panel.focused {
            Focus::Buffers
        } else {
            Focus::Waveform
        }
    }

    /// Cycles focus Waveform → Files → Buffers → Waveform.
    fn cycle_focus(&mut self) {
        match self.focus() {
            Focus::Waveform => {
                self.file_panel.focused = true;
                self.buffer_panel.focused = false;
            }
            Focus::Files => {
                self.file_panel.focused = false;
                self.buffer_panel.focused = true;
            }
            Focus::Buffers => {
                self.file_panel.focused = false;
                self.buffer_panel.focused = false;
            }
        }
    }

    /// Saves buffer `idx` to its existing path (no-op if it has none).
    fn save_buffer(&mut self, idx: usize) {
        if let Some(doc) = self.documents.get_mut(idx) {
            if let Some(path) = doc.path.clone() {
                if save_wav(doc, &path).is_ok() {
                    doc.dirty = false;
                    self.file_panel.mark_dirty(&path, false);
                }
            }
        }
    }

    /// Closes buffer `idx`, confirming first if it has unsaved changes.
    fn request_close_buffer(&mut self, idx: usize) {
        if self.documents.get(idx).is_some_and(|d| d.dirty) {
            self.confirm = Some(Confirm::CloseBuffer(idx));
        } else {
            self.close_buffer(idx);
        }
    }

    /// Removes buffer `idx` (and its parallel history), fixes the active index, and rebuilds
    /// derived state. Closing the last buffer leaves the empty state.
    fn close_buffer(&mut self, idx: usize) {
        if idx >= self.documents.len() {
            return;
        }
        self.documents.remove(idx);
        self.histories.remove(idx);
        if self.documents.is_empty() {
            self.active_document = 0;
            self.viewport = None;
            self.rebuild_audio();
            self.rebuild_waveform_caches();
            return;
        }
        // Keep the active index valid; bias toward the buffer that shifted into this slot.
        if self.active_document >= self.documents.len() {
            self.active_document = self.documents.len() - 1;
        } else if self.active_document > idx {
            self.active_document -= 1;
        }
        self.viewport = None;
        self.rebuild_audio();
        self.rebuild_waveform_caches();
    }

    /// Points the file panel at `input` (a directory path; `~` expands to $HOME). No-op if
    /// the path isn't an existing directory.
    fn open_directory(&mut self, input: &str) {
        let input = input.trim();
        if input.is_empty() {
            return;
        }
        let path = if input == "~" {
            dirs_home().map(PathBuf::from)
        } else if let Some(rest) = input.strip_prefix("~/") {
            dirs_home().map(|h| PathBuf::from(h).join(rest))
        } else {
            Some(PathBuf::from(input))
        };
        if let Some(path) = path {
            if path.is_dir() {
                self.file_panel.set_directory(path);
                self.file_panel.focused = true;
            }
        }
    }

    /// Renames buffer `idx` to `new_name`, renaming the file on disk if it has one (kept in
    /// the same directory). For an unsaved buffer it just sets the path for the next save.
    fn rename_buffer(&mut self, idx: usize, new_name: &str) {
        if new_name.is_empty() || idx >= self.documents.len() {
            return;
        }
        let old_path = self.documents[idx].path.clone();
        let parent = old_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.file_panel.directory.clone());
        let new_path = parent.join(new_name);
        if let Some(old) = old_path.as_ref() {
            if old.exists() && std::fs::rename(old, &new_path).is_err() {
                return; // leave the buffer untouched if the disk rename failed
            }
            self.file_panel.mark_dirty(old, false);
        }
        let dirty = self.documents[idx].dirty;
        self.documents[idx].path = Some(new_path.clone());
        self.file_panel.mark_dirty(&new_path, dirty);
        self.file_panel.scan();
    }

    /// Opens the rename dialog for the Files-panel selection, if it's a `.wav` file (not the
    /// `..` row or a subdirectory). Prefills the current name; Esc cancels.
    fn begin_rename_selected_file(&mut self) {
        if let Some((path, FileEntryKind::File)) = self.file_panel.selected_entry() {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            self.dialog = Some(Dialog::RenameFile { path, input: TextInput::fresh(name) });
        }
    }

    /// Asks to delete the Files-panel selection (if it's a `.wav` file) — deleting on disk is
    /// irreversible, so it goes through the confirmation modal rather than acting immediately.
    fn request_delete_selected_file(&mut self) {
        if let Some((path, FileEntryKind::File)) = self.file_panel.selected_entry() {
            self.confirm = Some(Confirm::DeleteFile(path));
        }
    }

    /// Renames `old_path` on disk to `new_name` (same directory), repointing any buffer open
    /// on it. No-op on an empty name, an unchanged name, or a failed rename.
    fn rename_file(&mut self, old_path: &Path, new_name: &str) {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return;
        }
        let parent = old_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.file_panel.directory.clone());
        let new_path = parent.join(new_name);
        if new_path == old_path || std::fs::rename(old_path, &new_path).is_err() {
            return;
        }
        // Keep any buffer open on this file pointed at the new name, carrying its dirty flag.
        for doc in &mut self.documents {
            if doc.path.as_deref() == Some(old_path) {
                doc.path = Some(new_path.clone());
            }
        }
        let was_dirty = self.file_panel.dirty_paths.contains(old_path);
        self.file_panel.mark_dirty(old_path, false);
        self.file_panel.mark_dirty(&new_path, was_dirty);
        self.file_panel.scan();
    }

    /// Deletes `path` from disk and refreshes the panel. A buffer open on it is left in memory
    /// (still re-savable); only its dirty marker is cleared.
    fn delete_file(&mut self, path: &Path) {
        if std::fs::remove_file(path).is_err() {
            return;
        }
        self.file_panel.mark_dirty(path, false);
        self.file_panel.scan();
    }

    fn apply_normalize(&mut self, target_db: f32) {
        let idx = self.active_document;
        let Some((start, end)) = self.operation_range(idx, self.snap_to_zero) else { return };
        self.histories[idx].apply(normalize_command(start, end, target_db), &mut self.documents[idx]);
        self.after_sample_mutation(idx);
    }

    fn apply_gain(&mut self, gains_db: Vec<f32>, tanh_clip: bool) {
        let idx = self.active_document;
        let Some((start, end)) = self.operation_range(idx, self.snap_to_zero) else { return };
        self.histories[idx].apply(gain_command(start, end, gains_db, tanh_clip), &mut self.documents[idx]);
        self.after_sample_mutation(idx);
    }

    /// Resamples the whole active document to `target_rate`. The sample count changes
    /// drastically, so the viewport is dropped to refit; `after_sample_mutation` notices the
    /// rate change and rebuilds the audio engine.
    fn apply_resample(&mut self, target_rate: u32) {
        let idx = self.active_document;
        let Some(doc) = self.documents.get(idx) else { return };
        if target_rate == 0 || target_rate == doc.sample_rate || doc.len_samples() == 0 {
            return;
        }
        self.histories[idx].apply(resample_command(target_rate), &mut self.documents[idx]);
        self.viewport = None;
        self.after_sample_mutation(idx);
    }

    /// Activates the selected file-panel entry: navigate into a directory (or `..`), or open
    /// a `.wav` file.
    fn open_selected_file(&mut self) {
        let Some((path, kind)) = self.file_panel.selected_entry() else {
            return;
        };
        match kind {
            FileEntryKind::Parent | FileEntryKind::Dir => self.file_panel.set_directory(path),
            FileEntryKind::File => self.load_file(path),
        }
    }

    /// Drops any audition playback/pending state. Dropping `AudioEngine` sends it a `Stop`
    /// and tears down its thread, so this is enough to silence it immediately.
    fn stop_audition(&mut self) {
        self.audition_audio = None;
        self.audition_playing_path = None;
        self.audition_pending = None;
    }

    /// Drives the Audition feature: called once per main-loop tick (same cadence as
    /// `sync_playhead_from_audio`). Watches the Files panel's selected entry and, after it
    /// settles on a `.wav` file for `AUDITION_DEBOUNCE`, plays that file straight from disk
    /// without loading it into a buffer — so skimming the list with Up/Down previews each
    /// file without a full decode-and-play on every single keypress.
    fn tick_audition(&mut self) {
        if !self.audition {
            if self.audition_audio.is_some() || self.audition_pending.is_some() {
                self.stop_audition();
            }
            return;
        }

        let current = if self.file_panel.focused {
            self.file_panel
                .selected_entry()
                .filter(|(_, kind)| *kind == FileEntryKind::File)
                .map(|(path, _)| path)
        } else {
            None
        };

        let already_on_target = self.audition_playing_path == current
            || self.audition_pending.as_ref().map(|(p, _)| p) == current.as_ref();
        if !already_on_target {
            // Selection moved to a different file (or off the file panel entirely) — stop
            // whatever was playing/pending right away; only the *new* target gets debounced.
            self.audition_audio = None;
            self.audition_playing_path = None;
            self.audition_pending = current.clone().map(|path| (path, Instant::now()));
        }

        if let Some((path, started)) = self.audition_pending.clone() {
            if Instant::now().duration_since(started) >= AUDITION_DEBOUNCE {
                self.audition_pending = None;
                if let Ok(document) = crate::model::io::load_wav(&path) {
                    self.audition_audio = AudioEngine::try_new(document.channels, document.sample_rate);
                    if let Some(engine) = &self.audition_audio {
                        engine.play(0);
                    }
                    self.audition_playing_path = Some(path);
                }
            }
        }
    }

    fn apply_fade(&mut self, fade_in: bool, pct: f32, curve: FadeCurve) {
        let idx = self.active_document;
        // Try snapping both endpoints to zero crossings so the boundary between faded and
        // unfaded audio lands near a near-zero sample (avoids a click where the envelope
        // hits 0 but the adjacent unfaded sample is still loud). If snapping collapses a
        // small selection to a degenerate range (both ends snap to the same crossing), fall
        // back to the un-snapped range — the fade is still applied, just without the snap.
        let Some((start, end)) = self.fade_operation_range(idx, fade_in, true)
            .or_else(|| self.fade_operation_range(idx, fade_in, false)) else { return };
        let fade_samples = ((end - start) as f32 * pct / 100.0).round() as usize;
        let fade_samples = fade_samples.max(1).min(end - start);
        let (fade_start, fade_end) = if fade_in {
            (start, start + fade_samples)
        } else {
            (end - fade_samples, end)
        };
        if fade_start >= fade_end || fade_end > end { return; }
        self.histories[idx].apply(fade_command(fade_start, fade_end, fade_in, curve), &mut self.documents[idx]);
        self.after_sample_mutation(idx);
    }

    /// Moves the cursor to `pos` (the result of a Next/Previous Rising Edge search) and
    /// re-centers the viewport on it, rather than just nudging it into view — at any
    /// meaningful zoom level the edge would otherwise land right at the screen's margin,
    /// not given the surrounding context a transient-finding jump is actually for.
    fn jump_to_transient(&mut self, pos: usize) {
        if let Some(document) = self.active_doc_mut() {
            document.cursor = pos;
        }
        let width = self.content_width;
        if let Some(viewport) = self.viewport.as_mut() {
            viewport.center_on(pos, width);
        }
        if let Some(audio) = &self.audio {
            if audio.is_playing() {
                audio.seek(pos);
            }
        }
    }

    /// A short exponential fade in at the very start of the file and fade out at the very
    /// end (the standard pre-export "technical fade" to mask the click a hard cut to/from
    /// silence would otherwise leave at the file's boundaries) — fixed at 5ms, no dialog,
    /// always the whole file regardless of any active selection.
    fn apply_technical_fades(&mut self) {
        const TECHNICAL_FADE_MS: f64 = 5.0;
        let idx = self.active_document;
        let Some(document) = self.active_doc() else { return };
        let fade_len = ((document.sample_rate as f64 * TECHNICAL_FADE_MS / 1000.0).round() as usize).max(1);
        self.histories[idx].apply(technical_fades_command(fade_len), &mut self.documents[idx]);
        self.after_sample_mutation(idx);
    }

    /// Mix source channels to a new mono document according to per-channel dB gains.
    /// "-inf" in a field means that channel contributes nothing to the mix.
    /// If `tanh_clip` is true, applies a tanh soft-limiter to the mixed output.
    fn apply_mix_to_mono(&mut self, inputs: &[TextInput], tanh_clip: bool) {
        let Some(src) = self.active_doc() else { return };
        if src.channels.is_empty() {
            return;
        }
        // If there's an active selection, operate only on that range.
        let (range_start, range_len) = match src.selection.map(|s| s.normalized()) {
            Some((s, e)) if s < e => (s, e - s),
            _ => (0, src.channels[0].len()),
        };
        let sample_rate = src.sample_rate;
        let bits_per_sample = src.bits_per_sample;
        let range_end = range_start + range_len;
        let new_markers: Vec<Marker> = src.markers.iter()
            .filter(|m| m.position >= range_start && m.position < range_end)
            .map(|m| Marker { position: m.position - range_start, label: m.label.clone() })
            .collect();

        // Parse gains: "-inf" or any parse failure → 0.0 linear (silence that channel).
        let gains: Vec<f32> = inputs
            .iter()
            .enumerate()
            .map(|(i, ti)| {
                let raw = ti.value().trim().to_lowercase();
                if raw == "-inf" || raw.is_empty() {
                    return 0.0f32;
                }
                match raw.parse::<f32>() {
                    Ok(db) => dsp::db_to_linear(db),
                    Err(_) => if i < src.channels.len() { 1.0 } else { 0.0 },
                }
            })
            .collect();

        let n_ch = src.channels.len().min(gains.len());
        let mut mixed = vec![0.0f32; range_len];
        for (ch_idx, gain) in gains.iter().enumerate().take(n_ch) {
            if *gain == 0.0 {
                continue;
            }
            let ch_slice = &src.channels[ch_idx][range_start..range_start + range_len];
            for (s, &v) in mixed.iter_mut().zip(ch_slice.iter()) {
                *s += v * gain;
            }
        }

        if tanh_clip {
            for s in &mut mixed {
                *s = s.tanh();
            }
        }

        let new_doc = Document {
            channels: vec![mixed],
            sample_rate,
            bits_per_sample,
            selection: None,
            cursor: 0,
            dirty: true,
            path: None,
            markers: new_markers,
            bext: None,
        };
        self.dialog = None;
        self.push_document(new_doc);
        self.histories.last_mut().unwrap().created_by_copy_to_new = true;
        self.viewport = None;
        self.rebuild_audio();
        self.rebuild_waveform_caches();
    }

    /// Chops the active document at its markers and saves each region as a numbered WAV file
    /// into `subfolder` (created inside the document's directory, or the file panel's current
    /// directory for unsaved buffers). Files are named `{base_name}-001.wav`, `-002.wav`, …
    /// The first region spans [0, first_marker), the last spans [last_marker, end).
    ///
    /// See [`RegionExportOptions`] for the optional per-region processing.
    ///
    /// Per-region processing, in order:
    /// 1. Limit length (`opts.limit_length_ms`): truncates the region's end so it's no
    ///    longer than the given duration. Done first so a shorter region is what actually
    ///    gets normalized/faded, rather than measuring/tapering audio that's about to be cut.
    /// 2. Normalize (`opts.normalize_db`): scales the region so its peak sample hits the
    ///    target dBFS, independently per region (each region can have a different peak).
    /// 3. Fade in/out (`opts.fade_*_ms`): a cosine-curve (exp²) fade applied last, so the
    ///    envelope taper itself is never included in the length/peak measurements above.
    fn export_regions(
        &mut self,
        subfolder: &str,
        base_name: &str,
        depth: BitDepth,
        dither: bool,
        opts: RegionExportOptions,
    ) {
        let idx = self.active_document;
        let Some(doc) = self.documents.get(idx) else { return };
        if doc.markers.is_empty() { return; }

        // Determine the output directory: sibling of the document's file, or current panel dir.
        let parent_dir = doc.path.as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.file_panel.directory.clone());

        let out_dir = parent_dir.join(subfolder);
        if std::fs::create_dir_all(&out_dir).is_err() {
            self.dialog = Some(Dialog::Info {
                message: format!("Could not create folder: {subfolder}"),
            });
            return;
        }

        // Build region boundaries: [0, m0), [m0, m1), …, [m_last, end).
        let total = doc.len_samples();
        let mut boundaries: Vec<usize> = vec![0];
        let mut sorted: Vec<usize> = doc.markers.iter().map(|m| m.position).collect();
        sorted.sort_unstable();
        sorted.dedup();
        boundaries.extend_from_slice(&sorted);
        boundaries.push(total);

        let sample_rate = doc.sample_rate;
        let bits = doc.bits_per_sample;

        // Collect (start, end, region_markers) for each segment.
        let regions: Vec<(usize, usize, Vec<Marker>)> = boundaries.windows(2).map(|w| {
            let (start, end) = (w[0], w[1]);
            let region_markers: Vec<Marker> = doc.markers.iter()
                .filter(|m| m.position > start && m.position < end)
                .map(|m| Marker { position: m.position - start, label: m.label.clone() })
                .collect();
            (start, end, region_markers)
        }).filter(|(s, e, _)| s < e).collect();

        let channels_snapshot: Vec<Vec<f32>> = doc.channels.clone();

        let limit_length_samples = opts
            .limit_length_ms
            .filter(|&ms| ms > 0.0)
            // A sub-sample limit (e.g. 0.01 ms at 44.1 kHz) rounds to 0 samples, which
            // would truncate every region to an empty WAV while still reporting success —
            // keep at least one sample.
            .map(|ms| ms_to_samples(ms, sample_rate).max(1));
        let fade_in_len = opts.fade_in_ms.map_or(0, |ms| ms_to_samples(ms, sample_rate));
        let fade_out_len = opts.fade_out_ms.map_or(0, |ms| ms_to_samples(ms, sample_rate));

        let mut error: Option<String> = None;
        for (i, (start, end, region_markers)) in regions.iter().enumerate() {
            let file_name = format!("{base_name}-{:03}.wav", i + 1);
            let path = out_dir.join(&file_name);
            // Limit length: trim the end so the region can't exceed the given duration —
            // done before normalize/fades so both act on the audio that's actually kept.
            // Applied by clamping the copy itself: copying the whole region only to
            // truncate it would move (and keep allocated) the potentially huge cut-off
            // part for nothing.
            let copy_end = limit_length_samples.map_or(*end, |max_len| (*start + max_len).min(*end));
            let mut region_channels: Vec<Vec<f32>> = channels_snapshot.iter()
                .map(|ch| ch[*start..copy_end].to_vec())
                .collect();
            let region_len = copy_end - start;
            let mut region_markers = region_markers.clone();
            region_markers.retain(|m| m.position < region_len);

            // Normalize this region independently to its own peak, before fades — a fade
            // only ever attenuates, so measuring the peak after fading would risk chasing a
            // peak the fade itself just reduced instead of the region's true loudest sample.
            if let Some(target_db) = opts.normalize_db {
                if let Some(gain) = dsp::normalize_gain(dsp::peak(&region_channels), target_db) {
                    for ch in &mut region_channels {
                        for s in ch.iter_mut() {
                            *s *= gain;
                        }
                    }
                }
            }
            // Apply fade in (exp² ramp: t² gives a convex rise from silence).
            if fade_in_len > 0 {
                let len = fade_in_len.min(region_len);
                for ch in &mut region_channels {
                    for (i, s) in ch[..len].iter_mut().enumerate() {
                        let t = i as f32 / len.max(1) as f32;
                        *s *= t * t;
                    }
                }
            }
            // Apply fade out (exp² fall: (1-t)² gives a convex fall to silence).
            if fade_out_len > 0 {
                let len = fade_out_len.min(region_len);
                let offset = region_len - len;
                for ch in &mut region_channels {
                    for (i, s) in ch[offset..].iter_mut().enumerate() {
                        let t = i as f32 / len.max(1) as f32;
                        *s *= (1.0 - t) * (1.0 - t);
                    }
                }
            }
            let region_doc = Document {
                channels: region_channels,
                sample_rate,
                bits_per_sample: bits,
                selection: None,
                cursor: 0,
                dirty: false,
                path: None,
                markers: region_markers,
                bext: None,
            };
            if let Err(e) = crate::model::io::save_wav_with(&region_doc, &path, depth, dither) {
                error = Some(format!("Save failed: {e}"));
                break;
            }
        }

        self.dialog = None;
        if let Some(msg) = error {
            self.dialog = Some(Dialog::Info { message: msg });
        } else {
            let n = regions.len();
            self.dialog = Some(Dialog::Info {
                message: format!("Saved {n} region{} to {subfolder}/", if n == 1 { "" } else { "s" }),
            });
            // Refresh the file panel so the new subfolder appears.
            self.file_panel.scan();
        }
    }

    fn load_file(&mut self, path: PathBuf) {
        self.stop_audition();
        if let Some(audio) = self.audio.take() {
            drop(audio);
        }
        match crate::model::io::load_wav(&path) {
            Ok(mut document) => {
                self.file_panel.focused = false;
                self.file_panel.filtering = false;
                self.file_panel.filter.clear();

                document.dirty = false;
                // Check if this path is already open
                if let Some(pos) = self.documents.iter().position(|d| d.path == Some(path.clone())) {
                    self.active_document = pos;
                } else {
                    self.push_document(document);
                }
                self.viewport = None;
                self.rebuild_audio();
                self.rebuild_waveform_caches();
            }
            Err(e) => {
                let _ = e;
            }
        }
    }

    /// Saves every dirty document that already has a path. Documents that were never
    /// saved (no path) are skipped — Save All can't choose a filename for each; those still
    /// need an explicit Save As.
    fn save_all(&mut self) {
        for document in &mut self.documents {
            if !document.dirty {
                continue;
            }
            if let Some(path) = document.path.clone() {
                if save_wav(document, &path).is_ok() {
                    document.dirty = false;
                    self.file_panel.mark_dirty(&path, false);
                }
            }
        }
    }

    /// Saves every dirty buffer that already has a path immediately, then walks any
    /// never-saved dirty buffers through a Save As prompt each, one at a time, before
    /// actually quitting — `save_all` alone would otherwise silently skip (and lose) them.
    fn begin_save_all_then_quit(&mut self) {
        self.save_all();
        let unnamed: Vec<usize> =
            self.documents.iter().enumerate().filter(|(_, d)| d.dirty && d.path.is_none()).map(|(i, _)| i).collect();
        if unnamed.is_empty() {
            self.should_quit = true;
            return;
        }
        self.queue_save_as(unnamed, SaveAsQueueThen::Quit);
    }

    /// Starts (or continues) a queued Save-As sequence: `indices` in the order they should
    /// be prompted, `then` run once they're all done.
    fn queue_save_as(&mut self, mut indices: Vec<usize>, then: SaveAsQueueThen) {
        indices.reverse(); // popped from the back, so store back-to-front for prompt order
        self.save_as_queue = indices;
        self.save_as_queue_then = Some(then);
        self.advance_save_as_queue();
    }

    /// Opens the Save As prompt for the next buffer in `save_as_queue`, or — once it's
    /// empty — closes the prompt and runs whatever `save_as_queue_then` says to do next.
    fn advance_save_as_queue(&mut self) {
        let Some(idx) = self.save_as_queue.pop() else {
            self.save_as_active = false;
            match self.save_as_queue_then.take() {
                Some(SaveAsQueueThen::Quit) => self.should_quit = true,
                Some(SaveAsQueueThen::CloseBuffer(idx)) => self.close_buffer(idx),
                None => {}
            }
            return;
        };
        self.active_document = idx;
        let name = self
            .documents
            .get(idx)
            .and_then(|d| d.path.as_ref())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled.wav".to_string());
        self.save_as_input = TextInput::fresh(name);
        self.save_as_focused = 0;
        // Default to the document's original bit depth so a Save As on a 24-bit file
        // offers 24-bit by default rather than always falling back to 32-bit float.
        self.save_as_depth = self.documents.get(idx)
            .map(|d| BitDepth::from_bits(d.bits_per_sample))
            .unwrap_or(BitDepth::Float32);
        self.save_as_dither = false;
        self.save_as_active = true;
    }

    fn rebuild_audio(&mut self) {
        if let Some(document) = self.active_doc() {
            let rate = document.sample_rate;
            self.audio = AudioEngine::try_new(document.channels.clone(), rate);
            self.audio_sample_rate = Some(rate);
        } else {
            self.audio = None;
            self.audio_sample_rate = None;
        }
    }

    fn sync_playhead_from_audio(&mut self) {
        let Some(audio) = self.audio.as_ref() else { return };
        if audio.playing.load(std::sync::atomic::Ordering::Relaxed) {
            let pos = audio.position.load(std::sync::atomic::Ordering::Relaxed);
            // A finished non-looping source leaves the position one past the end
            // (== len_samples); clamp to the last valid sample index so the playhead never
            // lands a column past the right edge. Mirrors the total_len-1 clamp the cursor
            // navigation already uses everywhere else.
            let total_len = self
                .documents
                .get(self.active_document)
                .map(|d| d.len_samples())
                .unwrap_or(0);
            self.playhead_position = Some(pos.min(total_len.saturating_sub(1)));
        } else {
            self.playhead_position = None;
        }
    }

    /// Moves the insertion point (cursor) to `pos` and scrolls it into view — the "Insertion
    /// Point Follows Playback" snap, factored out so it's testable without a real
    /// `AudioEngine` (the only other caller, `handle_playback_action`, is gated on one).
    fn snap_cursor_to(&mut self, pos: usize) {
        if let Some(document) = self.active_doc_mut() {
            document.cursor = pos;
        }
        let width = self.content_width;
        if let Some(viewport) = self.viewport.as_mut() {
            viewport.ensure_visible(pos, width);
        }
    }

    /// Drives "Viewport Follows Playback": while playing, once the playhead reaches the
    /// right edge of the view, recenter on it and keep recentering every frame from then
    /// on — `viewport_following` is what makes that sticky (continuous scrolling) instead
    /// of a one-off snap that would otherwise only refire each time the recentered edge is
    /// reached again. Works at any zoom level since it operates purely in sample space via
    /// `Viewport::center_on`, not in fixed pixel/column terms.
    fn tick_viewport_follow(&mut self) {
        if !self.viewport_follows_playback {
            self.viewport_following = false;
            return;
        }
        let Some(playhead) = self.playhead_position else {
            self.viewport_following = false;
            return;
        };
        let width = self.content_width;
        if !self.viewport_following {
            let Some(viewport) = self.viewport.as_ref() else { return };
            let col = (playhead.saturating_sub(viewport.scroll_offset)) as f64 / viewport.samples_per_column;
            if col + 1.0 < width as f64 {
                return; // still comfortably inside the view — nothing to do yet
            }
            self.viewport_following = true;
        }
        if let Some(viewport) = self.viewport.as_mut() {
            viewport.center_on(playhead, width);
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        // Checked first since it needs Drag/Up events too, unlike the generic dialog-click
        // handling just below (which only ever reacts to Down) — returns `false` immediately
        // whenever the envelope editor isn't open, falling through to that generic handling
        // unaffected for every other dialog.
        if self.try_handle_cdp_envelope_mouse(mouse) {
            return;
        }
        // When a dialog or Save-As prompt is open, absorb all mouse events so clicks on the
        // waveform/panels behind it don't fire. Route left-clicks that land on an interactive
        // row to the appropriate handler; everything else is just swallowed.
        if self.dialog.is_some() || self.save_as_active {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let pos = Position::new(mouse.column, mouse.row);
                let hit = self
                    .dialog_row_rects
                    .iter()
                    .enumerate()
                    .find(|(_, r)| r.contains(pos))
                    .map(|(row, r)| (row, pos.x.saturating_sub(r.x)));
                if let Some((row, x_in_row)) = hit {
                    self.handle_dialog_row_click(row, x_in_row);
                }
            }
            return;
        }

        // The menu takes precedence over everything beneath it, because it renders on top:
        // a click on the bar opens (or switches) it, and while it's open a click selects an
        // entry or dismisses it. This must run before the panel/waveform handlers below — an
        // open dropdown can overlap the Files/Buffers panels, and without this the panel would
        // steal a click meant for a menu entry. Mirrors the keyboard precedence, where an open
        // menu intercepts keys before anything else.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(idx) = self.menu.hit_test_bar(mouse.column, mouse.row) {
                self.menu.toggle_open(idx);
                return;
            }
            if self.menu.is_open() {
                if let Some(entry_idx) = self.menu.hit_test_entry(mouse.column, mouse.row) {
                    self.menu.select_entry(entry_idx);
                    if let Some(action) = self.menu.activate() {
                        self.handle_action(action);
                    }
                } else {
                    self.menu.close();
                }
                return;
            }
        }

        // A left click anywhere focuses whichever panel it landed in — including the
        // waveform, which has no toggle/key of its own to focus it (Tab cycles forward
        // through panels, but a direct click should jump straight to the one under the
        // cursor). Checked before any other handling below so every click path (menu,
        // toolbar, panel entries, waveform seek/select) starts from the right focus.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let pos = Position::new(mouse.column, mouse.row);
            if self.file_panel_area.contains(pos) {
                self.file_panel.focused = true;
                self.buffer_panel.focused = false;
            } else if self.buffer_panel_area.contains(pos) {
                self.buffer_panel.focused = true;
                self.file_panel.focused = false;
            } else if self.waveform_area.contains(pos) {
                self.file_panel.focused = false;
                self.buffer_panel.focused = false;
            }
        }

        // File panel: a single click only selects (auditioning it, if Audition is on, via
        // `tick_audition`); a double-click activates it (navigate dir / open file) — mirrors
        // the double-click-to-rename convention used for marker labels elsewhere.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if self.file_panel.handle_click(mouse.column, mouse.row) {
                self.file_panel.focused = true;
                let now = Instant::now();
                let is_double_click = self.last_file_click.is_some_and(|(t, x, y)| {
                    now.duration_since(t) < Duration::from_millis(400)
                        && x.abs_diff(mouse.column) <= 1
                        && y == mouse.row
                });
                self.last_file_click = Some((now, mouse.column, mouse.row));
                if is_double_click {
                    self.last_file_click = None;
                    self.open_selected_file();
                }
                return;
            }
        }

        // Buffer panel: click to switch active buffer.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(idx) = self.buffer_panel.hit_test(mouse.column, mouse.row) {
                self.buffer_panel.selected = idx;
                self.switch_to_buffer(idx);
                return;
            }
        }

        // Toolbar: click a button to run its action. (The menu is handled earlier, above the
        // panels, since an open dropdown can overlap them; the toolbar sits in its own chrome
        // band and never overlaps a panel, so it's fine to resolve it here.)
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(action) = self.toolbar.hit_test(mouse.column, mouse.row) {
                self.handle_action(action);
                return;
            }
        }

        // Marker interaction (drag a line to move it, double-click a label to rename) takes
        // priority over selection when the press lands on a marker.
        if self.try_handle_marker_mouse(mouse) {
            return;
        }

        // Waveform click/drag → seek + select.
        let area = self.waveform_area;
        if mouse.column < area.x
            || mouse.column >= area.x + area.width
            || mouse.row < area.y
            || mouse.row >= area.y + area.height
        {
            if matches!(mouse.kind, MouseEventKind::Up(_)) {
                self.mouse_down_anchor = None;
            }
            return;
        }
        let loop_range = if self.loop_playback {
            self.active_doc().map(|d| {
                d.selection.map(|sel| sel.normalized()).unwrap_or((0, d.len_samples()))
            })
        } else {
            None
        };

        let idx = self.active_document;
        let Some(viewport) = self.viewport.as_ref() else { return };
        let Some(document) = self.documents.get_mut(idx) else { return };
        let total_len = document.len_samples();
        if total_len == 0 {
            return;
        }
        let col = (mouse.column - area.x) as f64;
        let target =
            (viewport.scroll_offset as f64 + col * viewport.samples_per_column) as usize;
        let target = target.min(total_len - 1);
        let snap = self.snap_to_zero;
        let target = if snap { document.snap_to_zero_crossing(target) } else { target };
        // Selection bounds are exclusive-end ([start, end) everywhere), so when the pointer
        // sits in the column that visually contains end-of-file, the selection edge must be
        // total_len — with the plain `target` (the column's *first* sample, further clamped
        // to the last sample index) a mouse selection could never include the file's final
        // samples, and "select to the end, delete" left an orphaned sliver behind. The
        // cursor keeps using `target`: it's a sample index, not a bound.
        let col_end =
            (viewport.scroll_offset as f64 + (col + 1.0) * viewport.samples_per_column) as usize;
        let sel_edge = if col_end >= total_len { total_len } else { target };

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let now = Instant::now();
                let is_double_click = self.last_waveform_click.is_some_and(|(t, x, y)| {
                    now.duration_since(t) < Duration::from_millis(400)
                        && x.abs_diff(mouse.column) <= 1
                        && y == mouse.row
                });
                self.last_waveform_click = Some((now, mouse.column, mouse.row));

                if is_double_click && !mouse.modifiers.contains(KeyModifiers::CONTROL) {
                    // Select the region bounded by the nearest marker at-or-before the click
                    // and the nearest marker after it — or the start/end of the file when
                    // there's no marker on that side — same as Audacity/Sound Forge's
                    // double-click-between-markers gesture.
                    self.last_waveform_click = None;
                    let region_start = document
                        .markers
                        .iter()
                        .map(|m| m.position)
                        .filter(|&p| p <= target)
                        .max()
                        .unwrap_or(0);
                    let region_end = document
                        .markers
                        .iter()
                        .map(|m| m.position)
                        .filter(|&p| p > target)
                        .min()
                        .unwrap_or(total_len);
                    let (region_start, region_end) = if snap {
                        document.snap_range_to_zero_crossing(region_start, region_end)
                    } else {
                        (region_start, region_end)
                    };
                    if region_start < region_end {
                        document.selection = Some(Selection { start: region_start, end: region_end });
                        document.cursor = region_start;
                    }
                    self.mouse_down_anchor = None;
                } else if mouse.modifiers.contains(KeyModifiers::CONTROL) {
                    // Ctrl extends the existing selection: pin the *far* edge as the drag
                    // anchor and move the near edge to the click — then the normal Drag branch
                    // (which reads `mouse_down_anchor`) keeps extending as the mouse moves.
                    // `target` is already zero-crossing-snapped above.
                    let anchor = if let Some(sel) = document.selection {
                        let (sel_start, sel_end) = sel.normalized();
                        // Keep whichever edge is farther from the click fixed.
                        if target.abs_diff(sel_start) <= target.abs_diff(sel_end) { sel_end } else { sel_start }
                    } else {
                        document.cursor
                    };
                    document.selection = Some(Selection { start: anchor, end: sel_edge });
                    document.cursor = anchor.min(target);
                    self.mouse_down_anchor = Some(anchor);
                } else {
                    document.selection = None;
                    let anchor = if snap { document.snap_to_zero_crossing(target) } else { target };
                    document.cursor = anchor;
                    self.mouse_down_anchor = Some(anchor);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(anchor) = self.mouse_down_anchor {
                    let start = anchor.min(target);
                    document.cursor = start;
                    document.selection = Some(Selection {
                        start: anchor,
                        end: sel_edge,
                    });
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(anchor) = self.mouse_down_anchor {
                    if anchor != target {
                        let start = anchor.min(target);
                        document.cursor = start;
                        document.selection = Some(Selection {
                            start: anchor,
                            end: sel_edge,
                        });
                    }
                }
                self.mouse_down_anchor = None;
            }
            _ => return,
        }

        if let Some(audio) = &self.audio {
            if audio.is_playing() {
                if let Some((ls, le)) = loop_range {
                    audio.seek_looped(document.cursor, ls, le);
                } else {
                    audio.seek(document.cursor);
                }
            }
        }
    }

    /// Full mouse support for the CDP envelope editor: click selects the nearest breakpoint,
    /// double-click inserts a new one at the exact cursor position, click-and-drag moves the
    /// selected point, Shift+drag moves it at reduced speed for finer control, and
    /// Shift+click deletes the nearest point (respecting the floor of 2 breakpoints). Always
    /// returns `true` (event consumed) once the editor is confirmed open — every mouse event
    /// while any dialog is up is already swallowed by the caller, so this only has to decide
    /// *what to do* with it, never whether to let it through to whatever's behind the popup.
    /// Returns `false` immediately when the editor isn't open, so `handle_mouse` falls
    /// through to its normal dialog-click handling for every other dialog unaffected by this.
    fn try_handle_cdp_envelope_mouse(&mut self, mouse: MouseEvent) -> bool {
        if !matches!(self.dialog, Some(Dialog::CdpParams { envelope: Some(_), .. })) {
            return false;
        }
        // The grid rect is stashed via `render_cdp_envelope_editor`'s return value (see
        // `cdp_envelope_layout`, the single source of truth both sides use) into
        // `dialog_row_rects` by `App::render`, exactly like every other dialog's click rects.
        let Some(&grid) = self.dialog_row_rects.first() else { return true };

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let in_grid = mouse.column >= grid.x
                    && mouse.column < grid.x + grid.width
                    && mouse.row >= grid.y
                    && mouse.row < grid.y + grid.height;
                if !in_grid {
                    return true;
                }
                let Some(Dialog::CdpParams { envelope: Some(edit), fields, .. }) = self.dialog.as_mut() else {
                    return true;
                };
                let Some(CdpField::Number { min, max, .. }) = fields.get(edit.field_index) else { return true };
                let (min, max, time_max) = (*min, *max, edit.time_max);
                let (t, v) = cdp_envelope_mouse_to_domain(grid, time_max, min, max, mouse.column, mouse.row);

                let now = Instant::now();
                let is_double = self.last_cdp_envelope_click.is_some_and(|(t0, x0, y0)| {
                    now.duration_since(t0) < Duration::from_millis(400)
                        && x0.abs_diff(mouse.column) <= 1
                        && y0.abs_diff(mouse.row) <= 1
                });

                let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = self.dialog.as_mut() else {
                    return true;
                };
                if is_double {
                    self.last_cdp_envelope_click = None;
                    let insert_at = edit.points.partition_point(|&(pt, _)| pt < t);
                    edit.points.insert(insert_at, (t, v.clamp(min, max)));
                    edit.selected = insert_at;
                    return true;
                }
                self.last_cdp_envelope_click = Some((now, mouse.column, mouse.row));

                let Some((nearest_idx, _)) =
                    cdp_envelope_nearest_point(&edit.points, grid, time_max, min, max, mouse.column, mouse.row)
                else {
                    return true;
                };

                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                    if edit.points.len() > 2 {
                        edit.points.remove(nearest_idx);
                        edit.selected = edit.selected.min(edit.points.len() - 1);
                    }
                    return true;
                }

                edit.selected = nearest_idx;
                let (pt, pv) = edit.points[nearest_idx];
                self.dragging_cdp_point = Some(nearest_idx);
                self.dragging_cdp_point_anchor = Some((mouse.column, mouse.row, pt, pv));
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(point_idx) = self.dragging_cdp_point else { return true };
                let Some((anchor_col, anchor_row, anchor_t, anchor_v)) = self.dragging_cdp_point_anchor else {
                    return true;
                };
                let Some(Dialog::CdpParams { envelope: Some(edit), fields, .. }) = self.dialog.as_mut() else {
                    return true;
                };
                let Some(CdpField::Number { min, max, .. }) = fields.get(edit.field_index) else { return true };
                let (min, max) = (*min, *max);

                // Shift scales the mouse delta down before it's converted to a domain delta,
                // so the same physical movement produces a smaller change — precision
                // dragging, not a different mapping (there's no "finer" grid to snap to,
                // the mapping is already continuous).
                let scale = if mouse.modifiers.contains(KeyModifiers::SHIFT) { 4.0 } else { 1.0 };
                let dx = (mouse.column as f64 - anchor_col as f64) / scale;
                let dy = (mouse.row as f64 - anchor_row as f64) / scale;
                let dt = if grid.width <= 1 { 0.0 } else { dx / (grid.width - 1) as f64 * edit.time_max };
                // Row increases downward on screen but value increases upward, hence the sign flip.
                let dv = if grid.height <= 1 { 0.0 } else { -dy / (grid.height - 1) as f64 * (max - min) };

                let lower = if point_idx == 0 { 0.0 } else { edit.points[point_idx - 1].0 + 0.001 };
                let upper = if point_idx + 1 == edit.points.len() {
                    edit.time_max
                } else {
                    edit.points[point_idx + 1].0 - 0.001
                };
                let new_t = (anchor_t + dt).clamp(lower.min(upper), upper.max(lower));
                let new_v = (anchor_v + dv).clamp(min, max);

                if let Some(p) = edit.points.get_mut(point_idx) {
                    *p = (new_t, new_v);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging_cdp_point = None;
                self.dragging_cdp_point_anchor = None;
                true
            }
            _ => true,
        }
    }

    /// Handles mouse events that land on a marker: double-click a label to rename, or
    /// press-and-drag a marker to move it. Returns `true` if the event was consumed (so the
    /// caller skips the normal seek/select handling).
    fn try_handle_marker_mouse(&mut self, mouse: MouseEvent) -> bool {
        let area = self.waveform_area;
        let in_area = mouse.column >= area.x
            && mouse.column < area.x + area.width
            && mouse.row >= area.y
            && mouse.row < area.y + area.height;
        let idx = self.active_document;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Ctrl+click is reserved for selection extension.
                if !in_area || mouse.modifiers.contains(KeyModifiers::CONTROL) {
                    return false;
                }
                let hit = self
                    .marker_label_rects
                    .iter()
                    .find(|(r, _)| {
                        r.x <= mouse.column
                            && mouse.column < r.x + r.width
                            && r.y <= mouse.row
                            && mouse.row < r.y + r.height
                    })
                    .map(|(_, mi)| *mi);
                let now = Instant::now();
                let is_double = self.last_click.is_some_and(|(t, x, y)| {
                    now.duration_since(t) < Duration::from_millis(400)
                        && x.abs_diff(mouse.column) <= 1
                        && y == mouse.row
                });
                self.last_click = Some((now, mouse.column, mouse.row));
                let Some(mi) = hit else { return false };
                if is_double {
                    if let Some(marker) = self.documents.get(idx).and_then(|d| d.markers.get(mi)) {
                        self.dialog = Some(Dialog::RenameMarker {
                            position: marker.position,
                            input: TextInput::fresh(marker.label.clone()),
                        });
                    }
                    self.last_click = None;
                    self.dragging_marker = None;
                } else {
                    self.dragging_marker = Some(mi);
                    self.dragging_marker_start_position =
                        self.documents.get(idx).and_then(|d| d.markers.get(mi)).map(|m| m.position);
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(mi) = self.dragging_marker else { return false };
                let Some(viewport) = self.viewport.as_ref() else { return true };
                let scroll = viewport.scroll_offset;
                let spc = viewport.samples_per_column;
                let Some(doc) = self.documents.get_mut(idx) else { return true };
                let total = doc.len_samples();
                if total == 0 {
                    return true;
                }
                let colx = mouse.column.clamp(area.x, area.x + area.width - 1);
                let col = (colx - area.x) as f64;
                let pos = ((scroll as f64 + col * spc) as usize).min(total - 1);
                let mut path = None;
                if let Some(m) = doc.markers.get_mut(mi) {
                    m.position = pos;
                    doc.dirty = true;
                    path = doc.path.clone();
                }
                if let Some(p) = path {
                    self.file_panel.mark_dirty(&p, true);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(mi) = self.dragging_marker.take() {
                    let start_pos = self.dragging_marker_start_position.take();
                    if let Some(doc) = self.documents.get_mut(idx) {
                        // Capture the live-dragged-to position before sorting reshuffles
                        // indices, then collapse the whole drag gesture into one undoable
                        // `MoveMarkerCommand` — skipped entirely if nothing actually moved
                        // (e.g. a plain click with no drag in between).
                        let end_pos = doc.markers.get(mi).map(|m| m.position);
                        doc.markers.sort_by_key(|m| m.position);
                        if let (Some(from), Some(to)) = (start_pos, end_pos) {
                            if from != to {
                                self.histories[idx].apply(move_marker_command(from, to), doc);
                            }
                        }
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn handle_action(&mut self, action: Action) {
        if action == Action::Quit {
            // Warn if *any* open buffer is dirty, not just the active one.
            if self.documents.iter().any(|doc| doc.dirty) {
                self.confirm = Some(Confirm::Quit);
            } else {
                self.should_quit = true;
            }
            return;
        }

        // Panel/modal commands — work regardless of focus (e.g. a toolbar click).
        match action {
            Action::Noop => return,
            Action::OpenSelected => {
                self.open_selected_file();
                return;
            }
            Action::OpenDirectory => {
                let default = dirs_home().unwrap_or_else(|| "~".to_string());
                self.dialog = Some(Dialog::OpenDirectory { input: TextInput::fresh(default) });
                return;
            }
            Action::SearchFiles => {
                self.file_panel.focused = true;
                self.file_panel.filtering = true;
                self.file_panel.filter.clear();
                return;
            }
            Action::FocusNext => {
                self.cycle_focus();
                return;
            }
            Action::SwitchBuffer => {
                self.switch_to_buffer(self.buffer_panel.selected);
                return;
            }
            Action::SearchBuffers => {
                self.file_panel.focused = false;
                self.buffer_panel.focused = true;
                self.buffer_panel.filtering = true;
                self.buffer_panel.filter.clear();
                return;
            }
            Action::CloseBuffer => {
                self.request_close_buffer(self.active_document);
                return;
            }
            Action::RenameBuffer => {
                let idx = self.active_document;
                if let Some(doc) = self.documents.get(idx) {
                    let name = doc
                        .path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.dialog = Some(Dialog::RenameBuffer { index: idx, input: TextInput::fresh(name) });
                }
                return;
            }
            Action::RenameFile => {
                self.begin_rename_selected_file();
                return;
            }
            Action::DeleteFile => {
                self.request_delete_selected_file();
                return;
            }
            _ => {}
        }

        if action == Action::TogglePlayback {
            self.handle_playback_action(action);
            return;
        }

        if action == Action::SaveAll {
            self.save_all();
            return;
        }

        if action == Action::ResetConfig {
            self.confirm = Some(Confirm::ResetConfig);
            return;
        }

        // CopyToNew undo: when the user presses Undo on a buffer that was born from
        // CopyToNew and the undo stack is already empty, silently close the buffer rather
        // than doing nothing. This "undoes the creation" without triggering the save dialog
        // — the buffer was never saved to disk and never had an independent existence.
        if action == Action::Undo {
            let idx = self.active_document;
            if idx < self.histories.len()
                && self.histories[idx].created_by_copy_to_new
                && !self.histories[idx].can_undo()
            {
                self.close_buffer(idx);
                return;
            }
        }

        if matches!(
            action,
            Action::Cut
                | Action::Copy
                | Action::Paste
                | Action::Undo
                | Action::Redo
                | Action::Save
                | Action::SaveAs
                | Action::Reverse
                | Action::Delete
                | Action::Trim
        ) {
            self.handle_edit_action(action);
            return;
        }

        if action == Action::ClearSelection {
            if let Some(document) = self.active_doc_mut() {
                document.selection = None;
            }
            return;
        }

        if action == Action::SelectAll {
            if let Some(document) = self.active_doc_mut() {
                let len = document.len_samples();
                if len > 0 {
                    document.selection = Some(Selection { start: 0, end: len });
                    document.cursor = len - 1;
                }
            }
            return;
        }

        if action == Action::ToggleAutoVerticalZoom {
            let peak = self.visible_peak();
            if let Some(viewport) = self.viewport.as_mut() {
                viewport.auto_vertical_zoom = !viewport.auto_vertical_zoom;
                if viewport.auto_vertical_zoom && peak > 0.0001 {
                    viewport.set_amplitude_scale(0.95 / peak);
                } else if !viewport.auto_vertical_zoom {
                    viewport.set_amplitude_scale(1.0);
                }
            }
            self.save_config();
            return;
        }

        if action == Action::ToggleZeroSnap {
            self.snap_to_zero = !self.snap_to_zero;
            self.save_config();
            return;
        }

        if action == Action::ToggleLoop {
            self.loop_playback = !self.loop_playback;
            self.save_config();
            return;
        }

        if action == Action::ToggleFineMode {
            self.fine_mode = !self.fine_mode;
            self.save_config();
            return;
        }

        if action == Action::ToggleAudition {
            self.audition = !self.audition;
            if !self.audition {
                self.stop_audition();
            }
            self.save_config();
            return;
        }

        if action == Action::ToggleCursorFollowsPlayback {
            self.cursor_follows_playback = !self.cursor_follows_playback;
            self.save_config();
            return;
        }

        if action == Action::ToggleViewportFollowsPlayback {
            self.viewport_follows_playback = !self.viewport_follows_playback;
            if !self.viewport_follows_playback {
                self.viewport_following = false;
            }
            self.save_config();
            return;
        }

        if action == Action::ToggleGraphicsMode {
            self.graphics_mode = !self.graphics_mode;
            self.save_config();
            return;
        }

        if matches!(
            action,
            Action::InsertMarker
                | Action::DeleteMarker
                | Action::JumpPrevMarker
                | Action::JumpNextMarker
        ) {
            self.handle_marker_action(action);
            return;
        }

        if action == Action::IncreaseTransientThreshold {
            self.transient_threshold_db = (self.transient_threshold_db + 1.0).min(TRANSIENT_THRESHOLD_MAX_DB);
            self.save_config();
            return;
        }

        if action == Action::DecreaseTransientThreshold {
            self.transient_threshold_db = (self.transient_threshold_db - 1.0).max(TRANSIENT_THRESHOLD_MIN_DB);
            self.save_config();
            return;
        }

        if action == Action::NextRisingEdge {
            let idx = self.active_document;
            let threshold = self.transient_threshold_db;
            let edge = self.documents.get(idx).and_then(|d| d.find_next_rising_edge(d.cursor, threshold));
            if let Some(pos) = edge {
                self.jump_to_transient(pos);
            }
            return;
        }

        if action == Action::PrevRisingEdge {
            let idx = self.active_document;
            let threshold = self.transient_threshold_db;
            let edge = self.documents.get(idx).and_then(|d| d.find_previous_rising_edge(d.cursor, threshold));
            if let Some(pos) = edge {
                self.jump_to_transient(pos);
            }
            return;
        }

        if action == Action::AutoInsertMarkers {
            self.handle_auto_insert_markers();
            return;
        }

        if action == Action::Normalize {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::Normalize { input: TextInput::fresh("0.0") });
            }
            return;
        }

        if action == Action::Gain {
            if let Some(doc) = self.active_doc() {
                let is_stereo = doc.channels.len() == 2;
                self.dialog = Some(Dialog::Gain {
                    input: TextInput::fresh("0.0"),
                    right_input: TextInput::fresh("0.0"),
                    tanh_clip: false,
                    per_channel: false,
                    is_stereo,
                    focused: 0,
                });
            }
            return;
        }

        if action == Action::Resample {
            if let Some(rate) = self.active_doc().map(|d| d.sample_rate) {
                self.dialog = Some(Dialog::Resample { input: TextInput::new(""), current_rate: rate });
            }
            return;
        }

        if action == Action::FadeIn {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::FadeIn { curve: FadeCurve::Exp });
            }
            return;
        }

        if action == Action::FadeOut {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::FadeOut { curve: FadeCurve::Exp });
            }
            return;
        }

        if action == Action::TechnicalFades {
            self.apply_technical_fades();
            return;
        }

        if action == Action::CopyToNew {
            let result = self.active_doc().and_then(|d| {
                d.selection.map(|sel| {
                    let (start, end) = sel.normalized();
                    let samples = d.slice(start..end);
                    let markers: Vec<Marker> = d.markers.iter()
                        .filter(|m| m.position >= start && m.position < end)
                        .map(|m| Marker { position: m.position - start, label: m.label.clone() })
                        .collect();
                    (samples, markers, d.sample_rate, d.bits_per_sample)
                })
            });
            if let Some((samples, markers, sample_rate, bits_per_sample)) = result {
                let new_doc = Document {
                    channels: samples,
                    sample_rate,
                    bits_per_sample,
                    selection: None,
                    cursor: 0,
                    // A copy-to-new buffer holds unsaved data with no path, so it's dirty —
                    // this makes the quit/close confirmation fire for it.
                    dirty: true,
                    path: None,
                    markers,
                    bext: None,
                };
                self.push_document(new_doc);
                self.histories.last_mut().unwrap().created_by_copy_to_new = true;
                self.viewport = None;
                self.rebuild_audio();
                self.rebuild_waveform_caches();
            }
            return;
        }

        if action == Action::MixToMono {
            if let Some(doc) = self.active_doc() {
                let n = doc.channels.len();
                if n == 0 {
                    return;
                }
                let inputs: Vec<TextInput> = (0..n).map(|_| TextInput::new("0")).collect();
                self.dialog = Some(Dialog::MixToMono { inputs, focused: 0, tanh_clip: false });
            }
            return;
        }

        if action == Action::ExportRegions {
            if let Some(doc) = self.active_doc() {
                if doc.markers.is_empty() {
                    self.dialog = Some(Dialog::Info {
                        message: "No markers found. Add markers first to define regions.".to_string(),
                    });
                } else {
                    let default_depth = BitDepth::from_bits(doc.bits_per_sample);
                    self.dialog = Some(Dialog::ExportRegions {
                        folder_input: TextInput::new(""),
                        base_name_input: TextInput::new(""),
                        depth: default_depth,
                        dither: false,
                        limit_length: false,
                        limit_length_input: TextInput::fresh("1000"),
                        normalize: false,
                        normalize_input: TextInput::fresh("0.0"),
                        fade_in: true,
                        fade_in_input: TextInput::fresh("5"),
                        fade_out: true,
                        fade_out_input: TextInput::fresh("5"),
                        focused: 0,
                    });
                }
            }
            return;
        }

        if action == Action::CdpProcess {
            self.open_cdp_entry();
            return;
        }

        if action == Action::ConfigureCdpDirectory {
            // Unlike `open_cdp_entry` (validate-first, only prompts when broken), this is an
            // explicit "let me view/change this setting" entry point from the Options menu —
            // it always opens the setup prompt, prefilled with whatever's currently
            // configured (even if empty/invalid), rather than jumping straight to the browser
            // when the path happens to already be valid.
            self.dialog =
                Some(Dialog::CdpSetup { input: TextInput::new(self.config.cdp_dir.clone()), error: None });
            return;
        }

        if action == Action::NewFromLeft || action == Action::NewFromRight {
            let channel_idx = if action == Action::NewFromLeft { 0 } else { 1 };
            let result = self.active_doc().and_then(|d| {
                let (start, end) = match d.selection.map(|s| s.normalized()) {
                    Some((s, e)) if s < e => (s, e),
                    _ => (0, d.channels.first().map(|c| c.len()).unwrap_or(0)),
                };
                let channels = d.channels.get(channel_idx).map(|ch| vec![ch[start..end].to_vec()])?;
                let markers: Vec<Marker> = d.markers.iter()
                    .filter(|m| m.position >= start && m.position < end)
                    .map(|m| Marker { position: m.position - start, label: m.label.clone() })
                    .collect();
                Some((channels, markers, d.sample_rate, d.bits_per_sample))
            });
            if let Some((channels, markers, sample_rate, bits_per_sample)) = result {
                let new_doc = Document {
                    channels,
                    sample_rate,
                    bits_per_sample,
                    selection: None,
                    cursor: 0,
                    dirty: true,
                    path: None,
                    markers,
                    bext: None,
                };
                self.push_document(new_doc);
                self.histories.last_mut().unwrap().created_by_copy_to_new = true;
                self.viewport = None;
                self.rebuild_audio();
                self.rebuild_waveform_caches();
            }
            return;
        }

        // Holding an arrow key ramps the step up so crossing a long file doesn't mean
        // hundreds of keypresses; fine mode disables this entirely since its whole point
        // is slow, precise movement. `nav_step_multiplier` also resets/tracks hold state,
        // so it must run (exactly once) for every nav action even when not used below.
        // Computed before the viewport/document borrows below since it needs `&mut self`.
        let nav_multiplier = matches!(
            action,
            Action::MoveCursorLeft
                | Action::MoveCursorRight
                | Action::ExtendSelectionLeft
                | Action::ExtendSelectionRight
        )
        .then(|| self.nav_step_multiplier(action))
        .unwrap_or(1.0);

        let idx = self.active_document;
        let Some(viewport) = self.viewport.as_mut() else { return };
        let Some(document) = self.documents.get_mut(idx) else { return };
        let total_len = document.len_samples();
        if total_len == 0 {
            return;
        }
        let width = self.content_width;
        // Cursor/selection step: one whole column normally, or ~1/8th of one while fine mode
        // (toggled with backtick) is on — fine enough for precise edits but still faster than
        // crawling one sample per keypress, except when zoomed in so far that an eighth-column
        // already rounds down to a single sample. Modifier-free fine stepping replaces the old
        // Ctrl/Alt+arrow scheme, which no terminal/DE would reliably pass through.
        let column_step = (viewport.samples_per_column.max(1.0) as usize).max(1);
        let base_step = if self.fine_mode { (column_step / 8).max(1) } else { column_step };
        let step = ((base_step as f64 * nav_multiplier).round() as usize).max(1);
        let span = viewport.span(width);
        let loop_range = if self.loop_playback {
            Some(document.selection.map(|sel| sel.normalized()).unwrap_or((0, total_len)))
        } else {
            None
        };
        let old_cursor = document.cursor;
        match action {
            Action::Quit
            | Action::TogglePlayback
            | Action::Cut
            | Action::Copy
            | Action::Paste
            | Action::Undo
            | Action::Redo
            | Action::Save
            | Action::Reverse
            | Action::Normalize
            | Action::Resample
            | Action::Delete
            | Action::ToggleAutoVerticalZoom
            | Action::ToggleZeroSnap
            | Action::ToggleLoop
            | Action::ToggleFineMode
            | Action::ToggleAudition
            | Action::ToggleCursorFollowsPlayback
            | Action::ToggleViewportFollowsPlayback
            | Action::ToggleGraphicsMode
            | Action::ClearSelection
            | Action::SelectAll
            | Action::SaveAs
            | Action::SaveAll
            | Action::Gain
            | Action::CopyToNew
            | Action::MixToMono
            | Action::NewFromLeft
            | Action::NewFromRight
            | Action::FadeIn
            | Action::FadeOut
            | Action::TechnicalFades
            | Action::InsertMarker
            | Action::DeleteMarker
            | Action::JumpPrevMarker
            | Action::JumpNextMarker
            | Action::NextRisingEdge
            | Action::PrevRisingEdge
            | Action::AutoInsertMarkers
            | Action::IncreaseTransientThreshold
            | Action::DecreaseTransientThreshold
            | Action::Noop
            | Action::OpenSelected
            | Action::OpenDirectory
            | Action::SearchFiles
            | Action::FocusNext
            | Action::CloseBuffer
            | Action::RenameBuffer
            | Action::SwitchBuffer
            | Action::SearchBuffers
            | Action::RenameFile
            | Action::DeleteFile
            | Action::Trim
            | Action::ResetConfig
            | Action::ExportRegions
            | Action::CdpProcess
            | Action::ConfigureCdpDirectory => unreachable!(),
            // Cursor movement is identical whether or not it extends a selection; the
            // selection side-effect is applied in the second match below.
            Action::MoveCursorLeft | Action::ExtendSelectionLeft => {
                document.cursor = document.cursor.saturating_sub(step);
            }
            Action::MoveCursorRight | Action::ExtendSelectionRight => {
                document.cursor = (document.cursor + step).min(total_len - 1);
            }
            Action::JumpStart | Action::ExtendSelectionToStart => document.cursor = 0,
            Action::JumpEnd | Action::ExtendSelectionToEnd => document.cursor = total_len - 1,
            Action::ExtendSelectionToPrevMarker => {
                document.cursor = document
                    .markers
                    .iter()
                    .rev()
                    .find(|m| m.position < old_cursor)
                    .map(|m| m.position)
                    .unwrap_or(0);
            }
            Action::ExtendSelectionToNextMarker => {
                document.cursor = document
                    .markers
                    .iter()
                    .find(|m| m.position > old_cursor)
                    .map(|m| m.position)
                    .unwrap_or(total_len - 1);
            }
            Action::PageBack => {
                document.cursor = document.cursor.saturating_sub(span.max(1));
            }
            Action::PageForward => {
                document.cursor = (document.cursor + span.max(1)).min(total_len - 1);
            }
            Action::ExtendSelectionPageBack => {
                document.cursor = document.cursor.saturating_sub(span.max(1));
            }
            Action::ExtendSelectionPageForward => {
                document.cursor = (document.cursor + span.max(1)).min(total_len - 1);
            }
            Action::ZoomIn => viewport.zoom_in(document.cursor, width),
            Action::ZoomOut => viewport.zoom_out(document.cursor, width),
            Action::ZoomInVertical => viewport.zoom_in_vertical(),
            Action::ZoomOutVertical => viewport.zoom_out_vertical(),
        }

        let snap = self.snap_to_zero;
        // The cursor is a sample index and clamps to the last sample (total_len - 1), but
        // selection bounds are exclusive-end everywhere (delete/cut/trim take [start, end)).
        // A selection edge or anchor sitting on the last sample therefore counts as
        // total_len — otherwise "select to the end, delete" silently leaves the final
        // sample behind as an orphaned click at end-of-file.
        let sel_edge = |pos: usize| if pos == total_len - 1 { total_len } else { pos };
        match action {
            // Extend in either direction with the anchor held fixed (see Selection::extended):
            // the active edge follows the cursor, so reversing direction shrinks rather than
            // flips the selection.
            Action::ExtendSelectionLeft
            | Action::ExtendSelectionRight => {
                // Snap the active edge to a zero crossing, but *directionally*: a plain
                // nearest-crossing snap pulls a small step (when zoomed in, column_step is one
                // sample) straight back to the crossing it just left, so the selection appears
                // frozen. If snapping would erase the step's progress, keep the literal cursor.
                let raw = document.cursor;
                let cursor = if snap {
                    let snapped = document.snap_to_zero_crossing(raw);
                    let advanced = if raw >= old_cursor { snapped > old_cursor } else { snapped < old_cursor };
                    if advanced { snapped } else { raw }
                } else {
                    raw
                };
                document.selection =
                    Some(Selection::extended(document.selection, sel_edge(old_cursor), sel_edge(cursor)));
                document.cursor = cursor;
            }
            Action::ExtendSelectionToStart
            | Action::ExtendSelectionToEnd
            | Action::ExtendSelectionToPrevMarker
            | Action::ExtendSelectionToNextMarker
            | Action::ExtendSelectionPageBack
            | Action::ExtendSelectionPageForward => {
                // cursor is already at the target; anchor is kept from existing selection or old_cursor.
                document.selection = Some(Selection::extended(
                    document.selection,
                    sel_edge(old_cursor),
                    sel_edge(document.cursor),
                ));
            }
            // Plain cursor moves and jumps clear any active selection (same paradigm as
            // Word/RX: non-Shift navigation collapses the selection).
            Action::MoveCursorLeft
            | Action::MoveCursorRight
            | Action::JumpStart
            | Action::JumpEnd
            | Action::PageBack
            | Action::PageForward => {
                document.selection = None;
            }
            // Zoom changes don't affect the selection.
            _ => {}
        }

        viewport.ensure_visible(document.cursor, width);

        // Seek the playback position to the new cursor only for actions that actually move it.
        // Zoom actions change the viewport without moving the cursor — seeking during zoom
        // would restart playback from the cursor position instead of continuing from the
        // playhead's current location.
        let cursor_moved = !matches!(
            action,
            Action::ZoomIn | Action::ZoomOut | Action::ZoomInVertical | Action::ZoomOutVertical
        );
        if cursor_moved {
            if let Some(audio) = &self.audio {
                if audio.is_playing() {
                    if let Some((ls, le)) = loop_range {
                        audio.seek_looped(document.cursor, ls, le);
                    } else {
                        audio.seek(document.cursor);
                    }
                }
            }
        }
    }

    fn handle_edit_action(&mut self, action: Action) {
        let idx = self.active_document;
        if idx >= self.documents.len() {
            return;
        }
        // Preread self fields before the mutable document borrow.
        let mutates_samples = matches!(
            action,
            Action::Cut | Action::Delete | Action::Paste | Action::Undo | Action::Redo | Action::Reverse | Action::Trim
        );
        let snap = self.snap_to_zero;
        let content_width = self.content_width;
        let has_selection = self.documents[idx].selection.is_some();

        match action {
            Action::Save => {
                let doc = &self.documents[idx];
                if doc.path.is_some() {
                    // Has a path — saved through the mutable path below.
                } else {
                    return self.handle_action(Action::SaveAs);
                }
            }
            _ => {}
        }

        let document = self.documents.get_mut(idx).unwrap();

        match action {
            Action::Cut => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    let (start, end) = if snap {
                        document.snap_range_to_zero_crossing(start, end)
                    } else {
                        (start, end)
                    };
                    if start < end {
                        self.clipboard.set(document.slice(start..end), document.sample_rate);
                        self.histories[idx].apply(cut_command(start..end), document);
                    }
                }
            }
            Action::Delete => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    let (start, end) = if snap {
                        document.snap_range_to_zero_crossing(start, end)
                    } else {
                        (start, end)
                    };
                    if start < end {
                        self.histories[idx].apply(delete_command(start..end), document);
                    }
                }
            }
            Action::Copy => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    if start < end {
                        self.clipboard.set(document.slice(start..end), document.sample_rate);
                    }
                }
            }
            Action::Paste => {
                if !self.clipboard.is_empty() {
                    if has_selection {
                        if let Some(sel) = document.selection {
                            let (start, end) = sel.normalized();
                            if start < end {
                                self.histories[idx].apply(delete_command(start..end), document);
                            }
                        }
                    }
                    let at = document.cursor;
                    let data = self.clipboard.channels.clone();
                    self.histories[idx].apply(paste_command(at, data), document);
                }
            }
            Action::Undo => {
                self.histories[idx].undo(document);
            }
            Action::Redo => {
                self.histories[idx].redo(document);
            }
            Action::Save => {
                if let Some(path) = document.path.clone() {
                    if save_wav(document, &path).is_ok() {
                        document.dirty = false;
                        self.file_panel.mark_dirty(&path, false);
                    }
                }
            }
            Action::SaveAs => {
                let name = document
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "untitled.wav".to_string());
                self.save_as_input = TextInput::fresh(name);
                self.save_as_focused = 0;
                self.save_as_active = true;
            }
            Action::Reverse => {
                let (start, end) = match document.selection {
                    Some(sel) => sel.normalized(),
                    None => (0, document.len_samples()),
                };
                let (start, end) = if snap {
                    document.snap_range_to_zero_crossing(start, end)
                } else {
                    (start, end)
                };
                if start < end {
                    self.histories[idx].apply(reverse_command(start, end), document);
                }
            }
            Action::Trim => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    if start < end {
                        self.histories[idx].apply(trim_command(start, end), document);
                        self.viewport = None;
                    }
                }
            }
            _ => unreachable!(),
        }

        let cursor = document.cursor;
        let new_len = document.len_samples();
        if let Some(viewport) = self.viewport.as_mut() {
            // Sync before clamping: the edit may have shrunk the document, and total_len is
            // otherwise only refreshed at render time — ensure_visible would clamp
            // scroll_offset against the stale (longer) length and leave the view
            // overhanging past the new end-of-file.
            viewport.total_len = new_len;
            viewport.ensure_visible(cursor, content_width);
        }
        if mutates_samples {
            self.after_sample_mutation(idx);
        }
    }

    /// Insert/delete a marker at/near the cursor (both undoable, like any other document
    /// mutation), or jump the cursor to an adjacent marker (not a mutation, so not
    /// undoable).
    /// Scans the whole file for transients (`Document::find_all_rising_edges`, same
    /// algorithm and threshold as Next Rising Edge) and inserts a marker right before each
    /// one not already marked — one undo step for the whole batch, not one per marker.
    fn handle_auto_insert_markers(&mut self) {
        let idx = self.active_document;
        let Some(document) = self.documents.get(idx) else {
            return;
        };
        let edges = document.find_all_rising_edges(self.transient_threshold_db);
        let mut next_n = document.markers.len() + 1;
        let mut to_insert: Vec<Marker> = Vec::new();
        for pos in edges {
            let already_marked =
                document.markers.iter().any(|m| m.position == pos) || to_insert.iter().any(|m| m.position == pos);
            if already_marked {
                continue;
            }
            to_insert.push(Marker { position: pos, label: format!("Marker {next_n}") });
            next_n += 1;
        }
        if to_insert.is_empty() {
            return;
        }
        let document = &mut self.documents[idx];
        self.histories[idx].apply(auto_insert_markers_command(to_insert), document);
        if let Some(path) = document.path.clone() {
            self.file_panel.mark_dirty(&path, true);
        }
    }

    fn handle_marker_action(&mut self, action: Action) {
        let idx = self.active_document;
        if idx >= self.documents.len() {
            return;
        }
        let mut moved_cursor = false;
        let mut changed = false;
        match action {
            Action::InsertMarker => {
                let doc = &self.documents[idx];
                let pos = doc.cursor;
                if !doc.markers.iter().any(|m| m.position == pos) {
                    let label = format!("Marker {}", doc.markers.len() + 1);
                    self.histories[idx].apply(insert_marker_command(pos, label), &mut self.documents[idx]);
                    changed = true;
                }
            }
            Action::DeleteMarker => {
                let doc = &self.documents[idx];
                if let Some(i) = nearest_marker(&doc.markers, doc.cursor) {
                    let pos = doc.markers[i].position;
                    self.histories[idx].apply(delete_marker_command(pos), &mut self.documents[idx]);
                    changed = true;
                }
            }
            Action::JumpPrevMarker => {
                let doc = &mut self.documents[idx];
                if let Some(p) =
                    doc.markers.iter().rev().find(|m| m.position < doc.cursor).map(|m| m.position)
                {
                    doc.cursor = p;
                    moved_cursor = true;
                }
            }
            Action::JumpNextMarker => {
                let doc = &mut self.documents[idx];
                if let Some(p) = doc.markers.iter().find(|m| m.position > doc.cursor).map(|m| m.position) {
                    doc.cursor = p;
                    moved_cursor = true;
                }
            }
            _ => {}
        }
        if changed {
            if let Some(path) = self.documents[idx].path.clone() {
                self.file_panel.mark_dirty(&path, true);
            }
        }
        if moved_cursor {
            let cursor = self.documents[idx].cursor;
            if let Some(viewport) = self.viewport.as_mut() {
                viewport.ensure_visible(cursor, self.content_width);
            }
            if let Some(audio) = &self.audio {
                if audio.is_playing() {
                    audio.seek(cursor);
                }
            }
        }
    }

    fn handle_playback_action(&mut self, _action: Action) {
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        // Space is the only transport command: play from the cursor, or pause if playing.
        if audio.is_playing() {
            audio.pause();
            self.viewport_following = false;
            // "Insertion Point Follows Playback": snap the cursor to wherever playback
            // actually stopped and scroll it into view, rather than leaving the cursor
            // wherever it was when playback started.
            if self.cursor_follows_playback {
                if let Some(stopped_at) = self.playhead_position {
                    self.snap_cursor_to(stopped_at);
                }
            }
        } else {
            let Some(document) = self.active_doc() else {
                return;
            };
            if let Some((ls, le)) = self.loop_range() {
                audio.play_looped(document.cursor, ls, le);
            } else {
                audio.play(document.cursor);
            }
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area: Rect = frame.area();
        let focus = self.focus();
        // Reserve the tallest set's height for every mode so the layout doesn't jump on Tab.
        let toolbar_height = self.toolbar.reserved_rows(area.width);
        let chrome = split_chrome(area, toolbar_height);

        // Render chrome panels.
        self.file_panel_area = chrome.panel;
        self.buffer_panel_area = chrome.buffers;
        self.file_panel.render(frame, chrome.panel);
        let buf_names = self.buffer_names();
        self.buffer_panel.render(frame, chrome.buffers, &buf_names, self.active_document);
        self.toolbar.active_actions.clear();
        self.toolbar.is_playing = self.audio.as_ref().is_some_and(|a| a.is_playing());
        self.toolbar.transient_threshold_db = self.transient_threshold_db;
        if self.snap_to_zero {
            self.toolbar.active_actions.insert(Action::ToggleZeroSnap);
        }
        if self.loop_playback {
            self.toolbar.active_actions.insert(Action::ToggleLoop);
        }
        if self.fine_mode {
            self.toolbar.active_actions.insert(Action::ToggleFineMode);
        }
        if self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom) {
            self.toolbar.active_actions.insert(Action::ToggleAutoVerticalZoom);
        }
        if self.audition {
            self.toolbar.active_actions.insert(Action::ToggleAudition);
        }
        if self.cursor_follows_playback {
            self.toolbar.active_actions.insert(Action::ToggleCursorFollowsPlayback);
        }
        if self.viewport_follows_playback {
            self.toolbar.active_actions.insert(Action::ToggleViewportFollowsPlayback);
        }
        if self.graphics_mode {
            self.toolbar.active_actions.insert(Action::ToggleGraphicsMode);
        }
        self.toolbar.render(frame, chrome.toolbar, focus);
        // Fill the spacer row with the base background so it matches the toolbar below it
        // (rather than showing through to the terminal default).
        frame.render_widget(
            Block::default().style(Style::default().bg(theme::BASE)),
            chrome.spacer,
        );

        // The waveform pane is "focused" (and gets the accent color) when neither side panel
        // is — true for both the empty placeholder and a loaded document.
        let waveform_focused = !self.file_panel.focused && !self.buffer_panel.focused;
        let border_color = if waveform_focused { theme::FOCUS } else { theme::BORDER };

        let doc_idx = self.active_document;
        let no_doc = self.documents.get(doc_idx).is_none();
        if no_doc {
            let block = Block::default()
                .title(Span::styled(" No file loaded ", Style::default().fg(border_color)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .style(Style::default().fg(theme::CHROME_FG).bg(theme::BASE));
            let text = Paragraph::new("Select a file from the panel on the left (Tab to focus, / to search)")
                .alignment(Alignment::Center)
                .block(block);
            frame.render_widget(text, chrome.content);
            // Rendered last so an open dropdown (which extends below the menu bar, into
            // the content area) draws on top of everything instead of being overdrawn by
            // it — same ordering as the loaded-document path below.
            self.menu.render(frame, chrome.menu);
            // Modal overlays must still render with no document loaded — otherwise opening a
            // dialog here (e.g. renaming a file in the Files panel) leaves an invisible modal
            // that swallows all input, and the app looks frozen.
            self.render_overlays(frame, area);
            return;
        };

        let title_text = format!(" {} ", self.buffer_name(doc_idx));
        let title = Line::from(vec![
            Span::styled(title_text, Style::default().fg(border_color)),
            Span::styled(
                if self.documents[doc_idx].dirty { "* " } else { "" },
                Style::default().fg(theme::DIRTY),
            ),
        ]);
        let outer = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(theme::BASE));
        let inner = outer.inner(chrome.content);
        frame.render_widget(outer, chrome.content);

        let [waveform_area, status_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

        let gutter = DB_GUTTER_WIDTH.min(waveform_area.width / 2);
        let inner_waveform_area = Rect {
            x: waveform_area.x + gutter,
            y: waveform_area.y,
            width: waveform_area.width.saturating_sub(gutter * 2),
            height: waveform_area.height,
        };

        self.content_width = inner_waveform_area.width;
        self.waveform_area = inner_waveform_area;
        let total_len = self.documents[doc_idx].len_samples();
        let auto_vertical_zoom_default = self.config.auto_vertical_zoom;
        let viewport = self.viewport.get_or_insert_with(|| {
            let mut v = Viewport::fit_to_width(total_len, inner_waveform_area.width as usize);
            v.auto_vertical_zoom = auto_vertical_zoom_default;
            v
        });
        viewport.total_len = total_len;

        let channel_count = self.documents[doc_idx].channel_count().max(1);
        // Drop stale per-channel image state from a previous document with more channels
        // — never reuse it for a channel index that no longer exists.
        self.graphics_protocols.truncate(channel_count);
        let full_chunks =
            Layout::vertical(vec![Constraint::Fill(1); channel_count]).split(waveform_area);
        let selection = self.documents[doc_idx].selection.map(|s| s.normalized());

        // When auto vertical zoom is on, dynamically fit amplitude_scale to the visible
        // window's peak every frame, so scrolling/zooming to a quieter section zooms in to
        // match. The dB scale stays absolute dBFS regardless (see DbScaleWidget): fitting a
        // quiet peak pushes 0dB off the top so the marks read the true level of the loudest
        // visible sample — a −6 dBFS peak shows −6 near the top, not 0dB.
        // The exact dB level of the visible peak, shown on the dB scale as a precise
        // reference mark (see DbScaleWidget) — the auto-zoom equivalent of the fixed 0dB
        // reference always visible without it. `None` when auto zoom is off or the visible
        // window is silent (no peak to report).
        let mut peak_db: Option<f32> = None;
        if viewport.auto_vertical_zoom {
            let vp = visible_peak_raw(
                self.documents.get(doc_idx),
                Some(viewport),
                &self.waveform_caches,
                self.content_width,
            );
            if vp > 0.0001 {
                viewport.set_amplitude_scale(0.95 / vp);
                peak_db = Some(20.0 * vp.log10());
            }
        }

        let overlay_active =
            self.confirm.is_some() || self.save_as_active || self.dialog.is_some() || self.menu.is_open();
        let marker_refs: Vec<(usize, &str)> =
            self.documents[doc_idx].markers.iter().map(|m| (m.position, m.label.as_str())).collect();
        // Per-channel terminal row range actually covered by a rendered graphics image this
        // frame, so the marker overlay below knows which rows already have marker lines baked
        // into the bitmap (and must not also draw a buffer-cell line there — see the comment
        // by `overlay_active` above for why mixing the two corrupts the terminal display) vs.
        // which rows still need the legacy buffer-cell line (text-mode/no-picker/overlay-open
        // channels, and channel 0's reserved top row — see below).
        let mut channel_image_rows: Vec<Option<(u16, u16)>> = vec![None; channel_count];

        for (i, channel_full_area) in full_chunks.iter().enumerate() {
            let channel_inner = Rect {
                x: channel_full_area.x + gutter,
                y: channel_full_area.y,
                width: channel_full_area.width.saturating_sub(gutter * 2),
                height: channel_full_area.height,
            };
            let left_gutter = Rect {
                x: channel_full_area.x,
                y: channel_full_area.y,
                width: gutter,
                height: channel_full_area.height,
            };
            let right_gutter = Rect {
                x: channel_full_area.x + channel_full_area.width - gutter,
                y: channel_full_area.y,
                width: gutter,
                height: channel_full_area.height,
            };

            let samples = self.documents[doc_idx]
                .channels
                .get(i)
                .map(|c| c.as_slice())
                .unwrap_or(&[]);
            let widget = WaveformWidget {
                samples,
                viewport,
                cache: self.waveform_caches.get(i),
                selection,
                cursor: self.documents[doc_idx].cursor,
                playhead: self.playhead_position,
            };
            frame.render_widget(widget, channel_inner);

            // Graphics mode: when a graphics-capable terminal was detected at startup,
            // rasterize this channel's waveform into a real bitmap and display it via the
            // detected protocol (kitty/Sixel/iTerm2), drawn on top of the character-glyph
            // WaveformWidget just rendered above. Rebuilt fresh every frame from the same
            // live viewport/selection/cursor/playhead the text widget just used —
            // `StatefulProtocol` has no in-place "swap this image" method, so
            // `Picker::new_resize_protocol` (the crate's intended way to give it new
            // content) is called every frame rather than reused, since the waveform's
            // pixel content genuinely changes on essentially every redraw during
            // scrolling/zooming/playback anyway.
            //
            // Skipped whenever a menu/dialog overlay is showing: the kitty unicode-placeholder
            // protocol embeds one escape sequence per row that, once (re-)transmitted, paints
            // the *entire* row's width directly on the real terminal screen — independent of
            // ratatui's own cell-diffing, which only knows about the single buffer cell holding
            // that sequence. Re-transmitting every frame (a fresh id each time, since we never
            // reuse the previous `StatefulProtocol`) repaints that full row on the real terminal
            // even where an overlay drew plain text moments earlier in the same buffer, which is
            // what made dialogs flash and vanish a frame later. Skipping the retransmit while an
            // overlay is open leaves the text-mode `WaveformWidget` rendered above as the visible
            // fallback in that area instead.
            if self.graphics_mode && !overlay_active {
                if let Some(picker) = &self.picker {
                    channel_image_rows[i] = Some((channel_inner.y, channel_inner.y + channel_inner.height));
                    let font = picker.font_size();
                    let pixel_width = channel_inner.width as u32 * font.width.max(1) as u32;
                    let pixel_height = channel_inner.height as u32 * font.height.max(1) as u32;
                    let img = waveform_image::rasterize_waveform(
                        samples,
                        viewport,
                        self.waveform_caches.get(i),
                        selection,
                        self.documents[doc_idx].cursor,
                        self.playhead_position,
                        &marker_refs,
                        i == 0,
                        channel_inner.width,
                        pixel_width,
                        pixel_height,
                    );
                    let protocol = picker.new_resize_protocol(image::DynamicImage::ImageRgba8(img));
                    if i < self.graphics_protocols.len() {
                        self.graphics_protocols[i] = protocol;
                    } else {
                        self.graphics_protocols.push(protocol);
                    }
                    frame.render_stateful_widget(ratatui_image::StatefulImage::default(), channel_inner, &mut self.graphics_protocols[i]);
                }
            }

            let db_scale = DbScaleWidget { amplitude_scale: viewport.amplitude_scale, peak_db };
            frame.render_widget(db_scale, left_gutter);
            frame.render_widget(db_scale, right_gutter);
        }

        // Marker overlay: a dashed vertical line spanning all channels at each marker's
        // column, with its label on the top row. Label rects are recorded for double-click
        // (rename) and the lines for drag (move) hit-testing in `handle_mouse`.
        let scroll = viewport.scroll_offset;
        let spc = viewport.samples_per_column.max(f64::MIN_POSITIVE);
        let wf = self.waveform_area;
        self.marker_label_rects.clear();
        let marker_style = Style::default().fg(theme::MARKER).bg(theme::BASE);
        // A marker sitting exactly on the insertion point would otherwise hide it — the
        // marker's dashed line is drawn after (and on top of) the waveform's cursor line in
        // the same column. Recoloring that one marker to the cursor's accent keeps "the
        // insertion point is here" visible instead of silently losing it.
        let cursor = self.documents[doc_idx].cursor;
        let marker_at_cursor_style = Style::default().fg(theme::CURSOR).bg(theme::BASE);
        // Visible markers as (screen x, index), sorted left-to-right so each label can be
        // clipped at the next marker's line instead of overprinting it.
        let mut visible: Vec<(u16, usize)> = self.documents[doc_idx]
            .markers
            .iter()
            .enumerate()
            .filter_map(|(mi, m)| {
                if m.position < scroll {
                    return None;
                }
                let col = ((m.position - scroll) as f64 / spc) as i64;
                (0..wf.width as i64).contains(&col).then(|| (wf.x + col as u16, mi))
            })
            .collect();
        visible.sort_by_key(|&(x, _)| x);
        let buf = frame.buffer_mut();
        for (k, &(x, mi)) in visible.iter().enumerate() {
            let style = if self.documents[doc_idx].markers[mi].position == cursor {
                marker_at_cursor_style
            } else {
                marker_style
            };
            for y in wf.y..wf.y + wf.height {
                // Rows actually covered by a rendered graphics image already have this
                // marker's line baked into the bitmap (see `rasterize_waveform`'s `markers`
                // param) — drawing it again here as a plain character cell would fight the
                // kitty unicode-placeholder image for control of that row's escape sequence
                // and corrupt the terminal's cursor-position bookkeeping for the whole row,
                // which is what caused markers to glitch the display in graphics mode.
                if channel_image_rows.iter().flatten().any(|&(start, end)| y >= start && y < end) {
                    continue;
                }
                buf[(x, y)].set_char('┊').set_style(style).set_diff_option(CellDiffOption::AlwaysUpdate);
            }
            let lx = x + 1;
            // Stop the label before the next marker's line (or the pane's right edge).
            let limit = visible.get(k + 1).map(|&(nx, _)| nx).unwrap_or(wf.x + wf.width);
            let avail = limit.saturating_sub(lx) as usize;
            let shown: String = self.documents[doc_idx].markers[mi].label.chars().take(avail).collect();
            let shown_w = shown.chars().count() as u16;
            // The label row is covered by channel 0's image whenever graphics mode rendered
            // it (the label text is then rasterized directly into the bitmap instead — see
            // `show_marker_labels` in `rasterize_waveform`); only draw the buffer-cell text
            // when that row genuinely has no image underneath it.
            let label_row_has_image = channel_image_rows.iter().flatten().any(|&(start, end)| wf.y >= start && wf.y < end);
            if shown_w > 0 && !label_row_has_image {
                buf.set_string(lx, wf.y, &shown, style);
                for cx in lx..lx + shown_w {
                    buf[(cx, wf.y)].set_diff_option(CellDiffOption::AlwaysUpdate);
                }
            }
            self.marker_label_rects.push((
                Rect { x, y: wf.y, width: shown_w + 1, height: 1 },
                mi,
            ));
        }

        frame.render_widget(StatusBar { document: &self.documents[doc_idx], viewport, snap_to_zero: self.snap_to_zero, loop_playback: self.loop_playback, fine_mode: self.fine_mode, transient_threshold_db: self.transient_threshold_db, last_action: self.histories[doc_idx].last_label() }, status_area);

        // Rendered last (after the waveform, panels, and marker labels) so an open
        // dropdown — which extends below the menu bar into the content area — draws on
        // top of everything instead of being overdrawn by it.
        self.menu.render(frame, chrome.menu);

        self.render_overlays(frame, area);
    }

    /// Renders the modal overlays (quit/close/reset/delete confirm, Save As, and the general
    /// dialog) on top of whatever is beneath. Called from BOTH the loaded-document path and
    /// the "no file loaded" placeholder path: a modal absorbs all input, so one that opens
    /// while no buffer is loaded (e.g. Files-panel rename) must still be drawn — otherwise it
    /// is invisible and the app looks frozen, escapable only by the Esc/Enter the user can't see.
    fn render_overlays(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(confirm) = &self.confirm {
            let text = match confirm {
                Confirm::Quit => {
                    let n = self.documents.iter().filter(|d| d.dirty).count();
                    let noun = if n == 1 { "buffer" } else { "buffers" };
                    format!(" {n} unsaved {noun} — (s)ave all & quit · (y) quit anyway · (Esc) cancel ")
                }
                Confirm::CloseBuffer(_) => {
                    " Unsaved buffer — (s)ave & close · (y) close anyway · (Esc) cancel ".to_string()
                }
                Confirm::ResetConfig => {
                    " Reset all keybindings to defaults? (existing config saved as .bak) — (y) reset · (Esc) cancel ".to_string()
                }
                Confirm::DeleteFile(path) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    format!(" Delete \"{name}\" from disk? — (y) delete · (Esc) cancel ")
                }
            };
            render_confirm(frame, area, &text);
        }

        if self.save_as_active {
            let rects = render_save_as_dialog(
                frame, area,
                &self.save_as_input, self.save_as_depth, self.save_as_dither, self.save_as_focused,
            );
            // Last element is the apply (hints bar) rect; everything before it is interactive.
            self.dialog_n_interactive = rects.len().saturating_sub(1);
            self.dialog_row_rects = rects;
        }

        let dialog_rects = self
            .dialog
            .as_ref()
            .map(|d| render_dialog(frame, area, d, &self.cdp_catalog))
            .unwrap_or_default();
        if !dialog_rects.is_empty() {
            self.dialog_n_interactive = dialog_rects.len().saturating_sub(1);
            self.dialog_row_rects = dialog_rects;
        }

        // Graphics-mode envelope curve: `render_cdp_envelope_editor` (inside `render_dialog`
        // above) always draws the ASCII staircase into the grid's `Rect` first — cheap, and
        // the correct fallback when there's no picker. When a real terminal graphics
        // protocol is available, draw a bitmap (true diagonal line segments + filled-disc
        // points, `cdp_envelope_image::rasterize_cdp_envelope`) directly over that same
        // `Rect`, which simply occludes the ASCII version underneath — no special-casing
        // needed inside the ASCII renderer itself. Mirrors the waveform's own graphics
        // block, just for the editor's grid instead of a whole channel pane; unlike that
        // block there's no `overlay_active` guard to worry about, since a dialog being open
        // is exactly the precondition for this branch running at all.
        let envelope_curve = match &self.dialog {
            Some(Dialog::CdpParams { envelope: Some(edit), fields, .. }) => {
                fields.get(edit.field_index).and_then(|f| match f {
                    CdpField::Number { min, max, .. } => {
                        Some((edit.points.clone(), edit.selected, edit.time_max, *min, *max, edit.range))
                    }
                    _ => None,
                })
            }
            _ => None,
        };
        if let Some((points, selected, time_max, min, max, range)) = envelope_curve {
            if self.graphics_mode {
                if let (Some(picker), Some(&grid)) = (&self.picker, self.dialog_row_rects.first()) {
                    let font = picker.font_size();
                    let pixel_width = grid.width as u32 * font.width.max(1) as u32;
                    let pixel_height = grid.height as u32 * font.height.max(1) as u32;
                    let waveform_ref = self.cdp_envelope_waveform_ref(range, pixel_width as usize);
                    let img = cdp_envelope_image::rasterize_cdp_envelope(
                        &points, selected, time_max, min, max, &waveform_ref, pixel_width, pixel_height,
                    );
                    let protocol = picker.new_resize_protocol(image::DynamicImage::ImageRgba8(img));
                    self.cdp_envelope_graphics_protocol = Some(protocol);
                    if let Some(protocol) = self.cdp_envelope_graphics_protocol.as_mut() {
                        frame.render_stateful_widget(ratatui_image::StatefulImage::default(), grid, protocol);
                    }
                }
            }
        }
    }

    /// Rectified (abs-value) peak amplitude per *pixel* column across `range`, using the
    /// active document's own `WaveformCache` (the same multi-resolution pyramid the
    /// waveform view itself reads — and the same one-`min_max`-call-per-pixel-column
    /// technique `waveform_image::rasterize_waveform`'s bar mode uses, so this stays cheap
    /// regardless of how long `range` is, no raw per-frame scan needed) rather than a fresh
    /// linear scan. Drawn as a pale reference waveform behind the envelope curve in graphics
    /// mode, so the user can see which parts of the sound the automation will actually
    /// touch. Combines channels by taking the loudest one at each column (a peak-across-
    /// channels envelope, not a true mixdown) — plenty for a soft visual guide, not a
    /// precise measurement. Pixel resolution (not the coarser character-cell grid) matters
    /// here specifically so the traced shape reads as a real waveform silhouette rather than
    /// a blocky staircase of ~8px-wide flat steps.
    fn cdp_envelope_waveform_ref(&self, range: (usize, usize), cells: usize) -> Vec<f32> {
        let cells = cells.max(1);
        let mut peaks = vec![0.0f32; cells];
        let Some(doc) = self.documents.get(self.active_document) else { return peaks };
        let (start, end) = range;
        let span = end.saturating_sub(start);
        if span == 0 {
            return peaks;
        }
        for (col, peak) in peaks.iter_mut().enumerate() {
            let col_start = start + (col as f64 / cells as f64 * span as f64) as usize;
            let col_end_raw = start + ((col + 1) as f64 / cells as f64 * span as f64) as usize;
            let col_end = col_end_raw.max(col_start + 1).min(end);
            if col_start >= col_end {
                continue;
            }
            let mut column_peak = 0.0f32;
            for (ch_i, channel) in doc.channels.iter().enumerate() {
                let ch_end = col_end.min(channel.len());
                if col_start >= ch_end {
                    continue;
                }
                let (mn, mx) = match self.waveform_caches.get(ch_i) {
                    Some(cache) => cache.min_max(channel, col_start, ch_end),
                    None => crate::ui::waveform_cache::raw_min_max(&channel[col_start..ch_end]),
                };
                column_peak = column_peak.max(mn.abs()).max(mx.abs());
            }
            *peak = column_peak;
        }
        peaks
    }
}

/// Best-effort home directory as a string (from $HOME), for the Open Directory default.
fn dirs_home() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty())
}

/// Ensures a save/rename target ends in `.wav` (case-insensitive), appending it otherwise.
/// Empty input is returned unchanged (callers treat empty as "don't save").
fn ensure_wav_extension(name: &str) -> String {
    if name.is_empty()
        || std::path::Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
    {
        name.to_string()
    } else {
        format!("{name}.wav")
    }
}

/// Index of the marker closest to `pos`, or `None` if there are no markers.
fn nearest_marker(markers: &[Marker], pos: usize) -> Option<usize> {
    markers
        .iter()
        .enumerate()
        .min_by_key(|(_, m)| m.position.abs_diff(pos))
        .map(|(i, _)| i)
}

/// Peak sample magnitude within the visible window. Takes explicit parameters to avoid
/// borrow conflicts with concurrent mutable access to `self.viewport`.
fn visible_peak_raw(
    document: Option<&Document>,
    viewport: Option<&Viewport>,
    waveform_caches: &[WaveformCache],
    content_width: u16,
) -> f32 {
    let (Some(document), Some(viewport)) = (document, viewport) else {
        return 0.0;
    };
    let visible_end = (viewport.scroll_offset + viewport.span(content_width))
        .min(document.len_samples());
    if viewport.scroll_offset >= visible_end || content_width == 0 {
        return 0.0;
    }
    waveform_caches
        .iter()
        .zip(document.channels.iter())
        .fold(0.0f32, |peak, (cache, samples)| {
            let (mn, mx) = cache.min_max(samples, viewport.scroll_offset, visible_end);
            peak.max(mn.abs()).max(mx.abs())
        })
}

fn render_save_as_dialog(
    frame: &mut Frame,
    area: Rect,
    input: &TextInput,
    depth: BitDepth,
    dither: bool,
    focused: usize,
) -> Vec<Rect> {
    let width = 52u16.min(area.width);
    // header spacer + filename + format + blank + dither + blank + hints = 7 inner rows
    // + 2 border
    let height = 9u16.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);
    let dim_style = Style::default().fg(theme::BORDER).bg(theme::SURFACE0);

    // Row 0: filename text field
    let (before, under, after) = input.split_at_cursor();
    let filename_line = if focused == 0 {
        Line::from(vec![
            Span::styled(" Filename: ", label_style),
            Span::styled(before, base),
            Span::styled(under, cursor_style),
            Span::styled(after, base),
            Span::raw("  "),
        ])
    } else {
        Line::from(vec![
            Span::styled(" Filename: ", label_style),
            Span::styled(input.value().to_string(), base),
        ])
    };

    // Row 1: format cycle
    let format_line = if focused == 1 {
        Line::from(vec![
            Span::styled(" Format:  ◄ ", label_style),
            Span::styled(depth.label(), cursor_style),
            Span::styled(" ►  ", label_style),
        ])
    } else {
        Line::from(vec![
            Span::styled(" Format:  ", label_style),
            Span::styled(depth.label(), base),
        ])
    };

    // Row 2: dither checkbox (greyed out for float)
    let dither_line = if !depth.supports_dither() {
        Line::from(Span::styled(" [ ] Dither  (n/a for float)", dim_style))
    } else {
        let label = if dither { " [X] Dither" } else { " [ ] Dither" };
        if focused == 2 {
            Line::from(Span::styled(label, cursor_style))
        } else {
            Line::from(Span::styled(label, label_style))
        }
    };

    // Row 3: blank, Row 4: hints
    let hints = Line::from(vec![
        Span::styled(" Tab", hint_style),
        Span::styled(":next  ", label_style),
        Span::styled("←→", hint_style),
        Span::styled(":change  ", label_style),
        Span::styled("Space", hint_style),
        Span::styled(":check  ", label_style),
        Span::styled("Enter", hint_style),
        Span::styled(":apply", label_style),
    ]);

    let block = Block::default()
        .title("Save As")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            filename_line, format_line, Line::raw(""), dither_line, Line::raw(""), hints,
        ]).block(block),
        popup,
    );

    // Return hit-test rects: three interactive rows + apply (hints bar) as last element.
    // +1 on each vs. the line's raw index, for the header spacer row prepended above.
    let row_w = popup.width.saturating_sub(2);
    vec![
        Rect { x: popup.x + 1, y: popup.y + 2, width: row_w, height: 1 },
        Rect { x: popup.x + 1, y: popup.y + 3, width: row_w, height: 1 },
        Rect { x: popup.x + 1, y: popup.y + 5, width: row_w, height: 1 },
        Rect { x: popup.x + 1, y: popup.y + popup.height - 2, width: row_w, height: 1 },
    ]
}

fn render_confirm(frame: &mut Frame, area: Rect, text: &str) {
    let width = (text.chars().count() as u16 + 2).min(area.width);
    let height = 3.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::WARNING_FG))
        .style(Style::default().fg(theme::WARNING_FG).bg(theme::WARNING_BG));
    let paragraph = Paragraph::new(text)
        .alignment(Alignment::Center)
        .block(block);
    frame.render_widget(paragraph, popup);
}

/// Renders the Mix-to-Mono dialog: one row per source channel with a live dB field,
/// plus a hints row.  Tab cycles focus, Del = -inf, Enter applies, Esc cancels.
fn render_mix_to_mono_dialog(
    frame: &mut Frame,
    area: Rect,
    inputs: &[TextInput],
    focused: usize,
    tanh_clip: bool,
) -> Vec<Rect> {
    let n = inputs.len();
    // header spacer + channel rows + blank + tanh row + blank + hints row = n + 5 rows
    // (plus 2 for border)
    let inner_h = (n as u16) + 5;
    let height = (inner_h + 2).min(area.height);
    let width = 48u16.min(area.width);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);

    let mut lines: Vec<Line> = vec![Line::raw("")];
    for (i, ti) in inputs.iter().enumerate() {
        let ch_label = if n == 2 {
            if i == 0 { "L (dB): " } else { "R (dB): " }
        } else {
            "Ch (dB): "
        };
        let (before, under, after) = ti.split_at_cursor();
        if i == focused {
            lines.push(Line::from(vec![
                Span::styled(format!(" {ch_label}"), label_style),
                Span::styled(before, base),
                Span::styled(under, cursor_style),
                Span::styled(after, base),
                Span::raw("  "),
            ]));
        } else {
            let value = ti.value().to_string();
            lines.push(Line::from(vec![
                Span::styled(format!(" {ch_label}"), label_style),
                Span::styled(value, base),
            ]));
        }
    }
    // Blank separator before the checkbox, then tanh soft-limiter checkbox row.
    lines.push(Line::raw(""));
    let tanh_label = if tanh_clip { " [X] Tanh limiter" } else { " [ ] Tanh limiter" };
    if focused == n {
        lines.push(Line::from(Span::styled(tanh_label, cursor_style)));
    } else {
        lines.push(Line::from(Span::styled(tanh_label, label_style)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(" Tab", hint_style),
        Span::styled(":next  ", label_style),
        Span::styled("Space", hint_style),
        Span::styled(":check  ", label_style),
        Span::styled("Del", hint_style),
        Span::styled(":-inf  ", label_style),
        Span::styled("Enter", hint_style),
        Span::styled(":apply", label_style),
    ]));

    let block = Block::default()
        .title("Mix to Mono")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(Paragraph::new(lines).block(block), popup);

    // Return hit-test rects: channel rows at y+2..y+n+1 (header spacer at y+1), tanh at
    // y+n+3 (blank line at y+n+2).
    let row_w = popup.width.saturating_sub(2);
    let mut rects: Vec<Rect> = (0..n)
        .map(|i| Rect { x: popup.x + 1, y: popup.y + 2 + i as u16, width: row_w, height: 1 })
        .collect();
    rects.push(Rect { x: popup.x + 1, y: popup.y + 3 + n as u16, width: row_w, height: 1 });
    // Apply (hints bar) rect — last element triggers Enter when clicked.
    rects.push(Rect { x: popup.x + 1, y: popup.y + popup.height - 2, width: row_w, height: 1 });
    rects
}

/// Renders the Gain dialog. The layout grows with `GainRows`: a mono/multi-channel document
/// shows just the Gain field and the Tanh checkbox; a stereo document additionally shows the
/// "Per-channel gain" checkbox, and checking it splits the single Gain field into Left/Right.
/// Renders the Gain dialog. Layout is a *fixed* size regardless of `per_channel` — a stereo
/// document always reserves a line for the Right field and one for the "Per-channel gain"
/// checkbox (blank/unfocusable when not applicable), so checking the box never resizes or
/// reflows the popup:
///
/// ```text
/// Gain (dB): 0.0            <- becomes "Left (dB): " when per_channel is on
///                            <- becomes "Right (dB): 0.0" when per_channel is on
///
/// [ ] Per-channel gain      <- only shown at all when the document is stereo
///
/// [ ] Tanh limiter
///
/// Tab:next  Space:check  Enter:apply
/// ```
fn render_gain_dialog(
    frame: &mut Frame,
    area: Rect,
    input: &TextInput,
    right_input: &TextInput,
    focused: usize,
    tanh_clip: bool,
    per_channel: bool,
    is_stereo: bool,
) -> Vec<Rect> {
    let rows = GainRows::new(is_stereo, per_channel);
    let height = (if is_stereo { 11 } else { 8 }).min(area.height);
    let width = 38u16.min(area.width);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);

    let field_line = |label: &str, ti: &TextInput, row: usize| -> Line<'static> {
        if focused == row {
            let (before, under, after) = ti.split_at_cursor();
            Line::from(vec![
                Span::styled(format!(" {label}"), label_style),
                Span::styled(before, base),
                Span::styled(under, cursor_style),
                Span::styled(after, base),
                Span::raw("  "),
            ])
        } else {
            Line::from(vec![
                Span::styled(format!(" {label}"), label_style),
                Span::styled(ti.value().to_string(), base),
            ])
        }
    };

    let gain_label = if is_stereo && per_channel { "Left (dB): " } else { "Gain (dB): " };
    let gain_line = field_line(gain_label, input, 0);

    let tanh_label = if tanh_clip { " [X] Tanh limiter" } else { " [ ] Tanh limiter" };
    let tanh_style = if focused == rows.tanh { cursor_style } else { label_style };
    let tanh_line = Line::from(Span::styled(tanh_label, tanh_style));

    let hints = Line::from(vec![
        Span::styled(" Tab", hint_style),
        Span::styled(":next  ", label_style),
        Span::styled("Space", hint_style),
        Span::styled(":check  ", label_style),
        Span::styled("Enter", hint_style),
        Span::styled(":apply", label_style),
    ]);

    // Hit-test rects are placed at fixed lines per role (not packed sequentially), so the
    // reserved-but-blank Right/checkbox lines don't shift the tanh row's position — only
    // whether the Right rect exists in the list depends on `per_channel`. The `+2` (not
    // `+1`) accounts for the header spacer row prepended to `lines` below.
    let row_w = popup.width.saturating_sub(2);
    let rect_at = |line: u16| Rect { x: popup.x + 1, y: popup.y + 2 + line, width: row_w, height: 1 };

    let (lines, mut rects): (Vec<Line>, Vec<Rect>) = if is_stereo {
        let right_line = if per_channel {
            field_line("Right (dB): ", right_input, rows.right.expect("right is focusable when per_channel is on"))
        } else {
            Line::raw("")
        };
        let checkbox_label = if per_channel { " [X] Per-channel gain" } else { " [ ] Per-channel gain" };
        let checkbox_style = if Some(focused) == rows.checkbox { cursor_style } else { label_style };
        let checkbox_line = Line::from(Span::styled(checkbox_label, checkbox_style));

        let mut rects = vec![rect_at(0)];
        if per_channel {
            rects.push(rect_at(1));
        }
        rects.push(rect_at(3)); // "Per-channel gain" checkbox — always focusable on stereo.
        rects.push(rect_at(5)); // Tanh limiter checkbox.

        (
            vec![
                Line::raw(""),
                gain_line,
                right_line,
                Line::raw(""),
                checkbox_line,
                Line::raw(""),
                tanh_line,
                Line::raw(""),
                hints,
            ],
            rects,
        )
    } else {
        (
            vec![Line::raw(""), gain_line, Line::raw(""), tanh_line, Line::raw(""), hints],
            vec![rect_at(0), rect_at(2)],
        )
    };

    let block = Block::default()
        .title("Gain")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(Paragraph::new(lines).block(block), popup);

    // Apply (hints bar) rect — index == dialog_n_interactive triggers Enter.
    rects.push(Rect { x: popup.x + 1, y: popup.y + popup.height - 2, width: row_w, height: 1 });
    rects
}

fn render_fade_dialog(frame: &mut Frame, area: Rect, title: &str, curve: FadeCurve) -> Vec<Rect> {
    let width = 32u16.min(area.width);
    // header spacer + curve selector + blank + hints = 4 inner rows + 2 border
    let height = 6u16.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);

    // Row 0: curve selector
    let curve_line = Line::from(vec![
        Span::styled(" Curve:  ◄ ", label_style),
        Span::styled(curve.label(), cursor_style),
        Span::styled(" ►  ", label_style),
    ]);

    // Row 1: blank, Row 2: hints
    let hints = Line::from(vec![
        Span::styled(" ←→", hint_style),
        Span::styled(":change  ", label_style),
        Span::styled("Enter", hint_style),
        Span::styled(":apply", label_style),
    ]);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(
        Paragraph::new(vec![Line::raw(""), curve_line, Line::raw(""), hints]).block(block),
        popup,
    );

    let row_w = popup.width.saturating_sub(2);
    vec![
        Rect { x: popup.x + 1, y: popup.y + 2, width: row_w, height: 1 },
        Rect { x: popup.x + 1, y: popup.y + popup.height - 2, width: row_w, height: 1 },
    ]
}

fn render_dialog(frame: &mut Frame, area: Rect, dialog: &Dialog, catalog: &crate::model::cdp::CdpCatalog) -> Vec<Rect> {
    match dialog {
        Dialog::MixToMono { inputs, focused, tanh_clip } => {
            return render_mix_to_mono_dialog(frame, area, inputs, *focused, *tanh_clip);
        }
        Dialog::Gain { input, right_input, tanh_clip, per_channel, is_stereo, focused } => {
            return render_gain_dialog(frame, area, input, right_input, *focused, *tanh_clip, *per_channel, *is_stereo);
        }
        Dialog::FadeIn { curve } => {
            return render_fade_dialog(frame, area, "Fade In", *curve);
        }
        Dialog::FadeOut { curve } => {
            return render_fade_dialog(frame, area, "Fade Out", *curve);
        }
        Dialog::ExportRegions {
            folder_input, base_name_input, depth, dither,
            limit_length, limit_length_input, normalize, normalize_input,
            fade_in, fade_in_input, fade_out, fade_out_input, focused,
        } => {
            return render_export_regions_dialog(
                frame, area, folder_input, base_name_input, *depth, *dither,
                *limit_length, limit_length_input, *normalize, normalize_input,
                *fade_in, fade_in_input, *fade_out, fade_out_input, *focused,
            );
        }
        Dialog::Info { message } => {
            return render_info_dialog(frame, area, message);
        }
        Dialog::CdpBrowser { search, groups, group_selected, group_focus, entries, selected, .. } => {
            return render_cdp_browser_dialog(
                frame, area, search, groups, *group_selected, *group_focus, entries, *selected, catalog,
            );
        }
        Dialog::CdpParams {
            catalog_index, fields, second_input, focus, error, preview, envelope, list_edit,
            presets, preset_selected, save_prompt, scroll,
        } => {
            let def = catalog.processes.get(*catalog_index);
            if let Some(edit) = envelope {
                return render_cdp_envelope_editor(frame, area, fields, edit, def);
            }
            if let Some(edit) = list_edit {
                return render_cdp_list_editor(frame, area, fields, edit, def);
            }
            return render_cdp_params_dialog(
                frame, area, def, fields, second_input.as_ref(), *focus, error, preview,
                presets, *preset_selected, save_prompt.as_ref(), *scroll,
            );
        }
        Dialog::CdpRunning { title, step_label, step_index, step_total, started, purpose, .. } => {
            return render_cdp_running_dialog(
                frame, area, title, step_label, *step_index, *step_total, started.elapsed(), *purpose,
            );
        }
        Dialog::CdpOutput { title, lines, scroll } => {
            return render_cdp_output_dialog(frame, area, title, lines, *scroll);
        }
        Dialog::CdpSetup { input, error } => {
            return render_cdp_setup_dialog(frame, area, input, error.as_deref());
        }
        _ => {}
    }

    // Simple single-row text dialogs (Normalize, Resample, RenameMarker, OpenDirectory, RenameBuffer).
    let (title, prefix, input, suffix): (&str, String, Option<&TextInput>, String) = match dialog {
        Dialog::Normalize { input } => {
            ("Normalize", " Target peak (dBFS): ".into(), Some(input), " ".into())
        }
        Dialog::Resample { input, current_rate } => {
            ("Resample", format!(" New rate (current {current_rate} Hz): "), Some(input), " ".into())
        }
        Dialog::RenameMarker { input, .. } => ("Rename Marker", " Label: ".into(), Some(input), " ".into()),
        Dialog::OpenDirectory { input } => ("Open Directory", " Path: ".into(), Some(input), " ".into()),
        Dialog::RenameBuffer { input, .. } => ("Rename Buffer", " New name: ".into(), Some(input), " ".into()),
        Dialog::RenameFile { input, .. } => ("Rename File", " New name: ".into(), Some(input), " ".into()),
        Dialog::Gain { .. }
        | Dialog::FadeIn { .. }
        | Dialog::FadeOut { .. }
        | Dialog::MixToMono { .. }
        | Dialog::ExportRegions { .. }
        | Dialog::Info { .. }
        | Dialog::CdpSetup { .. }
        | Dialog::CdpBrowser { .. }
        | Dialog::CdpParams { .. }
        | Dialog::CdpRunning { .. }
        | Dialog::CdpOutput { .. } => {
            unreachable!("handled by match arms above")
        }
    };

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let mut spans = vec![Span::styled(prefix.clone(), base)];
    let mut content_len = prefix.chars().count();
    if let Some(input) = input {
        let (before, under, after) = input.split_at_cursor();
        content_len += before.chars().count() + under.chars().count() + after.chars().count();
        spans.push(Span::styled(before, base));
        spans.push(Span::styled(under, cursor_style));
        spans.push(Span::styled(after, base));
    }
    spans.push(Span::styled(suffix.clone(), base));
    content_len += suffix.chars().count();

    let width = (content_len as u16 + 2).min(area.width);
    // Border + blank spacer row + content row + border.
    let height = 4.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(
        Paragraph::new(vec![Line::raw(""), Line::from(spans)]).block(block),
        popup,
    );
    Vec::new()
}

fn render_export_regions_dialog(
    frame: &mut Frame,
    area: Rect,
    folder_input: &TextInput,
    base_name_input: &TextInput,
    depth: BitDepth,
    dither: bool,
    limit_length: bool,
    limit_length_input: &TextInput,
    normalize: bool,
    normalize_input: &TextInput,
    fade_in: bool,
    fade_in_input: &TextInput,
    fade_out: bool,
    fade_out_input: &TextInput,
    focused: usize,
) -> Vec<Rect> {
    let width = 54u16.min(area.width);
    // header spacer + subfolder + basename + blank + format + dither + blank + limit_length
    // + normalize + blank + fade_in + fade_out + blank + hints = 14 inner rows + 2 border
    const FULL_HEIGHT: u16 = 16;
    let height = FULL_HEIGHT.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);
    let dim_style = Style::default().fg(theme::BORDER).bg(theme::SURFACE0);

    let make_text_row = |label: &'static str, input: &TextInput, is_focused: bool| {
        if is_focused {
            let (before, under, after) = input.split_at_cursor();
            Line::from(vec![
                Span::styled(label, label_style),
                Span::styled(before, base),
                Span::styled(under, cursor_style),
                Span::styled(after, base),
                Span::raw("  "),
            ])
        } else {
            Line::from(vec![
                Span::styled(label, label_style),
                Span::styled(input.value().to_string(), base),
            ])
        }
    };

    let folder_line = make_text_row(" Subfolder: ", folder_input, focused == er_focus::SUBFOLDER);
    let base_line   = make_text_row(" Base name: ", base_name_input, focused == er_focus::BASE_NAME);

    let format_line = if focused == er_focus::FORMAT {
        Line::from(vec![
            Span::styled("   Format: ◄ ", label_style),
            Span::styled(depth.label(), cursor_style),
            Span::styled(" ►  ", label_style),
        ])
    } else {
        Line::from(vec![
            Span::styled("   Format: ", label_style),
            Span::styled(depth.label(), base),
        ])
    };

    let dither_line = if !depth.supports_dither() {
        Line::from(Span::styled("   [ ] Dither  (n/a for float)", dim_style))
    } else {
        let label = if dither { "   [X] Dither" } else { "   [ ] Dither" };
        if focused == er_focus::DITHER {
            Line::from(Span::styled(label, cursor_style))
        } else {
            Line::from(Span::styled(label, label_style))
        }
    };

    // A "[ ] Label   <value><suffix>" row shared by limit length, normalize, and the two
    // fades — a checkbox gates a text field, and `cb_idx`/`val_idx` are that row's two
    // focus indices (the checkbox and the value field are separately focusable/tabbable).
    // `label` is padded to a fixed width so all four rows' value fields land in the same
    // column regardless of how long each row's label text is — which is also what lets
    // `handle_dialog_row_click` split clicks at `er_focus::VALUE_COL`.
    const ROW_LABEL_WIDTH: usize = er_focus::ROW_LABEL_WIDTH;
    let checkbox_value_row = |label: &str, checked: bool, cb_idx: usize, val_idx: usize, input: &TextInput, suffix: &str| {
        let cb = if checked { "[X]" } else { "[ ]" };
        let cb_style = if focused == cb_idx { cursor_style } else if checked { label_style } else { dim_style };
        let suffix_style = if checked { label_style } else { dim_style };
        if focused == val_idx {
            let (before, under, after) = input.split_at_cursor();
            Line::from(vec![
                Span::styled(format!("   {cb} {label:<ROW_LABEL_WIDTH$} "), cb_style),
                Span::styled(before, base),
                Span::styled(under, cursor_style),
                Span::styled(after, base),
                Span::styled(suffix.to_string(), suffix_style),
            ])
        } else {
            let val_style = if checked { base } else { dim_style };
            Line::from(vec![
                Span::styled(format!("   {cb} {label:<ROW_LABEL_WIDTH$} "), cb_style),
                Span::styled(input.value().to_string(), val_style),
                Span::styled(suffix.to_string(), suffix_style),
            ])
        }
    };

    let limit_length_line = checkbox_value_row(
        "Limit length to", limit_length, er_focus::LIMIT_CB, er_focus::LIMIT_MS, limit_length_input, " ms",
    );
    let normalize_line = checkbox_value_row(
        "Normalize to", normalize, er_focus::NORMALIZE_CB, er_focus::NORMALIZE_DB, normalize_input, " dB",
    );
    let fade_in_line = checkbox_value_row(
        "Fade in at start", fade_in, er_focus::FADE_IN_CB, er_focus::FADE_IN_MS, fade_in_input, " ms",
    );
    let fade_out_line = checkbox_value_row(
        "Fade out at end", fade_out, er_focus::FADE_OUT_CB, er_focus::FADE_OUT_MS, fade_out_input, " ms",
    );

    let do_active = !folder_input.value().trim().is_empty() && !base_name_input.value().trim().is_empty();
    let do_style = if do_active { hint_style } else { dim_style };
    let hints = Line::from(vec![
        Span::styled(" Tab", hint_style),
        Span::styled(":next  ", label_style),
        Span::styled("←→", hint_style),
        Span::styled(":change  ", label_style),
        Span::styled("Space", hint_style),
        Span::styled(":check  ", label_style),
        Span::styled("Enter", do_style),
        Span::styled(":Do!", label_style),
    ]);

    let block = Block::default()
        .title("Export Regions to Subfolder")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            folder_line, base_line, Line::raw(""),
            format_line, dither_line, Line::raw(""),
            limit_length_line, normalize_line, Line::raw(""),
            fade_in_line, fade_out_line, Line::raw(""),
            hints,
        ]).block(block),
        popup,
    );

    // Each interactive row's Rect is clipped to the popup's inner area: on a terminal
    // shorter than FULL_HEIGHT the bottom rows aren't drawn, and an unclipped Rect there
    // would still catch clicks — worse, the hints Rect (placed relative to popup.height)
    // would collide with whichever field row happens to render at that offset, making a
    // "submit" click toggle that row's checkbox instead.
    let inner = Rect {
        x: popup.x + 1,
        y: popup.y + 1,
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    };
    let row = |y_offset: u16| {
        Rect { x: popup.x + 1, y: popup.y + y_offset, width: inner.width, height: 1 }.intersection(inner)
    };
    vec![
        row(2),  // subfolder (row 0) -- +1 for the header spacer row
        row(3),  // base name (row 1)
        row(5),  // format (row 2)
        row(6),  // dither (row 3)
        row(8),  // limit length (row 4)
        row(9),  // normalize (row 5)
        row(11), // fade in (row 6)
        row(12), // fade out (row 7)
        // Hints/apply bar — clicking it (or anywhere past row 7) submits, matching
        // handle_dialog_row_click's `row >= dialog_n_interactive` convention. Only
        // present when the popup is full-height: when clamped, the row at
        // popup.height - 2 shows a clipped field line, not the hints bar (Enter still
        // submits — this only drops the mouse target, never the action).
        if popup.height == FULL_HEIGHT {
            Rect { x: popup.x + 1, y: popup.y + popup.height - 2, width: inner.width, height: 1 }
        } else {
            Rect::default()
        },
    ]
}

/// The CDP process browser: a search field over a scrollable, filtered list of catalog
/// entries. Keyboard-only in v1 (returns no interactive rects) — the other dialogs in this
/// app that support mouse row-clicks (`ExportRegions`, `Gain`, `MixToMono`) are exactly the
/// ones with checkboxes/cyclers a click meaningfully shortcuts; a plain filtered list is
/// just as fast to drive from the keyboard, so mouse support was left for a follow-up
/// rather than adding `dialog_row_rects` plumbing this pass didn't strictly need.
/// The CDP-directory setup prompt. Fixed-width (unlike the generic single-row dialog path,
/// which sizes to its content and so would visibly grow column-by-column as the user types
/// a path) with the validation error on its own line instead of hijacking the title.
fn render_cdp_setup_dialog(
    frame: &mut Frame,
    area: Rect,
    input: &TextInput,
    error: Option<&str>,
) -> Vec<Rect> {
    let width = 72u16.min(area.width);
    let height = 8u16.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);
    let error_style = Style::default().fg(theme::RED).bg(theme::SURFACE0);

    let (before, under, after) = input.split_at_cursor();
    let lines = vec![
        Line::raw(""),
        Line::from(Span::styled(" Directory containing the CDP binaries:", label_style)),
        Line::from(vec![
            Span::styled(" ", base),
            Span::styled(before, base),
            Span::styled(under, cursor_style),
            Span::styled(after, base),
        ]),
        match error {
            Some(msg) => Line::from(Span::styled(format!(" ! {msg}"), error_style)),
            None => Line::raw(""),
        },
        Line::raw(""),
        Line::from(vec![
            Span::styled(" Enter", hint_style),
            Span::styled(":save & continue  ", label_style),
            Span::styled("Esc", hint_style),
            Span::styled(":cancel", label_style),
        ]),
    ];

    let block = Block::default()
        .title("CDP Directory")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
    Vec::new()
}

/// Formats a `ParamKind::Number`'s valid range for inline display next to its field, e.g.
/// `[0.1-100.0]` — every input field needs its range visible, not just discoverable by
/// typing an out-of-range value and getting bounced. Uses the same always-a-decimal-point
/// convention as `format_cdp_float_for_display` (see task: pre-filled float defaults) so a
/// whole-number bound reads unambiguously as a float range.
fn format_cdp_range(min: f64, max: f64) -> String {
    format!("[{}-{}]", format_cdp_float_for_display(min), format_cdp_float_for_display(max))
}


/// Maps a value in `[min, max]` to a grid row in `[0, height-1]`, row 0 being the *top* of
/// the grid (the max end) — matches how the waveform/dB-scale widgets orient their vertical
/// axis, so this reads the same way the rest of the app's plots do.
fn cdp_envelope_value_to_row(v: f64, min: f64, max: f64, height: usize) -> usize {
    if height <= 1 || max <= min {
        return 0;
    }
    let frac = ((v - min) / (max - min)).clamp(0.0, 1.0);
    ((1.0 - frac) * (height - 1) as f64).round() as usize
}

/// Inverse of the rendering-time (time, value) → (col, row) mapping: given a mouse position
/// (clamped into `grid`), returns the (time, value) point it corresponds to. `value` is not
/// pre-clamped to `[min, max]` past what the row clamp already guarantees, since callers
/// that need range-safety (inserting a point) clamp explicitly — keeping this function a
/// pure inverse of the grid mapping rather than baking in a policy about what's valid.
fn cdp_envelope_mouse_to_domain(grid: Rect, time_max: f64, min: f64, max: f64, col: u16, row: u16) -> (f64, f64) {
    let clamped_col = col.clamp(grid.x, grid.x + grid.width.saturating_sub(1));
    let clamped_row = row.clamp(grid.y, grid.y + grid.height.saturating_sub(1));
    let frac_x = if grid.width <= 1 { 0.0 } else { (clamped_col - grid.x) as f64 / (grid.width - 1) as f64 };
    let frac_y = if grid.height <= 1 { 0.0 } else { (clamped_row - grid.y) as f64 / (grid.height - 1) as f64 };
    (frac_x * time_max, max - frac_y * (max - min))
}

/// The screen cell a breakpoint `(time, value)` renders at — the exact forward mapping
/// `render_cdp_envelope_editor`'s marker overlay uses, factored out so mouse hit-testing
/// can never silently disagree with where a point is actually drawn.
fn cdp_envelope_point_cell(grid: Rect, time_max: f64, min: f64, max: f64, point: (f64, f64)) -> (u16, u16) {
    let grid_width = grid.width as usize;
    let col = if time_max <= 0.0 || grid_width <= 1 {
        0
    } else {
        ((point.0 / time_max) * (grid_width - 1) as f64).round().clamp(0.0, (grid_width - 1) as f64) as u16
    };
    let row = cdp_envelope_value_to_row(point.1, min, max, grid.height as usize) as u16;
    (grid.x + col, grid.y + row)
}

/// The index of the breakpoint whose rendered cell is closest (Chebyshev distance, matching
/// how "closest" looks to the eye on a character grid better than Euclidean would) to
/// `(col, row)`, plus that distance in cells — `None` only if `points` is empty, which
/// never happens in practice (`CdpEnvelopeEdit` always keeps at least 2).
fn cdp_envelope_nearest_point(
    points: &[(f64, f64)],
    grid: Rect,
    time_max: f64,
    min: f64,
    max: f64,
    col: u16,
    row: u16,
) -> Option<(usize, u16)> {
    points
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let (pc, pr) = cdp_envelope_point_cell(grid, time_max, min, max, p);
            (i, pc.abs_diff(col).max(pr.abs_diff(row)))
        })
        .min_by_key(|&(_, d)| d)
}

/// The ASCII breakpoint-curve editor, replacing the whole popup while
/// `Dialog::CdpBrowser.envelope` is `Some` (see its doc comment for the interaction model —
/// this is deliberately plain terminal characters rather than a bitmap; a kitty-graphics
/// version of the same editor is a plausible future upgrade but out of scope here). The
/// curve is drawn as a "staircase" — a vertical run of `│` wherever the interpolated value's
/// row changes between adjacent columns, `─` where it doesn't — rather than true diagonal
/// line segments, which keeps the per-cell logic to one row-difference comparison instead of
/// sub-cell rasterization; breakpoints themselves are drawn as `●` (the selected one
/// reverse-video) on top of that backdrop.
/// Pure layout for the envelope editor popup, shared by the renderer and
/// `App::try_handle_cdp_envelope_mouse` so the two can never drift apart — the mouse handler
/// needs the exact on-screen `grid` rect the renderer used to place breakpoints, and
/// recomputing it independently would silently break the moment one side's constants
/// changed without the other's.
struct CdpEnvelopeLayout {
    popup: Rect,
    /// The plotting area only — `Y_LABEL_WIDTH` columns and the header-spacer row excluded,
    /// so `(mouse.column - grid.x, mouse.row - grid.y)` is directly a (col, row) pair into
    /// `GRID_HEIGHT`/`grid_width`.
    grid: Rect,
}

const CDP_ENVELOPE_GRID_HEIGHT: usize = 16;
// Must match the y-axis label span's actual rendered width exactly (`{:>6}` + one
// separator char = 7) — any mismatch here silently shifts the mouse-hit-testing grid one
// column off from where breakpoints are actually drawn (a real bug this constant caused
// once already: `8` here against a 7-wide label made a click squarely on the leftmost
// point's marker land just outside `grid`, missing it entirely).
const CDP_ENVELOPE_Y_LABEL_WIDTH: u16 = 7;

fn cdp_envelope_layout(area: Rect) -> CdpEnvelopeLayout {
    let width = 130u16.min(area.width);
    // spacer+grid+x-axis-line+x-axis-labels+blank+point-status+hints+mouse-hints+2 border
    let height = (CDP_ENVELOPE_GRID_HEIGHT as u16 + 9).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    // Mirrors `Block::inner` for a bordered block (1 cell on every side) without needing a
    // live `Block` instance just to compute a Rect.
    let inner = Rect {
        x: popup.x + 1,
        y: popup.y + 1,
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    };
    let grid_width = (inner.width as usize).saturating_sub(CDP_ENVELOPE_Y_LABEL_WIDTH as usize).max(10);
    let grid = Rect {
        x: inner.x + CDP_ENVELOPE_Y_LABEL_WIDTH,
        y: inner.y + 1, // +1 for the header spacer row
        width: grid_width as u16,
        height: CDP_ENVELOPE_GRID_HEIGHT as u16,
    };
    CdpEnvelopeLayout { popup, grid }
}

fn render_cdp_envelope_editor(
    frame: &mut Frame,
    area: Rect,
    fields: &[CdpField],
    edit: &CdpEnvelopeEdit,
    def: Option<&crate::model::cdp::ProcessDef>,
) -> Vec<Rect> {
    let Some(CdpField::Number { min, max, .. }) = fields.get(edit.field_index) else {
        return Vec::new();
    };
    let (min, max) = (*min, *max);
    let param = def.and_then(|d| d.params.get(edit.field_index));
    let param_name = param.map(|p| p.name.as_str()).unwrap_or("Envelope");
    let required_envelope = param.is_some_and(|p| p.required_envelope);

    let layout = cdp_envelope_layout(area);
    let popup = layout.popup;
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);
    let dim_style = Style::default().fg(theme::BORDER).bg(theme::SURFACE0);
    let point_style = Style::default().fg(theme::FOCUS).bg(theme::SURFACE0);
    let selected_style = Style::default().fg(theme::SURFACE0).bg(theme::FOCUS);

    let block = Block::default()
        .title(format!("Envelope: {param_name}"))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    const GRID_HEIGHT: usize = CDP_ENVELOPE_GRID_HEIGHT;
    const Y_LABEL_WIDTH: usize = CDP_ENVELOPE_Y_LABEL_WIDTH as usize;
    let grid_width = layout.grid.width as usize;

    // Precompute each column's interpolated row so the staircase logic only ever compares
    // adjacent columns, never re-interpolates.
    let rows_at_col: Vec<usize> = (0..grid_width)
        .map(|col| {
            let t = if grid_width <= 1 { 0.0 } else { edit.time_max * col as f64 / (grid_width - 1) as f64 };
            let v = interp_cdp_envelope(&edit.points, t);
            cdp_envelope_value_to_row(v, min, max, GRID_HEIGHT)
        })
        .collect();

    let mut lines = vec![Line::raw("")];
    for row in 0..GRID_HEIGHT {
        let mut spans: Vec<Span> = Vec::with_capacity(grid_width + Y_LABEL_WIDTH);
        let y_label = if row == 0 {
            format!("{:>6}\u{2524}", format_cdp_float_for_display(max))
        } else if row + 1 == GRID_HEIGHT {
            format!("{:>6}\u{2524}", format_cdp_float_for_display(min))
        } else {
            format!("{:>7}", "")
        };
        spans.push(Span::styled(y_label, dim_style));

        for col in 0..grid_width {
            let this_row = rows_at_col[col];
            let prev_row = if col == 0 { this_row } else { rows_at_col[col - 1] };
            let ch = if row == this_row {
                '\u{2500}' // ─
            } else if (prev_row < this_row && row >= prev_row && row < this_row)
                || (prev_row > this_row && row <= prev_row && row > this_row)
            {
                '\u{2502}' // │ connecting the previous column's row to this one
            } else {
                ' '
            };
            spans.push(Span::styled(ch.to_string(), base));
        }
        lines.push(Line::from(spans));
    }

    // Overlay breakpoint markers on top of the interpolated backdrop. Uses the same forward
    // mapping the mouse handler's hit-testing does (`cdp_envelope_point_cell`), so a click
    // can never land somewhere that visually disagrees with where the dot is drawn.
    let mut overlay_lines = lines.clone();
    for (i, &point) in edit.points.iter().enumerate() {
        let (screen_col, screen_row) = cdp_envelope_point_cell(layout.grid, edit.time_max, min, max, point);
        let col = (screen_col - layout.grid.x) as usize;
        let row = (screen_row - layout.grid.y) as usize;
        let line_idx = 1 + row; // +1 for the header spacer line
        let span_idx = 1 + col; // +1 for the y-axis label span
        if let Some(line) = overlay_lines.get_mut(line_idx) {
            if let Some(span) = line.spans.get_mut(span_idx) {
                let style = if i == edit.selected { selected_style } else { point_style };
                *span = Span::styled("\u{25cf}", style);
            }
        }
    }
    lines = overlay_lines;

    // X axis.
    let mut axis = String::from(" ".repeat(Y_LABEL_WIDTH - 1));
    axis.push('\u{2514}');
    axis.push_str(&"\u{2500}".repeat(grid_width));
    lines.push(Line::from(Span::styled(axis, dim_style)));
    let time_label = format!(
        "{}{:<width$}{}",
        " ".repeat(Y_LABEL_WIDTH),
        format!("{:.3}s", 0.0),
        format!("{:.3}s", edit.time_max),
        width = grid_width.saturating_sub(8),
    );
    lines.push(Line::from(Span::styled(time_label, dim_style)));
    lines.push(Line::raw(""));

    let selected_point = edit.points.get(edit.selected).copied().unwrap_or((0.0, 0.0));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" Point {}/{}: t={:.3}s v={}", edit.selected + 1, edit.points.len(), selected_point.0, format_cdp_float_for_display(selected_point.1)),
            label_style,
        ),
    ]));
    let mut hint_spans = vec![
        Span::styled(" \u{2190}\u{2192}", hint_style),
        Span::styled(":point  ", label_style),
        Span::styled("Shift+\u{2190}\u{2192}", hint_style),
        Span::styled(":move time  ", label_style),
        Span::styled("\u{2191}\u{2193}", hint_style),
        Span::styled(":value  ", label_style),
        Span::styled("Shift+\u{2191}\u{2193}", hint_style),
        Span::styled(":fine value  ", label_style),
        Span::styled("n", hint_style),
        Span::styled(":insert  ", label_style),
        Span::styled("Del", hint_style),
        Span::styled(":remove  ", label_style),
    ];
    // A required datafile field has no valid constant to revert to (`ParamDef.required_envelope`'s
    // doc comment) — 'c' is a no-op there, so the hint that advertises it is omitted too.
    if !required_envelope {
        hint_spans.push(Span::styled("c", hint_style));
        hint_spans.push(Span::styled(":constant  ", label_style));
    }
    hint_spans.push(Span::styled("Enter", hint_style));
    hint_spans.push(Span::styled(":save  ", label_style));
    hint_spans.push(Span::styled("Esc", hint_style));
    hint_spans.push(Span::styled(":cancel", label_style));
    lines.push(Line::from(hint_spans));
    lines.push(Line::from(vec![
        Span::styled(" Click", hint_style),
        Span::styled(":select  ", label_style),
        Span::styled("Dbl-click", hint_style),
        Span::styled(":insert  ", label_style),
        Span::styled("Drag", hint_style),
        Span::styled(":move  ", label_style),
        Span::styled("Shift+drag", hint_style),
        Span::styled(":fine move  ", label_style),
        Span::styled("Shift+click", hint_style),
        Span::styled(":delete", label_style),
    ]));

    frame.render_widget(Paragraph::new(lines), inner);
    vec![layout.grid]
}

/// Fixed visible-row count for `render_cdp_list_editor` — a plain scrolling list, no
/// graphics-mode rendering, so unlike the envelope editor's grid there's no reason to size
/// it off the terminal beyond a sane fixed cap.
const CDP_LIST_EDITOR_ROWS: usize = 16;

/// The plain-list editor: one number per row, no time axis or interpolated curve — the
/// `required_list` counterpart to `render_cdp_envelope_editor`, much simpler since there's
/// nothing to interpolate between entries (`CdpListEdit`'s doc comment has the key
/// semantics). Keyboard-only, matching this popup's simplicity — no mouse hit-testing yet.
fn render_cdp_list_editor(
    frame: &mut Frame,
    area: Rect,
    fields: &[CdpField],
    edit: &CdpListEdit,
    def: Option<&crate::model::cdp::ProcessDef>,
) -> Vec<Rect> {
    let Some(CdpField::List { min, max, .. }) = fields.get(edit.field_index) else {
        return Vec::new();
    };
    let (min, max) = (*min, *max);
    let param = def.and_then(|d| d.params.get(edit.field_index));
    let param_name = param.map(|p| p.name.as_str()).unwrap_or("List");

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);
    let dim_style = Style::default().fg(theme::BORDER).bg(theme::SURFACE0);
    let selected_style = Style::default().fg(theme::SURFACE0).bg(theme::FOCUS);

    let width = 50u16.min(area.width);
    // header spacer + LIST_ROWS + blank + 2 hint lines, + 2 border.
    let height = (1 + CDP_LIST_EDITOR_ROWS as u16 + 1 + 2 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .title(format!("List: {param_name}"))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Fixed at the constant (not clamped to `inner.height`), matching every other fixed-size
    // CDP popup's convention in this file — a terminal too short simply crops the bottom.
    let list_rows = CDP_LIST_EDITOR_ROWS;
    let scroll_top = edit.selected.saturating_sub(list_rows.saturating_sub(1));
    let mut lines = vec![Line::raw("")];
    let mut rendered_rows = 0;
    for (i, &v) in edit.values.iter().enumerate().skip(scroll_top).take(list_rows) {
        let style = if i == edit.selected { selected_style } else { base };
        let text = format!(" {:>3}: {}", i + 1, format_cdp_float_for_display(v));
        lines.push(Line::from(Span::styled(text, style)));
        rendered_rows += 1;
    }
    for _ in rendered_rows..list_rows {
        lines.push(Line::raw(""));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(" \u{2190}\u{2192}", hint_style),
        Span::styled(":entry  ", label_style),
        Span::styled("\u{2191}\u{2193}", hint_style),
        Span::styled(":value  ", label_style),
        Span::styled("Shift+\u{2191}\u{2193}", hint_style),
        Span::styled(":fine value  ", label_style),
        Span::styled("n", hint_style),
        Span::styled(":insert  ", label_style),
        Span::styled("Del", hint_style),
        Span::styled(":remove", label_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" Enter", hint_style),
        Span::styled(":save  ", label_style),
        Span::styled("Esc", hint_style),
        Span::styled(":cancel  ", label_style),
        Span::styled(format!("range {}", format_cdp_range(min, max)), dim_style),
    ]));

    frame.render_widget(Paragraph::new(lines), inner);
    Vec::new()
}

/// The unified CDP process browser/params dialog: a Groups column, a searchable Processes
/// column, and a description column showing the highlighted process's full `description`
/// (word-wrapped) — see `Dialog::CdpBrowser`'s doc comment for the interaction model.
/// Visible process-list rows in `Dialog::CdpBrowser` — a fixed constant (not
/// content-dependent) shared between the renderer and `App::handle_dialog_row_click`'s
/// scroll-position math, so a click can never disagree with what's actually on screen. Also
/// the fixed height of the Groups column's list — the full group list (currently under 20
/// entries: `All`, `Recent`, plus one row per real `subcategory`) always fits without its
/// own scrolling, which is what lets `row` mean the same "row index" in both columns.
const CDP_BROWSER_LIST_ROWS: usize = 24;

/// The CDP process browser: Groups on the left, the searchable process list in the middle,
/// the highlighted process's full `description` on the right. Deliberately a *fixed*-size
/// popup (width and height are constants, independent of `entries`/`groups`/the selected
/// process) — see `Dialog::CdpBrowser`'s doc comment for why. Returns one `Rect` per visible
/// row — each spanning *both* the Groups and Processes columns, since they render in
/// lockstep row-for-row (`App::handle_dialog_row_click` disambiguates which column a click
/// landed in from `x_in_row` against `CDP_GROUP_COL_WIDTH`) — plus a trailing hints-bar
/// `Rect`, matching every other dialog's row-click convention in this file.
fn render_cdp_browser_dialog(
    frame: &mut Frame,
    area: Rect,
    search: &TextInput,
    groups: &[String],
    group_selected: usize,
    group_focus: bool,
    entries: &[usize],
    selected: usize,
    catalog: &crate::model::cdp::CdpCatalog,
) -> Vec<Rect> {
    let def = entries.get(selected).and_then(|&i| catalog.processes.get(i));

    let width = 150u16.min(area.width);
    // header spacer + search/label + blank + LIST_ROWS + blank + hints, + 2 border.
    let height = (1 + 1 + 1 + CDP_BROWSER_LIST_ROWS as u16 + 1 + 1 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);
    let dim_style = Style::default().fg(theme::BORDER).bg(theme::SURFACE0);
    // The focused column's header is bold peach (matches `theme::FOCUS`'s use elsewhere as
    // the one accent for "this is where input goes"); its highlighted row is a full reverse
    // block. The *other* column still shows which entry is selected (peach text, no
    // reverse) so switching focus back and forth never loses track of either choice — but
    // deliberately isn't the header's bold+peach combination, so a glance at either can't be
    // mistaken for "this column has focus" (CLAUDE.md: one accent per role, not stacked).
    let focus_label_style =
        Style::default().fg(theme::FOCUS).bg(theme::SURFACE0).add_modifier(ratatui::style::Modifier::BOLD);
    let soft_selected_style = Style::default().fg(theme::FOCUS).bg(theme::SURFACE0);

    let block = Block::default()
        .title("CDP Process")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    const PROCESSES_WIDTH: u16 = 46;
    let cols = Layout::horizontal([
        Constraint::Length(CDP_GROUP_COL_WIDTH),
        Constraint::Length(PROCESSES_WIDTH),
        Constraint::Min(10),
    ])
    .split(inner);
    let groups_col = cols[0];
    let processes_block = Block::default().borders(Borders::LEFT).border_style(Style::default().fg(theme::BORDER));
    let processes_col = processes_block.inner(cols[1]);
    frame.render_widget(processes_block, cols[1]);
    let desc_block = Block::default().borders(Borders::LEFT).border_style(Style::default().fg(theme::BORDER));
    let desc_col = desc_block.inner(cols[2]);
    frame.render_widget(desc_block, cols[2]);

    // Blank + label/search + blank precede the list in both columns, so a given row index
    // lands on the same screen line in either one.
    const HEADER_ROWS: u16 = 3;
    let list_rows = CDP_BROWSER_LIST_ROWS;

    // ---- Groups column ----
    let mut group_lines = vec![
        Line::raw(""),
        Line::from(Span::styled(" Groups", if group_focus { focus_label_style } else { label_style })),
        Line::raw(""),
    ];
    for (i, name) in groups.iter().enumerate() {
        let text = format!(" {name}");
        let style = if i == group_selected {
            if group_focus { cursor_style } else { soft_selected_style }
        } else {
            base
        };
        group_lines.push(Line::from(Span::styled(text, style)));
    }
    for _ in groups.len()..list_rows {
        group_lines.push(Line::raw(""));
    }
    frame.render_widget(Paragraph::new(group_lines), groups_col);

    // ---- Processes column: search + process list ----
    let (before, under, after) = search.split_at_cursor();
    let mut lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled(" Search: ", if group_focus { label_style } else { focus_label_style }),
            Span::styled(before, base),
            Span::styled(under, cursor_style),
            Span::styled(after, base),
        ]),
        Line::raw(""),
    ];

    // Fixed at the constant (not clamped to `inner.height`) so `App::handle_dialog_row_click`
    // — which can't see `inner.height` — computes the exact same `scroll_top` a click needs
    // to land on the right entry; on a terminal too short to show all of them, the popup's
    // own height clamp (above) simply crops the bottom of the list, matching how every other
    // dialog in this file that doesn't have real scroll support handles overflow.
    let scroll_top = selected.saturating_sub(list_rows.saturating_sub(1));
    let mut rendered_rows = 0;
    if entries.is_empty() {
        lines.push(Line::from(Span::styled(" No matches", dim_style)));
        rendered_rows = 1;
    } else {
        for (row, &catalog_idx) in entries.iter().enumerate().skip(scroll_top).take(list_rows) {
            let Some(d) = catalog.processes.get(catalog_idx) else { continue };
            let text = format!(" {}", d.title);
            let style = if row == selected {
                if group_focus { soft_selected_style } else { cursor_style }
            } else {
                base
            };
            lines.push(Line::from(Span::styled(text, style)));
            rendered_rows += 1;
        }
    }
    for _ in rendered_rows..list_rows {
        lines.push(Line::raw(""));
    }
    frame.render_widget(Paragraph::new(lines), processes_col);

    // One combined Rect per row spans both columns; see this function's doc comment for why.
    let click_width = (processes_col.x + processes_col.width).saturating_sub(groups_col.x);
    let mut row_rects = Vec::with_capacity(list_rows + 1);
    for row in 0..list_rows {
        row_rects.push(Rect {
            x: groups_col.x,
            y: groups_col.y + HEADER_ROWS + row as u16,
            width: click_width,
            height: 1,
        });
    }

    // ---- Hints bar: spans the full popup width, pinned to the bottom ----
    let hints_row = inner.y + inner.height.saturating_sub(1);
    let hints_rect = Rect { x: inner.x, y: hints_row, width: inner.width, height: 1 };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" \u{2191}\u{2193}", hint_style),
            Span::styled(":select  ", label_style),
            Span::styled("Tab", hint_style),
            Span::styled(":column  ", label_style),
            Span::styled("PgUp/PgDn", hint_style),
            Span::styled(":page  ", label_style),
            Span::styled("Enter", hint_style),
            Span::styled(":open  ", label_style),
            Span::styled("Esc", hint_style),
            Span::styled(":cancel", label_style),
        ])),
        hints_rect,
    );
    row_rects.push(hints_rect);

    // ---- Description column: the selected process's full description, word-wrapped ----
    // A 1-column margin baked into the *Rect* (not a leading space in the text) so it
    // applies uniformly to every wrapped row, not just each logical line's first visual
    // row — a leading `" "` in the text only padded the row `Wrap` happened to start on.
    let desc_padded =
        Rect { x: desc_col.x + 1, y: desc_col.y, width: desc_col.width.saturating_sub(2), height: desc_col.height };
    let desc_lines = match def {
        Some(d) => vec![
            Line::raw(""),
            Line::from(Span::styled(d.title.as_str(), label_style)),
            Line::from(Span::styled("\u{2500}".repeat(desc_padded.width as usize), dim_style)),
            Line::raw(""),
            Line::from(Span::styled(d.description.trim(), base)),
        ],
        None => vec![Line::raw(""), Line::from(Span::styled("No matches", dim_style))],
    };
    frame.render_widget(Paragraph::new(desc_lines).wrap(Wrap { trim: false }), desc_padded);

    row_rects
}

/// The longest label and Number-range text across `def`'s params, used as fixed column
/// widths so every row's value lands in the same place regardless of how long any other
/// row's label happens to be — a long param name used to run straight into its own range
/// text with no separating space (`"Grain Size Limit[2.0-200.0]"`) because the old fixed
/// 14-column guess didn't account for names longer than that.
fn cdp_params_column_widths(def: &crate::model::cdp::ProcessDef) -> (usize, usize) {
    use crate::model::cdp::ParamKind;
    let label_width = def.params.iter().map(|p| p.name.chars().count()).max().unwrap_or(0).max(9);
    let range_width = def
        .params
        .iter()
        .map(|p| match &p.kind {
            ParamKind::Number { min, max, .. } => format_cdp_range(*min, *max).chars().count(),
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    (label_width, range_width)
}

/// The parameter-editing form for one CDP process, opened from `Dialog::CdpBrowser`. Sized
/// to fit every field up to the terminal's height; past that, the field list scrolls to
/// keep the focused field visible (`visible_field_rows`/`scroll_top` below) while the
/// preset row, second-input row, buttons, and hints stay pinned. Column widths are computed
/// fresh from `def.params` (`cdp_params_column_widths`) rather than a fixed guess — see that
/// function's doc comment.
fn render_cdp_params_dialog(
    frame: &mut Frame,
    area: Rect,
    def: Option<&crate::model::cdp::ProcessDef>,
    fields: &[CdpField],
    second_input: Option<&CdpSecondInput>,
    focus: usize,
    error: &Option<String>,
    preview: &Option<CdpPreview>,
    presets: &[crate::model::cdp::preset::CdpPreset],
    preset_selected: Option<usize>,
    save_prompt: Option<&TextInput>,
    _scroll: usize,
) -> Vec<Rect> {
    let Some(def) = def else { return Vec::new() };
    let (label_width, range_width) = cdp_params_column_widths(def);

    let width = (14 + label_width + range_width + 24).clamp(50, 110) as u16;
    let width = width.min(area.width);
    let has_second_input = second_input.is_some() as usize;
    // header spacer + preset row + blank + [fields] + second-input? + blank + buttons +
    // error/blank + hints, + 2 border.
    let overhead = 1 + 1 + 1 + has_second_input + 1 + 1 + 1 + 1;
    let content_field_rows = fields.len().max(1);
    let ideal_height = overhead + content_field_rows + 2;
    let height = (ideal_height as u16).min(area.height);
    let visible_field_rows = (height as usize)
        .saturating_sub(2)
        .saturating_sub(overhead)
        .max(1)
        .min(content_field_rows);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);
    let automatable_label_style = Style::default().fg(theme::ACTIVE).bg(theme::SURFACE0);
    let dim_style = Style::default().fg(theme::BORDER).bg(theme::SURFACE0);
    let error_style = Style::default().fg(theme::RED).bg(theme::SURFACE0);
    let range_style = Style::default().fg(theme::BORDER).bg(theme::SURFACE0);
    let point_style = Style::default().fg(theme::FOCUS).bg(theme::SURFACE0);

    let block = Block::default()
        .title(def.title.as_str())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines = vec![Line::raw("")];

    // ---- Preset row ----
    let preset_focused = focus == CDP_PRESET_FOCUS;
    let preset_label = format!(" {:<label_width$}  ", "Preset");
    let preset_line = if let Some(input) = save_prompt {
        let (before, under, after) = input.split_at_cursor();
        Line::from(vec![
            Span::styled(" Save as: ", label_style),
            Span::styled(before, base),
            Span::styled(under, cursor_style),
            Span::styled(after, base),
        ])
    } else if presets.is_empty() {
        Line::from(vec![
            Span::styled(preset_label, if preset_focused { cursor_style } else { label_style }),
            Span::styled("(none saved)", dim_style),
            Span::styled("  s:save", hint_style),
        ])
    } else {
        let name = preset_selected.and_then(|i| presets.get(i)).map(|p| p.name.as_str()).unwrap_or("(custom)");
        let value = if preset_focused { format!("\u{25c4} {name} \u{25ba}") } else { name.to_string() };
        Line::from(vec![
            Span::styled(preset_label, if preset_focused { cursor_style } else { label_style }),
            Span::styled(value, base),
            Span::styled("  s:save  d:delete", hint_style),
        ])
    };
    lines.push(preset_line);
    lines.push(Line::raw(""));

    // ---- Field rows (scrolled window) ----
    let focus_field_row = (focus >= 1 && focus <= fields.len()).then(|| focus - 1);
    let scroll_top = match focus_field_row {
        Some(row) => row
            .saturating_sub(visible_field_rows.saturating_sub(1))
            .min(content_field_rows.saturating_sub(visible_field_rows)),
        None => 0,
    };
    for (i, (param, field)) in def.params.iter().zip(fields).enumerate().skip(scroll_top).take(visible_field_rows) {
        let is_focused = focus == i + 1;
        let label = format!(" {:<label_width$}  ", param.name);
        // Automatable params (the ones 'e' can open the envelope editor on) get a green
        // label so it's visible at a glance which params accept a `.brk` curve, without
        // having to focus each one to see the "(e:envelope)" hint.
        let label_style_here = if is_focused {
            cursor_style
        } else if param.automatable {
            automatable_label_style
        } else {
            label_style
        };
        let line = match field {
            CdpField::Number { envelope: Some(points), .. } => Line::from(vec![
                Span::styled(label, label_style_here),
                Span::styled(format!("{:<range_width$}", ""), range_style),
                Span::styled(format!(" envelope ({} pts, e to edit)", points.len()), point_style),
            ]),
            // A `required_envelope` param has no constant representation at all — showing
            // its (irrelevant, never-submitted) `input` text would read as "this number is
            // the value," which is actively misleading. `cdp_validate_fields` blocks
            // Apply/Preview until the user has actually opened the editor once.
            CdpField::Number { envelope: None, .. } if param.required_envelope => Line::from(vec![
                Span::styled(label, label_style_here),
                Span::styled(format!("{:<range_width$}", ""), range_style),
                Span::styled(" (not set — e to edit)", dim_style),
            ]),
            CdpField::Number { input, min, max, .. } => {
                let range = format!("{:<range_width$}", format_cdp_range(*min, *max));
                if is_focused {
                    let (before, under, after) = input.split_at_cursor();
                    let mut spans = vec![
                        Span::styled(label, label_style_here),
                        Span::styled(range, range_style),
                        Span::styled(" ", base),
                        Span::styled(before, base),
                        Span::styled(under, cursor_style),
                        Span::styled(after, base),
                    ];
                    if param.automatable {
                        spans.push(Span::styled("  (e:envelope)", dim_style));
                    }
                    Line::from(spans)
                } else {
                    Line::from(vec![
                        Span::styled(label, label_style_here),
                        Span::styled(range, range_style),
                        Span::styled(format!(" {}", input.value()), base),
                    ])
                }
            }
            CdpField::Toggle { on } => {
                let text = if *on { "[X]" } else { "[ ]" };
                Line::from(vec![
                    Span::styled(label, label_style_here),
                    Span::styled(format!("{:<range_width$}", ""), range_style),
                    Span::styled(format!(" {text}"), base),
                ])
            }
            CdpField::Choice { options, selected } => {
                let text = options.get(*selected).map(String::as_str).unwrap_or("");
                let value = if is_focused { format!(" \u{25c4} {text} \u{25ba}") } else { format!(" {text}") };
                Line::from(vec![
                    Span::styled(label, label_style_here),
                    Span::styled(format!("{:<range_width$}", ""), range_style),
                    Span::styled(value, base),
                ])
            }
            // A `required_list` param has no constant representation either — same
            // rationale as `required_envelope`'s "(not set)" display above, just for the
            // list editor (`App::open_cdp_list_editor`) instead of the envelope editor.
            CdpField::List { values, .. } if values.is_empty() => Line::from(vec![
                Span::styled(label, label_style_here),
                Span::styled(format!("{:<range_width$}", ""), range_style),
                Span::styled(" (not set — e to edit)", dim_style),
            ]),
            CdpField::List { values, .. } => Line::from(vec![
                Span::styled(label, label_style_here),
                Span::styled(format!("{:<range_width$}", ""), range_style),
                Span::styled(format!(" list ({} items, e to edit)", values.len()), point_style),
            ]),
        };
        lines.push(line);
    }
    if fields.is_empty() {
        lines.push(Line::from(Span::styled(" (no parameters)", dim_style)));
    }
    // Pad up to `visible_field_rows` so the trailing chrome (second-input/buttons/hints)
    // lands at the same row every frame regardless of the current scroll window's fill.
    let rendered_field_rows = fields.len().min(visible_field_rows).max(fields.is_empty() as usize);
    for _ in rendered_field_rows..visible_field_rows {
        lines.push(Line::raw(""));
    }

    if let Some(second) = second_input {
        let is_focused = focus == cdp_params_focus_second_input(fields.len());
        let label = format!(" {:<label_width$}  ", "2nd input");
        let name = second.selected_name();
        let value = if is_focused { format!(" \u{25c4} {name} \u{25ba}") } else { format!(" {name}") };
        lines.push(Line::from(vec![
            Span::styled(label, if is_focused { cursor_style } else { label_style }),
            Span::styled(format!("{:<range_width$}", ""), range_style),
            Span::styled(value, base),
        ]));
    }
    lines.push(Line::raw(""));

    let preview_focus = cdp_params_focus_preview(fields.len(), second_input.is_some());
    let apply_focus = cdp_params_focus_apply(fields.len(), second_input.is_some());
    let preview_label = if preview.is_some() { " [Preview \u{2713}]" } else { " [Preview]" };
    let preview_style = if focus == preview_focus { cursor_style } else { hint_style };
    let apply_style = if focus == apply_focus { cursor_style } else { hint_style };
    lines.push(Line::from(vec![
        Span::styled(preview_label, preview_style),
        Span::raw("  "),
        Span::styled("[Apply]", apply_style),
    ]));

    match error {
        Some(msg) => lines.push(Line::from(Span::styled(format!(" ! {msg}"), error_style))),
        None => lines.push(Line::raw("")),
    }

    lines.push(Line::from(vec![
        Span::styled(" \u{2191}\u{2193}", hint_style),
        Span::styled(":nudge  ", label_style),
        Span::styled("Tab", hint_style),
        Span::styled(":next  ", label_style),
        Span::styled("Enter", hint_style),
        Span::styled(":run  ", label_style),
        Span::styled("Esc", hint_style),
        Span::styled(":cancel", label_style),
    ]));

    frame.render_widget(Paragraph::new(lines), inner);
    Vec::new()
}

/// Hard-modal progress display for an in-flight CDP job. The spinner frame is derived from
/// `elapsed` (rather than a persisted counter) so this stays a pure function of the
/// dialog's state, matching every other renderer in this file.
fn render_cdp_running_dialog(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    step_label: &str,
    step_index: usize,
    step_total: usize,
    elapsed: std::time::Duration,
    purpose: crate::cdp::JobPurpose,
) -> Vec<Rect> {
    let width = 50u16.min(area.width);
    let height = 7u16.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);

    const SPINNER: [char; 4] = ['|', '/', '-', '\\'];
    let spinner = SPINNER[(elapsed.as_millis() / 120) as usize % SPINNER.len()];
    let progress = if step_total > 0 {
        format!(" {spinner} Step {}/{}: {step_label}", step_index + 1, step_total)
    } else {
        format!(" {spinner} {step_label}")
    };
    let verb = match purpose {
        crate::cdp::JobPurpose::Apply => "Running",
        crate::cdp::JobPurpose::Preview => "Previewing",
    };

    let lines = vec![
        Line::raw(""),
        Line::from(Span::styled(format!(" {verb} {title}\u{2026}"), base)),
        Line::raw(""),
        Line::from(Span::styled(progress, base)),
        Line::raw(""),
        Line::from(vec![Span::styled(" Esc", hint_style), Span::styled(":cancel", label_style)]),
    ];

    let block = Block::default()
        .title("CDP")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
    Vec::new()
}

/// Scrollable viewer for a failed CDP run's captured stdout+stderr.
fn render_cdp_output_dialog(frame: &mut Frame, area: Rect, title: &str, lines_text: &[String], scroll: usize) -> Vec<Rect> {
    let width = 70u16.min(area.width);
    let height = 20u16.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // A CDP error line easily runs well past this popup's width (e.g. "Parameter[3] Value
    // (...) out of range (...)") — word-wrapped via ratatui's own `Wrap`, with `.scroll`
    // driving it by *rendered* row rather than re-wrapping by hand. The last row is
    // reserved for the hints bar, which never scrolls with the content.
    let text_area = Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: inner.height.saturating_sub(1) };
    let hints_area = Rect { x: inner.x, y: inner.y + text_area.height, width: inner.width, height: 1 };

    let content: Vec<Line> = lines_text.iter().map(|l| Line::from(Span::styled(l.as_str(), base))).collect();
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: false }).scroll((scroll as u16, 0)),
        text_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" \u{2191}\u{2193}/PgUp/PgDn", hint_style),
            Span::styled(":scroll  ", label_style),
            Span::styled("Enter/Esc", hint_style),
            Span::styled(":close", label_style),
        ])),
        hints_area,
    );
    Vec::new()
}

fn render_info_dialog(frame: &mut Frame, area: Rect, message: &str) -> Vec<Rect> {
    let width = (message.chars().count() as u16 + 4).min(area.width).max(30);
    let height = 6u16.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);
    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let hint_style = Style::default().fg(theme::SHORTCUT).bg(theme::SURFACE0);
    let label_style = Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0);
    let close_line = Line::from(vec![
        Span::styled(" Enter", hint_style),
        Span::styled(":close", label_style),
    ]);
    let block = Block::default()
        .title("Info")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(base);
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::from(Span::styled(format!(" {message}"), base)),
            Line::raw(""),
            close_line,
        ]).block(block),
        popup,
    );
    let row_w = popup.width.saturating_sub(2);
    // hints bar is interactive (closes the dialog).
    vec![Rect { x: popup.x + 1, y: popup.y + popup.height - 2, width: row_w, height: 1 }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::delete::delete_command;

    /// Builds an `App` with deterministic settings (`Config::default()`), never touching
    /// the real `~/.config/tui-wave/config.toml` or risking a race against tests elsewhere
    /// that temporarily redirect `XDG_CONFIG_HOME`. Every test below must use this instead
    /// of `App::new` directly.
    fn new_app(document: Option<Document>, directory: Option<PathBuf>) -> App {
        App::new_with_config(document, directory, Config::default())
    }

    fn doc(val: f32, len: usize) -> Document {
        Document {
            channels: vec![vec![val; len]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        }
    }

    fn stereo_doc(left: f32, right: f32, len: usize) -> Document {
        Document {
            channels: vec![vec![left; len], vec![right; len]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        }
    }

    /// A regression test for a real bug: the menu used to render *before* the waveform
    /// content, so an open dropdown (which extends below the menu bar into the content
    /// area) got overdrawn by it — the dropdown's own text never survived to the screen.
    /// The menu must render last so it stays on top.
    #[test]
    fn open_menu_dropdown_survives_on_top_of_waveform_content() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = new_app(Some(doc(0.5, 10_000)), None);
        app.menu.open_first(); // "File" menu, whose first entry is "Save"
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        // The dropdown's first entry ("Save", the File menu's first item) renders inside
        // its bordered popup at (popup.x + 1, popup.y + 1) = (1, 2) — row 2 being the
        // *toolbar's* first row, which is exactly what would have overwritten it under the
        // old "menu renders before content" ordering. A loose "does 'Save' appear
        // anywhere on screen" check wouldn't catch that bug: the toolbar has its own Save
        // button with the same text regardless.
        let buffer = terminal.backend().buffer();
        let row: String = (1..6u16).map(|x| buffer[(x, 2)].symbol()).collect();
        assert_eq!(row, "Save ", "the dropdown's first entry must survive on top of the toolbar row beneath it");
    }

    /// Builds a mono doc with a quiet section followed by a sudden loud one — a clear
    /// transient — at 44100Hz, matching `Document`'s own transient test fixtures (441
    /// samples per 10ms analysis frame).
    fn doc_with_transient(quiet_frames: usize, loud_frames: usize) -> Document {
        const FRAME_LEN: usize = 441;
        let mut channel = vec![0.01f32; quiet_frames * FRAME_LEN];
        channel.extend(std::iter::repeat(0.5f32).take(loud_frames * FRAME_LEN));
        Document {
            channels: vec![channel],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        }
    }

    /// Next Rising Edge moves the cursor to right before the transient and scrolls it into
    /// view.
    #[test]
    fn next_rising_edge_moves_cursor_to_the_transient() {
        let mut app = new_app(Some(doc_with_transient(20, 30)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(app.documents[0].len_samples(), 80));

        app.handle_action(Action::NextRisingEdge);

        assert_eq!(app.documents[0].cursor, 20 * 441);
    }

    /// When zoomed in, jumping to a transient must center the viewport on it (not just
    /// nudge it into view at the screen's edge) so there's context on both sides of the
    /// new cursor position.
    #[test]
    fn next_rising_edge_centers_the_viewport_when_zoomed_in() {
        let mut app = new_app(Some(doc_with_transient(20, 30)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport {
            samples_per_column: 10.0, // zoomed in: span(80) = 800, far smaller than the file
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: 1_000_000.0,
            total_len: app.documents[0].len_samples(),
            auto_vertical_zoom: false,
        });

        app.handle_action(Action::NextRisingEdge);

        let edge = 20 * 441;
        assert_eq!(app.documents[0].cursor, edge);
        let viewport = app.viewport.as_ref().unwrap();
        let half_span = viewport.span(80) / 2;
        assert_eq!(viewport.scroll_offset + half_span, edge, "the edge should sit at the center column");
    }

    /// Previous Rising Edge searches backward and also centers the viewport.
    #[test]
    fn prev_rising_edge_moves_cursor_backward_and_centers_viewport() {
        let mut app = new_app(Some(doc_with_segments(&[(0.01, 20), (0.5, 20), (5.0, 20)])), None);
        app.content_width = 80;
        app.viewport = Some(Viewport {
            samples_per_column: 10.0,
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: 1_000_000.0,
            total_len: app.documents[0].len_samples(),
            auto_vertical_zoom: false,
        });
        app.documents[0].cursor = 45 * 441; // inside the loudest segment

        app.handle_action(Action::PrevRisingEdge);

        let edge = 40 * 441; // the closer of the two earlier transients
        assert_eq!(app.documents[0].cursor, edge);
        let viewport = app.viewport.as_ref().unwrap();
        let half_span = viewport.span(80) / 2;
        assert_eq!(viewport.scroll_offset + half_span, edge);
    }

    /// With no transient before the cursor, Previous Rising Edge leaves it untouched.
    #[test]
    fn prev_rising_edge_does_nothing_when_none_found() {
        let mut app = new_app(Some(doc_with_segments(&[(0.3, 50)])), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(app.documents[0].len_samples(), 80));
        app.documents[0].cursor = 100;

        app.handle_action(Action::PrevRisingEdge);

        assert_eq!(app.documents[0].cursor, 100);
    }

    /// With no transient ahead of the cursor, Next Rising Edge leaves the cursor untouched.
    #[test]
    fn next_rising_edge_does_nothing_when_none_found() {
        let mut app = new_app(Some(doc_with_transient(0, 30)), None); // constant level throughout
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(app.documents[0].len_samples(), 80));
        app.documents[0].cursor = 100;

        app.handle_action(Action::NextRisingEdge);

        assert_eq!(app.documents[0].cursor, 100);
    }

    /// Builds a mono doc with several constant-level segments (each `frames` analysis
    /// frames of 441 samples at 44100Hz), for tests with more than one transient.
    fn doc_with_segments(segments: &[(f32, usize)]) -> Document {
        const FRAME_LEN: usize = 441;
        let channel: Vec<f32> =
            segments.iter().flat_map(|&(level, frames)| std::iter::repeat(level).take(frames * FRAME_LEN)).collect();
        Document {
            channels: vec![channel],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        }
    }

    /// Auto-Insert Markers adds one marker right before each detected transient, all as a
    /// single undo step (one `Undo` removes the whole batch, not just the last one).
    #[test]
    fn auto_insert_markers_adds_one_per_transient_as_a_single_undo_step() {
        let mut app = new_app(Some(doc_with_segments(&[(0.01, 20), (0.5, 20), (5.0, 20)])), None);

        app.handle_action(Action::AutoInsertMarkers);

        let positions: Vec<usize> = app.documents[0].markers.iter().map(|m| m.position).collect();
        assert_eq!(positions, vec![20 * 441, 40 * 441]);

        app.handle_action(Action::Undo);
        assert!(app.documents[0].markers.is_empty(), "one undo should remove the whole batch");
    }

    /// A transient that already has a marker on it must not get a second, duplicate one.
    #[test]
    fn auto_insert_markers_skips_positions_already_marked() {
        let mut app = new_app(Some(doc_with_segments(&[(0.01, 20), (0.5, 20), (5.0, 20)])), None);
        app.documents[0].markers = vec![Marker { position: 20 * 441, label: "Already here".to_string() }];

        app.handle_action(Action::AutoInsertMarkers);

        let positions: Vec<usize> = app.documents[0].markers.iter().map(|m| m.position).collect();
        assert_eq!(positions, vec![20 * 441, 40 * 441]);
        assert_eq!(app.documents[0].markers[0].label, "Already here", "the existing marker must be untouched");
    }

    /// With no transients in the file, Auto-Insert Markers does nothing (and records no
    /// undo step).
    #[test]
    fn auto_insert_markers_does_nothing_when_none_found() {
        let mut app = new_app(Some(doc_with_segments(&[(0.3, 50)])), None);

        app.handle_action(Action::AutoInsertMarkers);

        assert!(app.documents[0].markers.is_empty());
        assert!(!app.histories[0].undo(&mut app.documents[0]), "no history entry should have been recorded");
    }

    /// Technical Fades applies a fixed 5ms exp fade in/out to the whole file in one
    /// undoable step, regardless of any active selection.
    #[test]
    fn technical_fades_applies_5ms_fades_to_the_whole_file() {
        let mut d = doc(1.0, 44100); // 1 second at 44100Hz
        d.selection = Some(Selection { start: 1000, end: 2000 });
        let mut app = new_app(Some(d), None);

        app.handle_action(Action::TechnicalFades);

        let expected_fade_len = (44100.0 * 0.005f64).round() as usize; // 5ms
        assert!((app.documents[0].channels[0][0]).abs() < 0.01, "should fade in from silence");
        assert!(
            (app.documents[0].channels[0][expected_fade_len - 1] - 1.0).abs() < 0.01,
            "head fade should reach full volume by its end"
        );
        assert!((app.documents[0].channels[0][22050] - 1.0).abs() < 0.001, "the middle must be untouched");
        assert!((*app.documents[0].channels[0].last().unwrap()).abs() < 0.01, "should fade out to silence");
        assert_eq!(app.documents[0].selection, None, "should clear the selection, not act on it");

        app.handle_action(Action::Undo);
        assert!((app.documents[0].channels[0][0] - 1.0).abs() < 0.001, "undo should restore the original head");
    }

    /// `+`/`-` adjust the transient threshold within the clamp range and persist it.
    #[test]
    fn transient_threshold_adjusts_and_clamps() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        assert_eq!(app.transient_threshold_db, 13.0);

        app.handle_action(Action::IncreaseTransientThreshold);
        assert_eq!(app.transient_threshold_db, 14.0);
        assert_eq!(app.config.transient_threshold_db, 14.0, "should persist immediately");

        app.handle_action(Action::DecreaseTransientThreshold);
        app.handle_action(Action::DecreaseTransientThreshold);
        assert_eq!(app.transient_threshold_db, 12.0);

        for _ in 0..40 {
            app.handle_action(Action::DecreaseTransientThreshold);
        }
        assert_eq!(app.transient_threshold_db, TRANSIENT_THRESHOLD_MIN_DB);

        for _ in 0..40 {
            app.handle_action(Action::IncreaseTransientThreshold);
        }
        assert_eq!(app.transient_threshold_db, TRANSIENT_THRESHOLD_MAX_DB);
    }

    /// Inserting a marker is undoable, like any other document mutation.
    #[test]
    fn insert_marker_is_undoable() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].cursor = 100;
        app.handle_action(Action::InsertMarker);
        assert_eq!(app.documents[0].markers.len(), 1);
        assert_eq!(app.documents[0].markers[0].position, 100);

        app.handle_action(Action::Undo);
        assert!(app.documents[0].markers.is_empty());

        app.handle_action(Action::Redo);
        assert_eq!(app.documents[0].markers.len(), 1);
    }

    /// Deleting a marker is undoable — the removed marker (position and label) comes back
    /// exactly as it was.
    #[test]
    fn delete_marker_is_undoable() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].markers = vec![Marker { position: 200, label: "Verse".to_string() }];
        app.documents[0].cursor = 200;

        app.handle_action(Action::DeleteMarker);
        assert!(app.documents[0].markers.is_empty());

        app.handle_action(Action::Undo);
        assert_eq!(app.documents[0].markers, vec![Marker { position: 200, label: "Verse".to_string() }]);
    }

    /// Reset Config must ask first rather than wiping keybindings on the spot. (Only the
    /// open + cancel path is exercised here; confirming would write to the real config file.)
    #[test]
    fn reset_config_action_confirms_before_resetting() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.handle_action(Action::ResetConfig);
        assert!(
            matches!(app.confirm, Some(Confirm::ResetConfig)),
            "Reset Config should open a confirmation, not reset immediately"
        );
        // Esc cancels without resetting.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.confirm.is_none());
    }

    /// `Action::CdpProcess` (Ctrl+p / Process menu) opens the setup prompt when
    /// `config.cdp_dir` is unset, rather than the browser — there's nothing to browse until
    /// a valid directory is configured.
    #[test]
    fn cdp_process_action_opens_setup_when_dir_unset() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        assert_eq!(app.config.cdp_dir, "");
        app.handle_action(Action::CdpProcess);
        assert!(
            matches!(app.dialog, Some(Dialog::CdpSetup { .. })),
            "expected CdpSetup when cdp_dir is unset, got {:?}",
            std::mem::discriminant(app.dialog.as_ref().unwrap())
        );
    }

    /// With a valid `cdp_dir` already configured, `Action::CdpProcess` skips straight to the
    /// browser instead of re-prompting for a path the user already set correctly.
    #[test]
    fn cdp_process_action_opens_browser_when_dir_valid() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        let cdp_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cdp");
        if crate::cdp::validate_cdp_dir(&cdp_dir).is_err() {
            eprintln!("skipping: no real CDP install found in this checkout");
            return;
        }
        app.config.cdp_dir = cdp_dir.to_string_lossy().to_string();
        app.handle_action(Action::CdpProcess);
        assert!(
            matches!(app.dialog, Some(Dialog::CdpBrowser { .. })),
            "expected CdpBrowser when cdp_dir is already valid"
        );
    }

    /// `Action::ConfigureCdpDirectory` (Options menu) always opens the setup prompt,
    /// prefilled with the current value, even when that value is already valid — unlike
    /// `Action::CdpProcess`'s validate-first shortcut, this is an explicit "let me look at
    /// or change this setting" entry point and must never silently skip past it.
    #[test]
    fn configure_cdp_directory_action_always_opens_setup_prefilled() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        let cdp_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cdp");
        if crate::cdp::validate_cdp_dir(&cdp_dir).is_err() {
            eprintln!("skipping: no real CDP install found in this checkout");
            return;
        }
        let cdp_dir_str = cdp_dir.to_string_lossy().to_string();
        app.config.cdp_dir = cdp_dir_str.clone();

        app.handle_action(Action::ConfigureCdpDirectory);
        match &app.dialog {
            Some(Dialog::CdpSetup { input, error }) => {
                assert_eq!(input.value(), cdp_dir_str);
                assert!(error.is_none());
            }
            _ => panic!("expected CdpSetup prefilled with the current path"),
        }
    }

    /// `open_cdp_browser` builds a plain, list-only dialog — no fields, no per-process
    /// state to react to (that's `Dialog::CdpParams`'s job now).
    #[test]
    fn open_cdp_browser_builds_a_plain_entry_list() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        match &app.dialog {
            Some(Dialog::CdpBrowser { entries, selected, .. }) => {
                assert!(!entries.is_empty(), "an empty search should match every catalog entry");
                assert_eq!(*selected, 0);
            }
            _ => panic!("expected Dialog::CdpBrowser"),
        }
    }

    /// Down/Up move `selected` (clamped to the filtered entry list); PageDown/PageUp move it
    /// by a full page (`CDP_BROWSER_PAGE_SIZE`) — both stay within `Dialog::CdpBrowser`,
    /// never touching a `Dialog::CdpParams` that doesn't exist yet.
    #[test]
    fn cdp_browser_arrow_and_page_keys_move_selection() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        let entry_count = match &app.dialog {
            Some(Dialog::CdpBrowser { entries, .. }) => entries.len(),
            _ => panic!("no dialog"),
        };
        assert!(entry_count > CDP_BROWSER_PAGE_SIZE, "need enough entries for a page move to mean anything");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { selected, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*selected, 1);

        app.handle_dialog_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { selected, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*selected, 1 + CDP_BROWSER_PAGE_SIZE);

        app.handle_dialog_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { selected, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*selected, 1);
    }

    /// Enter on the browser opens `Dialog::CdpParams` for the currently-selected process —
    /// the two-dialog flow's whole point: browsing never grows/shrinks a params form live,
    /// it commits to a completely separate dialog sized for that one process.
    #[test]
    fn enter_on_browser_opens_params_for_the_selected_process() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        let (catalog_index, expected_params) = match &app.dialog {
            Some(Dialog::CdpBrowser { entries, selected, .. }) => {
                let ci = entries[*selected];
                (ci, app.cdp_catalog.processes[ci].params.len())
            }
            _ => panic!("no dialog"),
        };

        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match &app.dialog {
            Some(Dialog::CdpParams { catalog_index: ci, fields, focus, presets, preset_selected, .. }) => {
                assert_eq!(*ci, catalog_index);
                assert_eq!(fields.len(), expected_params);
                assert_eq!(*focus, CDP_PRESET_FOCUS);
                assert!(presets.is_empty(), "a fresh process (never had presets saved) should show none");
                assert!(preset_selected.is_none());
            }
            _ => panic!("expected Dialog::CdpParams after Enter"),
        }
    }

    /// A mouse click on a process row in the browser selects *and* opens it in one step —
    /// clicking is how a mouse user commits a choice, matching Enter's behavior on the
    /// already-selected entry (see `App::handle_dialog_row_click`'s `Dialog::CdpBrowser` arm).
    #[test]
    fn clicking_a_browser_row_selects_and_opens_it() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        // Row 1 (0-based) in the rendered list is the second filtered entry.
        let catalog_index = match &app.dialog {
            Some(Dialog::CdpBrowser { entries, .. }) => entries[1],
            _ => panic!("no dialog"),
        };
        // x_in_row must land past the Groups column, or the click is read as a group pick
        // instead (see `App::handle_dialog_row_click`'s `Dialog::CdpBrowser` arm).
        app.handle_dialog_row_click(1, CDP_GROUP_COL_WIDTH);

        match &app.dialog {
            Some(Dialog::CdpParams { catalog_index: ci, .. }) => assert_eq!(*ci, catalog_index),
            _ => panic!("expected a click to open Dialog::CdpParams"),
        }
    }

    /// `cdp_params_column_widths` sizes both columns to the *longest* label/range in the
    /// process's own params, not a fixed guess — the fix for labels colliding with their own
    /// range text when a name ran past a fixed-width guess (e.g. "Grain Size Limit").
    #[test]
    fn cdp_params_column_widths_fit_the_longest_label_and_range() {
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog.find("blur_avrg").expect("blur_avrg in catalog");
        let (label_width, range_width) = cdp_params_column_widths(def);
        assert!(label_width >= "Channels".chars().count());
        assert!(range_width >= format_cdp_range(1.0, 200.0).chars().count());
    }

    /// Opens `Dialog::CdpParams` directly for `blur_avrg` ("Average"), focused on its one
    /// param, "Channels" — automatable, so the envelope editor can open on it. Shared setup
    /// for the envelope/preset tests below.
    fn open_blur_avrg_with_field_focused(app: &mut App) {
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let catalog_index = app
            .cdp_catalog
            .processes
            .iter()
            .position(|p| p.key == "blur_avrg")
            .expect("blur_avrg should be in the catalog");
        let _ = catalog;
        app.open_cdp_params(catalog_index);
        if let Some(Dialog::CdpParams { focus, .. }) = app.dialog.as_mut() {
            *focus = 1; // the one field, "Channels"
        }
    }

    /// Tab from the preset row moves focus into the first field (or straight to
    /// Preview/Apply for a process with none); a further Tab past the last control wraps
    /// back around to the preset row, closing the cycle.
    #[test]
    fn tab_cycles_from_preset_row_into_fields_and_back() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        let catalog_index = app
            .cdp_catalog
            .processes
            .iter()
            .position(|p| p.key == "blur_avrg")
            .expect("blur_avrg should be in the catalog");
        app.open_cdp_params(catalog_index);
        let Some(Dialog::CdpParams { focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*focus, CDP_PRESET_FOCUS);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Dialog::CdpParams { focus, fields, second_input, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*focus, 1, "Tab from the preset row should land on the first control");
        let apply_focus = cdp_params_focus_apply(fields.len(), second_input.is_some());

        // Already at focus 1 (the first control); apply_focus - 1 more tabs lands on Apply.
        for _ in 0..apply_focus.saturating_sub(1) {
            app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        let Some(Dialog::CdpParams { focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*focus, apply_focus, "should now be sitting on Apply");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Dialog::CdpParams { focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*focus, CDP_PRESET_FOCUS, "Tab past Apply should wrap back to the preset row");
    }

    /// 'e' on an automatable Number field opens the envelope editor seeded with two flat
    /// points at the field's current constant value — the curve should start exactly where
    /// the plain numeric value left off, not jump to some arbitrary default.
    #[test]
    fn e_opens_envelope_editor_seeded_from_the_current_value() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_blur_avrg_with_field_focused(&mut app);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        let Some(Dialog::CdpParams { envelope: Some(edit), fields, .. }) = &app.dialog else {
            panic!("expected the envelope editor to be open");
        };
        let Some(CdpField::Number { input, .. }) = fields.get(edit.field_index) else {
            panic!("expected a Number field");
        };
        let current: f64 = input.value().trim().parse().unwrap();
        assert_eq!(edit.points.len(), 2);
        assert_eq!(edit.points[0].1, current);
        assert_eq!(edit.points[1].1, current);
        assert_eq!(edit.points[0].0, 0.0);
    }

    /// Esc discards every edit made in the session and leaves the field a plain constant —
    /// opening the editor and immediately backing out must be a true no-op, not a silent
    /// "envelope with 2 identical points" left behind.
    #[test]
    fn esc_discards_envelope_edits_and_restores_constant_mode() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_blur_avrg_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)); // insert a point
        app.handle_dialog_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)); // nudge it

        app.handle_dialog_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        let Some(Dialog::CdpParams { envelope, fields, focus, .. }) = &app.dialog else {
            panic!("expected CdpParams to still be open (Esc closes the editor, not the dialog)");
        };
        assert!(envelope.is_none(), "editor should be closed");
        assert_eq!(*focus, 1, "should be back on the Channels field");
        let Some(CdpField::Number { envelope, .. }) = fields.first() else { panic!("expected a Number field") };
        assert!(envelope.is_none(), "field must stay a plain constant after Esc");
    }

    /// Enter commits the edited points into the field, switching it to envelope mode; a
    /// subsequent `to_value()` must produce `ParamValue::Breakpoints`, not `Number` — this
    /// is the whole reason the editor exists, so it's worth pinning past the dialog layer
    /// down to what `cdp_run` will actually hand the pipeline.
    #[test]
    fn enter_commits_envelope_and_to_value_returns_breakpoints() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_blur_avrg_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Some(Dialog::CdpParams { envelope, fields, .. }) = &app.dialog else {
            panic!("expected CdpParams still open");
        };
        assert!(envelope.is_none(), "editor should be closed after commit");
        let field = fields.first().expect("Channels field");
        match field.to_value() {
            crate::model::cdp::ParamValue::Breakpoints(points) => assert_eq!(points.len(), 3),
            _ => panic!("expected Breakpoints after committing an envelope"),
        }
    }

    /// Opens `Dialog::CdpParams` for `focus_hold`, focused on its one `required_envelope`
    /// field — the shared setup for the tests below (CDP-Ext-Plan.md Phase 3/"Tier 1b").
    fn open_focus_hold_with_field_focused(app: &mut App) {
        let catalog_index = app
            .cdp_catalog
            .processes
            .iter()
            .position(|p| p.key == "focus_hold")
            .expect("focus_hold should be in the catalog");
        app.open_cdp_params(catalog_index);
        if let Some(Dialog::CdpParams { focus, .. }) = app.dialog.as_mut() {
            *focus = 1;
        }
    }

    /// A fresh `required_envelope` field starts with no envelope at all (not a constant, not
    /// a pre-seeded breakpoint list) — the field is genuinely "not configured yet" until the
    /// user opens the editor once, matching the render-side "(not set — e to edit)" display.
    #[test]
    fn required_envelope_field_starts_unset() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_focus_hold_with_field_focused(&mut app);
        let Some(Dialog::CdpParams { fields, .. }) = &app.dialog else { panic!("no dialog") };
        let Some(CdpField::Number { envelope, .. }) = fields.first() else { panic!("expected a Number field") };
        assert!(envelope.is_none());
    }

    /// 'e' on a never-opened `required_envelope` field seeds 3 points with a bend in the
    /// middle, not a straight 2-point line — regression test for a real hang: at least one
    /// CDP process (`fractal wave`/`spectrum`'s Shape) never returns when handed a straight
    /// 2-point breakpoint file, regardless of whether its two points' values are equal or
    /// different (confirmed against the real binary), while a 3-point line with even a
    /// barely-perceptible bend completes in milliseconds. See
    /// `App::open_cdp_envelope_editor`'s doc comment for the full finding.
    #[test]
    fn opening_a_required_envelope_field_seeds_three_non_collinear_points() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_focus_hold_with_field_focused(&mut app);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else {
            panic!("expected the envelope editor to be open");
        };
        assert_eq!(edit.points.len(), 3, "must not be a straight 2-point line — see this test's doc comment");
        let (t0, v0) = edit.points[0];
        let (t1, v1) = edit.points[1];
        let (t2, v2) = edit.points[2];
        assert!(t0 < t1 && t1 < t2, "the middle point must sit strictly between the other two in time");
        assert_ne!(v1, v0, "the middle point must not be collinear with a flat start/end");
        assert_eq!(v0, v2, "start and end return to the same value — a small, unobtrusive default bump");
    }

    /// Apply/Preview must be blocked with a validation error while a `required_envelope`
    /// field is still unset — its `input` text is never the submitted value (`to_value`
    /// would otherwise silently emit a `ParamValue::Number` that CDP rejects as an
    /// unreadable file path), so `cdp_validate_fields` has to check `envelope.is_some()`
    /// directly rather than the field's numeric range.
    #[test]
    fn apply_is_blocked_until_a_required_envelope_field_is_set() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_focus_hold_with_field_focused(&mut app);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Some(Dialog::CdpParams { error, focus, .. }) = &app.dialog else {
            panic!("expected CdpParams still open after a blocked Apply");
        };
        assert!(error.is_some(), "an unset required_envelope field should block Apply with an error");
        assert_eq!(*focus, 1, "should focus the offending field");
    }

    /// Once the user has opened the editor and committed points (Enter inside the editor),
    /// the field is a valid `Breakpoints` value and Apply is no longer blocked by it.
    #[test]
    fn setting_a_required_envelope_field_unblocks_apply() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_focus_hold_with_field_focused(&mut app);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert!(
            matches!(&app.dialog, Some(Dialog::CdpParams { envelope: Some(_), .. })),
            "'e' should open the envelope editor for an automatable field regardless of required_envelope"
        );
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // commit

        let Some(Dialog::CdpParams { fields, envelope, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(envelope.is_none(), "editor should be closed after commit");
        let Some(CdpField::Number { envelope: field_env, .. }) = fields.first() else {
            panic!("expected a Number field")
        };
        assert!(field_env.is_some(), "committing the editor should set the field's envelope");

        let def = app.cdp_catalog.processes.iter().find(|p| p.key == "focus_hold").unwrap();
        assert!(
            App::cdp_validate_fields(def, fields).is_none(),
            "a set required_envelope field should no longer block validation"
        );
    }

    /// 'c' ("commit as constant") inside the envelope editor is a no-op for a
    /// `required_envelope` field — there's no valid constant to revert to, so the field must
    /// stay in envelope mode with its points untouched rather than being cleared to `None`.
    #[test]
    fn c_key_is_a_no_op_for_a_required_envelope_field() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_focus_hold_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));

        assert!(
            matches!(&app.dialog, Some(Dialog::CdpParams { envelope: Some(_), .. })),
            "'c' must not close the editor for a required_envelope field"
        );
    }

    /// Opens `Dialog::CdpParams` for `grain_reposition`, focused on its one
    /// `required_list` field — the shared setup for the `CdpField::List` tests below
    /// (CDP-Ext-Plan.md Phase 3's plain-list shape).
    fn open_grain_reposition_with_field_focused(app: &mut App) {
        let catalog_index = app
            .cdp_catalog
            .processes
            .iter()
            .position(|p| p.key == "grain_reposition")
            .expect("grain_reposition should be in the catalog");
        app.open_cdp_params(catalog_index);
        if let Some(Dialog::CdpParams { focus, .. }) = app.dialog.as_mut() {
            *focus = 1;
        }
    }

    /// A fresh `required_list` field starts with an empty list (not a constant, not a
    /// pre-seeded entry) — genuinely "not configured yet" until the user opens the editor
    /// once, matching the render-side "(not set — e to edit)" display.
    #[test]
    fn required_list_field_starts_unset() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_grain_reposition_with_field_focused(&mut app);
        let Some(Dialog::CdpParams { fields, .. }) = &app.dialog else { panic!("no dialog") };
        let Some(CdpField::List { values, .. }) = fields.first() else { panic!("expected a List field") };
        assert!(values.is_empty());
    }

    /// Apply/Preview must be blocked with a validation error while a `required_list` field
    /// is still unset (empty) — mirrors `apply_is_blocked_until_a_required_envelope_field_is_set`.
    #[test]
    fn apply_is_blocked_until_a_required_list_field_is_set() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_grain_reposition_with_field_focused(&mut app);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Some(Dialog::CdpParams { error, focus, .. }) = &app.dialog else {
            panic!("expected CdpParams still open after a blocked Apply");
        };
        assert!(error.is_some(), "an unset required_list field should block Apply with an error");
        assert_eq!(*focus, 1, "should focus the offending field");
    }

    /// 'e' on a never-opened `required_list` field seeds exactly one entry at the param's
    /// default value — unlike `required_envelope`'s 3-point seeding, a plain list has no
    /// known pathological-on-N-entries CDP behavior to sidestep, so there's no reason to
    /// seed more than one.
    #[test]
    fn opening_a_required_list_field_seeds_one_entry_at_default() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_grain_reposition_with_field_focused(&mut app);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        let Some(Dialog::CdpParams { list_edit: Some(edit), .. }) = &app.dialog else {
            panic!("expected the list editor to be open");
        };
        assert_eq!(edit.values.len(), 1);
    }

    /// Regression test for a real bug found by manual testing: submitting `grain_reposition`
    /// with out-of-order onset times ("2700.0, 1800.0") crashed with CDP's own "Sync times
    /// out of sequence" error. `grain_reposition`'s "Grain Onset Times" is a time-sequence
    /// list (`ParamDef.list_is_time_sequence`) — Up must never nudge an entry past its next
    /// neighbor, no matter how many times it's pressed.
    #[test]
    fn time_sequence_up_nudge_cannot_cross_the_next_entry() {
        let mut app = new_app(Some(doc(1.0, 44100)), None);
        open_grain_reposition_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)); // now 2 entries
        let Some(Dialog::CdpParams { list_edit: Some(edit), .. }) = &app.dialog else { panic!("no editor") };
        assert_eq!(edit.values.len(), 2);
        assert_eq!(edit.selected, 1, "'n' should select the newly inserted (later) entry");

        // Select the first (earlier) entry and hammer Up on it — it must approach but never
        // reach or cross the second entry's value.
        app.handle_dialog_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        for _ in 0..200 {
            app.handle_dialog_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }

        let Some(Dialog::CdpParams { list_edit: Some(edit), .. }) = &app.dialog else { panic!("no editor") };
        assert!(
            edit.values[0] < edit.values[1],
            "first entry ({}) must stay strictly before the second ({})",
            edit.values[0],
            edit.values[1]
        );

        // Committing and validating must therefore never trip the ascending-order check.
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Dialog::CdpParams { fields, .. }) = &app.dialog else { panic!("no dialog") };
        let def = app.cdp_catalog.processes.iter().find(|p| p.key == "grain_reposition").unwrap();
        assert!(App::cdp_validate_fields(def, fields).is_none());
    }

    /// Regression test for the other half of the same manual-testing bug report: the coarse
    /// Up/Down nudge step for a time-sequence field must scale off the real selection's
    /// duration, not the catalog's own generous safety-cap `max` (`grain_reposition`'s
    /// "Grain Onset Times" allows up to 7200s so long *files* aren't artificially capped —
    /// but a coarse step sized off that cap alone jumps by ~180s per press, useless and
    /// immediately out-of-range-feeling against a short real selection).
    #[test]
    fn time_sequence_coarse_nudge_scales_with_selection_duration_not_catalog_max() {
        let mut app = new_app(Some(doc(2.0, 44100)), None); // a short, 2-second selection
        open_grain_reposition_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        app.handle_dialog_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        let Some(Dialog::CdpParams { list_edit: Some(edit), .. }) = &app.dialog else { panic!("no editor") };
        assert!(
            edit.values[0] < 1.0,
            "one coarse Up on a 2s selection should move well under a second, not the \
             catalog max's ~180s-per-press (got {})",
            edit.values[0]
        );
    }

    /// 'n' inserts a duplicate of the selected entry right after it; Del removes the
    /// selected entry, kept at a minimum of 1 (unlike the envelope editor's 2-point
    /// minimum — a single-entry list is meaningful here).
    #[test]
    fn list_editor_insert_and_delete_entries() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_grain_reposition_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        let Some(Dialog::CdpParams { list_edit: Some(edit), .. }) = &app.dialog else { panic!("no editor") };
        assert_eq!(edit.values.len(), 2, "'n' should insert a duplicate entry");
        assert_eq!(edit.selected, 1, "'n' should select the newly inserted entry");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        let Some(Dialog::CdpParams { list_edit: Some(edit), .. }) = &app.dialog else { panic!("no editor") };
        assert_eq!(edit.values.len(), 1, "Del should remove the selected entry");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        let Some(Dialog::CdpParams { list_edit: Some(edit), .. }) = &app.dialog else { panic!("no editor") };
        assert_eq!(edit.values.len(), 1, "Del must not remove the last remaining entry");
    }

    /// Esc discards every edit made in the session, restoring the field to an empty list
    /// (its state before the editor was ever opened) — mirrors
    /// `esc_discards_envelope_edits_and_restores_constant_mode`.
    #[test]
    fn esc_discards_list_edits() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_grain_reposition_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        app.handle_dialog_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        app.handle_dialog_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        let Some(Dialog::CdpParams { list_edit, fields, .. }) = &app.dialog else {
            panic!("expected CdpParams to still be open (Esc closes the editor, not the dialog)");
        };
        assert!(list_edit.is_none(), "editor should be closed");
        let Some(CdpField::List { values, .. }) = fields.first() else { panic!("expected a List field") };
        assert!(values.is_empty(), "field must revert to its pre-edit (unset) state after Esc");
    }

    /// Once the user has opened the editor and committed at least one entry (Enter inside
    /// the editor), the field is a valid `List` value and Apply is no longer blocked by it.
    #[test]
    fn setting_a_required_list_field_unblocks_apply() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_grain_reposition_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // commit

        let Some(Dialog::CdpParams { fields, list_edit, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(list_edit.is_none(), "editor should be closed after commit");
        let Some(CdpField::List { values, .. }) = fields.first() else { panic!("expected a List field") };
        assert!(!values.is_empty(), "committing the editor should set the field's list");

        let def = app.cdp_catalog.processes.iter().find(|p| p.key == "grain_reposition").unwrap();
        assert!(
            App::cdp_validate_fields(def, fields).is_none(),
            "a set required_list field should no longer block validation"
        );
    }

    /// `interp_cdp_envelope` is exactly CDP's own breakpoint semantics: piecewise-linear
    /// between points, clamped flat outside their time range.
    #[test]
    fn interp_cdp_envelope_matches_piecewise_linear_semantics() {
        let points = vec![(0.0, 0.0), (1.0, 10.0), (2.0, 0.0)];
        assert_eq!(interp_cdp_envelope(&points, -1.0), 0.0, "before the first point clamps flat");
        assert_eq!(interp_cdp_envelope(&points, 0.0), 0.0);
        assert_eq!(interp_cdp_envelope(&points, 0.5), 5.0, "linear midpoint of the first segment");
        assert_eq!(interp_cdp_envelope(&points, 1.0), 10.0);
        assert_eq!(interp_cdp_envelope(&points, 1.5), 5.0, "linear midpoint of the second segment");
        assert_eq!(interp_cdp_envelope(&points, 3.0), 0.0, "past the last point clamps flat");
    }

    /// `cdp_envelope_waveform_ref` (the graphics-mode reference-waveform data source) must
    /// reflect where the actual audio is quiet vs. loud, not just return a flat/empty Vec —
    /// otherwise the whole point of the feature (showing the user where the envelope will
    /// actually apply) silently does nothing.
    #[test]
    fn cdp_envelope_waveform_ref_reflects_quiet_and_loud_regions() {
        let mut channel = vec![0.01f32; 1000];
        for s in channel.iter_mut().skip(500) {
            *s = 0.9;
        }
        let document = Document {
            channels: vec![channel],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        };
        let app = new_app(Some(document), None);
        let peaks = app.cdp_envelope_waveform_ref((0, 1000), 10);
        assert_eq!(peaks.len(), 10);
        for (i, &p) in peaks.iter().enumerate().take(4) {
            assert!(p < 0.1, "cell {i} in the quiet half should have a low peak, got {p}");
        }
        for (i, &p) in peaks.iter().enumerate().skip(6) {
            assert!(p > 0.5, "cell {i} in the loud half should have a high peak, got {p}");
        }
    }

    /// Opens the envelope editor for `blur_avrg`'s "Channels" field, inserts a second point
    /// so there are two well-separated breakpoints to click on, and renders once (a real
    /// `TestBackend` terminal, not a mocked one) so `dialog_row_rects` holds the actual grid
    /// `Rect` the mouse tests below click into — exactly what `App::render` would have
    /// produced for a real session. Returns the app plus the two points' current screen
    /// cells, computed via the same forward mapping the renderer/mouse-handler both use.
    fn open_cdp_envelope_editor_for_mouse_tests() -> (App, Rect, (u16, u16), (u16, u16)) {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = new_app(Some(doc(0.1, 44100)), None);
        open_blur_avrg_with_field_focused(&mut app);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)); // 2nd point, midpoint

        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let grid = *app.dialog_row_rects.first().expect("envelope editor should return its grid rect");

        let Some(Dialog::CdpParams { envelope: Some(edit), fields, .. }) = &app.dialog else {
            panic!("expected the envelope editor to be open");
        };
        let Some(CdpField::Number { min, max, .. }) = fields.get(edit.field_index) else {
            panic!("expected a Number field");
        };
        let cell0 = cdp_envelope_point_cell(grid, edit.time_max, *min, *max, edit.points[0]);
        let cell1 = cdp_envelope_point_cell(grid, edit.time_max, *min, *max, edit.points[1]);
        (app, grid, cell0, cell1)
    }

    fn cdp_mouse_at(col: u16, row: u16, kind: MouseEventKind, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent { kind, column: col, row, modifiers }
    }

    /// A plain click near an existing (but not exactly on) breakpoint selects the *nearest*
    /// one, not just an exact hit — clicking is imprecise on a character grid, so "nearest"
    /// is what makes the feature usable at all.
    #[test]
    fn click_near_a_point_selects_it() {
        // The helper's setup ('n' to insert a 2nd point) leaves `selected == 1`, so clicking
        // point 0 is what actually proves the click changed the selection.
        let (mut app, _grid, cell0, _cell1) = open_cdp_envelope_editor_for_mouse_tests();
        app.handle_mouse(cdp_mouse_at(cell0.0, cell0.1, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE));
        app.handle_mouse(cdp_mouse_at(cell0.0, cell0.1, MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE));
        let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(edit.selected, 0, "click near the first point should select it");
    }

    /// Double-click on empty grid space (away from both existing points) inserts a new
    /// breakpoint there and selects it.
    #[test]
    fn double_click_inserts_a_new_point() {
        let (mut app, grid, cell0, cell1) = open_cdp_envelope_editor_for_mouse_tests();
        // A spot roughly a quarter of the way across the grid, clear of both points (which
        // sit at t=0 and the midpoint) as long as the grid is wide enough — asserted below.
        let click_col = grid.x + grid.width / 8;
        let click_row = grid.y + grid.height / 2;
        assert!(click_col.abs_diff(cell0.0) > 1 && click_col.abs_diff(cell1.0) > 1, "click column must miss both existing points");

        let before = {
            let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
            edit.points.len()
        };
        app.handle_mouse(cdp_mouse_at(click_col, click_row, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE));
        app.handle_mouse(cdp_mouse_at(click_col, click_row, MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE));
        std::thread::sleep(Duration::from_millis(10));
        app.handle_mouse(cdp_mouse_at(click_col, click_row, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE));
        app.handle_mouse(cdp_mouse_at(click_col, click_row, MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE));

        let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(edit.points.len(), before + 1, "double-click should insert exactly one new point");
    }

    /// Click-and-drag on a breakpoint moves it; the value should end up close to the row the
    /// mouse was dragged to (grid.y = the max end, so dragging near the grid's top pushes
    /// the value up toward max).
    #[test]
    fn drag_moves_the_selected_point_toward_the_cursor() {
        let (mut app, grid, _cell0, cell1) = open_cdp_envelope_editor_for_mouse_tests();
        app.handle_mouse(cdp_mouse_at(cell1.0, cell1.1, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE));
        let value_before = {
            let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
            edit.points[edit.selected].1
        };

        let drag_row = grid.y; // drag all the way to the top = max value
        app.handle_mouse(cdp_mouse_at(cell1.0, drag_row, MouseEventKind::Drag(MouseButton::Left), KeyModifiers::NONE));
        app.handle_mouse(cdp_mouse_at(cell1.0, drag_row, MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE));

        let Some(Dialog::CdpParams { envelope: Some(edit), fields, .. }) = &app.dialog else { panic!("no dialog") };
        let Some(CdpField::Number { max, .. }) = fields.get(edit.field_index) else { panic!("expected Number") };
        let value_after = edit.points[edit.selected].1;
        assert!(value_after > value_before, "dragging toward the top should raise the value");
        assert!((value_after - max).abs() < (*max - value_before) * 0.2, "should end up close to max after dragging to the top row");
    }

    /// Shift+drag moves the point at reduced speed — the same physical mouse movement (from
    /// the same start) should produce a visibly smaller change than a plain drag.
    #[test]
    fn shift_drag_moves_at_reduced_speed() {
        let (mut app, _grid, _cell0, cell1) = open_cdp_envelope_editor_for_mouse_tests();
        app.handle_mouse(cdp_mouse_at(cell1.0, cell1.1, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE));
        let value_before = {
            let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
            edit.points[edit.selected].1
        };
        // Drag the mouse up by the same number of rows with and without Shift, from two
        // otherwise-identical sessions, and compare the resulting deltas.
        app.handle_mouse(cdp_mouse_at(cell1.0, cell1.1.saturating_sub(4), MouseEventKind::Drag(MouseButton::Left), KeyModifiers::SHIFT));
        let value_after_shift = {
            let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
            edit.points[edit.selected].1
        };

        let (mut app2, _grid2, _c0, cell1b) = open_cdp_envelope_editor_for_mouse_tests();
        app2.handle_mouse(cdp_mouse_at(cell1b.0, cell1b.1, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE));
        app2.handle_mouse(cdp_mouse_at(cell1b.0, cell1b.1.saturating_sub(4), MouseEventKind::Drag(MouseButton::Left), KeyModifiers::NONE));
        let value_after_plain = {
            let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app2.dialog else { panic!("no dialog") };
            edit.points[edit.selected].1
        };

        let shift_delta = (value_after_shift - value_before).abs();
        let plain_delta = (value_after_plain - value_before).abs();
        assert!(shift_delta < plain_delta, "Shift+drag ({shift_delta}) should move less than a plain drag ({plain_delta}) for the same mouse movement");
    }

    /// Shift+click deletes the nearest breakpoint (down to the floor of 2 points).
    #[test]
    fn shift_click_deletes_nearest_point() {
        let (mut app, _grid, _cell0, cell1) = open_cdp_envelope_editor_for_mouse_tests();
        let before = {
            let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
            edit.points.len()
        };
        app.handle_mouse(cdp_mouse_at(cell1.0, cell1.1, MouseEventKind::Down(MouseButton::Left), KeyModifiers::SHIFT));
        app.handle_mouse(cdp_mouse_at(cell1.0, cell1.1, MouseEventKind::Up(MouseButton::Left), KeyModifiers::SHIFT));
        let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(edit.points.len(), before - 1, "shift+click should delete exactly one point");
    }

    #[test]
    fn cdp_envelope_value_to_row_maps_max_to_top_and_min_to_bottom() {
        assert_eq!(cdp_envelope_value_to_row(100.0, 0.0, 100.0, 11), 0, "max value is the top row");
        assert_eq!(cdp_envelope_value_to_row(0.0, 0.0, 100.0, 11), 10, "min value is the bottom row");
        assert_eq!(cdp_envelope_value_to_row(50.0, 0.0, 100.0, 11), 5, "midpoint lands in the middle row");
    }

    /// Regression test for the reported bug: `blur_blur`'s "Blurring" param has a catalog
    /// `step` of 0.01 across a 0.1-100.0 range — plain Up/Down used to nudge by that step,
    /// which is imperceptible on a 16-row grid (worse, hard to even reach the far end of the
    /// range one press at a time). Plain Up/Down must now move by a coarse, range-scaled
    /// amount; Shift+Up/Down must still use the exact catalog step for fine control.
    #[test]
    fn plain_up_down_is_coarse_shift_up_down_is_fine() {
        let mut app = new_app(Some(doc(0.1, 44100)), None);
        let catalog_index = app
            .cdp_catalog
            .processes
            .iter()
            .position(|p| p.key == "blur_blur")
            .expect("blur_blur should be in the catalog");
        app.open_cdp_params(catalog_index);
        if let Some(Dialog::CdpParams { focus, .. }) = app.dialog.as_mut() {
            *focus = 1;
        }
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else {
            panic!("expected the envelope editor to be open");
        };
        let start = edit.points[edit.selected].1;

        app.handle_dialog_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
        let after_coarse = edit.points[edit.selected].1;
        let coarse_delta = after_coarse - start;
        assert!(coarse_delta > 0.5, "plain Up should move by a visibly large amount, got {coarse_delta}");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        let Some(Dialog::CdpParams { envelope: Some(edit), .. }) = &app.dialog else { panic!("no dialog") };
        let after_fine = edit.points[edit.selected].1;
        let fine_delta = after_coarse - after_fine;
        assert!((fine_delta - 0.01).abs() < 1e-9, "Shift+Down should move by exactly the catalog step (0.01), got {fine_delta}");
    }

    /// Left/Right on the preset row cycles `preset_selected` (wrapping) and immediately
    /// loads that preset's values into `fields` — the live-preview-while-cycling behavior
    /// the merged browser's own list navigation established the pattern for.
    #[test]
    fn cdp_params_cycle_preset_loads_saved_values() {
        let _guard = crate::config::XDG_CONFIG_HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp_dir = std::env::temp_dir().join(format!("tui_wave_cdp_preset_cycle_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &temp_dir) };

        crate::model::cdp::preset::save_preset(
            "blur_avrg",
            crate::model::cdp::preset::CdpPreset { name: "Wide".into(), values: vec![crate::model::cdp::ParamValue::Number(42.0)] },
        );
        crate::model::cdp::preset::save_preset(
            "blur_avrg",
            crate::model::cdp::preset::CdpPreset { name: "Narrow".into(), values: vec![crate::model::cdp::ParamValue::Number(3.0)] },
        );

        let mut app = new_app(Some(doc(0.1, 100)), None);
        let catalog_index = app.cdp_catalog.processes.iter().position(|p| p.key == "blur_avrg").unwrap();
        app.open_cdp_params(catalog_index);
        let Some(Dialog::CdpParams { presets, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(presets.len(), 2, "both saved presets should load");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let Some(Dialog::CdpParams { preset_selected, fields, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*preset_selected, Some(0));
        let Some(CdpField::Number { input, .. }) = fields.first() else { panic!("expected Number field") };
        let first_loaded: f64 = input.value().parse().unwrap();

        app.handle_dialog_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let Some(Dialog::CdpParams { preset_selected, fields, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*preset_selected, Some(1));
        let Some(CdpField::Number { input, .. }) = fields.first() else { panic!("expected Number field") };
        let second_loaded: f64 = input.value().parse().unwrap();
        assert_ne!(first_loaded, second_loaded, "cycling to the next preset should load different values");

        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// 's' opens the save-name prompt (prefilled empty for a fresh dialog); typing a name
    /// and pressing Enter persists it to disk and selects it as the active preset —
    /// end-to-end through the actual `App` key-handling path, not just the lower-level
    /// `model::cdp::preset` functions those handlers call.
    #[test]
    fn save_preset_via_s_key_persists_and_selects_it() {
        let _guard = crate::config::XDG_CONFIG_HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp_dir = std::env::temp_dir().join(format!("tui_wave_cdp_preset_save_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &temp_dir) };

        let mut app = new_app(Some(doc(0.1, 100)), None);
        let catalog_index = app.cdp_catalog.processes.iter().position(|p| p.key == "blur_avrg").unwrap();
        app.open_cdp_params(catalog_index);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(
            matches!(&app.dialog, Some(Dialog::CdpParams { save_prompt: Some(_), .. })),
            "'s' should open the save-name prompt"
        );
        for c in "My Preset".chars() {
            app.handle_dialog_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match &app.dialog {
            Some(Dialog::CdpParams { save_prompt, presets, preset_selected, .. }) => {
                assert!(save_prompt.is_none(), "Enter should close the prompt");
                assert_eq!(presets.len(), 1);
                assert_eq!(presets[0].name, "My Preset");
                assert_eq!(*preset_selected, Some(0), "the newly saved preset should be selected");
            }
            _ => panic!("expected CdpParams still open"),
        }
        // Also verify it actually reached disk, not just in-memory dialog state.
        let on_disk = crate::model::cdp::preset::load_presets("blur_avrg", 1);
        assert_eq!(on_disk.len(), 1);
        assert_eq!(on_disk[0].name, "My Preset");

        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// 'd' deletes the currently-selected preset from both disk and the in-memory list.
    #[test]
    fn delete_preset_via_d_key_removes_it_from_disk_and_list() {
        let _guard = crate::config::XDG_CONFIG_HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp_dir = std::env::temp_dir().join(format!("tui_wave_cdp_preset_delete_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &temp_dir) };

        crate::model::cdp::preset::save_preset(
            "blur_avrg",
            crate::model::cdp::preset::CdpPreset { name: "ToDelete".into(), values: vec![crate::model::cdp::ParamValue::Number(7.0)] },
        );

        let mut app = new_app(Some(doc(0.1, 100)), None);
        let catalog_index = app.cdp_catalog.processes.iter().position(|p| p.key == "blur_avrg").unwrap();
        app.open_cdp_params(catalog_index);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // select "ToDelete"

        app.handle_dialog_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

        match &app.dialog {
            Some(Dialog::CdpParams { presets, preset_selected, .. }) => {
                assert!(presets.is_empty(), "the deleted preset should be gone from the in-memory list");
                assert!(preset_selected.is_none());
            }
            _ => panic!("expected CdpParams still open"),
        }
        assert!(crate::model::cdp::preset::load_presets("blur_avrg", 1).is_empty(), "should be gone from disk too");

        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// A glob-output job's `Finished(Ok(..))` event (`output.results.len() > 1`, e.g. from
    /// distcut/envcut) opens each result as its own new buffer instead of splicing into the
    /// current selection — exercises `App::tick_cdp`'s multi-result branch end-to-end
    /// through the real `cdp_runner`, using a fake `/bin/sh` step (no real CDP install
    /// needed) that writes three numbered files, mirroring
    /// `cdp::runner::tests::glob_output_job_loads_every_numbered_file_as_a_separate_result`'s
    /// own "no real CDP needed" precedent but one layer up, at the UI dispatch level.
    #[test]
    fn glob_output_apply_opens_one_new_buffer_per_result() {
        let mut app = new_app(Some(doc(0.1, 4)), None);
        let starting_buffer_count = app.documents.len();

        app.cdp_pending = Some(CdpPending {
            doc_index: 0,
            range: (0, 4),
            label: "CDP: Fake Glob".into(),
            catalog_index: 0,
            fields: Vec::new(),
            second_input: None,
            focus: 0,
            presets: Vec::new(),
            preset_selected: None,
        });
        app.dialog = Some(Dialog::CdpRunning {
            job_id: 42,
            title: "Fake Glob".into(),
            step_label: String::new(),
            step_index: 0,
            step_total: 1,
            started: std::time::Instant::now(),
            purpose: crate::cdp::JobPurpose::Apply,
        });

        let planned = crate::model::cdp::pipeline::PlannedJob {
            steps: vec![crate::model::cdp::pipeline::Invocation {
                bin: "sh".into(),
                args: vec![
                    "-c".into(),
                    "cp in.wav g0.wav && cp in.wav g1.wav && cp in.wav g2.wav".into(),
                ],
                label: "fake glob".into(),
                expected_output: "g0.wav".into(),
            }],
            input_files: vec![crate::model::cdp::pipeline::TempWavSpec {
                relative_name: "in.wav".into(),
                input_index: 0,
                source_channels: vec![0],
            }],
            output_files: Vec::new(),
            glob_output: Some(crate::model::cdp::pipeline::GlobOutputSpec { prefix: "g".into() }),
            brk_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: false,
        };
        app.cdp_runner.submit(crate::cdp::Job {
            id: 42,
            cdp_dir: std::path::PathBuf::from("/bin"),
            planned,
            inputs: vec![vec![vec![0.1, 0.2, 0.3, 0.4]]],
            input_sample_rate: 44100,
            purpose: crate::cdp::JobPurpose::Apply,
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while app.dialog.is_some() && std::time::Instant::now() < deadline {
            app.tick_cdp();
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(app.dialog.is_none(), "the CdpRunning dialog should close once the job finishes");
        assert_eq!(
            app.documents.len(),
            starting_buffer_count + 3,
            "expected one new buffer per numbered output file"
        );
        for doc in &app.documents[starting_buffer_count..] {
            assert_eq!(doc.channels[0].len(), 4, "each new buffer should hold the copied 4 samples");
            assert!(doc.dirty, "a never-saved new buffer should start dirty");
        }
    }

    /// `App::cdp_groups` always starts with `CDP_GROUP_ALL` then `CDP_GROUP_RECENT`,
    /// followed by every real `subcategory` in the catalog, alphabetically sorted with no
    /// duplicates — the taxonomy `scripts/convert_soundthread_catalog.py` reconciles down to
    /// one clean set (CDP-Ext-Plan.md Phase 7).
    #[test]
    fn cdp_groups_lists_all_recent_then_alphabetical_subcategories() {
        let app = new_app(Some(doc(0.1, 100)), None);
        let groups = app.cdp_groups();
        assert_eq!(groups[0], CDP_GROUP_ALL);
        assert_eq!(groups[1], CDP_GROUP_RECENT);
        let subcategories = &groups[2..];
        assert!(!subcategories.is_empty(), "the real catalog has real subcategories");
        let mut sorted = subcategories.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(subcategories, sorted.as_slice(), "should already be sorted with no duplicates");
    }

    /// Tab toggles `Dialog::CdpBrowser.group_focus`; while it's set, Up/Down move
    /// `group_selected` (re-filtering `entries`, resetting the process list's `selected` to
    /// 0) instead of the process list's own `selected` — see `App::cdp_browser_move_group`.
    #[test]
    fn tab_toggles_browser_group_focus_and_arrow_keys_follow_it() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        let Some(Dialog::CdpBrowser { group_focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(!group_focus, "should start with the process list focused");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { group_focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(*group_focus);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { group_selected, selected, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*group_selected, 1, "Down while group-focused should move group_selected, not selected");
        assert_eq!(*selected, 0, "the process list's own selection is untouched by a group move");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { group_focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(!group_focus, "Tab toggles — a second press returns focus to the process list");
    }

    /// Highlighting a real `subcategory` group filters `entries` down to exactly the
    /// processes in that subcategory — the reason the groups column exists at all.
    #[test]
    fn selecting_a_group_filters_the_process_list_by_subcategory() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        let Some(Dialog::CdpBrowser { groups, entries, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(groups.len() > 2, "need at least one real subcategory group");
        let target_group = groups[2].clone();
        let unfiltered_count = entries.len();

        // All(0) -> Recent(1) -> the first real subcategory(2).
        app.cdp_browser_move_group(2);
        let Some(Dialog::CdpBrowser { entries, group_selected, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*group_selected, 2);
        assert!(!entries.is_empty(), "the chosen group came from the catalog's own subcategories, so it must have members");
        assert!(entries.len() <= unfiltered_count);
        for &i in entries {
            assert_eq!(app.cdp_catalog.processes[i].subcategory, target_group);
        }
    }

    /// Typing into `search` while a real subcategory group is highlighted narrows within
    /// that group rather than replacing its filter — group AND search compose, they don't
    /// override each other (per the user's own refinement of the Phase 7 design: "Search
    /// narrows within the highlighted group").
    #[test]
    fn search_narrows_within_the_highlighted_group() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        app.cdp_browser_move_group(2); // the first real subcategory group
        let Some(Dialog::CdpBrowser { entries, group_selected: 2, .. }) = &app.dialog else {
            panic!("expected group index 2 to be selected")
        };
        let unfiltered_count = entries.len();
        let sample_index = entries[0];
        let sample_key = app.cdp_catalog.processes[sample_index].key.clone();

        for c in sample_key.chars() {
            app.handle_dialog_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let Some(Dialog::CdpBrowser { entries, group_selected, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(*group_selected, 2, "typing must not disturb the highlighted group");
        assert!(entries.len() <= unfiltered_count, "a specific-enough query should narrow, not widen, the group's results");
        assert!(entries.contains(&sample_index), "the sampled entry's own key must still match a search for exactly that key");
    }

    /// Search matches a process's *name* (`key`/`title`) only — a term that only appears in
    /// `short_description`/`description` must not match, or the search box effectively
    /// searches everything rather than narrowing by name (the user-reported confusion this
    /// fixes: `blur_avrg`'s title is "Average" and its key is "blur_avrg", but its
    /// description happens to say "spectral energy").
    #[test]
    fn search_matches_process_name_not_description() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        let blur_avrg = app.cdp_catalog.processes.iter().position(|p| p.key == "blur_avrg").expect("blur_avrg in catalog");
        assert!(
            app.cdp_catalog.processes[blur_avrg].short_description.to_lowercase().contains("spectral"),
            "test assumes blur_avrg's short_description mentions \"spectral\" while its name doesn't"
        );

        app.open_cdp_browser();
        for c in "spectral".chars() {
            app.handle_dialog_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let Some(Dialog::CdpBrowser { entries, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(
            !entries.contains(&blur_avrg),
            "a description-only term must not match — search is name-only, not full-text"
        );
    }

    /// Right arrow while the Groups column has focus steps into the Processes column — the
    /// natural "step right" reading of the key, distinct from Tab's plain toggle. Right in
    /// the Processes column stays search-cursor movement (there's no column further right to
    /// step into).
    #[test]
    fn right_arrow_in_groups_column_moves_focus_to_processes() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { group_focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(*group_focus, "Tab should have moved focus to Groups");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { group_focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(!group_focus, "Right out of Groups should move focus to Processes");
    }

    /// Left arrow while the Processes column has focus steps back into Groups — the mirror
    /// image of `right_arrow_in_groups_column_moves_focus_to_processes` above. Left while
    /// already in Groups stays search-cursor movement (there's no column further left).
    #[test]
    fn left_arrow_in_processes_column_moves_focus_to_groups() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.open_cdp_browser();
        let Some(Dialog::CdpBrowser { group_focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(!group_focus, "should start with the process list focused");

        app.handle_dialog_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { group_focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(*group_focus, "Left out of Processes should move focus to Groups");

        // Left again, now already in Groups, has nowhere further left to step — falls
        // through to search-cursor movement, leaving focus on Groups unchanged.
        app.handle_dialog_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        let Some(Dialog::CdpBrowser { group_focus, .. }) = &app.dialog else { panic!("no dialog") };
        assert!(*group_focus, "Left while already in Groups should leave focus unchanged");
    }

    /// `Dialog::CdpBrowser.recent`'s order (most-recently-used first, from
    /// `model::cdp::recent::load_recent`) is what the "Recent" group actually shows — not
    /// catalog order, which would defeat the point of a recency-ordered shortcut list.
    #[test]
    fn recent_group_shows_most_recently_used_first() {
        let _guard = crate::config::XDG_CONFIG_HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp_dir = std::env::temp_dir().join(format!("tui_wave_cdp_recent_group_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &temp_dir) };

        let mut app = new_app(Some(doc(0.1, 100)), None);
        let first_key = app.cdp_catalog.processes[0].key.clone();
        let second_key = app.cdp_catalog.processes[1].key.clone();
        crate::model::cdp::recent::record_used(&first_key);
        crate::model::cdp::recent::record_used(&second_key);

        app.open_cdp_browser();
        app.cdp_browser_move_group(1); // All(0) -> Recent(1)
        let Some(Dialog::CdpBrowser { entries, group_selected: 1, .. }) = &app.dialog else {
            panic!("expected the Recent group to be selected")
        };
        let keys: Vec<&str> = entries.iter().map(|&i| app.cdp_catalog.processes[i].key.as_str()).collect();
        assert_eq!(keys, vec![second_key.as_str(), first_key.as_str()], "most-recently-used should come first");

        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// A successful Apply — the normal single-result splice path through `tick_cdp` — records
    /// the process as recently used, so it shows up in the browser's "Recent" group next time
    /// it's opened. Preview alone must not (see the other assertion below).
    #[test]
    fn applying_a_process_records_it_as_recently_used() {
        let _guard = crate::config::XDG_CONFIG_HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp_dir = std::env::temp_dir().join(format!("tui_wave_cdp_recent_apply_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &temp_dir) };

        let mut app = new_app(Some(doc(0.1, 4)), None);
        let catalog_index = 0;
        let expected_key = app.cdp_catalog.processes[catalog_index].key.clone();
        assert!(crate::model::cdp::recent::load_recent().is_empty(), "nothing recorded yet");

        app.cdp_pending = Some(CdpPending {
            doc_index: 0,
            range: (0, 4),
            label: "CDP: Fake".into(),
            catalog_index,
            fields: Vec::new(),
            second_input: None,
            focus: 0,
            presets: Vec::new(),
            preset_selected: None,
        });
        app.dialog = Some(Dialog::CdpRunning {
            job_id: 99,
            title: "Fake".into(),
            step_label: String::new(),
            step_index: 0,
            step_total: 1,
            started: std::time::Instant::now(),
            purpose: crate::cdp::JobPurpose::Apply,
        });

        let planned = crate::model::cdp::pipeline::PlannedJob {
            steps: vec![crate::model::cdp::pipeline::Invocation {
                bin: "cp".into(),
                args: vec!["in.wav".into(), "out.wav".into()],
                label: "fake copy".into(),
                expected_output: "out.wav".into(),
            }],
            input_files: vec![crate::model::cdp::pipeline::TempWavSpec {
                relative_name: "in.wav".into(),
                input_index: 0,
                source_channels: vec![0],
            }],
            output_files: vec![crate::model::cdp::pipeline::OutputWavSpec {
                relative_name: "out.wav".into(),
                dest_channels: vec![0],
            }],
            glob_output: None,
            brk_files: Vec::new(),
            deferred_window_params: Vec::new(),
            needs_simple_wav_input: false,
        };
        app.cdp_runner.submit(crate::cdp::Job {
            id: 99,
            cdp_dir: std::path::PathBuf::from("/bin"),
            planned,
            inputs: vec![vec![vec![0.1, 0.2, 0.3, 0.4]]],
            input_sample_rate: app.documents[0].sample_rate,
            purpose: crate::cdp::JobPurpose::Apply,
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while app.dialog.is_some() && std::time::Instant::now() < deadline {
            app.tick_cdp();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.dialog.is_none(), "the CdpRunning dialog should close once the job finishes");

        assert_eq!(
            crate::model::cdp::recent::load_recent(),
            vec![expected_key],
            "a successful Apply should record the process as recently used"
        );

        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// Shift+Tab cycles panel focus the opposite way to Tab:
    /// Waveform → Buffers → Files → Waveform. Both the kitty form (Tab+SHIFT) and the legacy
    /// BackTab form must work, and plain Tab must still cycle forward.
    #[test]
    fn shift_tab_cycles_focus_backward() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        app.file_panel.focused = false;
        app.buffer_panel.focused = false; // start at the waveform

        for expected in [Focus::Buffers, Focus::Files, Focus::Waveform] {
            app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
            assert_eq!(app.focus(), expected);
        }

        // Legacy BackTab form takes the same backward step.
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(app.focus(), Focus::Buffers);

        // Plain Tab still goes forward (Buffers → Waveform).
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus(), Focus::Waveform);
    }

    /// The shared buffer name (used by both the Buffers panel and the waveform header) is the
    /// file name when saved, and `_NEW_NNN` (1-based) for a never-saved buffer — never
    /// "untitled".
    #[test]
    fn buffer_name_uses_new_label_for_unsaved_and_filename_for_saved() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.documents.push(doc(0.1, 10));
        app.documents[1].path = Some(PathBuf::from("/tmp/song.wav"));
        assert_eq!(app.buffer_name(0), "_NEW_001");
        assert_eq!(app.buffer_name(1), "song.wav");
    }

    /// In dialogs, Shift+Tab steps focus backward (wrapping), the reverse of Tab — verified
    /// on Mix to Mono's three slots (two channel inputs + the tanh toggle).
    #[test]
    fn dialog_shift_tab_cycles_focus_backward() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.dialog = Some(Dialog::MixToMono {
            inputs: vec![TextInput::new("0"), TextInput::new("0")],
            focused: 0,
            tanh_clip: false,
        });
        let focused = |app: &App| match &app.dialog {
            Some(Dialog::MixToMono { focused, .. }) => *focused,
            _ => panic!("dialog should still be open"),
        };
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)); // 0 → 1
        assert_eq!(focused(&app), 1);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)); // 1 → 0
        assert_eq!(focused(&app), 0);
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)); // 0 → 2 (wrap)
        assert_eq!(focused(&app), 2);
    }

    /// Export Regions' "Do!" is inactive until both the subfolder and base name are filled:
    /// pressing Enter with either blank leaves the dialog open rather than dismissing it.
    /// (Only the inactive path is exercised — a valid submit would write files to disk.)
    #[test]
    fn export_regions_do_inactive_until_names_filled() {
        let mut app = new_app(Some(doc(0.1, 100)), None);
        let open = |folder: &str, base: &str| Dialog::ExportRegions {
            folder_input: TextInput::new(folder),
            base_name_input: TextInput::new(base),
            depth: BitDepth::Float32,
            dither: false,
            limit_length: false,
            limit_length_input: TextInput::new("1000"),
            normalize: false,
            normalize_input: TextInput::new("0.0"),
            fade_in: true,
            fade_in_input: TextInput::new("5"),
            fade_out: true,
            fade_out_input: TextInput::new("5"),
            focused: 0,
        };

        // Both blank → Enter is a no-op, dialog stays open.
        app.dialog = Some(open("", ""));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.dialog.is_some(), "Do! must be inactive with both names blank");

        // Only one filled → still inactive.
        app.dialog = Some(open("regions", "   "));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.dialog.is_some(), "Do! must be inactive with a blank base name");
    }

    /// Export Regions' "Normalize regions" option scales each region independently to the
    /// target dB peak — a quiet region and a loud region in the same file must both end up
    /// at the target, not share one gain computed from the whole document's peak.
    #[test]
    fn export_regions_normalizes_each_region_independently() {
        let mut samples = vec![0.2f32; 50];
        samples.extend(vec![0.8f32; 50]);
        let dir = std::env::temp_dir().join(format!("tuiwave_export_norm_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let document = Document {
            channels: vec![samples],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: Some(dir.join("src.wav")),
            markers: vec![Marker { position: 50, label: "m".into() }],
            bits_per_sample: 32,
            bext: None,
        };
        let mut app = new_app(Some(document), None);
        app.export_regions(
            "out", "r", BitDepth::Float32, false,
            RegionExportOptions {
                limit_length_ms: None,
                normalize_db: Some(0.0), // normalize to 0 dBFS
                fade_in_ms: None,
                fade_out_ms: None,
            },
        );

        let peak = |path: &std::path::Path| {
            let doc = crate::model::io::load_wav(path).unwrap();
            doc.channels[0].iter().fold(0.0f32, |m, &s| m.max(s.abs()))
        };
        assert!(
            (peak(&dir.join("out/r-001.wav")) - 1.0).abs() < 0.01,
            "the quiet region should be normalized up to 0dBFS on its own"
        );
        assert!(
            (peak(&dir.join("out/r-002.wav")) - 1.0).abs() < 0.01,
            "the loud region should be normalized to 0dBFS independently of the first region"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// "Limit length" trims a region's end before fades are applied — trimming first means
    /// the fade-out ramp lands on the tail of the *shortened* region. If fades ran first (on
    /// the original length) and were trimmed away afterward, this region would come out with
    /// no audible fade at all, since the faded samples would all fall past the cut point.
    #[test]
    fn export_regions_limit_length_trims_before_fade_out() {
        // sample_rate=1000 makes 1 sample == 1 ms, so lengths are easy to reason about.
        let samples = vec![1.0f32; 100];
        let dir = std::env::temp_dir().join(format!("tuiwave_export_limit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let document = Document {
            channels: vec![samples],
            sample_rate: 1000,
            selection: None,
            cursor: 0,
            dirty: false,
            path: Some(dir.join("src.wav")),
            markers: vec![Marker { position: 100, label: "m".into() }], // single region [0,100)
            bits_per_sample: 32,
            bext: None,
        };
        let mut app = new_app(Some(document), None);
        app.export_regions(
            "out", "r", BitDepth::Float32, false,
            RegionExportOptions {
                limit_length_ms: Some(50.0), // limit length to 50 ms (50 samples)
                normalize_db: None,
                fade_in_ms: None,
                fade_out_ms: Some(10.0), // fade out over the last 10 ms (10 samples)
            },
        );

        let doc = crate::model::io::load_wav(&dir.join("out/r-001.wav")).unwrap();
        assert_eq!(doc.channels[0].len(), 50, "the region must be trimmed to the limit");
        assert!(
            (doc.channels[0][39] - 1.0).abs() < 0.01,
            "audio before the trimmed region's fade-out window must be untouched"
        );
        assert!(
            doc.channels[0][49] < 0.05,
            "the fade-out must ramp down over the trimmed region's own tail, not the \
             original (now-discarded) tail: sample[49] = {}",
            doc.channels[0][49]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A region shorter than the length limit is left alone — "limit length" only ever
    /// trims, it never pads a short region out to the limit.
    #[test]
    fn export_regions_limit_length_leaves_shorter_regions_untouched() {
        let samples = vec![1.0f32; 110]; // regions of 30 and 80 samples once split at 30
        let dir = std::env::temp_dir().join(format!("tuiwave_export_limit_short_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let document = Document {
            channels: vec![samples],
            sample_rate: 1000,
            selection: None,
            cursor: 0,
            dirty: false,
            path: Some(dir.join("src.wav")),
            markers: vec![Marker { position: 30, label: "m".into() }],
            bits_per_sample: 32,
            bext: None,
        };
        let mut app = new_app(Some(document), None);
        app.export_regions(
            "out", "r", BitDepth::Float32, false,
            RegionExportOptions {
                limit_length_ms: Some(50.0), // limit length to 50 ms
                normalize_db: None,
                fade_in_ms: None,
                fade_out_ms: None,
            },
        );

        let r1 = crate::model::io::load_wav(&dir.join("out/r-001.wav")).unwrap();
        let r2 = crate::model::io::load_wav(&dir.join("out/r-002.wav")).unwrap();
        assert_eq!(r1.channels[0].len(), 30, "a region shorter than the limit must be untouched");
        assert_eq!(r2.channels[0].len(), 50, "a region longer than the limit must be trimmed to it");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Tab must cycle through all 12 Export Regions fields (subfolder, base name, format,
    /// dither, limit-length checkbox + ms, normalize checkbox + dB, fade-in checkbox + ms,
    /// fade-out checkbox + ms) and wrap back to 0.
    #[test]
    fn export_regions_dialog_tab_cycles_through_all_twelve_fields() {
        let mut app = new_app(Some(doc(0.1, 200)), None);
        app.dialog = Some(Dialog::ExportRegions {
            folder_input: TextInput::new("out"),
            base_name_input: TextInput::new("r"),
            depth: BitDepth::Float32,
            dither: false,
            limit_length: false,
            limit_length_input: TextInput::fresh("1000"),
            normalize: false,
            normalize_input: TextInput::fresh("0.0"),
            fade_in: true,
            fade_in_input: TextInput::fresh("5"),
            fade_out: true,
            fade_out_input: TextInput::fresh("5"),
            focused: 0,
        });
        let focused = |app: &App| match &app.dialog {
            Some(Dialog::ExportRegions { focused, .. }) => *focused,
            _ => panic!("expected Dialog::ExportRegions to be open"),
        };
        for expected in 1..=11 {
            app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            assert_eq!(focused(&app), expected);
        }
        app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(focused(&app), 0, "Tab from the last field must wrap back to the first");
    }

    /// Builds an Export Regions dialog with the given per-option (checkbox, field text)
    /// states — the shape every validation test below needs.
    fn export_regions_dialog(
        limit: (bool, &str),
        normalize: (bool, &str),
        fade_in: (bool, &str),
        fade_out: (bool, &str),
    ) -> Dialog {
        Dialog::ExportRegions {
            folder_input: TextInput::new("out"),
            base_name_input: TextInput::new("r"),
            depth: BitDepth::Float32,
            dither: false,
            limit_length: limit.0,
            limit_length_input: TextInput::new(limit.1),
            normalize: normalize.0,
            normalize_input: TextInput::new(normalize.1),
            fade_in: fade_in.0,
            fade_in_input: TextInput::new(fade_in.1),
            fade_out: fade_out.0,
            fade_out_input: TextInput::new(fade_out.1),
            focused: 0,
        }
    }

    /// Regression test: a checked option whose value field doesn't parse must block the
    /// submit and focus the offending field. A blank Normalize field used to fall back to
    /// 0 dBFS — silently boosting every exported region to full scale — and a blank limit
    /// silently disabled a cap the checkbox said was on.
    #[test]
    fn export_regions_enter_with_invalid_checked_field_keeps_dialog_open_and_focuses_it() {
        let cases = [
            (export_regions_dialog((true, ""), (false, "0"), (false, "5"), (false, "5")), er_focus::LIMIT_MS),
            (export_regions_dialog((true, "0"), (false, "0"), (false, "5"), (false, "5")), er_focus::LIMIT_MS),
            (export_regions_dialog((false, "1000"), (true, "-"), (false, "5"), (false, "5")), er_focus::NORMALIZE_DB),
            (export_regions_dialog((false, "1000"), (false, "0"), (true, ""), (false, "5")), er_focus::FADE_IN_MS),
            (export_regions_dialog((false, "1000"), (false, "0"), (false, "5"), (true, "6.-")), er_focus::FADE_OUT_MS),
        ];
        for (dialog, expected_focus) in cases {
            let mut app = new_app(Some(doc(0.5, 200)), None);
            app.documents[0].markers = vec![Marker { position: 100, label: "m".into() }];
            app.dialog = Some(dialog);
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            match &app.dialog {
                Some(Dialog::ExportRegions { focused, .. }) => assert_eq!(
                    *focused, expected_focus,
                    "the dialog must stay open with focus moved to the invalid field"
                ),
                other => panic!(
                    "expected the dialog to stay open on an invalid field (focus {expected_focus}), got {}",
                    if other.is_some() { "another dialog" } else { "no dialog" },
                ),
            }
        }
    }

    /// An *unchecked* option's field content is irrelevant — garbage there must not block
    /// the export.
    #[test]
    fn export_regions_enter_ignores_invalid_fields_of_unchecked_options() {
        let dir = std::env::temp_dir().join(format!("tuiwave_export_unchecked_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = new_app(Some(doc(0.5, 200)), None);
        app.documents[0].markers = vec![Marker { position: 100, label: "m".into() }];
        app.documents[0].path = Some(dir.join("src.wav"));
        app.dialog = Some(export_regions_dialog((false, ""), (false, "-"), (false, ""), (false, "x")));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(app.dialog, Some(Dialog::Info { .. })),
            "the export must run (success popup) despite unparseable fields on unchecked options"
        );
        assert!(dir.join("out/r-001.wav").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression test: a sub-sample length limit (rounds to 0 samples) must not truncate
    /// every region to an empty WAV — at least one sample is always kept.
    #[test]
    fn export_regions_sub_sample_limit_keeps_at_least_one_sample() {
        let dir = std::env::temp_dir().join(format!("tuiwave_export_subsample_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let document = Document {
            channels: vec![vec![0.5f32; 100]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: Some(dir.join("src.wav")),
            markers: vec![Marker { position: 50, label: "m".into() }],
            bits_per_sample: 32,
            bext: None,
        };
        let mut app = new_app(Some(document), None);
        app.export_regions(
            "out", "r", BitDepth::Float32, false,
            RegionExportOptions {
                limit_length_ms: Some(0.01), // 0.441 samples at 44.1 kHz — rounds to 0
                normalize_db: None,
                fade_in_ms: None,
                fade_out_ms: None,
            },
        );
        let r1 = crate::model::io::load_wav(dir.join("out/r-001.wav")).unwrap();
        assert_eq!(r1.channels[0].len(), 1, "a sub-sample limit must clamp to one sample, not zero");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression test: the clickable hints/apply Rect must sit on the row where the
    /// "Enter:Do!" bar actually renders. It used to be placed one row below it (the dialog
    /// height over-counted its 13 content lines as 14), so clicking the visible hints bar
    /// hit nothing and the submit was silently swallowed.
    #[test]
    fn export_regions_hints_rect_sits_on_the_rendered_hints_row() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = new_app(Some(doc(0.5, 200)), None);
        app.documents[0].markers = vec![Marker { position: 100, label: "m".into() }];
        app.dialog = Some(export_regions_dialog((false, "1000"), (false, "0"), (true, "5"), (true, "5")));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let hints = *app.dialog_row_rects.last().unwrap();
        // Find the row where "Do!" is rendered and compare.
        let buffer = terminal.backend().buffer();
        let hints_row = (0..24u16)
            .find(|&y| {
                let line: String = (0..80u16).map(|x| buffer[(x, y)].symbol()).collect();
                line.contains(":Do!")
            })
            .expect("the hints bar must be rendered somewhere");
        assert_eq!(hints.y, hints_row, "the clickable apply Rect must cover the visible hints bar");
        assert!(hints.width > 0);
    }

    /// On a terminal too short for the full dialog, the hints/apply Rect is dropped
    /// (zero-sized) instead of landing on top of whichever field row renders at
    /// popup.height - 2 — a "submit" click there used to toggle that row's checkbox.
    #[test]
    fn export_regions_clipped_dialog_drops_the_hints_rect_instead_of_colliding() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = new_app(Some(doc(0.5, 200)), None);
        app.documents[0].markers = vec![Marker { position: 100, label: "m".into() }];
        app.dialog = Some(export_regions_dialog((false, "1000"), (false, "0"), (true, "5"), (true, "5")));
        let mut terminal = Terminal::new(TestBackend::new(80, 13)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let rects = &app.dialog_row_rects;
        let hints = *rects.last().unwrap();
        assert_eq!(hints.width, 0, "a clipped dialog must not offer a mis-placed mouse submit target");
        for (i, r) in rects[..rects.len() - 1].iter().enumerate() {
            for (j, other) in rects[..rects.len() - 1].iter().enumerate().skip(i + 1) {
                assert!(
                    r.width == 0 || other.width == 0 || r.y != other.y,
                    "field rows {i} and {j} must never overlap ({r:?} vs {other:?})"
                );
            }
        }
    }

    /// Clicking the value text of a checkbox+value row must focus the value field for
    /// editing; only clicks on the label/checkbox part toggle. It used to be impossible
    /// to reach the ms/dB fields by mouse — any click on the row flipped the checkbox.
    #[test]
    fn export_regions_click_on_value_text_focuses_the_field_without_toggling() {
        let mut app = new_app(Some(doc(0.5, 200)), None);
        app.dialog = Some(export_regions_dialog((true, "1000"), (false, "0"), (true, "5"), (true, "5")));
        // Normally set at render time: 8 interactive rows before the hints/apply bar.
        app.dialog_n_interactive = 8;

        // Row 4 is the limit-length row; a click at the value column focuses the ms field.
        app.handle_dialog_row_click(4, er_focus::VALUE_COL);
        match &app.dialog {
            Some(Dialog::ExportRegions { focused, limit_length, .. }) => {
                assert_eq!(*focused, er_focus::LIMIT_MS, "the value field must receive focus");
                assert!(*limit_length, "the checkbox must not be toggled by a value click");
            }
            _ => panic!("expected Dialog::ExportRegions to be open"),
        }

        // A click left of the value column still toggles (and focuses) the checkbox.
        app.handle_dialog_row_click(4, er_focus::VALUE_COL - 1);
        match &app.dialog {
            Some(Dialog::ExportRegions { focused, limit_length, .. }) => {
                assert_eq!(*focused, er_focus::LIMIT_CB);
                assert!(!*limit_length, "a label/checkbox click must toggle");
            }
            _ => panic!("expected Dialog::ExportRegions to be open"),
        }

        // Same split on the fade-out row (row 7 → focus indices 10/11).
        app.handle_dialog_row_click(7, er_focus::VALUE_COL + 3);
        match &app.dialog {
            Some(Dialog::ExportRegions { focused, fade_out, .. }) => {
                assert_eq!(*focused, er_focus::FADE_OUT_MS);
                assert!(*fade_out, "the fade-out checkbox must not be toggled by a value click");
            }
            _ => panic!("expected Dialog::ExportRegions to be open"),
        }
    }

    #[test]
    fn delete_file_removes_it_from_disk() {
        let mut app = new_app(None, None);
        let path = std::env::temp_dir().join(format!("tuiwave_del_{}.wav", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        assert!(path.exists());
        app.delete_file(&path);
        assert!(!path.exists(), "delete_file should remove the file from disk");
    }

    #[test]
    fn rename_file_moves_on_disk_and_repoints_open_buffer() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        let dir = std::env::temp_dir();
        let old = dir.join(format!("tuiwave_ren_old_{}.wav", std::process::id()));
        let new = dir.join(format!("tuiwave_ren_new_{}.wav", std::process::id()));
        std::fs::remove_file(&new).ok();
        std::fs::write(&old, b"x").unwrap();
        app.documents[0].path = Some(old.clone());

        app.rename_file(&old, new.file_name().unwrap().to_str().unwrap());

        assert!(!old.exists(), "the old name should be gone");
        assert!(new.exists(), "the renamed file should exist");
        assert_eq!(
            app.documents[0].path.as_deref(),
            Some(new.as_path()),
            "a buffer open on the file should follow the rename"
        );
        std::fs::remove_file(&new).ok();
    }

    /// Quitting with no never-saved buffers (just dirty ones that already have a path)
    /// saves them all and quits immediately — no Save As prompt needed.
    #[test]
    fn save_and_quit_with_only_named_dirty_buffers_quits_immediately() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.documents[0].path = Some(PathBuf::from("/tmp/tui_wave_test_named_only.wav"));
        app.documents[0].dirty = true;
        app.confirm = Some(Confirm::Quit);

        app.handle_confirm_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        assert!(app.should_quit);
        assert!(!app.save_as_active);
        std::fs::remove_file("/tmp/tui_wave_test_named_only.wav").ok();
    }

    /// Quitting with several never-saved (no-path) dirty buffers must prompt for a
    /// filename for each one in turn — not silently skip and lose them — before actually
    /// quitting.
    #[test]
    fn save_and_quit_with_unnamed_buffers_prompts_for_each_name_in_order() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.documents[0].dirty = true; // idx 0: never saved
        app.push_document(doc(0.2, 10)); // idx 1: never saved
        app.documents[1].dirty = true;
        app.confirm = Some(Confirm::Quit);

        app.handle_confirm_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        // Must not quit yet — two buffers still need a filename.
        assert!(!app.should_quit);
        assert!(app.save_as_active);
        assert_eq!(app.active_document, 0, "should prompt for the first buffer first");

        let dir = std::env::temp_dir().join(format!("tui_wave_quit_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        app.file_panel.set_directory(dir.clone());

        app.save_as_input = TextInput::fresh("first.wav".to_string());
        app.handle_save_as_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // One buffer named and saved; still not done — the second one is up next.
        assert!(!app.should_quit);
        assert!(app.save_as_active);
        assert_eq!(app.active_document, 1);
        assert!(!app.documents[0].dirty);
        assert!(app.documents[0].path.is_some());

        app.save_as_input = TextInput::fresh("second.wav".to_string());
        app.handle_save_as_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Both named and saved — now it actually quits.
        assert!(app.should_quit);
        assert!(!app.save_as_active);
        assert!(!app.documents[1].dirty);
        assert!(app.documents[1].path.is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Backing out (Esc) of a queued Save-As prompt cancels the whole pending sequence —
    /// it must not quit, and must not silently move on to the next buffer either.
    #[test]
    fn escaping_a_queued_save_as_cancels_the_whole_sequence() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.documents[0].dirty = true;
        app.push_document(doc(0.2, 10));
        app.documents[1].dirty = true;
        app.confirm = Some(Confirm::Quit);
        app.handle_confirm_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(app.save_as_active);

        app.handle_save_as_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(!app.save_as_active);
        assert!(!app.should_quit);
        assert!(app.save_as_queue.is_empty(), "the pending sequence must be cleared, not just paused");
    }

    /// Closing a single never-saved buffer (with "save") must also prompt for a filename
    /// rather than silently discarding it — the buffer isn't closed until that's done.
    #[test]
    fn close_buffer_with_save_on_a_never_saved_buffer_prompts_for_a_name_first() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10)); // idx 1, never saved
        app.documents[1].dirty = true;
        app.confirm = Some(Confirm::CloseBuffer(1));

        app.handle_confirm_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        assert_eq!(app.documents.len(), 2, "must not close until the name is given");
        assert!(app.save_as_active);
        assert_eq!(app.active_document, 1);

        let dir = std::env::temp_dir().join(format!("tui_wave_close_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        app.file_panel.set_directory(dir.clone());
        app.save_as_input = TextInput::fresh("named.wav".to_string());
        app.handle_save_as_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.documents.len(), 1, "should close only after being named and saved");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Renaming a marker (via the double-click dialog) is undoable.
    #[test]
    fn rename_marker_is_undoable() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].markers = vec![Marker { position: 200, label: "Old Name".to_string() }];
        app.dialog =
            Some(Dialog::RenameMarker { position: 200, input: TextInput::fresh("New Name".to_string()) });

        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.documents[0].markers[0].label, "New Name");

        app.handle_action(Action::Undo);
        assert_eq!(app.documents[0].markers[0].label, "Old Name");
    }

    /// Opening the Gain dialog on a stereo document must record `is_stereo: true` so the
    /// "Per-channel gain" checkbox appears — it's captured once at open time since
    /// `render_dialog` has no document access to recompute it every frame.
    #[test]
    fn gain_dialog_is_stereo_aware_when_opened_on_a_two_channel_document() {
        let mut app = new_app(Some(stereo_doc(0.5, 0.25, 100)), None);
        app.handle_action(Action::Gain);
        match app.dialog {
            Some(Dialog::Gain { is_stereo, per_channel, .. }) => {
                assert!(is_stereo, "a 2-channel document must be flagged stereo");
                assert!(!per_channel, "per-channel gain must default to off");
            }
            _ => panic!("expected Dialog::Gain to be open"),
        }
    }

    /// A mono document must not offer the per-channel option — there's only one channel to
    /// split gain across.
    #[test]
    fn gain_dialog_is_not_stereo_aware_on_a_mono_document() {
        let mut app = new_app(Some(doc(0.5, 100)), None);
        app.handle_action(Action::Gain);
        match app.dialog {
            Some(Dialog::Gain { is_stereo, .. }) => assert!(!is_stereo),
            _ => panic!("expected Dialog::Gain to be open"),
        }
    }

    /// The Gain popup on a stereo document must be the same fixed size whether or not
    /// per-channel is checked — a resizing/reflowing popup was reported as looking ugly.
    /// The Right field and "Per-channel gain" checkbox lines are always reserved, so
    /// checking the box only fills in a line that was previously blank.
    #[test]
    fn gain_dialog_popup_size_is_fixed_regardless_of_per_channel() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        fn popup_rect(app: &mut App) -> Rect {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            app.dialog_row_rects[0]
        }

        let mut app = new_app(Some(stereo_doc(0.5, 0.25, 100)), None);
        app.handle_action(Action::Gain);
        let before = popup_rect(&mut app);

        app.handle_dialog_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(matches!(app.dialog, Some(Dialog::Gain { per_channel: true, .. })));
        let after = popup_rect(&mut app);

        assert_eq!(before, after, "the Gain field's row must not move when per-channel is toggled");
    }

    /// Space on the "Per-channel gain" checkbox toggles it on. Its focus index is 1 (right
    /// after the Gain field, which is always focus index 0) when per-channel is off, since
    /// the Right field isn't in the focus cycle yet.
    #[test]
    fn gain_dialog_space_toggles_per_channel_checkbox() {
        let mut app = new_app(Some(stereo_doc(0.5, 0.25, 100)), None);
        app.dialog = Some(Dialog::Gain {
            input: TextInput::fresh("0.0"),
            right_input: TextInput::fresh("0.0"),
            tanh_clip: false,
            per_channel: false,
            is_stereo: true,
            focused: 1,
        });
        app.handle_dialog_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(matches!(app.dialog, Some(Dialog::Gain { per_channel: true, .. })));
    }

    /// With per-channel gain on, Enter applies the Left and Right fields independently —
    /// this is the whole point of the feature.
    #[test]
    fn gain_dialog_per_channel_applies_independent_gain_to_each_channel() {
        let mut app = new_app(Some(stereo_doc(0.5, 0.5, 4)), None);
        app.dialog = Some(Dialog::Gain {
            input: TextInput::new("6.0206"), // Left: +6dB -> 2.0x
            right_input: TextInput::new("-6.0206"), // Right: -6dB -> 0.5x
            tanh_clip: false,
            per_channel: true,
            is_stereo: true,
            focused: 0,
        });
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!((app.documents[0].channels[0][0] - 1.0).abs() < 0.001, "left channel should double");
        assert!((app.documents[0].channels[1][0] - 0.25).abs() < 0.001, "right channel should halve");
    }

    /// With per-channel gain off (the default, even on a stereo document), Enter applies the
    /// single Gain field uniformly to every channel — unchanged behavior from before this
    /// feature existed.
    #[test]
    fn gain_dialog_uniform_applies_the_same_gain_to_every_channel() {
        let mut app = new_app(Some(stereo_doc(0.5, 0.25, 4)), None);
        app.dialog = Some(Dialog::Gain {
            input: TextInput::new("6.0206"), // +6dB -> 2.0x
            right_input: TextInput::fresh("0.0"),
            tanh_clip: false,
            per_channel: false,
            is_stereo: true,
            focused: 0,
        });
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!((app.documents[0].channels[0][0] - 1.0).abs() < 0.001);
        assert!((app.documents[0].channels[1][0] - 0.5).abs() < 0.001);
    }

    /// Dragging a marker (mouse down on its label, drag, release) collapses into a single
    /// undoable move — undo restores the pre-drag position.
    #[test]
    fn dragging_a_marker_is_undoable_as_one_move() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].markers = vec![Marker { position: 100, label: "M".to_string() }];
        app.content_width = 80;
        app.waveform_area = Rect { x: 0, y: 0, width: 80, height: 4 };
        app.viewport = Some(Viewport { samples_per_column: 1.0, scroll_offset: 0, amplitude_scale: 1.0, min_samples_per_column: 1.0, max_samples_per_column: 1_000.0, total_len: 1_000, auto_vertical_zoom: false });
        app.marker_label_rects = vec![(Rect { x: 5, y: 0, width: 5, height: 1 }, 0)];

        let mouse_at = |col: u16, kind: MouseEventKind| MouseEvent { kind, column: col, row: 0, modifiers: KeyModifiers::NONE };
        app.handle_mouse(mouse_at(6, MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(mouse_at(50, MouseEventKind::Drag(MouseButton::Left)));
        app.handle_mouse(mouse_at(50, MouseEventKind::Up(MouseButton::Left)));

        assert_eq!(app.documents[0].markers[0].position, 50);
        app.handle_action(Action::Undo);
        assert_eq!(app.documents[0].markers[0].position, 100);
    }

    /// A plain click on a marker label (mouse down + up, no drag in between) must not push
    /// a no-op undo entry — there was no actual movement to undo.
    #[test]
    fn clicking_a_marker_without_dragging_does_not_record_history() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].markers = vec![Marker { position: 100, label: "M".to_string() }];
        app.content_width = 80;
        app.waveform_area = Rect { x: 0, y: 0, width: 80, height: 4 };
        app.viewport = Some(Viewport { samples_per_column: 1.0, scroll_offset: 0, amplitude_scale: 1.0, min_samples_per_column: 1.0, max_samples_per_column: 1_000.0, total_len: 1_000, auto_vertical_zoom: false });
        app.marker_label_rects = vec![(Rect { x: 5, y: 0, width: 5, height: 1 }, 0)];

        let mouse_at = |col: u16, kind: MouseEventKind| MouseEvent { kind, column: col, row: 0, modifiers: KeyModifiers::NONE };
        app.handle_mouse(mouse_at(6, MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(mouse_at(6, MouseEventKind::Up(MouseButton::Left)));

        assert_eq!(app.documents[0].markers[0].position, 100);
        assert!(!app.histories[0].undo(&mut app.documents[0]), "no history entry should have been recorded");
    }

    /// Double-clicking the waveform background selects the region bounded by the nearest
    /// A marker sitting exactly at the insertion point must render in the cursor's accent
    /// color, not the normal marker color — otherwise its dashed line (drawn after, and
    /// so on top of, the waveform's cursor line) silently hides where the cursor actually
    /// Shift+] (rendered here as '}') selects from the cursor to the next marker, advances
    /// the cursor to the end of that selection, and scrolls it into view.
    #[test]
    fn extend_selection_to_next_marker_selects_and_advances_cursor() {
        let mut app = new_app(Some(doc(0.1, 10_000)), None);
        app.documents[0].markers = vec![
            Marker { position: 1_000, label: "A".to_string() },
            Marker { position: 5_000, label: "B".to_string() },
        ];
        app.documents[0].cursor = 1_000;
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(10_000, 80));

        app.handle_action(Action::ExtendSelectionToNextMarker);

        assert_eq!(app.documents[0].selection, Some(Selection { start: 1_000, end: 5_000 }));
        assert_eq!(app.documents[0].cursor, 5_000, "cursor should advance to the end of the selection");
    }

    /// With no marker ahead of the cursor, it selects to the end of the file instead.
    #[test]
    fn extend_selection_to_next_marker_falls_back_to_end_of_file() {
        let mut app = new_app(Some(doc(0.1, 10_000)), None);
        app.documents[0].markers = vec![Marker { position: 1_000, label: "A".to_string() }];
        app.documents[0].cursor = 1_000;
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(10_000, 80));

        app.handle_action(Action::ExtendSelectionToNextMarker);

        // The selection extends *through* end-of-file (exclusive end == total_len), not
        // just to the last sample index — see the sel_edge regression tests below.
        assert_eq!(app.documents[0].selection, Some(Selection { start: 1_000, end: 10_000 }));
        assert_eq!(app.documents[0].cursor, 9_999);
    }

    /// Regression test: Shift+End must select *through* end-of-file. The selection end is
    /// an exclusive bound, so an edge clamped to the last sample index (total_len - 1)
    /// meant Delete/Cut always left the file's final sample behind — an orphaned stray
    /// value rendering as a spike at the end of the waveform.
    #[test]
    fn extend_selection_to_end_then_delete_removes_everything_to_eof() {
        let mut app = new_app(Some(doc(0.1, 10_000)), None);
        app.documents[0].cursor = 5_000;
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(10_000, 80));

        app.handle_action(Action::ExtendSelectionToEnd);
        assert_eq!(app.documents[0].selection, Some(Selection { start: 5_000, end: 10_000 }));

        app.handle_action(Action::Delete);
        assert_eq!(app.documents[0].len_samples(), 5_000, "no orphan samples may survive past the deleted tail");
    }

    /// Same off-by-one from the other side: starting at the end (Shift+Left from the last
    /// sample) must anchor the selection at total_len so the final sample is included.
    #[test]
    fn extend_selection_left_from_the_end_includes_the_last_sample() {
        let mut app = new_app(Some(doc(0.1, 10_000)), None);
        app.documents[0].cursor = 9_999;
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(10_000, 80));

        app.handle_action(Action::ExtendSelectionToStart);

        assert_eq!(app.documents[0].selection, Some(Selection { start: 10_000, end: 0 }));
        assert_eq!(app.documents[0].selection.unwrap().normalized(), (0, 10_000));
    }

    /// Regression test: a mouse drag into the column that visually contains end-of-file
    /// must select through total_len. The pointer's sample position is clamped to the last
    /// sample *index*, so a drag to the right edge could never include the file's final
    /// samples and deleting the "selected tail" left a sliver behind.
    #[test]
    fn mouse_drag_to_the_right_edge_selects_through_eof() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.content_width = 80;
        app.waveform_area = Rect { x: 0, y: 0, width: 80, height: 4 };
        // fit-to-width: 12.5 samples per column, so column 79 covers samples 987..1000.
        app.viewport = Some(Viewport::fit_to_width(1_000, 80));

        let mouse_at = |col: u16, kind: MouseEventKind| MouseEvent { kind, column: col, row: 1, modifiers: KeyModifiers::NONE };
        app.handle_mouse(mouse_at(20, MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(mouse_at(79, MouseEventKind::Drag(MouseButton::Left)));
        app.handle_mouse(mouse_at(79, MouseEventKind::Up(MouseButton::Left)));

        assert_eq!(app.documents[0].selection, Some(Selection { start: 250, end: 1_000 }));

        app.handle_action(Action::Delete);
        assert_eq!(app.documents[0].len_samples(), 250, "deleting a drag-to-the-end selection must not leave a sliver");
    }

    /// Shift+[ (rendered here as '{') selects backward to the previous marker, or the
    /// start of the file if there's none — and also advances the cursor to the active
    /// (now leftmost) edge of the selection.
    #[test]
    fn extend_selection_to_prev_marker_selects_and_falls_back_to_start_of_file() {
        let mut app = new_app(Some(doc(0.1, 10_000)), None);
        app.documents[0].markers = vec![
            Marker { position: 1_000, label: "A".to_string() },
            Marker { position: 5_000, label: "B".to_string() },
        ];
        app.documents[0].cursor = 5_000;
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(10_000, 80));

        app.handle_action(Action::ExtendSelectionToPrevMarker);
        // The anchor (the edge held fixed) is where the cursor started — 5000 — with the
        // active edge following the cursor backward to 1000; `Selection` isn't normalized.
        assert_eq!(app.documents[0].selection, Some(Selection { start: 5_000, end: 1_000 }));
        assert_eq!(app.documents[0].cursor, 1_000);

        // Repeating from there, with no earlier marker, falls back to the start of the
        // file — the anchor stays at the original 5000 (Selection::extended keeps the
        // existing selection's start, not the now-stale `old_cursor`).
        app.handle_action(Action::ExtendSelectionToPrevMarker);
        assert_eq!(app.documents[0].selection, Some(Selection { start: 5_000, end: 0 }));
        assert_eq!(app.documents[0].cursor, 0);
    }

    /// is. A marker elsewhere must keep the normal marker color.
    #[test]
    fn marker_at_cursor_position_uses_cursor_accent_color() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut doc = doc(0.1, 10_000);
        doc.cursor = 500;
        doc.markers = vec![
            Marker { position: 500, label: "Here".to_string() },
            Marker { position: 2000, label: "Elsewhere".to_string() },
        ];
        let mut app = new_app(Some(doc), None);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let wf = app.waveform_area;
        let at_cursor_col = wf.x + (500.0 / app.viewport.as_ref().unwrap().samples_per_column) as u16;
        let elsewhere_col = wf.x + (2000.0 / app.viewport.as_ref().unwrap().samples_per_column) as u16;

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(at_cursor_col, wf.y)].fg, theme::CURSOR, "marker at the cursor must use the cursor accent");
        assert_eq!(buffer[(elsewhere_col, wf.y)].fg, theme::MARKER, "a marker elsewhere must keep the normal marker color");
    }

    /// marker at-or-before the click and the nearest marker after it.
    #[test]
    fn double_click_selects_region_between_markers() {
        let mut app = new_app(Some(doc(0.5, 1_000)), None);
        app.snap_to_zero = false;
        app.documents[0].markers = vec![
            Marker { position: 200, label: "A".into() },
            Marker { position: 600, label: "B".into() },
        ];
        app.waveform_area = Rect { x: 0, y: 0, width: 1_000, height: 4 };
        app.viewport = Some(Viewport::fit_to_width(1_000, 1_000)); // 1 sample per column

        let click = |col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        // Double-click between the two markers selects exactly the span between them.
        app.handle_mouse(click(400));
        app.handle_mouse(click(400));
        assert_eq!(app.documents[0].selection, Some(Selection { start: 200, end: 600 }));

        // Double-click before the first marker selects from the start of the file.
        app.handle_mouse(click(50));
        app.handle_mouse(click(50));
        assert_eq!(app.documents[0].selection, Some(Selection { start: 0, end: 200 }));

        // Double-click past the last marker selects to the end of the file.
        app.handle_mouse(click(800));
        app.handle_mouse(click(800));
        assert_eq!(app.documents[0].selection, Some(Selection { start: 600, end: 1_000 }));
    }

    /// A left click in the waveform area should focus it (and defocus the Files/Buffers
    /// panels), even though the waveform has no toggle key of its own to focus it directly.
    #[test]
    fn clicking_waveform_focuses_it_and_defocuses_panels() {
        let mut app = new_app(Some(doc(0.5, 1_000)), None);
        app.file_panel.focused = true;
        app.waveform_area = Rect { x: 10, y: 0, width: 50, height: 10 };

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });

        assert!(!app.file_panel.focused);
        assert!(!app.buffer_panel.focused);
    }

    /// A left click inside the Files panel's rendered area focuses it, even when the click
    /// doesn't land on a specific file entry (e.g. empty space below the list).
    #[test]
    fn clicking_files_panel_area_focuses_it() {
        let mut app = new_app(Some(doc(0.5, 1_000)), None);
        app.buffer_panel.focused = true;
        app.file_panel_area = Rect { x: 0, y: 0, width: 20, height: 30 };
        app.waveform_area = Rect { x: 20, y: 0, width: 50, height: 30 };

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 25,
            modifiers: KeyModifiers::NONE,
        });

        assert!(app.file_panel.focused);
        assert!(!app.buffer_panel.focused);
    }

    /// Pressing Enter on a buffer in the Buffers panel commits the switch and hands focus to
    /// the waveform — picking a buffer is almost always followed by editing it, unlike the
    /// Files panel's Enter (browsing to open several files shouldn't require re-focusing
    /// between each one).
    #[test]
    fn enter_on_buffers_panel_switches_focus_to_waveform() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10));
        app.file_panel.focused = false;
        app.buffer_panel.focused = true;
        app.buffer_panel.selected = 1;

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.active_document, 1, "Enter should switch to the selected buffer");
        assert!(!app.buffer_panel.focused, "focus should leave the Buffers panel");
        assert_eq!(app.focus(), Focus::Waveform);
    }

    /// Same check when Enter confirms a Buffers-panel filter search — the filter-mode Enter
    /// path is separate code from the plain-Enter path above, so it needs its own coverage.
    #[test]
    fn enter_while_filtering_buffers_switches_focus_to_waveform() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10));
        app.file_panel.focused = false;
        app.buffer_panel.focused = true;
        app.buffer_panel.filtering = true;
        app.buffer_panel.selected = 1;

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.active_document, 1);
        assert!(!app.buffer_panel.focused);
        assert!(!app.buffer_panel.filtering);
    }

    /// Ctrl+A selects the whole document's audio, regardless of any prior selection.
    #[test]
    fn select_all_selects_the_whole_document() {
        let mut app = new_app(Some(doc(0.5, 1_000)), None);
        app.documents[0].selection = Some(Selection { start: 10, end: 20 });
        app.handle_action(Action::SelectAll);
        assert_eq!(app.documents[0].selection, Some(Selection { start: 0, end: 1_000 }));
    }

    /// On startup the Files panel should be focused so the first thing a user does is pick
    /// Builds an app rooted at the fixtures directory and selects `mono_sine.wav` in the
    /// Files panel (entries are ordered Parent, then dirs, then files — `tests/fixtures`
    /// has no subdirectories, so index 1 is the first file alphabetically).
    fn app_with_fixture_selected() -> App {
        let mut app = new_app(None, Some(PathBuf::from("tests/fixtures")));
        app.file_panel.focused = true;
        app.file_panel.selected = 1;
        assert!(
            app.file_panel.selected_entry().unwrap().0.ends_with("mono_sine.wav"),
            "fixture directory layout changed — update the expected index"
        );
        app
    }

    /// The app is modal: plain 'a' toggles Audition while the Files panel is focused, but
    /// the very same key toggles Auto Vertical Zoom when the Waveform is focused instead —
    /// each panel's command set can reuse a letter the other panel already claimed.
    #[test]
    fn plain_a_is_audition_in_files_focus_but_auto_vzoom_in_waveform_focus() {
        let mut app = app_with_fixture_selected();
        assert!(app.file_panel.focused);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.audition, "plain 'a' in Files focus should toggle Audition");

        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.file_panel.focused = false;
        app.viewport = Some(Viewport::fit_to_width(1_000, 80));
        assert!(!app.audition);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(!app.audition, "plain 'a' in Waveform focus must not touch Audition");
        assert!(
            app.viewport.as_ref().unwrap().auto_vertical_zoom,
            "plain 'a' in Waveform focus should toggle Auto Vertical Zoom instead"
        );
    }

    /// With Audition off, navigating the Files panel must never start decoding/playing —
    /// the feature should be fully inert until toggled on.
    #[test]
    fn audition_off_never_starts_playback() {
        let mut app = app_with_fixture_selected();
        assert!(!app.audition);
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_playing_path.is_none());
        assert!(app.audition_pending.is_none());
    }

    /// Landing on a file debounces before playing: immediately after selecting it, nothing
    /// should be considered "playing" yet, only "pending".
    #[test]
    fn audition_debounces_before_playing() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        assert!(app.audition_playing_path.is_none(), "must not play before the debounce elapses");
        assert!(app.audition_pending.is_some());
    }

    /// After the debounce window elapses, Audition commits to the selected file —
    /// `audition_playing_path` switches over even if no audio device is available in this
    /// test environment (engine construction itself is best-effort).
    #[test]
    fn audition_plays_after_debounce_elapses() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_pending.is_none());
        assert_eq!(app.audition_playing_path, app.file_panel.selected_entry().map(|(p, _)| p));
    }

    /// Navigating to a different file stops whatever was playing/pending for the old one
    /// immediately, restarting the debounce for the new selection.
    #[test]
    fn audition_switches_targets_on_navigation() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_playing_path.is_some());

        app.file_panel.selected = 2; // stereo_sine.wav
        app.tick_audition();
        assert!(app.audition_playing_path.is_none(), "switching targets should stop the old one right away");
        assert!(app.audition_pending.is_some());
    }

    /// Toggling Audition off must immediately silence anything currently playing/pending.
    #[test]
    fn toggling_audition_off_stops_playback() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_playing_path.is_some());

        app.handle_action(Action::ToggleAudition);
        assert!(!app.audition);
        assert!(app.audition_playing_path.is_none());
        assert!(app.audition_pending.is_none());
    }

    /// Actually opening a file (the real "load it") must stop any audition in progress —
    /// auditioning and the loaded document's own playback must never overlap.
    #[test]
    fn opening_a_file_stops_audition() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_playing_path.is_some());

        app.open_selected_file();
        assert!(app.audition_playing_path.is_none());
        assert!(app.audition_pending.is_none());
    }

    /// A single click on a file-panel entry only selects it; a double-click is required to
    /// actually open/load it. Mouse hit-testing reads `FilePanel`'s rendered row rects, so
    /// this renders once first (via a `TestBackend`) to populate them for real.
    #[test]
    fn single_click_selects_double_click_opens() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = new_app(None, Some(PathBuf::from("tests/fixtures")));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        // Find the rendered row for mono_sine.wav (index 1: Parent, then files alphabetically)
        // by hit-testing every cell in the panel without mutating real state.
        let area = app.file_panel_area;
        let (col, row) = (area.x..area.x + area.width)
            .flat_map(|x| (area.y..area.y + area.height).map(move |y| (x, y)))
            .find(|&(x, y)| app.file_panel.hit_test(x, y) == Some(1))
            .expect("mono_sine.wav row not found in rendered panel");

        let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE };

        // Single click only selects — no document gets loaded.
        app.handle_mouse(click);
        assert_eq!(app.documents.len(), 0, "a single click must not open the file");
        assert!(app.file_panel.selected_entry().unwrap().0.ends_with("mono_sine.wav"));

        // A second click on the same cell within the double-click window opens it.
        app.handle_mouse(click);
        assert_eq!(app.documents.len(), 1, "a double-click must open the file");
    }

    /// On startup the Files panel should be focused so the first thing a user does is pick
    /// a file, rather than landing on an empty waveform with nothing to act on.
    #[test]
    fn files_panel_is_focused_on_startup() {
        let app = new_app(None, None);
        assert!(app.file_panel.focused);
        assert!(!app.buffer_panel.focused);
    }

    /// A genuine hold — repeats landing tightly spaced (simulating terminal auto-repeat,
    /// which fires every ~20-50ms) — must ramp the multiplier above 1x once enough of them
    /// land in a row, and a gap long enough to be a fresh keypress resets the count.
    #[test]
    fn nav_step_multiplier_ramps_on_a_genuine_hold() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);

        let first = app.nav_step_multiplier(Action::MoveCursorRight);
        assert_eq!(first, 1.0, "a fresh press should not be accelerated");

        // Simulate a held key: many repeats at a tight (~30ms) gap, well under the
        // 120ms fast-repeat threshold. Acceleration only kicks in once the streak count
        // clears the start threshold (5).
        let mut multiplier = 1.0;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(30));
            multiplier = app.nav_step_multiplier(Action::MoveCursorRight);
        }
        assert!(multiplier > 1.0, "a sustained tight-gap hold should accelerate");

        std::thread::sleep(Duration::from_millis(100));
        let switched = app.nav_step_multiplier(Action::MoveCursorLeft);
        assert_eq!(switched, 1.0, "switching to a different action should reset the streak");

        std::thread::sleep(Duration::from_millis(400));
        let after_gap = app.nav_step_multiplier(Action::MoveCursorLeft);
        assert_eq!(after_gap, 1.0, "a gap past the fast-repeat threshold should be treated as a fresh press");
    }

    /// The actual bug report this guards against: tapping the same arrow key repeatedly
    /// *by hand* (not holding it) must never accelerate, no matter how long the tapping is
    /// sustained — elapsed wall-clock time alone must not be what acceleration ramps on.
    #[test]
    fn nav_step_multiplier_never_accelerates_from_manual_tapping() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);

        // Each tap is 150ms apart — past the 120ms fast-repeat gap — repeated many times
        // (1.35s of sustained tapping). Every single one must stay at 1x.
        for _ in 0..9 {
            std::thread::sleep(Duration::from_millis(150));
            let multiplier = app.nav_step_multiplier(Action::MoveCursorRight);
            assert_eq!(multiplier, 1.0, "a manual tap, however sustained, must never accelerate");
        }
    }

    /// Fine mode must never accelerate, even mid a genuine tight-gap hold.
    #[test]
    fn nav_step_multiplier_disabled_in_fine_mode() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.fine_mode = true;
        let mut multiplier = 1.0;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(30));
            multiplier = app.nav_step_multiplier(Action::MoveCursorRight);
        }
        assert_eq!(multiplier, 1.0, "fine mode must never accelerate");
    }

    /// "Insertion Point Follows Playback": snapping moves the cursor to the given position
    /// and scrolls it into view, regardless of where the cursor was before.
    #[test]
    fn snap_cursor_to_moves_cursor_and_scrolls_into_view() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(1_000_000, 80));
        app.documents[0].cursor = 0;

        app.snap_cursor_to(500_000);

        assert_eq!(app.documents[0].cursor, 500_000);
        let viewport = app.viewport.as_ref().unwrap();
        let span = viewport.span(80);
        assert!(
            viewport.scroll_offset <= 500_000 && 500_000 < viewport.scroll_offset + span,
            "the snapped-to position must be visible in the viewport"
        );
    }

    /// "Viewport Follows Playback": while the playhead is comfortably inside the view,
    /// nothing happens; once it reaches the right edge, the view recenters on it and keeps
    /// recentering every subsequent tick (continuous scroll, not a one-off snap).
    #[test]
    fn tick_viewport_follow_recenters_once_playhead_reaches_the_edge() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.content_width = 80;
        // A deliberately zoomed-in viewport (span(80) = 800, far smaller than the
        // 1,000,000-sample file) so there's real room to scroll, unlike `fit_to_width`
        // which would fit the whole file into one screen and leave no room to test with.
        app.viewport = Some(Viewport {
            samples_per_column: 10.0,
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: 1_000_000.0,
            total_len: 1_000_000,
            auto_vertical_zoom: false,
        });
        app.viewport_follows_playback = true;

        // Playhead near the start, comfortably inside the view: no recenter yet.
        app.playhead_position = Some(100);
        app.tick_viewport_follow();
        assert!(!app.viewport_following);
        assert_eq!(app.viewport.as_ref().unwrap().scroll_offset, 0);

        // Move the playhead to the right edge of the current view — this should trigger
        // the sticky "following" mode and recenter.
        let span = app.viewport.as_ref().unwrap().span(80);
        app.playhead_position = Some(span - 1);
        app.tick_viewport_follow();
        assert!(app.viewport_following, "reaching the right edge should engage following");
        let half = app.viewport.as_ref().unwrap().span(80) / 2;
        assert_eq!(app.viewport.as_ref().unwrap().scroll_offset, (span - 1).saturating_sub(half));

        // Once following, it keeps recentering on every subsequent tick, even though the
        // playhead is no longer literally at the edge (it's at the new center).
        let playhead_2 = span - 1 + 1000;
        app.playhead_position = Some(playhead_2);
        app.tick_viewport_follow();
        let viewport = app.viewport.as_ref().unwrap();
        assert_eq!(viewport.scroll_offset + viewport.span(80) / 2, playhead_2);
    }

    /// Pausing playback (handled via `tick_viewport_follow` seeing no playhead) must drop
    /// out of following mode.
    #[test]
    fn viewport_follow_resets_when_playhead_disappears() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(1_000_000, 80));
        app.viewport_follows_playback = true;
        app.viewport_following = true;
        app.playhead_position = None; // playback stopped

        app.tick_viewport_follow();
        assert!(!app.viewport_following);
    }

    /// Toggling the feature off must drop out of following mode immediately, even mid-follow.
    #[test]
    fn viewport_follow_resets_when_toggled_off() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(1_000_000, 80));
        app.viewport_follows_playback = true;
        app.viewport_following = true;
        app.playhead_position = Some(500);

        app.viewport_follows_playback = false;
        app.tick_viewport_follow();
        assert!(!app.viewport_following);
    }

    /// Copy-to-New must create a *dirty* buffer (unsaved data, no path), so the quit/close
    /// confirmation fires for it instead of the app exiting silently.
    #[test]
    fn copy_to_new_marks_buffer_dirty() {
        let mut d = doc(0.5, 100);
        d.selection = Some(Selection { start: 10, end: 40 });
        let mut app = new_app(Some(d), None);
        app.handle_action(Action::CopyToNew);
        assert_eq!(app.documents.len(), 2);
        assert!(app.documents[1].dirty, "copy-to-new buffer should be dirty");
        assert!(app.documents[1].path.is_none());
        assert_eq!(app.documents[1].len_samples(), 30);
    }

    /// Closing a buffer removes its parallel history and keeps `active_document` valid.
    #[test]
    fn close_buffer_fixes_active_index_and_history() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10)); // idx 1
        app.push_document(doc(0.3, 10)); // idx 2
        assert_eq!(app.documents.len(), 3);
        assert_eq!(app.histories.len(), 3);

        app.active_document = 1;
        app.close_buffer(1); // remove the middle buffer
        assert_eq!(app.documents.len(), 2);
        assert_eq!(app.histories.len(), 2, "history must stay index-parallel");
        assert!(app.active_document < app.documents.len());
        // Remaining buffers are [0.1, 0.3]; active (still index 1) now points at 0.3.
        assert_eq!(app.documents[1].channels[0][0], 0.3);

        // Closing down to empty leaves a valid empty state.
        app.close_buffer(1);
        app.close_buffer(0);
        assert!(app.documents.is_empty());
        assert_eq!(app.active_document, 0);
    }

    /// Buffer search filters which buffers Up/Dn navigate, skipping non-matches.
    #[test]
    fn buffer_search_filters_navigation() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10));
        app.push_document(doc(0.3, 10));
        app.documents[0].path = Some(PathBuf::from("/x/alpha.wav"));
        app.documents[1].path = Some(PathBuf::from("/x/beta.wav"));
        app.documents[2].path = Some(PathBuf::from("/x/alphabet.wav"));

        app.buffer_panel.filter = "alpha".to_string();
        assert_eq!(app.filtered_buffer_indices(), vec![0, 2]); // beta filtered out

        app.buffer_panel.selected = 0;
        app.move_buffer_selection(1);
        assert_eq!(app.buffer_panel.selected, 2); // skipped index 1
        app.move_buffer_selection(1);
        assert_eq!(app.buffer_panel.selected, 2); // clamped at the last match
        app.move_buffer_selection(-1);
        assert_eq!(app.buffer_panel.selected, 0);
    }

    /// Navigating the Buffers panel with Up/Down must load the buffer immediately —
    /// no separate Enter keypress required to actually switch to it.
    #[test]
    fn moving_buffer_selection_switches_the_active_document_immediately() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10));
        app.push_document(doc(0.3, 10));
        app.active_document = 0;
        app.buffer_panel.selected = 0;

        app.move_buffer_selection(1);
        assert_eq!(app.active_document, 1, "Down should switch to buffer 1 right away");

        app.move_buffer_selection(1);
        assert_eq!(app.active_document, 2, "Down should switch to buffer 2 right away");

        app.move_buffer_selection(-1);
        assert_eq!(app.active_document, 1, "Up should switch back to buffer 1 right away");
    }

    /// Undo/redo must never cross buffers — applying an edit to one document and then
    /// undoing while a *different* document is active must not touch the other document.
    #[test]
    fn undo_history_is_isolated_per_buffer() {
        let mut app = new_app(Some(doc(1.0, 10)), None);
        app.push_document(doc(2.0, 10)); // becomes buffer 1, now active

        // Edit only buffer 1.
        let idx = 1;
        app.histories[idx].apply(delete_command(0..5), &mut app.documents[idx]);
        assert_eq!(app.documents[1].len_samples(), 5);
        assert_eq!(app.documents[0].len_samples(), 10);

        // Switching to buffer 0 and undoing must be a no-op: its history is empty.
        app.active_document = 0;
        assert!(!app.histories[0].undo(&mut app.documents[0]));
        assert_eq!(app.documents[0].len_samples(), 10);

        // Buffer 1's own undo still restores its edit.
        assert!(app.histories[1].undo(&mut app.documents[1]));
        assert_eq!(app.documents[1].len_samples(), 10);
    }

    /// Several edits on one buffer undo in reverse order, one level at a time.
    #[test]
    fn multiple_undo_levels_unwind_in_order() {
        let mut app = new_app(Some(doc(1.0, 20)), None);
        let idx = 0;
        app.histories[idx].apply(delete_command(0..5), &mut app.documents[idx]); // 20 -> 15
        app.histories[idx].apply(delete_command(0..5), &mut app.documents[idx]); // 15 -> 10
        app.histories[idx].apply(delete_command(0..5), &mut app.documents[idx]); // 10 -> 5
        assert_eq!(app.documents[0].len_samples(), 5);

        assert!(app.histories[0].undo(&mut app.documents[0]));
        assert_eq!(app.documents[0].len_samples(), 10);
        assert!(app.histories[0].undo(&mut app.documents[0]));
        assert_eq!(app.documents[0].len_samples(), 15);
        assert!(app.histories[0].undo(&mut app.documents[0]));
        assert_eq!(app.documents[0].len_samples(), 20);
        assert!(!app.histories[0].undo(&mut app.documents[0]));
    }

    fn nav_app() -> App {
        let mut app = new_app(Some(doc(0.5, 1000)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport {
            samples_per_column: 10.0,
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: 1_000_000.0,
            total_len: 1000,
            auto_vertical_zoom: false,
        });
        app.documents[0].cursor = 100;
        app
    }

    #[test]
    fn plain_navigation_clears_an_active_selection() {
        let mut app = nav_app();
        app.handle_action(Action::ExtendSelectionRight);
        assert!(app.documents[0].selection.is_some(), "selection should exist after ExtendSelectionRight");
        app.handle_action(Action::MoveCursorLeft);
        assert!(app.documents[0].selection.is_none(), "MoveCursorLeft must clear selection");
    }

    #[test]
    fn plain_jump_clears_an_active_selection() {
        let mut app = nav_app();
        app.handle_action(Action::ExtendSelectionRight);
        assert!(app.documents[0].selection.is_some());
        app.handle_action(Action::JumpStart);
        assert!(app.documents[0].selection.is_none(), "JumpStart must clear selection");

        app.documents[0].cursor = 100;
        app.handle_action(Action::ExtendSelectionRight);
        assert!(app.documents[0].selection.is_some());
        app.handle_action(Action::JumpEnd);
        assert!(app.documents[0].selection.is_none(), "JumpEnd must clear selection");
    }

    #[test]
    fn plain_page_nav_clears_an_active_selection() {
        let mut app = nav_app();
        app.handle_action(Action::ExtendSelectionRight);
        assert!(app.documents[0].selection.is_some());
        app.handle_action(Action::PageForward);
        assert!(app.documents[0].selection.is_none(), "PageForward must clear selection");

        app.handle_action(Action::ExtendSelectionRight);
        assert!(app.documents[0].selection.is_some());
        app.handle_action(Action::PageBack);
        assert!(app.documents[0].selection.is_none(), "PageBack must clear selection");
    }

    #[test]
    fn shift_page_nav_extends_selection_by_one_viewport() {
        let mut app = nav_app(); // cursor=100, spc=10, width=80 → span=800
        // Extend forward by one page (800 samples).
        app.handle_action(Action::ExtendSelectionPageForward);
        let sel = app.documents[0].selection.expect("selection should be set after ExtendSelectionPageForward");
        assert_eq!(sel.start, 100, "anchor stays at pre-move cursor");
        assert_eq!(sel.end, 900, "active edge moves one page forward");

        // Extend backward from 900 by one page (800 samples) → 100.
        app.handle_action(Action::ExtendSelectionPageBack);
        let sel = app.documents[0].selection.expect("selection still set");
        assert_eq!(sel.start, 100, "anchor is still fixed");
        assert_eq!(sel.end, 100, "active edge moved back one page, now equals anchor");
    }

    #[test]
    fn shift_home_end_extend_to_file_boundaries() {
        let mut app = nav_app(); // cursor=100
        app.handle_action(Action::ExtendSelectionToStart);
        let sel = app.documents[0].selection.expect("selection set after ExtendSelectionToStart");
        assert_eq!(sel.start, 100, "anchor at cursor position before action");
        assert_eq!(sel.end, 0, "active edge at file start");

        // Continue extending to end; anchor stays at 100.
        app.handle_action(Action::ExtendSelectionToEnd);
        let sel = app.documents[0].selection.expect("selection set after ExtendSelectionToEnd");
        assert_eq!(sel.start, 100, "anchor kept from previous action");
        assert_eq!(sel.end, 1000, "active edge extends through end-of-file (exclusive bound), not just to the last sample");
    }

    /// A very small selection (fewer samples than the zero-crossing search window) must not
    /// silently skip the fade when snap collapses start and end to the same crossing.
    #[test]
    fn fade_in_on_tiny_selection_is_applied_not_silently_skipped() {
        // Fill with a non-zero constant: every point is equally "bad" for zero-crossing snap,
        // so snapping always moves both endpoints to the same position → the bug triggers.
        let samples = vec![0.5f32; 20];
        let document = Document {
            channels: vec![samples],
            sample_rate: 44100,
            selection: Some(crate::model::selection::Selection { start: 8, end: 12 }),
            cursor: 8,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        };
        let mut app = new_app(Some(document), None);
        // Before the fade all selected samples are 0.5.
        assert!((app.documents[0].channels[0][8] - 0.5).abs() < 1e-6);
        app.handle_action(Action::FadeIn);
        // After FadeIn the dialog should open (or if it was already applied, check the result).
        // FadeIn requires Enter to confirm — open the dialog then confirm.
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // The first sample of the selection must now be near 0 (fade-in starts at 0).
        assert!(
            app.documents[0].channels[0][8].abs() < 0.1,
            "fade-in was not applied to small selection: sample[8] = {}",
            app.documents[0].channels[0][8]
        );
    }

    /// Same check for Fade Out — the last sample of the selection must be near zero.
    #[test]
    fn fade_out_on_tiny_selection_is_applied_not_silently_skipped() {
        let samples = vec![0.5f32; 20];
        let document = Document {
            channels: vec![samples],
            sample_rate: 44100,
            selection: Some(crate::model::selection::Selection { start: 8, end: 12 }),
            cursor: 8,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        };
        let mut app = new_app(Some(document), None);
        app.handle_action(Action::FadeOut);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.documents[0].channels[0][11].abs() < 0.1,
            "fade-out was not applied to small selection: sample[11] = {}",
            app.documents[0].channels[0][11]
        );
    }

    /// With no selection, Fade In must run from the start of the file to the cursor — not
    /// the whole file — so audio past the insertion point is left untouched.
    #[test]
    fn fade_in_with_no_selection_runs_from_start_to_cursor() {
        let mut document = doc(0.5, 100);
        document.cursor = 40;
        let mut app = new_app(Some(document), None);
        app.handle_action(Action::FadeIn);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.documents[0].channels[0][0].abs() < 0.1,
            "fade-in should start near silence at sample 0"
        );
        assert!(
            (app.documents[0].channels[0][39] - 0.5).abs() < 0.1,
            "fade-in should reach full volume by the cursor"
        );
        assert!(
            (app.documents[0].channels[0][60] - 0.5).abs() < 1e-6,
            "audio past the cursor must be untouched by fade-in: sample[60] = {}",
            app.documents[0].channels[0][60]
        );
    }

    /// With no selection, Fade Out must run from the cursor to the end of the file — not
    /// the whole file — so audio before the insertion point is left untouched.
    #[test]
    fn fade_out_with_no_selection_runs_from_cursor_to_end() {
        let mut document = doc(0.5, 100);
        document.cursor = 60;
        let mut app = new_app(Some(document), None);
        app.handle_action(Action::FadeOut);
        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            (app.documents[0].channels[0][30] - 0.5).abs() < 1e-6,
            "audio before the cursor must be untouched by fade-out: sample[30] = {}",
            app.documents[0].channels[0][30]
        );
        assert!(
            (app.documents[0].channels[0][60] - 0.5).abs() < 0.1,
            "fade-out should start at full volume at the cursor"
        );
        assert!(
            app.documents[0].channels[0][99].abs() < 0.1,
            "fade-out should reach near silence by the last sample"
        );
    }
}

