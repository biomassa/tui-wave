//! The Airwindows execution layer — the counterpart of `src/cdp/` and `src/praat/`.
//!
//! Unlike those two it spawns nothing: an Airwindows process is a function call into C++ that
//! `build.rs` compiled into this binary, which is why there is no binary to locate, no temp
//! WAV to write, no timeout to enforce, and nothing at all for the user to install. Planning
//! lives in `model::airwindows`, keeping the same three-layer split the other backends have.

mod ffi;
pub mod runner;

/// Exactly what the app reaches for: a name to look up (`cdp_run` and the parameter readout
/// both resolve a catalog entry's `bin`), the registry entry behind an index, and an instance
/// to render with. The rest of `ffi`'s surface exists for the catalog generator and stays
/// private to the module — re-exporting it here would only produce unused-import warnings that
/// say nothing about this app.
pub use ffi::{find_by_name, plugin_info, Instance};
