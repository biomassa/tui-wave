//! An "ExtProcess Chain" — an ordered list of process steps, each with its own saved parameter
//! values, that runs as a single pipeline: step N's output becomes step N+1's input. Pure
//! data + validation, no UI/audio deps, no process spawning — see
//! `src/ui/app.rs`'s chain-editor dialog for building one and `src/cdp/runner.rs` for
//! actually running one. Persistence (`chain_preset.rs`) and the "recently run" list
//! (`chain_recent.rs`) are separate modules, mirroring how `preset.rs`/`recent.rs` split for
//! a single process.
//!
//! # Branches
//!
//! A step may carry [`Branch`]es feeding the inputs its own process takes beyond the running
//! buffer. Each branch is a sub-chain in its own right — nesting is unlimited — plus a
//! [`BranchSource`] saying what its *first* step draws from:
//!
//! - [`BranchSource::Buffer`] — a separately picked open document. This is what a chain's
//!   "side-chain" meant before branches existed, and legacy presets migrate to exactly it.
//! - [`BranchSource::Tap`] — a copy of the signal arriving at the branch's owning step.
//!
//! `Tap` is what makes parallel processing expressible, and it is why there is no separate
//! "split" node: every leg of a combiner taps the same point and diverges inside itself, so an
//! empty leg is the dry signal and a leg with steps in it is a wet one. How many branches a
//! step may carry is a property of its process ([`super::ProcessDef::branch_arity`]) — one for
//! a CDP dual-input process, several for a native combiner, none at all for anything else.
//!
//! Deliberately **not** persisted: *which buffer* feeds a `Buffer` branch. That's chosen live
//! from whatever documents happen to be open when the chain runs, the same way a single
//! dual-input process's `CdpSecondInput.selected` is chosen fresh every time its dialog opens
//! rather than saved in `CdpPreset` — see `CDP-CHAIN-PLAN.md`'s design decision 4.
//!
//! # Envelopes
//!
//! A chain owns a [`super::EnvelopeBank`], and inside a chain that bank is the *only* way a
//! parameter can be automated: `validate` rejects a step holding a raw
//! [`ParamValue::Breakpoints`]. That rule is a guardrail against a real bug rather than a
//! matter of taste — breakpoints are in seconds baked to whichever selection was live when
//! they were drawn, and nothing recorded that duration, so a chain replayed against a
//! different selection silently mis-timed its automation. A bank curve is normalized and
//! projected onto the real axis at run time; see `super::envelope_bank`.

use serde::{Deserialize, Serialize};

use super::catalog::CdpCatalog;
use super::def::{IoKind, ParamValue};
use super::envelope_bank::{BankEnvelope, BankError, EnvelopeBank, EnvelopeRef};

/// How deeply splits may nest: a top-level split, and one more inside either of its branches.
///
/// A cap rather than "unlimited", which is what the model allows, because the *editor* draws
/// branches as side-by-side columns and each level divides the width. Two levels is four columns
/// at the deepest, which still leaves each one wide enough to show a parameter's label, slider
/// and value — the thing the layout exists for. Past that the picture stops being readable well
/// before the arithmetic stops working, so the limit is where legibility ends, not where the
/// data model does.
pub const MAX_SPLIT_DEPTH: usize = 2;

/// One element of the path addressing a step or a branch anywhere in a chain.
///
/// Paths **alternate**, always starting with `Step`: `[Step(1)]` is the second top-level step,
/// `[Step(1), Branch(0)]` is that step's first branch, and `[Step(1), Branch(0), Step(2)]` is
/// the third step inside it. The accessors below return `None` on a path that breaks the
/// alternation rather than trusting it, so a malformed path is a miss and never a wrong hit.
///
/// This replaced a plain `Vec<usize>` when branch arity went past one: with a single side-chain
/// per step, "descend into the step at index i" was the only move a path element could mean, so
/// the branch could stay implicit. It cannot now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PathSeg {
    Step(usize),
    Branch(usize),
}

/// Where a chain path points. See [`PathSeg`] for the alternation rule.
pub type Path = Vec<PathSeg>;

/// What a branch's first step draws its audio from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BranchSource {
    /// A copy of the signal arriving at this branch's owning step — so several branches of one
    /// combiner all start from the same audio and differ only in what they do to it.
    #[default]
    Tap,
    /// A separately picked open document, unprocessed until this branch's own steps run. The
    /// pick itself is live-only and lives in the editor's `buffer_picks`, never in the file.
    Buffer,
    /// The finished output of an **earlier step or branch**, reused rather than recomputed.
    ///
    /// This is what turns the chain from a tree into a DAG, and it is the only way to feed a
    /// two-input process from something the chain itself made: without it, the second input of
    /// a `combine`/`morph` could only be a separate file or a sub-chain built again from
    /// scratch, so a result you had already computed had to be computed twice to be used twice.
    ///
    /// The path names a step or a branch, and must name one that *completes before* this branch
    /// starts — see [`outputs_available_to`], the single definition of that, which drives both
    /// validation and the editor's own list of choices. Because only already-finished outputs
    /// are addressable, a cycle cannot be expressed at all; this stays acyclic by construction
    /// rather than by a check that could be got wrong.
    From(Path),
}

/// One parallel input to a step: where it starts, and what happens to it on the way in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    #[serde(default)]
    pub source: BranchSource,
    /// May be empty, which is meaningful rather than degenerate: an empty `Tap` branch is the
    /// dry signal, and an empty `Buffer` branch is the picked file used as-is — exactly what a
    /// dual-input process with no side-chain has always done.
    #[serde(default)]
    pub steps: Vec<ChainStep>,
}

impl Branch {
    pub fn buffer() -> Self {
        Self { source: BranchSource::Buffer, steps: Vec::new() }
    }
}

/// One step in a chain: which process, its parameter values (mirrors `preset::CdpPreset`'s
/// `values` shape exactly — index-parallel to that process's `ProcessDef.params` at save time),
/// and any parallel inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainStep {
    pub process_key: String,
    pub values: Vec<ParamValue>,
    #[serde(default)]
    pub branches: Vec<Branch>,
    /// Read-only compatibility shim for chains saved before branches existed, where a step
    /// carried at most one `side_chain` fed by a picked buffer. Never written back
    /// (`skip_serializing`) and never read by anything but [`CdpChain::migrate_legacy`], which
    /// folds it into `branches` on load and empties it. Named for what it is so that reaching
    /// for it by mistake reads wrong.
    #[serde(rename = "side_chain", default, skip_serializing)]
    pub legacy_side_chain: Vec<ChainStep>,
}

impl ChainStep {
    pub fn new(process_key: impl Into<String>, values: Vec<ParamValue>) -> Self {
        Self {
            process_key: process_key.into(),
            values,
            branches: Vec::new(),
            legacy_side_chain: Vec::new(),
        }
    }
}

/// Where a chain's result goes.
///
/// A property of the *chain*, not of the run, so it is saved with the preset and stated on the
/// editor's OUT row: a chain built to produce new material should not have to be remembered as
/// "the one you run with something else selected". A step that declares
/// `ProcessDef::output_new_buffer` still forces a new buffer regardless — that is a fact about
/// the process (it changes the channel count), not a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChainOutput {
    /// Replace the audio the chain read, as one undoable edit.
    #[default]
    Splice,
    /// Open the result as a new document, leaving the source untouched.
    NewBuffer,
}

/// A named, ordered chain of steps and the envelope bank they share — the whole thing
/// `chain_preset::save_chain` persists as one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CdpChain {
    pub name: String,
    pub steps: Vec<ChainStep>,
    /// Every named envelope any step in this chain references. Per-chain rather than a global
    /// library so the file is self-contained: copying it copies its curves, and no rename
    /// elsewhere can leave a saved chain pointing at nothing.
    #[serde(default)]
    pub bank: EnvelopeBank,
    #[serde(default)]
    pub output: ChainOutput,
}

impl CdpChain {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
            bank: EnvelopeBank::default(),
            output: ChainOutput::default(),
        }
    }
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
    /// More branches than this step's process has inputs to feed. Replaced the old
    /// `SideChainOnSingleInputStep`, which could only say "one is too many for a step that
    /// takes one input"; arity is now a number rather than a yes/no.
    TooManyBranches { key: String, arity: usize, actual: usize },
    /// Fewer branches than this step's process requires — today only a dual-input process
    /// whose mandatory second input has no branch to come from. `normalize_branches` tops
    /// these up on load, so reaching this means a hand-edited file.
    MissingBranches { key: String, required: usize, actual: usize },
    /// A split nested deeper than [`MAX_SPLIT_DEPTH`]. Unreachable through the editor, which
    /// stops offering "Branch out" at that depth; a hand-edited file can still say it.
    SplitTooDeep { key: String, depth: usize },
    /// A branch reads an output that has not finished — or does not exist — by the time it
    /// needs it. Only branches earlier in the run order are addressable, so this means a
    /// hand-edited file or a reference left dangling by a deletion.
    BranchNotAvailable { at: Path, wanted: Path },
    /// A step holds a raw [`ParamValue::Breakpoints`]. Legal for a standalone process, never
    /// inside a chain — see this module's header for why, and `envelope_bank` for what
    /// replaces it.
    RawEnvelopeInChain { key: String, param: usize },
    /// A step references a bank curve, or holds a bank, that doesn't check out.
    Bank(BankError),
}

impl From<BankError> for ChainError {
    fn from(err: BankError) -> Self {
        ChainError::Bank(err)
    }
}

impl CdpChain {
    /// Checks every step (and every branch, at any depth) against `catalog`. Pure logic —
    /// no filesystem or process access — so it's fully unit-testable the same way
    /// `pipeline.rs`'s planner already is.
    pub fn validate(&self, catalog: &CdpCatalog) -> Result<(), ChainError> {
        if self.steps.is_empty() {
            return Err(ChainError::EmptyChain);
        }
        self.bank.validate()?;
        for step in &self.steps {
            step.validate(catalog, &self.bank)?;
        }
        self.validate_branch_references()
    }

    /// Every [`BranchSource::From`] must name a branch that finishes first. Checked over the
    /// whole chain rather than per step, because "first" is a property of the traversal and not
    /// of any one step's contents.
    fn validate_branch_references(&self) -> Result<(), ChainError> {
        for path in outputs_available_to(self, None) {
            let Some(branch) = branch_at(self, &path) else { continue };
            let BranchSource::From(source) = &branch.source else { continue };
            if !outputs_available_to(self, Some(&path)).iter().any(|p| p == source) {
                return Err(ChainError::BranchNotAvailable { at: path, wanted: source.clone() });
            }
        }
        Ok(())
    }

    /// Folds any `side_chain` read from a pre-branches preset file into `branches`, at every
    /// depth, and empties the shim field. Idempotent, and a no-op on anything saved since.
    ///
    /// Called by every loader (`chain_preset`, `chain_last`) rather than by `validate`, because
    /// a caller that only validates should not be silently mutating what it was handed.
    ///
    /// The migrated branch is [`BranchSource::Buffer`], which is exactly what a side-chain
    /// always was: its first step drew from a separately picked open document. `Tap` did not
    /// exist, so no saved chain can have meant it.
    pub fn migrate_legacy(&mut self) {
        for step in &mut self.steps {
            step.migrate_legacy();
        }
        self.migrate_raw_envelopes();
    }

    /// Lifts any raw [`ParamValue::Breakpoints`] left in a saved chain into the bank.
    ///
    /// Such a value predates the bank and is the bug the bank exists to end: its times are in
    /// seconds baked to whichever selection was live when it was drawn, and nothing recorded
    /// that duration. `validate` refuses one outright, so without this a chain saved with an
    /// envelope would simply stop running.
    ///
    /// The authored duration is unrecoverable, so this assumes the curve spanned its selection
    /// — which is what the editor's seeded two-point line does — and normalizes by the curve's
    /// own last time. Best-effort, and strictly better than what it replaces, where the value
    /// was already meaningless against any selection but the one it was drawn on. The Y range
    /// is unrecoverable too, so the reference window is the curve's own span; the shape and its
    /// relative motion survive exactly, which is what a curve is for.
    fn migrate_raw_envelopes(&mut self) {
        let mut pending = Vec::new();
        collect_raw_envelopes(&self.steps, &mut Vec::new(), &mut pending);
        for (path_index, points) in pending {
            let span = points.last().map(|&(t, _)| t).unwrap_or(0.0);
            let lo = points.iter().map(|&(_, v)| v).fold(f64::INFINITY, f64::min);
            let hi = points.iter().map(|&(_, v)| v).fold(f64::NEG_INFINITY, f64::max);
            let (lo, hi) = if lo.is_finite() && hi.is_finite() && hi > lo { (lo, hi) } else { (0.0, 1.0) };
            let name = self.bank.next_name();
            let envelope = BankEnvelope::normalized(name.clone(), &points, span, lo, hi);
            let name = self.bank.insert_unique(envelope);
            set_envelope_ref(
                &mut self.steps,
                &path_index,
                EnvelopeRef { name, min: lo, max: hi, invert: false },
            );
        }
    }
}

impl CdpChain {
    /// Tops every step up to its process's [`super::ProcessDef::branch_arity_min`], at every
    /// depth, so the "a dual-input step always has its one branch" invariant holds for anything
    /// that just came off disk or out of a catalog change.
    ///
    /// Separate from [`CdpChain::migrate_legacy`] because it needs the catalog and that does
    /// not: a loader without a catalog in hand can still fold `side_chain` into `branches`
    /// correctly, and only a caller that can resolve a process key can know how many branches
    /// that process wants. Both are called together from the editor's load paths.
    pub fn normalize_branches(&mut self, catalog: &CdpCatalog) {
        for step in &mut self.steps {
            step.normalize_branches(catalog);
        }
    }
}

/// Where a raw `Breakpoints` was found. A private address, not [`PathSeg`], because it also has
/// to name the *value* within a step, which a chain path never does.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EnvSeg {
    Step(usize),
    Branch(usize),
    Value(usize),
}

/// Every raw `Breakpoints` in a step tree, so the caller can lift them into the bank without
/// holding a borrow of the steps while it mutates it.
fn collect_raw_envelopes(steps: &[ChainStep], prefix: &mut Vec<EnvSeg>, out: &mut Vec<(Vec<EnvSeg>, Vec<(f64, f64)>)>) {
    for (i, step) in steps.iter().enumerate() {
        prefix.push(EnvSeg::Step(i));
        for (v, value) in step.values.iter().enumerate() {
            if let ParamValue::Breakpoints(points) = value {
                let mut path = prefix.clone();
                path.push(EnvSeg::Value(v));
                out.push((path, points.clone()));
            }
        }
        for (b, branch) in step.branches.iter().enumerate() {
            prefix.push(EnvSeg::Branch(b));
            collect_raw_envelopes(&branch.steps, prefix, out);
            prefix.pop();
        }
        prefix.pop();
    }
}

/// Writes an [`EnvelopeRef`] back at the position [`collect_raw_envelopes`] reported.
fn set_envelope_ref(steps: &mut [ChainStep], path: &[EnvSeg], reference: EnvelopeRef) {
    let Some((EnvSeg::Step(i), rest)) = path.split_first().map(|(a, b)| (*a, b)) else { return };
    let Some(step) = steps.get_mut(i) else { return };
    match rest.split_first().map(|(a, b)| (*a, b)) {
        Some((EnvSeg::Value(v), tail)) if tail.is_empty() => {
            if let Some(slot) = step.values.get_mut(v) {
                *slot = ParamValue::EnvelopeRef(reference);
            }
        }
        Some((EnvSeg::Branch(b), tail)) => {
            if let Some(branch) = step.branches.get_mut(b) {
                set_envelope_ref(&mut branch.steps, tail, reference);
            }
        }
        _ => {}
    }
}

/// Every step and branch whose output is finished by the time `target` begins — the set
/// [`BranchSource::From`] may name, in the order they complete.
///
/// `target` may name a **branch** (what has finished before that branch runs) or a **step**
/// (what has finished before that step's own branches run). The step form is what the editor
/// asks when a step is being *added*: the branch it will own does not exist yet, so there is
/// nothing to point at but the position it will occupy.
///
/// Derived by walking the chain in exactly the order the runner does — each step's branches in
/// turn, then the step itself — collecting outputs as they finish and stopping the moment
/// `target` is reached. Deriving it from the same traversal rather than reasoning about paths is
/// what keeps "what the editor offers" and "what the runner can actually supply" the same set:
/// an offered output that had not been computed yet would fail at the point of no return,
/// halfway through a chain.
///
/// `target` may be `None` to ask for *every* output in completion order.
pub fn outputs_available_to(chain: &CdpChain, target: Option<&[PathSeg]>) -> Vec<Path> {
    let mut done = Vec::new();
    let mut reached = false;
    collect_finished_outputs(&chain.steps, &mut Vec::new(), target, &mut done, &mut reached);
    done
}

fn collect_finished_outputs(
    steps: &[ChainStep],
    prefix: &mut Path,
    target: Option<&[PathSeg]>,
    done: &mut Vec<Path>,
    reached: &mut bool,
) {
    for (i, step) in steps.iter().enumerate() {
        if *reached {
            return;
        }
        prefix.push(PathSeg::Step(i));
        if target.is_some_and(|t| t == prefix.as_slice()) {
            // Stop *before* this step's branches: they run as part of it, so from inside one of
            // them none of them has finished.
            *reached = true;
            prefix.pop();
            return;
        }
        for (b, branch) in step.branches.iter().enumerate() {
            prefix.push(PathSeg::Branch(b));
            if target.is_some_and(|t| t == prefix.as_slice()) {
                // Everything collected so far is finished; this branch, and everything after it,
                // is not.
                *reached = true;
                prefix.pop();
                prefix.pop();
                return;
            }
            collect_finished_outputs(&branch.steps, prefix, target, done, reached);
            if *reached {
                prefix.pop();
                prefix.pop();
                return;
            }
            done.push(prefix.clone());
            prefix.pop();
        }
        // A step finishes after every branch feeding it.
        done.push(prefix.clone());
        prefix.pop();
    }
}

/// Whether `def` can be a chain step at all: it must both consume the previous step's audio
/// and produce audio for the next one. That rules out synthesis (`IoKind::None` — nothing
/// upstream to consume), pitch-curve transforms (`IoKind::Curve` — carries no audio on either
/// side), glob output (`IoKind::WavGlob` — many results, not one to feed onward), and the
/// variadic input kinds (`VariadicWav`/`GroupedWav` — their extra files come from a per-run
/// picker a saved chain has no way to carry). See `CDP-CHAIN-PLAN.md` design decision 3.
///
/// Public and shared with `ui::app`'s browser filter (`cdp_filter_entries`) rather than
/// duplicated there. It *was* duplicated, as `matches!(output, Wav | Ana) && input != None` —
/// an approximation that agreed with this rule for every input kind that existed at the time
/// and then silently diverged the moment `VariadicWav`/`GroupedWav` were added, offering those
/// processes as chain steps that `validate` would immediately reject. Hence one function.
pub fn process_is_chainable(def: &super::ProcessDef) -> bool {
    matches!(def.input, IoKind::Wav | IoKind::Ana | IoKind::DualWav | IoKind::DualAna)
        && matches!(def.output, IoKind::Wav | IoKind::Ana)
}

impl ChainStep {
    fn validate(&self, catalog: &CdpCatalog, bank: &EnvelopeBank) -> Result<(), ChainError> {
        self.validate_at(catalog, bank, 0)
    }

    fn validate_at(&self, catalog: &CdpCatalog, bank: &EnvelopeBank, depth: usize) -> Result<(), ChainError> {
        let def = catalog
            .processes
            .iter()
            .find(|p| p.key == self.process_key)
            .ok_or_else(|| ChainError::UnknownProcess { key: self.process_key.clone() })?;

        if !process_is_chainable(def) {
            return Err(ChainError::ProcessNotChainable { key: self.process_key.clone() });
        }

        let arity = def.branch_arity();
        if self.branches.len() > arity {
            return Err(ChainError::TooManyBranches {
                key: self.process_key.clone(),
                arity,
                actual: self.branches.len(),
            });
        }
        let required = def.branch_arity_min();
        if self.branches.len() < required {
            return Err(ChainError::MissingBranches {
                key: self.process_key.clone(),
                required,
                actual: self.branches.len(),
            });
        }

        for (i, value) in self.values.iter().enumerate() {
            match value {
                ParamValue::Breakpoints(_) => {
                    return Err(ChainError::RawEnvelopeInChain { key: self.process_key.clone(), param: i })
                }
                ParamValue::EnvelopeRef(reference) => {
                    if !bank.contains(&reference.name) {
                        return Err(ChainError::Bank(BankError::UnknownEnvelope {
                            name: reference.name.clone(),
                        }));
                    }
                }
                _ => {}
            }
        }

        if !self.branches.is_empty() && depth >= MAX_SPLIT_DEPTH {
            return Err(ChainError::SplitTooDeep { key: self.process_key.clone(), depth: depth + 1 });
        }
        for branch in &self.branches {
            for inner in &branch.steps {
                inner.validate_at(catalog, bank, depth + 1)?;
            }
        }
        Ok(())
    }

    /// [`ChainStep::normalize_branches`] against an already-resolved def — for the commit path,
    /// which has the `ProcessDef` in hand and no reason to look it up again by key.
    pub fn normalize_branches_for(&mut self, def: &super::ProcessDef) {
        while self.branches.len() < def.branch_arity_min() {
            self.branches.push(Branch::buffer());
        }
    }

    fn normalize_branches(&mut self, catalog: &CdpCatalog) {
        if let Some(def) = catalog.processes.iter().find(|p| p.key == self.process_key) {
            while self.branches.len() < def.branch_arity_min() {
                // Mandatory branches are the dual-input second input, which has always been fed
                // by a separately picked document -- `Buffer`, never `Tap`.
                self.branches.push(Branch::buffer());
            }
        }
        for branch in &mut self.branches {
            for step in &mut branch.steps {
                step.normalize_branches(catalog);
            }
        }
    }

    fn migrate_legacy(&mut self) {
        if !self.legacy_side_chain.is_empty() {
            let steps = std::mem::take(&mut self.legacy_side_chain);
            self.branches.push(Branch { source: BranchSource::Buffer, steps });
        }
        for branch in &mut self.branches {
            for step in &mut branch.steps {
                step.migrate_legacy();
            }
        }
    }

}

/// After inserting a step at index `inserted` in the list at `parent`, rewrites every
/// [`BranchSource::From`] path that named a later sibling of that list, or anything nested under
/// one, so it still names the same step.
///
/// Without this, inserting shifts the sibling indices while the stored paths stay put, so a
/// `From` would silently read a *different* step's output rather than fail. The prefix is
/// checked, not just the segment at that depth, for the reason [`steps_at`] returns `None` on a
/// malformed path: two different lists can hold the same index.
pub fn shift_branch_sources_for_insert(chain: &mut CdpChain, parent: &[PathSeg], inserted: usize) {
    fn walk(steps: &mut [ChainStep], parent: &[PathSeg], inserted: usize) {
        for step in steps.iter_mut() {
            for branch in &mut step.branches {
                if let BranchSource::From(path) = &mut branch.source {
                    let depth = parent.len();
                    if path.len() > depth && path[..depth] == *parent {
                        if let PathSeg::Step(i) = path[depth] {
                            if i >= inserted {
                                path[depth] = PathSeg::Step(i + 1);
                            }
                        }
                    }
                }
                walk(&mut branch.steps, parent, inserted);
            }
        }
    }
    walk(&mut chain.steps, parent, inserted);
}

/// The step list a `parent` path refers to: an empty path means `chain`'s own top-level
/// `steps`; otherwise the path must alternate `Step`/`Branch` and *end* with a `Branch`, since
/// only a branch holds a step list. Used by both the chain editor (`ui/app.rs`) and the
/// execution engine to address a step at any depth uniformly.
///
/// Returns `None` for a path that breaks the alternation, rather than interpreting it — a
/// malformed path must be a miss, never a wrong hit on some other step.
pub fn steps_at<'a>(chain: &'a CdpChain, parent: &[PathSeg]) -> Option<&'a Vec<ChainStep>> {
    let mut steps = &chain.steps;
    let mut segs = parent.iter();
    while let Some(seg) = segs.next() {
        let PathSeg::Step(i) = seg else { return None };
        let step = steps.get(*i)?;
        let Some(PathSeg::Branch(b)) = segs.next() else { return None };
        steps = &step.branches.get(*b)?.steps;
    }
    Some(steps)
}

/// Mutable counterpart to [`steps_at`].
pub fn steps_at_mut<'a>(chain: &'a mut CdpChain, parent: &[PathSeg]) -> Option<&'a mut Vec<ChainStep>> {
    let mut steps = &mut chain.steps;
    let mut segs = parent.iter();
    while let Some(seg) = segs.next() {
        let PathSeg::Step(i) = seg else { return None };
        let step = steps.get_mut(*i)?;
        let Some(PathSeg::Branch(b)) = segs.next() else { return None };
        steps = &mut step.branches.get_mut(*b)?.steps;
    }
    Some(steps)
}

/// The single step at `path` — whose last element must be a `Step`. `None` if any segment
/// doesn't resolve, or the path doesn't end at a step.
pub fn step_at<'a>(chain: &'a CdpChain, path: &[PathSeg]) -> Option<&'a ChainStep> {
    let (&last, parent) = path.split_last()?;
    let PathSeg::Step(i) = last else { return None };
    steps_at(chain, parent)?.get(i)
}

/// Mutable counterpart to [`step_at`].
pub fn step_at_mut<'a>(chain: &'a mut CdpChain, path: &[PathSeg]) -> Option<&'a mut ChainStep> {
    let (&last, parent) = path.split_last()?;
    let PathSeg::Step(i) = last else { return None };
    steps_at_mut(chain, parent)?.get_mut(i)
}

/// The single branch at `path` — whose last element must be a `Branch`.
pub fn branch_at<'a>(chain: &'a CdpChain, path: &[PathSeg]) -> Option<&'a Branch> {
    let (&last, parent) = path.split_last()?;
    let PathSeg::Branch(b) = last else { return None };
    step_at(chain, parent)?.branches.get(b)
}

/// Mutable counterpart to [`branch_at`].
pub fn branch_at_mut<'a>(chain: &'a mut CdpChain, path: &[PathSeg]) -> Option<&'a mut Branch> {
    let (&last, parent) = path.split_last()?;
    let PathSeg::Branch(b) = last else { return None };
    step_at_mut(chain, parent)?.branches.get_mut(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::envelope_bank::{BankEnvelope, EnvelopeRef};

    fn catalog() -> CdpCatalog {
        let (catalog, warnings) = CdpCatalog::load(None);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        catalog
    }

    fn step(process_key: &str) -> ChainStep {
        ChainStep::new(process_key, Vec::new())
    }

    fn chain(steps: Vec<ChainStep>) -> CdpChain {
        CdpChain {
            name: "test".into(),
            steps,
            bank: EnvelopeBank::default(),
            output: ChainOutput::default(),
        }
    }

    #[test]
    fn empty_chain_is_rejected() {
        assert_eq!(chain(Vec::new()).validate(&catalog()), Err(ChainError::EmptyChain));
    }

    #[test]
    fn a_single_chainable_step_validates() {
        // blur_avrg: input = ana, output = ana -- single-input, chainable.
        assert_eq!(chain(vec![step("blur_avrg")]).validate(&catalog()), Ok(()));
    }

    #[test]
    fn a_multi_step_chain_of_chainable_processes_validates() {
        assert_eq!(
            chain(vec![step("blur_avrg"), step("focus_freeze_1")]).validate(&catalog()),
            Ok(())
        );
    }

    #[test]
    fn unknown_process_key_is_rejected() {
        assert_eq!(
            chain(vec![step("not_a_real_process")]).validate(&catalog()),
            Err(ChainError::UnknownProcess { key: "not_a_real_process".into() })
        );
    }

    #[test]
    fn a_synthesis_process_is_not_chainable() {
        // synth_wave_1: input = none -- nothing for a prior step's output to feed into.
        assert_eq!(
            chain(vec![step("synth_wave_1")]).validate(&catalog()),
            Err(ChainError::ProcessNotChainable { key: "synth_wave_1".into() })
        );
    }

    #[test]
    fn a_curve_process_is_not_chainable() {
        // repitch_quantise_1: input = curve, output = curve -- not audio at all.
        assert_eq!(
            chain(vec![step("repitch_quantise_1")]).validate(&catalog()),
            Err(ChainError::ProcessNotChainable { key: "repitch_quantise_1".into() })
        );
    }

    #[test]
    fn a_glob_output_process_is_not_chainable() {
        // distcut_distcut_1: output = wav_glob -- produces N new buffers, not one audio result.
        assert_eq!(
            chain(vec![step("distcut_distcut_1")]).validate(&catalog()),
            Err(ChainError::ProcessNotChainable { key: "distcut_distcut_1".into() })
        );
    }

    /// A dual-input process's second input is mandatory, so its one branch must exist. An
    /// *empty* `Buffer` branch is the "use the picked buffer as-is" case, which is what a
    /// dual-input step with no side-chain has always meant.
    #[test]
    fn a_dual_input_step_needs_its_one_branch_even_when_empty() {
        let cat = catalog();
        let bare = chain(vec![step("combine_mean_1")]);
        assert_eq!(
            bare.validate(&cat),
            Err(ChainError::MissingBranches { key: "combine_mean_1".into(), required: 1, actual: 0 })
        );

        let mut fixed = bare.clone();
        fixed.normalize_branches(&cat);
        assert_eq!(fixed.steps[0].branches.len(), 1);
        assert_eq!(fixed.steps[0].branches[0].source, BranchSource::Buffer);
        assert!(fixed.steps[0].branches[0].steps.is_empty(), "empty means 'use the pick as-is'");
        assert_eq!(fixed.validate(&cat), Ok(()));
    }

    #[test]
    fn normalize_branches_reaches_every_depth_and_is_idempotent() {
        let cat = catalog();
        let mut outer = step("combine_mean_1");
        outer.branches = vec![Branch { source: BranchSource::Tap, steps: vec![step("combine_mean_1")] }];
        let mut c = chain(vec![outer]);
        c.normalize_branches(&cat);
        let once = c.clone();
        c.normalize_branches(&cat);
        assert_eq!(c, once, "idempotent");
        assert_eq!(c.steps[0].branches[0].steps[0].branches.len(), 1, "nested step topped up");
        assert_eq!(c.validate(&cat), Ok(()));
    }

    #[test]
    fn a_dual_input_step_accepts_exactly_one_branch() {
        let mut dual = step("combine_mean_1");
        dual.branches = vec![Branch { source: BranchSource::Buffer, steps: vec![step("blur_avrg")] }];
        assert_eq!(chain(vec![dual]).validate(&catalog()), Ok(()));
    }

    #[test]
    fn a_second_branch_on_a_dual_input_step_is_rejected() {
        let mut dual = step("combine_mean_1");
        dual.branches = vec![Branch::buffer(), Branch::buffer()];
        assert_eq!(
            chain(vec![dual]).validate(&catalog()),
            Err(ChainError::TooManyBranches { key: "combine_mean_1".into(), arity: 1, actual: 2 })
        );
    }

    #[test]
    fn a_branch_on_a_single_input_step_is_rejected() {
        let mut single = step("blur_avrg");
        single.branches = vec![Branch::buffer()];
        assert_eq!(
            chain(vec![single]).validate(&catalog()),
            Err(ChainError::TooManyBranches { key: "blur_avrg".into(), arity: 0, actual: 1 })
        );
    }

    /// Nesting is allowed to [`MAX_SPLIT_DEPTH`] and refused past it — the model would carry any
    /// depth, but the editor draws each level as columns dividing the width, so the limit is
    /// where the picture stops being readable rather than where the data stops working.
    #[test]
    fn branches_nest_to_the_split_depth_limit_and_no_further() {
        let cat = catalog();
        let tap = || Branch { source: BranchSource::Tap, steps: Vec::new() };

        let mut inner = step("combine_mean_1");
        inner.branches = vec![tap()];
        let mut outer = step("combine_mean_1");
        outer.branches = vec![Branch { source: BranchSource::Buffer, steps: vec![inner.clone()] }];
        assert_eq!(chain(vec![outer.clone()]).validate(&cat), Ok(()), "two levels is the limit");

        // A third: the innermost step carrying a branch of its own.
        let mut third = step("combine_mean_1");
        third.branches = vec![tap()];
        let mut deep_inner = step("combine_mean_1");
        deep_inner.branches = vec![Branch { source: BranchSource::Tap, steps: vec![third] }];
        let mut deep_outer = step("combine_mean_1");
        deep_outer.branches = vec![Branch { source: BranchSource::Buffer, steps: vec![deep_inner] }];
        assert_eq!(
            chain(vec![deep_outer]).validate(&cat),
            Err(ChainError::SplitTooDeep { key: "combine_mean_1".into(), depth: MAX_SPLIT_DEPTH + 1 })
        );
    }

    #[test]
    fn a_single_input_step_two_levels_deep_still_cannot_carry_a_branch() {
        let mut bad_inner = step("blur_avrg");
        bad_inner.branches = vec![Branch::buffer()];
        let mut outer = step("combine_mean_1");
        outer.branches = vec![Branch { source: BranchSource::Buffer, steps: vec![bad_inner] }];
        assert_eq!(
            chain(vec![outer]).validate(&catalog()),
            Err(ChainError::TooManyBranches { key: "blur_avrg".into(), arity: 0, actual: 1 })
        );
    }

    // --- paths -----------------------------------------------------------------------

    #[test]
    fn paths_address_steps_and_branches_at_arbitrary_depth() {
        use PathSeg::{Branch as B, Step as S};
        let mut inner = step("blur_avrg");
        inner.values = vec![ParamValue::Number(7.0)];
        let mut middle = step("combine_mean_1");
        middle.branches = vec![Branch { source: BranchSource::Tap, steps: vec![inner] }];
        let mut outer = step("combine_mean_1");
        outer.branches = vec![Branch { source: BranchSource::Buffer, steps: vec![middle] }];
        let c = chain(vec![step("focus_freeze_1"), outer]);

        assert_eq!(steps_at(&c, &[]).unwrap().len(), 2);
        assert_eq!(step_at(&c, &[S(0)]).unwrap().process_key, "focus_freeze_1");
        assert_eq!(steps_at(&c, &[S(1), B(0)]).unwrap().len(), 1);
        assert_eq!(step_at(&c, &[S(1), B(0), S(0)]).unwrap().process_key, "combine_mean_1");
        assert_eq!(
            step_at(&c, &[S(1), B(0), S(0), B(0), S(0)]).unwrap().values,
            vec![ParamValue::Number(7.0)]
        );
        assert_eq!(branch_at(&c, &[S(1), B(0)]).unwrap().source, BranchSource::Buffer);
        assert_eq!(branch_at(&c, &[S(1), B(0), S(0), B(0)]).unwrap().source, BranchSource::Tap);

        assert!(step_at(&c, &[S(5)]).is_none(), "out-of-range index");
        assert!(branch_at(&c, &[S(0), B(0)]).is_none(), "focus_freeze_1 has no branch");
    }

    /// A path that breaks the Step/Branch alternation must miss rather than resolve to
    /// something else — the reason the accessors check rather than trust.
    #[test]
    fn a_malformed_path_resolves_to_nothing() {
        use PathSeg::{Branch as B, Step as S};
        let mut outer = step("combine_mean_1");
        outer.branches = vec![Branch { source: BranchSource::Tap, steps: vec![step("blur_avrg")] }];
        let c = chain(vec![outer]);

        assert!(step_at(&c, &[B(0)]).is_none(), "a path may not start with a branch");
        assert!(step_at(&c, &[S(0), S(0)]).is_none(), "two steps in a row");
        assert!(steps_at(&c, &[S(0)]).is_none(), "a step holds no step list of its own");
        assert!(branch_at(&c, &[S(0), B(0), S(0)]).is_none(), "does not end at a branch");
    }

    #[test]
    fn mutable_accessors_reach_the_same_places() {
        use PathSeg::{Branch as B, Step as S};
        let mut outer = step("combine_mean_1");
        outer.branches = vec![Branch { source: BranchSource::Tap, steps: vec![step("blur_avrg")] }];
        let mut c = chain(vec![outer]);

        steps_at_mut(&mut c, &[S(0), B(0)]).unwrap().push(step("focus_freeze_1"));
        assert_eq!(steps_at(&c, &[S(0), B(0)]).unwrap().len(), 2);
        step_at_mut(&mut c, &[S(0), B(0), S(1)]).unwrap().values = vec![ParamValue::Number(1.0)];
        assert_eq!(
            step_at(&c, &[S(0), B(0), S(1)]).unwrap().values,
            vec![ParamValue::Number(1.0)]
        );
        branch_at_mut(&mut c, &[S(0), B(0)]).unwrap().source = BranchSource::Buffer;
        assert_eq!(branch_at(&c, &[S(0), B(0)]).unwrap().source, BranchSource::Buffer);
    }

    // --- branch references -----------------------------------------------------------

    /// Availability follows the run order exactly: each step's branches in turn, then the step
    /// itself. So a later branch can read an earlier branch *and* an earlier step.
    #[test]
    fn a_branch_may_read_only_outputs_that_finish_before_it() {
        use PathSeg::{Branch as B, Step as S};
        let mut mixer = step("combine_mean_1");
        mixer.branches = vec![Branch { source: BranchSource::Tap, steps: Vec::new() }];
        let mut other = step("combine_mean_1");
        other.branches = vec![Branch { source: BranchSource::Tap, steps: Vec::new() }];
        let c = chain(vec![step("blur_avrg"), mixer, other]);

        // Nothing at all has finished before the very first thing runs.
        assert_eq!(outputs_available_to(&c, Some(&[S(1), B(0)])), vec![vec![S(0)]]);
        // By the third step's branch, the plain step, the second step's branch and the second
        // step itself have all finished — a step finishing *after* the branches feeding it.
        assert_eq!(
            outputs_available_to(&c, Some(&[S(2), B(0)])),
            vec![vec![S(0)], vec![S(1), B(0)], vec![S(1)]]
        );
        // With no target, everything, in completion order.
        assert_eq!(
            outputs_available_to(&c, None),
            vec![vec![S(0)], vec![S(1), B(0)], vec![S(1)], vec![S(2), B(0)], vec![S(2)]]
        );
    }

    /// A step's own output is not available to a branch that feeds it — the step has not run.
    /// A *step* target asks what has finished before that step runs at all — which is what the
    /// editor needs while a step is still being added and owns no branch to ask about yet.
    #[test]
    fn a_step_target_stops_before_that_steps_own_branches() {
        use PathSeg::{Branch as B, Step as S};
        let mut mixer = step("combine_mean_1");
        mixer.branches = vec![Branch { source: BranchSource::Tap, steps: Vec::new() }];
        let c = chain(vec![step("blur_avrg"), mixer, step("blur_avrg")]);

        // Before step 1 runs, only step 0 has finished — not step 1's own branch.
        assert_eq!(outputs_available_to(&c, Some(&[S(1)])), vec![vec![S(0)]]);
        // Before step 2 runs, step 1's branch and step 1 itself have.
        assert_eq!(
            outputs_available_to(&c, Some(&[S(2)])),
            vec![vec![S(0)], vec![S(1), B(0)], vec![S(1)]]
        );
    }

    #[test]
    fn a_branch_cannot_read_the_step_it_feeds() {
        use PathSeg::{Branch as B, Step as S};
        let mut mixer = step("combine_mean_1");
        mixer.branches = vec![Branch { source: BranchSource::From(vec![S(0)]), steps: Vec::new() }];
        assert_eq!(
            chain(vec![mixer]).validate(&catalog()),
            Err(ChainError::BranchNotAvailable { at: vec![S(0), B(0)], wanted: vec![S(0)] })
        );
    }

    /// Reading an earlier *step* is the case that motivated widening this past branches: a
    /// two-input process fed by something the chain already made, without recomputing it.
    #[test]
    fn a_branch_may_read_an_earlier_steps_output() {
        use PathSeg::{Branch as B, Step as S};
        let mut combine = step("combine_mean_1");
        combine.branches = vec![Branch { source: BranchSource::From(vec![S(0)]), steps: Vec::new() }];
        let c = chain(vec![step("blur_avrg"), combine]);
        assert_eq!(c.validate(&catalog()), Ok(()));
        assert_eq!(branch_at(&c, &[S(1), B(0)]).unwrap().source, BranchSource::From(vec![S(0)]));
    }

    /// Inserting a step ahead of one that a `From` names must move the reference with it.
    /// Left alone the path would keep its old index and so name whichever step took that slot —
    /// reading the wrong audio rather than failing.
    #[test]
    fn inserting_a_step_moves_the_branch_sources_that_named_later_ones() {
        use PathSeg::{Branch as B, Step as S};
        let mut combine = step("combine_mean_1");
        combine.branches = vec![Branch { source: BranchSource::From(vec![S(0)]), steps: Vec::new() }];
        let mut c = chain(vec![step("blur_avrg"), combine]);

        // A step goes in at the head: blur_avrg becomes step 1, the combiner step 2.
        c.steps.insert(0, step("phase_phase_1"));
        shift_branch_sources_for_insert(&mut c, &[], 0);

        assert_eq!(
            branch_at(&c, &[S(2), B(0)]).unwrap().source,
            BranchSource::From(vec![S(1)]),
            "the reference follows the step it named"
        );
        assert_eq!(c.validate(&catalog()), Ok(()));
    }

    /// A reference to a step *before* the insertion point does not move, and neither does one
    /// in a different list that happens to share an index.
    #[test]
    fn inserting_a_step_leaves_earlier_and_unrelated_branch_sources_alone() {
        use PathSeg::{Branch as B, Step as S};
        let mut combine = step("combine_mean_1");
        combine.branches = vec![Branch { source: BranchSource::From(vec![S(0)]), steps: Vec::new() }];
        let mut c = chain(vec![step("blur_avrg"), combine]);

        // Inserting *after* both: nothing shifts.
        c.steps.push(step("phase_phase_1"));
        shift_branch_sources_for_insert(&mut c, &[], 2);
        assert_eq!(branch_at(&c, &[S(1), B(0)]).unwrap().source, BranchSource::From(vec![S(0)]));

        // And an insert into a *branch's* list leaves the top-level reference untouched, even
        // though both name index 0.
        shift_branch_sources_for_insert(&mut c, &[S(1), B(0)], 0);
        assert_eq!(branch_at(&c, &[S(1), B(0)]).unwrap().source, BranchSource::From(vec![S(0)]));
    }

    #[test]
    fn sibling_branches_finish_left_to_right_so_b_may_read_a() {
        use PathSeg::{Branch as B, Step as S};
        let mut mixer = step("combine_mean_1");
        // Two branches on one step, which only a native combiner really allows -- built by hand
        // here to exercise the ordering rule itself.
        mixer.branches = vec![Branch { source: BranchSource::Tap, steps: Vec::new() }, Branch { source: BranchSource::Tap, steps: Vec::new() }];
        let c = chain(vec![mixer]);

        assert_eq!(outputs_available_to(&c, Some(&[S(0), B(0)])), Vec::<Path>::new());
        assert_eq!(outputs_available_to(&c, Some(&[S(0), B(1)])), vec![vec![S(0), B(0)]]);
    }

    #[test]
    fn reading_a_branch_that_has_not_finished_is_rejected() {
        use PathSeg::{Branch as B, Step as S};
        let cat = catalog();
        let mut first = step("combine_mean_1");
        let mut second = step("combine_mean_1");
        second.branches = vec![Branch { source: BranchSource::Tap, steps: Vec::new() }];

        // Backwards: the *first* step's branch reading the second's, which runs later.
        first.branches = vec![Branch { source: BranchSource::From(vec![S(1), B(0)]), steps: Vec::new() }];
        let c = chain(vec![first.clone(), second.clone()]);
        assert_eq!(
            c.validate(&cat),
            Err(ChainError::BranchNotAvailable { at: vec![S(0), B(0)], wanted: vec![S(1), B(0)] })
        );

        // A branch cannot read itself either -- the same rule, and why no cycle is expressible.
        let mut selfish = step("combine_mean_1");
        selfish.branches = vec![Branch { source: BranchSource::From(vec![S(0), B(0)]), steps: Vec::new() }];
        assert!(matches!(
            chain(vec![selfish]).validate(&cat),
            Err(ChainError::BranchNotAvailable { .. })
        ));

        // Forwards is fine.
        second.branches = vec![Branch { source: BranchSource::From(vec![S(0), B(0)]), steps: Vec::new() }];
        first.branches = vec![Branch { source: BranchSource::Tap, steps: Vec::new() }];
        assert_eq!(chain(vec![first, second]).validate(&cat), Ok(()));
    }

    #[test]
    fn a_branch_reference_round_trips_through_toml() {
        use PathSeg::{Branch as B, Step as S};
        let mut first = step("combine_mean_1");
        first.branches = vec![Branch { source: BranchSource::Tap, steps: Vec::new() }];
        let mut second = step("combine_mean_1");
        second.branches = vec![Branch { source: BranchSource::From(vec![S(0), B(0)]), steps: Vec::new() }];
        let c = chain(vec![first, second]);
        let back: CdpChain = toml::from_str(&toml::to_string(&c).expect("serialize")).expect("deserialize");
        assert_eq!(c, back);
    }

    // --- envelopes -------------------------------------------------------------------

    #[test]
    fn a_raw_breakpoint_envelope_is_rejected_inside_a_chain() {
        let mut s = step("blur_avrg");
        s.values = vec![ParamValue::Breakpoints(vec![(0.0, 1.0), (10.0, 4.0)])];
        assert_eq!(
            chain(vec![s]).validate(&catalog()),
            Err(ChainError::RawEnvelopeInChain { key: "blur_avrg".into(), param: 0 })
        );
    }

    #[test]
    fn an_envelope_reference_validates_only_against_a_bank_that_holds_it() {
        let mut s = step("blur_avrg");
        s.values = vec![ParamValue::EnvelopeRef(EnvelopeRef {
            name: "swell".into(),
            min: 1.0,
            max: 100.0,
            invert: false,
        })];
        let mut c = chain(vec![s]);
        assert_eq!(
            c.validate(&catalog()),
            Err(ChainError::Bank(BankError::UnknownEnvelope { name: "swell".into() }))
        );

        c.bank.envelopes.push(BankEnvelope {
            name: "swell".into(),
            points: vec![(0.0, 0.0), (1.0, 1.0)],
        });
        assert_eq!(c.validate(&catalog()), Ok(()));
    }

    #[test]
    fn a_bad_bank_fails_the_whole_chain() {
        let mut c = chain(vec![step("blur_avrg")]);
        // Absolute seconds smuggled into the bank -- exactly what the bank exists to prevent.
        c.bank.envelopes.push(BankEnvelope { name: "s".into(), points: vec![(0.0, 0.0), (10.0, 1.0)] });
        assert!(matches!(c.validate(&catalog()), Err(ChainError::Bank(_))));
    }

    // --- persistence -----------------------------------------------------------------

    #[test]
    fn chain_with_branches_and_a_bank_round_trips_through_toml() {
        let mut dual = step("combine_mean_1");
        dual.values = vec![ParamValue::EnvelopeRef(EnvelopeRef {
            name: "swell".into(),
            min: 0.0,
            max: 1.0,
            invert: true,
        })];
        dual.branches = vec![Branch {
            source: BranchSource::Tap,
            steps: vec![ChainStep::new("blur_avrg", vec![ParamValue::Number(4.0)])],
        }];
        let c = CdpChain {
            name: "Round Trip".into(),
            steps: vec![dual],
            output: ChainOutput::NewBuffer,
            bank: EnvelopeBank {
                envelopes: vec![BankEnvelope {
                    name: "swell".into(),
                    points: vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.2)],
                }],
            },
        };

        let text = toml::to_string(&c).expect("serialize");
        let back: CdpChain = toml::from_str(&text).expect("deserialize");
        assert_eq!(c, back);
    }

    /// A chain saved before branches existed must load and mean exactly what it meant then: a
    /// side-chain was always fed by a separately picked buffer, so it migrates to
    /// `BranchSource::Buffer` and never to `Tap`.
    #[test]
    fn a_legacy_side_chain_preset_migrates_to_a_buffer_branch() {
        let legacy = r#"
name = "legacy"

[[steps]]
process_key = "combine_mean_1"
values = []

[[steps.side_chain]]
process_key = "blur_avrg"
values = []
"#;
        let mut c: CdpChain = toml::from_str(legacy).expect("deserialize legacy");
        assert_eq!(c.steps[0].legacy_side_chain.len(), 1, "read into the shim before migrating");
        assert!(c.steps[0].branches.is_empty());

        c.migrate_legacy();

        assert!(c.steps[0].legacy_side_chain.is_empty(), "shim emptied");
        assert_eq!(c.steps[0].branches.len(), 1);
        assert_eq!(c.steps[0].branches[0].source, BranchSource::Buffer);
        assert_eq!(c.steps[0].branches[0].steps[0].process_key, "blur_avrg");
        assert_eq!(c.validate(&catalog()), Ok(()));
    }

    /// The shape every real saved chain on disk actually has: a Praat step, plain values, and an
    /// *empty* `side_chain` written by the pre-branches format. It must load, migrate to no
    /// branches at all (an empty side-chain was never a branch), and validate.
    #[test]
    fn a_real_pre_branches_preset_loads_and_validates_unchanged() {
        let legacy = r#"
name = "AudioTools chain_1"

[[steps]]
process_key = "blur_avrg"
side_chain = []

[[steps.values]]
Number = 25.0
"#;
        let mut c: CdpChain = toml::from_str(legacy).expect("deserialize");
        c.migrate_legacy();
        let cat = catalog();
        c.normalize_branches(&cat);

        assert!(c.steps[0].branches.is_empty(), "an empty side-chain is not a branch");
        assert_eq!(c.steps[0].values, vec![ParamValue::Number(25.0)]);
        assert_eq!(c.output, ChainOutput::Splice, "a preset with no `output` splices, as it did");
        assert!(c.bank.envelopes.is_empty());
        assert_eq!(c.validate(&cat), Ok(()));
    }

    /// A chain saved with an envelope predates the bank, and `validate` refuses a raw one — so
    /// without migration such a preset would simply stop running. The shape survives; only its
    /// absolute time and value axes, which were never recoverable, are normalized away.
    #[test]
    fn a_saved_raw_envelope_is_lifted_into_the_bank_on_load() {
        let legacy = r#"
name = "automated"

[[steps]]
process_key = "blur_avrg"
side_chain = []

[[steps.values]]
Breakpoints = [[0.0, 4.0], [5.0, 40.0], [10.0, 4.0]]
"#;
        let mut c: CdpChain = toml::from_str(legacy).expect("deserialize");
        c.migrate_legacy();

        assert_eq!(c.bank.envelopes.len(), 1, "the curve moved into the bank");
        assert_eq!(c.bank.envelopes[0].name, "Env 1");
        // Normalized by its own span on both axes: the shape is intact, the units are gone.
        assert_eq!(c.bank.envelopes[0].points, vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]);
        assert_eq!(c.bank.validate(), Ok(()));

        let ParamValue::EnvelopeRef(reference) = &c.steps[0].values[0] else {
            panic!("the step now references it")
        };
        assert_eq!(reference.name, "Env 1");
        // Read back through its own window, the curve produces what it always did.
        assert_eq!(c.bank.produced_span(reference), Some((4.0, 40.0)));
        assert_eq!(c.validate(&catalog()), Ok(()));
    }

    #[test]
    fn migration_reaches_every_depth_and_is_idempotent() {
        let legacy = r#"
name = "nested legacy"

[[steps]]
process_key = "combine_mean_1"
values = []

[[steps.side_chain]]
process_key = "combine_mean_1"
values = []

[[steps.side_chain.side_chain]]
process_key = "blur_avrg"
values = []
"#;
        let mut c: CdpChain = toml::from_str(legacy).expect("deserialize legacy");
        c.migrate_legacy();
        let once = c.clone();
        c.migrate_legacy();
        assert_eq!(c, once, "migration is idempotent");

        let inner = &c.steps[0].branches[0].steps[0];
        assert_eq!(inner.branches.len(), 1);
        assert_eq!(inner.branches[0].source, BranchSource::Buffer);
        assert_eq!(inner.branches[0].steps[0].process_key, "blur_avrg");
    }

    /// The shim is read-only: once migrated, saving must never write `side_chain` back out.
    #[test]
    fn the_legacy_field_is_never_serialized() {
        let mut s = step("combine_mean_1");
        s.legacy_side_chain = vec![step("blur_avrg")];
        let text = toml::to_string(&chain(vec![s])).expect("serialize");
        assert!(!text.contains("side_chain"), "unexpected legacy field in:\n{text}");
    }

}
