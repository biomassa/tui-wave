use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::model::document::Document;
use crate::ui::theme;
use crate::ui::viewport::Viewport;

pub struct StatusBar<'a> {
    pub document: &'a Document,
    pub viewport: &'a Viewport,
    pub snap_to_zero: bool,
    pub loop_playback: bool,
    pub fine_mode: bool,
    /// Next Rising Edge's transient threshold (`+`/`-`), shown so the current dB value is
    /// always visible — unlike the other toggles here, it's a number, not just on/off.
    pub transient_threshold_db: f32,
    /// Label of the last applied edit (top of the undo stack), shown so the user can
    /// confirm what an operation/undo just did. `None` when nothing has been edited.
    pub last_action: Option<&'a str>,
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let seconds = self.document.cursor as f64 / self.document.sample_rate as f64;
        let selection = match self.document.selection {
            Some(sel) if !sel.is_empty() => format!("{} samples", sel.len()),
            _ => "none".to_string(),
        };
        let rate_khz = self.document.sample_rate as f64 / 1000.0;
        let rate_str = if self.document.sample_rate % 1000 == 0 {
            format!("{:.0}kHz", rate_khz)
        } else {
            format!("{:.1}kHz", rate_khz)
        };
        let bits = self.document.bits_per_sample;
        let snap = if self.snap_to_zero { " Zero x: on " } else { "" };
        let loop_ = if self.loop_playback { " Loop: on " } else { "" };
        let fine = if self.fine_mode { " Fine: on " } else { "" };
        // Head/Tail marks are shown as a *pair* count, because that is the unit every DISTMORE
        // process is specified in ("at least two pairs of time-values") — a raw mark count
        // would leave the user doing the division to find out whether a process will run. The
        // "+1" flags a trailing unpaired Head, which is otherwise invisible in a pair count and
        // is exactly the state a half-finished marking session leaves behind. Hidden entirely
        // when there are none, so files that never use the feature pay no status-bar width.
        let head_tail = if self.document.head_tail_marks.is_empty() {
            String::new()
        } else {
            let pairs = self.document.head_tail_pairs();
            let odd = if self.document.head_tail_marks.len() % 2 == 1 { " +1" } else { "" };
            format!(" H/T: {pairs} pairs{odd} ")
        };
        let last = self.last_action.map(|l| format!(" last: {} ", l)).unwrap_or_default();
        let text = format!(
            " pos: {} ({:.3}s) | {}/{}-bit | zoom: {:.1} spl/col | amp: {:.2}x | sel: {} | edge: {:.0}dB |{}{}{}{}{}",
            self.document.cursor,
            seconds,
            rate_str,
            bits,
            self.viewport.samples_per_column,
            self.viewport.amplitude_scale,
            selection,
            self.transient_threshold_db,
            snap,
            loop_,
            fine,
            head_tail,
            last,
        );
        Paragraph::new(Line::from(text))
            .style(Style::default().fg(theme::STATUS_FG).bg(theme::STATUS_BG))
            .render(area, buf);
    }
}
