//! Named, saved parameter sets ("presets") for CDP processes — one TOML file per process
//! key under `$XDG_CONFIG_HOME/tui-wave/cdp_presets/`, loaded/saved/deleted from the params
//! dialog's preset row (`Dialog::CdpParams`). Pure data + file I/O, no UI; mirrors
//! `CdpCatalog::user_dir`'s directory-resolution and `Config::save`'s
//! never-fail-on-bad-persisted-state philosophy throughout.
//!
//! Every public function is a thin XDG-path-resolving wrapper around a `_in`-suffixed core
//! that takes the presets directory explicitly — the same split `Config::backup_existing`/
//! `backup_path` uses, and for the same reason: `std::env::set_var("XDG_CONFIG_HOME", ..)`
//! is process-global, so a test that mutates it races every other test mutating it
//! concurrently in the same test binary. Taking the directory as a plain argument sidesteps
//! that entirely — tests pass their own temp dir directly, no env var involved.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::def::ParamValue;

/// One saved parameter set for a specific process. `values` is index-parallel to that
/// process's `ProcessDef.params` at save time — a later catalog edit that adds/removes a
/// param invalidates a saved preset, which `load_presets` detects (by length mismatch) and
/// silently drops rather than risk applying values to the wrong params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CdpPreset {
    pub name: String,
    pub values: Vec<ParamValue>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct CdpPresetFile {
    #[serde(default)]
    preset: Vec<CdpPreset>,
}

/// The directory presets are read from/written to: `$XDG_CONFIG_HOME/tui-wave/cdp_presets/`
/// (falling back to `$HOME/.config/tui-wave/cdp_presets/`) — mirrors `CdpCatalog::user_dir`.
pub fn presets_dir() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_home.join("tui-wave").join("cdp_presets")
}

/// Loads all saved presets for `process_key`, sorted by name. See [`load_presets_in`] for
/// the malformed-file/mismatched-shape handling; this just points it at [`presets_dir`].
pub fn load_presets(process_key: &str, expected_param_count: usize) -> Vec<CdpPreset> {
    load_presets_in(&presets_dir(), process_key, expected_param_count)
}

/// Saves `preset` for `process_key`, overwriting any existing preset with the same name or
/// appending a new one. See [`save_preset_in`].
pub fn save_preset(process_key: &str, preset: CdpPreset) {
    save_preset_in(&presets_dir(), process_key, preset);
}

/// Deletes the preset named `name` for `process_key`, if it exists. See
/// [`delete_preset_in`].
pub fn delete_preset(process_key: &str, name: &str) {
    delete_preset_in(&presets_dir(), process_key, name);
}

fn preset_file_path(dir: &Path, process_key: &str) -> PathBuf {
    dir.join(format!("{process_key}.toml"))
}

/// A missing or malformed file yields an empty `Vec` (never blocks opening the params
/// dialog over a corrupt preset file) — a preset whose value count no longer matches
/// `expected_param_count` (the catalog def changed shape, or the file was hand-edited
/// badly) is dropped individually rather than discarding the whole file, so one bad entry
/// doesn't take every other saved preset for that process down with it.
fn load_presets_in(dir: &Path, process_key: &str, expected_param_count: usize) -> Vec<CdpPreset> {
    let path = preset_file_path(dir, process_key);
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let Ok(file) = toml::from_str::<CdpPresetFile>(&text) else { return Vec::new() };
    let mut presets: Vec<CdpPreset> =
        file.preset.into_iter().filter(|p| p.values.len() == expected_param_count).collect();
    presets.sort_by(|a, b| a.name.cmp(&b.name));
    presets
}

/// Best-effort: a write failure (read-only filesystem, missing permissions) is silently
/// ignored, matching `Config::save`.
fn save_preset_in(dir: &Path, process_key: &str, preset: CdpPreset) {
    let path = preset_file_path(dir, process_key);
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let mut file = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| toml::from_str::<CdpPresetFile>(&text).ok())
        .unwrap_or_default();
    match file.preset.iter_mut().find(|p| p.name == preset.name) {
        Some(existing) => *existing = preset,
        None => file.preset.push(preset),
    }
    if let Ok(toml_string) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml_string);
    }
}

/// Best-effort, same silent-failure philosophy as [`save_preset_in`]. A no-op (not an
/// error) if the process has no preset file, or no preset by that name.
fn delete_preset_in(dir: &Path, process_key: &str, name: &str) {
    let path = preset_file_path(dir, process_key);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(mut file) = toml::from_str::<CdpPresetFile>(&text) else { return };
    file.preset.retain(|p| p.name != name);
    if let Ok(toml_string) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, uniquely-named temp directory per test — passed directly to the `_in`
    /// functions, never through `XDG_CONFIG_HOME`, so parallel test threads can't race each
    /// other over a shared process-global env var (see this module's top-level doc comment).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tui_wave_cdp_preset_test_{tag}_{}_{:p}", std::process::id(), &tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn loading_presets_for_an_unknown_process_returns_empty() {
        let dir = TempDir::new("unknown");
        assert!(load_presets_in(&dir.0, "nonexistent_process", 3).is_empty());
    }

    #[test]
    fn save_then_load_round_trips_every_param_value_variant() {
        let dir = TempDir::new("roundtrip");
        let preset = CdpPreset {
            name: "My Preset".into(),
            values: vec![
                ParamValue::Number(1.5),
                ParamValue::Toggle(true),
                ParamValue::Choice(2),
                ParamValue::Breakpoints(vec![(0.0, 0.0), (0.5, 10.0), (1.0, 0.0)]),
            ],
        };
        save_preset_in(&dir.0, "test_process", preset.clone());

        let loaded = load_presets_in(&dir.0, "test_process", 4);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], preset);
    }

    #[test]
    fn saving_a_preset_with_an_existing_name_overwrites_it() {
        let dir = TempDir::new("overwrite");
        save_preset_in(&dir.0, "proc", CdpPreset { name: "A".into(), values: vec![ParamValue::Number(1.0)] });
        save_preset_in(&dir.0, "proc", CdpPreset { name: "A".into(), values: vec![ParamValue::Number(2.0)] });

        let loaded = load_presets_in(&dir.0, "proc", 1);
        assert_eq!(loaded.len(), 1, "same name should overwrite, not append");
        assert_eq!(loaded[0].values, vec![ParamValue::Number(2.0)]);
    }

    #[test]
    fn presets_are_scoped_per_process() {
        let dir = TempDir::new("scoped");
        save_preset_in(&dir.0, "proc_a", CdpPreset { name: "Shared Name".into(), values: vec![ParamValue::Number(1.0)] });
        save_preset_in(&dir.0, "proc_b", CdpPreset { name: "Shared Name".into(), values: vec![ParamValue::Number(2.0)] });

        assert_eq!(load_presets_in(&dir.0, "proc_a", 1)[0].values, vec![ParamValue::Number(1.0)]);
        assert_eq!(load_presets_in(&dir.0, "proc_b", 1)[0].values, vec![ParamValue::Number(2.0)]);
    }

    #[test]
    fn delete_removes_only_the_named_preset() {
        let dir = TempDir::new("delete");
        save_preset_in(&dir.0, "proc", CdpPreset { name: "Keep".into(), values: vec![ParamValue::Number(1.0)] });
        save_preset_in(&dir.0, "proc", CdpPreset { name: "Remove".into(), values: vec![ParamValue::Number(2.0)] });

        delete_preset_in(&dir.0, "proc", "Remove");

        let loaded = load_presets_in(&dir.0, "proc", 1);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Keep");
    }

    #[test]
    fn delete_on_missing_file_or_name_is_a_harmless_no_op() {
        let dir = TempDir::new("delete_noop");
        delete_preset_in(&dir.0, "never_saved", "whatever"); // no file yet — must not panic
        save_preset_in(&dir.0, "proc", CdpPreset { name: "Keep".into(), values: vec![ParamValue::Number(1.0)] });
        delete_preset_in(&dir.0, "proc", "does not exist");
        assert_eq!(load_presets_in(&dir.0, "proc", 1).len(), 1);
    }

    /// A preset whose value count no longer matches the (possibly-changed) process
    /// definition is dropped, but every other valid preset in the same file still loads.
    #[test]
    fn mismatched_param_count_preset_is_dropped_others_still_load() {
        let dir = TempDir::new("mismatch");
        save_preset_in(&dir.0, "proc", CdpPreset { name: "Stale".into(), values: vec![ParamValue::Number(1.0)] });
        save_preset_in(
            &dir.0,
            "proc",
            CdpPreset { name: "Current".into(), values: vec![ParamValue::Number(1.0), ParamValue::Number(2.0)] },
        );

        let loaded = load_presets_in(&dir.0, "proc", 2);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Current");
    }

    #[test]
    fn malformed_preset_file_yields_empty_not_a_panic() {
        let dir = TempDir::new("malformed");
        std::fs::write(dir.0.join("proc.toml"), "not valid toml {{{").unwrap();
        assert!(load_presets_in(&dir.0, "proc", 1).is_empty());
    }

    /// `ParamValue::FormantBufferRef` is the only *unit* variant in the externally-tagged
    /// `ParamValue` enum — serde serializes it as a bare string, landing in the same TOML
    /// array as the other variants' inline tables (a heterogeneous array). Verified the
    /// `toml` crate accepts that mix on both ends; this pins it against a future
    /// toml-crate/serde change.
    #[test]
    fn formant_buffer_ref_value_round_trips_through_a_preset_file() {
        let dir = TempDir::new("formant_ref");
        let preset = CdpPreset {
            name: "probe".into(),
            values: vec![ParamValue::FormantBufferRef, ParamValue::Number(2.0), ParamValue::Toggle(true)],
        };
        save_preset_in(&dir.0, "probe_proc", preset.clone());
        let loaded = load_presets_in(&dir.0, "probe_proc", 3);
        assert_eq!(loaded, vec![preset], "FormantBufferRef unit variant must round-trip through a preset file");
    }
}
