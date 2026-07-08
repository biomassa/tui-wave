//! CDP process execution: spawning the actual command-line binaries and validating the
//! configured install directory. See `model::cdp` for the process catalog and pure pipeline
//! planning that this module executes.

pub mod runner;

use std::path::Path;

pub use runner::{CdpError, CdpEvent, CdpRunner, Job, JobPurpose};

/// Binaries whose presence stands in for "this looks like a real CDP install" — checking
/// all ~250 would be slow and pointless; these four span the families every process in the
/// catalog depends on (housekeep is used by nothing in v1 but is ubiquitous across CDP
/// installs, making it a good canary for a partial/corrupt copy).
const SENTINEL_BINARIES: &[&str] = &["pvoc", "modify", "blur", "housekeep"];

/// Checks that `dir` looks like a CDP binaries directory: it exists and contains the
/// sentinel binaries. Returns a human-readable reason on failure, for display in the
/// `CdpSetup` dialog.
pub fn validate_cdp_dir(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    for bin in SENTINEL_BINARIES {
        let path = dir.join(bin);
        if !path.is_file() {
            return Err(format!("{} not found in {}", bin, dir.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_cdp_dir_in_repo_validates() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("cdp");
        if !dir.is_dir() {
            eprintln!("skipping: no cdp/ directory present in this checkout");
            return;
        }
        assert_eq!(validate_cdp_dir(&dir), Ok(()));
    }

    #[test]
    fn nonexistent_dir_is_rejected() {
        let err = validate_cdp_dir(Path::new("/definitely/not/a/real/path")).unwrap_err();
        assert!(err.contains("not a directory"));
    }

    #[test]
    fn dir_missing_sentinel_binaries_is_rejected() {
        let dir = std::env::temp_dir().join(format!("tui-wave-cdp-validate-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = validate_cdp_dir(&dir).unwrap_err();
        assert!(err.contains("not found"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
