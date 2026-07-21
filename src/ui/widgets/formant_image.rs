//! Graphics-mode rasterizer for the Formant Info popup's spectral-envelope visualization —
//! `ui::app`'s `formant_heatmap_lines`/`formant_snapshot_curve_lines` text-mode grid, redrawn
//! at real pixel resolution for kitty/Sixel/iTerm2 terminals, the same "ASCII first, bitmap
//! occludes it" pattern `cdp_envelope_image.rs` uses for the CDP breakpoint-envelope editor.

use image::{Rgba, RgbaImage};

use crate::model::dsp::linear_to_db;
use crate::model::formant::FormantEnvelope;
use crate::ui::theme;
use super::waveform_image::{color_to_rgba, draw_vspan_aa};

/// `value`'s dB level normalized against `(min_db, max_db)` into `[0, 1]` — mirrors
/// `ui::app::formant_normalized_fraction` exactly. Kept as its own copy here rather than
/// exposed from `ui::app`, the same "same math, two render targets, each owns its own copy"
/// split `cdp_envelope_image.rs`'s `interp_cdp_envelope` and the ASCII renderer's own use of
/// it already establish (there, they share one `pub` function instead — this one's simple
/// enough that a shared function would be more indirection than the four-line body it wraps).
fn normalized_fraction(value: f32, min_db: f32, max_db: f32) -> f32 {
    let db = linear_to_db(value);
    if max_db > min_db { ((db - min_db) / (max_db - min_db)).clamp(0.0, 1.0) } else { 0.0 }
}

fn cell_color(value: f32, min_db: f32, max_db: f32) -> Rgba<u8> {
    color_to_rgba(theme::formant_gradient_color(normalized_fraction(value, min_db, max_db)))
}

/// Rasterizes a `Formant` buffer's full time-varying spectral envelope as a heatmap —
/// `formant_heatmap_lines`'s text-mode grid at true pixel resolution instead of one cell per
/// terminal character. Nearest-neighbor per pixel (matching the text renderer's own
/// downsampling approach, just at a much finer grain — typically 8-10x per axis, a terminal
/// cell's own font-pixel size) rather than interpolated, since this is a coarse read-only
/// preview, not a precision view. Row 0 (top) is the *highest* spectral bin, matching
/// `formant_heatmap_lines`'s own top-to-bottom convention so the two read identically.
pub fn rasterize_formant_heatmap(
    env: &FormantEnvelope,
    min_db: f32,
    max_db: f32,
    pixel_width: u32,
    pixel_height: u32,
) -> RgbaImage {
    let pixel_width = pixel_width.max(1);
    let pixel_height = pixel_height.max(1);
    let mut img = RgbaImage::new(pixel_width, pixel_height);
    let last_bin = env.specenvcnt.saturating_sub(1);
    let last_window = env.windows.saturating_sub(1);
    for y in 0..pixel_height {
        let bin = if pixel_height <= 1 {
            last_bin
        } else {
            last_bin - (y as usize * last_bin) / (pixel_height - 1) as usize
        };
        for x in 0..pixel_width {
            let window = if pixel_width <= 1 { 0 } else { x as usize * last_window / (pixel_width - 1) as usize };
            img.put_pixel(x, y, cell_color(env.get(window, bin), min_db, max_db));
        }
    }
    img
}

/// Rasterizes a `Snapshot` buffer's single spectral envelope slice as a true anti-aliased
/// line (amplitude vs. frequency bin) — the bitmap analog of
/// `formant_snapshot_curve_lines`'s braille dot-matrix curve, the same "continuous diagonal
/// segments instead of a stepped approximation" upgrade
/// `cdp_envelope_image::rasterize_cdp_envelope` gives the breakpoint-envelope editor's own
/// curve. Colored per-column by that column's own normalized fraction, same convention as
/// the heatmap and the text-mode curve.
pub fn rasterize_formant_snapshot_curve(
    env: &FormantEnvelope,
    min_db: f32,
    max_db: f32,
    pixel_width: u32,
    pixel_height: u32,
) -> RgbaImage {
    let pixel_width = pixel_width.max(1);
    let pixel_height = pixel_height.max(1);
    let mut img = RgbaImage::new(pixel_width, pixel_height);
    let background = color_to_rgba(theme::SURFACE0);
    for pixel in img.pixels_mut() {
        *pixel = background;
    }
    if env.specenvcnt == 0 {
        return img;
    }
    let last_bin = env.specenvcnt.saturating_sub(1);
    let value_to_y = |v: f32| -> f64 { (1.0 - normalized_fraction(v, min_db, max_db) as f64) * (pixel_height as f64 - 1.0) };
    let mut prev_y: Option<f64> = None;
    for x in 0..pixel_width {
        let bin = if pixel_width <= 1 { 0 } else { x as usize * last_bin / (pixel_width - 1) as usize };
        let value = env.get(0, bin);
        let y = value_to_y(value);
        let color = cell_color(value, min_db, max_db);
        let (y_lo, y_hi) = match prev_y {
            Some(py) => (py.min(y), py.max(y)),
            None => (y, y),
        };
        draw_vspan_aa(&mut img, x, y_lo, y_hi, |_| color);
        prev_y = Some(y);
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(specenvcnt: usize, windows: usize, values: Vec<f32>) -> FormantEnvelope {
        FormantEnvelope { windows, specenvcnt, arate: 100.0, values }
    }

    #[test]
    fn heatmap_has_the_requested_dimensions() {
        let e = env(4, 2, vec![0.1, 0.9, 0.3, 0.05, 0.8, 0.2, 0.6, 0.4]);
        let img = rasterize_formant_heatmap(&e, -40.0, 0.0, 64, 32);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }

    #[test]
    fn heatmap_zero_dimensions_clamp_to_one_pixel() {
        let e = env(4, 2, vec![0.1, 0.9, 0.3, 0.05, 0.8, 0.2, 0.6, 0.4]);
        let img = rasterize_formant_heatmap(&e, -40.0, 0.0, 0, 0);
        assert_eq!(img.width(), 1);
        assert_eq!(img.height(), 1);
    }

    /// A heatmap with real variation should actually produce more than one color — the same
    /// invariant `ui::app::formant_heatmap_shows_more_than_one_color_for_wide_dynamic_range_data`
    /// checks for the text-mode grid, checked here for the bitmap path.
    #[test]
    fn heatmap_shows_more_than_one_color_for_varied_data() {
        let e = env(4, 4, vec![
            1.0, 0.0001, 0.0001, 0.0001, //
            0.0001, 1.0, 0.0001, 0.0001, //
            0.0001, 0.0001, 1.0, 0.0001, //
            0.0001, 0.0001, 0.0001, 1.0, //
        ]);
        let img = rasterize_formant_heatmap(&e, -80.0, 0.0, 40, 40);
        let distinct: std::collections::HashSet<Rgba<u8>> = img.pixels().copied().collect();
        assert!(distinct.len() > 1, "expected more than one color, got {distinct:?}");
    }

    #[test]
    fn snapshot_curve_has_the_requested_dimensions() {
        let e = env(8, 1, vec![0.1, 0.9, 0.3, 0.05, 0.8, 0.2, 0.6, 0.4]);
        let img = rasterize_formant_snapshot_curve(&e, -40.0, 0.0, 64, 32);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }

    /// A flat (all-zero) snapshot must not panic despite `linear_to_db(0.0)` producing a
    /// large-but-finite negative dB, and the degenerate `min_db == max_db` branch in
    /// `normalized_fraction` must render without dividing by zero.
    #[test]
    fn snapshot_curve_handles_a_perfectly_flat_envelope() {
        let e = env(8, 1, vec![0.0; 8]);
        let img = rasterize_formant_snapshot_curve(&e, -120.0, -120.0, 40, 20);
        assert_eq!(img.width(), 40);
        assert_eq!(img.height(), 20);
    }

    #[test]
    fn snapshot_curve_draws_something_other_than_background() {
        let e = env(8, 1, vec![0.1, 0.9, 0.3, 0.05, 0.8, 0.2, 0.6, 0.4]);
        let img = rasterize_formant_snapshot_curve(&e, -40.0, 0.0, 60, 30);
        let bg = color_to_rgba(theme::SURFACE0);
        assert!(img.pixels().any(|&p| p != bg), "expected the curve to draw something over the background");
    }
}
