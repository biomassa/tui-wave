//! Named, saved breakpoint-envelope shapes — one TOML file per preset under
//! `$XDG_CONFIG_HOME/tui-wave/envelope_presets/`, usable from *any* automatable Number
//! field's envelope editor (`CdpEnvelopeEdit`) regardless of which process/param it belongs
//! to — "system-wide," unlike a CDP process preset (`preset.rs`, scoped to one process key +
//! param count) or a CDP chain preset (`chain_preset.rs`, scoped to one whole chain). Mirrors
//! both those modules' discipline exactly: same `_in`-suffixed-core/XDG-wrapper split, same
//! never-fail-on-bad-persisted-state philosophy, same reason tests never touch
//! `XDG_CONFIG_HOME` directly.
//!
//! Stores raw `(time, value)` pairs verbatim, in whatever units the field they were drawn on
//! happened to use. Reusing a preset on a *different* field (a different `time_max`/
//! `min`/`max`) always rescales it on load (`App::rescale_preset_to_envelope`) — the same
//! "proportionally fit to the current time axis, clamp values to the current range"
//! treatment "use curve" already gives an external `PitchCurve`, so a shape saved from one
//! process's envelope still comes out sensible on a completely different one's.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvelopePreset {
    pub name: String,
    pub points: Vec<(f64, f64)>,
}

/// The directory presets are read from/written to:
/// `$XDG_CONFIG_HOME/tui-wave/envelope_presets/` (falling back to
/// `$HOME/.config/tui-wave/envelope_presets/`) — mirrors `chain_preset::chains_dir`.
pub fn presets_dir() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_home.join("tui-wave").join("envelope_presets")
}

/// Loads every saved preset, sorted by name. See [`list_presets_in`] for the
/// malformed-file handling; this just points it at [`presets_dir`].
pub fn list_presets() -> Vec<EnvelopePreset> {
    list_presets_in(&presets_dir())
}

/// Saves `preset`, overwriting any existing preset with the same name. See
/// [`save_preset_in`].
pub fn save_preset(preset: &EnvelopePreset) {
    save_preset_in(&presets_dir(), preset);
}

/// Deletes the preset named `name`, if it exists. See [`delete_preset_in`].
pub fn delete_preset(name: &str) {
    delete_preset_in(&presets_dir(), name);
}

/// Turns a preset name into a safe filename: anything that isn't alphanumeric, `-`, or `_`
/// becomes `_` — mirrors `chain_preset::sanitize_name` exactly (a preset name is free-typed
/// text, not already filesystem-safe like a process key). Two different names that sanitize
/// to the same filename will collide (last save wins); accepted as a rare-in-practice edge
/// case rather than adding a disambiguation scheme for it.
fn sanitize_name(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn preset_file_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.toml", sanitize_name(name)))
}

/// A missing directory yields an empty `Vec`. Each file is parsed independently: one
/// malformed preset file is skipped (not counted, not panicking), every other valid preset
/// still loads.
fn list_presets_in(dir: &Path) -> Vec<EnvelopePreset> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut presets: Vec<EnvelopePreset> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .filter_map(|text| toml::from_str::<EnvelopePreset>(&text).ok())
        .collect();
    presets.sort_by(|a, b| a.name.cmp(&b.name));
    presets
}

/// Best-effort: a write failure (read-only filesystem, missing permissions) is silently
/// ignored, matching `chain_preset::save_chain_in`.
fn save_preset_in(dir: &Path, preset: &EnvelopePreset) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(toml_string) = toml::to_string_pretty(preset) {
        let _ = std::fs::write(preset_file_path(dir, &preset.name), toml_string);
    }
}

/// Best-effort, same silent-failure philosophy as [`save_preset_in`]. A no-op (not an error)
/// if no preset by that name was ever saved.
fn delete_preset_in(dir: &Path, name: &str) {
    let _ = std::fs::remove_file(preset_file_path(dir, name));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, uniquely-named temp directory per test — passed directly to the `_in`
    /// functions, never through `XDG_CONFIG_HOME` (see this module's top-level doc comment).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tui_wave_envelope_preset_test_{tag}_{}_{:p}", std::process::id(), &tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn sample_preset(name: &str) -> EnvelopePreset {
        EnvelopePreset { name: name.into(), points: vec![(0.0, 0.0), (0.5, 100.0), (1.0, 20.0)] }
    }

    #[test]
    fn listing_with_no_directory_yet_returns_empty() {
        let dir = TempDir::new("unknown");
        std::fs::remove_dir_all(&dir.0).unwrap();
        assert!(list_presets_in(&dir.0).is_empty());
    }

    #[test]
    fn save_then_list_round_trips() {
        let dir = TempDir::new("roundtrip");
        let preset = sample_preset("Slow Swell");
        save_preset_in(&dir.0, &preset);

        let loaded = list_presets_in(&dir.0);
        assert_eq!(loaded, vec![preset]);
    }

    #[test]
    fn saving_a_preset_with_an_existing_name_overwrites_it() {
        let dir = TempDir::new("overwrite");
        save_preset_in(&dir.0, &sample_preset("Same Name"));
        let mut updated = sample_preset("Same Name");
        updated.points = vec![(0.0, 1.0), (1.0, 2.0)];
        save_preset_in(&dir.0, &updated);

        let loaded = list_presets_in(&dir.0);
        assert_eq!(loaded.len(), 1, "same name should overwrite, not create a second file");
        assert_eq!(loaded[0].points, vec![(0.0, 1.0), (1.0, 2.0)]);
    }

    #[test]
    fn preset_names_with_unsafe_characters_still_save_and_load() {
        let dir = TempDir::new("unsafe_name");
        let preset = sample_preset("Weird/Name: with * chars?");
        save_preset_in(&dir.0, &preset);

        let loaded = list_presets_in(&dir.0);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Weird/Name: with * chars?", "the original name is preserved inside the file");
    }

    #[test]
    fn delete_removes_only_the_named_preset() {
        let dir = TempDir::new("delete");
        save_preset_in(&dir.0, &sample_preset("Keep"));
        save_preset_in(&dir.0, &sample_preset("Remove"));

        delete_preset_in(&dir.0, "Remove");

        let loaded = list_presets_in(&dir.0);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Keep");
    }

    #[test]
    fn delete_on_a_never_saved_name_is_a_harmless_no_op() {
        let dir = TempDir::new("delete_noop");
        delete_preset_in(&dir.0, "never_saved");
        save_preset_in(&dir.0, &sample_preset("Keep"));
        delete_preset_in(&dir.0, "does not exist");
        assert_eq!(list_presets_in(&dir.0).len(), 1);
    }

    #[test]
    fn malformed_preset_file_is_skipped_others_still_load() {
        let dir = TempDir::new("malformed");
        save_preset_in(&dir.0, &sample_preset("Good"));
        std::fs::write(dir.0.join("bad.toml"), "not valid toml {{{").unwrap();

        let loaded = list_presets_in(&dir.0);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Good");
    }

    #[test]
    fn presets_are_sorted_by_name() {
        let dir = TempDir::new("sorted");
        save_preset_in(&dir.0, &sample_preset("Zebra"));
        save_preset_in(&dir.0, &sample_preset("Alpha"));
        let loaded = list_presets_in(&dir.0);
        assert_eq!(loaded.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Alpha", "Zebra"]);
    }
}
