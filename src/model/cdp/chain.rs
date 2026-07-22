//! A "CDP Chain" — an ordered list of CDP process steps, each with its own saved parameter
//! values, that runs as a single pipeline: step N's output becomes step N+1's input. Pure
//! data + validation, no UI/audio deps, no process spawning — see
//! `src/ui/app.rs`'s chain-editor dialog for building one and `src/cdp/runner.rs` for
//! actually running one. Persistence (`chain_preset.rs`) and the "recently run" list
//! (`chain_recent.rs`) are separate modules, mirroring how `preset.rs`/`recent.rs` split for
//! a single process.
//!
//! A dual-input step (`IoKind::DualWav`/`DualAna`) may optionally carry a `side_chain`: a
//! sub-chain run against a separately-picked buffer to produce that step's second input,
//! instead of using a raw already-open buffer unprocessed. An **empty** `side_chain` is a
//! real, valid state — it means "use the picked buffer as-is," exactly matching the existing
//! single-process dual-input flow's behavior (`CdpSecondInput` in `ui/app.rs`). Nesting is
//! unlimited: a side-chain step may itself be dual-input with its own populated
//! `side_chain`, recursively — no CDP process needs a third *simultaneous* input, but there's
//! no reason a side-chain's own second input can't be built the same way the main chain's
//! can. `steps_at`/`step_at` below address any step at any depth by a `path: &[usize]`.
//!
//! Deliberately **not** persisted here: *which buffer* feeds the main chain or any
//! side-chain. That's chosen live from whatever documents happen to be open when the chain
//! runs, the same way a single dual-input process's `CdpSecondInput.selected` is chosen
//! fresh every time its dialog opens rather than saved in `CdpPreset` — see
//! `CDP-CHAIN-PLAN.md`'s design decision 4.

use serde::{Deserialize, Serialize};

use super::catalog::CdpCatalog;
use super::def::{IoKind, ParamValue};

/// One step in a chain: which CDP process, and its parameter values (mirrors
/// `preset::CdpPreset`'s `values` shape exactly — index-parallel to that process's
/// `ProcessDef.params` at save time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainStep {
    pub process_key: String,
    pub values: Vec<ParamValue>,
    /// Empty unless this step's own process is dual-input and a side-chain has been
    /// configured for it. Always empty on a side-chain's own steps (no nesting).
    #[serde(default)]
    pub side_chain: Vec<ChainStep>,
}

/// A named, ordered chain of steps — the whole thing `chain_preset::save_chain` persists as
/// one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CdpChain {
    pub name: String,
    pub steps: Vec<ChainStep>,
}

/// Why a `CdpChain` failed validation. Surfaced in the chain editor as a plain message
/// (mirrors `pipeline::PlanError`'s role for a single process) rather than matched on by
/// the UI — every variant already carries enough context to build a full sentence.
#[derive(Debug, Clone, PartialEq)]
pub enum ChainError {
    /// A chain with no steps at all can't run — nothing to splice.
    EmptyChain,
    /// `process_key` doesn't match anything in the loaded catalog — the most likely real
    /// cause is a saved chain surviving a catalog change that renamed or removed a process.
    UnknownProcess { key: String },
    /// The process's `input`/`output` shape isn't `Wav`/`Ana`-in, `Wav`/`Ana`-out — synthesis
    /// (`IoKind::None`), pitch-curve transforms (`IoKind::Curve`), and glob-output processes
    /// (`IoKind::WavGlob`) each produce a result shape ("no real input," "a curve, not
    /// audio," "N new buffers") that doesn't compose into "feeds the next step's audio
    /// input" — see `CDP-CHAIN-PLAN.md`'s design decision 3.
    ProcessNotChainable { key: String },
    /// `side_chain` is non-empty but the step's own process only takes one input — there's
    /// no second input for a side-chain to feed.
    SideChainOnSingleInputStep { key: String },
}

impl CdpChain {
    /// Checks every step (and any side-chains, at any depth) against `catalog`. Pure logic —
    /// no filesystem or process access — so it's fully unit-testable the same way
    /// `pipeline.rs`'s planner already is.
    pub fn validate(&self, catalog: &CdpCatalog) -> Result<(), ChainError> {
        if self.steps.is_empty() {
            return Err(ChainError::EmptyChain);
        }
        for step in &self.steps {
            step.validate(catalog)?;
        }
        Ok(())
    }
}

impl ChainStep {
    fn validate(&self, catalog: &CdpCatalog) -> Result<(), ChainError> {
        let def = catalog
            .processes
            .iter()
            .find(|p| p.key == self.process_key)
            .ok_or_else(|| ChainError::UnknownProcess { key: self.process_key.clone() })?;

        let input_ok = matches!(def.input, IoKind::Wav | IoKind::Ana | IoKind::DualWav | IoKind::DualAna);
        let output_ok = matches!(def.output, IoKind::Wav | IoKind::Ana);
        if !input_ok || !output_ok {
            return Err(ChainError::ProcessNotChainable { key: self.process_key.clone() });
        }

        if !self.side_chain.is_empty() {
            let is_dual = matches!(def.input, IoKind::DualWav | IoKind::DualAna);
            if !is_dual {
                return Err(ChainError::SideChainOnSingleInputStep { key: self.process_key.clone() });
            }
            for inner in &self.side_chain {
                inner.validate(catalog)?;
            }
        }
        Ok(())
    }

    /// Whether this step's process takes a second input at all — the chain editor and
    /// runner both need this to know whether a side-chain (or a raw second-buffer pick) is
    /// even offered for a given step. `None` if `process_key` doesn't resolve in `catalog`
    /// (the same "stale saved chain" case `validate` reports as `UnknownProcess`).
    pub fn is_dual_input(&self, catalog: &CdpCatalog) -> Option<bool> {
        catalog
            .processes
            .iter()
            .find(|p| p.key == self.process_key)
            .map(|def| matches!(def.input, IoKind::DualWav | IoKind::DualAna))
    }
}

/// The step list a `path` refers to: `parent_path` empty means `chain`'s own top-level
/// `steps`; each element of `parent_path` after that steps into the `side_chain` of the step
/// at that index in the list found so far. Used by both the chain editor (`ui/app.rs`) and
/// the execution engine to address a step at any depth uniformly, rather than the fixed
/// "main step" / "one side-chain step" distinction an earlier version of this feature had.
pub fn steps_at<'a>(chain: &'a CdpChain, parent_path: &[usize]) -> Option<&'a Vec<ChainStep>> {
    let mut steps = &chain.steps;
    for &i in parent_path {
        steps = &steps.get(i)?.side_chain;
    }
    Some(steps)
}

/// Mutable counterpart to [`steps_at`].
pub fn steps_at_mut<'a>(chain: &'a mut CdpChain, parent_path: &[usize]) -> Option<&'a mut Vec<ChainStep>> {
    let mut steps = &mut chain.steps;
    for &i in parent_path {
        steps = &mut steps.get_mut(i)?.side_chain;
    }
    Some(steps)
}

/// The single step at `path` (its last element is its index within the list `steps_at`
/// would return for `path`'s parent) — `None` if any segment of the path doesn't resolve.
pub fn step_at<'a>(chain: &'a CdpChain, path: &[usize]) -> Option<&'a ChainStep> {
    let (&last, parent) = path.split_last()?;
    steps_at(chain, parent)?.get(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> CdpCatalog {
        let (catalog, warnings) = CdpCatalog::load(None);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        catalog
    }

    fn step(process_key: &str) -> ChainStep {
        ChainStep { process_key: process_key.into(), values: Vec::new(), side_chain: Vec::new() }
    }

    #[test]
    fn empty_chain_is_rejected() {
        let chain = CdpChain { name: "empty".into(), steps: Vec::new() };
        assert_eq!(chain.validate(&catalog()), Err(ChainError::EmptyChain));
    }

    #[test]
    fn a_single_chainable_step_validates() {
        // blur_avrg: input = ana, output = ana -- single-input, chainable.
        let chain = CdpChain { name: "one step".into(), steps: vec![step("blur_avrg")] };
        assert_eq!(chain.validate(&catalog()), Ok(()));
    }

    #[test]
    fn a_multi_step_chain_of_chainable_processes_validates() {
        let chain =
            CdpChain { name: "multi".into(), steps: vec![step("blur_avrg"), step("focus_freeze_1")] };
        assert_eq!(chain.validate(&catalog()), Ok(()));
    }

    #[test]
    fn unknown_process_key_is_rejected() {
        let chain = CdpChain { name: "bad key".into(), steps: vec![step("not_a_real_process")] };
        assert_eq!(
            chain.validate(&catalog()),
            Err(ChainError::UnknownProcess { key: "not_a_real_process".into() })
        );
    }

    #[test]
    fn a_synthesis_process_is_not_chainable() {
        // synth_wave_1: input = none -- nothing for a prior step's output to feed into.
        let chain = CdpChain { name: "synth".into(), steps: vec![step("synth_wave_1")] };
        assert_eq!(
            chain.validate(&catalog()),
            Err(ChainError::ProcessNotChainable { key: "synth_wave_1".into() })
        );
    }

    #[test]
    fn a_curve_process_is_not_chainable() {
        // repitch_quantise_1: input = curve, output = curve -- not audio at all.
        let chain = CdpChain { name: "curve".into(), steps: vec![step("repitch_quantise_1")] };
        assert_eq!(
            chain.validate(&catalog()),
            Err(ChainError::ProcessNotChainable { key: "repitch_quantise_1".into() })
        );
    }

    #[test]
    fn a_glob_output_process_is_not_chainable() {
        // distcut_distcut_1: output = wav_glob -- produces N new buffers, not one audio result.
        let chain = CdpChain { name: "glob".into(), steps: vec![step("distcut_distcut_1")] };
        assert_eq!(
            chain.validate(&catalog()),
            Err(ChainError::ProcessNotChainable { key: "distcut_distcut_1".into() })
        );
    }

    #[test]
    fn an_empty_side_chain_on_a_dual_input_step_is_valid() {
        // combine_mean_1: input = dual_ana -- an empty side_chain means "use the picked
        // buffer as-is," exactly matching today's existing dual-input behavior.
        let chain = CdpChain { name: "dual, no side-chain".into(), steps: vec![step("combine_mean_1")] };
        assert_eq!(chain.validate(&catalog()), Ok(()));
    }

    #[test]
    fn a_populated_side_chain_on_a_dual_input_step_is_valid() {
        let mut dual = step("combine_mean_1");
        dual.side_chain = vec![step("blur_avrg")];
        let chain = CdpChain { name: "dual with side-chain".into(), steps: vec![dual] };
        assert_eq!(chain.validate(&catalog()), Ok(()));
    }

    #[test]
    fn a_side_chain_on_a_single_input_step_is_rejected() {
        let mut single = step("blur_avrg");
        single.side_chain = vec![step("focus_freeze_1")];
        let chain = CdpChain { name: "bad side-chain".into(), steps: vec![single] };
        assert_eq!(
            chain.validate(&catalog()),
            Err(ChainError::SideChainOnSingleInputStep { key: "blur_avrg".into() })
        );
    }

    /// Nesting is unlimited: a side-chain step may itself be dual-input with its own
    /// populated side-chain, to any depth — confirmed at 3 levels here (main step -> its
    /// side-chain's own dual-input step -> *that* step's own side-chain).
    #[test]
    fn side_chains_may_nest_to_any_depth() {
        let mut innermost_dual = step("combine_mean_1");
        innermost_dual.side_chain = vec![step("blur_avrg")];
        let mut middle_dual = step("combine_mean_1");
        middle_dual.side_chain = vec![innermost_dual];
        let outer_dual = ChainStep { process_key: "combine_mean_1".into(), values: Vec::new(), side_chain: vec![middle_dual] };
        let chain = CdpChain { name: "deeply nested".into(), steps: vec![outer_dual] };
        assert_eq!(chain.validate(&catalog()), Ok(()));
    }

    /// A side-chain-of-a-side-chain step still can't itself carry a side-chain unless *it's*
    /// dual-input — the depth restriction is gone, but the "only a dual-input step can have
    /// one at all" rule still applies uniformly at every depth.
    #[test]
    fn a_single_input_step_two_levels_deep_still_cannot_carry_a_side_chain() {
        let mut bad_inner = step("blur_avrg"); // single-input
        bad_inner.side_chain = vec![step("focus_freeze_1")];
        let outer_dual = ChainStep { process_key: "combine_mean_1".into(), values: Vec::new(), side_chain: vec![bad_inner] };
        let chain = CdpChain { name: "bad at depth 2".into(), steps: vec![outer_dual] };
        assert_eq!(
            chain.validate(&catalog()),
            Err(ChainError::SideChainOnSingleInputStep { key: "blur_avrg".into() })
        );
    }

    #[test]
    fn steps_at_and_step_at_navigate_arbitrary_depth_paths() {
        let mut middle_dual = ChainStep { process_key: "combine_mean_1".into(), values: Vec::new(), side_chain: vec![step("blur_avrg")] };
        middle_dual.side_chain[0].values = vec![ParamValue::Number(7.0)];
        let outer_dual = ChainStep { process_key: "combine_mean_1".into(), values: Vec::new(), side_chain: vec![middle_dual] };
        let chain = CdpChain { name: "paths".into(), steps: vec![step("focus_freeze_1"), outer_dual] };

        assert_eq!(steps_at(&chain, &[]).unwrap().len(), 2);
        assert_eq!(step_at(&chain, &[0]).unwrap().process_key, "focus_freeze_1");
        assert_eq!(step_at(&chain, &[1]).unwrap().process_key, "combine_mean_1");
        assert_eq!(steps_at(&chain, &[1]).unwrap().len(), 1, "step 1's own side-chain");
        assert_eq!(step_at(&chain, &[1, 0]).unwrap().process_key, "combine_mean_1");
        assert_eq!(steps_at(&chain, &[1, 0]).unwrap().len(), 1, "step [1,0]'s own side-chain");
        assert_eq!(step_at(&chain, &[1, 0, 0]).unwrap().process_key, "blur_avrg");
        assert_eq!(step_at(&chain, &[1, 0, 0]).unwrap().values, vec![ParamValue::Number(7.0)]);
        assert!(step_at(&chain, &[1, 0, 0, 0]).is_none(), "blur_avrg has no side-chain of its own");
        assert!(step_at(&chain, &[5]).is_none(), "out-of-range index");

        // Mutate through the same path and confirm it's reflected.
        let mut chain = chain;
        steps_at_mut(&mut chain, &[1, 0]).unwrap().push(step("focus_freeze_1"));
        assert_eq!(steps_at(&chain, &[1, 0]).unwrap().len(), 2);
    }

    #[test]
    fn is_dual_input_reports_correctly_for_known_and_unknown_processes() {
        let cat = catalog();
        assert_eq!(step("combine_mean_1").is_dual_input(&cat), Some(true));
        assert_eq!(step("blur_avrg").is_dual_input(&cat), Some(false));
        assert_eq!(step("not_a_real_process").is_dual_input(&cat), None);
    }

    /// The recursive `side_chain: Vec<ChainStep>` field, and `CdpChain` as a whole, must
    /// survive a TOML round-trip cleanly — same discipline as `def.rs`'s `ProcessDef` schema
    /// tests, validated in isolation before any persistence/UI code depends on it.
    #[test]
    fn chain_with_a_populated_side_chain_round_trips_through_toml() {
        let mut dual = step("combine_mean_1");
        dual.values = vec![ParamValue::Number(0.5)];
        dual.side_chain = vec![ChainStep {
            process_key: "blur_avrg".into(),
            values: vec![ParamValue::Number(4.0)],
            side_chain: Vec::new(),
        }];
        let chain = CdpChain { name: "Round Trip".into(), steps: vec![dual] };

        let text = toml::to_string(&chain).expect("serialize");
        let back: CdpChain = toml::from_str(&text).expect("deserialize");
        assert_eq!(chain, back);
    }
}
