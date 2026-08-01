//! The `.headstails` sidecar file: where a document's Head/Tail marks
//! (`Document.head_tail_marks`) live on disk.
//!
//! Ordinary markers ride in the WAV itself, as `cue `/`adtl` chunks (`model::bwf`), because
//! that is what Audacity and Sound Forge read. Head/Tail marks deliberately do **not**: they
//! are a CDP-specific concept with no cue-chunk equivalent, and folding them into the same
//! chunk list would make them indistinguishable from ordinary markers to every other program
//! that opens the file — and to this one, on the next load.
//!
//! **The format is CDP's own marklist format**: one time in seconds per line, in increasing
//! order, alternating Head then Tail (the first is always a Head — see
//! `Document.head_tail_marks`). That is exactly what `distmore` wants handed to it, so the
//! file is directly usable outside this editor and trivially hand-editable. The cost is that
//! positions round-trip through seconds rather than staying sample-exact; `SECONDS_PRECISION`
//! is chosen so that round-trip is lossless at any sane sample rate (see its doc comment).
//!
//! Written on document save, next to the audio file, with the same stem
//! (`take1.wav` → `take1.headstails`). Read on load, if present. Both are best-effort in the
//! same spirit as `Config::save` and `preset::load_presets_in`: a malformed or unreadable
//! sidecar yields no marks rather than blocking the audio from opening, and a failed write is
//! swallowed rather than failing the save of the audio itself.

use std::path::{Path, PathBuf};

/// The file extension, without the dot.
pub const HEADSTAILS_EXTENSION: &str = "headstails";

/// Decimal places written per time value. Nine is enough that
/// `(position / rate)` → text → `(secs * rate).round()` returns the original sample index at
/// every rate in normal use: at 192 kHz one sample is ~5.2e-6 s, so nine places leaves three
/// orders of magnitude of headroom, and f64 itself carries ~15-16 significant digits, which
/// covers a 9-decimal value well past any real file duration.
const SECONDS_PRECISION: usize = 9;

/// The sidecar path for `audio_path`: same directory, same stem, `.headstails` extension.
pub fn sidecar_path(audio_path: impl AsRef<Path>) -> PathBuf {
    audio_path.as_ref().with_extension(HEADSTAILS_EXTENSION)
}

/// Converts sample positions to the text form written to disk. Split out from `save` so it
/// can be reused wherever CDP needs a marklist datafile built from the same marks.
pub fn marks_to_text(marks: &[usize], sample_rate: u32) -> String {
    let rate = sample_rate.max(1) as f64;
    let mut out = String::new();
    for &position in marks {
        out.push_str(&format!("{:.*}\n", SECONDS_PRECISION, position as f64 / rate));
    }
    out
}

/// Parses the text form back into sample positions.
///
/// Tolerant by design, because this file is meant to be hand-editable and to interoperate
/// with whatever else writes CDP marklists: blank lines are skipped, `#` comments are
/// skipped, surrounding whitespace is ignored, and a line that isn't a number is skipped
/// rather than failing the whole file. Negative times are dropped (there is no sample before
/// zero). The result is sorted and deduplicated so it satisfies `Document.head_tail_marks`'s
/// invariants no matter what order the file listed them in — out-of-order marks would
/// otherwise silently reassign every Head/Tail role after the first one.
pub fn marks_from_text(text: &str, sample_rate: u32) -> Vec<usize> {
    let rate = sample_rate.max(1) as f64;
    let mut marks: Vec<usize> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs >= 0.0)
        .map(|secs| (secs * rate).round() as usize)
        .collect();
    marks.sort_unstable();
    marks.dedup();
    marks
}

/// Loads the head/tail marks for `audio_path`, if a sidecar exists next to it. Returns an
/// empty `Vec` when there's no sidecar, it can't be read, or it holds nothing usable —
/// opening the audio must never be blocked by its sidecar.
pub fn load(audio_path: impl AsRef<Path>, sample_rate: u32) -> Vec<usize> {
    let Ok(text) = std::fs::read_to_string(sidecar_path(audio_path)) else {
        return Vec::new();
    };
    marks_from_text(&text, sample_rate)
}

/// Writes the sidecar for `audio_path`.
///
/// With no marks, an **existing** sidecar is removed rather than an empty file being written:
/// deleting every mark and saving should actually clear them, and leaving a zero-byte file
/// behind would mean the next load silently re-created nothing while the file still cluttered
/// the directory. Nothing is created when there's nothing to save, so a document that never
/// had head/tail marks never litters the user's folder with an empty file.
///
/// Best-effort: any I/O failure is swallowed. The audio save is the operation the user asked
/// for, and failing it over a sidecar would be the wrong trade.
pub fn save(audio_path: impl AsRef<Path>, marks: &[usize], sample_rate: u32) {
    let path = sidecar_path(audio_path);
    if marks.is_empty() {
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        return;
    }
    let _ = std::fs::write(&path, marks_to_text(marks, sample_rate));
}

/// Moves the sidecar alongside an audio file that has just been renamed.
///
/// Renaming moved only the audio, so `take1.headstails` stayed behind while the file it described
/// became `take2.wav` — the marks then silently vanished on the next load, and the stale sidecar
/// sat in the folder forever. Anything that renames audio has to move this too, which is why the
/// operation lives here beside the naming rule rather than at the call sites.
///
/// Best-effort, like [`save`]: a rename that already succeeded must not be reported as failed
/// over its sidecar. Does nothing when there is no sidecar to move.
pub fn rename_sidecar(old_audio: impl AsRef<Path>, new_audio: impl AsRef<Path>) {
    let old = sidecar_path(old_audio);
    let new = sidecar_path(new_audio);
    if old == new || !old.exists() {
        return;
    }
    let _ = std::fs::rename(&old, &new);
}

/// Deletes the sidecar belonging to an audio file that has just been deleted, so it does not
/// outlive the audio it describes.
pub fn remove_sidecar(audio_path: impl AsRef<Path>) {
    let path = sidecar_path(audio_path);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, uniquely-named temp directory per test — see `preset.rs`'s own tests for why
    /// tests in this codebase never reach for a shared path.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tui_wave_headstails_test_{tag}_{}_{:p}", std::process::id(), &tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn wav(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Renaming the audio must carry the sidecar with it, or the marks are silently lost on the
    /// next load and a stale sidecar is stranded in the folder.
    #[test]
    fn renaming_the_audio_moves_the_sidecar_with_it() {
        let dir = TempDir::new("rename");
        let old = dir.wav("take1.wav");
        let new = dir.wav("take2.wav");
        save(&old, &[48_000, 96_000], 48_000);
        assert!(sidecar_path(&old).exists());

        rename_sidecar(&old, &new);

        assert!(!sidecar_path(&old).exists(), "the old sidecar must not be left behind");
        assert_eq!(
            load(&new, 48_000),
            vec![48_000, 96_000],
            "and the marks must load from the new name"
        );
    }

    /// A rename with no sidecar to move is a no-op, not an error — most files have no marks.
    #[test]
    fn renaming_without_a_sidecar_does_nothing() {
        let dir = TempDir::new("rename_none");
        let old = dir.wav("take1.wav");
        let new = dir.wav("take2.wav");
        rename_sidecar(&old, &new);
        assert!(!sidecar_path(&new).exists());
    }

    /// Deleting the audio must take the sidecar too, so a later file with the same stem cannot
    /// inherit marks that were never its own.
    #[test]
    fn deleting_the_audio_removes_the_sidecar() {
        let dir = TempDir::new("delete");
        let audio = dir.wav("take.wav");
        save(&audio, &[1_000, 2_000], 48_000);
        assert!(sidecar_path(&audio).exists());

        remove_sidecar(&audio);

        assert!(!sidecar_path(&audio).exists());
        assert!(load(&audio, 48_000).is_empty(), "a fresh file of the same name starts clean");
    }

    #[test]
    fn the_sidecar_sits_next_to_the_audio_with_the_same_stem() {
        assert_eq!(
            sidecar_path("/takes/vocal take 1.wav"),
            PathBuf::from("/takes/vocal take 1.headstails")
        );
    }

    /// The written file must be CDP's own marklist format — plain seconds, one per line, in
    /// order — so `distmore` can read it directly and a user can edit it by hand.
    #[test]
    fn the_written_format_is_one_plain_time_in_seconds_per_line() {
        let text = marks_to_text(&[0, 22050, 44100], 44100);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].parse::<f64>().unwrap(), 0.0);
        assert_eq!(lines[1].parse::<f64>().unwrap(), 0.5);
        assert_eq!(lines[2].parse::<f64>().unwrap(), 1.0);
        assert!(text.ends_with('\n'), "a trailing newline, as text files have");
    }

    /// The whole point of `SECONDS_PRECISION`: positions must come back *exactly*, not
    /// approximately, or marks would creep by a sample on every save/load cycle.
    #[test]
    fn sample_positions_round_trip_exactly_at_every_common_rate() {
        for rate in [8_000, 22_050, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000] {
            let marks = vec![1, 2, 3, 7, 4_999, 100_001, rate as usize * 600 + 1];
            let text = marks_to_text(&marks, rate);
            assert_eq!(marks_from_text(&text, rate), marks, "rate {rate}");
        }
    }

    #[test]
    fn save_then_load_round_trips_through_a_real_file() {
        let dir = TempDir::new("roundtrip");
        let wav = dir.wav("take.wav");
        let marks = vec![4410, 8820, 22050, 30870];
        save(&wav, &marks, 44100);
        assert!(sidecar_path(&wav).exists());
        assert_eq!(load(&wav, 44100), marks);
    }

    #[test]
    fn loading_with_no_sidecar_yields_no_marks() {
        let dir = TempDir::new("absent");
        assert!(load(dir.wav("nothing.wav"), 44100).is_empty());
    }

    /// Hand-editable means hand-breakable: junk lines, comments and blanks are skipped rather
    /// than taking the whole file down with them.
    #[test]
    fn a_malformed_file_yields_whatever_was_parseable_not_a_failure() {
        let dir = TempDir::new("malformed");
        let wav = dir.wav("take.wav");
        std::fs::write(
            sidecar_path(&wav),
            "# head/tail marks\n0.1\n\n  0.2  \nnot a number\n-5.0\n0.3\n",
        )
        .unwrap();
        assert_eq!(load(&wav, 10_000), vec![1000, 2000, 3000]);
    }

    /// Roles are derived from list order, so an out-of-order hand-edited file must be sorted
    /// on the way in — otherwise every Head/Tail role after the first would silently flip.
    #[test]
    fn out_of_order_and_duplicate_lines_are_normalized_on_load() {
        assert_eq!(marks_from_text("0.3\n0.1\n0.2\n0.1\n", 10_000), vec![1000, 2000, 3000]);
    }

    /// Deleting every mark and saving must actually clear the sidecar, not leave a stale file
    /// that the next load would keep finding.
    #[test]
    fn saving_with_no_marks_removes_an_existing_sidecar() {
        let dir = TempDir::new("clearing");
        let wav = dir.wav("take.wav");
        save(&wav, &[100, 200], 44100);
        assert!(sidecar_path(&wav).exists());
        save(&wav, &[], 44100);
        assert!(!sidecar_path(&wav).exists(), "the stale sidecar is gone");
        assert!(load(&wav, 44100).is_empty());
    }

    /// ...but a document that never had head/tail marks must not litter the user's folder
    /// with an empty file every time it's saved.
    #[test]
    fn saving_with_no_marks_and_no_existing_sidecar_creates_nothing() {
        let dir = TempDir::new("no_litter");
        let wav = dir.wav("take.wav");
        save(&wav, &[], 44100);
        assert!(!sidecar_path(&wav).exists());
    }
}
