use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use crate::ui::theme;

/// Width reserved on each side of a channel's waveform pane for the dB scale gutter.
pub const DB_GUTTER_WIDTH: u16 = 4;

const DB_MARKS: [(f32, &str); 6] = [
    (0.0, "0dB"),
    (-3.0, "-3"),
    (-6.0, "-6"),
    (-12.0, "-12"),
    (-18.0, "-18"),
    (-24.0, "-24"),
];

/// Renders the vertical dB axis for one channel's waveform pane. `reference_amplitude` is
/// 1.0 for the absolute dBFS scale (auto vertical zoom off — 0dB always means full scale)
/// or the document's peak amplitude for the relative scale (auto vertical zoom on — 0dB
/// tracks wherever the loudest peak actually is).
#[derive(Clone, Copy)]
pub struct DbScaleWidget {
    pub amplitude_scale: f32,
    pub reference_amplitude: f32,
}

impl Widget for DbScaleWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                buf[(x, y)].set_bg(theme::BASE);
            }
        }

        let mid_row = area.height as f64 / 2.0;
        let half_height = area.height as f64 / 2.0;
        // Marks are listed most- to least-important (0dB first); when the pane is too
        // short to give every mark a distinct row, the first one to claim a row wins
        // rather than a later mark silently overwriting an earlier label.
        let mut claimed_rows = vec![false; area.height as usize];

        for &(db, label) in DB_MARKS.iter() {
            let amplitude = self.reference_amplitude * 10f32.powf(db / 20.0);
            // No clamp here — off-screen marks (scaled > 1.0) produce rows < 0 or >= height,
            // which draw_label already rejects. Clamping to [0,1] was wrong: at 2x vertical
            // zoom (amplitude_scale=2.0) 0dB, -3dB, and -6dB all clamped to 1.0 and stacked
            // at row 0, hiding the fact that those levels are above the visible amplitude range.
            let scaled = (amplitude * self.amplitude_scale) as f64;

            let top_row = (mid_row - scaled * half_height).round() as i64;
            draw_label(buf, area, top_row, label, &mut claimed_rows);

            let bottom_row = (mid_row + scaled * half_height).round() as i64;
            if bottom_row != top_row {
                draw_label(buf, area, bottom_row, label, &mut claimed_rows);
            }
        }
    }
}

fn draw_label(buf: &mut Buffer, area: Rect, row: i64, label: &str, claimed_rows: &mut [bool]) {
    if row < 0 || row >= area.height as i64 || claimed_rows[row as usize] {
        return;
    }
    claimed_rows[row as usize] = true;
    let y = area.y + row as u16;
    for (i, ch) in label.chars().enumerate() {
        if i as u16 >= area.width {
            break;
        }
        buf[(area.x + i as u16, y)]
            .set_char(ch)
            .set_style(Style::default().fg(theme::DB_SCALE).bg(theme::BASE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn zero_db_lands_at_the_top_edge_for_absolute_scale() {
        let widget = DbScaleWidget {
            amplitude_scale: 1.0,
            reference_amplitude: 1.0,
        };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 20);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        // 0dB => amplitude 1.0 => top_row = round(mid - 1.0*half) = 0
        assert_eq!(buf[(0, 0)].symbol(), "0");
    }

    #[test]
    fn relative_scale_anchors_zero_db_to_peak_not_full_scale() {
        // A quiet file (peak 0.5) with auto vertical zoom on: 0dB should land wherever the
        // peak amplitude (0.5) maps to, not where amplitude 1.0 would.
        let widget = DbScaleWidget {
            amplitude_scale: 1.0,
            reference_amplitude: 0.5,
        };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 20);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        // mid=10, half=10, amplitude=0.5*1.0=0.5 => top_row = round(10 - 0.5*10) = 5
        assert_eq!(buf[(0, 5)].symbol(), "0");
    }

    /// When amplitude_scale > 1 (zoomed in vertically), marks above the visible amplitude
    /// ceiling must NOT be pinned to the top row — they should disappear entirely. The old
    /// code clamped `scaled` to [0,1], which caused 0dB, -3dB, and -6dB to all pile up at
    /// row 0 at 2x zoom rather than going off-screen. Removing the clamp fixes this: those
    /// marks produce negative row indices and are rejected by draw_label's bounds check.
    #[test]
    fn at_2x_zoom_off_screen_marks_are_excluded_and_minus_6_leads() {
        let widget = DbScaleWidget {
            amplitude_scale: 2.0,
            reference_amplitude: 1.0,
        };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 20);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        // 0dB (scaled = 2.0): row = round(10 - 20.0) = -10 → off-screen, must not appear
        // -3dB (scaled ≈ 1.42): also off-screen
        // -6dB (scaled ≈ 1.002): row ≈ round(10 - 10.02) = 0 → topmost visible mark
        assert_eq!(buf[(0, 0)].symbol(), "-", "row 0 should start with '-6', not '0dB'");
        assert_eq!(buf[(1, 0)].symbol(), "6");
    }

    #[test]
    fn colliding_marks_keep_the_first_one_drawn() {
        // A short pane where adjacent marks collide to the same row: the more important
        // (earlier-listed) mark must win, not get silently overwritten by a later one.
        let widget = DbScaleWidget {
            amplitude_scale: 1.0,
            reference_amplitude: 1.0,
        };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 18);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        assert_eq!(buf[(0, 8)].symbol(), "-");
        assert_eq!(buf[(1, 8)].symbol(), "1");
        assert_eq!(buf[(2, 8)].symbol(), "8");
    }
}
