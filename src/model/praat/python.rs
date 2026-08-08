//! Pointing a praatAudioTools `py`-group script at the interpreter tui-wave wants it to use.
//!
//! ## The problem
//!
//! The 34 `py`-group processes shell out to a sibling `.py` helper, so they need `numpy`,
//! `scipy` and `soundfile`. `install.sh` puts those in a virtual environment the app owns, and
//! the runner prepends that venv's `bin` to the Praat child's `PATH`.
//!
//! That works on Linux and only on Linux, for a reason the scripts make plain:
//!
//! ```praat
//! if macintosh
//!     if fileReadable("/opt/homebrew/bin/python3")
//!         python_exe$ = "/opt/homebrew/bin/python3"
//!     elsif fileReadable("/Library/Frameworks/Python.framework/Versions/3.14/bin/python3")
//!         …
//! else
//!     python_exe$ = "python3"        ← the only branch `PATH` can reach
//! endif
//! ```
//!
//! On macOS the script resolves an **absolute** path, which no amount of `PATH` manipulation can
//! influence, so the venv is bypassed and the imports fail. A Mac commonly has two or three
//! Pythons (system, Homebrew, python.org) and Praat is not launched from a terminal, so it never
//! sees a shell's environment either. One script even hardcodes
//! `C:/Users/User/praat_ddsp_env/Scripts/python.exe` — the author's own machine.
//!
//! ## The fix
//!
//! Rewrite every interpreter assignment in a *copy* of the script to the interpreter we chose,
//! exactly as [`super::rewrite`] does for pause dialogs. Whichever branch the script's own
//! `if macintosh … elsif windows …` chain takes, every branch now yields the same value, so the
//! discovery becomes a harmless no-op. The original in the submodule is never touched.
//!
//! ## Why the rule is about the *value*, not the variable name
//!
//! The scripts use at least seven names for this variable (`pythonCmd$`, `python_exe$`,
//! `candidate1$`, `pyCandidate2$`, `python_command$`, `python_cmd$`, …), and matching on names
//! means a name nobody predicted silently keeps its hardcoded path. Matching on the assigned
//! literal instead needs no such list: across all 64 scripts there are 373 interpreter literals
//! and exactly 4 strings that merely mention Python ("Python engine reported failure.",
//! "Python not found on PATH (tried 'python' and 'py').", …), and every one of those 4 is prose
//! containing spaces while every one of the 373 is a bare command or path. See
//! [`is_interpreter_literal`].
//!
//! Verified across the whole plugin: no script invokes Python by string literal — every
//! `runSubprocess:` goes through one of these variables — so rewriting the assignments covers
//! every call.

/// Whether a Praat string literal names a Python interpreter rather than mentioning one.
///
/// True for `python`, `python3`, `py`, a `.exe` form of any of those, and any absolute or
/// relative path ending in one. False for prose, which is what every non-interpreter string
/// containing "python" in this plugin turns out to be — the discriminator is that a command
/// never contains whitespace and prose always does.
pub fn is_interpreter_literal(literal: &str) -> bool {
    if literal.is_empty() || literal.chars().any(char::is_whitespace) {
        return false;
    }
    // The last path segment is what names the program; the rest is where it lives.
    let name = literal.rsplit(['/', '\\']).next().unwrap_or(literal);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    if name == "py" || name == "python" {
        return true;
    }
    // `python3`, `python3.12`, `python3.14` — a version suffix of digits and dots.
    match name.strip_prefix("python") {
        Some(rest) => {
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
        }
        None => false,
    }
}

/// Splits `line` into `(prefix, literal)` when it is an assignment of a plain string literal to
/// a `$` variable, e.g. `    python_exe$ = "python3"`. `None` for anything else — a comment, a
/// call, an assignment built by concatenation.
///
/// Deliberately strict: only a line whose *entire* right-hand side is one quoted literal is
/// touched. `pythonCmd$ = pythonDir$ + "/python3"` is left alone, because replacing part of a
/// computed path would produce something that is neither the script's answer nor ours.
fn split_literal_assignment(line: &str) -> Option<(&str, &str, &str)> {
    let eq = line.find('=')?;
    let (lhs, rest) = line.split_at(eq);
    // `==` is a comparison, not an assignment; so is `<=`/`>=`/`!=`.
    let rhs = rest.strip_prefix('=')?;
    if rhs.starts_with('=') || lhs.ends_with(['<', '>', '!', '=']) {
        return None;
    }
    let name = lhs.trim();
    let name = name.strip_suffix('$')?;
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }

    // Offsets, not length arithmetic. Subtracting the trimmed length from the line's own length
    // silently assumes the value runs to the end of the line, and 4 of these scripts are CRLF:
    // the `\r` left the prefix one byte too long, so the rewritten line came out as
    // `pythonCmd$ = ""/venv/bin/python3"` and Praat refused the whole file with "No closing
    // quote in string constant". Found by running the real scripts, not by any unit test —
    // every fixture here had Unix endings.
    let value_start = eq + 1 + (rhs.len() - rhs.trim_start().len());
    let value = rhs.trim();
    let value_end = value_start + value.len();

    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    // A doubled quote is Praat's escape, so an inner quote means this is not one flat literal.
    if inner.contains('"') {
        return None;
    }
    // Prefix keeps the original indentation and spacing around `=`; suffix keeps whatever
    // followed the value — trailing spaces, and the `\r` of a CRLF file.
    Some((&line[..value_start], inner, &line[value_end..]))
}

/// Praat's name for the folder holding the running script.
///
/// 33 of the 64 `py` scripts read it, most to find the `.py` helper they drive
/// (`defaultDirectory$ + "/spat_binaural_bridge.py"`). Since the rewritten copy runs from a temp
/// directory, it would point there — where no helper exists — so [`rewrite_default_directory`]
/// substitutes the original script's real folder. Never assigned to anywhere in the plugin,
/// verified across all 64 scripts, so replacing it with a literal cannot break an assignment.
const DEFAULT_DIRECTORY: &str = "defaultDirectory$";

/// Replaces every read of `defaultDirectory$` with a literal `dir`, so a copy running elsewhere
/// resolves its siblings exactly as the original would have.
///
/// Textual and identifier-aware: `defaultDirectory$` is only replaced where it stands alone, so
/// a longer name that merely contains it is untouched. Quoted strings are left alone too — a
/// script that *prints* the word should keep printing it.
fn rewrite_default_directory(source: &str, dir: &str) -> (String, usize) {
    let literal = format!("\"{}\"", dir.replace('"', "\"\""));
    let mut out = String::with_capacity(source.len());
    let mut count = 0;
    for line in source.split_inclusive('\n') {
        // A comment cannot affect behaviour, and rewriting one only makes the diff noisier.
        if line.trim_start().starts_with('#') {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        let mut in_string = false;
        while let Some(at) = rest.find(DEFAULT_DIRECTORY) {
            // Track quotes only up to the match, so an occurrence inside a string is skipped.
            let before = &rest[..at];
            in_string ^= before.matches('"').count() % 2 == 1;
            out.push_str(before);
            let after = &rest[at + DEFAULT_DIRECTORY.len()..];
            // `defaultDirectoryFoo$` would have matched the prefix; require the next character
            // not to continue an identifier.
            let continues = after.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_');
            if in_string || continues {
                out.push_str(DEFAULT_DIRECTORY);
            } else {
                out.push_str(&literal);
                count += 1;
            }
            rest = after;
        }
        out.push_str(rest);
    }
    (out, count)
}

/// Rewrites every interpreter assignment in `source` to `interpreter`.
///
/// Returns the new source and how many assignments were replaced. A count of zero means the
/// script resolves its interpreter some way this does not recognise, which the caller can treat
/// as a reason to say so rather than to run something that will fail on an import.
pub fn rewrite_interpreter(source: &str, interpreter: &str) -> (String, usize) {
    // Praat escapes a quote by doubling it. Our own venv path will not contain one, but a
    // user-configured interpreter might, and a broken literal would be a syntax error rather
    // than a wrong path.
    let escaped = interpreter.replace('"', "\"\"");
    let mut out = String::with_capacity(source.len());
    let mut replaced = 0;
    for line in source.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        match split_literal_assignment(body) {
            Some((prefix, literal, suffix)) if is_interpreter_literal(literal) => {
                out.push_str(prefix);
                out.push('"');
                out.push_str(&escaped);
                out.push('"');
                out.push_str(suffix);
                out.push_str(newline);
                replaced += 1;
            }
            _ => out.push_str(line),
        }
    }
    (out, replaced)
}

/// The whole treatment a `py`-group script needs to run from a temp copy: its interpreter
/// repointed at `interpreter`, and `defaultDirectory$` pinned to `original_dir` so the `.py`
/// helper beside the original is still found.
///
/// Returns the rewritten source and how many interpreter assignments were replaced — zero means
/// the script selects its interpreter in some way this does not recognise, which the caller
/// reports rather than running something that will fail later on an import.
pub fn rewrite_for_venv(source: &str, interpreter: &str, original_dir: &str) -> (String, usize) {
    let (source, replaced) = rewrite_interpreter(source, interpreter);
    let (source, _) = rewrite_default_directory(&source, original_dir);
    (source, replaced)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_interpreter_literal_the_plugin_actually_uses_is_recognised() {
        // The exact set found across all 64 py-group scripts, counts in the module docs.
        for literal in [
            "python",
            "python3",
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
            "/Library/Frameworks/Python.framework/Versions/3.14/bin/python3",
            "C:/Users/User/praat_ddsp_env/Scripts/python.exe",
            "py",
            "python3.12",
            "python.exe",
        ] {
            assert!(is_interpreter_literal(literal), "{literal:?} should be an interpreter");
        }
    }

    #[test]
    fn prose_that_merely_mentions_python_is_not_an_interpreter() {
        // Every non-interpreter string containing "python" in the plugin, verbatim.
        for literal in [
            "Python recomposition failed.",
            "Python not found on PATH (tried 'python' and 'py').",
            "Python engine reported failure.",
            "Python dependencies are unavailable (NumPy, SciPy, SoundFile).",
            "Tried: python3, python, py",
            "",
        ] {
            assert!(!is_interpreter_literal(literal), "{literal:?} should not be an interpreter");
        }
    }

    /// Something merely *starting* with the word must not qualify — `pythonic` is not a program.
    #[test]
    fn a_name_that_only_starts_with_python_is_not_an_interpreter() {
        assert!(!is_interpreter_literal("pythonic"));
        assert!(!is_interpreter_literal("python_helper"));
        assert!(!is_interpreter_literal("/usr/bin/pythonish"));
    }

    #[test]
    fn the_macos_discovery_chain_collapses_to_one_interpreter() {
        let source = "\
if macintosh
    if fileReadable(\"/opt/homebrew/bin/python3\")
        python_exe$ = \"/opt/homebrew/bin/python3\"
    elsif fileReadable(\"/usr/local/bin/python3\")
        python_exe$ = \"/usr/local/bin/python3\"
    else
        python_exe$ = \"python3\"
    endif
elsif windows
    python_exe$ = \"python\"
else
    python_exe$ = \"python3\"
endif
";
        let (out, n) = rewrite_interpreter(source, "/home/u/.config/tui-wave/praat/pyenv/bin/python3");
        assert_eq!(n, 5, "every branch's assignment is replaced");
        assert!(!out.contains("python_exe$ = \"python3\""));
        assert!(!out.contains("python_exe$ = \"/opt/homebrew/bin/python3\""));
        assert_eq!(
            out.matches("python_exe$ = \"/home/u/.config/tui-wave/praat/pyenv/bin/python3\"").count(),
            5
        );
        // The `fileReadable` probes are untouched — they are tests, not assignments, and the
        // branch they select no longer matters now that every branch agrees.
        assert!(out.contains("fileReadable(\"/opt/homebrew/bin/python3\")"));
        // Structure and indentation survive.
        assert_eq!(out.lines().count(), source.lines().count());
        assert!(out.contains("        python_exe$ = "), "indentation preserved");
    }

    #[test]
    fn variable_names_do_not_matter() {
        let source = "\
pythonCmd$ = \"python3\"
candidate1$ = \"python\"
pyCandidate2$ = \"/usr/local/bin/python3\"
python_command$ = \"py\"
";
        let (out, n) = rewrite_interpreter(source, "/venv/python3");
        assert_eq!(n, 4);
        for name in ["pythonCmd", "candidate1", "pyCandidate2", "python_command"] {
            assert!(out.contains(&format!("{name}$ = \"/venv/python3\"")), "{name} not rewritten");
        }
    }

    #[test]
    fn a_message_assignment_is_left_alone() {
        let source = "diagMsg$ = \"Python not found on PATH (tried 'python' and 'py').\"\n";
        let (out, n) = rewrite_interpreter(source, "/venv/python3");
        assert_eq!(n, 0);
        assert_eq!(out, source);
    }

    /// A computed path is neither the script's answer nor ours if half-replaced, so it is not
    /// touched at all.
    #[test]
    fn a_concatenated_assignment_is_left_alone() {
        let source = "pythonCmd$ = pythonDir$ + \"/python3\"\n";
        let (out, n) = rewrite_interpreter(source, "/venv/python3");
        assert_eq!(n, 0);
        assert_eq!(out, source);
    }

    #[test]
    fn comparisons_are_not_assignments() {
        let source = "if pythonCmd$ = \"python3\"\nendif\n";
        let (out, n) = rewrite_interpreter(source, "/venv/python3");
        // `if x$ = "y"` is Praat's equality test, not an assignment — rewriting it would
        // silently invert a branch the script uses to decide it found nothing.
        assert_eq!(n, 0, "left alone:\n{out}");
    }

    #[test]
    fn a_script_with_no_python_is_returned_unchanged() {
        let source = "form Foo\n    positive Bar 1.0\nendform\nSelect all\n";
        let (out, n) = rewrite_interpreter(source, "/venv/python3");
        assert_eq!(n, 0);
        assert_eq!(out, source);
    }

    #[test]
    fn a_quote_in_the_interpreter_path_is_escaped_praat_style() {
        let (out, n) = rewrite_interpreter("pythonCmd$ = \"python3\"\n", "/od\"d/python3");
        assert_eq!(n, 1);
        assert_eq!(out, "pythonCmd$ = \"/od\"\"d/python3\"\n");
    }

    #[test]
    fn a_file_without_a_trailing_newline_keeps_its_shape() {
        let (out, n) = rewrite_interpreter("pythonCmd$ = \"python3\"", "/venv/python3");
        assert_eq!(n, 1);
        assert_eq!(out, "pythonCmd$ = \"/venv/python3\"");
    }

    /// Four of the plugin's scripts (the `Latent*` family) use CRLF, and the first version of
    /// this rewriter mangled every one of them into `pythonCmd$ = ""/venv/bin/python3"` — Praat
    /// then refused the whole file with "No closing quote in string constant". Every fixture
    /// above has Unix endings, so nothing here caught it; the real smoke test did.
    #[test]
    fn a_crlf_script_is_rewritten_without_mangling_the_quotes() {
        let source = "if macintosh\r\n    pythonCmd$ = \"/opt/homebrew/bin/python3\"\r\nelse\r\n    pythonCmd$ = \"python3\"\r\nendif\r\n";
        let (out, n) = rewrite_interpreter(source, "/venv/bin/python3");
        assert_eq!(n, 2);
        assert!(
            out.contains("    pythonCmd$ = \"/venv/bin/python3\"\r\n"),
            "quotes mangled or CR lost:\n{out:?}"
        );
        assert!(!out.contains("\"\""), "doubled quote introduced:\n{out:?}");
        // Every line still has an even number of quotes — the property Praat actually enforces.
        for (i, line) in out.lines().enumerate() {
            assert_eq!(line.matches('"').count() % 2, 0, "line {} unbalanced: {line:?}", i + 1);
        }
        // CRLF is preserved rather than silently normalised: we are editing someone else's file.
        assert_eq!(out.matches("\r\n").count(), source.matches("\r\n").count());
    }

    /// Trailing whitespace after the value is kept, for the same reason the CR is.
    #[test]
    fn trailing_whitespace_after_the_value_survives() {
        let (out, n) = rewrite_interpreter("pythonCmd$ = \"python3\"   \n", "/venv/python3");
        assert_eq!(n, 1);
        assert_eq!(out, "pythonCmd$ = \"/venv/python3\"   \n");
    }

    /// Every real script must come out with balanced quotes on every line — the general form of
    /// the CRLF bug, over the whole corpus rather than one hand-written case.
    #[test]
    fn no_real_script_is_left_with_an_unbalanced_line() {
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        let Ok(entries) = std::fs::read_dir(checkout.join("py")) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "praat") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else { continue };
            let (out, _) = rewrite_interpreter(&source, "/venv/bin/python3");
            for (i, (before, after)) in source.lines().zip(out.lines()).enumerate() {
                assert_eq!(
                    after.matches('"').count() % 2,
                    before.matches('"').count() % 2,
                    "{} line {}: {before:?} -> {after:?}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    i + 1
                );
            }
        }
    }

    /// The rule is only worth anything if it holds against the **real** plugin, so this runs the
    /// rewrite over every script in the checkout and asserts the thing that actually matters:
    /// afterwards, no script can reach a Python other than the one we chose.
    ///
    /// This is what would have caught the macOS bug. On Linux the hardcoded absolute paths are
    /// simply never taken, so nothing about a Linux run — including the whole existing test
    /// suite — reveals that they are there.
    #[test]
    fn no_real_script_can_still_reach_a_hardcoded_interpreter() {
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/praat-audiotools");
        let Ok(entries) = std::fs::read_dir(checkout.join("py")) else {
            eprintln!("submodule not checked out; skipping");
            return;
        };

        const OURS: &str = "/home/u/.config/tui-wave/praat/pyenv/bin/python3";
        let (mut scripts, mut rewritten, mut total) = (0, 0, 0);
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "praat") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else { continue };
            scripts += 1;
            let (out, n) = rewrite_interpreter(&source, OURS);
            total += n;
            if n > 0 {
                rewritten += 1;
            }

            // Nothing may still *assign* an interpreter that is not ours. Checking assignments
            // rather than mere occurrences leaves the `fileReadable(...)` probes alone, which
            // is correct: they are conditions, and every branch now ends at the same value.
            for line in out.lines() {
                if let Some((_, literal, _)) = split_literal_assignment(line) {
                    if is_interpreter_literal(literal) {
                        assert_eq!(
                            literal,
                            OURS,
                            "{} still assigns {literal:?}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                }
            }
        }

        assert!(scripts > 50, "expected the py group, found {scripts} scripts");
        assert!(
            rewritten > 50,
            "only {rewritten} of {scripts} scripts had an interpreter rewritten"
        );
        // The corpus figure quoted in the module docs; a large drift means upstream reshaped
        // these blocks and the rule deserves re-checking rather than silently covering less.
        assert!(total > 300, "only {total} assignments rewritten across {scripts} scripts");
    }

    /// The design shares one temp filename between the pause rewrite and the Python rewrite,
    /// which is only safe while no script needs both. It holds today — no `py`-group script has
    /// a `beginPause` dialog — and this is what says so if that ever changes.
    #[test]
    fn no_process_needs_both_a_pause_rewrite_and_a_python_rewrite() {
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        for def in &catalog.processes {
            let hoisted = def.params.iter().any(|p| p.praat_pause_block.is_some());
            assert!(
                !(hoisted && def.praat_python_rewrite),
                "{} needs both rewrites; they would fight over the same copy",
                def.key
            );
        }
    }

    /// Every `py`-group entry is flagged, and nothing else is — the converter's detection and
    /// the group are two independent facts that ought to agree.
    #[test]
    fn exactly_the_py_group_is_flagged_for_a_python_rewrite() {
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let mut flagged = 0;
        for def in &catalog.processes {
            let in_py_group = def.bin.starts_with("py/");
            assert_eq!(
                def.praat_python_rewrite, in_py_group,
                "{} : bin {:?} but praat_python_rewrite = {}",
                def.key, def.bin, def.praat_python_rewrite
            );
            flagged += usize::from(def.praat_python_rewrite);
        }
        // A canary on the group's size, not a property of it: the number moves whenever
        // `PY_ALLOWED_IMPORTS` changes, and the point is that such a change is noticed rather
        // than absorbed silently. 34 -> 45 on 2026-08-08, when librosa/scikit-learn/OpenCV/
        // nara_wpe/mido and the torch stack were admitted in two optional tiers.
        assert_eq!(flagged, 45, "the py group is 45 processes");
    }

    /// End to end through the planner: a py-group process must come out asking for a rewritten
    /// copy, and a non-py one must not.
    #[test]
    fn the_planner_asks_for_a_rewritten_copy_only_for_py_group_processes() {
        use crate::model::cdp::ParamValue;
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let venv = std::path::Path::new("/home/u/.config/tui-wave/praat/pyenv/bin/python3");
        let plugin = std::path::Path::new("/plugin");

        let py = catalog
            .processes
            .iter()
            .find(|d| d.bin.starts_with("py/") && d.params.iter().all(|p| !matches!(p.kind, crate::model::cdp::ParamKind::CrystalVdat)))
            .expect("a py-group process");
        let values: Vec<ParamValue> = py.params.iter().map(|p| p.kind.default_value()).collect();
        let job = crate::model::praat::plan_praat_job_with(py, &values, plugin, Some(venv))
            .expect("py-group process plans");
        let rewrite = job.python_rewrite.expect("py-group process needs the rewrite");
        assert_eq!(rewrite.interpreter, venv.to_string_lossy());
        assert!(
            job.driver_source.contains(&format!("runScript: \"{}\"", rewrite.script_name)),
            "the driver must call the rewritten copy:\n{}",
            job.driver_source
        );

        // With no venv the script keeps its own discovery — someone with the packages on their
        // system interpreter should not be forced into a venv to keep working.
        let none = crate::model::praat::plan_praat_job_with(py, &values, plugin, None).unwrap();
        assert!(none.python_rewrite.is_none());

        let other = catalog
            .processes
            .iter()
            .find(|d| d.bin.starts_with("Reverb/"))
            .expect("a non-py process");
        let values: Vec<ParamValue> = other.params.iter().map(|p| p.kind.default_value()).collect();
        let job = crate::model::praat::plan_praat_job_with(other, &values, plugin, Some(venv))
            .expect("plans");
        assert!(job.python_rewrite.is_none(), "{} must not be rewritten", other.key);
    }
}

