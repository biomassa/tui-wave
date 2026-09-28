//! Compresses the built-in process catalogs into `OUT_DIR`, where `model::cdp::catalog` embeds
//! them with `include_bytes!` and inflates them at load.
//!
//! The five TOML files are 3.3MB of text and deflate to about 450KB, which is most of that saving
//! in the binary. Each is read as a `String` here, so a file that is not UTF-8 fails the build,
//! the same check `include_str!` made.

use std::path::Path;

const CATALOGS: [&str; 5] = [
    "catalog.toml",
    "catalog_extra.toml",
    "catalog_titles.toml",
    "praat_catalog.toml",
    "airwindows_catalog.toml",
];

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    for name in CATALOGS {
        let src = Path::new("src/model/cdp").join(name);
        println!("cargo:rerun-if-changed={}", src.display());
        let text = std::fs::read_to_string(&src)
            .unwrap_or_else(|e| panic!("reading {}: {e}", src.display()));
        let packed = miniz_oxide::deflate::compress_to_vec(text.as_bytes(), 9);
        let dest = Path::new(&out_dir).join(format!("{name}.z"));
        std::fs::write(&dest, packed).unwrap_or_else(|e| panic!("writing {}: {e}", dest.display()));
    }
}
