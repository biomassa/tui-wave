//! The 10 most recently *applied* CDP processes, most-recent-first — backs the "Recent"
//! group in the process browser (`Dialog::CdpBrowser`, see CDP-Ext-Plan.md Phase 7).
//! Persisted to `$XDG_CONFIG_HOME/tui-wave/cdp_recent.toml`, a single ordered list of
//! catalog keys (unlike `model::cdp::preset`, which is one file per process — there's only
//! ever one Recent list, not one per process). Preview does not count as "used"; only a
//! successful Apply calls `record_used`.
//!
//! Same `_in`-suffixed-core-plus-XDG-wrapper split as `preset.rs`, and for the identical
//! reason: `std::env::set_var("XDG_CONFIG_HOME", ..)` is process-global, so a test that
//! mutates it races every other test doing the same in the same parallel test binary.
//! Tests call the `_in` variant directly with their own temp dir, never touching the env
//! var at all.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Recent entries beyond this are dropped — keeps the list short enough to scan at a
/// glance, per the feature's own name.
const MAX_RECENT: usize = 10;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct CdpRecentFile {
    #[serde(default)]
    key: Vec<String>,
}

/// The directory the recent-processes file lives in:
/// `$XDG_CONFIG_HOME/tui-wave/` (falling back to `$HOME/.config/tui-wave/`) — mirrors
/// `CdpCatalog::user_dir`/`model::cdp::preset::presets_dir`, minus their extra
/// subdirectory since there's only ever the one file here.
fn recent_file_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_home.join("tui-wave").join("cdp_recent.toml")
}

/// Loads the recent-processes list, most-recent-first. A missing or malformed file yields
/// an empty `Vec` (never blocks opening the browser over a corrupt/absent file).
pub fn load_recent() -> Vec<String> {
    load_recent_in(&recent_file_path())
}

/// Records `key` as just-used: moves it to the front if already present (no duplicate
/// entries), otherwise inserts it at the front, then truncates to `MAX_RECENT` and saves.
pub fn record_used(key: &str) {
    record_used_in(&recent_file_path(), key);
}

fn load_recent_in(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(file) = toml::from_str::<CdpRecentFile>(&text) else { return Vec::new() };
    file.key
}

fn record_used_in(path: &Path, key: &str) {
    let mut keys = load_recent_in(path);
    keys.retain(|k| k != key);
    keys.insert(0, key.to_string());
    keys.truncate(MAX_RECENT);

    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(toml_string) = toml::to_string_pretty(&CdpRecentFile { key: keys }) {
        let _ = std::fs::write(path, toml_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, uniquely-named temp file path per test — passed directly to the `_in`
    /// functions, never through `XDG_CONFIG_HOME`, so parallel test threads can't race each
    /// other over a shared process-global env var (see this module's top-level doc comment).
    struct TempFile(PathBuf);
    impl TempFile {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tui_wave_cdp_recent_test_{tag}_{}_{:p}", std::process::id(), &tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.join("cdp_recent.toml"))
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                std::fs::remove_dir_all(dir).ok();
            }
        }
    }

    #[test]
    fn loading_with_no_file_yet_returns_empty() {
        let f = TempFile::new("unknown");
        assert!(load_recent_in(&f.0).is_empty());
    }

    #[test]
    fn recording_a_use_inserts_at_the_front() {
        let f = TempFile::new("insert");
        record_used_in(&f.0, "blur_avrg");
        record_used_in(&f.0, "modify_speed_2");
        assert_eq!(load_recent_in(&f.0), vec!["modify_speed_2", "blur_avrg"]);
    }

    #[test]
    fn reusing_an_already_recent_key_moves_it_to_the_front_without_duplicating() {
        let f = TempFile::new("dedupe");
        record_used_in(&f.0, "a");
        record_used_in(&f.0, "b");
        record_used_in(&f.0, "c");
        record_used_in(&f.0, "a");
        assert_eq!(load_recent_in(&f.0), vec!["a", "c", "b"]);
    }

    #[test]
    fn list_is_capped_at_ten_dropping_the_oldest() {
        let f = TempFile::new("cap");
        for i in 0..12 {
            record_used_in(&f.0, &format!("proc_{i}"));
        }
        let loaded = load_recent_in(&f.0);
        assert_eq!(loaded.len(), 10);
        assert_eq!(loaded[0], "proc_11", "most recent should be first");
        assert_eq!(loaded[9], "proc_2", "the two oldest (proc_0, proc_1) should be dropped");
    }

    #[test]
    fn malformed_file_yields_empty_not_a_panic() {
        let f = TempFile::new("malformed");
        std::fs::write(&f.0, "not valid toml {{{").unwrap();
        assert!(load_recent_in(&f.0).is_empty());
    }
}
