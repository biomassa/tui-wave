use std::collections::{HashMap, HashSet};

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
    /// Actions whose entry (see `keymap::Action::is_checkable`) should render with a
    /// checkmark — e.g. dot-matrix mode when it's on. Mirrors `Toolbar::active_actions`:
    /// `App::render` clears and repopulates it every frame from live toggle state.
    pub active_actions: HashSet<Action>,
    /// Actions the active buffer will refuse, rendered dimmed. Repopulated every frame from
    /// `App::action_allowed_on_streamed_buffer` — the *same* predicate that decides what
    /// `handle_action` permits, so what the menu shows and what pressing it does cannot disagree.
    ///
    /// Dimmed entries still dispatch. Greying says "this will not work" up front; the refusal
    /// dialog then says *why*, which is more use than a keypress that silently does nothing.
    pub disabled_actions: HashSet<Action>,
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
                    entry("Save As...",                  Action::SaveAs,      "Ctrl+Shift+S"),
                    entry("Save All",                 Action::SaveAll,     "Ctrl+l"),
                    entry("Export Regions to Subfolder...", Action::ExportRegions, "Shift+E"),
                    entry("Export Channels...",             Action::ExportChannels, ""),
                    entry("Export (FLAC/MP3)...",        Action::Export,         ""),
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
                    entry("Gradient",                         Action::ToggleDotMatrixGradient,     ""),
                    entry("Time Ruler",                       Action::ToggleTimeRuler,             ""),
                    entry("Scroll Channels Up",               Action::ScrollChannelsUp,            ","),
                    entry("Scroll Channels Down",             Action::ScrollChannelsDown,          "."),
                ],
            },
            MenuItem {
                label: "Process",
                mnemonic: 'P',
                entries: vec![
                    entry("Reverse",         Action::Reverse,       "Ctrl+r"),
                    entry("Normalize...",       Action::Normalize,     "Ctrl+n"),
                    entry("Gain...",            Action::Gain,          "Ctrl+g"),
                    entry("Fade In...",         Action::FadeIn,        "Ctrl+f"),
                    entry("Fade Out...",        Action::FadeOut,       "Ctrl+o"),
                    entry("Trim",            Action::Trim,          "Ctrl+t"),
                    entry("Auto-Trim Silence...", Action::AutoTrimSilence, ""),
                    entry("Resample...",        Action::Resample,      "Ctrl+e"),
                    entry("Technical Fades", Action::TechnicalFades,"Ctrl+b"),
                    entry("Mix to Mono...",     Action::MixToMono,     "Ctrl+m"),
                    entry("Mix Multichannel to Stereo...", Action::MixToStereo, ""),
                    entry("Remove Empty Channels...", Action::RemoveEmptyChannels, ""),
                    entry("Remove DC Offset...", Action::RemoveDcOffset, ""),
                    entry("High-Pass Filter...", Action::HighPass, ""),
                ],
            },
            MenuItem {
                label: "ExtProcess",
                // `X`, because `E` is Edit's and `P` is Process's. Mnemonics are pure Alt+key
                // lookups (`open_by_mnemonic`) and are not underlined in the rendered label,
                // so any free letter would work — but one that appears in the label is the
                // only kind a user can guess.
                mnemonic: 'X',
                entries: vec![
                    entry("ExtProcess...",              Action::CdpProcess,            "Ctrl+p"),
                    entry("ExtProcess Chain...",        Action::CdpChain,              "Ctrl+h"),
                    entry("CDP Extract Pitch Curve",    Action::ExtractPitchCurve,     ""),
                    entry("CDP Load Pitch Curve...",    Action::LoadPitchCurve,        ""),
                    entry("CDP Extract Formants (Pitch-wise)", Action::ExtractFormants, ""),
                    entry("CDP Extract Formants (Frequency-wise)", Action::ExtractFormantsFreqwise, ""),
                    entry("CDP Freeze Formant Snapshot at Cursor", Action::FreezeSnapshotAtCursor, ""),
                    entry("Configure CDP Directory...",  Action::ConfigureCdpDirectory, ""),
                ],
            },
            MenuItem {
                label: "Markers",
                mnemonic: 'M',
                entries: vec![
                    entry("Insert Marker",                        Action::InsertMarker,               "m"),
                    entry("Delete Marker",                        Action::DeleteMarker,               "M"),
                    entry("Insert Head/Tail Mark",                Action::InsertHeadTailMark,         "h"),
                    entry("Delete Head/Tail Mark",                Action::DeleteHeadTailMark,         "H"),
                    entry("Previous Marker",                      Action::JumpPrevMarker,             "["),
                    entry("Next Marker",                          Action::JumpNextMarker,             "]"),
                    entry("Extend Selection to Previous Marker",  Action::ExtendSelectionToPrevMarker,"{"),
                    entry("Extend Selection to Next Marker",      Action::ExtendSelectionToNextMarker,"}"),
                    entry("Next Rising Edge",                     Action::NextRisingEdge,             "/"),
                    entry("Previous Rising Edge",                 Action::PrevRisingEdge,             "\\"),
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
        ];
        Self {
            items,
            active_actions: HashSet::new(),
            disabled_actions: HashSet::new(),
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
            .flat_map(|(i, item)| title_spans(item, self.open == Some(i)))
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

        // Fixed-width columns (label, then checkmark, then right-aligned shortcut) so every
        // row's checkmark and shortcut line up vertically instead of drifting with each
        // label's own length. Measured in *chars*, not `str::len()` (UTF-8 bytes) — "✓"
        // (U+2713) is a single display column but 3 bytes, and byte-counting it here would
        // silently throw the alignment off by 2 on every checked row.
        const CHECK_SLOT: &str = "  "; // width of " ✓", used as the un-checked filler too
        let has_checkable = entries.iter().any(|e| e.action.is_checkable());
        let label_width = entries.iter().map(|e| e.label.chars().count()).max().unwrap_or(0);
        let shortcut_width = entries.iter().map(|e| e.shortcut.chars().count()).max().unwrap_or(0);
        let checkmark_width = if has_checkable { CHECK_SLOT.chars().count() } else { 0 };
        let inner_width = (label_width + checkmark_width + shortcut_width + 4) as u16;
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
                let checkmark = if !has_checkable {
                    ""
                } else if e.action.is_checkable() && self.active_actions.contains(&e.action) {
                    " \u{2713}"
                } else {
                    CHECK_SLOT
                };
                let label = format!("{:<label_width$}", e.label);
                let pad = (popup.width as usize)
                    .saturating_sub(2)
                    .saturating_sub(label.chars().count())
                    .saturating_sub(checkmark.chars().count())
                    .saturating_sub(e.shortcut.chars().count());
                let disabled = self.disabled_actions.contains(&e.action);
                let line = if self.selected == i {
                    // Selected: one uniform highlight rather than juggling a third accent
                    // color against it, which would risk a low-contrast clash. A disabled entry
                    // keeps the highlight — it is still where the cursor is — but in the dim
                    // foreground, so "selected" and "unavailable" read at the same time.
                    let fg = if disabled { theme::BORDER } else { theme::HIGHLIGHT_FG };
                    let style = Style::default().fg(fg).bg(theme::HIGHLIGHT_BG);
                    Line::styled(format!("{label}{checkmark}{}{}", " ".repeat(pad), e.shortcut), style)
                } else if disabled {
                    // One flat dim colour across the whole row, shortcut included: a peach
                    // shortcut beside a greyed label would advertise a key that does nothing.
                    Line::styled(
                        format!("{label}{checkmark}{}{}", " ".repeat(pad), e.shortcut),
                        Style::default().fg(theme::BORDER),
                    )
                } else {
                    Line::from(vec![
                        Span::styled(label, Style::default().fg(theme::CHROME_FG)),
                        // Same accent the toolbar uses for an active toggle's label, so
                        // "this is currently on" reads the same way in both places.
                        Span::styled(checkmark, Style::default().fg(theme::ACTIVE)),
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

/// One bar title, split so its `Alt+` mnemonic letter renders in `theme::SHORTCUT` — the same
/// peach every other advertised keystroke uses, so "the key that opens this" reads the same way
/// on the bar as it does on a menu row's shortcut column and a toolbar button.
///
/// The mnemonic is matched case-insensitively against the label and only the *first* occurrence
/// is accented: `ExtProcess`'s mnemonic is `X`, which appears in the label lowercase (`Alt+x`
/// and `Alt+X` both open it — see `open_by_mnemonic`). A mnemonic that appears nowhere in its
/// own label would leave the user nothing to guess from, so the title simply renders plain
/// rather than accenting an arbitrary letter.
///
/// An **open** menu keeps its title one uniform highlight, exactly as a selected menu entry
/// does: peach on `HIGHLIGHT_BG` (mauve) is two light pastel accents against each other, which
/// is the low-contrast clash the entry renderer already avoids. The bar's accent says "press
/// this letter to get here", and once you are there it has nothing left to say.
fn title_spans(item: &MenuItem, open: bool) -> Vec<Span<'static>> {
    if open {
        let style = Style::default().fg(theme::HIGHLIGHT_FG).bg(theme::HIGHLIGHT_BG);
        return vec![Span::styled(format!(" {} ", item.label), style)];
    }
    let base = Style::default().fg(theme::CHROME_FG).bg(theme::CHROME_BG);
    let accent = Style::default().fg(theme::SHORTCUT).bg(theme::CHROME_BG);
    // Split by *chars*, not bytes: every label here is ASCII, but slicing a label by a byte
    // index is the kind of thing that only breaks once someone adds a non-ASCII one.
    let chars: Vec<char> = item.label.chars().collect();
    let Some(at) = chars.iter().position(|c| c.eq_ignore_ascii_case(&item.mnemonic)) else {
        return vec![Span::styled(format!(" {} ", item.label), base)];
    };
    vec![
        Span::styled(format!(" {}", chars[..at].iter().collect::<String>()), base),
        Span::styled(chars[at].to_string(), accent),
        Span::styled(format!("{} ", chars[at + 1..].iter().collect::<String>()), base),
    ]
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
    fn a_bar_title_accents_its_alt_mnemonic() {
        let menu = MenuBar::new(&HashMap::new());
        let file = &menu.items[0];
        let spans = title_spans(file, false);
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<Vec<_>>(),
            vec![" ", "F", "ile "],
        );
        assert_eq!(spans[1].style.fg, Some(theme::SHORTCUT));
        assert_eq!(spans[0].style.fg, Some(theme::CHROME_FG));
    }

    #[test]
    fn a_lowercase_mnemonic_letter_is_still_the_one_accented() {
        // ExtProcess's mnemonic is 'X', which appears in its own label in lowercase.
        let menu = MenuBar::new(&HashMap::new());
        let ext = menu.items.iter().find(|i| i.label == "ExtProcess").expect("ExtProcess menu");
        let spans = title_spans(ext, false);
        assert_eq!(spans[1].content.as_ref(), "x");
        assert_eq!(spans[1].style.fg, Some(theme::SHORTCUT));
    }

    #[test]
    fn accenting_the_mnemonic_does_not_move_a_title() {
        // `hit_test_bar` indexes `layout_bar_items`, which sizes each title as label + 2 —
        // splitting the rendered title into three spans must not change what it occupies.
        let menu = MenuBar::new(&HashMap::new());
        for item in &menu.items {
            for open in [false, true] {
                let width: usize =
                    title_spans(item, open).iter().map(|s| s.content.chars().count()).sum();
                assert_eq!(width, item.label.chars().count() + 2, "{} (open={open})", item.label);
            }
        }
    }

    #[test]
    fn an_open_menu_title_stays_one_uniform_highlight() {
        let menu = MenuBar::new(&HashMap::new());
        let spans = title_spans(&menu.items[0], true);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.bg, Some(theme::HIGHLIGHT_BG));
        assert_eq!(spans[0].style.fg, Some(theme::HIGHLIGHT_FG));
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
