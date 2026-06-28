use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use crate::ui::theme;
use crate::ui::viewport::Viewport;
use crate::ui::waveform_cache::{raw_min_max, WaveformCache};

/// Lower-eighth block characters, U+2581..U+2588 — chosen over Nerd Font glyphs (which are
/// icon-style symbols, e.g. file-type/git icons, not graduated fill levels) and over the
/// less universally-supported upper-eighth blocks (Unicode's Legacy Computing Supplement,
/// patchy font coverage). These eight are standard Unicode, present in essentially every
/// monospace font, and are what terminal sparkline/plot tools (gnuplot's dumb terminal,
/// ttyplot, etc.) already rely on.
const LOWER_EIGHTHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn lower_eighth(n: u8) -> Option<char> {
    (1..=8).contains(&n).then(|| LOWER_EIGHTHS[(n - 1) as usize])
}

/// Renders one channel's waveform into `area` using min/max downsampling: each terminal
/// column represents `viewport.samples_per_column` samples. The per-column min/max comes
/// from the precomputed `WaveformCache` rather than scanning raw samples, so render cost
/// stays bounded by screen width regardless of file length or zoom level.
///
/// The bar's top and bottom edges land at fractional (sub-row) positions almost everywhere
/// except by coincidence — floor/ceil-ing them to whole character rows throws away most of
/// that precision, which matters most for quiet signals and zoomed-in views where a bar
/// might only be 1-2 rows tall to begin with. Instead, the boundary row at each edge is
/// drawn with a lower-eighth-block glyph sized to its fractional coverage: directly for the
/// top edge (a lower-N/8 glyph already fills "from the bottom up", which is the right
/// orientation there), and via an fg/bg swap on the complementary glyph for the bottom edge
/// (filling "from the top down" using only bottom-aligned glyphs, the only kind with
/// reliable font support).
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
}

/// The terminal column the playhead falls on, given the current scroll/zoom, or `None`
/// when it's scrolled out of view (off the left edge or past the right edge).
fn playhead_column(viewport: &Viewport, playhead: usize, width: u16) -> Option<u16> {
    if playhead < viewport.scroll_offset {
        return None;
    }
    let col = ((playhead - viewport.scroll_offset) as f64 / viewport.samples_per_column) as i64;
    (0..width as i64).contains(&col).then(|| col as u16)
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

        let mid_row = area.height as f64 / 2.0;
        let half_height = area.height as f64 / 2.0;

        for col in 0..area.width {
            let start = self.viewport.scroll_offset
                + (col as f64 * self.viewport.samples_per_column) as usize;
            let end = self.viewport.scroll_offset
                + ((col + 1) as f64 * self.viewport.samples_per_column) as usize;
            let end = end.min(self.samples.len());
            if start >= self.samples.len() || start >= end {
                continue;
            }

            let (min, max) = match self.cache {
                Some(cache) => cache.min_max(self.samples, start, end),
                None => raw_min_max(&self.samples[start..end]),
            };

            let scaled_min = (min * self.viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;
            let scaled_max = (max * self.viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;

            // Amplitude 1.0 is the top row, -1.0 is the bottom row, 0.0 is mid_row. These
            // are continuous (sub-row) positions, not yet rounded to whole character rows.
            let top_y = (mid_row - scaled_max * half_height).clamp(0.0, area.height as f64);
            let bottom_y = (mid_row - scaled_min * half_height).clamp(0.0, area.height as f64);

            let selected = self
                .selection
                .is_some_and(|(sel_start, sel_end)| start < sel_end && end > sel_start);
            let color = if selected {
                theme::WAVEFORM_SELECTED
            } else {
                theme::WAVEFORM
            };
            let x = area.x + col;
            // Inverted selection: fill the whole column with the waveform color as the
            // background, so the bar (drawn in WAVEFORM_SELECTED / YELLOW on top of it)
            // looks like the palette has swapped relative to an unselected column.
            let bg = if selected { theme::WAVEFORM } else { theme::BASE };
            if selected {
                for row in 0..area.height {
                    buf[(x, area.y + row)].set_bg(theme::WAVEFORM);
                }
            }

            // A column spanning very few samples — in the limit exactly one, where
            // min == max — has zero geometric height here and would otherwise render
            // nothing at all (the bug at high zoom: a 1:1 sample/column view going blank).
            // Draw it as a single eighth-block sliver positioned at the value's row instead
            // of skipping — the finest unit this renderer can represent anyway, and the
            // closest approximation to "a single sample" a character cell allows.
            const MIN_BAR_HEIGHT: f64 = 1.0 / 8.0;
            if bottom_y - top_y < MIN_BAR_HEIGHT {
                // True silence (min == max == 0) stays blank, same as a longer silent span
                // (which also has zero geometric height and renders nothing) — only a
                // non-zero degenerate column gets the visibility fix below.
                if min == 0.0 && max == 0.0 {
                    continue;
                }
                let center = ((top_y + bottom_y) / 2.0).min(area.height as f64 - f64::EPSILON).max(0.0);
                let row = center.floor() as i64;
                if row >= 0 && row < area.height as i64 {
                    let frac_into_row = center - row as f64;
                    let filled = (((1.0 - frac_into_row) * 8.0).round() as i64).clamp(1, 8) as u8;
                    if let Some(ch) = lower_eighth(filled) {
                        let y = area.y + row as u16;
                        buf[(x, y)].set_char(ch).set_style(Style::default().fg(color));
                    }
                }
                continue;
            }

            let top_row = top_y.floor() as i64;
            let bottom_row_excl = bottom_y.ceil() as i64;

            for row in top_row..bottom_row_excl {
                if row < 0 || row >= area.height as i64 {
                    continue;
                }
                let y = area.y + row as u16;
                let row_top = row as f64;
                let row_bottom = row as f64 + 1.0;

                let is_top_edge = row_top < top_y && top_y < row_bottom;
                let is_bottom_edge = row_top < bottom_y && bottom_y < row_bottom;

                if is_top_edge {
                    let frac_into_row = top_y - row_top; // how far down the edge falls
                    let filled = ((1.0 - frac_into_row) * 8.0).round() as i64;
                    if let Some(ch) = lower_eighth(filled.clamp(0, 8) as u8) {
                        buf[(x, y)].set_char(ch).set_style(Style::default().fg(color));
                    }
                } else if is_bottom_edge {
                    let frac_into_row = bottom_y - row_top; // how far down to fill from the top
                    let filled = (frac_into_row * 8.0).round() as i64;
                    let complement = 8 - filled.clamp(0, 8);
                    if let Some(ch) = lower_eighth(complement as u8) {
                        // Swap fg/bg: the glyph's "ink" (bottom complement/8) renders in the
                        // pane background color, while the "non-ink" area (top filled/8 — what
                        // we actually want colored) renders in the bar's background. When
                        // selected the pane background is WAVEFORM (SKY) not BASE.
                        buf[(x, y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(bg).bg(color));
                    } else {
                        buf[(x, y)].set_char(' ').set_style(Style::default().bg(bg));
                    }
                } else {
                    buf[(x, y)].set_char('█').set_style(Style::default().fg(color));
                }
            }
        }

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

    #[test]
    fn lower_eighth_covers_one_through_eight() {
        assert_eq!(lower_eighth(1), Some('▁'));
        assert_eq!(lower_eighth(8), Some('█'));
        assert_eq!(lower_eighth(0), None);
        assert_eq!(lower_eighth(9), None);
    }

    /// At 1 sample/column (max zoom in), every column spans exactly one sample, so
    /// min == max and the bar has zero geometric height — the bug being guarded against
    /// here is that such columns used to render nothing at all. Every column with a
    /// non-zero sample must now show at least the eighth-block sliver.
    #[test]
    fn single_sample_columns_render_a_sliver_instead_of_going_blank() {
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
        };
        widget.render(area, &mut buf);

        for x in 0..20u16 {
            let has_mark = (0..10u16).any(|y| buf[(x, y)].symbol() != " ");
            assert!(has_mark, "column {x} rendered nothing for a non-zero single-sample value");
        }
    }

    /// Non-integer zoom levels (e.g. 1.5 samples/column) mix degenerate one-sample columns
    /// with non-degenerate two-sample ones — the one-sample columns used to go blank,
    /// producing a sparse, inconsistent look. They must render a sliver too.
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
        };
        widget.render(area, &mut buf);

        for x in 0..26u16 {
            let has_mark = (0..10u16).any(|y| buf[(x, y)].symbol() != " ");
            assert!(has_mark, "column {x} rendered nothing at a fractional zoom level");
        }
    }

    /// A literally silent (all-zero) single-sample column is the one case that should
    /// legitimately render nothing — there's no amplitude to show a sliver for.
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
        };
        widget.render(area, &mut buf);

        for x in 0..5u16 {
            for y in 0..10u16 {
                assert_eq!(buf[(x, y)].symbol(), " ", "a silent sample should not draw a mark");
            }
        }
    }
}
