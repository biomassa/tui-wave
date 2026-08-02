//! Generating the Praat *driver script* — the small program that actually runs a
//! praatAudioTools process.
//!
//! ## Why a driver is needed at all
//!
//! CDP binaries take an input filename and an output filename on argv. praatAudioTools scripts
//! take neither: they operate on **the Sound object currently selected in Praat's object
//! list**, and leave their result there as another object. Invoking one directly fails with
//! `Error: Please select exactly one Sound object.` (exit 255). So every run is wrapped in a
//! generated script that loads the temp WAV, calls the plugin script through `runScript:`, and
//! saves whatever came back.
//!
//! ## Locating the result
//!
//! Only about 180 of the plugin's 416 scripts end on an explicit `selectObject: result`; the
//! rest end on `endproc`, `endif`, `Play` or an `appendInfoLine`, so what is selected when the
//! script returns is not a contract we can rely on. The driver instead does `select all` and
//! takes the **highest-numbered** Sound, which is Praat's most recently created one. Verified
//! across a 120-script sample: this locates the right object wherever the script left the
//! selection.
//!
//! Deliberately *not* an error when that turns out to be the input object itself. A script that
//! transforms the Sound in place (via `Formula...`) legitimately produces no new object, and
//! failing those would reject working processes. Catching a genuine silent no-op is the smoke
//! test's job — it can compare output bytes against input, which this script cannot.
//!
//! ## Pure
//!
//! Nothing here touches the filesystem or spawns anything; it returns script *text*. The paths
//! it is given are written into the script as literals, but the input and output paths are not
//! — those arrive as argv, filled into the driver's own `form` positionally by `--run`.

/// One argument passed through to the plugin script's `form`, in declaration order.
///
/// The split matters because Praat is typed at the call site: a `real`/`positive`/`integer`/
/// `natural` field must receive a bare numeric literal, and a `sentence`/`word`/`text` or
/// `optionmenu` field must receive a quoted string. Passing a quoted number to a numeric field
/// is an error, and so is passing a bare word to a text field.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverArg {
    Number(f64),
    /// Includes every `optionmenu`/`choice` selection: Praat matches those by their **option
    /// label**, never by index, so the catalog stores the labels verbatim and they travel as
    /// ordinary strings.
    Text(String),
}

/// Render a Rust string as a Praat string literal: wrapped in double quotes, with any embedded
/// double quote **doubled**. That doubling is Praat's only escape mechanism — there is no
/// backslash escaping in its string literals, so a `\` needs no special handling and passes
/// through unchanged (which is what makes Windows paths survive).
pub fn praat_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Render one numeric argument. Rust's `f64` `Display` never switches to scientific notation,
/// which matters because Praat's parser does not accept `1e-7`; it also drops a trailing `.0`,
/// so an integral value reaches an `integer`/`natural` field as `3` rather than `3.0`.
///
/// Non-finite values have no Praat spelling at all, so they are rejected rather than emitted as
/// the literal text `NaN`/`inf` (which Praat would read as an undefined *variable name* and
/// fail on with a confusing message far from the real cause).
fn praat_number_literal(value: f64) -> Result<String, DriverError> {
    if !value.is_finite() {
        return Err(DriverError::NonFiniteNumber(value));
    }
    Ok(format!("{value}"))
}

/// Why a driver script could not be rendered. Only one failure mode exists: a parameter value
/// that has no Praat spelling.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverError {
    NonFiniteNumber(f64),
}

/// The knobs on one generated driver, other than the script and its arguments.
///
/// A struct rather than two more positional parameters because `driver_script(p, &args, 1, true)`
/// says nothing about what the `true` selects, and both fields already need explaining.
/// [`Default`] is the pre-picture behaviour exactly — one input, no picture — which is what most
/// call sites and tests want.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DriverOptions {
    /// How many Sound objects the script expects selected. `0` is normalised to `1`.
    pub input_count: usize,
    /// Save Praat's Picture window to a third `outfile` path after the audio is written.
    pub save_picture: bool,
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriverError::NonFiniteNumber(v) => {
                write!(f, "parameter value {v} cannot be expressed in a Praat script")
            }
        }
    }
}

/// Build the driver script for one run.
///
/// `script_path` must be **absolute**. Relative paths resolve against the *calling script's*
/// folder for `runScript:` but against the process's working directory for `--run`, and the two
/// are different places here — the driver lives in the job's temp directory while the plugin
/// script lives in the submodule.
///
/// `options.input_count` is how many Sound objects the script expects to find selected. Almost
/// always 1; the handful of morph/concatenate/align scripts want 2, which they read as
/// `selected("Sound", 1)` and `selected("Sound", 2)` — i.e. by position within the selection.
/// Praat orders a selection by object number, and the driver reads the inputs in order, so
/// input 1 is always the first-read file.
///
/// ## The picture
///
/// `options.save_picture` appends two lines that hand back what the script drew. Around 290 of
/// the plugin's scripts have a `Draw_visualization`-style form boolean which, when on, paints a
/// multi-panel figure into Praat's **Picture window** — a window a headless `--run` never shows
/// and drops on exit, which is why the toggle used to do nothing observable. The picture is
/// still there when `runScript:` returns (no script in the checkout issues `Erase all` *after*
/// its last drawing command), so the driver can save it to a PNG the app decodes and displays.
///
/// Three details, each established empirically:
///
/// * **The viewport must be pinned first.** `Save as 300-dpi PNG file:` writes only the
///   currently-*selected* viewport, and a script leaves that wherever its last panel was — one
///   probe returned a 2396x331 footer strip instead of the whole figure. Selecting the full
///   12x12-inch canvas keeps everything; the surrounding white is cropped off in Rust, where it
///   is free. Not 8x8 (the suite's nominal canvas): real drawings were measured out to ~11.9
///   inches wide, and clipping a user's picture to save a crop is a bad trade.
/// * **Both lines are `nocheck`**, so a drawing that fails cannot fail the run.
/// * **They come after the audio is saved**, for the same reason from the other direction: by
///   the time anything here can go wrong, the result the user actually asked for is on disk.
pub fn driver_script(
    script_path: &str,
    args: &[DriverArg],
    options: DriverOptions,
) -> Result<String, DriverError> {
    let input_count = options.input_count.max(1);

    let mut call = format!("runScript: {}", praat_string_literal(script_path));
    for arg in args {
        let rendered = match arg {
            DriverArg::Number(v) => praat_number_literal(*v)?,
            DriverArg::Text(s) => praat_string_literal(s),
        };
        call.push_str(", ");
        call.push_str(&rendered);
    }

    let mut form = String::from("form Driver\n");
    for i in 1..=input_count {
        form.push_str(&format!("    infile Input_file_{i}\n"));
    }
    form.push_str("    outfile Output_file\n");
    if options.save_picture {
        form.push_str("    outfile Picture_file\n");
    }
    form.push_str("endform\n");

    let mut reads = String::new();
    for i in 1..=input_count {
        reads.push_str(&format!("snd{i} = Read from file: input_file_{i}$\n"));
    }
    let selection = (1..=input_count).map(|i| format!("snd{i}")).collect::<Vec<_>>().join(", ");

    let picture = if options.save_picture {
        "nocheck Select outer viewport: 0, 12, 0, 12\n\
         nocheck Save as 300-dpi PNG file: picture_file$\n"
    } else {
        ""
    };

    // `Save as 32-bit WAV file:` rather than the plain `Save as WAV file:`, which writes 16-bit.
    // Praat cannot write float at all (its WAV writer only ever emits PCM), so int32 is the
    // widest return leg available; `model::wavread` already reads it, including the
    // WAVE_FORMAT_EXTENSIBLE wrapper Praat puts around it.
    Ok(format!(
        "{form}\
         {reads}\
         selectObject: {selection}\n\
         {call}\n\
         select all\n\
         n = numberOfSelected(\"Sound\")\n\
         if n < 1\n\
         \x20   exitScript: \"praat script produced no Sound object\"\n\
         endif\n\
         last = selected(\"Sound\", n)\n\
         selectObject: last\n\
         Save as 32-bit WAV file: output_file$\n\
         {picture}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_string_is_wrapped_in_quotes() {
        assert_eq!(praat_string_literal("Soft Fold"), "\"Soft Fold\"");
    }

    /// Praat has no backslash escape; a quote is escaped by doubling it. Getting this wrong is
    /// what produced `Error: Unknown value ""Custom (use values below)""` during the research
    /// probe — the option label reached Praat with literal quote characters in it.
    #[test]
    fn an_embedded_quote_is_doubled_not_backslashed() {
        assert_eq!(praat_string_literal(r#"a "b" c"#), r#""a ""b"" c""#);
        assert!(!praat_string_literal(r#"a "b""#).contains('\\'));
    }

    /// Option labels routinely contain commas and parentheses, which must survive untouched —
    /// they are data, not argument separators, once inside a literal.
    #[test]
    fn punctuation_in_an_option_label_survives() {
        assert_eq!(
            praat_string_literal("Custom (use values below), v2"),
            "\"Custom (use values below), v2\""
        );
    }

    /// Backslashes pass through, so a Windows-style path is not mangled.
    #[test]
    fn a_backslash_is_not_an_escape() {
        assert_eq!(praat_string_literal(r"C:\tmp\x.wav"), "\"C:\\tmp\\x.wav\"");
    }

    #[test]
    fn an_integral_number_loses_its_trailing_zero() {
        assert_eq!(praat_number_literal(3.0).unwrap(), "3");
        assert_eq!(praat_number_literal(-1.0).unwrap(), "-1");
    }

    /// Praat's parser has no exponent form, so a tiny value must still be spelled out.
    #[test]
    fn a_tiny_number_is_not_written_in_scientific_notation() {
        let rendered = praat_number_literal(0.000_000_1).unwrap();
        assert!(!rendered.contains('e'), "{rendered} used exponent notation");
        assert_eq!(rendered, "0.0000001");
    }

    /// Compared by variant rather than by value: `NaN != NaN`, so an `assert_eq!` on the whole
    /// `Err` can never hold for the case that matters most here.
    #[test]
    fn a_non_finite_number_is_rejected_rather_than_emitted() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                matches!(praat_number_literal(value), Err(DriverError::NonFiniteNumber(_))),
                "{value} was not rejected"
            );
        }
    }

    #[test]
    fn a_no_argument_script_emits_a_bare_run_script_call() {
        let script = driver_script("/plugins/Reverb/Plate.praat", &[], DriverOptions::default()).unwrap();
        assert!(script.contains("runScript: \"/plugins/Reverb/Plate.praat\"\n"));
        assert!(!script.contains("runScript: \"/plugins/Reverb/Plate.praat\", "));
    }

    /// Numbers bare, strings quoted, in declaration order — the exact shape Praat's positional
    /// form filling expects.
    #[test]
    fn arguments_keep_their_order_and_their_types() {
        let script = driver_script(
            "/p/Distortion/Wavefolder.praat",
            &[
                DriverArg::Text("Custom (use settings below)".into()),
                DriverArg::Number(0.5),
                DriverArg::Number(1.0),
                DriverArg::Number(0.0),
            ],
            DriverOptions::default(),
        )
        .unwrap();
        assert!(script.contains(
            "runScript: \"/p/Distortion/Wavefolder.praat\", \
             \"Custom (use settings below)\", 0.5, 1, 0\n"
        ));
    }

    #[test]
    fn the_driver_reads_the_input_and_saves_the_output_as_int32() {
        let script = driver_script("/p/x.praat", &[], DriverOptions::default()).unwrap();
        assert!(script.contains("snd1 = Read from file: input_file_1$"));
        assert!(script.contains("Save as 32-bit WAV file: output_file$"));
        // 16-bit is what the unqualified command would give; make sure we never emit it.
        assert!(!script.contains("Save as WAV file:"));
    }

    /// The input and output paths ride in on argv rather than being baked into the script, so
    /// the driver text is independent of them.
    #[test]
    fn the_driver_takes_its_paths_from_a_form_not_from_literals() {
        let script = driver_script("/p/x.praat", &[], DriverOptions::default()).unwrap();
        assert!(script.starts_with("form Driver\n"));
        assert!(script.contains("    infile Input_file_1\n"));
        assert!(script.contains("    outfile Output_file\n"));
    }

    /// The result is located by object number, not by trusting the script's final selection.
    #[test]
    fn the_result_is_the_highest_numbered_sound() {
        let script = driver_script("/p/x.praat", &[], DriverOptions::default()).unwrap();
        assert!(script.contains("select all"));
        assert!(script.contains("n = numberOfSelected(\"Sound\")"));
        assert!(script.contains("last = selected(\"Sound\", n)"));
    }

    /// A script that produced nothing must fail loudly. Without this the driver would fall
    /// through to `Save as` with an arbitrary selection and write something unrelated.
    #[test]
    fn producing_no_sound_at_all_is_an_error() {
        let script = driver_script("/p/x.praat", &[], DriverOptions::default()).unwrap();
        assert!(script.contains("if n < 1"));
        assert!(script.contains("exitScript:"));
    }

    /// A two-Sound script reads its inputs as `selected("Sound", 1)` and `("Sound", 2)`, so both
    /// must be loaded *and selected together* before the call — selecting only the last one read
    /// would make the script see a single Sound and refuse.
    #[test]
    fn two_inputs_are_read_in_order_and_selected_together() {
        let script = driver_script("/p/Pitch/Contour_Transfer.praat", &[], DriverOptions { input_count: 2, ..Default::default() }).unwrap();
        assert!(script.contains("    infile Input_file_1\n"));
        assert!(script.contains("    infile Input_file_2\n"));
        assert!(script.contains("snd1 = Read from file: input_file_1$"));
        assert!(script.contains("snd2 = Read from file: input_file_2$"));
        assert!(script.contains("selectObject: snd1, snd2\n"));
        // Read order fixes selection order, since Praat orders a selection by object number.
        let first = script.find("snd1 = Read").unwrap();
        let second = script.find("snd2 = Read").unwrap();
        assert!(first < second, "inputs must be read in declaration order");
    }

    /// Zero is not a meaningful input count and must not produce a script that selects nothing.
    #[test]
    fn an_input_count_of_zero_is_treated_as_one() {
        let script = driver_script("/p/x.praat", &[], DriverOptions { input_count: 0, ..Default::default() }).unwrap();
        assert!(script.contains("selectObject: snd1\n"));
        assert!(!script.contains("selectObject: \n"));
    }

    /// The picture costs nothing when it is not asked for: no third form field, no save.
    #[test]
    fn no_picture_is_saved_by_default() {
        let script = driver_script("/p/x.praat", &[], DriverOptions::default()).unwrap();
        assert!(!script.contains("Picture_file"));
        assert!(!script.contains("PNG"));
    }

    /// The extra `outfile` and the extra argv entry must appear together — Praat fills a form
    /// strictly by position and count, so a field without an argument is `Found 2 arguments but
    /// expected more.` and an argument without a field is an equally opaque failure.
    #[test]
    fn saving_a_picture_adds_a_third_outfile_field_after_the_output() {
        let script =
            driver_script("/p/x.praat", &[], DriverOptions { input_count: 1, save_picture: true })
                .unwrap();
        let output = script.find("    outfile Output_file\n").unwrap();
        let picture = script.find("    outfile Picture_file\n").unwrap();
        assert!(output < picture, "Picture_file must be the last form field");
        assert!(script.find("endform").unwrap() > picture);
    }

    /// Both properties in one test because they are the same guarantee from two sides: whatever
    /// the drawing does, the audio the user actually asked for is already on disk and cannot be
    /// taken away by it.
    #[test]
    fn the_picture_is_saved_after_the_audio_and_cannot_fail_the_run() {
        let script =
            driver_script("/p/x.praat", &[], DriverOptions { input_count: 1, save_picture: true })
                .unwrap();
        let wav = script.find("Save as 32-bit WAV file:").unwrap();
        let png = script.find("Save as 300-dpi PNG file: picture_file$").unwrap();
        assert!(wav < png, "the audio must be written before the picture is attempted");
        for line in script.lines().filter(|l| l.contains("PNG") || l.contains("outer viewport")) {
            assert!(line.starts_with("nocheck "), "not nocheck-guarded: {line}");
        }
    }

    /// Praat's PNG save writes only the *selected* viewport, and a plugin script leaves that
    /// wherever its last panel was — without pinning the full canvas first, a probe got back a
    /// 2396x331 footer strip instead of the figure. 12x12 inches is the whole canvas; drawings
    /// were measured out to ~11.9 inches wide, so 8x8 would clip real ones.
    #[test]
    fn the_full_canvas_is_selected_before_the_picture_is_saved() {
        let script =
            driver_script("/p/x.praat", &[], DriverOptions { input_count: 1, save_picture: true })
                .unwrap();
        let viewport = script.find("Select outer viewport: 0, 12, 0, 12").unwrap();
        let png = script.find("Save as 300-dpi PNG file:").unwrap();
        assert!(viewport < png);
    }

    /// A non-finite value must not reach the script text.
    #[test]
    fn a_non_finite_argument_fails_the_whole_script() {
        let err = driver_script("/p/x.praat", &[DriverArg::Number(f64::NAN)], DriverOptions::default());
        assert!(err.is_err());
    }
}
