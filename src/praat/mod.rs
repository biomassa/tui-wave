//! Running Praat as an external process. The execution half of the Praat backend; the pure
//! planning half is `model::praat`.

pub mod runner;

pub use runner::{
    praat_bin_for, probe_praat, validate_audiotools_dir, PraatError, PraatEvent, PraatJob,
    PraatRunner, DEFAULT_TIMEOUT,
};
