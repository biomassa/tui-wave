use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use super::keymap::Action;
use super::theme;

pub struct MenuEntry {
    pub label: &'static str,
    pub shortcut: &'static str,
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
    pub fn new() -> Self {
        let items = vec![
            MenuItem {
                label: "File",
                mnemonic: 'F',
                entries: vec![
                    MenuEntry {
                        label: "Save",
                        shortcut: "Ctrl+s",
                        action: Action::Save,
                    },
                    MenuEntry {
                        label: "Save As",
                        shortcut: "Ctrl+Shift+S",
                        action: Action::SaveAs,
                    },
                    MenuEntry {
                        label: "Save All",
                        shortcut: "Ctrl+l",
                        action: Action::SaveAll,
                    },
                    MenuEntry {
                        label: "Quit",
                        shortcut: "q",
                        action: Action::Quit,
                    },
                ],
            },
            MenuItem {
                label: "Edit",
                mnemonic: 'E',
                entries: vec![
                    MenuEntry {
                        label: "Cut",
                        shortcut: "Ctrl+x",
                        action: Action::Cut,
                    },
                    MenuEntry {
                        label: "Copy",
                        shortcut: "Ctrl+c",
                        action: Action::Copy,
                    },
                    MenuEntry {
                        label: "Copy to New",
                        shortcut: "C",
                        action: Action::CopyToNew,
                    },
                    MenuEntry {
                        label: "Delete",
                        shortcut: "Del",
                        action: Action::Delete,
                    },
                    MenuEntry {
                        label: "Paste",
                        shortcut: "Ctrl+v",
                        action: Action::Paste,
                    },
                    MenuEntry {
                        label: "Undo",
                        shortcut: "Ctrl+z",
                        action: Action::Undo,
                    },
                    MenuEntry {
                        label: "Redo",
                        shortcut: "Ctrl+y",
                        action: Action::Redo,
                    },
                    MenuEntry {
                        label: "Clear Selection",
                        shortcut: "Ctrl+d",
                        action: Action::ClearSelection,
                    },
                ],
            },
            MenuItem {
                label: "View",
                mnemonic: 'V',
                entries: vec![
                    MenuEntry {
                        label: "Zoom In",
                        shortcut: "Up/+",
                        action: Action::ZoomIn,
                    },
                    MenuEntry {
                        label: "Zoom Out",
                        shortcut: "Down/-",
                        action: Action::ZoomOut,
                    },
                    MenuEntry {
                        label: "Zoom In (Vertical)",
                        shortcut: "Shift+Up",
                        action: Action::ZoomInVertical,
                    },
                    MenuEntry {
                        label: "Zoom Out (Vertical)",
                        shortcut: "Shift+Down",
                        action: Action::ZoomOutVertical,
                    },
                    MenuEntry {
                        label: "Auto Vertical Zoom",
                        shortcut: "a",
                        action: Action::ToggleAutoVerticalZoom,
                    },
                    MenuEntry {
                        label: "Zero-Crossing Snap",
                        shortcut: "z",
                        action: Action::ToggleZeroSnap,
                    },
                    MenuEntry {
                        label: "Fine Step Mode",
                        shortcut: "`",
                        action: Action::ToggleFineMode,
                    },
                ],
            },
            MenuItem {
                label: "Process",
                mnemonic: 'P',
                entries: vec![
                    MenuEntry {
                        label: "Reverse",
                        shortcut: "Ctrl+r",
                        action: Action::Reverse,
                    },
                    MenuEntry {
                        label: "Normalize",
                        shortcut: "Ctrl+n",
                        action: Action::Normalize,
                    },
                    MenuEntry {
                        label: "Gain",
                        shortcut: "Ctrl+g",
                        action: Action::Gain,
                    },
                    MenuEntry {
                        label: "Fade In",
                        shortcut: "Ctrl+f",
                        action: Action::FadeIn,
                    },
                    MenuEntry {
                        label: "Fade Out",
                        shortcut: "Ctrl+o",
                        action: Action::FadeOut,
                    },
                    MenuEntry {
                        label: "Trim",
                        shortcut: "Ctrl+t",
                        action: Action::Trim,
                    },
                    MenuEntry {
                        label: "Resample",
                        shortcut: "Ctrl+e",
                        action: Action::Resample,
                    },
                ],
            },
            MenuItem {
                label: "Markers",
                mnemonic: 'M',
                entries: vec![
                    MenuEntry {
                        label: "Insert Marker",
                        shortcut: "m",
                        action: Action::InsertMarker,
                    },
                    MenuEntry {
                        label: "Delete Marker",
                        shortcut: "M",
                        action: Action::DeleteMarker,
                    },
                    MenuEntry {
                        label: "Previous Marker",
                        shortcut: "[",
                        action: Action::JumpPrevMarker,
                    },
                    MenuEntry {
                        label: "Next Marker",
                        shortcut: "]",
                        action: Action::JumpNextMarker,
                    },
                ],
            },
            MenuItem {
                label: "Transport",
                mnemonic: 'T',
                entries: vec![
                    MenuEntry {
                        label: "Play/Pause",
                        shortcut: "Space",
                        action: Action::TogglePlayback,
                    },
                    MenuEntry {
                        label: "Loop Playback",
                        shortcut: "l",
                        action: Action::ToggleLoop,
                    },
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
                        Span::styled(e.shortcut, Style::default().fg(theme::SHORTCUT)),
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
        let mut menu = MenuBar::new();
        assert!(menu.open_by_mnemonic('e'));
        assert!(menu.is_open());
    }

    #[test]
    fn move_right_wraps_around() {
        let mut menu = MenuBar::new();
        menu.open_first();
        for _ in 0..menu.items.len() {
            menu.move_right();
        }
        // Wrapped all the way around back to the first menu, first entry.
        assert_eq!(menu.activate(), Some(Action::Save));
    }

    #[test]
    fn activate_closes_menu() {
        let mut menu = MenuBar::new();
        menu.open_by_mnemonic('E');
        let action = menu.activate();
        assert_eq!(action, Some(Action::Cut));
        assert!(!menu.is_open());
    }
}
