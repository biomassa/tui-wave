//! Graphics-mode waveform rendering (kitty/Sixel/iTerm2 protocols via `ratatui-image`),
//! used instead of `widgets::waveform`'s character-glyph renderer when the terminal
//! supports a real bitmap protocol (see `terminal::detect_graphics_picker`).
//!
//! [`rasterize_waveform`] mirrors [`super::waveform::WaveformWidget`]'s per-column min/max
//! downsampling exactly — same `WaveformCache` lookups, same theme colors, same
//! cursor/playhead/selection logic — just at real pixel resolution instead of one glyph per
//! character cell. Span edges stay in continuous (sub-pixel) coordinates all the way to
//! [`draw_vspan_aa`], which blends the fractional first/last row of each column's span
//! against the pixel underneath — snapping edges to whole rows instead turned sub-pixel
//! amplitude changes into flat runs with hard 1px jumps (a visibly staircased trace).

use image::{Rgba, RgbaImage};
use ratatui::style::Color;

use crate::ui::theme;
use crate::ui::viewport::Viewport;
use crate::ui::waveform_cache::{raw_min_max, WaveformCache};

/// A simple, unmistakably-not-text test pattern (a diagonal-striped gradient) — used by
/// Phase 1 to confirm an image actually reaches the screen via the detected graphics
/// protocol, before [`rasterize_waveform`] existed. Kept around as a quick visual sanity
/// check independent of any real audio data; no longer wired into the real render path.
#[cfg_attr(not(test), allow(dead_code))]
pub fn smoke_test_image(width: u32, height: u32) -> image::DynamicImage {
    let width = width.max(1);
    let height = height.max(1);
    let mut img = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let stripe = ((x + y) / 8) % 2 == 0;
            let r = (255 * x / width.max(1)) as u8;
            let g = (255 * y / height.max(1)) as u8;
            let b = if stripe { 200u8 } else { 60u8 };
            img.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }
    image::DynamicImage::ImageRgba8(img)
}

/// Extracts the `(r, g, b)` bytes from a theme `Color`, which in this codebase is always a
/// `Color::Rgb` constant (see `ui::theme`) — falls back to opaque black for any other
/// variant, which should never actually occur given how every color passed in here is one
/// of `theme`'s own constants.
fn color_to_rgba(color: Color) -> Rgba<u8> {
    match color {
        Color::Rgb(r, g, b) => Rgba([r, g, b, 255]),
        _ => Rgba([0, 0, 0, 255]),
    }
}

/// Blends `color` over the pixel at `(x, y)` with the given coverage in `[0, 1]` —
/// coverage 1.0 writes the color exactly, fractional coverage mixes toward whatever is
/// already there (the plain background, or the inverted-selection fill).
fn blend_pixel(img: &mut RgbaImage, x: u32, y: u32, color: Rgba<u8>, coverage: f64) {
    let c = coverage.clamp(0.0, 1.0);
    let under = *img.get_pixel(x, y);
    let mix = |u: u8, o: u8| (u as f64 + (o as f64 - u as f64) * c).round() as u8;
    img.put_pixel(
        x,
        y,
        Rgba([mix(under[0], color[0]), mix(under[1], color[1]), mix(under[2], color[2]), 255]),
    );
}

/// Draws a vertical span covering the continuous range `[y0, y1]` in pixel column `col`,
/// anti-aliased: fully-covered rows get the color exactly, the fractional first/last rows
/// get a blend proportional to how much of them the span covers. A span thinner than one
/// pixel is widened to 1px around its center (and shifted back inside the image if that
/// pushes past an edge) so the trace never fades out entirely.
fn draw_vspan_aa(img: &mut RgbaImage, col: u32, y0: f64, y1: f64, color: Rgba<u8>) {
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
        blend_pixel(img, col, row, color, coverage);
    }
}

/// Rasterizes one channel's waveform into an `pixel_width` x `pixel_height` RGBA image,
/// covering the same sample range a `WaveformWidget` would over a `cell_width`-column-wide
/// character area (i.e. `viewport.span(cell_width)` samples) — `cell_width` is needed
/// because `viewport.samples_per_column` is defined in *character columns*, not pixels, so
/// converting it to samples-per-pixel-column requires knowing how many pixel columns one
/// character column actually spans.
pub fn rasterize_waveform(
    samples: &[f32],
    viewport: &Viewport,
    cache: Option<&WaveformCache>,
    selection: Option<(usize, usize)>,
    cursor: usize,
    playhead: Option<usize>,
    markers: &[(usize, &str)],
    show_marker_labels: bool,
    cell_width: u16,
    pixel_width: u32,
    pixel_height: u32,
) -> RgbaImage {
    let pixel_width = pixel_width.max(1);
    let pixel_height = pixel_height.max(1);
    let mut img = RgbaImage::new(pixel_width, pixel_height);
    let background = color_to_rgba(theme::BASE);
    for pixel in img.pixels_mut() {
        *pixel = background;
    }

    if samples.is_empty() || cell_width == 0 {
        return img;
    }

    let samples_per_pixel_column = viewport.span(cell_width) as f64 / pixel_width as f64;
    let mid_y = pixel_height as f64 / 2.0;
    let half_height = pixel_height as f64 / 2.0;
    let waveform_color = color_to_rgba(theme::WAVEFORM);
    let selected_color = color_to_rgba(theme::WAVEFORM_SELECTED);
    // Inverted selection background: fill selected columns with WAVEFORM (SKY) before
    // drawing the bar, so the bar (in WAVEFORM_SELECTED / YELLOW) appears inverted.
    if let Some((sel_start, sel_end)) = selection {
        let selection_bg = color_to_rgba(theme::WAVEFORM);
        for col in 0..pixel_width {
            let i0 = (viewport.scroll_offset as f64 + col as f64 * samples_per_pixel_column).floor() as usize;
            if i0 >= sel_start && i0 < sel_end {
                for row in 0..pixel_height {
                    img.put_pixel(col, row, selection_bg);
                }
            }
        }
    }

    // Below ~4 samples per pixel column the filled-bar approach breaks down: bars become
    // 1-2px tall and look disconnected, and below 1.0 spc integer truncation makes
    // start==end so every column is silently skipped (blank image). Switch to a connected
    // polyline that linearly interpolates between adjacent samples, giving the smooth
    // waveform-trace look that professional editors show at high zoom.
    if samples_per_pixel_column < 4.0 {
        let mut prev_y: Option<f64> = None;
        for col in 0..pixel_width {
            let sample_f = viewport.scroll_offset as f64 + col as f64 * samples_per_pixel_column;
            let i0 = sample_f.floor() as usize;
            if i0 >= samples.len() {
                break;
            }
            let i1 = (i0 + 1).min(samples.len() - 1);
            let frac = sample_f - i0 as f64;
            let v = samples[i0] as f64 * (1.0 - frac) + samples[i1] as f64 * frac;
            let scaled = (v * viewport.amplitude_scale as f64).clamp(-1.0, 1.0);
            let curr_y = mid_y - scaled * half_height;

            let selected = selection.is_some_and(|(s, e)| i0 >= s && i0 < e);
            let color = if selected { selected_color } else { waveform_color };

            // Vertical segment from the previous column's y to this one's y so consecutive
            // pixel columns are always visually joined — no gaps between sample positions.
            let (y_lo, y_hi) = match prev_y {
                Some(py) => (py.min(curr_y), py.max(curr_y)),
                None => (curr_y, curr_y),
            };
            draw_vspan_aa(&mut img, col, y_lo, y_hi, color);
            prev_y = Some(curr_y);
        }
    } else {
        // Span the previous column actually drew, used to keep the trace connected.
        // Adjacent columns min/max *disjoint* sample ranges, so the inter-sample step across
        // the column boundary belongs to neither column's bar; at mid zoom (only a handful
        // of samples per column) on a steep slope that missed step spans several pixel rows
        // and the trace visibly breaks into dashes. Extending each bar to overlap its
        // predecessor by half a pixel (so the two columns always share a row) is the
        // bar-mode equivalent of the polyline branch's prev_y connection above.
        let mut prev_span: Option<(f64, f64)> = None; // (top_y, bottom_y) as drawn
        for col in 0..pixel_width {
            let start = viewport.scroll_offset + (col as f64 * samples_per_pixel_column) as usize;
            let end = viewport.scroll_offset + ((col + 1) as f64 * samples_per_pixel_column) as usize;
            let end = end.min(samples.len());
            if start >= samples.len() || start >= end {
                prev_span = None;
                continue;
            }

            let (min, max) = match cache {
                Some(cache) => cache.min_max(samples, start, end),
                None => raw_min_max(&samples[start..end]),
            };

            let scaled_min = (min * viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;
            let scaled_max = (max * viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;

            // Amplitude 1.0 is the top pixel row, -1.0 the bottom, 0.0 the middle. The span
            // stays in continuous (sub-pixel) coordinates; draw_vspan_aa blends the
            // fractional edge rows rather than snapping to whole rows, which is what keeps
            // sub-pixel amplitude changes from rendering as a staircase.
            let mut top_y = (mid_y - scaled_max * half_height).clamp(0.0, pixel_height as f64);
            let mut bottom_y = (mid_y - scaled_min * half_height).clamp(0.0, pixel_height as f64);

            let selected = selection.is_some_and(|(sel_start, sel_end)| start < sel_end && end > sel_start);
            let color = if selected { selected_color } else { waveform_color };

            if let Some((prev_top, prev_bottom)) = prev_span {
                top_y = top_y.min(prev_bottom - 0.5).max(0.0);
                bottom_y = bottom_y.max(prev_top + 0.5).min(pixel_height as f64);
            }
            draw_vspan_aa(&mut img, col, top_y, bottom_y, color);
            prev_span = Some((top_y, bottom_y));
        }
    }

    // Markers are drawn first (and at the cursor's color when a marker sits exactly on the
    // insertion point, so it doesn't visually hide the cursor — mirrors the text renderer's
    // `marker_at_cursor_style` recoloring), then the cursor and playhead on top, since all of
    // this lives inside the bitmap rather than as separate terminal-cell overlays: drawing a
    // marker as a plain character cell over a kitty unicode-placeholder image cell corrupts
    // the terminal's own cursor-position bookkeeping for that row (the placeholder's escape
    // sequence assumes exclusive control of every cell in its row), which is what caused
    // markers to render invisibly or garble the display when placed in graphics mode.
    let marker_color = color_to_rgba(theme::MARKER);
    let cursor_color = color_to_rgba(theme::CURSOR);
    for &(marker, _) in markers {
        let color = if marker == cursor { cursor_color } else { marker_color };
        draw_marker_line(&mut img, viewport, marker, samples_per_pixel_column, color);
    }

    draw_marker_line(&mut img, viewport, cursor, samples_per_pixel_column, cursor_color);
    if let Some(ph) = playhead {
        draw_marker_line(&mut img, viewport, ph, samples_per_pixel_column, color_to_rgba(theme::PLAYHEAD));
    }

    // Labels are rasterized directly into the bitmap (only by the topmost channel, mirroring
    // the text renderer drawing them once on the waveform pane's top row) for the same reason
    // marker lines moved in here: a plain character cell drawn over a kitty image cell fights
    // the image for control of that row's escape sequence and corrupts the terminal display.
    if show_marker_labels {
        draw_marker_labels(&mut img, viewport, markers, cursor, samples_per_pixel_column);
    }

    img
}

/// Pixel width/height of one rendered glyph at [`LABEL_SCALE`] — `font8x8` glyphs are 8x8.
const GLYPH_PX: i64 = 8 * LABEL_SCALE;
/// Integer upscale applied to the 8x8 bitmap font. Left at 1 (native size): a typical
/// terminal cell is only slightly taller than 8px, so upscaling made labels look oversized
/// and blocky relative to the rest of the UI.
const LABEL_SCALE: i64 = 1;

/// Draws each visible marker's label as bitmap text along the image's top edge, clipped
/// before the next marker's line (or the image's right edge) so adjacent labels never
/// overlap — the pixel-space equivalent of the text renderer's `visible`/`avail` clipping.
fn draw_marker_labels(
    img: &mut RgbaImage,
    viewport: &Viewport,
    markers: &[(usize, &str)],
    cursor: usize,
    samples_per_pixel_column: f64,
) {
    let marker_color = color_to_rgba(theme::MARKER);
    let cursor_color = color_to_rgba(theme::CURSOR);
    let background = color_to_rgba(theme::BASE);

    let mut visible: Vec<(i64, &str, Rgba<u8>)> = markers
        .iter()
        .filter_map(|&(position, label)| {
            if position < viewport.scroll_offset {
                return None;
            }
            let col = ((position - viewport.scroll_offset) as f64 / samples_per_pixel_column) as i64;
            (0..img.width() as i64).contains(&col).then(|| {
                let color = if position == cursor { cursor_color } else { marker_color };
                (col, label, color)
            })
        })
        .collect();
    visible.sort_by_key(|&(col, _, _)| col);

    for (i, &(col, label, color)) in visible.iter().enumerate() {
        let limit = visible.get(i + 1).map(|&(c, _, _)| c).unwrap_or(img.width() as i64);
        // Leave a 1px gap past the marker's own line before the label starts.
        let lx = col + LABEL_SCALE;
        let max_chars = ((limit - lx) / GLYPH_PX).max(0) as usize;
        for (ci, ch) in label.chars().take(max_chars).enumerate() {
            draw_glyph(img, lx + ci as i64 * GLYPH_PX, 0, ch, color, background);
        }
    }
}

/// Draws one `font8x8` glyph at `(x0, y0)`, upscaled by [`LABEL_SCALE`] — every pixel in the
/// glyph's cell is painted (`fg` for a set bit, `bg` otherwise) rather than leaving "off"
/// pixels transparent, so label text stays legible over a busy waveform underneath.
fn draw_glyph(img: &mut RgbaImage, x0: i64, y0: i64, ch: char, fg: Rgba<u8>, bg: Rgba<u8>) {
    let code = ch as u32;
    let glyph = if code < 128 {
        font8x8::legacy::BASIC_LEGACY[code as usize]
    } else {
        font8x8::legacy::NOTHING_TO_DISPLAY
    };
    for (row, bits) in glyph.iter().enumerate() {
        for col in 0..8i64 {
            // `font8x8`'s own documented example (`legacy` module docs) iterates `1 << bit`
            // for bit in 0..8 and prints left-to-right, i.e. bit 0 is the *leftmost* column —
            // using bit 7 as leftmost mirrors every asymmetric glyph (invisible on a
            // left-right-symmetric letter like "A", obvious on "M"/"R"/"K").
            let on = bits & (1 << col) != 0;
            let color = if on { fg } else { bg };
            for sy in 0..LABEL_SCALE {
                for sx in 0..LABEL_SCALE {
                    let px = x0 + col * LABEL_SCALE + sx;
                    let py = y0 + row as i64 * LABEL_SCALE + sy;
                    if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                        img.put_pixel(px as u32, py as u32, color);
                    }
                }
            }
        }
    }
}

/// Draws a single full-height vertical line at `sample`'s pixel column, if it's currently
/// visible — the pixel-resolution equivalent of `waveform::playhead_column` plus the
/// cursor/playhead drawing loop, used for both the cursor and the playhead (playhead drawn
/// second by the caller, so it visually overrides the cursor at overlapping columns during
/// playback, matching the text renderer's draw order).
fn draw_marker_line(img: &mut RgbaImage, viewport: &Viewport, sample: usize, samples_per_pixel_column: f64, color: Rgba<u8>) {
    if sample < viewport.scroll_offset {
        return;
    }
    let col = ((sample - viewport.scroll_offset) as f64 / samples_per_pixel_column) as i64;
    if !(0..img.width() as i64).contains(&col) {
        return;
    }
    let col = col as u32;
    for row in 0..img.height() {
        img.put_pixel(col, row, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::viewport::Viewport;

    fn viewport(scroll_offset: usize, samples_per_column: f64) -> Viewport {
        Viewport {
            samples_per_column,
            scroll_offset,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: 1_000_000.0,
            total_len: 1_000_000,
            auto_vertical_zoom: false,
        }
    }

    #[test]
    fn smoke_test_image_has_the_requested_dimensions() {
        let img = smoke_test_image(64, 32);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }

    #[test]
    fn smoke_test_image_clamps_zero_dimensions_to_one_pixel() {
        let img = smoke_test_image(0, 0);
        assert_eq!(img.width(), 1);
        assert_eq!(img.height(), 1);
    }

    #[test]
    fn empty_samples_renders_background_only() {
        let img = rasterize_waveform(&[], &viewport(0, 1.0), None, None, 0, None, &[], false, 80, 160, 40);
        let bg = color_to_rgba(theme::BASE);
        for pixel in img.pixels() {
            assert_eq!(*pixel, bg);
        }
    }

    #[test]
    fn loud_signal_reaches_near_the_top_and_bottom_rows() {
        // Oscillating between +1.0 and -1.0 so a single column's min/max spans the full
        // amplitude range (a constant 1.0 would only ever reach the *top*, since min==max).
        let samples: Vec<f32> = (0..1000).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, None, 0, None, &[], false, 80, 160, 40);
        let waveform_color = color_to_rgba(theme::WAVEFORM);
        assert_eq!(*img.get_pixel(80, 0), waveform_color, "a full-amplitude signal should reach the top row");
        assert_eq!(
            *img.get_pixel(80, 39),
            waveform_color,
            "a full-amplitude signal should reach the bottom row"
        );
    }

    #[test]
    fn selection_uses_the_selected_color() {
        let samples = vec![1.0f32; 1000];
        // cell_width=80, pixel_width=160 -> 2px per character column, samples_per_column=12.5
        // -> 1000 samples span the whole 80-column / 160px width.
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, Some((0, 500)), 0, None, &[], false, 80, 160, 40);
        let selected_color = color_to_rgba(theme::WAVEFORM_SELECTED);
        let unselected_color = color_to_rgba(theme::WAVEFORM);
        assert_eq!(*img.get_pixel(10, 0), selected_color, "left half (selected) should use the selected color");
        assert_eq!(*img.get_pixel(150, 0), unselected_color, "right half (unselected) should use the normal color");
    }

    #[test]
    fn cursor_and_playhead_draw_vertical_lines_with_playhead_on_top() {
        let samples = vec![0.1f32; 1000];
        let cursor_color = color_to_rgba(theme::CURSOR);
        let playhead_color = color_to_rgba(theme::PLAYHEAD);

        // Cursor alone, away from column 0, draws its color somewhere in its column.
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, None, 500, None, &[], false, 80, 160, 40);
        let cursor_col = (500.0 / 12.5 * (160.0 / 80.0)) as u32; // sample -> pixel column
        assert_eq!(*img.get_pixel(cursor_col, 5), cursor_color);

        // With a playhead at the same sample position, the playhead color wins (drawn
        // after the cursor), matching the text renderer's "playhead overrides cursor" order.
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, None, 500, Some(500), &[], false, 80, 160, 40);
        assert_eq!(*img.get_pixel(cursor_col, 5), playhead_color);
    }

    #[test]
    fn marker_draws_a_vertical_line_in_the_marker_color() {
        let samples = vec![0.1f32; 1000];
        let marker_color = color_to_rgba(theme::MARKER);
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, None, 0, None, &[(500, "")], false, 80, 160, 40);
        let marker_col = (500.0 / 12.5 * (160.0 / 80.0)) as u32;
        assert_eq!(*img.get_pixel(marker_col, 5), marker_color);
    }

    #[test]
    fn marker_at_the_cursor_uses_the_cursor_color_instead() {
        let samples = vec![0.1f32; 1000];
        let cursor_color = color_to_rgba(theme::CURSOR);
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, None, 500, None, &[(500, "")], false, 80, 160, 40);
        let marker_col = (500.0 / 12.5 * (160.0 / 80.0)) as u32;
        assert_eq!(
            *img.get_pixel(marker_col, 5),
            cursor_color,
            "a marker coincident with the cursor should not hide it"
        );
    }

    #[test]
    fn out_of_view_cursor_draws_nothing() {
        let samples = vec![0.1f32; 1000];
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, None, 999_999, None, &[], false, 80, 160, 40);
        let cursor_color = color_to_rgba(theme::CURSOR);
        assert!(img.pixels().all(|p| *p != cursor_color), "a cursor scrolled out of view must not draw");
    }

    #[test]
    fn line_mode_renders_nonblank_at_sub_one_spc() {
        // At zoom=1.0 spl/col the pixel-column spc is far below 1.0 (many pixels per sample).
        // The old bar-mode code produced a blank image here because start==end for every column.
        // Line mode must draw visible waveform pixels instead.
        //
        // Geometry: 200 samples, spc=20.0, span=200 → all samples visible; pixel_width=200
        // → spc_px = 200/200 = 1.0 (still < 4.0, triggers line mode). The ramp goes from
        // -1.0 (bottom) to +1.0 (top) across the full visible width.
        let samples: Vec<f32> = (0..200).map(|i| (i as f32 / 199.0) * 2.0 - 1.0).collect();
        let vp = viewport(0, 20.0); // span = 20 * 10 cols = 200 samples
        let img = rasterize_waveform(&samples, &vp, None, None, 0, None, &[], false, 10, 200, 80);
        let bg = color_to_rgba(theme::BASE);
        assert!(
            img.pixels().any(|p| *p != bg),
            "line mode must render waveform pixels, not a blank image"
        );
        // Ramp goes -1→+1 so the left side should be near the bottom and right side near the top.
        let top_quarter = img.rows().take(20).flatten().any(|p| *p != bg);
        let bot_quarter = img.rows().rev().take(20).flatten().any(|p| *p != bg);
        assert!(top_quarter, "rising ramp must reach the top quarter of the image by the end");
        assert!(bot_quarter, "rising ramp must start in the bottom quarter of the image");
        // Very few columns should be fully blank — the old code blanked everything.
        let blank_columns = (0..200u32).filter(|&x| (0..80u32).all(|y| *img.get_pixel(x, y) == bg)).count();
        assert!(blank_columns < 10, "line mode should leave very few fully-blank columns (got {blank_columns})");
    }

    #[test]
    fn bar_mode_trace_is_vertically_connected_on_steep_slopes() {
        // A sine whose period is ~8 pixel columns at this zoom: near each zero crossing the
        // signal moves most of a column-bar's height *between* the last sample of one column
        // and the first sample of the next — the regime where independent per-column min/max
        // bars leave multi-pixel vertical gaps and the trace breaks into dashes.
        let samples: Vec<f32> = (0..2000)
            .map(|i| (2.0 * std::f32::consts::PI * i as f32 / 48.0).sin())
            .collect();
        // cell_width=100 at 12 spl/col -> span 1200 samples over 200px -> 6 samples per
        // pixel column (bar mode, above the 4.0 polyline threshold).
        let vp = viewport(0, 12.0);
        let img = rasterize_waveform(&samples, &vp, None, None, 0, None, &[], false, 100, 200, 200);
        let bg = color_to_rgba(theme::BASE);

        // Any non-background pixel counts as trace coverage — anti-aliased edge pixels are
        // blends, not the exact waveform color.
        let rows_of = |x: u32| -> Vec<u32> {
            (0..img.height()).filter(|&y| *img.get_pixel(x, y) != bg).collect()
        };
        // Skip column 0/1 (the cursor line at sample 0 recolors column 0).
        for x in 2..img.width() - 1 {
            let a = rows_of(x);
            let b = rows_of(x + 1);
            if a.is_empty() || b.is_empty() {
                continue;
            }
            let (a_lo, a_hi) = (*a.first().unwrap(), *a.last().unwrap());
            let (b_lo, b_hi) = (*b.first().unwrap(), *b.last().unwrap());
            assert!(
                a_lo <= b_hi && b_lo <= a_hi,
                "columns {x} and {} must share at least one row (got [{a_lo},{a_hi}] vs [{b_lo},{b_hi}]) — \
                 the trace visibly breaks apart otherwise",
                x + 1
            );
        }
    }

    #[test]
    fn sub_pixel_amplitudes_render_anti_aliased_not_staircased() {
        // A slow, quiet ripple whose per-column amplitude change is a fraction of a pixel
        // row. Snapping spans to whole rows renders this as flat runs with hard 1px jumps
        // (a staircase); anti-aliasing must instead express the sub-pixel positions as
        // partially-covered edge pixels — blends strictly between background and the
        // waveform color.
        let samples: Vec<f32> = (0..4000)
            .map(|i| 0.05 * (2.0 * std::f32::consts::PI * i as f32 / 1200.0).sin())
            .collect();
        let vp = viewport(0, 12.0); // 6 samples per pixel column -> bar mode
        let img = rasterize_waveform(&samples, &vp, None, None, 0, None, &[], false, 100, 200, 200);
        let bg = color_to_rgba(theme::BASE);
        let waveform_color = color_to_rgba(theme::WAVEFORM);
        let cursor_color = color_to_rgba(theme::CURSOR);
        let blended = img
            .pixels()
            .filter(|p| **p != bg && **p != waveform_color && **p != cursor_color)
            .count();
        assert!(
            blended > 0,
            "sub-pixel span edges must produce blended (anti-aliased) pixels, not snap to whole rows"
        );
    }

    #[test]
    fn marker_label_is_rasterized_when_requested() {
        let samples = vec![0.1f32; 1000];
        let marker_color = color_to_rgba(theme::MARKER);
        let background = color_to_rgba(theme::BASE);
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, None, 0, None, &[(500, "M")], true, 80, 160, 40);
        let marker_col = (500.0 / 12.5 * (160.0 / 80.0)) as i64;
        let label_x = (marker_col + LABEL_SCALE) as u32;
        let has_label_pixels = (0..8u32).any(|row| {
            (0..GLYPH_PX as u32).any(|col| *img.get_pixel(label_x + col, row) == marker_color)
        });
        assert!(has_label_pixels, "expected some marker-colored pixels in the label's glyph cell");
        // No label rendered without the request — column stays background past the line.
        let img = rasterize_waveform(&samples, &viewport(0, 12.5), None, None, 0, None, &[(500, "M")], false, 80, 160, 40);
        assert!(
            (0..8u32).all(|row| (0..GLYPH_PX as u32).all(|col| *img.get_pixel(label_x + col, row) == background)),
            "expected no label pixels when show_marker_labels is false"
        );
    }
}

