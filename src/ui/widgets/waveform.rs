use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::ui::viewport::Viewport;
use crate::ui::waveform_cache::{raw_min_max, WaveformCache};

/// Renders one channel's waveform into `area` using min/max downsampling: each terminal
/// column represents `viewport.samples_per_column` samples. The per-column min/max comes
/// from the precomputed `WaveformCache` rather than scanning raw samples, so render cost
/// stays bounded by screen width regardless of file length or zoom level.
pub struct WaveformWidget<'a> {
    pub samples: &'a [f32],
    pub viewport: &'a Viewport,
    pub cache: Option<&'a WaveformCache>,
    /// Normalized (start, end) sample range to highlight, if any.
    pub selection: Option<(usize, usize)>,
    pub playhead: usize,
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
        if area.width == 0 || area.height == 0 || self.samples.is_empty() {
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

            // Amplitude 1.0 is the top row, -1.0 is the bottom row, 0.0 is mid_row.
            let top_row = (mid_row - scaled_max * half_height).floor() as i64;
            let bottom_row = (mid_row - scaled_min * half_height).ceil() as i64;

            let selected = self
                .selection
                .is_some_and(|(sel_start, sel_end)| start < sel_end && end > sel_start);
            let color = if selected { Color::Yellow } else { Color::Cyan };

            for row in top_row.max(0)..bottom_row.min(area.height as i64) {
                let x = area.x + col;
                let y = area.y + row as u16;
                buf[(x, y)].set_char('█').set_style(Style::default().fg(color));
            }
        }

        // Drawn last so the playhead is always visible on top of the waveform, even where
        // it overlaps an existing bar.
        if let Some(col) = playhead_column(self.viewport, self.playhead, area.width) {
            let x = area.x + col;
            for row in 0..area.height {
                let y = area.y + row;
                buf[(x, y)]
                    .set_char('│')
                    .set_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
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
}
