//! Named, saved CDP chains — one TOML file per chain under
//! `$XDG_CONFIG_HOME/tui-wave/cdp_chains/`, loaded/saved/deleted from the chain editor
//! dialog. Pure data + file I/O, no UI; mirrors `model::cdp::preset`'s discipline exactly,
//! just keyed by chain name instead of process key — see that module's own doc comment for
//! the full rationale (same `_in`-suffixed-core/XDG-wrapper split, same
//! never-fail-on-bad-persisted-state philosophy, same reason tests never touch
//! `XDG_CONFIG_HOME` directly).
//!
//! One file per chain (not one shared file, unlike `recent.rs`) because a chain — especially
//! one with populated side-chains — can grow large enough that sharding by name, the same
//! way `preset.rs` shards by process key, is worth it.

use std::path::{Path, PathBuf};

use super::chain::CdpChain;

/// The directory chains are read from/written to:
/// `$XDG_CONFIG_HOME/tui-wave/cdp_chains/` (falling back to `$HOME/.config/tui-wave/cdp_chains/`)
/// — mirrors `preset::presets_dir`.
pub fn chains_dir() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_home.join("tui-wave").join("cdp_chains")
}

/// Loads every saved chain, sorted by name. See [`list_chains_in`] for the
/// malformed-file handling; this just points it at [`chains_dir`].
pub fn list_chains() -> Vec<CdpChain> {
    list_chains_in(&chains_dir())
}

/// Saves `chain`, overwriting any existing chain with the same name. See [`save_chain_in`].
pub fn save_chain(chain: &CdpChain) {
    save_chain_in(&chains_dir(), chain);
}

/// Deletes the chain named `name`, if it exists. See [`delete_chain_in`].
pub fn delete_chain(name: &str) {
    delete_chain_in(&chains_dir(), name);
}

/// Turns a chain name into a safe filename: anything that isn't alphanumeric, `-`, or `_`
/// becomes `_` (a chain name is free-typed text, unlike a process key, so — unlike
/// `preset.rs`, which can use `process_key` directly — this can't assume the name is already
/// filesystem-safe). Two different names that sanitize to the same filename will collide
/// (last save wins); accepted as a rare-in-practice edge case rather than adding a
/// disambiguation scheme for it.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn chain_file_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.toml", sanitize_name(name)))
}

/// A missing directory yields an empty `Vec`. Each file is parsed independently: one
/// malformed chain file is skipped (not counted, not panicking), every other valid chain
/// still loads — the same "one bad entry doesn't take the rest down with it" philosophy as
/// `preset.rs::load_presets_in`, just applied across files instead of within one.
fn list_chains_in(dir: &Path) -> Vec<CdpChain> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut chains: Vec<CdpChain> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .filter_map(|text| toml::from_str::<CdpChain>(&text).ok())
        .collect();
    chains.sort_by(|a, b| a.name.cmp(&b.name));
    chains
}

/// Best-effort: a write failure (read-only filesystem, missing permissions) is silently
/// ignored, matching `preset::save_preset_in`.
fn save_chain_in(dir: &Path, chain: &CdpChain) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(toml_string) = toml::to_string_pretty(chain) {
        let _ = std::fs::write(chain_file_path(dir, &chain.name), toml_string);
    }
}

/// Best-effort, same silent-failure philosophy as [`save_chain_in`]. A no-op (not an error)
/// if no chain by that name was ever saved.
fn delete_chain_in(dir: &Path, name: &str) {
    let _ = std::fs::remove_file(chain_file_path(dir, name));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::chain::ChainStep;
    use crate::model::cdp::def::ParamValue;

    /// A fresh, uniquely-named temp directory per test — passed directly to the `_in`
    /// functions, never through `XDG_CONFIG_HOME` (see this module's top-level doc comment).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tui_wave_cdp_chain_preset_test_{tag}_{}_{:p}", std::process::id(), &tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn sample_chain(name: &str) -> CdpChain {
        CdpChain {
            name: name.into(),
            steps: vec![ChainStep {
                process_key: "blur_avrg".into(),
                values: vec![ParamValue::Number(4.0)],
                side_chain: Vec::new(),
            }],
        }
    }

    #[test]
    fn listing_with_no_directory_yet_returns_empty() {
        let dir = TempDir::new("unknown");
        std::fs::remove_dir_all(&dir.0).unwrap(); // directory doesn't exist at all
        assert!(list_chains_in(&dir.0).is_empty());
    }

    #[test]
    fn save_then_list_round_trips_a_chain_with_a_side_chain() {
        let dir = TempDir::new("roundtrip");
        let mut chain = sample_chain("My Vocoder Setup");
        chain.steps[0].side_chain = vec![ChainStep {
            process_key: "focus_freeze_1".into(),
            values: vec![ParamValue::Number(1.0)],
            side_chain: Vec::new(),
        }];
        save_chain_in(&dir.0, &chain);

        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded, vec![chain]);
    }

    #[test]
    fn saving_a_chain_with_an_existing_name_overwrites_it() {
        let dir = TempDir::new("overwrite");
        save_chain_in(&dir.0, &sample_chain("Same Name"));
        let mut updated = sample_chain("Same Name");
        updated.steps[0].values = vec![ParamValue::Number(99.0)];
        save_chain_in(&dir.0, &updated);

        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded.len(), 1, "same name should overwrite, not create a second file");
        assert_eq!(loaded[0].steps[0].values, vec![ParamValue::Number(99.0)]);
    }

    #[test]
    fn chain_names_with_unsafe_characters_still_save_and_load() {
        let dir = TempDir::new("unsafe_name");
        let chain = sample_chain("Weird/Name: with * chars?");
        save_chain_in(&dir.0, &chain);

        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Weird/Name: with * chars?", "the original name is preserved inside the file");
    }

    #[test]
    fn delete_removes_only_the_named_chain() {
        let dir = TempDir::new("delete");
        save_chain_in(&dir.0, &sample_chain("Keep"));
        save_chain_in(&dir.0, &sample_chain("Remove"));

        delete_chain_in(&dir.0, "Remove");

        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Keep");
    }

    #[test]
    fn delete_on_a_never_saved_name_is_a_harmless_no_op() {
        let dir = TempDir::new("delete_noop");
        delete_chain_in(&dir.0, "never_saved"); // no file yet -- must not panic
        save_chain_in(&dir.0, &sample_chain("Keep"));
        delete_chain_in(&dir.0, "does not exist");
        assert_eq!(list_chains_in(&dir.0).len(), 1);
    }

    #[test]
    fn malformed_chain_file_is_skipped_others_still_load() {
        let dir = TempDir::new("malformed");
        save_chain_in(&dir.0, &sample_chain("Good"));
        std::fs::write(dir.0.join("bad.toml"), "not valid toml {{{").unwrap();

        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Good");
    }

    #[test]
    fn chains_are_sorted_by_name() {
        let dir = TempDir::new("sorted");
        save_chain_in(&dir.0, &sample_chain("Zebra"));
        save_chain_in(&dir.0, &sample_chain("Alpha"));
        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["Alpha", "Zebra"]);
    }
}
