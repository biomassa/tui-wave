//! Safe Rust over the Airwindows C API in `shim.cpp`. This is the only module that touches
//! the FFI; everything above it sees ordinary Rust types.
//!
//! **Depends on nothing else in this crate, deliberately.** `src/bin/dump-airwindows-catalog.rs`
//! pulls this file in directly with `#[path]` to generate the catalog, and it can only do that
//! while the file's dependencies are `std` alone — the moment it reaches for anything under
//! `crate::`, that binary drags in the entire app to ask a plugin its parameter names.

use std::ffi::{c_char, c_float, c_int, c_void, CStr};
use std::sync::Mutex;

unsafe extern "C" {
    fn aw_count() -> c_int;
    fn aw_name(i: c_int) -> *const c_char;
    fn aw_category(i: c_int) -> *const c_char;
    fn aw_description(i: c_int) -> *const c_char;
    fn aw_nparams(i: c_int) -> c_int;
    fn aw_is_mono(i: c_int) -> c_int;
    fn aw_create(i: c_int, sample_rate: c_float) -> *mut c_void;
    fn aw_destroy(h: *mut c_void);
    fn aw_param_name(h: *mut c_void, idx: c_int, buf: *mut c_char, len: c_int);
    fn aw_param_display(h: *mut c_void, idx: c_int, buf: *mut c_char, len: c_int);
    fn aw_param_label(h: *mut c_void, idx: c_int, buf: *mut c_char, len: c_int);
    fn aw_set_param(h: *mut c_void, idx: c_int, v: c_float);
    fn aw_get_param(h: *mut c_void, idx: c_int) -> c_float;
    fn aw_process(
        h: *mut c_void,
        in_l: *mut c_float,
        in_r: *mut c_float,
        out_l: *mut c_float,
        out_r: *mut c_float,
        frames: c_int,
    );
}

/// Serializes `aw_create`. The C++ sets a *static* `defaultSampleRate` immediately before
/// running the generator, because a plugin constructor may read the sample rate while
/// computing its initial state -- so two threads constructing at once could hand one of them
/// the other's rate. Nothing else in this module needs a lock: the registry is read-only
/// after static init, and every other call takes an instance the caller already owns.
static CREATE_LOCK: Mutex<()> = Mutex::new(());

/// Longest string a plugin's `getParameter*` can produce, plus room. The C++ side clamps to
/// whatever it is handed; this only has to be large enough not to truncate real values.
const TEXT_LEN: usize = 128;

/// One plugin as the registry describes it. Every field borrows storage that lives for the
/// life of the process, so this costs no allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginInfo {
    /// The plugin's own name, e.g. `"Density"`. Unique across the registry, and what the
    /// catalog uses as its `bin`.
    pub name: &'static str,
    /// Chris Johnson's own category, e.g. `"Brightness"`, `"Consoles"`, `"Reverb"` -- the
    /// browser's group column is built from these (see `model::cdp::group`).
    pub category: &'static str,
    /// The one-line description from `Airwindopedia.txt`.
    pub description: &'static str,
    pub n_params: usize,
    /// True when the plugin's processing is channel-independent. Note this does *not* mean
    /// "takes one channel": every plugin has two legs regardless. It means the two legs do
    /// not interact, so the result is the same whether the legs are fed together or apart.
    pub is_mono: bool,
}

pub fn plugin_count() -> usize {
    // SAFETY: reads a `std::vector`'s size; the registry is fully populated by static
    // initializers before `main` and never mutated afterwards.
    unsafe { aw_count().max(0) as usize }
}

pub fn plugin_info(index: usize) -> Option<PluginInfo> {
    if index >= plugin_count() {
        return None;
    }
    let i = index as c_int;
    // SAFETY: `index` is in range, so each accessor returns a pointer into a registry
    // `std::string` that outlives the process and is never mutated after static init.
    unsafe {
        Some(PluginInfo {
            name: borrow(aw_name(i))?,
            category: borrow(aw_category(i))?,
            description: borrow(aw_description(i))?,
            n_params: aw_nparams(i).max(0) as usize,
            is_mono: aw_is_mono(i) != 0,
        })
    }
}

/// Every plugin in registry order. The order is `ModuleAdd.h`'s, which is alphabetical by
/// name -- stable across builds, which matters because the generated catalog is keyed by it.
pub fn plugins() -> impl Iterator<Item = PluginInfo> {
    (0..plugin_count()).filter_map(plugin_info)
}

pub fn find_by_name(name: &str) -> Option<usize> {
    (0..plugin_count()).find(|&i| plugin_info(i).is_some_and(|p| p.name == name))
}

/// SAFETY: `p` must be null or a pointer to a NUL-terminated string valid for `'static`.
unsafe fn borrow(p: *const c_char) -> Option<&'static str> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

/// A live plugin instance, owning its DSP state.
///
/// Not `Sync`: the C++ object holds mutable filter/delay state that `process` writes through
/// a shared reference's worth of aliasing on the C side. It *is* `Send`, which is what the
/// runner needs -- an instance is created on and confined to the worker thread.
pub struct Instance {
    handle: *mut c_void,
    n_params: usize,
}

// SAFETY: the instance owns all its state, holds no thread-local or global references, and
// the only global it touches (`defaultSampleRate`) is read once during construction under
// `CREATE_LOCK`. Moving it between threads is sound; sharing it is not, hence no `Sync`.
unsafe impl Send for Instance {}

impl Instance {
    /// `None` if `index` is out of range, if the sample rate is one the base class rejects
    /// (it asserts above 2000 Hz), or if the generator yields nothing.
    pub fn new(index: usize, sample_rate: u32) -> Option<Self> {
        let info = plugin_info(index)?;
        let _guard = CREATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: index is in range (checked by `plugin_info`), and the lock makes the
        // static-sample-rate write and the generator call one critical section.
        let handle = unsafe { aw_create(index as c_int, sample_rate as c_float) };
        (!handle.is_null()).then_some(Self { handle, n_params: info.n_params })
    }

    pub fn n_params(&self) -> usize {
        self.n_params
    }

    /// Parameters are always normalized 0.0-1.0; the plugin maps that to its own units
    /// internally, and `param_display` is the only way to see the result.
    pub fn set_param(&mut self, index: usize, value: f32) {
        if index < self.n_params {
            // SAFETY: valid handle, index checked against the plugin's own count.
            unsafe { aw_set_param(self.handle, index as c_int, value.clamp(0.0, 1.0)) }
        }
    }

    /// The current normalized value. Read immediately after construction this is the
    /// plugin's *default* — which exists nowhere else, since upstream sets defaults as bare
    /// assignments in the constructor body rather than declaring them.
    pub fn get_param(&self, index: usize) -> f32 {
        if index >= self.n_params {
            return 0.0;
        }
        // SAFETY: valid handle, index checked against the plugin's own count.
        unsafe { aw_get_param(self.handle, index as c_int) }
    }

    pub fn param_name(&self, index: usize) -> String {
        self.text(index, aw_param_name)
    }

    /// The plugin's rendering of the *current* value in its own units -- e.g. `"-6.0000"`
    /// for a gain, where the stored parameter is 0.5. This is where the normalized-to-real
    /// mapping lives; it exists nowhere in declarative form, which is why the dialog asks
    /// the plugin rather than the catalog.
    pub fn param_display(&self, index: usize) -> String {
        self.text(index, aw_param_display)
    }

    /// The unit suffix, e.g. `"dB"`. Usually empty.
    pub fn param_label(&self, index: usize) -> String {
        self.text(index, aw_param_label)
    }

    fn text(
        &self,
        index: usize,
        f: unsafe extern "C" fn(*mut c_void, c_int, *mut c_char, c_int),
    ) -> String {
        if index >= self.n_params {
            return String::new();
        }
        let mut buf = [0u8; TEXT_LEN];
        // SAFETY: valid handle, in-range index, and `buf` is exactly `TEXT_LEN` writable
        // bytes. The C side always NUL-terminates within the length it is given.
        unsafe { f(self.handle, index as c_int, buf.as_mut_ptr().cast(), TEXT_LEN as c_int) };
        let end = buf.iter().position(|&b| b == 0).unwrap_or(TEXT_LEN);
        String::from_utf8_lossy(&buf[..end]).trim().to_string()
    }

    /// Renders one block. All four slices must be the same length; input and output must not
    /// be the same buffers (several plugins read input ahead of the write position).
    ///
    /// Inputs are `&mut` because the C signature is `float**` -- the plugins do not in fact
    /// write to them, but asserting that through a `*const` cast would be a claim this side
    /// cannot verify across 517 vendored translation units.
    pub fn process(
        &mut self,
        in_l: &mut [f32],
        in_r: &mut [f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let frames = in_l.len().min(in_r.len()).min(out_l.len()).min(out_r.len());
        if frames == 0 {
            return;
        }
        // SAFETY: valid handle; four distinct, non-overlapping slices each at least `frames`
        // long (borrowck guarantees they cannot alias, since all four are `&mut`).
        unsafe {
            aw_process(
                self.handle,
                in_l.as_mut_ptr(),
                in_r.as_mut_ptr(),
                out_l.as_mut_ptr(),
                out_r.as_mut_ptr(),
                frames as c_int,
            )
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the handle came from `aw_create`, is non-null (checked in `new`), and is
        // owned solely by this value -- `Instance` is not `Clone`.
        unsafe { aw_destroy(self.handle) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_populated() {
        // The static initializers in `ModuleAdd.h` run before any test does. A zero here
        // means they were stripped -- which is exactly what happens if the archive is linked
        // without `--whole-archive` and nothing references those symbols.
        assert!(plugin_count() > 400, "only {} plugins registered", plugin_count());
    }

    #[test]
    fn every_plugin_has_a_name_a_category_and_a_description() {
        for p in plugins() {
            assert!(!p.name.is_empty());
            assert!(!p.category.is_empty(), "{} has no category", p.name);
            assert!(!p.description.is_empty(), "{} has no description", p.name);
        }
    }

    #[test]
    fn plugin_names_are_unique() {
        let mut names: Vec<&str> = plugins().map(|p| p.name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate plugin names in the registry");
    }

    /// Upstream's `getParameterName` is a `switch` with one `vst_strncpy` per case, and a
    /// missing `break` would silently give *every* parameter the last case's name — shipping a
    /// whole catalog of mislabelled controls with nothing to see wrong. The breaks are present
    /// throughout the vendored sources; this asserts it so a submodule bump that regresses it
    /// fails loudly.
    ///
    /// The check is "not all identical" rather than "all distinct", because **repeated names
    /// are legitimate upstream**: `BezEQ` really does name two of its five parameters `"x"`
    /// (placeholders between Treble/Mid/Bass), and it is not alone. All-identical is the
    /// fallthrough signature specifically; merely-repeated is Chris Johnson's own labelling and
    /// not ours to override.
    #[test]
    fn a_plugin_does_not_give_every_parameter_the_same_name() {
        for (i, p) in plugins().enumerate() {
            if p.n_params < 2 {
                continue;
            }
            let Some(inst) = Instance::new(i, 48_000) else {
                panic!("{} failed to instantiate", p.name)
            };
            let names: Vec<String> = (0..p.n_params).map(|k| inst.param_name(k)).collect();
            assert!(
                names.iter().any(|n| *n != names[0]),
                "{} names all {} of its parameters {:?} — a `break` is missing from its \
                 getParameterName switch",
                p.name,
                p.n_params,
                names[0]
            );
        }
    }

    #[test]
    fn every_plugin_instantiates_and_renders_finite_audio() {
        let frames = 512;
        for (i, p) in plugins().enumerate() {
            let Some(mut inst) = Instance::new(i, 48_000) else {
                panic!("{} failed to instantiate", p.name)
            };
            let mut l: Vec<f32> = (0..frames)
                .map(|n| (n as f32 * 0.05).sin() * 0.25)
                .collect();
            let mut r = l.clone();
            let mut ol = vec![0.0f32; frames];
            let mut or = vec![0.0f32; frames];
            inst.process(&mut l, &mut r, &mut ol, &mut or);
            for (ch, buf) in [("L", &ol), ("R", &or)] {
                assert!(
                    buf.iter().all(|s| s.is_finite()),
                    "{} produced a non-finite sample on {ch}",
                    p.name
                );
            }
        }
    }
}
