//! Graphics-mode rendering for the CDP breakpoint-envelope editor (kitty/Sixel/iTerm2
//! protocols via `ratatui-image`), used instead of the ASCII "staircase" curve
//! (`App::render_cdp_envelope_editor`'s text renderer) when the terminal supports a real
//! bitmap protocol — the same `graphics_mode`/`Picker` machinery `waveform_image.rs` uses
//! for the waveform, applied to this editor's grid instead. Unlike the waveform, at pixel
//! resolution the curve is drawn as true diagonal line segments (not a staircase — the
//! per-cell ASCII approximation exists only because character cells can't slant), and
//! breakpoints are drawn as filled discs rather than `●` glyphs.
//!
//! Reuses [`super::waveform_image`]'s low-level anti-aliased pixel helpers
//! (`color_to_rgba`/`blend_pixel`/`draw_vspan_aa`) rather than duplicating them — both
//! modules draw into an `RgbaImage` with the same "continuous coordinates in, anti-aliased
//! vertical spans out" approach.

use image::{Rgba, RgbaImage};

use crate::ui::theme;
use super::waveform_image::{color_to_rgba, draw_vspan_aa};

/// The value CDP's own breakpoint automation would produce at time `t`: piecewise-linear
/// interpolation between `points` (sorted by time), clamped to the first/last point's value
/// outside their time range. Shared by both the ASCII and bitmap renderers (and `App`'s own
/// key/mouse handling for the editor) so there is exactly one definition of what the curve
/// means — imported back into `ui::app` rather than duplicated there.
pub fn interp_cdp_envelope(points: &[(f64, f64)], t: f64) -> f64 {
    let Some(&(first_t, first_v)) = points.first() else { return 0.0 };
    if t <= first_t {
        return first_v;
    }
    let Some(&(last_t, last_v)) = points.last() else { return first_v };
    if t >= last_t {
        return last_v;
    }
    for pair in points.windows(2) {
        let (t0, v0) = pair[0];
        let (t1, v1) = pair[1];
        if t >= t0 && t <= t1 {
            if t1 > t0 {
                return v0 + (v1 - v0) * (t - t0) / (t1 - t0);
            }
            return v0;
        }
    }
    last_v
}

/// Filled-disc radius (in pixels) for an ordinary breakpoint marker.
const POINT_RADIUS: f64 = 3.0;
/// Larger radius for the selected point, so it stands out without needing a reverse-video
/// concept (which doesn't translate directly to a bitmap the way it does for a text cell).
const SELECTED_POINT_RADIUS: f64 = 4.5;

/// Draws a filled, anti-aliased disc centered at continuous coordinates `(cx, cy)`. Coverage
/// at each pixel is estimated from the distance of its center to the disc's edge (clamped to
/// `[0, 1]`), giving a soft edge instead of a jagged circle at these small radii.
fn draw_disc(img: &mut RgbaImage, cx: f64, cy: f64, radius: f64, color: Rgba<u8>) {
    let x_min = (cx - radius - 1.0).floor().max(0.0) as u32;
    let x_max = (cx + radius + 1.0).ceil().min(img.width() as f64 - 1.0).max(0.0) as u32;
    let y_min = (cy - radius - 1.0).floor().max(0.0) as u32;
    let y_max = (cy + radius + 1.0).ceil().min(img.height() as f64 - 1.0).max(0.0) as u32;
    if img.width() == 0 || img.height() == 0 {
        return;
    }
    for y in y_min..=y_max {
        for x in x_min..=x_max {
            let dx = x as f64 + 0.5 - cx;
            let dy = y as f64 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            // 1px feather at the edge: full coverage inside (radius - 0.5), fading to 0 at
            // (radius + 0.5), rather than a hard cutoff that looks jagged at this size.
            let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0);
            if coverage > 0.0 {
                super::waveform_image::blend_pixel(img, x, y, color, coverage);
            }
        }
    }
}

/// The reference-waveform equivalent of `draw_vspan_aa`: same continuous-range, same
/// fractional-edge anti-aliasing and same "widen to at least 1px" treatment for a silent
/// span, but every row's coverage is additionally scaled by a constant `alpha` — a flat,
/// capped-opacity wash rather than `draw_vspan_aa`'s full-strength fill, which is what keeps
/// this reading as a pale backdrop no matter how loud the underlying audio gets.
fn draw_low_alpha_vspan(img: &mut RgbaImage, col: u32, y0: f64, y1: f64, color: Rgba<u8>, alpha: f64) {
    if alpha <= 0.0 {
        return;
    }
    let h = img.height() as f64;
    let (mut lo, mut hi) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
    if hi - lo < 1.0 {
        let mid = (lo + hi) / 2.0;
        lo = mid - 0.5;
        hi = mid + 0.5;
    }
    if lo < 0.0 {
        hi = (hi - lo).min(h);
        lo = 0.0;
    }
    if hi > h {
        lo = (lo - (hi - h)).max(0.0);
        hi = h;
    }
    let first = lo.floor() as u32;
    let last_excl = (hi.ceil() as u32).min(img.height());
    for row in first..last_excl {
        let coverage = hi.min(row as f64 + 1.0) - lo.max(row as f64);
        super::waveform_image::blend_pixel(img, col, row, color, coverage * alpha);
    }
}

/// Rasterizes a breakpoint envelope into a `pixel_width` x `pixel_height` RGBA image. The
/// time/value → pixel mapping is the continuous analog of the ASCII renderer's
/// `cdp_envelope_value_to_row` (row 0 = `max`, last row = `min`) and the mouse handler's
/// `cdp_envelope_mouse_to_domain` (column 0 = `t=0`, last column = `t=time_max`) — switching
/// `graphics_mode` on or off must never change what a given screen position means, only how
/// it's drawn.
pub fn rasterize_cdp_envelope(
    points: &[(f64, f64)],
    selected: usize,
    time_max: f64,
    min: f64,
    max: f64,
    waveform_ref: &[f32],
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

    // Pale reference waveform (the actual audio the envelope will apply to), drawn first so
    // the curve/points always render crisply on top of it — bottom-anchored (silence = the
    // very bottom row, full-scale = the very top), low-alpha, not meant to be read
    // precisely, just to show *where* in time the sound has content — the same shape the
    // main waveform panel would show, just rectified to one lobe instead of bipolar. Unlike
    // `rasterize_waveform`'s bar mode, no inter-column overlap-extension is needed here:
    // every column's span shares the same fixed `bottom` anchor, so adjacent spans always
    // overlap there regardless of how much their tops differ — a real gap (the thing that
    // trick exists to prevent) simply can't occur. `waveform_ref` is expected at
    // (near-)pixel resolution (see `App::cdp_envelope_waveform_ref`) so there's genuine
    // per-column variation for the top edge to trace — coarser cell-resolution data
    // upsampled by nearest-neighbor is what made this read as blocky bars before.
    if !waveform_ref.is_empty() {
        let ref_color = color_to_rgba(theme::WAVEFORM);
        const REF_MAX_ALPHA: f64 = 0.35;
        let bottom = pixel_height as f64;
        for col in 0..pixel_width {
            let cell = ((col as f64 / pixel_width as f64) * waveform_ref.len() as f64) as usize;
            let peak = waveform_ref.get(cell).copied().unwrap_or(0.0).clamp(0.0, 1.0) as f64;
            let top = bottom - peak * bottom;
            draw_low_alpha_vspan(&mut img, col, top, bottom, ref_color, peak * REF_MAX_ALPHA);
        }
    }

    if points.is_empty() || time_max <= 0.0 {
        return img;
    }

    let value_to_y = |v: f64| -> f64 {
        if max <= min {
            return pixel_height as f64 / 2.0;
        }
        let frac = ((v - min) / (max - min)).clamp(0.0, 1.0);
        (1.0 - frac) * (pixel_height as f64 - 1.0)
    };
    let time_to_x = |t: f64| -> f64 {
        if pixel_width <= 1 {
            0.0
        } else {
            (t / time_max * (pixel_width - 1) as f64).clamp(0.0, (pixel_width - 1) as f64)
        }
    };

    let curve_color = color_to_rgba(theme::TEXT);
    let mut prev_y: Option<f64> = None;
    for col in 0..pixel_width {
        let t = if pixel_width <= 1 { 0.0 } else { col as f64 / (pixel_width - 1) as f64 * time_max };
        let v = interp_cdp_envelope(points, t);
        let y = value_to_y(v);
        let (y_lo, y_hi) = match prev_y {
            Some(py) => (py.min(y), py.max(y)),
            None => (y, y),
        };
        draw_vspan_aa(&mut img, col, y_lo, y_hi, curve_color);
        prev_y = Some(y);
    }

    // Breakpoint markers on top of the curve; the selected one drawn last (and larger) so
    // it's never partly hidden under an adjacent point's disc. The active point is the
    // accent color (FOCUS/orange, matching the ASCII editor's reverse-video selection);
    // inactive points are pale (SUBTEXT0) so the selected one is unambiguous at a glance.
    let point_color = color_to_rgba(theme::SUBTEXT0);
    let selected_color = color_to_rgba(theme::FOCUS);
    for (i, &(t, v)) in points.iter().enumerate() {
        if i == selected {
            continue;
        }
        draw_disc(&mut img, time_to_x(t), value_to_y(v), POINT_RADIUS, point_color);
    }
    if let Some(&(t, v)) = points.get(selected) {
        draw_disc(&mut img, time_to_x(t), value_to_y(v), SELECTED_POINT_RADIUS, selected_color);
    }

    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interp_matches_piecewise_linear_semantics() {
        let points = vec![(0.0, 0.0), (1.0, 10.0), (2.0, 0.0)];
        assert_eq!(interp_cdp_envelope(&points, -1.0), 0.0);
        assert_eq!(interp_cdp_envelope(&points, 0.5), 5.0);
        assert_eq!(interp_cdp_envelope(&points, 1.5), 5.0);
        assert_eq!(interp_cdp_envelope(&points, 3.0), 0.0);
    }

    #[test]
    fn empty_points_renders_background_only() {
        let img = rasterize_cdp_envelope(&[], 0, 1.0, 0.0, 100.0, &[], 40, 20);
        let bg = color_to_rgba(theme::SURFACE0);
        assert!(img.pixels().all(|&p| p == bg));
    }

    #[test]
    fn image_has_the_requested_dimensions() {
        let points = vec![(0.0, 0.0), (1.0, 100.0)];
        let img = rasterize_cdp_envelope(&points, 0, 1.0, 0.0, 100.0, &[], 64, 32);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }

    #[test]
    fn zero_dimensions_clamp_to_one_pixel() {
        let points = vec![(0.0, 0.0), (1.0, 100.0)];
        let img = rasterize_cdp_envelope(&points, 0, 1.0, 0.0, 100.0, &[], 0, 0);
        assert_eq!(img.width(), 1);
        assert_eq!(img.height(), 1);
    }

    /// A flat envelope (both points at the same value) should draw a horizontal line at the
    /// row corresponding to that value — every column should have at least one non-background
    /// pixel near the same row.
    #[test]
    fn flat_envelope_draws_a_horizontal_line_at_the_value_row() {
        let points = vec![(0.0, 50.0), (1.0, 50.0)];
        let img = rasterize_cdp_envelope(&points, 0, 1.0, 0.0, 100.0, &[], 50, 20);
        let bg = color_to_rgba(theme::SURFACE0);
        let expected_row: u32 = 9; // midpoint of a 0..100 range over 20 rows, value 50 -> ~row 9-10
        for col in [0u32, 25, 49] {
            let has_nonbg_near_row = (expected_row.saturating_sub(1)..=expected_row + 1)
                .any(|row| img.get_pixel(col, row) != &bg);
            assert!(has_nonbg_near_row, "column {col} should have curve color near row {expected_row}");
        }
    }

    /// A point drawn well inside the image bounds should produce a visibly filled disc — not
    /// just a single pixel — around its center.
    #[test]
    fn point_marker_draws_a_filled_disc() {
        let points = vec![(0.0, 0.0), (0.5, 100.0), (1.0, 0.0)];
        let img = rasterize_cdp_envelope(&points, 1, 1.0, 0.0, 100.0, &[], 60, 30);
        let bg = color_to_rgba(theme::SURFACE0);
        // The selected (middle) point sits at col ~29-30, row 0 (value=100=max=top row).
        let mut nonbg = 0;
        for y in 0..4 {
            for x in 25..35 {
                if img.get_pixel(x, y) != &bg {
                    nonbg += 1;
                }
            }
        }
        assert!(nonbg > 4, "expected a filled disc (several non-background pixels), got {nonbg}");
    }

    /// The reference waveform is a *pale* backdrop: a loud column should tint noticeably
    /// more of its bottom rows than a quiet one, but nowhere near full curve/point contrast
    /// (`REF_MAX_ALPHA` caps it well under 1.0 coverage).
    #[test]
    fn reference_waveform_draws_taller_bars_for_louder_columns() {
        let points = vec![(0.0, 0.0), (1.0, 0.0)]; // flat at the bottom, out of the way
        let waveform_ref = vec![0.0f32, 1.0f32]; // silent then full-scale
        let img = rasterize_cdp_envelope(&points, 0, 1.0, 0.0, 100.0, &waveform_ref, 40, 20);
        let bg = color_to_rgba(theme::SURFACE0);

        // Column in the silent half (excluding the curve's own bottom row) should be
        // background; a column in the loud half should have a tinted bottom row.
        let quiet_col_bg = (0..15).all(|row| img.get_pixel(5, row) != &color_to_rgba(theme::TEXT))
            && img.get_pixel(5, 5) == &bg;
        assert!(quiet_col_bg, "silent portion should have no reference-waveform tint away from the curve");

        let loud_bottom_row = img.height() - 1;
        assert_ne!(img.get_pixel(35, loud_bottom_row - 3), &bg, "loud portion should tint rows well above the very bottom");
    }

    /// The reference waveform must never fully saturate — it's a backdrop, not competing
    /// visual content — even at peak amplitude.
    #[test]
    fn reference_waveform_never_reaches_full_contrast() {
        let points = vec![(0.0, 0.0), (1.0, 0.0)];
        let waveform_ref = vec![1.0f32; 10];
        let img = rasterize_cdp_envelope(&points, 0, 1.0, 0.0, 100.0, &waveform_ref, 40, 20);
        let bg = color_to_rgba(theme::SURFACE0);
        let full_color = color_to_rgba(theme::WAVEFORM);
        let bottom_row = img.height() - 1;
        for x in 0..img.width() {
            let px = img.get_pixel(x, bottom_row);
            assert_ne!(px, &full_color, "reference waveform should never blend at full strength");
            assert_ne!(px, &bg, "a peak-amplitude column should still show *some* tint");
        }
    }
}
