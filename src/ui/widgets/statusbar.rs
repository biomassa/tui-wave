use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::model::document::Document;
use crate::ui::viewport::Viewport;

pub struct StatusBar<'a> {
    pub document: &'a Document,
    pub viewport: &'a Viewport,
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let seconds = self.document.playhead as f64 / self.document.sample_rate as f64;
        let text = format!(
            " pos: {} ({:.3}s) | zoom: {:.1} spl/col | amp: {:.2}x ",
            self.document.playhead, seconds, self.viewport.samples_per_column, self.viewport.amplitude_scale
        );
        Paragraph::new(Line::from(text))
            .style(Style::default().fg(Color::Black).bg(Color::Gray))
            .render(area, buf);
    }
}
