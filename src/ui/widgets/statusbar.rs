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
        let snap = if self.snap_to_zero { " Zero x: on " } else { "" };
        let loop_ = if self.loop_playback { " Loop: on " } else { "" };
        let fine = if self.fine_mode { " Fine: on " } else { "" };
        let last = self.last_action.map(|l| format!(" last: {} ", l)).unwrap_or_default();
        let text = format!(
            " pos: {} ({:.3}s) | zoom: {:.1} spl/col | amp: {:.2}x | sel: {} |{}{}{}{}",
            self.document.cursor,
            seconds,
            self.viewport.samples_per_column,
            self.viewport.amplitude_scale,
            selection,
            snap,
            loop_,
            fine,
            last,
        );
        Paragraph::new(Line::from(text))
            .style(Style::default().fg(theme::STATUS_FG).bg(theme::STATUS_BG))
            .render(area, buf);
    }
}
