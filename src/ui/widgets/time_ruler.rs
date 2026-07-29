//! The optional horizontal time axis (`Config.time_ruler`, View menu → "Time Ruler"),
//! drawn on a single row between the waveform panes and the status bar.
//!
//! The counterpart of the vertical dB gutters (`widgets::db_scale`): both annotate the
//! waveform with an absolute scale, on perpendicular edges, and both derive their positions
//! from the same mapping the waveform itself renders with — here `viewport.scroll_offset` +
//! `samples_per_column`, so a tick always lands on the column holding that instant.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use crate::ui::theme;
use crate::ui::viewport::Viewport;

/// Tick glyph — a plain vertical line, the same stroke the cursor/playhead/marker lines use,
/// so a tick reads as "this column" rather than as a separate kind of ornament (a T-shaped
/// `┬` implied a join with something above it that isn't there).
const TICK_CHAR: char = '│';

/// Minimum columns between two ticks. A `m:ss` label is 4-6 columns wide (up to 9 with
/// sub-second decimals), so this leaves a clear gap between one label and the next tick
/// instead of letting them run together.
const MIN_TICK_SPACING: u16 = 12;

/// The candidate tick intervals, in seconds, smallest first — the "nice" round numbers a
/// time axis is expected to step in (never 3.7s or 40s). The first one wide enough to clear
/// `MIN_TICK_SPACING` at the current zoom wins, so the ruler stays readable from
/// single-sample zoom (milliseconds) out to a whole hour-long file (minutes).
const TICK_STEPS_SECONDS: [f64; 22] = [
    0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0,
    120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
];

/// Renders the horizontal time axis for the currently visible sample range.
pub struct TimeRulerWidget<'a> {
    pub viewport: &'a Viewport,
    pub sample_rate: u32,
}

/// Picks the smallest "nice" interval from [`TICK_STEPS_SECONDS`] that puts at least
/// `MIN_TICK_SPACING` columns between consecutive ticks. Falls back to the largest step
/// when even an hour isn't wide enough (a file so long, or a view so narrow, that no listed
/// step clears the spacing) — some ticks beat none, and the labels simply crowd.
pub fn tick_step_seconds(seconds_per_column: f64) -> f64 {
    let min_step = seconds_per_column * MIN_TICK_SPACING as f64;
    TICK_STEPS_SECONDS
        .iter()
        .copied()
        .find(|&step| step >= min_step)
        .unwrap_or(TICK_STEPS_SECONDS[TICK_STEPS_SECONDS.len() - 1])
}

/// Formats `seconds` as `m:ss`, with as many decimal places as the tick interval needs to
/// keep consecutive labels distinct (a 0.1s step must read "0:01.2", not two identical
/// "0:01"s). Minutes are never zero-padded and roll past 60 rather than wrapping into hours
/// — an audio editor's timeline is a duration, not a clock, and "75:00" is more directly
/// useful than "1:15:00" when the number being read is an offset into a file.
pub fn format_time(seconds: f64, step: f64) -> String {
    let decimals = if step >= 1.0 {
        0
    } else if step >= 0.1 {
        1
    } else if step >= 0.01 {
        2
    } else {
        3
    };
    let seconds = seconds.max(0.0);
    let minutes = (seconds / 60.0).floor();
    let rest = seconds - minutes * 60.0;
    // Width counts the "SS" plus the "." and its decimals, so seconds stay zero-padded to
    // two digits either way ("0:07.50", not "0:7.50").
    let width = if decimals == 0 { 2 } else { 3 + decimals };
    format!("{minutes}:{rest:0width$.decimals$}")
}

impl<'a> Widget for TimeRulerWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let style = Style::default().fg(theme::TIME_RULER).bg(theme::BASE);
        let y = area.y;
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_char(' ').set_style(style);
        }

        let sample_rate = self.sample_rate.max(1) as f64;
        let seconds_per_column = self.viewport.samples_per_column / sample_rate;
        if !seconds_per_column.is_finite() || seconds_per_column <= 0.0 {
            return;
        }
        let step = tick_step_seconds(seconds_per_column);
        let start_seconds = self.viewport.scroll_offset as f64 / sample_rate;

        // Start at the first multiple of `step` at or after the left edge, so ticks sit on
        // round times (0:00, 0:05, …) that stay put as the view scrolls, rather than on
        // offsets measured from wherever the scroll happens to be.
        let mut tick_index = (start_seconds / step).ceil();
        loop {
            let t = tick_index * step;
            let column = ((t - start_seconds) / seconds_per_column).round();
            if !column.is_finite() || column >= area.width as f64 {
                break;
            }
            let x = area.x + column.max(0.0) as u16;
            buf[(x, y)].set_char(TICK_CHAR).set_style(style);

            // The label runs to the right of its own tick and is clipped before the next one,
            // so labels never overprint each other however tight the spacing gets.
            let label = format_time(t, step);
            let next_tick_x = area.x + (column + step / seconds_per_column).min(area.width as f64) as u16;
            let label_start = x + 1;
            let limit = next_tick_x.min(area.x + area.width);
            for (i, ch) in label.chars().enumerate() {
                let lx = label_start + i as u16;
                if lx >= limit {
                    break;
                }
                buf[(lx, y)].set_char(ch).set_style(style);
            }
            tick_index += 1.0;
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
            total_len: 10_000_000,
            auto_vertical_zoom: false,
            channel_scroll: 0,
        }
    }

    fn row_text(buf: &Buffer, area: Rect) -> String {
        (area.x..area.x + area.width)
            .map(|x| buf[(x, area.y)].symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    fn render(scroll_offset: usize, samples_per_column: f64, width: u16) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        let widget = TimeRulerWidget { viewport: &viewport(scroll_offset, samples_per_column), sample_rate: 48_000 };
        widget.render(area, &mut buf);
        row_text(&buf, area)
    }

    #[test]
    fn tick_step_grows_with_zoom_out_and_stays_on_round_numbers() {
        // 48kHz, 480 samples/column => 0.01 s/col => 12 columns is 0.12s => next step up is 0.2.
        assert_eq!(tick_step_seconds(0.01), 0.2);
        // Zoomed way out: 1 s/col => 12s minimum => 15s is the first listed step that clears it.
        assert_eq!(tick_step_seconds(1.0), 15.0);
        // Zoomed way in: 1 sample/column at 48kHz => ~20.8µs/col => 0.001s clears it easily.
        assert_eq!(tick_step_seconds(1.0 / 48_000.0), 0.001);
    }

    /// Every listed step keeps ticks at least `MIN_TICK_SPACING` apart — the property the
    /// whole table exists to guarantee, checked across a wide sweep of zoom levels rather
    /// than at the handful of hand-picked ones above.
    #[test]
    fn chosen_step_always_clears_the_minimum_spacing() {
        let mut seconds_per_column = 1e-6;
        while seconds_per_column < 100.0 {
            let step = tick_step_seconds(seconds_per_column);
            let spacing = step / seconds_per_column;
            assert!(
                spacing >= MIN_TICK_SPACING as f64,
                "at {seconds_per_column} s/col, step {step} gives only {spacing} columns"
            );
            seconds_per_column *= 1.3;
        }
    }

    #[test]
    fn format_time_is_minutes_colon_seconds_with_step_appropriate_decimals() {
        assert_eq!(format_time(0.0, 1.0), "0:00");
        assert_eq!(format_time(83.0, 1.0), "1:23");
        assert_eq!(format_time(7.5, 0.5), "0:07.5");
        assert_eq!(format_time(7.5, 0.05), "0:07.50");
        assert_eq!(format_time(0.125, 0.001), "0:00.125");
    }

    /// Past an hour the ruler keeps counting minutes rather than rolling into an h:mm:ss
    /// clock — the number is an offset into a file, not a time of day.
    #[test]
    fn format_time_counts_minutes_past_sixty() {
        assert_eq!(format_time(4500.0, 60.0), "75:00");
    }

    #[test]
    fn ruler_labels_the_visible_range_from_the_scroll_offset() {
        // 48kHz, 24000 samples/column = 0.5 s/col => step 10s => a tick every 20 columns.
        let text = render(0, 24_000.0, 80);
        assert!(text.starts_with("│0:00"), "expected a 0:00 tick at the left edge; got {text:?}");
        assert!(text.contains("│0:10"), "expected a 0:10 tick; got {text:?}");
        assert!(text.contains("│0:30"), "expected a 0:30 tick; got {text:?}");
    }

    /// Ticks land on round absolute times, not on offsets from wherever the view is scrolled
    /// to — so scrolling slides the ruler under the labels instead of renumbering them.
    #[test]
    fn ticks_sit_on_round_times_when_scrolled_to_an_odd_offset() {
        // Scrolled to 7s (336000 samples at 48kHz); step is 10s, so the first tick is 0:10.
        let text = render(336_000, 24_000.0, 80);
        assert!(text.contains("│0:10"), "expected the first round tick at 0:10; got {text:?}");
        assert!(!text.contains("0:07"), "ticks must not be measured from the scroll offset; got {text:?}");
    }

    /// Zoomed in far enough that whole seconds would repeat, labels must carry decimals.
    #[test]
    fn deep_zoom_labels_include_sub_second_detail() {
        // 48 samples/column = 1ms/col => step 0.02s => a tick every 20 columns.
        let text = render(0, 48.0, 80);
        assert!(text.contains("0:00.02"), "expected sub-second labels at deep zoom; got {text:?}");
        assert!(text.contains("0:00.04"), "expected sub-second labels at deep zoom; got {text:?}");
    }

    /// A label is clipped at the next tick rather than overprinting it — the ruler must stay
    /// readable even when the geometry puts ticks close together.
    #[test]
    fn labels_never_overprint_the_next_tick() {
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        let widget = TimeRulerWidget { viewport: &viewport(0, 24_000.0), sample_rate: 48_000 };
        widget.render(area, &mut buf);
        let text = row_text(&buf, area);
        // Every tick column still holds the tick glyph (nothing wrote over it), and the
        // expected number of them are present: 80 columns at 0.5 s/col is 40s, stepping by
        // 10s => 0:00 / 0:10 / 0:20 / 0:30.
        assert_eq!(text.matches('│').count(), 4, "got {text:?}");
    }

    #[test]
    fn zero_width_area_renders_nothing() {
        let area = Rect::new(0, 0, 0, 1);
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        let widget = TimeRulerWidget { viewport: &viewport(0, 24_000.0), sample_rate: 48_000 };
        widget.render(area, &mut buf); // must not panic
    }
}
