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
        }
    }
}

/// The Praat variable name a field labelled `label` creates.
///
/// Two rules, and both bite. Only the **first character** is lowercased — `Output_Gain` becomes
/// `output_Gain`, not `output_gain`. And **spaces become underscores**: a pause field may be
/// declared `positive: "Frame length s", frame_length_s`, and Praat makes that
/// `frame_length_s`. The first scripts hoisted here happened to use underscore-only labels, so
/// the space rule was unexercised until a block with spaced labels needed it.
pub fn variable_name(label: &str) -> String {
    let underscored = label.replace(' ', "_");
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
    /// The Praat source line(s) this assignment becomes.
    fn render(&self) -> Vec<String> {
        match self {
            Assignment::Number { label, value } => {
                vec![format!("{} = {}", variable_name(label), render_number(*value))]
            }
            Assignment::Choice { label, index, text } => {
                let name = variable_name(label);
                vec![
                    format!("{name} = {index}"),
                    format!("{name}$ = {}", string_literal(text)),
                ]
            }
            Assignment::Text { label, value } => {
                vec![format!("{}$ = {}", variable_name(label), string_literal(value))]
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
                for rendered in assignment.render() {
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

    /// An optionmenu sets both variables, because scripts read both.
    #[test]
    fn an_optionmenu_assigns_both_the_index_and_the_text() {
        let a = Assignment::Choice {
            label: "Spatial_Mode".into(),
            index: 2,
            text: "Stereo Wide".into(),
        };
        assert_eq!(a.render(), vec!["spatial_Mode = 2", "spatial_Mode$ = \"Stereo Wide\""]);
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
