//! Compiles the Airwindows DSP (via airwin2rack's consolidated sources) and the `src/shim.cpp`
//! C API over it into a static library.
//!
//! This is the only C++ in the project and the only reason a build script exists. It is also
//! the first dependency whose *code* ships inside the release artifacts rather than being
//! installed separately by the user, which is why `THIRD_PARTY_NOTICES.md` grew a section
//! that CDP and Praat did not need.
//!
//! **Why this is its own crate.** Cargo bakes a package's version into its build-script unit
//! hash, so while this file belonged to `tui-wave`, every release bump produced a fresh
//! `OUT_DIR` and recompiled all ~1040 translation units from scratch -- four full rebuilds
//! across a single afternoon of releases, around eight minutes and 28MB of archive apiece,
//! for a version string that no C++ here can observe. Living in a separately-versioned crate
//! means the archive is rebuilt when the vendored submodule or `shim.cpp` changes and not
//! otherwise, which is what the `rerun-if-changed` list below has always *said*; the crate
//! boundary is what finally makes it true.
//!
//! The plugin sources are taken from the submodule's committed `src/autogen_airwin/`, not
//! generated here: airwin2rack's `scripts/import.pl` has already done the transformation from
//! upstream Airwindows (swapping the VST2 SDK include for its own ~90-line shim header,
//! namespacing each plugin, dropping `getChunk`/`setChunk`) and committed the result. So no
//! Perl, no VST SDK, and no nested `libs/airwindows` checkout is needed at build time -- the
//! submodule is cloned non-recursively on purpose.
//!
//! **A release build reuses the compiled archive from a small cache.** The archive is about
//! 28 MB, and compiling it takes minutes. `setup.sh` builds in a temporary target directory
//! and deletes it, so without a cache every install compiles all of it again. The cache is one
//! file in `~/.cache/tui-wave/airwindows/` (or `$XDG_CACHE_HOME`), named by a hash of everything
//! that decides the archive: the vendored sources, `shim.cpp`, the compiler, its flags, and the
//! target. A different input gives a different name, so a stale archive is never linked. The
//! cache is only for release builds. It is safe to delete, and any cache error is ignored.
//! `TUI_WAVE_AIRWINDOWS_CACHE_DIR` moves it, and `TUI_WAVE_NO_BUILD_CACHE=1` turns it off.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Relative to the *workspace* root, which is two levels above this crate's manifest.
const SUBMODULE: &str = "third_party/airwin2rack";

/// Part of the cache key. Change it when this file changes how the archive is made, so that an
/// archive from the old logic is never reused.
const CACHE_FORMAT: &str = "1";

fn main() {
    // `CARGO_MANIFEST_DIR` is `crates/airwindows-sys`; the vendored submodule sits at the
    // workspace root beside `third_party/praat-audiotools`, so climb out of the crate first.
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = crate_root
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/airwindows-sys sits two levels below the workspace root")
        .to_path_buf();
    let aw = workspace_root.join(SUBMODULE);
    let src = aw.join("src");
    let autogen = src.join("autogen_airwin");

    if !autogen.is_dir() {
        // Reached by two routes, and the second is the one worth naming: a clone made without
        // submodules, or -- far more often -- an existing clone updated with `git pull`, which
        // does not fetch a submodule that was added since you cloned. The message says so
        // because the first thing a user does with "directory is missing" is check whether the
        // directory is missing, which tells them nothing.
        panic!(
            "{} is missing.\n\n\
             The Airwindows backend is compiled from a git submodule, which is not fetched by\n\
             `git clone` or `git pull` on their own. From the repository root, run:\n\
             \n    git submodule update --init\n\n\
             (Plain `--init`, not `--init --recursive`: {SUBMODULE} declares submodules of its\n\
             own that this project never reads.)\n",
            autogen.display()
        );
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(&src)
        .file(crate_root.join("src/shim.cpp"))
        .file(src.join("airwin_consolidated_base.cpp"));

    // Every `<Name>.cpp` and `<Name>Proc.cpp` in the autogen tree. Collected by walking the
    // directory rather than by parsing the submodule's `CMakeLists.txt`, so a submodule bump
    // that adds plugins needs no change here -- and sorted, so the archive member order (and
    // therefore the build's reproducibility) does not depend on filesystem iteration order.
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&autogen)
        .unwrap_or_else(|e| panic!("reading {}: {e}", autogen.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cpp"))
        .collect();
    sources.sort();
    assert!(
        !sources.is_empty(),
        "{} contains no .cpp files",
        autogen.display()
    );
    let plugin_sources = sources.len();
    build.files(&sources);

    // Upstream Airwindows is 2011-era code that predates most of these being errors, and it
    // is vendored verbatim on purpose -- we do not own it and will not patch it, so the
    // warnings are noise on every single build. Silenced rather than fixed for exactly the
    // reason CLAUDE.md gives for *not* silencing our own `dead_code`: these say nothing about
    // this project's health.
    build
        .warnings(false)
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-sign-compare")
        .flag_if_supported("-Wno-reorder")
        .flag_if_supported("-Wno-parentheses");

    // `cc` prints the two `cargo:rustc-link-*` lines for a normal compile, but it cannot print
    // them for an archive that comes from the cache. So `cc` is silent here, and both paths
    // print the same lines below. A cache hit then links exactly like a compile.
    build.cargo_metadata(false);
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let archive = out_dir.join("libairwindows.a");
    let cache_entry = cache_entry_path(&build, &crate_root, &src, &sources);
    let restored = cache_entry.as_deref().is_some_and(|entry| restore_from_cache(entry, &archive));
    if restored {
        eprintln!("airwindows: reused the compiled library from {}", cache_entry.as_ref().unwrap().display());
    } else {
        build.compile("airwindows");
        if let Some(entry) = &cache_entry {
            store_in_cache(&archive, entry);
        }
    }
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=airwindows");
    // `cc` no longer prints these, so name the inputs that can change the archive.
    for var in ["CXX", "CXXFLAGS", "CFLAGS", "HOST_CXX", "TARGET_CXX", "CRATE_CC_NO_DEFAULTS", "MACOSX_DEPLOYMENT_TARGET", "SDKROOT", "TUI_WAVE_AIRWINDOWS_CACHE_DIR", "TUI_WAVE_NO_BUILD_CACHE"] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    // Rebuild triggers. The autogen tree is deliberately watched as a *directory*: naming all
    // ~1000 files individually would make cargo re-stat every one of them on each build, and
    // the directory mtime moves whenever a submodule bump adds or removes a plugin.
    println!("cargo:rerun-if-changed=src/shim.cpp");
    println!("cargo:rerun-if-changed={}", autogen.display());
    println!("cargo:rerun-if-changed={}", src.join("ModuleAdd.h").display());
    println!(
        "cargo:rerun-if-changed={}",
        src.join("airwin_consolidated_base.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        src.join("airwin_consolidated_base.h").display()
    );
    println!("cargo:rerun-if-changed={}", src.join("AirwinRegistry.h").display());

    // libstdc++ / libc++ is not linked automatically by the `cc` crate's static archive.
    // Named explicitly here so the requirement is stated once, in the build, rather than
    // being discovered as a link failure on whichever platform is built second. This is also
    // what makes it a *runtime* dependency of the packages -- see the `libstdc++` entries in
    // Cargo.toml's deb/rpm metadata.
    link_cpp_stdlib();

    eprintln!("airwindows: {plugin_sources} plugin translation units in the library");
}

/// The C++ standard library is `libc++` on macOS and `libstdc++` on Linux. `cc` knows this
/// for its own linking but does not emit the `cargo:rustc-link-lib` directive for us.
fn link_cpp_stdlib() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let lib = match target.as_str() {
        "macos" | "ios" => "c++",
        _ => "stdc++",
    };
    println!("cargo:rustc-link-lib={lib}");
}

/// Two 64-bit FNV-1a lanes with different multipliers, printed as 32 hex digits. This is a
/// cache name and not a security check, and a build script has no hash crate to use.
struct KeyHasher {
    a: u64,
    b: u64,
}

impl KeyHasher {
    fn new() -> Self {
        Self { a: 0xcbf2_9ce4_8422_2325, b: 0x9e37_79b9_7f4a_7c15 }
    }

    fn write(&mut self, bytes: &[u8]) {
        for &x in bytes {
            self.a = (self.a ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3);
            self.b = (self.b ^ u64::from(x)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }

    /// A field: its length first, so that two fields cannot run together into one.
    fn field(&mut self, bytes: &[u8]) {
        self.write(&(bytes.len() as u64).to_le_bytes());
        self.write(bytes);
    }

    fn finish(&self) -> String {
        format!("{:016x}{:016x}", self.a, self.b)
    }
}

/// Every source file under `dir` that the compiler can read, in a fixed order.
fn compiler_inputs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            compiler_inputs(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("cpp" | "h" | "hpp" | "cc" | "inc")
        ) {
            out.push(path);
        }
    }
}

/// The cache file for this exact build, or `None` when the cache is not used: a debug build, an
/// opt-out, no home directory, or an input that could not be read. Debug builds are left out on
/// purpose. They use the target directory as before, and if they shared the cache they would
/// replace the release archive that `setup.sh` wants to find there.
fn cache_entry_path(
    build: &cc::Build,
    crate_root: &Path,
    src: &Path,
    plugin_sources: &[PathBuf],
) -> Option<PathBuf> {
    if std::env::var("PROFILE").ok().as_deref() != Some("release")
        || std::env::var_os("TUI_WAVE_NO_BUILD_CACHE").is_some_and(|v| !v.is_empty() && v != "0")
    {
        return None;
    }
    let dir = match std::env::var_os("TUI_WAVE_AIRWINDOWS_CACHE_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let base = std::env::var_os("XDG_CACHE_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
            base.join("tui-wave").join("airwindows")
        }
    };

    let mut key = KeyHasher::new();
    key.field(CACHE_FORMAT.as_bytes());
    for var in ["TARGET", "OPT_LEVEL", "DEBUG", "CXX", "CXXFLAGS", "CFLAGS", "CRATE_CC_NO_DEFAULTS", "MACOSX_DEPLOYMENT_TARGET", "SDKROOT"] {
        key.field(var.as_bytes());
        key.field(std::env::var(var).unwrap_or_default().as_bytes());
    }
    // The compiler that would run, its flags, and its own version text.
    let tool = build.get_compiler();
    key.field(tool.path().to_string_lossy().as_bytes());
    for arg in tool.args() {
        key.field(arg.to_string_lossy().as_bytes());
    }
    let version = std::process::Command::new(tool.path()).arg("--version").output().ok()?;
    key.field(&version.stdout);

    // The sources. `plugin_sources` is what `build.files` compiles. The rest of `src` holds the
    // headers they include, so all of it counts.
    let mut inputs = vec![crate_root.join("src/shim.cpp")];
    inputs.extend(plugin_sources.iter().cloned());
    compiler_inputs(src, &mut inputs);
    inputs.sort();
    inputs.dedup();
    for path in inputs {
        let mut bytes = Vec::new();
        std::fs::File::open(&path).ok()?.read_to_end(&mut bytes).ok()?;
        key.field(path.to_string_lossy().replace('\\', "/").as_bytes());
        key.field(&bytes);
    }
    Some(dir.join(format!("libairwindows-{}.a", key.finish())))
}

/// Copy a cached archive to `archive`. `false` when there is none, or it does not look like a
/// static archive, so a damaged file is compiled again and not linked.
fn restore_from_cache(entry: &Path, archive: &Path) -> bool {
    let mut magic = [0u8; 8];
    let readable = std::fs::File::open(entry).and_then(|mut f| f.read_exact(&mut magic)).is_ok();
    readable && &magic == b"!<arch>\n" && std::fs::copy(entry, archive).is_ok()
}

/// Save the archive under its cache name, and remove the archives of older builds. A cache
/// problem must never fail the build, so every error is ignored.
fn store_in_cache(archive: &Path, entry: &Path) {
    let Some(dir) = entry.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Written under a temporary name and then renamed, so another build never reads half a file.
    let temp = dir.join(format!(".airwindows-{}.tmp", std::process::id()));
    if std::fs::copy(archive, &temp).is_err() || std::fs::rename(&temp, entry).is_err() {
        let _ = std::fs::remove_file(&temp);
        return;
    }
    if let Ok(read) = std::fs::read_dir(dir) {
        for old in read.filter_map(|e| e.ok()).map(|e| e.path()) {
            let name = old.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if old != entry && name.starts_with("libairwindows-") && name.ends_with(".a") {
                let _ = std::fs::remove_file(old);
            }
        }
    }
}
