use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::keymap::Action;
use super::theme;

/// Toolbar buttons share the exact same `Action` as menu entries and keyboard shortcuts
/// (see `MenuBar`) so there is one dispatch path, not three that can drift apart. Each
/// button shows its keyboard shortcut inline so every bound command is visible without
/// opening a menu.
pub struct Toolbar {
    buttons: Vec<(&'static str, &'static str, Action)>,
    rects: Vec<Rect>,
}

impl Toolbar {
    pub fn new() -> Self {
        let buttons = vec![
            ("Play/Pause", "Space", Action::TogglePlayback),
            ("Stop", "Esc", Action::Stop),
            ("Cut", "^X", Action::Cut),
            ("Copy", "^C", Action::Copy),
            ("Paste", "^V", Action::Paste),
            ("Undo", "^Z", Action::Undo),
            ("Redo", "^Y", Action::Redo),
            ("Zoom+", "Up", Action::ZoomIn),
            ("Zoom-", "Dn", Action::ZoomOut),
            ("VZoom+", "S+Up", Action::ZoomInVertical),
            ("VZoom-", "S+Dn", Action::ZoomOutVertical),
            ("AutoVZ", "A", Action::ToggleAutoVerticalZoom),
            ("Save", "^S", Action::Save),
            ("Quit", "Q", Action::Quit),
        ];
        Self {
            buttons,
            rects: Vec::new(),
        }
    }

    /// Packs buttons left-to-right, wrapping to the next row when a button wouldn't fit,
    /// and renders each row as its own `Line` — rather than relying on `Paragraph`'s word
    /// wrap — so the rects used for mouse hit-testing can be computed by the exact same
    /// logic that produced what's on screen, with no risk of the two drifting apart.
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.rects.clear();
        let mut lines = Vec::new();
        let mut spans = Vec::new();
        let mut x = area.x;
        let mut row = 0u16;

        let chrome = Style::default().fg(theme::CHROME_FG);
        let shortcut_style = Style::default().fg(theme::SHORTCUT);

        for (label, shortcut, _) in &self.buttons {
            let width = (label.len() + shortcut.len() + 3) as u16; // "[" label ":" shortcut "]"
            if x + width > area.x + area.width && x > area.x {
                lines.push(Line::from(std::mem::take(&mut spans)));
                row += 1;
                x = area.x;
            }
            if row >= area.height {
                break;
            }
            self.rects.push(Rect {
                x,
                y: area.y + row,
                width,
                height: 1,
            });
            spans.push(Span::styled("[", chrome));
            spans.push(Span::styled(*label, chrome));
            spans.push(Span::styled(":", chrome));
            spans.push(Span::styled(*shortcut, shortcut_style));
            spans.push(Span::styled("] ", chrome));
            x += width + 1;
        }
        lines.push(Line::from(spans));

        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme::CHROME_BG)),
            area,
        );
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<Action> {
        self.rects
            .iter()
            .position(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
            .map(|i| self.buttons[i].2)
    }
}
