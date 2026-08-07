//! Loading a user-chosen image for the picker's preview pane.
//!
//! The counterpart of `praat::picture` on the *input* side, and deliberately not the same
//! function. That one crops a Praat figure to its ink, because a 12x12-inch canvas holding a
//! drawing in one corner is mostly page white and blitting it would waste the popup. A
//! photograph has no page white and no ink bounds — cropping it to "content" would silently
//! trim whatever the user actually chose — so this only ever scales.
//!
//! **PNG only**, matching Praat, which links `libpng` and nothing else (a JPEG or TIFF fails
//! there with `Error reading PNG file`). The picker filters to `.png` for the same reason, so
//! a decode failure here means a damaged or misnamed file rather than an unsupported format —
//! which is exactly what makes it worth reporting: the preview is the last place a bad file can
//! be caught before a run spends its time on it.

use std::path::{Path, PathBuf};

use image::RgbaImage;

/// Longest side of the preview handed to the renderer.
///
/// Smaller than `praat::picture::MAX_DIMENSION` (2400) on purpose. That figure is sized for
/// hairline strokes and 7-point annotations, where anything the blit upscales is detail thrown
/// away. A photograph carries no such fine structure, and this decodes on **every arrow key**
/// through the file list rather than once per run — so the ceiling is set by how much work a
/// keypress may do, not by how much detail a figure holds. 1024 fills the preview pane of any
/// realistic terminal and costs at most 4 MB of RGBA.
pub const MAX_PREVIEW_DIMENSION: u32 = 1024;

/// Above this, the file is described but not decoded.
///
/// A PNG is compressed, so the decoded cost is `width * height * 4` regardless of how small the
/// file is — a "decompression bomb" at 20000x20000 is 1.6 GB of RGBA from a few hundred KB on
/// disk. Guarding the *file* size is a cheap proxy that costs one `metadata` call and no
/// decode; the pixel-count guard below is the one that actually bounds memory. Both exist
/// because either alone lets the other case through.
const MAX_PREVIEW_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Above this many source pixels, the file is described but not decoded — see
/// [`MAX_PREVIEW_FILE_BYTES`]. 80 megapixels is far past any camera a user would sonify and
/// still only ~320 MB of RGBA transiently, which is survivable if the guard is ever wrong.
const MAX_PREVIEW_PIXELS: u64 = 80_000_000;

/// Everything the picker's preview pane shows for one file.
///
/// Built for a path and cached against it (`PhotoPreview::path`), because the pane is redrawn
/// every frame but the file only changes when the highlight moves.
///
/// `image` is `None` for a file that could not or should not be decoded, and `error` says which
/// — but `dimensions` and `file_bytes` are filled in either way where they can be. A file too
/// large to preview is still a file the user may legitimately want to run, so the pane shows
/// its facts and declines only the bitmap.
pub struct PhotoPreview {
    /// The file this was built for. The cache key: a preview is rebuilt only when the
    /// highlighted path differs from this.
    pub path: PathBuf,
    /// Scaled to fit [`MAX_PREVIEW_DIMENSION`], never cropped.
    pub image: Option<RgbaImage>,
    /// The source's own pixel dimensions, before scaling — what the sonifiers actually read, so
    /// this is the figure worth showing rather than the preview's.
    pub dimensions: Option<(u32, u32)>,
    pub file_bytes: u64,
    /// Why there is no bitmap. `None` when there is one.
    pub error: Option<String>,
}

impl PhotoPreview {
    /// A preview holding only the facts — no bitmap, and a reason.
    fn described(path: &Path, file_bytes: u64, dimensions: Option<(u32, u32)>, error: String) -> Self {
        Self { path: path.to_path_buf(), image: None, dimensions, file_bytes, error: Some(error) }
    }
}

/// Load `path` for display in the preview pane.
///
/// Never returns an error: every failure becomes a `PhotoPreview` that says what went wrong, so
/// the pane always has something to render and browsing past an unreadable file is not an event
/// the caller has to handle.
pub fn load(path: &Path) -> PhotoPreview {
    let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if file_bytes > MAX_PREVIEW_FILE_BYTES {
        return PhotoPreview::described(
            path,
            file_bytes,
            None,
            format!("too large to preview (over {} MB)", MAX_PREVIEW_FILE_BYTES / (1024 * 1024)),
        );
    }

    // Header first: `image`'s reader gives dimensions without decoding pixels, which is both the
    // cheap way to fill the metadata panel and the only way to refuse an oversized image
    // *before* allocating for it.
    let reader = match image::ImageReader::open(path) {
        Ok(reader) => reader,
        Err(err) => return PhotoPreview::described(path, file_bytes, None, err.to_string()),
    };
    let dimensions = reader.into_dimensions().ok();
    if let Some((w, h)) = dimensions {
        if u64::from(w) * u64::from(h) > MAX_PREVIEW_PIXELS {
            return PhotoPreview::described(
                path,
                file_bytes,
                dimensions,
                "too many pixels to preview".to_string(),
            );
        }
    }

    let decoded = match image::open(path) {
        Ok(decoded) => decoded.to_rgba8(),
        Err(err) => return PhotoPreview::described(path, file_bytes, dimensions, err.to_string()),
    };
    let dimensions = dimensions.or(Some(decoded.dimensions()));
    PhotoPreview {
        path: path.to_path_buf(),
        image: Some(fit(decoded)),
        dimensions,
        file_bytes,
        error: None,
    }
}

/// Scale to fit [`MAX_PREVIEW_DIMENSION`] on the longest side, preserving aspect. Returned
/// unchanged when it already fits — the common case for the kind of image these scripts are
/// pointed at, and a resize that only ever costs when it buys something.
fn fit(image: RgbaImage) -> RgbaImage {
    let (width, height) = image.dimensions();
    let longest = width.max(height);
    if longest <= MAX_PREVIEW_DIMENSION {
        return image;
    }
    let scale = f64::from(MAX_PREVIEW_DIMENSION) / f64::from(longest);
    // `.max(1)`: a very wide, very short image would otherwise round its short side to zero,
    // and `resize` to a zero dimension yields an empty image rather than an error.
    let target_w = ((f64::from(width) * scale).round() as u32).max(1);
    let target_h = ((f64::from(height) * scale).round() as u32).max(1);
    // Triangle rather than `praat::picture`'s Lanczos3: that one is reducing 7-point text and
    // hairlines, where ringing is worth paying for sharpness. This runs per keypress on
    // photographic content, where the difference is invisible and the speed is not.
    image::imageops::resize(&image, target_w, target_h, image::imageops::FilterType::Triangle)
}

/// A file size for the metadata panel — `412 KB`, `1.4 MB`. Whole numbers below a megabyte,
/// where a decimal would be noise, and one decimal above it, where it is the difference between
/// "1 MB" and "1.9 MB".
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
        let path = dir.join(name);
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        image.save(&path).expect("write fixture");
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tui-wave-photo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// The ordinary case: a small PNG previews at its own size, and reports the source's
    /// dimensions rather than the preview's (they happen to agree here, which the oversized
    /// test below is what distinguishes).
    #[test]
    fn a_small_png_previews_at_its_own_size() {
        let dir = temp_dir("small");
        let preview = load(&write_png(&dir, "a.png", 320, 240));
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert_eq!(preview.dimensions, Some((320, 240)));
        assert_eq!(preview.image.expect("bitmap").dimensions(), (320, 240));
        assert!(preview.file_bytes > 0);
    }

    /// Scaling must preserve the aspect ratio and report the *source* dimensions — the figure
    /// that matters, since it is what the sonifiers read. Reporting the scaled size would tell
    /// the user their 2048-wide image was 1024 wide.
    #[test]
    fn an_oversized_png_is_scaled_but_reports_its_true_dimensions() {
        let dir = temp_dir("large");
        let preview = load(&write_png(&dir, "b.png", 2048, 1024));
        assert_eq!(preview.dimensions, Some((2048, 1024)));
        let (w, h) = preview.image.expect("bitmap").dimensions();
        assert_eq!(w, MAX_PREVIEW_DIMENSION);
        assert_eq!(h, MAX_PREVIEW_DIMENSION / 2, "aspect ratio was not preserved");
    }

    /// Browsing past a corrupt or misnamed file must not be an event the caller handles — the
    /// pane says what happened and the list keeps working.
    #[test]
    fn an_undecodable_file_yields_a_reason_rather_than_a_failure() {
        let dir = temp_dir("bad");
        let path = dir.join("not-really.png");
        std::fs::write(&path, b"this is not a PNG").expect("write");
        let preview = load(&path);
        assert!(preview.image.is_none());
        assert!(preview.error.is_some());
        // Still worth showing: the user picked *something*, and its size is a real fact.
        assert_eq!(preview.file_bytes, 17);
    }

    /// A missing file is the same shape as an unreadable one — no panic, no error type to
    /// unwrap, just a preview that explains itself.
    #[test]
    fn a_missing_file_is_described_rather_than_panicking() {
        let preview = load(Path::new("/nonexistent/definitely-not-here.png"));
        assert!(preview.image.is_none());
        assert!(preview.error.is_some());
    }

    #[test]
    fn byte_sizes_read_the_way_a_file_manager_writes_them() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(1024 * 1024 * 3 / 2), "1.5 MB");
    }
}
