//! CDP (Composer's Desktop Project) process catalog and pipeline planning — pure data and
//! logic, no process spawning (see `src/cdp/runner.rs`) and no UI.

pub mod catalog;
pub mod def;
pub mod pipeline;
pub mod preset;
pub mod recent;

pub use catalog::CdpCatalog;
pub use def::{IoKind, ParamDef, ParamKind, ParamValue, ProcessDef};
pub use pipeline::{plan_job, InputSpec, PlanError, PvocSettings};
