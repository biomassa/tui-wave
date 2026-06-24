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

/// Spacing constants, shared by layout (`build`) and measurement (`section_width`) so the
/// two can never disagree about how wide anything is.
const GAP: u16 = 1; // trailing space after each button
const SECTION_GAP: u16 = 2; // blank columns between sections (no divider line)

impl Toolbar {
    pub fn new() -> Self {
        let groups = vec![
            // Play has no section label — play/pause is the whole "transport".
            ToolGroup {
                label: "",
                buttons: vec![("Play", "Spc", Action::TogglePlayback)],
            },
            ToolGroup {
                label: "FILE",
                buttons: vec![
                    ("Save", "^s", Action::Save),
                    ("Quit", "q", Action::Quit),
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

    /// On-screen width of one whole section's content — its accent label block (` LABEL `)
    /// plus its buttons. Does not include the inter-section gap (handled by the caller).
    fn section_width(&self, group: &ToolGroup) -> u16 {
        let mut w = 0;
        if !group.label.is_empty() {
            w += group.label.chars().count() as u16 + 2; // " LABEL " accent block
        }
        for &(label, shortcut, action) in &group.buttons {
            let label = self.button_label(label, action);
            w += label.chars().count() as u16 + 1 + shortcut.chars().count() as u16 + GAP;
        }
        w
    }

    /// Number of rows the toolbar needs to show every button at `width`, with no truncation.
    /// `App` uses this to size the toolbar's chrome row so it grows only as far as needed.
    pub fn rows_needed(&self, width: u16) -> u16 {
        let (_, _, rows) = self.build(Rect { x: 0, y: 0, width, height: u16::MAX });
        rows.max(1)
    }

    /// Packs whole sections left-to-right, wrapping to a new row only at section boundaries.
    /// Section labels are drawn as a dim accent block (no divider lines); sections are
    /// separated by blank columns. Returns the rendered lines, per-button clickable rects,
    /// and rows used. Pure given `self` — drives both render and measurement.
    fn build(&self, area: Rect) -> (Vec<Line<'static>>, Vec<(Rect, Action)>, u16) {
        let right = area.x + area.width;
        let group_style = Style::default().fg(theme::TOOLBAR_GROUP).bg(theme::TOOLBAR_GROUP_BG);
        let chrome = Style::default().fg(theme::CHROME_FG);
        let shortcut_style = Style::default().fg(theme::SHORTCUT);

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut rects: Vec<(Rect, Action)> = Vec::new();
        let mut x = area.x;
        let mut row: u16 = 0;
        let mut placed_any = false;

        for group in &self.groups {
            let gap = if x > area.x { SECTION_GAP } else { 0 };
            // Wrap to a fresh row when this whole section won't fit on the current one.
            if x > area.x && x + gap + self.section_width(group) > right {
                lines.push(Line::from(std::mem::take(&mut spans)));
                row += 1;
                x = area.x;
                if row >= area.height {
                    break;
                }
            }
            // Blank-column gap between sections (no divider line); none at a row start.
            if x > area.x {
                spans.push(Span::styled(" ".repeat(SECTION_GAP as usize), chrome));
                x += SECTION_GAP;
            }
            if !group.label.is_empty() {
                spans.push(Span::styled(format!(" {} ", group.label), group_style));
                x += group.label.chars().count() as u16 + 2;
            }
            placed_any = true;

            for &(label, shortcut, action) in &group.buttons {
                let label = self.button_label(label, action);
                let btn_w = label.chars().count() as u16 + 1 + shortcut.chars().count() as u16;
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
