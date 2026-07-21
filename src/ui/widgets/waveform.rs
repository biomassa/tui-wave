use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use crate::model::dsp;
use crate::ui::theme;
use crate::ui::viewport::Viewport;
use crate::ui::waveform_cache::{raw_min_max, WaveformCache};
use crate::ui::widgets::braille::{braille_char, DOT_BITS};

/// Renders one channel's waveform into `area` as braille dot-matrix glyphs, giving 2x
/// horizontal resolution (each terminal column splits into a left/right sub-column with its
/// own min/max, from the precomputed `WaveformCache` rather than scanning raw samples so
/// render cost stays bounded by screen width) and 4x vertical resolution (4 dot-rows per
/// character row) versus one glyph per cell. Each terminal cell still carries only one
/// foreground color (a braille glyph can't color individual dots), so an unselected column's
/// color is graded per row by `theme::gradient_color` from `WAVEFORM_DOT_LOW` (near the
/// centerline) through `WAVEFORM_DOT_MID` to `WAVEFORM_DOT_HIGH` (near the amplitude
/// extremes) — the same "position on screen IS the amplitude" mapping the dB gutter uses, so
/// louder rows read as a warmer color, echoing btop's braille CPU graphs shading by height —
/// unless `gradient` is off (flat `WAVEFORM_DOT_LOW`). Selected columns skip the gradient
/// entirely (flat `WAVEFORM_SELECTED`).
pub struct WaveformWidget<'a> {
    pub samples: &'a [f32],
    pub viewport: &'a Viewport,
    pub cache: Option<&'a WaveformCache>,
    /// Normalized (start, end) sample range to highlight, if any.
    pub selection: Option<(usize, usize)>,
    /// The insertion point / playback start cursor (always visible as a yellow │).
    pub cursor: usize,
    /// The playback position (only `Some` during playback, rendered as red │).
    pub playhead: Option<usize>,
    /// Whether unselected dots are graded by amplitude (`theme::gradient_color`) or drawn
    /// flat at `theme::WAVEFORM_DOT_LOW`. See `Config.dot_matrix_gradient`.
    pub gradient: bool,
}

/// The terminal column the playhead falls on, given the current scroll/zoom, or `None`
/// when it's scrolled out of view (off the left edge or past the right edge).
fn playhead_column(viewport: &Viewport, playhead: usize, width: u16) -> Option<u16> {
    if playhead < viewport.scroll_offset {
        return None;
    }
    let col = ((playhead - viewport.scroll_offset) as f64 / viewport.samples_per_column) as i64;
    (0..width as i64).contains(&col).then_some(col as u16)
}

impl<'a> Widget for WaveformWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                buf[(x, y)].set_bg(theme::BASE);
            }
        }

        if self.samples.is_empty() {
            return;
        }

        self.render_dots(area, buf);

        // Draw the cursor (insertion point) first so the playhead (drawn second) can
        // visually override it at overlapping columns during playback.
        if let Some(col) = playhead_column(self.viewport, self.cursor, area.width) {
            let x = area.x + col;
            for row in 0..area.height {
                let y = area.y + row;
                buf[(x, y)]
                    .set_char('│')
                    .set_style(Style::default().fg(theme::CURSOR).bg(theme::BASE).add_modifier(Modifier::BOLD));
            }
        }

        // Draw the playback playhead on top — only present during active playback.
        if let Some(ph) = self.playhead {
            if let Some(col) = playhead_column(self.viewport, ph, area.width) {
                let x = area.x + col;
                for row in 0..area.height {
                    let y = area.y + row;
                    buf[(x, y)]
                        .set_char('│')
                        .set_style(Style::default().fg(theme::PLAYHEAD).bg(theme::BASE).add_modifier(Modifier::BOLD));
                }
            }
        }
    }
}

impl<'a> WaveformWidget<'a> {
    fn render_dots(&self, area: Rect, buf: &mut Buffer) {
        let mid_row = area.height as f64 / 2.0;
        let half_height = area.height as f64 / 2.0;
        let dot_rows = area.height as i64 * 4;
        let dot_mid = mid_row * 4.0;
        let dot_half_height = half_height * 4.0;
        let selection_bg = theme::dot_matrix_selection_bg();

        for col in 0..area.width {
            let start = self.viewport.scroll_offset
                + (col as f64 * self.viewport.samples_per_column) as usize;
            let end = self.viewport.scroll_offset
                + ((col + 1) as f64 * self.viewport.samples_per_column) as usize;
            let end = end.min(self.samples.len());
            if start >= self.samples.len() || start >= end {
                continue;
            }

            let selected = self
                .selection
                .is_some_and(|(sel_start, sel_end)| start < sel_end && end > sel_start);
            // Selection uses a dimmed version of the gradient's own "quiet" green — paired
            // with the flat black (`WAVEFORM_SELECTED`) dots below, so a selection reads as
            // "gradient inverted to its low end," not an unrelated accent color.
            let bg = if selected { selection_bg } else { theme::BASE };
            let x = area.x + col;
            if selected {
                for row in 0..area.height {
                    buf[(x, area.y + row)].set_bg(selection_bg);
                }
            }

            // Split the column's sample range at its midpoint into a left and right
            // sub-column, each queried independently — this is what gives the dot-matrix
            // renderer its extra horizontal resolution over one flat bar per column.
            let mid_sample = start + (end - start) / 2;
            let sub_ranges = [(start, mid_sample), (mid_sample, end)];

            let mut masks = vec![0u8; area.height as usize];

            for (sub_col, &(s, e)) in sub_ranges.iter().enumerate() {
                // A degenerate half (e.g. a 1-sample column split in two) falls back to the
                // full column's range so it still reflects real data instead of going blank.
                let (min, max) = match self.cache {
                    Some(cache) if s < e => cache.min_max(self.samples, s, e),
                    Some(cache) => cache.min_max(self.samples, start, end),
                    None if s < e => raw_min_max(&self.samples[s..e]),
                    None => raw_min_max(&self.samples[start..end]),
                };
                if min == 0.0 && max == 0.0 {
                    continue;
                }

                let scaled_min = (min * self.viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;
                let scaled_max = (max * self.viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;
                let top_dot = (dot_mid - scaled_max * dot_half_height).clamp(0.0, dot_rows as f64);
                let bottom_dot = (dot_mid - scaled_min * dot_half_height).clamp(0.0, dot_rows as f64);

                let (top_idx, bottom_idx_excl) = if bottom_dot - top_dot < 1.0 {
                    let center = ((top_dot + bottom_dot) / 2.0).min(dot_rows as f64 - f64::EPSILON).max(0.0);
                    let idx = center.floor() as i64;
                    (idx, idx + 1)
                } else {
                    (top_dot.floor() as i64, bottom_dot.ceil() as i64)
                };

                for dot_idx in top_idx..bottom_idx_excl {
                    if dot_idx < 0 || dot_idx >= dot_rows {
                        continue;
                    }
                    let row = (dot_idx / 4) as usize;
                    let local = (dot_idx % 4) as usize;
                    masks[row] |= DOT_BITS[local][sub_col];
                }
            }

            for (row, &mask) in masks.iter().enumerate() {
                if mask == 0 {
                    continue;
                }
                let y = area.y + row as u16;
                let color = if selected {
                    theme::WAVEFORM_SELECTED
                } else if self.gradient {
                    let amp_frac = ((mid_row - (row as f64 + 0.5)) / half_height).abs().clamp(0.0, 1.0) as f32;
                    theme::gradient_color(dsp::linear_to_db(amp_frac))
                } else {
                    theme::WAVEFORM_DOT_LOW
                };
                buf[(x, y)].set_char(braille_char(mask)).set_style(Style::default().fg(color).bg(bg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn playhead_column_at_left_edge() {
        let v = viewport(1000, 10.0);
        assert_eq!(playhead_column(&v, 1000, 80), Some(0));
    }

    #[test]
    fn playhead_column_mid_view() {
        let v = viewport(1000, 10.0);
        // 50 columns in => sample 1000 + 50*10 = 1500
        assert_eq!(playhead_column(&v, 1500, 80), Some(50));
    }

    #[test]
    fn playhead_column_none_when_scrolled_out_of_view() {
        let v = viewport(1000, 10.0);
        assert_eq!(playhead_column(&v, 500, 80), None); // before the visible window
        assert_eq!(playhead_column(&v, 1000 + 10 * 80, 80), None); // past the right edge
    }

    /// At 1 sample/column (max zoom in), every column spans exactly one sample, so min ==
    /// max and the span has zero geometric height — the bug being guarded against here is
    /// that such columns used to render nothing at all. Every column with a non-zero sample
    /// must show at least a single dot.
    #[test]
    fn single_sample_columns_render_a_dot_instead_of_going_blank() {
        let samples: Vec<f32> = (0..20).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let area = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(area);
        let widget = WaveformWidget {
            samples: &samples,
            viewport: &viewport(0, 1.0),
            cache: None,
            selection: None,
            cursor: usize::MAX, // off-screen, so the cursor line doesn't interfere
            playhead: None,
            gradient: true,
        };
        widget.render(area, &mut buf);

        for x in 0..20u16 {
            let has_mark = (0..10u16).any(|y| buf[(x, y)].symbol() != " ");
            assert!(has_mark, "column {x} rendered nothing for a non-zero single-sample value");
        }
    }

    /// Non-integer zoom levels (e.g. 1.5 samples/column) mix degenerate one-sample columns
    /// with non-degenerate two-sample ones — the one-sample columns used to go blank,
    /// producing a sparse, inconsistent look. They must render a dot too.
    #[test]
    fn fractional_samples_per_column_has_no_blank_columns_for_nonzero_audio() {
        let samples: Vec<f32> = (0..40).map(|i| if i % 2 == 0 { 0.3 } else { 0.6 }).collect();
        let area = Rect::new(0, 0, 26, 10);
        let mut buf = Buffer::empty(area);
        let widget = WaveformWidget {
            samples: &samples,
            viewport: &viewport(0, 1.5),
            cache: None,
            selection: None,
            cursor: usize::MAX,
            playhead: None,
            gradient: true,
        };
        widget.render(area, &mut buf);

        for x in 0..26u16 {
            let has_mark = (0..10u16).any(|y| buf[(x, y)].symbol() != " ");
            assert!(has_mark, "column {x} rendered nothing at a fractional zoom level");
        }
    }

    /// A literally silent (all-zero) single-sample column is the one case that should
    /// legitimately render nothing — there's no amplitude to show a dot for.
    #[test]
    fn single_sample_silent_column_renders_nothing() {
        let samples = vec![0.0f32; 5];
        let area = Rect::new(0, 0, 5, 10);
        let mut buf = Buffer::empty(area);
        let widget = WaveformWidget {
            samples: &samples,
            viewport: &viewport(0, 1.0),
            cache: None,
            selection: None,
            cursor: usize::MAX,
            playhead: None,
            gradient: true,
        };
        widget.render(area, &mut buf);

        for x in 0..5u16 {
            for y in 0..10u16 {
                assert_eq!(buf[(x, y)].symbol(), " ", "a silent sample should not draw a mark");
            }
        }
    }

    #[test]
    fn dot_matrix_uses_flat_green_when_gradient_is_off() {
        let samples: Vec<f32> = (0..20).map(|i| if i % 2 == 0 { 0.9 } else { -0.9 }).collect();
        let area = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(area);
        let widget = WaveformWidget {
            samples: &samples,
            viewport: &viewport(0, 1.0),
            cache: None,
            selection: None,
            cursor: usize::MAX,
            playhead: None,
            gradient: false,
        };
        widget.render(area, &mut buf);

        for x in 0..20u16 {
            for y in 0..10u16 {
                let cell = &buf[(x, y)];
                if cell.symbol() != " " {
                    assert_eq!(cell.fg, theme::WAVEFORM_DOT_LOW, "dot at ({x},{y}) should be flat green with gradient off");
                }
            }
        }
    }

    #[test]
    fn gradient_reddens_toward_the_amplitude_extremes() {
        // Alternating near-full-scale samples, several per column, so each column's min/max
        // span the whole pane height (top-to-bottom bar) rather than one single-sample sliver.
        let samples: Vec<f32> = (0..80).map(|i| if i % 2 == 0 { 0.95 } else { -0.95 }).collect();
        let area = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(area);
        let widget = WaveformWidget {
            samples: &samples,
            viewport: &viewport(0, 4.0),
            cache: None,
            selection: None,
            cursor: usize::MAX,
            playhead: None,
            gradient: true,
        };
        widget.render(area, &mut buf);

        let edge_fg = buf[(0, 0)].fg;
        let center_fg = buf[(0, 5)].fg;
        assert_ne!(buf[(0, 0)].symbol(), " ", "edge row should be filled for a full-scale span");
        assert_ne!(buf[(0, 5)].symbol(), " ", "center row should be filled for a full-scale span");
        assert_ne!(edge_fg, center_fg, "gradient should vary color between the edge and the center");
    }
}
