use std::collections::HashMap;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use super::keymap::Action;
use super::theme;

pub struct MenuEntry {
    pub label: &'static str,
    pub shortcut: String,
    pub action: Action,
}

pub struct MenuItem {
    pub label: &'static str,
    pub mnemonic: char,
    pub entries: Vec<MenuEntry>,
}

/// Every menu entry, toolbar button (see `Toolbar`), and keyboard shortcut (see
/// `keymap::map_key`) resolves to the same `Action` and funnels through
/// `App::handle_action` — the single dispatch point that keeps all three input paths from
/// drifting apart.
pub struct MenuBar {
    pub items: Vec<MenuItem>,
    open: Option<usize>,
    selected: usize,
    item_rects: Vec<Rect>,
    entry_rects: Vec<Rect>,
}

impl MenuBar {
    pub fn new(shortcuts: &HashMap<Action, String>) -> Self {
        // Look up a shortcut from the config-derived map, falling back to the current
        // hardcoded default so the menu is never blank even if a binding was omitted.
        let sc = |action: Action, default: &str| -> String {
            shortcuts.get(&action).cloned().unwrap_or_else(|| default.to_string())
        };
        let entry = |label: &'static str, action: Action, default: &str| -> MenuEntry {
            MenuEntry { label, shortcut: sc(action, default), action }
        };
        let items = vec![
            MenuItem {
                label: "File",
                mnemonic: 'F',
                entries: vec![
                    entry("Save",                     Action::Save,        "Ctrl+s"),
                    entry("Save As",                  Action::SaveAs,      "Ctrl+Shift+S"),
                    entry("Save All",                 Action::SaveAll,     "Ctrl+l"),
                    entry("Export Regions to Subfolder", Action::ExportRegions, "Shift+E"),
                    entry("New from Left Channel",    Action::NewFromLeft,  "L"),
                    entry("New from Right Channel",   Action::NewFromRight, "R"),
                    entry("Reset Config to Defaults", Action::ResetConfig, ""),
                    entry("Quit",                     Action::Quit,        "q"),
                ],
            },
            MenuItem {
                label: "Edit",
                mnemonic: 'E',
                entries: vec![
                    entry("Cut",                           Action::Cut,                      "Ctrl+x"),
                    entry("Copy",                          Action::Copy,                     "Ctrl+c"),
                    entry("Copy to New",                   Action::CopyToNew,                "C"),
                    entry("Delete",                        Action::Delete,                   "Del"),
                    entry("Paste",                         Action::Paste,                    "Ctrl+v"),
                    entry("Undo",                          Action::Undo,                     "Ctrl+z"),
                    entry("Redo",                          Action::Redo,                     "Ctrl+y"),
                    entry("Clear Selection",               Action::ClearSelection,           "Ctrl+d"),
                    entry("Select All",                    Action::SelectAll,                "Ctrl+a"),
                    entry("Extend Selection to Start",     Action::ExtendSelectionToStart,   "Shift+Home"),
                    entry("Extend Selection to End",       Action::ExtendSelectionToEnd,     "Shift+End"),
                    entry("Extend Selection Page Back",    Action::ExtendSelectionPageBack,  "Shift+PgUp"),
                    entry("Extend Selection Page Fwd",     Action::ExtendSelectionPageForward, "Shift+PgDn"),
                ],
            },
            MenuItem {
                label: "View",
                mnemonic: 'V',
                entries: vec![
                    entry("Zoom In",                          Action::ZoomIn,                      "Up"),
                    entry("Zoom Out",                         Action::ZoomOut,                     "Down"),
                    entry("Zoom In (Vertical)",               Action::ZoomInVertical,              "Shift+Up"),
                    entry("Zoom Out (Vertical)",              Action::ZoomOutVertical,             "Shift+Down"),
                    entry("Auto Vertical Zoom",               Action::ToggleAutoVerticalZoom,      "a"),
                    entry("Zero-Crossing Snap",               Action::ToggleZeroSnap,              "z"),
                    entry("Fine Step Mode",                   Action::ToggleFineMode,              "`"),
                    entry("Insertion Point Follows Playback", Action::ToggleCursorFollowsPlayback, "i"),
                    entry("Viewport Follows Playback",        Action::ToggleViewportFollowsPlayback, "f"),
                    entry("Graphics Mode",                    Action::ToggleGraphicsMode,          "g"),
                ],
            },
            MenuItem {
                label: "Process",
                mnemonic: 'P',
                entries: vec![
                    entry("CDP Process...",  Action::CdpProcess,    "Ctrl+p"),
                    entry("Reverse",         Action::Reverse,       "Ctrl+r"),
                    entry("Normalize",       Action::Normalize,     "Ctrl+n"),
                    entry("Gain",            Action::Gain,          "Ctrl+g"),
                    entry("Fade In",         Action::FadeIn,        "Ctrl+f"),
                    entry("Fade Out",        Action::FadeOut,       "Ctrl+o"),
                    entry("Trim",            Action::Trim,          "Ctrl+t"),
                    entry("Resample",        Action::Resample,      "Ctrl+e"),
                    entry("Technical Fades", Action::TechnicalFades,"Ctrl+b"),
                    entry("Mix to Mono",     Action::MixToMono,     "Ctrl+m"),
                ],
            },
            MenuItem {
                label: "Markers",
                mnemonic: 'M',
                entries: vec![
                    entry("Insert Marker",                        Action::InsertMarker,               "m"),
                    entry("Delete Marker",                        Action::DeleteMarker,               "M"),
                    entry("Previous Marker",                      Action::JumpPrevMarker,             "["),
                    entry("Next Marker",                          Action::JumpNextMarker,             "]"),
                    entry("Extend Selection to Previous Marker",  Action::ExtendSelectionToPrevMarker,"{"),
                    entry("Extend Selection to Next Marker",      Action::ExtendSelectionToNextMarker,"}"),
                    entry("Next Rising Edge",                     Action::NextRisingEdge,             "/"),
                    entry("Previous Rising Edge",                 Action::PrevRisingEdge,             "?"),
                    entry("Auto-Insert Markers at Transients",    Action::AutoInsertMarkers,          "t"),
                    entry("Increase Transient Threshold",         Action::IncreaseTransientThreshold, "+"),
                    entry("Decrease Transient Threshold",         Action::DecreaseTransientThreshold, "-"),
                ],
            },
            MenuItem {
                label: "Transport",
                mnemonic: 'T',
                entries: vec![
                    entry("Play/Pause",    Action::TogglePlayback, "Space"),
                    entry("Loop Playback", Action::ToggleLoop,     "l"),
                ],
            },
            MenuItem {
                label: "Options",
                mnemonic: 'O',
                entries: vec![
                    entry("Configure CDP Directory...", Action::ConfigureCdpDirectory, ""),
                ],
            },
        ];
        Self {
            items,
            open: None,
            selected: 0,
            item_rects: Vec::new(),
            entry_rects: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open_by_mnemonic(&mut self, ch: char) -> bool {
        if let Some(i) = self
            .items
            .iter()
            .position(|it| it.mnemonic.eq_ignore_ascii_case(&ch))
        {
            self.open = Some(i);
            self.selected = 0;
            true
        } else {
            false
        }
    }

    pub fn open_first(&mut self) {
        self.open = Some(0);
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    /// Used by mouse clicks on the bar itself: clicking an already-open menu closes it.
    pub fn toggle_open(&mut self, index: usize) {
        if self.open == Some(index) {
            self.open = None;
        } else {
            self.open = Some(index);
            self.selected = 0;
        }
    }

    pub fn select_entry(&mut self, index: usize) {
        self.selected = index;
    }

    pub fn move_left(&mut self) {
        if let Some(i) = self.open {
            self.open = Some((i + self.items.len() - 1) % self.items.len());
            self.selected = 0;
        }
    }

    pub fn move_right(&mut self) {
        if let Some(i) = self.open {
            self.open = Some((i + 1) % self.items.len());
            self.selected = 0;
        }
    }

    pub fn move_up(&mut self) {
        if let Some(i) = self.open {
            let len = self.items[i].entries.len().max(1);
            self.selected = (self.selected + len - 1) % len;
        }
    }

    pub fn move_down(&mut self) {
        if let Some(i) = self.open {
            let len = self.items[i].entries.len().max(1);
            self.selected = (self.selected + 1) % len;
        }
    }

    /// Activates the currently-highlighted entry of the open menu and closes it.
    pub fn activate(&mut self) -> Option<Action> {
        let i = self.open?;
        let action = self.items[i].entries.get(self.selected).map(|e| e.action);
        self.close();
        action
    }

    pub fn hit_test_bar(&self, x: u16, y: u16) -> Option<usize> {
        self.item_rects
            .iter()
            .position(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    }

    pub fn hit_test_entry(&self, x: u16, y: u16) -> Option<usize> {
        self.entry_rects
            .iter()
            .position(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.item_rects = layout_bar_items(&self.items, area);

        let spans: Vec<Span> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let style = if self.open == Some(i) {
                    Style::default().fg(theme::HIGHLIGHT_FG).bg(theme::HIGHLIGHT_BG)
                } else {
                    Style::default().fg(theme::CHROME_FG).bg(theme::CHROME_BG)
                };
                Span::styled(format!(" {} ", item.label), style)
            })
            .collect();
        let bar_style = Style::default().fg(theme::CHROME_FG).bg(theme::CHROME_BG);
        frame.render_widget(Paragraph::new(Line::from(spans)).style(bar_style), area);

        if let Some(open_index) = self.open {
            self.render_submenu(frame, open_index);
        }
    }

    fn render_submenu(&mut self, frame: &mut Frame, index: usize) {
        let bar_rect = self.item_rects[index];
        let entries = &self.items[index].entries;
        let inner_width = entries
            .iter()
            .map(|e| e.label.len() + e.shortcut.len() + 4)
            .max()
            .unwrap_or(12) as u16;
        let popup = Rect {
            x: bar_rect.x,
            y: bar_rect.y + 1,
            width: inner_width.max(12),
            height: entries.len() as u16 + 2,
        };

        frame.render_widget(Clear, popup);
        let list_items: Vec<ListItem> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let pad = (popup.width as usize)
                    .saturating_sub(2)
                    .saturating_sub(e.label.len())
                    .saturating_sub(e.shortcut.len());
                let line = if self.selected == i {
                    // Selected: one uniform highlight rather than juggling a third accent
                    // color against it, which would risk a low-contrast clash.
                    let style = Style::default().fg(theme::HIGHLIGHT_FG).bg(theme::HIGHLIGHT_BG);
                    Line::styled(format!("{}{}{}", e.label, " ".repeat(pad), e.shortcut), style)
                } else {
                    Line::from(vec![
                        Span::styled(e.label, Style::default().fg(theme::CHROME_FG)),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(e.shortcut.clone(), Style::default().fg(theme::SHORTCUT)),
                    ])
                };
                ListItem::new(line)
            })
            .collect();
        let list = List::new(list_items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .style(Style::default().bg(theme::CHROME_BG)),
        );
        frame.render_widget(list, popup);

        self.entry_rects = (0..entries.len())
            .map(|i| Rect {
                x: popup.x + 1,
                y: popup.y + 1 + i as u16,
                width: popup.width.saturating_sub(2),
                height: 1,
            })
            .collect();
    }
}

fn layout_bar_items(items: &[MenuItem], area: Rect) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(items.len());
    let mut x = area.x;
    for item in items {
        let width = item.label.chars().count() as u16 + 2;
        rects.push(Rect {
            x,
            y: area.y,
            width,
            height: 1,
        });
        x += width;
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_by_mnemonic_finds_case_insensitively() {
        let mut menu = MenuBar::new(&HashMap::new());
        assert!(menu.open_by_mnemonic('e'));
        assert!(menu.is_open());
    }

    #[test]
    fn move_right_wraps_around() {
        let mut menu = MenuBar::new(&HashMap::new());
        menu.open_first();
        for _ in 0..menu.items.len() {
            menu.move_right();
        }
        // Wrapped all the way around back to the first menu, first entry.
        assert_eq!(menu.activate(), Some(Action::Save));
    }

    #[test]
    fn activate_closes_menu() {
        let mut menu = MenuBar::new(&HashMap::new());
        menu.open_by_mnemonic('E');
        let action = menu.activate();
        assert_eq!(action, Some(Action::Cut));
        assert!(!menu.is_open());
    }
}
