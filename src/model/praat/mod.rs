//! Praat / praatAudioTools process planning — pure data and logic, no process spawning (see
//! `src/praat/runner.rs`) and no UI.
//!
//! The counterpart of `model::cdp` for the second external backend. The two share a catalog and
//! a `ProcessDef` (a Praat entry is one with `Category::Praat`, see `ProcessDef::backend`), and
//! diverge only here, at the point where a definition becomes something to execute: CDP puts
//! its arguments straight on a per-process binary's argv, while Praat has a single binary that
//! must be handed a generated script.

pub mod driver;
pub mod plan;

pub use driver::{driver_script, praat_string_literal, DriverArg, DriverError};
pub use plan::{
    input_wav_name, plan_praat_job, praat_input_count, PraatPlanError, PraatPlannedJob,
    DRIVER_SCRIPT, OUTPUT_WAV,
};
