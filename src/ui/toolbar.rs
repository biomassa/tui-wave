use std::collections::{HashMap, HashSet};

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::app::Focus;
use super::keymap::Action;
use super::theme;

/// A labelled group of related toolbar buttons. Each button is `(label, shortcut, action)`.
struct ToolGroup {
    label: &'static str,
    buttons: Vec<(&'static str, String, Action)>,
}

/// Toolbar buttons share the exact same `Action` as menu entries and keyboard shortcuts
/// (see `MenuBar`) so there is one dispatch path, not three that can drift apart. Buttons
/// are organised into labelled sections (TRANSPORT, EDIT, …) divided by subtle bars; each
/// button shows its keyboard shortcut inline so every bound command is visible at a glance.
/// The toolbar's height is adaptive (see `rows_needed`): it stays compact on a wide terminal
/// and grows only as far as needed so no button is ever dropped.
pub struct Toolbar {
    /// One command set per focus (see `Focus`): the panel is modal and shows only the set
    /// relevant to the focused panel.
    waveform: Vec<ToolGroup>,
    files: Vec<ToolGroup>,
    buffers: Vec<ToolGroup>,
    /// Per-button clickable rects with the action each triggers, recomputed every render.
    rects: Vec<(Rect, Action)>,
    pub active_actions: HashSet<Action>,
    pub is_playing: bool,
    /// Next Rising Edge's current transient threshold, shown live in place of a static
    /// "Thresh+"/"Thresh-" label pair (see `button_label`).
    pub transient_threshold_db: f32,
}

/// Spacing constants, shared by layout (`build`) and measurement (`section_width`) so the
/// two can never disagree about how wide anything is.
const GAP: u16 = 1; // trailing space after each button
const SECTION_GAP: u16 = 1; // extra blank columns between sections (on top of a button's trailing space)

impl Toolbar {
    pub fn new(waveform_shortcuts: &HashMap<Action, String>) -> Self {
        // Shortcut helper: look up from the config-derived map or fall back to the current default.
        let sc = |action: Action, default: &str| -> String {
            waveform_shortcuts.get(&action).cloned().unwrap_or_else(|| default.to_string())
        };
        // Waveform-focus set: Play prefix + labelled sections.
        let waveform = vec![
            // Play has no section label — play/pause is the whole "transport".
            ToolGroup {
                label: "",
                buttons: vec![("Play", sc(Action::TogglePlayback, "Spc"), Action::TogglePlayback)],
            },
            ToolGroup {
                label: "FILE",
                buttons: vec![
                    ("Save",         sc(Action::Save,          "^s"),    Action::Save),
                    ("Quit",         sc(Action::Quit,          "q"),     Action::Quit),
                    ("regToFolder",  sc(Action::ExportRegions, "S+E"),   Action::ExportRegions),
                    ("newFromLeft",  sc(Action::NewFromLeft,   "L"),     Action::NewFromLeft),
                    ("newFromRight", sc(Action::NewFromRight,  "R"),     Action::NewFromRight),
                ],
            },
            ToolGroup {
                label: "EDIT",
                buttons: vec![
                    ("Cut",       sc(Action::Cut,            "^x"), Action::Cut),
                    ("Copy",      sc(Action::Copy,           "^c"), Action::Copy),
                    ("copyToNew", sc(Action::CopyToNew,      "C"),  Action::CopyToNew),
                    ("Paste",     sc(Action::Paste,          "^v"), Action::Paste),
                    ("Undo",      sc(Action::Undo,           "^z"), Action::Undo),
                    ("Redo",      sc(Action::Redo,           "^y"), Action::Redo),
                    ("Deselect",  sc(Action::ClearSelection, "^d"), Action::ClearSelection),
                ],
            },
            ToolGroup {
                label: "VIEW",
                buttons: vec![
                    ("Zoom+",   sc(Action::ZoomIn,             "Up"),  Action::ZoomIn),
                    ("Zoom-",   sc(Action::ZoomOut,            "Dn"),  Action::ZoomOut),
                    ("VZoom+",  sc(Action::ZoomInVertical,     "S+Up"), Action::ZoomInVertical),
                    ("VZoom-",  sc(Action::ZoomOutVertical,    "S+Dn"), Action::ZoomOutVertical),
                    ("AutoVZoom", sc(Action::ToggleAutoVerticalZoom, "a"), Action::ToggleAutoVerticalZoom),
                ],
            },
            ToolGroup {
                label: "PROCESS",
                buttons: vec![
                    ("Rev",       sc(Action::Reverse,        "^r"),  Action::Reverse),
                    ("Norm",      sc(Action::Normalize,      "^n"),   Action::Normalize),
                    ("Gain",      sc(Action::Gain,           "^g"),   Action::Gain),
                    ("FadeIn",    sc(Action::FadeIn,         "^f"),   Action::FadeIn),
                    ("FadeOut",   sc(Action::FadeOut,        "^o"),   Action::FadeOut),
                    ("Trim",      sc(Action::Trim,           "^t"),   Action::Trim),
                    ("Resamp",    sc(Action::Resample,       "^e"),   Action::Resample),
                    ("bothFades", sc(Action::TechnicalFades, "^b"),   Action::TechnicalFades),
                    ("mixToMono", sc(Action::MixToMono,     "^m"),   Action::MixToMono),
                ],
            },
            ToolGroup {
                label: "MARK",
                buttons: vec![
                    ("Add",      sc(Action::InsertMarker,               "m"), Action::InsertMarker),
                    ("Del",      sc(Action::DeleteMarker,               "M"), Action::DeleteMarker),
                    ("Prev",     sc(Action::JumpPrevMarker,             "["), Action::JumpPrevMarker),
                    ("Next",     sc(Action::JumpNextMarker,             "]"), Action::JumpNextMarker),
                    ("ExtPrev",  sc(Action::ExtendSelectionToPrevMarker,"{"), Action::ExtendSelectionToPrevMarker),
                    ("ExtNext",  sc(Action::ExtendSelectionToNextMarker,"}"), Action::ExtendSelectionToNextMarker),
                    ("NextEdge", sc(Action::NextRisingEdge,             "/"), Action::NextRisingEdge),
                    ("PrevEdge", sc(Action::PrevRisingEdge,             "?"), Action::PrevRisingEdge),
                    ("AutoMark", sc(Action::AutoInsertMarkers,          "t"), Action::AutoInsertMarkers),
                    // Labels are overridden dynamically in `button_label` (the live dB
                    // value, then the bare +/- shortcuts) — these are just placeholders.
                    ("", sc(Action::IncreaseTransientThreshold, "+"), Action::IncreaseTransientThreshold),
                    ("", sc(Action::DecreaseTransientThreshold, "-"), Action::DecreaseTransientThreshold),
                ],
            },
            ToolGroup {
                label: "OPTS",
                buttons: vec![
                    ("zeroXSnap",      sc(Action::ToggleZeroSnap,              "z"), Action::ToggleZeroSnap),
                    ("Loop",           sc(Action::ToggleLoop,                  "l"), Action::ToggleLoop),
                    ("fineNavi",       sc(Action::ToggleFineMode,              "`"), Action::ToggleFineMode),
                    ("insPointFollows",sc(Action::ToggleCursorFollowsPlayback, "i"), Action::ToggleCursorFollowsPlayback),
                    ("viewFollows",    sc(Action::ToggleViewportFollowsPlayback,"f"), Action::ToggleViewportFollowsPlayback),
                    ("graphics",       sc(Action::ToggleGraphicsMode,          "g"), Action::ToggleGraphicsMode),
                ],
            },
        ];
        // Files-focus set: a flat, unlabelled list of file-browser commands.
        // These use contextual shortcuts (e.g. ^o = OpenDirectory here, FadeOut elsewhere)
        // so they stay as literal strings, not looked up from the global keybinding map.
        let files = vec![ToolGroup {
            label: "",
            buttons: vec![
                ("Open",     "Enter".to_string(),  Action::OpenSelected),
                ("OpenDir",  "^o".to_string(),     Action::OpenDirectory),
                ("Select",   "Up/Dn".to_string(),  Action::Noop),
                ("Page",     "PgUp/Dn".to_string(),Action::Noop),
                ("Audition", "a".to_string(),      Action::ToggleAudition),
                ("Rename",   "^r".to_string(),     Action::RenameFile),
                ("Delete",   "Del".to_string(),    Action::DeleteFile),
                ("Search",   "/".to_string(),      Action::SearchFiles),
                ("Focus",    "Tab".to_string(),    Action::FocusNext),
                ("Quit",     "q".to_string(),      Action::Quit),
            ],
        }];
        // Buffers-focus set. Up/Dn both selects and loads the buffer immediately — no
        // separate "Switch" command, since there's nothing left for Enter to commit.
        // ^s/^w/^r/^a/^l are contextual (differ from their waveform meanings) — kept literal.
        let buffers = vec![ToolGroup {
            label: "",
            buttons: vec![
                ("Switch",  "Up/Dn".to_string(), Action::Noop),
                ("Search",  "/".to_string(),     Action::SearchBuffers),
                ("Save",    "^s".to_string(),    Action::Save),
                ("Close",   "^w".to_string(),    Action::CloseBuffer),
                ("Rename",  "^r".to_string(),    Action::RenameBuffer),
                ("SaveAll", "^a".to_string(),    Action::SaveAll),
                ("Reload",  "^l".to_string(),    Action::ReloadBuffer),
            ],
        }];
        Self {
            waveform,
            files,
            buffers,
            rects: Vec::new(),
            active_actions: HashSet::new(),
            is_playing: false,
            transient_threshold_db: 13.0,
        }
    }

    fn groups_for(&self, focus: Focus) -> &[ToolGroup] {
        match focus {
            Focus::Waveform => &self.waveform,
            Focus::Files => &self.files,
            Focus::Buffers => &self.buffers,
        }
    }

    fn button_label(&self, label: &str, action: Action) -> String {
        if action == Action::TogglePlayback && self.is_playing {
            "Stop".to_string()
        } else if action == Action::IncreaseTransientThreshold {
            format!("Thresh {:.0}dB", self.transient_threshold_db)
        } else {
            label.to_string()
        }
    }

    /// On-screen width of one whole section's content — its accent label block (`LABEL `)
    /// plus its buttons. No leading pad: the section starts flush at its column.
    fn section_width(&self, group: &ToolGroup) -> u16 {
        let mut w = 0;
        if !group.label.is_empty() {
            w += group.label.chars().count() as u16 + 2; // "LABEL: "
        }
        for (label, shortcut, action) in &group.buttons {
            let label = self.button_label(label, *action);
            w += label.chars().count() as u16 + 1 + shortcut.chars().count() as u16 + GAP;
        }
        w
    }

    /// Emits one section (accent label block + buttons) starting at column `x` on row `y`,
    /// recording each button's clickable rect. Returns the column just past the section.
    fn emit_section(
        &self,
        group: &ToolGroup,
        mut x: u16,
        y: u16,
        spans: &mut Vec<Span<'static>>,
        rects: &mut Vec<(Rect, Action)>,
    ) -> u16 {
        let group_style = Style::default().fg(theme::TOOLBAR_GROUP);
        let chrome = Style::default().fg(theme::CHROME_FG);
        let shortcut_style = Style::default().fg(theme::SHORTCUT);
        if !group.label.is_empty() {
            spans.push(Span::styled(format!("{}: ", group.label), group_style));
            x += group.label.chars().count() as u16 + 2;
        }
        for (label, shortcut, action) in &group.buttons {
            let label = self.button_label(label, *action);
            let btn_w = label.chars().count() as u16 + 1 + shortcut.chars().count() as u16;
            rects.push((Rect { x, y, width: btn_w, height: 1 }, *action));
            let label_style = if self.active_actions.contains(action) {
                Style::default().fg(theme::ACTIVE)
            } else {
                chrome
            };
            spans.push(Span::styled(label, label_style));
            spans.push(Span::styled(" ", chrome));
            spans.push(Span::styled(shortcut.clone(), shortcut_style));
            spans.push(Span::styled(" ".repeat(GAP as usize), chrome));
            x += btn_w + GAP;
        }
        x
    }

    /// Number of rows the toolbar needs at `width`. `App` uses this to size the chrome row.
    pub fn rows_needed(&self, width: u16, focus: Focus) -> u16 {
        let (_, _, rows) = self.build(self.groups_for(focus), Rect { x: 0, y: 0, width, height: u16::MAX });
        rows.max(1)
    }

    /// Rows to reserve for the toolbar regardless of focus — the tallest of the three command
    /// sets — so the chrome height (and the layout below it) doesn't jump when switching panels.
    pub fn reserved_rows(&self, width: u16) -> u16 {
        [Focus::Waveform, Focus::Files, Focus::Buffers]
            .into_iter()
            .map(|f| self.rows_needed(width, f))
            .max()
            .unwrap_or(1)
    }

    /// Renders the toolbar. The first group (Play) is a row-0 prefix; the remaining sections
    /// pack tightly left-to-right, and every wrapped row restarts at the same column as the
    /// first section (FILE) — so each row's leading section lines up under FILE, while the
    /// inter-section spacing stays tight. Returns lines, per-button rects, and rows used.
    fn build(&self, groups: &[ToolGroup], area: Rect) -> (Vec<Line<'static>>, Vec<(Rect, Action)>, u16) {
        let chrome = Style::default().fg(theme::CHROME_FG);
        let prefix = &groups[0];
        let grid_groups = &groups[1..];

        let prefix_w = self.section_width(prefix);
        let origin = area.x + prefix_w; // FILE's column; wrapped rows restart here
        let right = area.x + area.width;

        let mut rects: Vec<(Rect, Action)> = Vec::new();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut row = 0u16;

        // Row 0: Play at the far left; FILE then begins exactly at `origin`.
        let mut x = self.emit_section(prefix, area.x, area.y, &mut spans, &mut rects);
        let mut first_in_row = true;

        for group in grid_groups {
            let sw = self.section_width(group);
            if !first_in_row && x + SECTION_GAP + sw > right {
                lines.push(Line::from(std::mem::take(&mut spans)));
                row += 1;
                // Indent the new row to FILE's column.
                spans.push(Span::styled(" ".repeat(prefix_w as usize), chrome));
                x = origin;
                first_in_row = true;
            }
            if !first_in_row {
                spans.push(Span::styled(" ".repeat(SECTION_GAP as usize), chrome));
                x += SECTION_GAP;
            }
            x = self.emit_section(group, x, area.y + row, &mut spans, &mut rects);
            first_in_row = false;
        }
        lines.push(Line::from(spans));
        (lines, rects, row + 1)
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, focus: Focus) {
        self.rects.clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (lines, rects, _) = self.build(self.groups_for(focus), area);
        self.rects = rects;
        // Toolbar sits on the main app background (theme::BASE), not the menu's chrome color,
        // so it blends with the spacer row and the editor area below it.
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme::BASE)),
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
