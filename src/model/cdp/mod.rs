//! CDP (Composer's Desktop Project) process catalog and pipeline planning — pure data and
//! logic, no process spawning (see `src/cdp/runner.rs`) and no UI.

pub mod catalog;
pub mod chain;
pub mod chain_last;
pub mod chain_preset;
pub mod chain_recent;
pub mod def;
pub mod envelope_bank;
pub mod envelope_preset;
pub mod group;
pub mod input_buffers;
pub mod native;
pub mod pipeline;
pub mod preset;
pub mod process_last;
pub mod recent;

pub use catalog::CdpCatalog;
pub use chain::{
    branch_at, branch_at_mut, step_at, step_at_mut, steps_at, steps_at_mut, BranchSource, CdpChain, ChainError, ChainOutput,
    ChainStep, Path, PathSeg,
};
pub use envelope_bank::{BankEnvelope, BankError, EnvelopeBank, EnvelopeRef};
pub use group::{cdp_group, groups_for, CdpGroup};
pub use def::{
    Category, CrystalVdat, HiliteBandRow, IoKind, ParamDef, ParamKind, ParamNote, ParamValue,
    ProcessDef, TableColumn,
};
pub use pipeline::{
    plan_ana_chain, plan_extract_formants, plan_extract_pitch_curve, plan_job, plan_oneform_get,
    FormantExtractionMode, InputSpec, PlanError, PvocSettings,
};
