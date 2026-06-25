//! Graphics-mode waveform rendering (kitty/Sixel/iTerm2 protocols via `ratatui-image`),
//! used instead of `widgets::waveform`'s character-glyph renderer when the terminal
//! supports a real bitmap protocol (see `terminal::detect_graphics_picker`).
//!
//! [`rasterize_waveform`] mirrors [`super::waveform::WaveformWidget`]'s per-column min/max
//! downsampling exactly — same `WaveformCache` lookups, same theme colors, same
//! cursor/playhead/selection logic — just at real pixel resolution instead of one glyph per
//! character cell, so there's no eighth-block sub-row rounding to reason about: a pixel row
//! either is or isn't inside the bar's continuous `[top_y, bottom_y)` span.

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

    for col in 0..pixel_width {
        let start = viewport.scroll_offset + (col as f64 * samples_per_pixel_column) as usize;
        let end = viewport.scroll_offset + ((col + 1) as f64 * samples_per_pixel_column) as usize;
        let end = end.min(samples.len());
        if start >= samples.len() || start >= end {
            continue;
        }

        let (min, max) = match cache {
            Some(cache) => cache.min_max(samples, start, end),
            None => raw_min_max(&samples[start..end]),
        };

        let scaled_min = (min * viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;
        let scaled_max = (max * viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;

        // Amplitude 1.0 is the top pixel row, -1.0 the bottom, 0.0 the middle — continuous
        // (sub-pixel) positions in principle, but at real pixel resolution there's no
        // eighth-block rounding to do: floor/ceil to whole pixel rows directly, the same
        // precision loss a real bitmap waveform display would have (a fraction of a pixel
        // row, not a fraction of an 8-level glyph).
        let top_y = (mid_y - scaled_max * half_height).clamp(0.0, pixel_height as f64);
        let bottom_y = (mid_y - scaled_min * half_height).clamp(0.0, pixel_height as f64);

        let selected = selection.is_some_and(|(sel_start, sel_end)| start < sel_end && end > sel_start);
        let color = if selected { selected_color } else { waveform_color };

        let top_row = top_y.floor() as u32;
        let bottom_row_excl = (bottom_y.ceil() as u32).max(top_row + 1).min(pixel_height);
        for row in top_row..bottom_row_excl {
            img.put_pixel(col, row, color);
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
