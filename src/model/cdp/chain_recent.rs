//! The 10 most recently *run* CDP chains, most-recent-first — mirrors `model::cdp::recent`
//! exactly, just tracking chain names instead of process catalog keys. Persisted to
//! `$XDG_CONFIG_HOME/tui-wave/cdp_recent_chains.toml`, a single ordered list (there's only
//! ever one such list, so — like `recent.rs` and unlike `chain_preset.rs`'s per-chain
//! sharding — one shared file is enough). Preview does not count as "run"; only a
//! completed, spliced chain run calls `record_used`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Recent entries beyond this are dropped — same cap and rationale as `recent::MAX_RECENT`.
const MAX_RECENT: usize = 10;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct CdpRecentChainsFile {
    #[serde(default)]
    name: Vec<String>,
}

/// The recent-chains file's path: `$XDG_CONFIG_HOME/tui-wave/cdp_recent_chains.toml`
/// (falling back to `$HOME/.config/tui-wave/cdp_recent_chains.toml`).
fn recent_file_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_home.join("tui-wave").join("cdp_recent_chains.toml")
}

/// Loads the recent-chains list, most-recent-first. A missing or malformed file yields an
/// empty `Vec` (never blocks opening the chain editor over a corrupt/absent file).
pub fn load_recent() -> Vec<String> {
    load_recent_in(&recent_file_path())
}

/// Records `name` as just-run: moves it to the front if already present (no duplicate
/// entries), otherwise inserts it at the front, then truncates to `MAX_RECENT` and saves.
pub fn record_used(name: &str) {
    record_used_in(&recent_file_path(), name);
}

fn load_recent_in(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(file) = toml::from_str::<CdpRecentChainsFile>(&text) else { return Vec::new() };
    file.name
}

fn record_used_in(path: &Path, name: &str) {
    let mut names = load_recent_in(path);
    names.retain(|n| n != name);
    names.insert(0, name.to_string());
    names.truncate(MAX_RECENT);

    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(toml_string) = toml::to_string_pretty(&CdpRecentChainsFile { name: names }) {
        let _ = std::fs::write(path, toml_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, uniquely-named temp file path per test — never through `XDG_CONFIG_HOME`
    /// (see `recent.rs`'s own tests for why).
    struct TempFile(PathBuf);
    impl TempFile {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tui_wave_cdp_recent_chains_test_{tag}_{}_{:p}", std::process::id(), &tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.join("cdp_recent_chains.toml"))
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
        record_used_in(&f.0, "Vocoder Setup");
        record_used_in(&f.0, "Speech Cleanup");
        assert_eq!(load_recent_in(&f.0), vec!["Speech Cleanup", "Vocoder Setup"]);
    }

    #[test]
    fn reusing_an_already_recent_name_moves_it_to_the_front_without_duplicating() {
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
            record_used_in(&f.0, &format!("chain_{i}"));
        }
        let loaded = load_recent_in(&f.0);
        assert_eq!(loaded.len(), 10);
        assert_eq!(loaded[0], "chain_11", "most recent should be first");
        assert_eq!(loaded[9], "chain_2", "the two oldest (chain_0, chain_1) should be dropped");
    }

    #[test]
    fn malformed_file_yields_empty_not_a_panic() {
        let f = TempFile::new("malformed");
        std::fs::write(&f.0, "not valid toml {{{").unwrap();
        assert!(load_recent_in(&f.0).is_empty());
    }
}
