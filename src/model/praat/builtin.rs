//! Praat scripts tui-wave ships itself, rather than running out of the praatAudioTools
//! submodule.
//!
//! ## Why any exist
//!
//! praatAudioTools records from the microphone in ten scripts — every `Vector Chain/Live_*` —
//! but never on its own: the capture is always the first stage of a fixed processing chain, so
//! there is no way to reach it without also getting a Neural Drone and a Crystalline Reverb.
//! The capture line itself is one statement, identical in all ten. Exposing it means shipping a
//! script, and shipping a script means owning it: an addition to the submodule would be
//! untracked working-tree litter that the next `git submodule update` discards.
//!
//! ## Why they are embedded rather than installed
//!
//! `include_str!`, not a path under an assets directory, because a packaged build has no such
//! directory: the AppImage, the `.deb` and the `.pkg.tar.zst` all ship a single binary. A script
//! read from disk at run time would work from a development checkout and fail everywhere else.
//! The source of truth stays the real `.praat` file in `assets/praat/` — editable, diffable and
//! runnable by hand against `praat --run` — and the compiler copies it in.
//!
//! The runner writes the text into the job's own temp directory beside the generated driver,
//! exactly as it already does for a pause-rewritten script (`praat::rewrite`), and the driver
//! calls it by bare filename. So this needs no new mechanism in the runner beyond the write
//! itself: a relative `runScript:` resolves against the calling script's folder, and the two
//! always sit in the same one.

/// Filename a built-in script is written under inside the job's temp directory.
///
/// Fixed and shared by every built-in, since a job runs exactly one process and its directory is
/// disposable. Deliberately not `process.praat` — that name belongs to a pause-rewritten copy,
/// and a job is never both.
pub const BUILTIN_SCRIPT: &str = "builtin.praat";

/// Catalog key of the Record process. Named here because three places have to agree on it — the
/// converter that emits the catalog entry, [`source_for`], and the tests — and a typo in any one
/// of them would surface only as a process that cannot run.
pub const RECORD_KEY: &str = "praat_generative_synthesis_tui_wave_record";

/// The Record script, compiled in from `assets/praat/record.praat`.
const RECORD_SOURCE: &str = include_str!("../../../assets/praat/record.praat");

/// Source text for a built-in process, or `None` for a process that is not one.
///
/// Keyed by catalog key rather than by `bin`, because the key is what the catalog guarantees is
/// unique — two entries could legitimately share a script.
pub fn source_for(key: &str) -> Option<&'static str> {
    match key {
        RECORD_KEY => Some(RECORD_SOURCE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `bin` the Record entry carries, as the converter emits it.
    ///
    /// Lives here rather than beside [`RECORD_KEY`] because nothing outside these tests reads
    /// it: the value is written by `scripts/convert_praat_audiotools.py` into the catalog, and
    /// production code only ever reads it back off the loaded `ProcessDef`. Declaring it in the
    /// module proper would be a constant no build but the test build uses.
    ///
    /// It is a path that does not exist in the submodule and is never resolved against it —
    /// `praat_builtin` is what says so. The directory segment is load-bearing all the same:
    /// `cdp_group` derives a Praat process's browser group from it, so this is what files Record
    /// under **Generative**, whose processes `App::praat_opens_new_buffer` already sends to a
    /// new buffer. Which is right for a recording: it is new material, not an edit of whatever
    /// happened to be selected.
    const RECORD_BIN: &str = "Generative & Synthesis/record.praat";

    #[test]
    fn the_record_script_is_embedded_and_declares_the_three_exposed_settings() {
        let source = source_for(RECORD_KEY).expect("Record must have embedded source");
        assert!(source.contains("form Record"));
        // The three form fields, in the order the catalog entry passes them.
        assert!(source.contains("positive Duration_seconds"));
        assert!(source.contains("word Sample_rate"));
        assert!(source.contains("positive Input_gain"));
    }

    /// The capture line is the one thing that has to stay exactly as praatAudioTools has it,
    /// device name included — "Microphone" is what Praat calls the system's selected input, and
    /// naming a real interface instead would pin the capture to hardware that may not be there.
    #[test]
    fn the_capture_line_keeps_the_upstream_device_and_balance() {
        let source = source_for(RECORD_KEY).unwrap();
        assert!(
            source.contains(
                "Record Sound (fixed time): \"Microphone\", input_gain, 0.5, sample_rate$, duration_seconds"
            ),
            "the capture line drifted from upstream's:\n{source}"
        );
    }

    #[test]
    fn an_unknown_key_has_no_builtin_source() {
        assert_eq!(source_for("praat_reverb_stereo_shimmer"), None);
        assert_eq!(source_for(""), None);
    }

    /// The bin's first segment is what `cdp_group` reads to file this under Generative.
    #[test]
    fn the_record_bin_sits_in_the_generative_directory() {
        assert_eq!(RECORD_BIN.split('/').next(), Some("Generative & Synthesis"));
    }

    /// Three separate places have to agree about Record — the converter that writes the catalog
    /// entry, the constants here, and the script's own `form` — and nothing but this test makes
    /// them. A converter re-run after a submodule bump regenerates `praat_catalog.toml`
    /// wholesale, so a built-in silently vanishing from it is the exact failure this guards.
    #[test]
    fn the_generated_catalog_carries_record_as_a_builtin_in_the_generative_group() {
        let (catalog, _) = crate::model::cdp::CdpCatalog::load(None);
        let def = catalog
            .processes
            .iter()
            .find(|p| p.key == RECORD_KEY)
            .expect("Record is missing from the catalog — did the converter drop BUILTINS?");

        assert_eq!(def.bin, RECORD_BIN);
        assert!(def.praat_builtin, "must not be resolved against the submodule");
        assert_eq!(def.backend(), crate::model::cdp::def::Backend::Praat);
        // Zero inputs: this is what lets it run with no document open.
        assert_eq!(def.input, crate::model::cdp::def::IoKind::None);
        assert_eq!(
            crate::model::cdp::cdp_group(def).map(|g| g.name),
            Some("Generative"),
            "the group is what routes the result to a new buffer"
        );

        // The catalog's parameters are what the driver passes positionally to the script's
        // `form`, so their order and count must match it exactly — Praat fills a form by
        // position and a mismatch is exit 255, not a warning.
        let names: Vec<&str> = def.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Duration_seconds_(0.1-3600)", "Sample_rate", "Input_gain_(0-1)"]);

        let source = source_for(RECORD_KEY).unwrap();
        let positions: Vec<usize> = names
            .iter()
            .map(|n| {
                source
                    .find(n)
                    .unwrap_or_else(|| panic!("{n} is not a form field of the script"))
            })
            .collect();
        // The bracketed ranges are part of the label Praat sees and the converter reads — a
        // finite bound may only ever come from a name that declares one.
        assert!(names[0].contains("(0.1-3600)") && names[2].contains("(0-1)"));
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "the catalog's parameter order does not match the script's form: {positions:?}"
        );
    }
}

