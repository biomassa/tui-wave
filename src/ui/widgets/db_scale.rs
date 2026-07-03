use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use crate::model::dsp;
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

/// Beyond the fixed marks above, the axis continues in further steps of `DEEP_DB_STEP` dB
/// (matching the existing -6/-12/-18/-24 pattern, so every generated mark is an even round
/// number) down to `DEEP_DB_FLOOR`. Without this, zooming in enough to push -24dB near the
/// edge left everything below it — most of the pane, since amplitude only approaches zero
/// asymptotically as dB drops — completely blank: the loudest visible sample would show
/// -24dB with no detail at all for anything quieter. Deep marks near `DEEP_DB_FLOOR` are
/// generated even when far below the visible range; they just fail `draw_label`'s
/// bounds/collision check and are silently skipped, so a generous floor costs nothing.
const DEEP_DB_STEP: f32 = 6.0;
const DEEP_DB_FLOOR: f32 = -144.0;

/// Renders the vertical dB axis for one channel's waveform pane. The scale is always
/// absolute dBFS — 0dB means full scale (amplitude 1.0) — and `amplitude_scale` positions
/// the marks. So when the view is zoomed vertically (manually, or by auto vertical zoom
/// fitting a quiet peak) 0dB moves off the top edge and the visible marks reflect the true
/// level of the loudest sample on screen: a −6 dBFS peak shows −6 near the top, not 0dB.
#[derive(Clone, Copy)]
pub struct DbScaleWidget {
    pub amplitude_scale: f32,
    /// The exact dB level of the currently visible peak, when auto vertical zoom is fitting
    /// to it — `None` otherwise. Without auto zoom, 0dB is always on (or near) the axis as a
    /// fixed reference; auto zoom moves that reference to wherever the peak actually is, so
    /// this fills the same role with the true (not rounded-to-the-grid) peak level. Drawn
    /// with top priority, ahead of the fixed/generated grid marks, so it always shows even
    /// when it doesn't land on a multiple of 3 or 6 — e.g. "-17", not just "-18" or "-12".
    pub peak_db: Option<f32>,
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

        // Marks are drawn most- to least-important: the fixed set first, then the generated
        // deep marks continuing downward. `claimed_rows` keeps this priority order — a deep
        // mark that would land on the same row as an already-drawn one is simply skipped.
        let mut draw_mark = |db: f32, label: &str, claimed_rows: &mut [bool]| {
            let amplitude = dsp::db_to_linear(db);
            // No clamp here — off-screen marks (scaled > 1.0) produce rows < 0 or >= height,
            // which draw_label already rejects. Clamping to [0,1] was wrong: at 2x vertical
            // zoom (amplitude_scale=2.0) 0dB, -3dB, and -6dB all clamped to 1.0 and stacked
            // at row 0, hiding the fact that those levels are above the visible amplitude range.
            let scaled = (amplitude * self.amplitude_scale) as f64;

            let top_row = (mid_row - scaled * half_height).round() as i64;
            draw_label(buf, area, top_row, label, claimed_rows);

            let bottom_row = (mid_row + scaled * half_height).round() as i64;
            if bottom_row != top_row {
                draw_label(buf, area, bottom_row, label, claimed_rows);
            }
        };

        // The exact peak marker goes first (highest priority) so it always wins any collision
        // with the grid — it's the more important, more precise value at this zoom level.
        if let Some(peak_db) = self.peak_db {
            let label = (peak_db.round() as i32).to_string();
            draw_mark(peak_db, &label, &mut claimed_rows);
        }

        for &(db, label) in DB_MARKS.iter() {
            draw_mark(db, label, &mut claimed_rows);
        }

        // Integer stepping (not repeated f32 subtraction) so every generated label is an
        // exact whole number ("-30", never "-29.999998" from accumulated float error).
        let mut db = DB_MARKS[DB_MARKS.len() - 1].0 as i32 - DEEP_DB_STEP as i32;
        while db as f32 >= DEEP_DB_FLOOR {
            draw_mark(db as f32, &db.to_string(), &mut claimed_rows);
            db -= DEEP_DB_STEP as i32;
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
        let widget = DbScaleWidget { amplitude_scale: 1.0, peak_db: None };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 20);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        // 0dB => amplitude 1.0 => top_row = round(mid - 1.0*half) = 0
        assert_eq!(buf[(0, 0)].symbol(), "0");
    }

    /// Auto vertical zoom fitting a quiet −6 dBFS peak (scale = 0.95 / 0.5 = 1.9): the scale
    /// stays absolute, so 0dB is pushed off the top and −6 dB sits at the peak. You must never
    /// see "0dB" on a signal whose loudest sample is −6 dBFS.
    #[test]
    fn auto_zoom_to_quiet_peak_pushes_0db_off_top_and_shows_minus_6() {
        let widget = DbScaleWidget { amplitude_scale: 0.95 / 0.5, peak_db: None };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 20);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        // 0dB (scaled = 1.9) → row round(10 - 19) = -9, off-screen. -6dB (scaled ≈ 0.95) →
        // row round(10 - 9.5) ≈ 0, the topmost mark. Row 0 must read "-6", not "0".
        assert_eq!(buf[(0, 0)].symbol(), "-");
        assert_eq!(buf[(1, 0)].symbol(), "6");
        // 0dB appears nowhere on the axis.
        let has_zero_db = (0..20).any(|y| buf[(0, y)].symbol() == "0");
        assert!(!has_zero_db, "0dB must not be shown when the peak is only -6 dBFS");
    }

    /// When amplitude_scale > 1 (zoomed in vertically), marks above the visible amplitude
    /// ceiling must NOT be pinned to the top row — they should disappear entirely. The old
    /// code clamped `scaled` to [0,1], which caused 0dB, -3dB, and -6dB to all pile up at
    /// row 0 at 2x zoom rather than going off-screen. Removing the clamp fixes this: those
    /// marks produce negative row indices and are rejected by draw_label's bounds check.
    #[test]
    fn at_2x_zoom_off_screen_marks_are_excluded_and_minus_6_leads() {
        let widget = DbScaleWidget { amplitude_scale: 2.0, peak_db: None };
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
        let widget = DbScaleWidget { amplitude_scale: 1.0, peak_db: None };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 18);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        assert_eq!(buf[(0, 8)].symbol(), "-");
        assert_eq!(buf[(1, 8)].symbol(), "1");
        assert_eq!(buf[(2, 8)].symbol(), "8");
    }

    /// Reported bug: auto vertical zoom fitting a very quiet peak (here ~-24 dBFS) pushed
    /// amplitude_scale high enough that -24 sat right at the edge with nothing below it —
    /// the fixed 6-entry mark list stopped there, leaving the rest of the pane (most of it,
    /// since amplitude only approaches zero asymptotically) completely blank. The axis must
    /// keep populating detail deeper than -24: at this scale, -30 and -36 should also appear.
    #[test]
    fn deep_zoom_populates_marks_below_minus_24() {
        // peak ~0.063 (-24 dBFS) fit to 0.95 => scale ≈ 15.08, same order of magnitude as
        // the screenshot that reported this bug (a -24dB peak with nothing else visible).
        let widget = DbScaleWidget { amplitude_scale: 0.95 / 0.063, peak_db: None };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 40);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let row_text = |y: u16| -> String {
            (0..DB_GUTTER_WIDTH).map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' ')).collect::<String>().trim().to_string()
        };
        let labels: Vec<String> = (0..40).map(row_text).filter(|s| !s.is_empty()).collect();

        assert!(labels.contains(&"-24".to_string()), "expected -24 near the peak; got {labels:?}");
        assert!(labels.contains(&"-30".to_string()), "deeper marks must populate past -24; got {labels:?}");
        assert!(labels.contains(&"-36".to_string()), "deeper marks must populate past -24; got {labels:?}");
        // Every generated deep label is a whole multiple of 6 (an "even" round number) —
        // no float-accumulation artifacts like "-29" or "-30.0".
        for label in &labels {
            if let Ok(n) = label.parse::<i32>() {
                if n <= -30 {
                    assert_eq!(n % 6, 0, "deep mark {n} is not a multiple of 6: {labels:?}");
                }
            }
        }
    }

    /// Auto vertical zoom should show the *exact* peak level, not just the nearest grid
    /// line — a peak at a non-round dB value (here -17 dBFS, not a multiple of 3 or 6) must
    /// still appear on the axis, with top priority over any grid mark it collides with.
    #[test]
    fn peak_db_shows_the_exact_non_round_peak_level() {
        let peak_amplitude = 10f32.powf(-17.0 / 20.0); // -17 dBFS, not on the 3/6 grid
        let widget = DbScaleWidget {
            amplitude_scale: 0.95 / peak_amplitude,
            peak_db: Some(20.0 * peak_amplitude.log10()),
        };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 20);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        let row_text = |y: u16| -> String {
            (0..DB_GUTTER_WIDTH).map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' ')).collect::<String>().trim().to_string()
        };
        let labels: Vec<String> = (0..20).map(row_text).filter(|s| !s.is_empty()).collect();
        assert!(labels.contains(&"-17".to_string()), "expected the exact peak -17; got {labels:?}");
    }

    /// With auto vertical zoom off (`peak_db: None`), no exact-peak marker is drawn — only
    /// the fixed/generated grid. Guards against a regression where peak_db silently defaults
    /// to showing something even when the caller has no peak to report.
    #[test]
    fn no_peak_db_means_no_extra_marker() {
        let widget = DbScaleWidget { amplitude_scale: 0.95 / 10f32.powf(-17.0 / 20.0), peak_db: None };
        let area = Rect::new(0, 0, DB_GUTTER_WIDTH, 20);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        let row_text = |y: u16| -> String {
            (0..DB_GUTTER_WIDTH).map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' ')).collect::<String>().trim().to_string()
        };
        let labels: Vec<String> = (0..20).map(row_text).filter(|s| !s.is_empty()).collect();
        assert!(!labels.contains(&"-17".to_string()), "no peak marker should appear when peak_db is None; got {labels:?}");
    }
}
