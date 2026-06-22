use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::ui::viewport::Viewport;

/// Renders one channel's waveform into `area` using min/max downsampling: each terminal
/// column represents `viewport.samples_per_column` samples, never iterating every sample
/// per frame regardless of zoom level.
pub struct WaveformWidget<'a> {
    pub samples: &'a [f32],
    pub viewport: &'a Viewport,
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

            let slice = &self.samples[start..end];
            let (min, max) = slice
                .iter()
                .fold((f32::MAX, f32::MIN), |(mn, mx), &s| (mn.min(s), mx.max(s)));

            let scaled_min = (min * self.viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;
            let scaled_max = (max * self.viewport.amplitude_scale).clamp(-1.0, 1.0) as f64;

            // Amplitude 1.0 is the top row, -1.0 is the bottom row, 0.0 is mid_row.
            let top_row = (mid_row - scaled_max * half_height).floor() as i64;
            let bottom_row = (mid_row - scaled_min * half_height).ceil() as i64;

            for row in top_row.max(0)..bottom_row.min(area.height as i64) {
                let x = area.x + col;
                let y = area.y + row as u16;
                buf[(x, y)]
                    .set_char('█')
                    .set_style(Style::default().fg(Color::Cyan));
            }
        }
    }
}
