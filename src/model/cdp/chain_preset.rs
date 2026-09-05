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
        // Every reader of a saved chain comes through here, so this is where the pre-branches
        // `side_chain` shape is folded into `branches` — one place, rather than each caller
        // remembering to. Topping a step up to its process's required branch count needs the
        // catalog and so happens in the editor's own load paths instead.
        .map(|mut chain| {
            chain.migrate_legacy();
            chain
        })
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
            steps: vec![ChainStep::new("blur_avrg", vec![ParamValue::Number(4.0)])],
            bank: Default::default(),
            output: Default::default(),
        }
    }

    #[test]
    fn listing_with_no_directory_yet_returns_empty() {
        let dir = TempDir::new("unknown");
        std::fs::remove_dir_all(&dir.0).unwrap(); // directory doesn't exist at all
        assert!(list_chains_in(&dir.0).is_empty());
    }

    #[test]
    fn save_then_list_round_trips_a_chain_with_a_branch() {
        let dir = TempDir::new("roundtrip");
        let mut chain = sample_chain("My Vocoder Setup");
        chain.steps[0].branches = vec![crate::model::cdp::chain::Branch {
            source: crate::model::cdp::BranchSource::Buffer,
            steps: vec![ChainStep::new("focus_freeze_1", vec![ParamValue::Number(1.0)])],
        }];
        save_chain_in(&dir.0, &chain);

        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded, vec![chain]);
    }

    /// Everything a chain can hold must survive a save/load, not just the shape the editor
    /// happened to be tested with: the envelope bank and the references into it, every branch
    /// source, nested branches, a native combiner, the output destination, and every
    /// `ParamValue` variant.
    ///
    /// The whole-struct comparison at the end is the real assertion; the individual ones above
    /// it are there so a failure names the part that broke instead of printing two chains. The
    /// serialization check comes first because `save_chain_in` ignores a failed
    /// `toml::to_string_pretty` — a chain TOML cannot represent would otherwise be silently not
    /// saved, and the only symptom would be a preset missing from the list.
    #[test]
    fn a_chain_holding_everything_it_can_hold_round_trips() {
        use crate::model::cdp::chain::{Branch, BranchSource, ChainOutput, PathSeg::Branch as B, PathSeg::Step as S};
        use crate::model::cdp::envelope_bank::{BankEnvelope, EnvelopeBank, EnvelopeRef};
        use crate::model::cdp::def::{CrystalVdat, HiliteBandRow};

        let dir = TempDir::new("everything");

        // Two curves, referenced from two different steps at different ranges and polarities.
        let bank = EnvelopeBank {
            envelopes: vec![
                BankEnvelope { name: "swell".into(), points: vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.25)] },
                BankEnvelope { name: "stutter".into(), points: vec![(0.0, 1.0), (1.0, 0.0)] },
            ],
        };

        // A step carrying one of every `ParamValue` variant, so a variant that cannot be
        // serialized is caught here rather than by a user losing a saved chain.
        let every_value = ChainStep::new(
            "blur_avrg",
            vec![
                ParamValue::Number(4.25),
                ParamValue::Toggle(true),
                ParamValue::Choice(2),
                ParamValue::EnvelopeRef(EnvelopeRef {
                    name: "swell".into(),
                    min: 1.0,
                    max: 100.0,
                    invert: true,
                }),
                ParamValue::List(vec![0.5, 1.5, 2.5]),
                ParamValue::Table(vec![vec![0.0, 1.0], vec![2.0, 3.0]]),
                ParamValue::MarkerTimeList(vec![('a', 0.25), ('b', 1.75)]),
                ParamValue::HiliteBand(vec![HiliteBandRow {
                    lofrq: 100.0,
                    hifrq: 2000.0,
                    amp_bit: true,
                    ramp_bit: false,
                    transpose_bit: true,
                    add_bit: false,
                    amp1: 0.25,
                    amp2: 0.75,
                    transpose_value: 1.5,
                    transpose_additive: true,
                }]),
                ParamValue::FormantBufferRef,
                ParamValue::FilePath("/tmp/some/where.wav".into()),
                ParamValue::Text("free text".into()),
                ParamValue::CrystalVdat(CrystalVdat {
                    vertices: vec![[0.0, 1.0, 2.0], [3.0, 4.0, 5.0]],
                    envelope: vec![(0.0, 0.0), (1.0, 1.0)],
                }),
            ],
        );

        // A native combiner with both leg shapes: a tap that nests a branch of its own, and a
        // leg reading the finished output of the sibling beside it. Two legs, because
        // `native::MAX_MIX_LEGS` is the ceiling and an over-wide fixture would not be a chain
        // the editor could produce.
        let mut inner_combine = ChainStep::new("combine_mean_1", vec![ParamValue::Number(0.5)]);
        inner_combine.branches = vec![Branch {
            // A step-naming `From`, as against the branch-naming one on the mixer below.
            source: BranchSource::From(vec![S(0)]),
            steps: vec![ChainStep::new("focus_freeze_1", vec![ParamValue::Number(1.0)])],
        }];
        let mut mixer = ChainStep::new(
            crate::model::cdp::native::MIXER_KEY,
            vec![
                ParamValue::Toggle(true),
                ParamValue::Number(-0.1),
                ParamValue::EnvelopeRef(EnvelopeRef {
                    name: "stutter".into(),
                    min: -60.0,
                    max: 12.0,
                    invert: false,
                }),
                ParamValue::Toggle(false),
                ParamValue::Number(-6.0),
                ParamValue::Toggle(true),
            ],
        );
        mixer.branches = vec![
            Branch { source: BranchSource::Tap, steps: vec![inner_combine] },
            // Leg B reads leg A: siblings finish left to right.
            Branch { source: BranchSource::From(vec![S(1), B(0)]), steps: Vec::new() },
        ];

        let chain = CdpChain {
            name: "Everything / at once?".into(),
            steps: vec![every_value, mixer],
            bank,
            // Not the default, so a round-trip that dropped it would show.
            output: ChainOutput::NewBuffer,
        };

        assert!(
            toml::to_string_pretty(&chain).is_ok(),
            "a chain TOML cannot represent is silently never saved"
        );

        save_chain_in(&dir.0, &chain);
        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded.len(), 1, "the chain saved and came back");
        let back = &loaded[0];

        assert_eq!(back.name, chain.name, "a name with unsafe characters survives inside the file");
        assert_eq!(back.output, ChainOutput::NewBuffer, "the output destination is part of the chain");
        assert_eq!(back.bank, chain.bank, "the envelope bank round-trips, curves and all");
        assert_eq!(back.steps[0].values, chain.steps[0].values, "every ParamValue variant round-trips");
        assert_eq!(
            back.steps[1].branches.iter().map(|b| b.source.clone()).collect::<Vec<_>>(),
            vec![BranchSource::Tap, BranchSource::From(vec![S(1), B(0)])],
            "both leg sources, including the path a From names"
        );
        assert_eq!(
            back.steps[1].branches[0].steps[0].branches[0].source,
            BranchSource::From(vec![S(0)]),
            "a branch nested inside a branch keeps its own source, path and all"
        );

        // The references still resolve against the bank that came back with them, which is what
        // makes a recalled chain runnable rather than merely well-formed.
        for step in [&back.steps[0], &back.steps[1]] {
            for value in &step.values {
                if let ParamValue::EnvelopeRef(reference) = value {
                    assert!(
                        back.bank.envelopes.iter().any(|e| e.name == reference.name),
                        "recalled chain references \"{}\", which is not in its own bank",
                        reference.name
                    );
                }
            }
        }

        assert_eq!(*back, chain, "and nothing else changed on the way through");
    }

    /// Which document feeds a `Buffer` branch is deliberately *not* saved: it is a live pick in
    /// the editor (`buffer_picks`), so a recalled chain asks for it again rather than naming a
    /// buffer index that means something different in the next session.
    #[test]
    fn a_buffer_branch_saves_its_source_but_no_document() {
        use crate::model::cdp::chain::{Branch, BranchSource};
        let dir = TempDir::new("buffer_pick");
        let mut chain = sample_chain("Side-chained");
        chain.steps[0].branches =
            vec![Branch { source: BranchSource::Buffer, steps: Vec::new() }];
        save_chain_in(&dir.0, &chain);

        let text = std::fs::read_to_string(chain_file_path(&dir.0, &chain.name)).unwrap();
        assert!(text.contains("Buffer"), "the source itself is saved");
        assert!(!text.contains("buffer_picks"), "the pick is not");

        let loaded = list_chains_in(&dir.0);
        assert_eq!(loaded[0].steps[0].branches[0].source, BranchSource::Buffer);
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
