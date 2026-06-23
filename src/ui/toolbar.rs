use std::collections::HashSet;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::keymap::Action;
use super::theme;

/// A labelled group of related toolbar buttons. Each button is `(label, shortcut, action)`.
struct ToolGroup {
    label: &'static str,
    buttons: Vec<(&'static str, &'static str, Action)>,
}

/// Toolbar buttons share the exact same `Action` as menu entries and keyboard shortcuts
/// (see `MenuBar`) so there is one dispatch path, not three that can drift apart. Buttons
/// are organised into labelled sections (TRANSPORT, EDIT, …) divided by subtle bars; each
/// button shows its keyboard shortcut inline so every bound command is visible at a glance.
/// The toolbar's height is adaptive (see `rows_needed`): it stays compact on a wide terminal
/// and grows only as far as needed so no button is ever dropped.
pub struct Toolbar {
    groups: Vec<ToolGroup>,
    /// Per-button clickable rects with the action each triggers, recomputed every render.
    rects: Vec<(Rect, Action)>,
    pub active_actions: HashSet<Action>,
    pub is_playing: bool,
}

/// Spacing constants, shared by layout (`build`) and measurement (`rows_needed`) so the two
/// can never disagree about how wide anything is.
const GAP: u16 = 1; // between buttons
const SEP_W: u16 = 3; // " │ "

impl Toolbar {
    pub fn new() -> Self {
        let groups = vec![
            ToolGroup {
                label: "TRANSPORT",
                buttons: vec![
                    ("Play", "Spc", Action::TogglePlayback),
                    ("Stop", "Esc", Action::Stop),
                ],
            },
            ToolGroup {
                label: "EDIT",
                buttons: vec![
                    ("Cut", "^x", Action::Cut),
                    ("Copy", "^c", Action::Copy),
                    ("New", "C", Action::CopyToNew),
                    ("Paste", "^v", Action::Paste),
                    ("Undo", "^z", Action::Undo),
                    ("Redo", "^y", Action::Redo),
                ],
            },
            ToolGroup {
                label: "PROCESS",
                buttons: vec![
                    ("Rev", "^r", Action::Reverse),
                    ("Norm", "^n", Action::Normalize),
                    ("Gain", "^g", Action::Gain),
                    ("FadeIn", "^f", Action::FadeIn),
                    ("FadeOut", "^o", Action::FadeOut),
                    ("Trim", "^t", Action::Trim),
                    ("Resamp", "^e", Action::Resample),
                ],
            },
            ToolGroup {
                label: "VIEW",
                buttons: vec![
                    ("Zoom+", "Up", Action::ZoomIn),
                    ("Zoom-", "Dn", Action::ZoomOut),
                    ("VZoom+", "S+Up", Action::ZoomInVertical),
                    ("VZoom-", "S+Dn", Action::ZoomOutVertical),
                    ("Auto", "a", Action::ToggleAutoVerticalZoom),
                ],
            },
            ToolGroup {
                label: "MARK",
                buttons: vec![("Add", "m", Action::InsertMarker)],
            },
            ToolGroup {
                label: "OPTS",
                buttons: vec![
                    ("Snap", "z", Action::ToggleZeroSnap),
                    ("Loop", "l", Action::ToggleLoop),
                ],
            },
            ToolGroup {
                label: "FILE",
                buttons: vec![
                    ("Save", "^s", Action::Save),
                    ("Quit", "q", Action::Quit),
                ],
            },
        ];
        Self {
            groups,
            rects: Vec::new(),
            active_actions: HashSet::new(),
            is_playing: false,
        }
    }

    fn button_label(&self, label: &'static str, action: Action) -> &'static str {
        if action == Action::TogglePlayback && self.is_playing {
            "Stop"
        } else {
            label
        }
    }

    /// Number of rows the toolbar needs to show every button at `width`, with no truncation.
    /// `App` uses this to size the toolbar's chrome row so it grows only as far as needed.
    pub fn rows_needed(&self, width: u16) -> u16 {
        let (_, _, rows) = self.build(Rect { x: 0, y: 0, width, height: u16::MAX });
        rows.max(1)
    }

    /// Lays the groups out left-to-right, wrapping when something won't fit on the current
    /// row. Returns the rendered lines, the per-button clickable rects, and the number of
    /// rows used. A group divider is dropped when it would land at the start of a wrapped
    /// row. Anything past `area.height` rows is not emitted (the caller sizes the area so
    /// that doesn't normally happen). Pure given `self` — drives both render and measurement.
    fn build(&self, area: Rect) -> (Vec<Line<'static>>, Vec<(Rect, Action)>, u16) {
        let right = area.x + area.width;
        let group_style = Style::default().fg(theme::TOOLBAR_GROUP);
        let sep_style = Style::default().fg(theme::DIVIDER);
        let chrome = Style::default().fg(theme::CHROME_FG);
        let shortcut_style = Style::default().fg(theme::SHORTCUT);

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut rects: Vec<(Rect, Action)> = Vec::new();
        let mut x = area.x;
        let mut row: u16 = 0;
        let mut placed_any = false;

        'groups: for (gi, group) in self.groups.iter().enumerate() {
            let label_w = group.label.chars().count() as u16 + 1; // label + trailing space
            let sep_w = if gi > 0 { SEP_W } else { 0 };
            if x > area.x && x + sep_w + label_w > right {
                lines.push(Line::from(std::mem::take(&mut spans)));
                row += 1;
                x = area.x;
                if row >= area.height {
                    break;
                }
                // Fresh row: drop the leading divider.
                spans.push(Span::styled(format!("{} ", group.label), group_style));
            } else {
                if gi > 0 {
                    spans.push(Span::styled(" │ ", sep_style));
                    x += sep_w;
                }
                spans.push(Span::styled(format!("{} ", group.label), group_style));
            }
            x += label_w;
            placed_any = true;

            for &(label, shortcut, action) in &group.buttons {
                let label = self.button_label(label, action);
                let btn_w = label.chars().count() as u16 + 1 + shortcut.chars().count() as u16;
                if x > area.x && x + btn_w > right {
                    lines.push(Line::from(std::mem::take(&mut spans)));
                    row += 1;
                    x = area.x;
                    if row >= area.height {
                        break 'groups;
                    }
                }
                rects.push((Rect { x, y: area.y + row, width: btn_w, height: 1 }, action));
                let label_style = if self.active_actions.contains(&action) {
                    Style::default().fg(theme::ACTIVE)
                } else {
                    chrome
                };
                spans.push(Span::styled(label.to_string(), label_style));
                spans.push(Span::styled(" ", chrome));
                spans.push(Span::styled(shortcut.to_string(), shortcut_style));
                spans.push(Span::styled(" ".repeat(GAP as usize), chrome));
                x += btn_w + GAP;
            }
        }
        if !spans.is_empty() {
            lines.push(Line::from(spans));
        }
        let rows = if placed_any { row + 1 } else { 0 };
        (lines, rects, rows)
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.rects.clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (lines, rects, _) = self.build(area);
        self.rects = rects;
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme::CHROME_BG)),
            area,
        );
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<Action> {
        self.rects
            .iter()
            .find(|(r, _)| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
            .map(|(_, action)| *action)
    }
}
