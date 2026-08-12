//! Compiles the Airwindows DSP (via airwin2rack's consolidated sources) and the
//! `src/airwindows/shim.cpp` C API over it into a static library linked into the binary.
//!
//! This is the only C++ in the project and the only reason a build script exists. It is also
//! the first dependency whose *code* ships inside the release artifacts rather than being
//! installed separately by the user, which is why `THIRD_PARTY_NOTICES.md` grew a section
//! that CDP and Praat did not need.
//!
//! The plugin sources are taken from the submodule's committed `src/autogen_airwin/`, not
//! generated here: airwin2rack's `scripts/import.pl` has already done the transformation from
//! upstream Airwindows (swapping the VST2 SDK include for its own ~90-line shim header,
//! namespacing each plugin, dropping `getChunk`/`setChunk`) and committed the result. So no
//! Perl, no VST SDK, and no nested `libs/airwindows` checkout is needed at build time -- the
//! submodule is cloned non-recursively on purpose.

use std::path::PathBuf;

const SUBMODULE: &str = "third_party/airwin2rack";

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let aw = root.join(SUBMODULE);
    let src = aw.join("src");
    let autogen = src.join("autogen_airwin");

    if !autogen.is_dir() {
        panic!(
            "{} is missing.\n\n\
             The Airwindows backend is built from a git submodule. Run:\n\
             \n    git submodule update --init {SUBMODULE}\n",
            autogen.display()
        );
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(&src)
        .file(root.join("src/airwindows/shim.cpp"))
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

    build.compile("airwindows");

    // Rebuild triggers. The autogen tree is deliberately watched as a *directory*: naming all
    // ~1000 files individually would make cargo re-stat every one of them on each build, and
    // the directory mtime moves whenever a submodule bump adds or removes a plugin.
    println!("cargo:rerun-if-changed=src/airwindows/shim.cpp");
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

    eprintln!("airwindows: compiled {plugin_sources} plugin translation units");
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
