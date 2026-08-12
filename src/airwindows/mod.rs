//! The Airwindows execution layer — the counterpart of `src/cdp/` and `src/praat/`.
//!
//! Unlike those two it spawns nothing: an Airwindows process is a function call into C++ that
//! `build.rs` compiled into this binary, which is why there is no binary to locate, no temp
//! WAV to write, no timeout to enforce, and nothing at all for the user to install. Planning
//! lives in `model::airwindows`, keeping the same three-layer split the other backends have.

pub mod runner;

/// Exactly what the editor reaches for: a name to look up (`cdp_run` and the parameter readout
/// both resolve a catalog entry's `bin`), the registry entry behind an index, and an instance to
/// render with. The rest of `airwindows-sys`'s surface is for the catalog generator, which
/// depends on that crate directly rather than coming through here.
pub use airwindows_sys::{find_by_name, plugin_info, Instance};
