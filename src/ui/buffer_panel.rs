use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use super::theme;

pub struct BufferPanel {
    pub active: usize,
    /// Selection cursor — moved by Up/Dn while focused; `Enter`/`SwitchBuffer` switches the
    /// active buffer to it. Kept synced to `active` while the panel isn't focused.
    pub selected: usize,
    pub focused: bool,
    /// (row rect, buffer index) for each visible row, for mouse hit-testing.
    rects: Vec<(Rect, usize)>,
}

impl BufferPanel {
    pub fn new() -> Self {
        Self { active: 0, selected: 0, focused: false, rects: Vec::new() }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, names: &[String], active: usize) {
        self.active = active;
        // While unfocused the cursor tracks the active buffer, so focusing starts there.
        if !self.focused {
            self.selected = active;
        }
        self.selected = self.selected.min(names.len().saturating_sub(1));
        self.rects.clear();

        let border_style = if self.focused {
            Style::default().fg(theme::FOCUS)
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
        // Keep whichever row the user is acting on (selection if focused, else active) in view.
        let cursor = if self.focused { self.selected } else { active };
        let scroll = if names.is_empty() {
            0
        } else {
            cursor
                .saturating_sub(display_height.saturating_sub(1) / 2)
                .min(names.len().saturating_sub(display_height))
        };

        let mut items = Vec::new();
        for (i, name) in names.iter().enumerate().skip(scroll).take(display_height) {
            let is_active = i == active;
            let is_selected = i == self.selected;
            let (fg, bg) = if is_selected && self.focused {
                (theme::HIGHLIGHT_FG, theme::HIGHLIGHT_BG)
            } else if is_active {
                (theme::CHROME_FG, theme::SURFACE0)
            } else {
                (theme::CHROME_FG, theme::BASE)
            };
            // ">" marks the active buffer regardless of where the selection cursor is.
            let display = if is_active { format!(">{}", name) } else { format!(" {}", name) };
            items.push(ListItem::new(Line::from(Span::styled(display, Style::default().fg(fg).bg(bg)))));
            self.rects.push((
                Rect { x: inner.x, y: inner.y + (i - scroll) as u16, width: inner.width, height: 1 },
                i,
            ));
        }

        frame.render_widget(List::new(items).style(Style::default().bg(theme::BASE)), inner);
    }

    /// Moves the selection cursor (while focused), clamped to `count` buffers.
    pub fn move_selection(&mut self, delta: isize, count: usize) {
        if count == 0 {
            return;
        }
        let max = count - 1;
        let next = (self.selected as isize + delta).clamp(0, max as isize) as usize;
        self.selected = next;
    }

    /// Buffer index at a screen position, if a row was hit.
    pub fn hit_test(&self, x: u16, y: u16) -> Option<usize> {
        self.rects
            .iter()
            .find(|(r, _)| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
            .map(|(_, idx)| *idx)
    }
}
