use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use super::theme;

pub struct BufferPanel {
    pub active: usize,
    pub focused: bool,
    rects: Vec<Rect>,
}

impl BufferPanel {
    pub fn new() -> Self {
        Self { active: 0, focused: false, rects: Vec::new() }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, names: &[String], active: usize) {
        self.active = active;
        self.rects.clear();

        let border_style = if self.focused {
            Style::default().fg(theme::ACTIVE)
        } else {
            Style::default().fg(theme::BORDER)
        };
        let block = Block::default()
            .title(" Buffers ")
            .borders(Borders::ALL)
            .border_style(border_style)
            .style(Style::default().bg(theme::BASE));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let display_height = inner.height as usize;
        let scroll = if names.is_empty() { 0 } else {
            active.saturating_sub(display_height.saturating_sub(1) / 2)
                .min(names.len().saturating_sub(display_height))
        };

        let mut items = Vec::new();
        for (i, name) in names.iter().enumerate().skip(scroll).take(display_height) {
            let is_active = i == active;
            let (fg, bg) = if is_active && self.focused {
                (theme::HIGHLIGHT_FG, theme::HIGHLIGHT_BG)
            } else if is_active {
                (theme::CHROME_FG, theme::SURFACE0)
            } else {
                (theme::CHROME_FG, theme::BASE)
            };
            let display = if is_active { format!(">{}", name) } else { format!(" {}", name) };
            items.push(ListItem::new(Line::from(Span::styled(display, Style::default().fg(fg).bg(bg)))));
            self.rects.push(Rect {
                x: inner.x,
                y: inner.y + (i - scroll) as u16,
                width: inner.width,
                height: 1,
            });
        }

        frame.render_widget(List::new(items).style(Style::default().bg(theme::BASE)), inner);
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<usize> {
        self.rects.iter().position(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    }
}
