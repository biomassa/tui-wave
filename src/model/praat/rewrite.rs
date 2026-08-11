//! Rewriting a Praat script's `beginPause` dialogs into plain variable assignments.
//!
//! ## Why this exists
//!
//! Praat allows exactly **one** `form … endform` per script run. An author who needs a second
//! page of settings has no choice but a `beginPause … endPause` dialog, and three scripts in
//! this plugin do that — `Sidechain_Feedback_VCA`'s changelog says so in as many words
//! ("an attempt to split the long form into two `form...endform` blocks failed … because Praat
//! only supports one `form` per script run"). `Universal Convolution Generator` goes further and
//! declares no `form` at all: its entire UI is a two-stage pause wizard.
//!
//! Under `praat --run` a pause dialog does not merely block waiting for a click — it
//! **segfaults** (exit 139, core dumped; confirmed against praat 6.6.30). So those scripts were
//! unrunnable, and were excluded from the catalog outright.
//!
//! The fix is to run a *copy* of the script with every pause block replaced by the assignments
//! that block would have produced, taking the values from this app's own parameter dialog
//! instead. The original in the submodule is never touched.
//!
//! ## The substitution rules, all verified against the real scripts
//!
//! A pause field declares a variable the same way a `form` field does, and the naming rule is
//! the one that is easy to get wrong: Praat lowercases **only the first character** of the
//! label. `Output_Gain` becomes `output_Gain`, not `output_gain` — `Sidechain_Feedback_VCA`
//! reads `output_Gain` on line 943 and would fail with "Unknown variable" on anything else.
//!
//! An `optionmenu` sets **two** variables: a numeric one holding the 1-based index and a `$`
//! one holding the option's text. That is not a nicety either — the same script tests
//! `multichannel_policy = 3` numerically and `spatial_Mode$ = "Mono"` as a string, so emitting
//! one without the other breaks it.
//!
//! `endPause` returns the number of the button pressed, and a script may branch on it.
//! `Polyphonic_Improviser` ends `clicked = endPause: "Cancel", "OK", 2, 1` and then runs
//! `if clicked = 1 … exitScript`, so a rewrite that assumed button 1 would turn every run into
//! a silent no-op. The value used is the dialog's **default** button — the first number after
//! the labels — i.e. the run behaves as though the user accepted the dialog as presented.
//!
//! ## The second rewrite: `chooseDirectory$`
//!
//! [`rewrite_directory_choosers`] does the same trick for a *folder chooser*. A script that asks
//! `corpusDir$ = chooseDirectory$("Select Corpus Folder")` is asking a question this app can
//! already answer better than Praat can — `ParamKind::FolderPath` has a real folder picker, and
//! `cdp_validate_fields` blocks Apply until one is chosen, so the value can never arrive empty.
//! So the assignment is replaced by the picked path and the modal never opens.
//!
//! It is the *same* class of failure as a pause dialog, not a milder one: a modal under `--run`
//! segfaults Praat outright. Two scripts call it unconditionally, and one
//! (`OT_Grammar_Learning_from_Audio`) calls it inside the branch of a mode it otherwise ships
//! working — a latent crash for anyone picking that mode.
//!
//! Unlike a pause block, a missing chooser is an **error** rather than a no-op: the param exists
//! precisely because that call does, so a script that no longer makes it is a script the catalog
//! entry was generated against and no longer describes.

use std::collections::BTreeMap;

/// Why a script could not be rewritten. Every variant means the script no longer has the shape
/// its catalog entry was generated against, which is a reason to fail the run loudly rather
/// than to run something that will open a window and take Praat down with it.
#[derive(Debug, Clone, PartialEq)]
pub enum RewriteError {
    /// The script has fewer pause blocks than the catalog refers to — upstream removed one.
    MissingBlock { index: usize, found: usize },
    /// A `beginPause` with no `endPause` after it.
    UnterminatedBlock { line: usize },
    /// The script no longer assigns `variable` from `chooseDirectory$`. Unlike a pause block
    /// with no assignments — which is deleted, and rightly so — a hoisted folder param exists
    /// *because* of that call, so its absence means the entry no longer describes the script.
    MissingDirectoryChooser { variable: String },
}

impl std::fmt::Display for RewriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RewriteError::MissingBlock { index, found } => write!(
                f,
                "script has {found} pause dialog(s) but this process expects at least {}; \
                 the plugin may have been updated — regenerate the catalog",
                index + 1
            ),
            RewriteError::UnterminatedBlock { line } => {
                write!(f, "beginPause on line {line} has no matching endPause")
            }
            RewriteError::MissingDirectoryChooser { variable } => write!(
                f,
                "script no longer assigns {variable} from chooseDirectory$; \
                 the plugin may have been updated — regenerate the catalog"
            ),
        }
    }
}

/// The Praat variable name a field labelled `label` creates.
///
/// Three rules, and all of them bite. Only the **first character** is lowercased — `Output_Gain`
/// becomes `output_Gain`, not `output_gain`. **Spaces become underscores**: a pause field may be
/// declared `positive: "Frame length s", frame_length_s`, and Praat makes that
/// `frame_length_s`. The first scripts hoisted here happened to use underscore-only labels, so
/// the space rule was unexercised until a block with spaced labels needed it.
///
/// And a **trailing unit or range in parentheses is dropped**, along with any `_` before it:
/// `real Lock_strength_(%) 35` declares `lock_strength`. That rule was missing here and in the
/// converter's `praat_variable` until a user reported `Harmonic_Formant_Locking` sounding
/// unchanged — which it was not, but investigating it found the converter unable to match a
/// preset branch's `lock_strength = 20` to the `Lock_strength_(%)` param, so 24 processes shipped
/// preset tables that quietly listed fewer fields than the script sets.
///
/// No *hoisted* param carries a parenthetical today, so this half was latent rather than broken
/// — and it is the half that would have been worse: a preset table can only mislabel a dialog,
/// while an assignment to a variable the script never reads changes what the run does, silently.
pub fn variable_name(label: &str) -> String {
    let trimmed = match (label.rfind('('), label.ends_with(')')) {
        (Some(open), true) => label[..open].trim_end_matches('_').trim_end(),
        _ => label,
    };
    let underscored = trimmed.replace(' ', "_");
    let mut chars = underscored.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Praat's own spelling of a string literal: double quotes, with an embedded quote **doubled**.
/// There is no backslash escaping in Praat strings, so a `\` needs no special handling.
fn string_literal(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Whether `source` reads `variable` as a whole identifier rather than as a substring of a
/// longer one. `$` and `.` before the match rule out a string variable's tail and a field
/// access, both of which are different names that merely end the same way.
pub(crate) fn mentions_variable(source: &str, variable: &str) -> bool {
    let bytes = source.as_bytes();
    let name = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    source.match_indices(variable).any(|(at, _)| {
        if at > 0 && (name(bytes[at - 1]) || bytes[at - 1] == b'$' || bytes[at - 1] == b'.') {
            return false;
        }
        match bytes.get(at + variable.len()) {
            Some(&next) => !name(next),
            None => true,
        }
    })
}

/// Pause-dialog fields whose label does **not** derive the variable the script goes on to read,
/// with the variable it actually reads.
///
/// Praat names a pause field's variable after its label — `"Wow rate Hz"` sets `wow_rate_Hz` —
/// so a script that renames the label without renaming its reads shows a control that changes
/// nothing. That is a defect in the script, not in this program: the field is equally inert in
/// stock Praat, where the dialog writes a variable no later line looks at and the hardcoded
/// default survives. It is worth repairing anyway, because the alternative is a parameter in
/// our own dialog that silently does nothing.
///
/// **Upstream always wins.** Each entry is a claim that a specific defect is present, and
/// [`corrected_variable`] re-checks that claim against the script in front of it: the fix is
/// applied only while the label-derived variable is read *nowhere* and the named one *is* read.
/// So a fixed script — whether upstream renames the reads to match the label or renames the
/// label to match the reads — stops matching and runs exactly as written, with no edit here.
/// `every_pause_variable_fix_is_still_needed` fails once an entry stops applying, so a stale
/// one is deleted rather than left to rot.
///
/// Keyed by label rather than by script because the guard already carries the precision: a
/// different script using the same label correctly fails the check and is left alone.
const PAUSE_VARIABLE_FIXES: &[(&str, &str)] = &[
    // Time & Granular/Magnetic_Tape_Degradation.praat, v0.3. Every other field in the same
    // block matches its variable; these two were renamed in the 2026-08-10 rework.
    ("HF loss per generation", "hf_loss_per_generation"),
    ("Scale peak ceiling", "scale_peak"),
    // Time & Granular/HFD-Driven_Time_Warping.praat, v2.3. Label gained " relative RMS".
    ("Silence gate dB relative RMS", "silence_gate_dB"),
];

/// The variable an assignment for `label` must write, given the script it is being spliced into.
///
/// [`variable_name`]'s answer — what Praat itself would do — unless a [`PAUSE_VARIABLE_FIXES`]
/// entry applies *and* `source` still shows the defect it describes.
fn corrected_variable(label: &str, source: &str) -> String {
    let derived = variable_name(label);
    for (broken, reads) in PAUSE_VARIABLE_FIXES {
        if *broken == label && !mentions_variable(source, &derived) && mentions_variable(source, reads)
        {
            return (*reads).to_string();
        }
    }
    derived
}

/// One assignment line to stand in for a field the pause dialog would have set.
#[derive(Debug, Clone, PartialEq)]
pub enum Assignment {
    /// `real`/`positive`/`integer`/`natural`, and `boolean` as 0/1.
    Number { label: String, value: f64 },
    /// `optionmenu` — emits both the numeric and the `$` variable. `index` is 1-based, as
    /// Praat numbers options.
    Choice { label: String, index: usize, text: String },
    /// `sentence`/`word`/`text` — only a `$` variable.
    Text { label: String, value: String },
}

impl Assignment {
    /// The Praat source line(s) this assignment becomes, for splicing into `source`.
    ///
    /// `source` is consulted only by [`corrected_variable`], to decide whether this label is one
    /// of the few whose script reads a different variable than Praat would set.
    fn render_into(&self, source: &str) -> Vec<String> {
        match self {
            Assignment::Number { label, value } => {
                vec![format!("{} = {}", corrected_variable(label, source), render_number(*value))]
            }
            Assignment::Choice { label, index, text } => {
                let name = corrected_variable(label, source);
                vec![
                    format!("{name} = {index}"),
                    format!("{name}$ = {}", string_literal(text)),
                ]
            }
            Assignment::Text { label, value } => {
                vec![format!("{}$ = {}", corrected_variable(label, source), string_literal(value))]
            }
        }
    }
}

/// A finite number as Praat source. Matches `driver::praat_number_literal` — `{v}` renders 2.0
/// as `2` — and refuses to emit a non-finite value, which has no Praat spelling at all and
/// would become the bare word `inf` (an unknown variable) in the script.
fn render_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "0".to_string()
    }
}

/// Replace every `beginPause … endPause` block in `source` with the assignments given for it.
///
/// `blocks` is keyed by the block's index in source order. A block with no entry is **removed
/// and replaced by nothing**, which is correct rather than lossy: the nine
/// `Universal Convolution Generator` entries each carry one algorithm's block, and the other
/// eight sit inside an `if algorithm$ = "…"` that entry never satisfies, so nothing that would
/// have run is lost.
///
/// Indentation of the `beginPause` line is carried onto the generated lines, purely so a failed
/// run's leftover script in the temp directory is still readable — Praat itself does not care.
/// Remove a `boolean` field from the script's `form` and assign its variable instead.
///
/// For a toggle that exists only to gate a hoisted block — `Show_advanced_settings` guarding an
/// "advanced parameters" dialog. Once those parameters are in this app's own dialog the switch
/// controls nothing a user would recognise: it cannot be turned off (the values below would
/// silently stop applying) and turning it on shows nothing new. Leaving it visible meant a
/// checkbox that refused to be clicked and explained itself with a message about a dialog that
/// no longer opens (user report, 2026-08-03).
///
/// Deleting the field from the *form* rather than hiding the row in the dialog is what keeps
/// everything consistent: Praat fills a form positionally, so a field the catalog no longer
/// declares must not be there to receive an argument either. The variable is then assigned
/// immediately after `endform`, before any code that reads it.
fn apply_form_locks(lines: &mut Vec<String>, locks: &[(String, f64)]) {
    if locks.is_empty() {
        return;
    }
    let mut assignments: Vec<String> = Vec::new();
    for (label, value) in locks {
        let declaration = format!("boolean {label}");
        if let Some(at) = lines.iter().position(|l| {
            let t = l.trim();
            t.starts_with(&declaration)
                && t[declaration.len()..].chars().next().is_none_or(char::is_whitespace)
        }) {
            lines.remove(at);
        }
        assignments.push(format!("{} = {}", variable_name(label), render_number(*value)));
    }
    if let Some(at) = lines.iter().position(|l| l.trim() == "endform") {
        for (offset, line) in assignments.into_iter().enumerate() {
            lines.insert(at + 1 + offset, line);
        }
    }
}

pub fn rewrite_pause_blocks(
    source: &str,
    blocks: &BTreeMap<usize, Vec<Assignment>>,
    form_locks: &[(String, f64)],
) -> Result<String, RewriteError> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut block_index = 0usize;
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("beginPause:") || line.trim_start().starts_with("beginPause ") {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            let Some(end) = (i + 1..lines.len()).find(|&j| is_end_pause(lines[j])) else {
                return Err(RewriteError::UnterminatedBlock { line: i + 1 });
            };

            out.push(format!(
                "{indent}# --- pause dialog {block_index} replaced by tui-wave ---"
            ));
            for assignment in blocks.get(&block_index).map(Vec::as_slice).unwrap_or(&[]) {
                // Against the *original* source: the block being replaced is still present in it,
                // which does not matter — `corrected_variable` asks whether the script reads a
                // name, and the pause block itself contains no reads, only labels and defaults.
                for rendered in assignment.render_into(source) {
                    out.push(format!("{indent}{rendered}"));
                }
            }
            // `endPause` returns the pressed button, and a script may branch on it — only
            // assign when the script actually captured it, so a bare `endPause:` introduces no
            // stray variable.
            if let Some(variable) = end_pause_target(lines[end]) {
                out.push(format!("{indent}{variable} = {}", default_button(lines[end])));
            }

            block_index += 1;
            i = end + 1;
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }

    // Every block the catalog refers to must actually exist, or the entry was generated
    // against a script this no longer is.
    if let Some(&highest) = blocks.keys().next_back() {
        if highest >= block_index {
            return Err(RewriteError::MissingBlock { index: highest, found: block_index });
        }
    }

    apply_form_locks(&mut out, form_locks);

    let mut text = out.join("\n");
    if source.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// Replace every `<var>$ = chooseDirectory$ …` assignment named in `directories` with the folder
/// the user picked, so the modal that statement would open never opens.
///
/// `directories` pairs the variable **as the script spells it, `$` included** with an absolute
/// path. Every named variable must actually be assigned that way — see
/// [`RewriteError::MissingDirectoryChooser`] for why that is an error and a missing pause block's
/// assignments are not.
///
/// A variable assigned more than once (`KL_Divergence_Corpus_Resynthesis` chooses two corpora)
/// gets **every** occurrence replaced, since each is a modal of its own; the pairs are keyed by
/// variable, so the two corpora are two entries.
pub fn rewrite_directory_choosers(
    source: &str,
    directories: &[(String, String)],
) -> Result<String, RewriteError> {
    if directories.is_empty() {
        return Ok(source.to_string());
    }
    let lines: Vec<&str> = source.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut seen: Vec<&str> = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        match directory_chooser_target(line)
            .and_then(|var| directories.iter().find(|(name, _)| name == var))
        {
            Some((var, path)) => {
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                out.push(format!("{indent}{var} = {}", string_literal(path)));
                seen.push(var);
                // Praat continues a statement with a leading `...`, and the chooser's prompt may
                // well be split that way. Skipping the continuation is what makes this replace
                // the *statement* rather than leave a dangling fragment behind it.
                i += 1;
                while i < lines.len() && lines[i].trim_start().starts_with("...") {
                    i += 1;
                }
                continue;
            }
            None => out.push(line.to_string()),
        }
        i += 1;
    }

    if let Some((variable, _)) = directories.iter().find(|(name, _)| !seen.contains(&name.as_str()))
    {
        return Err(RewriteError::MissingDirectoryChooser { variable: variable.clone() });
    }

    let mut text = out.join("\n");
    if source.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// The `$`-variable a line assigns from `chooseDirectory$`, if it does.
///
/// Deliberately strict about the left-hand side: a bare variable name ending in `$` and nothing
/// else. That is what keeps `if corpusDir$ == ""` (lhs `if corpusDir$`, which has a space in it)
/// and a comment mentioning the call — `CorpusMap` has two — from being read as assignments.
fn directory_chooser_target(line: &str) -> Option<&str> {
    let (lhs, rhs) = line.split_once('=')?;
    let name = lhs.trim();
    if !name.ends_with('$')
        || !name[..name.len() - 1]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        || name.len() < 2
    {
        return None;
    }
    // Praat spells the call `chooseDirectory$: "…"`, `chooseDirectory$("…")` and
    // `chooseDirectory$ ("…")` — all three appear in this plugin. Requiring a delimiter after
    // the name keeps a longer identifier that merely starts the same way from matching.
    let rest = rhs.trim_start().strip_prefix("chooseDirectory$")?;
    (rest.is_empty() || rest.starts_with([':', '(', ' ', '\t'])).then_some(name)
}

/// Whether a line closes a pause block: `endPause: …` or `clicked = endPause: …`.
fn is_end_pause(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("endPause")
        || trimmed
            .split_once('=')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with("endPause"))
}

/// The variable an `endPause` line assigns to, if any — `clicked` in `clicked = endPause: …`.
fn end_pause_target(line: &str) -> Option<String> {
    let (lhs, rhs) = line.split_once('=')?;
    rhs.trim_start().starts_with("endPause").then(|| lhs.trim().to_string())
}

/// The dialog's default button number: the first bare integer after the button labels. Falls
/// back to 1, which is both the commonest real value and the only sane guess for a malformed
/// line. See the module docs for why guessing wrong is not harmless.
fn default_button(line: &str) -> u32 {
    let after_labels = line.rsplit('"').next().unwrap_or(line);
    after_labels
        .split(|c: char| !c.is_ascii_digit())
        .find(|token| !token.is_empty())
        .and_then(|token| token.parse().ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether any line *executes* a pause, as opposed to mentioning one in prose. Both target
    /// scripts discuss `beginPause` in their changelog comments — `Sidechain_Feedback_VCA`
    /// explains there why it had to use one — so a plain `contains` would fail on a correct
    /// rewrite. Same distinction the converter's `code_only` draws.
    fn has_live_pause(source: &str) -> bool {
        source.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("beginPause") || super::is_end_pause(l) && !t.starts_with('#')
        })
    }

    fn checkout() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools")
    }

    /// Praat lowercases only the *first* character of a label. Getting this wrong fails at run
    /// time with "Unknown variable", not at build time.
    #[test]
    fn only_the_first_character_of_a_label_is_lowercased() {
        assert_eq!(variable_name("Output_Gain"), "output_Gain");
        assert_eq!(variable_name("Spatial_Mode"), "spatial_Mode");
        assert_eq!(variable_name("Interaural_delay_ms"), "interaural_delay_ms");
        assert_eq!(variable_name("V3_speed_ratio"), "v3_speed_ratio");
        // Spaces become underscores — the shape an "advanced settings" block uses.
        assert_eq!(variable_name("Frame length s"), "frame_length_s");
        assert_eq!(variable_name("Use percentile mapping"), "use_percentile_mapping");
        assert_eq!(variable_name("K max"), "k_max");
    }

    /// A trailing unit or range in parentheses is dropped, `_` and all.
    #[test]
    fn a_trailing_unit_is_not_part_of_the_variable_name() {
        assert_eq!(variable_name("Lock_strength_(%)"), "lock_strength");
        assert_eq!(variable_name("Start_frequency_(Hz)"), "start_frequency");
        assert_eq!(variable_name("Threshold_(0-1)"), "threshold");
        assert_eq!(variable_name("Duration_(0_=_original)"), "duration");
        assert_eq!(variable_name("Grain density (grains/sec)"), "grain_density");
        // Not a *trailing* parenthetical, so nothing is dropped — the rule is about a unit
        // written after the name, not about parentheses anywhere in it.
        assert_eq!(variable_name("Rule_(a)_weight"), "rule_(a)_weight");
    }

    /// The audit behind that rule, over the whole catalog: a label whose derived variable the
    /// script never mentions means this function and Praat disagree about that label, and the
    /// consequences are silent either way — arguments are matched positionally, so nothing errors.
    ///
    /// It is what found the unit-parenthetical rule in the first place (a user report that
    /// `Harmonic_Formant_Locking` sounded unchanged; it did not, but the investigation found the
    /// converter unable to see `lock_strength = 20` inside a preset branch). 80 params failed
    /// this before the fix, 1 after.
    ///
    /// Params the converter *invents* are excluded, having no form field to correspond to: a
    /// hoisted folder, a number split out of a `key=value` field, and the renamed preset menu.
    /// A `$`-variable counts as a match, since a `sentence`/`text`/`optionmenu` field declares
    /// one.
    #[test]
    fn no_form_label_derives_a_variable_its_script_never_reads() {
        use crate::model::cdp::def::Backend;
        let checkout = checkout();
        if !checkout.is_dir() {
            return; // submodule not initialised
        }
        // Upstream's own changelog: "FORM: Processing_chunk_s and Crossfade_ms were never read
        // by any code path -- marked '(reserved)' pending a chunked implementation." A dead form
        // field is upstream's to fix, and dropping the param would change the argument count.
        const RESERVED: &[(&str, &str)] =
            &[("praat_analysis_climax_profile_matcher", "Processing_chunk_s")];

        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let mut unread = Vec::new();
        for def in catalog.processes.iter().filter(|d| d.backend() == Backend::Praat) {
            let Ok(source) = std::fs::read_to_string(checkout.join(&def.bin)) else { continue };
            for param in &def.params {
                if param.name == "Internal Preset"
                    || param.praat_directory_var.is_some()
                    || param.key_value_group.is_some()
                    || RESERVED.contains(&(def.key.as_str(), param.name.as_str()))
                {
                    continue;
                }
                let variable = variable_name(&param.name);
                // A `PAUSE_VARIABLE_FIXES` label is unread *by design* — that mismatch is the
                // defect the entry exists to repair, and `corrected_variable` has already
                // confirmed against this very source that it is still present.
                if corrected_variable(&param.name, &source) != variable {
                    continue;
                }
                if !mentions_variable(&source, &variable) {
                    unread.push(format!("{}: {:?} -> {variable}", def.key, param.name));
                }
            }
        }
        assert!(
            unread.is_empty(),
            "labels whose derived variable no script reads:\n  {}",
            unread.join("\n  ")
        );
    }

    /// Every [`PAUSE_VARIABLE_FIXES`] entry must still be repairing a live defect.
    ///
    /// The table compensates for a bug in someone else's script, so the moment upstream fixes it
    /// the entry becomes a lie — and worse than a lie if the label is later reused for a field
    /// that works, since `corrected_variable`'s guard would then be the only thing standing
    /// between us and writing the wrong variable. Failing here is how a fixed script gets its
    /// entry deleted instead of carried forever.
    #[test]
    fn every_pause_variable_fix_is_still_needed() {
        let checkout = checkout();
        if !checkout.is_dir() {
            return; // submodule not initialised
        }
        let mut sources = Vec::new();
        let mut stack = vec![checkout];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "praat") {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        sources.push(text);
                    }
                }
            }
        }
        assert!(!sources.is_empty(), "no scripts found in the checkout");

        for (label, reads) in PAUSE_VARIABLE_FIXES {
            let applies = sources.iter().any(|s| corrected_variable(label, s) == *reads);
            assert!(
                applies,
                "PAUSE_VARIABLE_FIXES entry {label:?} -> {reads} no longer applies to any script \
                 — upstream fixed it, so delete the entry"
            );
        }
    }

    /// An optionmenu sets both variables, because scripts read both.
    #[test]
    fn an_optionmenu_assigns_both_the_index_and_the_text() {
        let a = Assignment::Choice {
            label: "Spatial_Mode".into(),
            index: 2,
            text: "Stereo Wide".into(),
        };
        assert_eq!(a.render_into(""), vec!["spatial_Mode = 2", "spatial_Mode$ = \"Stereo Wide\""]);
    }

    /// A script whose pause label does not name the variable it reads gets the value anyway —
    /// and stops being touched the moment upstream repairs it, in either direction.
    #[test]
    fn a_mismatched_pause_label_writes_the_variable_the_script_reads() {
        let broken = "hf_loss_per_generation = 0.10\n\
                      beginPause: \"Advanced\"\n\
                      real: \"HF loss per generation\", hf_loss_per_generation\n\
                      clicked = endPause: \"Cancel\", \"OK\", 2, 1\n\
                      if hf_loss_per_generation > 0\nendif\n";
        let mut blocks = BTreeMap::new();
        blocks.insert(
            0,
            vec![Assignment::Number { label: "HF loss per generation".into(), value: 0.42 }],
        );
        let out = rewrite_pause_blocks(broken, &blocks, &[]).expect("rewrites");
        assert!(
            out.contains("hf_loss_per_generation = 0.42"),
            "the value must reach the variable the script actually reads:\n{out}"
        );
        assert!(
            !out.contains("hF_loss_per_generation ="),
            "and must not also write the inert one Praat would have set:\n{out}"
        );

        // Upstream fixes the reads to match the label: the guard sees the derived variable is
        // read after all and stands aside, so the script runs exactly as written.
        let fixed = broken.replace("if hf_loss_per_generation > 0", "if hF_loss_per_generation > 0");
        let out = rewrite_pause_blocks(&fixed, &blocks, &[]).expect("rewrites");
        assert!(out.contains("hF_loss_per_generation = 0.42"), "{out}");

        // And a script that never mentions the corrected variable is never guessed at.
        let unrelated = "beginPause: \"Advanced\"\n\
                         real: \"HF loss per generation\", 0.1\n\
                         clicked = endPause: \"OK\", 1\n";
        let out = rewrite_pause_blocks(unrelated, &blocks, &[]).expect("rewrites");
        assert!(out.contains("hF_loss_per_generation = 0.42"), "{out}");
    }

    /// The real script, rewritten: no dialog survives, every variable it read is assigned, and
    /// `clicked` takes the default button.
    #[test]
    fn sidechain_feedback_vca_rewrites_to_assignments() {
        let path = checkout().join("Distortion/Sidechain_Feedback_VCA.praat");
        let Ok(source) = std::fs::read_to_string(&path) else { return }; // submodule not init'd
        let mut blocks = BTreeMap::new();
        blocks.insert(
            0,
            vec![
                Assignment::Choice { label: "Spatial_Mode".into(), index: 2, text: "Stereo Wide".into() },
                Assignment::Number { label: "Interaural_delay_ms".into(), value: 0.68 },
                Assignment::Choice { label: "Multichannel_policy".into(), index: 1, text: "Downmix to mono, then duplicate".into() },
                Assignment::Choice { label: "Output_mode".into(), index: 1, text: "Normalize each stage to 0.95 (v0.2/v0.3)".into() },
                Assignment::Number { label: "Output_Gain".into(), value: 1.0 },
                Assignment::Number { label: "Random_seed".into(), value: 0.0 },
                Assignment::Number { label: "Draw_visualization".into(), value: 0.0 },
                Assignment::Number { label: "Play_result".into(), value: 0.0 },
                Assignment::Number { label: "Debug".into(), value: 0.0 },
            ],
        );
        let out = rewrite_pause_blocks(&source, &blocks, &[]).expect("rewrites");

        assert!(!has_live_pause(&out), "a surviving beginPause would segfault Praat");
        // Every variable the script goes on to read must now be set.
        for expected in [
            "spatial_Mode$ = \"Stereo Wide\"",
            "multichannel_policy = 1",
            "output_Gain = 1",
            "random_seed = 0",
            "debug = 0",
        ] {
            assert!(out.contains(expected), "missing: {expected}");
        }
        // `clicked = endPause: "Continue", 1` -> the default button.
        assert!(out.contains("clicked = 1"), "clicked must be assigned");
    }

    /// The one script whose correct output is independently knowable: its own `else` branch
    /// already assigns the same variables to the same defaults, so the rewrite can be checked
    /// against what the script itself would have done.
    #[test]
    fn polyphonic_improviser_matches_the_scripts_own_placeholder_branch() {
        let path = checkout().join("Time & Granular/Polyphonic_Improviser.praat");
        let Ok(source) = std::fs::read_to_string(&path) else { return };
        let defaults = [
            ("V3_speed_ratio", 0.50), ("V4_speed_ratio", 1.50),
            ("V1_amplitude", 1.0), ("V2_amplitude", 0.85),
            ("V3_amplitude", 0.75), ("V4_amplitude", 0.65),
            ("V1_pan", -0.35), ("V2_pan", 0.40), ("V3_pan", -0.75), ("V4_pan", 0.75),
        ];
        let mut blocks = BTreeMap::new();
        blocks.insert(
            0,
            defaults
                .iter()
                .map(|(l, v)| Assignment::Number { label: (*l).into(), value: *v })
                .collect(),
        );
        let out = rewrite_pause_blocks(&source, &blocks, &[]).expect("rewrites");
        assert!(!has_live_pause(&out));
        for (label, value) in defaults {
            let line = format!("{} = {}", variable_name(label), value);
            assert!(out.contains(&line), "rewrite disagrees with the script's own else branch: {line}");
        }
        // `endPause: "Cancel", "OK", 2, 1` — button 2 is OK. Button 1 runs `exitScript`, so
        // getting this wrong turns every run into a silent no-op.
        assert!(out.contains("clicked = 2"), "must accept the dialog, not cancel it");
    }

    /// Nine blocks are left with no assignments and simply disappear; the guards around them
    /// mean nothing that would have run is lost.
    #[test]
    fn an_unreferenced_block_is_removed_rather_than_left_in_place() {
        let path = checkout().join("Reverb/Universal Convolution Generator.praat");
        let Ok(source) = std::fs::read_to_string(&path) else { return };
        let mut blocks = BTreeMap::new();
        blocks.insert(0, vec![Assignment::Choice { label: "Algorithm".into(), index: 1, text: "Accelerando".into() }]);
        blocks.insert(1, vec![Assignment::Number { label: "First_hit_time".into(), value: 0.1 }]);
        let out = rewrite_pause_blocks(&source, &blocks, &[]).expect("rewrites");
        assert!(!has_live_pause(&out), "all ten dialogs must go, not just the two used");
        assert!(out.contains("algorithm$ = \"Accelerando\""));
        assert!(out.contains("first_hit_time = 0.1"));
        // The guards themselves survive, so the untouched algorithms stay unreachable.
        assert!(out.contains("elsif algorithm$ = \"Swing\""));
    }

    /// A catalog entry naming a block the script no longer has must fail the run, not quietly
    /// run a script with a live dialog in it.
    #[test]
    fn referring_to_a_block_that_does_not_exist_is_an_error() {
        let mut blocks = BTreeMap::new();
        blocks.insert(3, vec![Assignment::Number { label: "X".into(), value: 1.0 }]);
        let err = rewrite_pause_blocks("beginPause: \"a\"\nendPause: \"ok\", 1\n", &blocks, &[])
            .expect_err("must refuse");
        assert_eq!(err, RewriteError::MissingBlock { index: 3, found: 1 });
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;

    fn checkout() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools")
    }

    /// Whether any line still *executes* a folder chooser. `CorpusMap` discusses the call twice
    /// in its changelog comments, so a plain `contains` would fail on a correct rewrite — the
    /// same distinction `has_live_pause` draws.
    fn has_live_chooser(source: &str) -> bool {
        source.lines().any(|l| directory_chooser_target(l).is_some())
    }

    /// The statement is replaced, not merely preceded by an assignment — a surviving
    /// `chooseDirectory$` would open a modal and take Praat down with it.
    #[test]
    fn the_chooser_statement_is_replaced_rather_than_appended() {
        let source = "x = 1\ncorpusDir$ = chooseDirectory$(\"Select Corpus Folder\")\ny = 2\n";
        let out = rewrite_directory_choosers(&source.to_string(), &[(
            "corpusDir$".into(),
            "/home/me/corpus".into(),
        )])
        .expect("rewrites");
        assert_eq!(out, "x = 1\ncorpusDir$ = \"/home/me/corpus\"\ny = 2\n");
        assert!(!has_live_chooser(&out));
    }

    /// All three spellings this plugin uses. `chooseDirectory$ (` with a space is what
    /// `Batch_Channel_Format_Exporter` writes, and a prefix match without the delimiter check
    /// would be the easy way to get this subtly wrong.
    #[test]
    fn every_spelling_of_the_call_is_recognised() {
        for call in [
            "chooseDirectory$: \"pick\"",
            "chooseDirectory$(\"pick\")",
            "chooseDirectory$ (\"pick\")",
        ] {
            let out = rewrite_directory_choosers(
                &format!("dir$ = {call}\n"),
                &[("dir$".into(), "/tmp/x".into())],
            )
            .unwrap_or_else(|e| panic!("{call}: {e}"));
            assert_eq!(out, "dir$ = \"/tmp/x\"\n");
        }
    }

    /// A comparison against the same variable is not an assignment, and a comment is not code.
    /// Both shapes sit within a few lines of the real call in the scripts this rewrites.
    #[test]
    fn a_comparison_or_a_comment_is_not_mistaken_for_the_assignment() {
        let source = "# raw backslashes from chooseDirectory$, producing invalid JSON\n\
                      if corpusDir$ == \"\"\n    exitScript: \"cancelled\"\nendif\n\
                      corpusDir$ = chooseDirectory$: \"pick\"\n";
        let out = rewrite_directory_choosers(source, &[("corpusDir$".into(), "/c".into())])
            .expect("rewrites");
        assert!(out.contains("# raw backslashes from chooseDirectory$"), "the comment survives");
        assert!(out.contains("if corpusDir$ == \"\""), "the comparison survives");
        assert!(out.contains("corpusDir$ = \"/c\""));
        assert!(!has_live_chooser(&out));
    }

    /// Praat escapes a `"` by doubling it and has no backslash escape at all, which is what lets
    /// a Windows path survive unmangled. Both halves matter here — this value is a filesystem
    /// path, where a `\` is ordinary and a stray one would silently change the folder.
    #[test]
    fn a_path_with_a_quote_or_a_backslash_round_trips() {
        let out = rewrite_directory_choosers(
            "d$ = chooseDirectory$: \"pick\"\n",
            &[("d$".into(), "C:\\Users\\me\\my \"best\" corpus".into())],
        )
        .expect("rewrites");
        assert_eq!(out, "d$ = \"C:\\Users\\me\\my \"\"best\"\" corpus\"\n");
    }

    /// Two choosers, two variables, both replaced — the shape
    /// `KL_Divergence_Corpus_Resynthesis` has.
    #[test]
    fn two_choosers_are_both_replaced() {
        let source = "a$ = chooseDirectory$: \"A\"\nb$ = chooseDirectory$: \"B\"\n";
        let out = rewrite_directory_choosers(
            source,
            &[("a$".into(), "/a".into()), ("b$".into(), "/b".into())],
        )
        .expect("rewrites");
        assert_eq!(out, "a$ = \"/a\"\nb$ = \"/b\"\n");
    }

    /// A prompt split across Praat's `...` continuation lines is one statement, and leaving the
    /// tail behind would be a syntax error rather than a stray comment.
    #[test]
    fn a_continued_statement_is_consumed_whole() {
        let source = "d$ = chooseDirectory$:\n    ... \"a very long prompt\"\nnext = 1\n";
        let out = rewrite_directory_choosers(source, &[("d$".into(), "/d".into())])
            .expect("rewrites");
        assert_eq!(out, "d$ = \"/d\"\nnext = 1\n");
    }

    /// Nothing to do means nothing done: every other process must be unaffected.
    #[test]
    fn without_directories_the_script_is_returned_unchanged() {
        let source = "d$ = chooseDirectory$: \"pick\"\n";
        assert_eq!(rewrite_directory_choosers(source, &[]).expect("rewrites"), source);
    }

    /// A param naming a chooser the script no longer makes must fail the run loudly. The
    /// alternative is running a script whose folder variable is now whatever Praat left it as.
    #[test]
    fn naming_a_chooser_that_does_not_exist_is_an_error() {
        let err = rewrite_directory_choosers("x = 1\n", &[("gone$".into(), "/g".into())])
            .expect_err("must refuse");
        assert_eq!(err, RewriteError::MissingDirectoryChooser { variable: "gone$".into() });
    }

    /// The real script, in the mode that made this worth building: `OT_Grammar` ships today and
    /// segfaults the moment anyone picks its pair-corpus GEN mode.
    #[test]
    fn ot_grammar_pair_corpus_loses_its_chooser() {
        let path = checkout().join("Analysis/OT_Grammar_Learning_from_Audio.praat");
        let Ok(source) = std::fs::read_to_string(&path) else { return }; // submodule not init'd
        let out = rewrite_directory_choosers(&source, &[("pairRoot$".into(), "/corpora/ot".into())])
            .expect("rewrites");
        assert!(!has_live_chooser(&out), "a surviving chooser would segfault Praat");
        assert!(out.contains("pairRoot$ = \"/corpora/ot\""));
        // The branch that reads it is untouched, so the mode still does its own work.
        assert!(out.contains("goodDir$ = pairRoot$ + \"/good\""));
    }

    /// The other real one, whose call is unconditional — every run reached it.
    #[test]
    fn semantic_timbre_retrieval_loses_its_chooser() {
        let path = checkout().join("py/Semantic_timbre_retrieval.praat");
        let Ok(source) = std::fs::read_to_string(&path) else { return };
        let out = rewrite_directory_choosers(&source, &[("corpusDir$".into(), "/corpora/t".into())])
            .expect("rewrites");
        assert!(!has_live_chooser(&out));
        assert!(out.contains("corpusDir$ = \"/corpora/t\""));
    }
}

#[cfg(test)]
mod form_lock_tests {
    use super::*;

    /// The gating switch is removed from the `form` and assigned instead. Removing it from the
    /// form — rather than hiding its row in the dialog — is what keeps the two sides consistent:
    /// Praat fills a form by position, so a field the catalog no longer declares must not stay
    /// in the script waiting for an argument.
    #[test]
    fn a_locked_form_field_is_deleted_and_assigned() {
        let source = "form Test\n    real Amount 1.0\n    boolean Show_advanced_settings 0\nendform\n\
                      if show_advanced_settings\n    x = 1\nendif\n";
        let out = rewrite_pause_blocks(&source.replace('\\', ""), &BTreeMap::new(),
                                       &[("Show_advanced_settings".into(), 1.0)])
            .expect("rewrites");

        assert!(!out.contains("boolean Show_advanced_settings"), "the form field must be gone:\n{out}");
        assert!(out.contains("real Amount"), "other form fields must survive:\n{out}");
        // Assigned right after endform, so it is set before anything reads it.
        let after_endform = out.split("endform\n").nth(1).expect("an endform");
        assert!(
            after_endform.starts_with("show_advanced_settings = 1"),
            "must be assigned immediately after the form:\n{out}"
        );
    }

    /// No locks means the form is untouched — every other process must be unaffected.
    #[test]
    fn without_locks_the_form_is_left_alone() {
        let source = "form Test\n    boolean Show_advanced_settings 0\nendform\n";
        let out = rewrite_pause_blocks(source, &BTreeMap::new(), &[]).expect("rewrites");
        assert_eq!(out, source);
    }
}
