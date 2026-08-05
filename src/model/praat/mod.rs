//! Praat / praatAudioTools process planning — pure data and logic, no process spawning (see
//! `src/praat/runner.rs`) and no UI.
//!
//! The counterpart of `model::cdp` for the second external backend. The two share a catalog and
//! a `ProcessDef` (a Praat entry is one with `Category::Praat`, see `ProcessDef::backend`), and
//! diverge only here, at the point where a definition becomes something to execute: CDP puts
//! its arguments straight on a per-process binary's argv, while Praat has a single binary that
//! must be handed a generated script.

pub mod builtin;
pub mod driver;
pub mod plan;
pub mod python;
pub mod rewrite;

/// The four-argument form is the one production uses; `plan::plan_praat_job` remains for the
/// callers and tests that have nothing to do with Python. See its doc comment.
pub use plan::plan_praat_job_with;
