use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

/// Width reserved on each side of a channel's waveform pane for the dB scale gutter.
pub const DB_GUTTER_WIDTH: u16 = 4;

const DB_MARKS: [(f32, &str); 6] = [
    (0.0, "0dB"),
    (-6.0, "-6"),
    (-12.0, "-12"),
    (-18.0, "-18"),
    (-24.0, "-24"),
    (-36.0, "-36"),
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

        let mid_row = area.height as f64 / 2.0;
        let half_height = area.height as f64 / 2.0;

        for &(db, label) in DB_MARKS.iter() {
            let amplitude = self.reference_amplitude * 10f32.powf(db / 20.0);
            let scaled = (amplitude * self.amplitude_scale).clamp(0.0, 1.0) as f64;

            let top_row = (mid_row - scaled * half_height).round() as i64;
            draw_label(buf, area, top_row, label);

            let bottom_row = (mid_row + scaled * half_height).round() as i64;
            if bottom_row != top_row {
                draw_label(buf, area, bottom_row, label);
            }
        }
    }
}

fn draw_label(buf: &mut Buffer, area: Rect, row: i64, label: &str) {
    if row < 0 || row >= area.height as i64 {
        return;
    }
    let y = area.y + row as u16;
    for (i, ch) in label.chars().enumerate() {
        if i as u16 >= area.width {
            break;
        }
        buf[(area.x + i as u16, y)].set_char(ch);
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
}
