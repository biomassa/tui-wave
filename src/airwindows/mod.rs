//! The Airwindows execution layer — the counterpart of `src/cdp/` and `src/praat/`.
//!
//! Unlike those two it spawns nothing: an Airwindows process is a function call into C++ that
//! `build.rs` compiled into this binary, which is why there is no binary to locate, no temp
//! WAV to write, no timeout to enforce, and nothing at all for the user to install. Planning
//! lives in `model::airwindows`, keeping the same three-layer split the other backends have.

mod ffi;
pub mod runner;

pub use ffi::{find_by_name, plugin_count, plugin_info, plugins, Instance, PluginInfo};
