//! The single most-recently *successfully run* (Applied, not Previewed) CDP chain —
//! auto-saved every time a chain run actually splices (`App::finish_chain_run`), recalled via
//! the chain editor's own `l` key ("Recall last chain"). Distinct from `chain_preset.rs`'s
//! named, user-saved chains: this is one dedicated slot, not scanned by
//! `chain_preset::list_chains` and never shown in the named-preset cycling list, specifically
//! so a chain the user forgot to save before running it is never lost for good — running a
//! chain always leaves *something* recoverable, saving under a name is just for keeping it
//! around longer than "whatever I ran most recently."
//!
//! Same `_in`-suffixed-core/XDG-wrapper split as `chain_preset.rs`/`chain_recent.rs`, for the
//! identical reason (tests never touch `XDG_CONFIG_HOME` directly).

use std::path::{Path, PathBuf};

use super::chain::CdpChain;

/// The path the last-run chain is saved to: `$XDG_CONFIG_HOME/tui-wave/cdp_last_chain.toml`
/// (falling back to `$HOME/.config/tui-wave/cdp_last_chain.toml`) — a single file, a sibling
/// of `chain_preset::chains_dir()` rather than a member of it, so `chain_preset::list_chains`'s
/// directory scan never picks it up.
fn last_chain_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_home.join("tui-wave").join("cdp_last_chain.toml")
}

/// Loads the last successfully-run chain, if any has ever been run (or the file is
/// missing/malformed — never blocks opening the chain editor over a corrupt/absent file).
pub fn load_last_chain() -> Option<CdpChain> {
    load_last_chain_in(&last_chain_path())
}

/// Overwrites the last-run-chain slot with `chain`. Best-effort: a write failure (read-only
/// filesystem, missing permissions) is silently ignored, matching every other persistence
/// module in this family.
pub fn save_last_chain(chain: &CdpChain) {
    save_last_chain_in(&last_chain_path(), chain);
}

fn load_last_chain_in(path: &Path) -> Option<CdpChain> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

fn save_last_chain_in(path: &Path, chain: &CdpChain) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(toml_string) = toml::to_string_pretty(chain) {
        let _ = std::fs::write(path, toml_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::chain::ChainStep;
    use crate::model::cdp::def::ParamValue;

    /// A fresh, uniquely-named temp file path per test — never through `XDG_CONFIG_HOME`
    /// (see `chain_recent.rs`'s own tests for why).
    struct TempFile(PathBuf);
    impl TempFile {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tui_wave_cdp_last_chain_test_{tag}_{}_{:p}", std::process::id(), &tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.join("cdp_last_chain.toml"))
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                std::fs::remove_dir_all(dir).ok();
            }
        }
    }

    fn sample_chain(name: &str) -> CdpChain {
        CdpChain {
            name: name.into(),
            steps: vec![ChainStep { process_key: "blur_avrg".into(), values: vec![ParamValue::Number(4.0)], side_chain: Vec::new() }],
        }
    }

    #[test]
    fn loading_with_no_file_yet_returns_none() {
        let f = TempFile::new("unknown");
        assert!(load_last_chain_in(&f.0).is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let f = TempFile::new("roundtrip");
        let chain = sample_chain("My Chain");
        save_last_chain_in(&f.0, &chain);
        assert_eq!(load_last_chain_in(&f.0), Some(chain));
    }

    #[test]
    fn saving_again_overwrites_the_previous_last_chain() {
        let f = TempFile::new("overwrite");
        save_last_chain_in(&f.0, &sample_chain("First"));
        save_last_chain_in(&f.0, &sample_chain("Second"));
        assert_eq!(load_last_chain_in(&f.0).map(|c| c.name), Some("Second".to_string()));
    }

    #[test]
    fn malformed_file_yields_none_not_a_panic() {
        let f = TempFile::new("malformed");
        std::fs::write(&f.0, "not valid toml {{{").unwrap();
        assert!(load_last_chain_in(&f.0).is_none());
    }
}
