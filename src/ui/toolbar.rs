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
    /// Shown in place of `waveform` when the active buffer is streamed. Almost everything in the
    /// ordinary waveform set is refused on such a buffer, so listing it would be a panel of
    /// commands that only produce refusals; these three are what actually works.
    ///
    /// The only set whose buttons carry no shortcut, deliberately. Elsewhere a shortcut-less
    /// button is redundant with the menus and was removed for that reason — but here, with
    /// everything else refused, clicking these *is* the discoverable route, and two of them have
    /// no key to show because they never needed one.
    streamed: Vec<ToolGroup>,
    /// Per-button clickable rects with the action each triggers, recomputed every render.
    rects: Vec<(Rect, Action)>,
    pub active_actions: HashSet<Action>,
    pub is_playing: bool,
    /// Next Rising Edge's current transient threshold, shown live in place of a static
    /// "Thresh+"/"Thresh-" label pair (see `button_label`).
    pub transient_threshold_db: f32,
    /// Whether the active buffer is streamed, so `groups_for` can swap in [`Self::streamed`].
    /// Repopulated every frame by `App::render`, like `active_actions` and `is_playing`.
    pub streamed_buffer: bool,
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
                    ("AddHT",    sc(Action::InsertHeadTailMark,         "h"), Action::InsertHeadTailMark),
                    ("DelHT",    sc(Action::DeleteHeadTailMark,         "H"), Action::DeleteHeadTailMark),
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
        // Named in the toolbar's own camelCase style rather than the menus' — these read as
        // commands on a hint panel, not as menu entries, so no trailing ellipsis either. No
        // shortcut column on the three file commands: Save As has one but the other two have
        // none, and a lone shortcut beside two blanks reads worse than none at all.
        //
        // Play keeps its own, in the same unlabelled transport group the ordinary set opens
        // with, because it is the one command here whose key a user already knows and would
        // otherwise assume a read-only buffer had taken away.
        let streamed = vec![
            ToolGroup {
                label: "",
                buttons: vec![("Play", sc(Action::TogglePlayback, "Spc"), Action::TogglePlayback)],
            },
            ToolGroup {
                label: "STREAMED (read-only)",
                buttons: vec![
                    ("saveAs", String::new(), Action::SaveAs),
                    ("removeEmptyChannels", String::new(), Action::RemoveEmptyChannels),
                    ("exportChannels", String::new(), Action::ExportChannels),
                ],
            },
        ];

        Self {
            waveform,
            files,
            buffers,
            streamed,
            rects: Vec::new(),
            active_actions: HashSet::new(),
            is_playing: false,
            transient_threshold_db: 13.0,
            streamed_buffer: false,
        }
    }

    fn groups_for(&self, focus: Focus) -> &[ToolGroup] {
        match focus {
            // Only the waveform set is swapped: with the Files or Buffers panel focused the
            // commands on offer are about files and buffers, not about the streamed document, and
            // those all work regardless.
            Focus::Waveform if self.streamed_buffer => &self.streamed,
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
            } else if self.streamed_buffer && shortcut.is_empty() {
                // Everywhere else the peach belongs to the *shortcut*, and the label stays in
                // chrome text. These buttons have no shortcut to carry it, so the label takes it
                // instead — otherwise the only commands that work on a streamed buffer would be
                // the only ones rendered as inert text.
                Style::default().fg(theme::SHORTCUT)
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
    /// Rows to reserve for the toolbar in **every** state — the tallest command set there is — so
    /// the chrome height, and the whole layout below it, never moves.
    ///
    /// Measured over the sets directly rather than through `groups_for`, because that one
    /// substitutes the short `streamed` set for the waveform one: reserving from it made the panel
    /// shrink whenever a streamed buffer was active and jump on the frame the substitution took
    /// effect (user report). The ordinary waveform set is the tallest of them all, so reserving it
    /// unconditionally is both the fix and the simplest statement of the rule.
    pub fn reserved_rows(&self, width: u16) -> u16 {
        [&self.waveform, &self.files, &self.buffers, &self.streamed]
            .into_iter()
            .map(|groups| self.rows_for(width, groups))
            .max()
            .unwrap_or(1)
    }

    /// Rows `groups` needs at `width`.
    fn rows_for(&self, width: u16, groups: &[ToolGroup]) -> u16 {
        let (_, _, rows) =
            self.build(groups, Rect { x: 0, y: 0, width, height: u16::MAX });
        rows.max(1)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every toolbar button must have a shortcut to show — **except** in the streamed set.
    ///
    /// The toolbar exists so that every *bound* command is visible at a glance, so a button with an
    /// empty shortcut renders as a bare label with nothing to press — it reads as broken (user
    /// report, of `rmEmptyCh`), and such commands belong in the menus only.
    ///
    /// The streamed set is the deliberate exception. Nearly every ordinary command is refused on a
    /// streamed buffer, so these three are the only ones that do anything, and two of them have no
    /// key because they never needed one. There, clicking is the discoverable route rather than a
    /// redundant second one — which is the opposite of the situation the rule exists for.
    #[test]
    fn every_toolbar_button_shows_a_shortcut() {
        let bar = Toolbar::new(&HashMap::new());
        let mut bare: Vec<String> = Vec::new();
        for (set, groups) in [
            ("waveform", &bar.waveform),
            ("files", &bar.files),
            ("buffers", &bar.buffers),
        ] {
            for group in groups.iter() {
                for (label, shortcut, action) in &group.buttons {
                    if shortcut.trim().is_empty() {
                        bare.push(format!("  {set}/{}: {label} ({action:?})", group.label));
                    }
                }
            }
        }
        assert!(bare.is_empty(), "toolbar buttons with no shortcut to show:\n{}", bare.join("\n"));
    }

    /// The streamed set offers exactly the commands a streamed buffer permits, and it replaces the
    /// waveform set only when one is active — with the Files or Buffers panel focused those sets
    /// still apply, since their commands work regardless.
    #[test]
    fn the_streamed_set_replaces_only_the_waveform_set() {
        let mut bar = Toolbar::new(&HashMap::new());

        let actions = |b: &Toolbar, f: Focus| -> Vec<Action> {
            b.groups_for(f).iter().flat_map(|g| g.buttons.iter().map(|(_, _, a)| *a)).collect()
        };

        assert_eq!(actions(&bar, Focus::Waveform).len() > 10, true, "the ordinary set is long");
        bar.streamed_buffer = true;
        assert_eq!(
            actions(&bar, Focus::Waveform),
            vec![
                Action::TogglePlayback,
                Action::SaveAs,
                Action::RemoveEmptyChannels,
                Action::ExportChannels
            ],
            "a streamed buffer shows only what works on it — including playback, which streams \
             from disk rather than needing a resident copy"
        );
        let labels: Vec<&str> =
            bar.streamed.iter().flat_map(|g| g.buttons.iter().map(|(l, _, _)| *l)).collect();
        assert_eq!(
            labels,
            vec!["Play", "saveAs", "removeEmptyChannels", "exportChannels"],
            "named in the toolbar's own style, without the menus' ellipsis"
        );
        // Play is the one command here a user already has a key for, so it shows it; the other
        // three are click-only, which is what `every_toolbar_button_shows_a_shortcut` exempts.
        assert_eq!(bar.streamed[0].buttons[0].1, "Spc");
        assert_eq!(
            actions(&bar, Focus::Files),
            actions(&Toolbar::new(&HashMap::new()), Focus::Files),
            "the Files set is unchanged"
        );
        assert_eq!(
            actions(&bar, Focus::Buffers),
            actions(&Toolbar::new(&HashMap::new()), Focus::Buffers),
            "and so is the Buffers set"
        );
    }

    /// The reserved height must not depend on focus *or* on whether a streamed buffer is active.
    ///
    /// The chrome sits above everything else, so any change to its height moves the whole layout.
    /// Reserving from `groups_for` made it shrink in streamed mode — that set is one row where the
    /// waveform set is four — and the panel visibly jumped (user report, while scrolling).
    #[test]
    fn the_reserved_height_is_the_same_in_every_state() {
        for width in [80u16, 100, 150, 190, 240, 400] {
            let mut bar = Toolbar::new(&HashMap::new());
            let ordinary = bar.reserved_rows(width);
            bar.streamed_buffer = true;
            assert_eq!(
                bar.reserved_rows(width),
                ordinary,
                "width {width}: a streamed buffer changed the reserved height"
            );
            // And it is the tallest set's height, so nothing is ever clipped.
            let tallest = [&bar.waveform, &bar.files, &bar.buffers, &bar.streamed]
                .into_iter()
                .map(|g| bar.rows_for(width, g))
                .max()
                .unwrap();
            assert_eq!(ordinary, tallest, "width {width}: must reserve the tallest set");
            assert_eq!(
                ordinary,
                bar.rows_for(width, &bar.waveform),
                "width {width}: the ordinary waveform set is expected to be the tallest"
            );
        }
    }
}
