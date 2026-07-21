//! CDP (Composer's Desktop Project) process catalog and pipeline planning — pure data and
//! logic, no process spawning (see `src/cdp/runner.rs`) and no UI.

pub mod catalog;
pub mod def;
pub mod pipeline;
pub mod preset;
pub mod recent;

pub use catalog::CdpCatalog;
pub use def::{Category, HiliteBandRow, IoKind, ParamDef, ParamKind, ParamValue, ProcessDef, TableColumn};
pub use pipeline::{
    plan_extract_formants, plan_extract_pitch_curve, plan_job, plan_oneform_get, FormantExtractionMode, InputSpec,
    PlanError, PvocSettings,
};
