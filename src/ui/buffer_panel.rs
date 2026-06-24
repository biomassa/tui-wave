use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Block, Borders, Paragraph};
use ratatui::Frame;

use super::theme;

pub struct BufferPanel {
    pub active: usize,
    /// Selection cursor — moved by Up/Dn while focused; `Enter`/`SwitchBuffer` switches the
    /// active buffer to it. Kept synced to `active` while the panel isn't focused.
    pub selected: usize,
    pub focused: bool,
    /// Search filter (same "/" behaviour as the Files panel) and whether it's being typed.
    pub filter: String,
    pub filtering: bool,
    /// (row rect, buffer index) for each visible row, for mouse hit-testing.
    rects: Vec<(Rect, usize)>,
}

impl BufferPanel {
    pub fn new() -> Self {
        Self {
            active: 0,
            selected: 0,
            focused: false,
            filter: String::new(),
            filtering: false,
            rects: Vec::new(),
        }
    }

    /// Whether a buffer name passes the current filter.
    pub fn matches(&self, name: &str) -> bool {
        self.filter.is_empty() || name.to_lowercase().contains(&self.filter.to_lowercase())
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
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Optional filter line at the top (like the Files panel).
        let mut y = inner.y;
        if self.filtering {
            let style = Style::default().fg(theme::PEACH).bg(theme::SURFACE0);
            frame.render_widget(
                Paragraph::new(format!("/{}_", self.filter)).style(style),
                Rect { x: inner.x, y, width: inner.width, height: 1 },
            );
            y += 1;
        }
        let list_height = inner.height.saturating_sub(if self.filtering { 1 } else { 0 }) as usize;
        if list_height == 0 {
            return;
        }

        // Visible buffers = those passing the filter, paired with their real index.
        let visible: Vec<(usize, &String)> = names
            .iter()
            .enumerate()
            .filter(|(_, n)| self.matches(n))
            .collect();
        // Center the row the user is acting on (selection if focused, else active).
        let cursor_row = visible
            .iter()
            .position(|(i, _)| *i == if self.focused { self.selected } else { active })
            .unwrap_or(0);
        let scroll = cursor_row
            .saturating_sub(list_height.saturating_sub(1) / 2)
            .min(visible.len().saturating_sub(list_height));

        let mut items = Vec::new();
        for (i, name) in visible.iter().skip(scroll).take(list_height) {
            let i = *i;
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
                Rect { x: inner.x, y, width: inner.width, height: 1 },
                i,
            ));
            y += 1;
        }

        frame.render_widget(
            List::new(items).style(Style::default().bg(theme::BASE)),
            Rect { x: inner.x, y: inner.y + if self.filtering { 1 } else { 0 }, width: inner.width, height: list_height as u16 },
        );
    }

    /// Buffer index at a screen position, if a row was hit.
    pub fn hit_test(&self, x: u16, y: u16) -> Option<usize> {
        self.rects
            .iter()
            .find(|(r, _)| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
            .map(|(_, idx)| *idx)
    }
}
