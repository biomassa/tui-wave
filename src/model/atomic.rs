//! Write-to-temp-then-rename, so a failed or interrupted write cannot destroy the file it was
//! replacing.
//!
//! Every save path in this app used to open the user's own file with `O_TRUNC` and write into it
//! sample by sample. That zeroes the original *before* the first byte of the replacement is
//! written, so a disk-full, a permissions change, an unplugged drive or a crash part-way through
//! left a truncated file where the take used to be — unrecoverable, and for quick Save the target
//! is the source recording itself.
//!
//! Staging costs nothing but a rename and removes that whole class of loss: the original stays
//! untouched until a complete replacement exists beside it, and `rename` within a directory is
//! atomic, so a reader either sees the old file or the new one and never a half-written one.
//!
//! The temp file is a **sibling** of the target, not something in `$TMPDIR`: `rename` is only
//! atomic (and only works at all, without a copy) within one filesystem, and the target's
//! directory is the one place guaranteed to be on the same filesystem as the target.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Marks a staging file, so anything left by a killed process is identifiable rather than
/// looking like the user's own data. Deliberately not `.tmp`, which users do use.
const STAGING_SUFFIX: &str = "tui-wave-tmp";

/// The staging path for `path`: same directory, dot-prefixed, PID-tagged.
///
/// Dot-prefixed so it is hidden on Unix and so it cannot match the Files panel's `.wav` filter
/// while it exists. PID-tagged so two instances saving the same target at once stage separately —
/// they will still race on the final `rename`, but each will have written a *complete* file, so
/// the loser is overwritten rather than interleaved with.
fn staging_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    dir.join(format!(".{name}.{STAGING_SUFFIX}-{}", std::process::id()))
}

/// Runs `write` against a staging file beside `path`, then renames it over `path`.
///
/// `write` receives the staging path and must create and finish that file; it never sees the real
/// target, so there is no way for it to touch the original. On any error — from `write` itself or
/// from the rename — the staging file is removed and `path` is left exactly as it was.
///
/// The staged file is flushed to disk before the rename, so a power loss cannot leave the rename
/// visible while the contents it published are not.
pub fn write_atomically<T, E>(
    path: &Path,
    write: impl FnOnce(&Path) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<io::Error>,
{
    let staging = staging_path(path);
    // A staging file surviving from a killed run would otherwise be appended to or confuse the
    // writer; this is also why the name is PID-tagged, so this only ever clears our own.
    let _ = fs::remove_file(&staging);

    let value = match write(&staging) {
        Ok(value) => value,
        Err(e) => {
            let _ = fs::remove_file(&staging);
            return Err(e);
        }
    };

    match publish(&staging, path) {
        Ok(()) => Ok(value),
        Err(e) => {
            let _ = fs::remove_file(&staging);
            Err(E::from(e))
        }
    }
}

/// Flushes the staged file, carries the target's permissions across, and renames it into place.
fn publish(staging: &Path, path: &Path) -> io::Result<()> {
    // `sync_all` on the staged file before the rename: without it the rename can reach the disk
    // first, and a power loss then publishes a file whose contents were never written.
    if let Ok(file) = fs::File::open(staging) {
        let _ = file.sync_all();
    }
    // A fresh file is created under the process umask, so replacing a file that had, say, 0640
    // would silently widen it. Best-effort — a filesystem without permissions simply skips this.
    if let Ok(existing) = fs::metadata(path) {
        let _ = fs::set_permissions(staging, existing.permissions());
    }
    fs::rename(staging, path)?;
    // And the directory entry itself, so the rename survives a power loss. Unsupported on some
    // platforms (notably Windows), hence best-effort.
    if let Some(dir) = path.parent() {
        if let Ok(handle) = fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("tui_wave_atomic_{tag}_{}_{:p}", std::process::id(), &tag));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Guards the directory so a failing assertion below cannot leak it into `$TMPDIR`.
    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_successful_write_replaces_the_target() {
        let dir = Dir(tmp("ok"));
        let target = dir.0.join("take.wav");
        fs::write(&target, b"original").unwrap();

        write_atomically(&target, |staging| {
            fs::write(staging, b"replacement")
        })
        .expect("the write should succeed");

        assert_eq!(fs::read(&target).unwrap(), b"replacement");
    }

    /// The whole point: a write that fails part-way must leave the original file intact, where
    /// truncating it in place left a half-written one.
    #[test]
    fn a_failed_write_leaves_the_original_untouched() {
        let dir = Dir(tmp("fail"));
        let target = dir.0.join("take.wav");
        fs::write(&target, b"original").unwrap();

        let result: Result<(), io::Error> = write_atomically(&target, |staging| {
            // Write some of it, then fail — exactly the shape of a disk filling up mid-save.
            let mut file = fs::File::create(staging)?;
            file.write_all(b"half")?;
            Err(io::Error::other("disk full"))
        });

        assert!(result.is_err());
        assert_eq!(
            fs::read(&target).unwrap(),
            b"original",
            "the original must survive a failed replacement"
        );
    }

    /// A failure must not leave its staging file behind either, or a folder collects debris.
    #[test]
    fn nothing_is_left_behind_on_either_path() {
        let dir = Dir(tmp("clean"));
        let target = dir.0.join("take.wav");

        write_atomically(&target, |staging| fs::write(staging, b"new")).unwrap();
        let after_success: Vec<_> = fs::read_dir(&dir.0).unwrap().flatten().collect();
        assert_eq!(after_success.len(), 1, "only the target should remain");

        let _: Result<(), io::Error> =
            write_atomically(&target, |staging| {
                fs::write(staging, b"partial")?;
                Err(io::Error::other("nope"))
            });
        let after_failure: Vec<_> = fs::read_dir(&dir.0).unwrap().flatten().collect();
        assert_eq!(after_failure.len(), 1, "the staging file must be cleaned up");
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    /// Writing a file that does not exist yet is the ordinary Save As case.
    #[test]
    fn a_new_file_is_created_normally() {
        let dir = Dir(tmp("new"));
        let target = dir.0.join("fresh.wav");
        write_atomically(&target, |staging| fs::write(staging, b"data")).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"data");
    }

    /// The staging file must be a sibling — a rename across filesystems is not atomic and, for
    /// `$TMPDIR` on its own mount, not even possible without a copy.
    #[test]
    fn staging_happens_beside_the_target() {
        let path = Path::new("/some/where/take.wav");
        let staging = staging_path(path);
        assert_eq!(staging.parent(), path.parent());
        let name = staging.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with('.'), "hidden on Unix: {name}");
        assert!(name.contains(STAGING_SUFFIX), "identifiable as ours: {name}");
        assert!(!name.ends_with(".wav"), "must not look like audio: {name}");
    }

    /// Permissions of the file being replaced are carried across, so saving over a file the user
    /// had locked down does not silently reopen it to the umask default.
    #[cfg(unix)]
    #[test]
    fn replacing_a_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir(tmp("perms"));
        let target = dir.0.join("take.wav");
        fs::write(&target, b"original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();

        write_atomically(&target, |staging| fs::write(staging, b"replacement")).unwrap();

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "the replacement must not widen the file's mode");
    }
}
