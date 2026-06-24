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
const MIN_CELL_W: u16 = 8; // smallest useful grid column width

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

    /// Packs sections into a fixed grid of `cols` cells (each `cell_w` wide), row-major.
    /// Returns each section's `(row, start_cell)` and the total row count. Because every
    /// section begins on a cell boundary, section starts line up vertically across rows.
    fn pack(&self, cols: u16, cell_w: u16) -> (Vec<(u16, u16)>, u16) {
        let mut placements = Vec::with_capacity(self.groups.len());
        let mut row = 0u16;
        let mut used = 0u16; // cells consumed on the current row
        for group in &self.groups {
            let cells = (self.section_width(group).div_ceil(cell_w)).max(1);
            if used > 0 && used + cells > cols {
                row += 1;
                used = 0;
            }
            placements.push((row, used));
            used += cells;
        }
        (placements, row + 1)
    }

    /// Chooses the column grid for `width`: the column count (with its derived cell width)
    /// that fits every section in the fewest rows, preferring more columns on a tie. Aligning
    /// sections to cell boundaries is what makes their starts line up across wrapped rows.
    fn grid(&self, width: u16) -> (u16, u16) {
        let n = self.groups.len() as u16;
        let max_cols = n.min((width / MIN_CELL_W).max(1));
        let mut best = (u16::MAX, 1u16, width.max(1)); // (rows, cols, cell_w)
        for cols in 1..=max_cols {
            let cell_w = width / cols;
            if cell_w == 0 {
                continue;
            }
            // Skip configs where some section can't fit within one row of `cols` cells.
            if cols > 1
                && self
                    .groups
                    .iter()
                    .any(|g| self.section_width(g).div_ceil(cell_w) > cols)
            {
                continue;
            }
            let (_, rows) = self.pack(cols, cell_w);
            if rows < best.0 || (rows == best.0 && cols > best.1) {
                best = (rows, cols, cell_w);
            }
        }
        (best.1, best.2) // (cols, cell_w)
    }

    /// Number of rows the toolbar needs at `width`. `App` uses this to size the chrome row.
    pub fn rows_needed(&self, width: u16) -> u16 {
        let (cols, cell_w) = self.grid(width);
        self.pack(cols, cell_w).1.max(1)
    }

    /// Renders the sections on the column grid from `grid`/`pack`. Section labels are a dim
    /// accent block (no divider lines); each section starts at its cell's column so starts
    /// align vertically. Returns the lines, per-button clickable rects, and rows used.
    fn build(&self, area: Rect) -> (Vec<Line<'static>>, Vec<(Rect, Action)>, u16) {
        let group_style = Style::default().fg(theme::TOOLBAR_GROUP).bg(theme::TOOLBAR_GROUP_BG);
        let chrome = Style::default().fg(theme::CHROME_FG);
        let shortcut_style = Style::default().fg(theme::SHORTCUT);

        let (cols, cell_w) = self.grid(area.width);
        let (placements, total_rows) = self.pack(cols, cell_w);
        let mut rects: Vec<(Rect, Action)> = Vec::new();
        let mut lines: Vec<Line<'static>> = Vec::new();

        let rows_to_draw = total_rows.min(area.height);
        for r in 0..rows_to_draw {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut x = area.x;
            for (group, &(pr, pc)) in self.groups.iter().zip(&placements) {
                if pr != r {
                    continue;
                }
                // Pad to this section's cell column so starts align across rows.
                let target_x = area.x + pc * cell_w;
                if target_x > x {
                    spans.push(Span::styled(" ".repeat((target_x - x) as usize), chrome));
                    x = target_x;
                }
                if !group.label.is_empty() {
                    spans.push(Span::styled(format!(" {} ", group.label), group_style));
                    x += group.label.chars().count() as u16 + 2;
                }
                for &(label, shortcut, action) in &group.buttons {
                    let label = self.button_label(label, action);
                    let btn_w = label.chars().count() as u16 + 1 + shortcut.chars().count() as u16;
                    rects.push((Rect { x, y: area.y + r, width: btn_w, height: 1 }, action));
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
            lines.push(Line::from(spans));
        }
        (lines, rects, total_rows)
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
