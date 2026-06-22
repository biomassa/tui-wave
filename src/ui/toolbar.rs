use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::keymap::Action;

/// Toolbar buttons share the exact same `Action` as menu entries and keyboard shortcuts
/// (see `MenuBar`) so there is one dispatch path, not three that can drift apart.
pub struct Toolbar {
    buttons: Vec<(&'static str, Action)>,
    rects: Vec<Rect>,
}

impl Toolbar {
    pub fn new() -> Self {
        let buttons = vec![
            ("Play/Pause", Action::TogglePlayback),
            ("Stop", Action::Stop),
            ("Cut", Action::Cut),
            ("Copy", Action::Copy),
            ("Paste", Action::Paste),
            ("Undo", Action::Undo),
            ("Redo", Action::Redo),
            ("Zoom+", Action::ZoomIn),
            ("Zoom-", Action::ZoomOut),
        ];
        Self {
            buttons,
            rects: Vec::new(),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.rects.clear();
        let mut spans = Vec::with_capacity(self.buttons.len() * 2);
        let mut x = area.x;
        for (label, _) in &self.buttons {
            let text = format!("[{label}]");
            let width = text.chars().count() as u16;
            self.rects.push(Rect {
                x,
                y: area.y,
                width,
                height: 1,
            });
            spans.push(Span::raw(text));
            spans.push(Span::raw(" "));
            x += width + 1;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<Action> {
        self.rects
            .iter()
            .position(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
            .map(|i| self.buttons[i].1)
    }
}
