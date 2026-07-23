//! The single most-recently successfully-*applied* CDP process (a single "CDP Process..."
//! run, not a chain — see `chain_last.rs` for that) — auto-saved every time a process
//! actually splices (Apply, not Preview), recalled via the browser's own `Ctrl+L` ("Recall
//! last process"). Mirrors `chain_last.rs` exactly: same `_in`-suffixed-core/XDG-wrapper
//! split, same never-fail-on-bad-persisted-state philosophy, same reason tests never touch
//! `XDG_CONFIG_HOME` directly, same single dedicated slot (not one of the named presets in
//! `preset.rs`, so it never clutters that per-process preset list or its own cycling) —
//! recalling always leaves the exact process+parameters last applied recoverable, even if
//! the user forgot to save a preset before running it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::def::ParamValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastProcess {
    pub process_key: String,
    pub values: Vec<ParamValue>,
}

/// The path the last-applied process is saved to:
/// `$XDG_CONFIG_HOME/tui-wave/cdp_last_process.toml` (falling back to
/// `$HOME/.config/tui-wave/cdp_last_process.toml`) — a single file, a sibling of
/// `preset::presets_dir()` and `chain_preset::chains_dir()` rather than a member of either,
/// so neither's directory scan ever picks it up.
fn last_process_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_home.join("tui-wave").join("cdp_last_process.toml")
}

/// Loads the last successfully-applied process, if any has ever been run (or the file is
/// missing/malformed — never blocks opening the browser over a corrupt/absent file).
pub fn load_last_process() -> Option<LastProcess> {
    load_last_process_in(&last_process_path())
}

/// Overwrites the last-applied-process slot with `process`. Best-effort: a write failure
/// (read-only filesystem, missing permissions) is silently ignored, matching every other
/// persistence module in this family.
pub fn save_last_process(process: &LastProcess) {
    save_last_process_in(&last_process_path(), process);
}

fn load_last_process_in(path: &Path) -> Option<LastProcess> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

fn save_last_process_in(path: &Path, process: &LastProcess) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(toml_string) = toml::to_string_pretty(process) {
        let _ = std::fs::write(path, toml_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, uniquely-named temp file path per test — never through `XDG_CONFIG_HOME`
    /// (see `chain_last.rs`'s own tests for why).
    struct TempFile(PathBuf);
    impl TempFile {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tui_wave_cdp_last_process_test_{tag}_{}_{:p}", std::process::id(), &tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.join("cdp_last_process.toml"))
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                std::fs::remove_dir_all(dir).ok();
            }
        }
    }

    fn sample_process(key: &str) -> LastProcess {
        LastProcess { process_key: key.into(), values: vec![ParamValue::Number(4.0)] }
    }

    #[test]
    fn loading_with_no_file_yet_returns_none() {
        let f = TempFile::new("unknown");
        assert!(load_last_process_in(&f.0).is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let f = TempFile::new("roundtrip");
        let process = sample_process("blur_avrg");
        save_last_process_in(&f.0, &process);
        assert_eq!(load_last_process_in(&f.0), Some(process));
    }

    #[test]
    fn saving_again_overwrites_the_previous_last_process() {
        let f = TempFile::new("overwrite");
        save_last_process_in(&f.0, &sample_process("blur_avrg"));
        save_last_process_in(&f.0, &sample_process("phase_phase_1"));
        assert_eq!(load_last_process_in(&f.0).map(|p| p.process_key), Some("phase_phase_1".to_string()));
    }

    #[test]
    fn malformed_file_yields_none_not_a_panic() {
        let f = TempFile::new("malformed");
        std::fs::write(&f.0, "not valid toml {{{").unwrap();
        assert!(load_last_process_in(&f.0).is_none());
    }
}
