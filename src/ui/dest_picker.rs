//! The destination directory picker shared by every dialog that writes a file.
//!
//! Six dialogs choose somewhere to write — Save As, Export, Export Channels, Export Regions,
//! Save Curve As, Save Matrix As — and before this they did it three incompatible ways: four
//! silently resolved a bare filename against whatever directory the Files panel happened to be
//! showing (invisible in the dialog, so the destination was unknowable without closing it), and
//! two offered a typed folder field, which is the thing a picker exists to avoid.
//!
//! This is one browsable list, rendered **inline on the left** of the dialog with the form
//! fields beside it, so the destination is visible without a keystroke and changing it costs no
//! mode switch. An overlay was the cheaper build and was rejected for exactly that: a
//! destination you have to press a key to see is one you will not check.
//!
//! It is a plain `FilePanel` underneath — the same widget the Files panel, the curve loader and
//! the image picker all use — constructed with an **empty extension list**, which
//! `FilePanel::scan_dir` already reads as "directories only". Nothing here re-implements
//! browsing; hidden entries, the pinned `..` row and the sort order all come from there and stay
//! in step with the rest of the app for free.

use std::path::{Path, PathBuf};

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crossterm::event::{KeyCode, KeyEvent};

use super::file_panel::{EntryKind, FilePanel};
use super::theme;

/// Columns the directory list occupies inside a dialog that hosts one.
///
/// Fixed rather than a fraction of the popup: the form beside it holds labelled fields whose
/// width is set by their content ("Filename: ", a 32-bit-float label), and letting the list take
/// a share of a wider terminal would stretch the form's blank space rather than show more path.
/// 30 fits the directory names that actually occur without crowding the fields.
pub const WIDTH: u16 = 30;

/// Rows the full-width current-path row takes, below both columns. Hosts reserve this plus one
/// for their hints bar; see [`DestPicker::render_path`].
pub const PATH_ROWS: u16 = 1;

/// Rows the "Save in" label takes.
const HEADER_ROWS: u16 = 1;

/// Blank rows above that label, so the header does not sit flush against the popup border.
const TOP_PAD: u16 = 1;

/// Visible directory rows, and so the height every hosting dialog is sized around.
///
/// A fixed constant rather than whatever is left over, for the reason `CDP_BROWSER_LIST_ROWS`
/// gives: the renderer and any click hit-testing must agree, and a content-dependent height
/// makes them drift. Sized generously on purpose — the first version showed seven rows against
/// a 44-entry home directory and was reported as unusably cramped; a folder list you have to
/// scroll to see three entries of is a worse way to choose a folder than typing the path was.
pub const LIST_ROWS: u16 = 18;

/// A browsable directory choice for one dialog session.
///
/// Deliberately holds no filename: *what* to call the file is the hosting dialog's own field,
/// and the two are edited independently. `directory` is the only thing this owns.
pub struct DestPicker {
    panel: FilePanel,
}

impl DestPicker {
    /// Opens at `start`, listing directories only.
    pub fn new(start: PathBuf) -> Self {
        let mut panel = FilePanel::new_with_extension(start, &[], "Save in");
        // Focus drives the accent colour, and the hosting dialog decides which of its fields is
        // focused — so this starts unfocused and is told per-frame by `set_focused`.
        panel.focused = false;
        Self { panel }
    }

    /// The chosen directory itself — for the two dialogs that create a *subfolder* inside it
    /// (Export Channels, Export Regions) rather than writing a single named file.
    pub fn directory(&self) -> &Path {
        &self.panel.directory
    }

    /// Resolves `name` against the chosen directory, leaving an absolute path alone.
    ///
    /// Typing a full path still works, and still wins — the picker is an easier way to say where,
    /// not a restriction on what can be said.
    pub fn resolve(&self, name: &str) -> PathBuf {
        let path = PathBuf::from(name);
        if path.is_absolute() {
            path
        } else {
            self.panel.directory.join(name)
        }
    }

    /// Whether the list draws as the focused pane. Called once per frame by the host rather than
    /// stored across keystrokes, so it cannot disagree with the host's own focus index.
    pub fn set_focused(&mut self, focused: bool) {
        self.panel.focused = focused;
    }

    /// Handles one key while the list has focus. `true` when it was consumed.
    ///
    /// Enter **navigates** and never commits, which is the same split
    /// `App::use_cdp_picker_directory` needed for its folder mode and for the same reason: a
    /// picker whose Enter committed could only ever reach one level below wherever it opened.
    /// The host keeps Enter's usual "apply" meaning on every other field, so choosing a nested
    /// folder and then saving is Tab-away-then-Enter, not a special key.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => self.panel.move_up(),
            KeyCode::Down => self.panel.move_down(),
            KeyCode::Home => self.panel.move_top(),
            KeyCode::End => self.panel.move_bottom(),
            KeyCode::PageUp => self.panel.move_page_up(),
            KeyCode::PageDown => self.panel.move_page_down(),
            KeyCode::Enter => self.activate(),
            _ => return false,
        }
        true
    }

    /// Selects the row under `(x, y)`. `true` when one was hit, so a caller can tell a click
    /// inside the column from one that missed it — the inline column shares its dialog with
    /// other rows, unlike the overlay pickers, so a miss must fall through rather than be
    /// swallowed.
    pub fn handle_click(&mut self, x: u16, y: u16) -> bool {
        self.panel.handle_click(x, y)
    }

    /// Descends into the highlighted directory — the mouse's double-click, the keyboard's
    /// Enter. Shares `handle_key`'s rule that this never commits, only navigates.
    pub fn activate(&mut self) {
        if let Some((path, EntryKind::Parent | EntryKind::Dir)) = self.panel.selected_entry() {
            self.panel.set_directory(path);
        }
    }

    /// Draws the list and the current-path footer into `area`.
    ///
    /// The footer is the point of the whole component: the list shows where you could go, and
    /// this shows where you *are* — which is the fact the dialogs were missing. Truncated from
    /// the **left** when it does not fit, because the tail of a path identifies a folder and the
    /// head is the part every candidate shares.
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        // One full-height rule down the right, separating this column from the form — the same
        // single-divider treatment `CdpBrowser`'s columns get, and the reason `render_column`
        // draws the list itself borderless. It spans exactly this area, which the host sizes to
        // stop above the full-width path row (see `render_path`).
        let divider = Rect { x: area.x + area.width.saturating_sub(1), width: 1, ..area };
        frame.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(theme::BORDER))
                .style(Style::default().bg(theme::SURFACE0)),
            divider,
        );
        let area = Rect { width: area.width.saturating_sub(1), ..area };

        // A blank row above the label, matching the leading spacer every other dialog in the
        // app opens with — without it the header sits flush against the popup's own top border
        // and reads as part of the frame rather than as content.
        let accent = if self.panel.focused { theme::FOCUS } else { theme::CHROME_FG };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " Save in",
                Style::default().fg(accent).bg(theme::SURFACE0),
            ))),
            Rect { y: area.y + TOP_PAD, height: HEADER_ROWS.min(area.height), ..area },
        );

        let used = TOP_PAD + HEADER_ROWS;
        let list = Rect {
            y: area.y + used,
            height: area.height.saturating_sub(used),
            ..area
        };
        self.panel.render_column(frame, list);
    }

    /// Draws the chosen directory as a **full-width** row, spanning the whole dialog rather than
    /// the list column.
    ///
    /// Split out of `render` because a real path routinely does not fit 30 columns (user report,
    /// 2026-08-07: `/home/dingus/scripts` truncated in the pane while the form beside it sat
    /// half empty). The destination is the one thing here that must be readable in full, so it
    /// gets the width the rest of the dialog is not using.
    ///
    /// Still truncated from the **left** when even that is not enough, because the tail
    /// identifies a folder and the head is what every candidate shares.
    pub fn render_path(&self, frame: &mut Frame, area: Rect) {
        let shown =
            truncate_left(&self.panel.directory.to_string_lossy(), (area.width as usize).saturating_sub(2));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {shown}"),
                Style::default().fg(theme::CHROME_FG).bg(theme::SURFACE0),
            ))),
            area,
        );
    }
}

/// Keeps the last `width` characters, prefixed with `…` when anything was dropped.
fn truncate_left(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    // One column goes to the ellipsis, so keep one fewer character than the budget.
    let skip = count - width.saturating_sub(1);
    let tail: String = text.chars().skip(skip).collect();
    format!("\u{2026}{tail}")
}

/// Splits a dialog popup into the directory pane and the space left for its own fields.
///
/// One function so a dialog's renderer and its click hit-testing cannot compute different
/// splits — the same reason `photo_picker_layout` exists.
///
/// Degrades rather than overlapping: below `WIDTH * 2` the popup gives the list nothing and the
/// form everything, since a dialog squeezed onto a narrow terminal is more useful with readable
/// fields and no picker than with both unusable.
pub fn split(popup: Rect) -> (Option<Rect>, Rect) {
    let inner = Rect {
        x: popup.x + 1,
        y: popup.y + 1,
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    };
    if inner.width < WIDTH * 2 {
        return (None, inner);
    }
    let list = Rect { width: WIDTH, ..inner };
    let form = Rect {
        x: inner.x + WIDTH,
        width: inner.width.saturating_sub(WIDTH),
        ..inner
    };
    (Some(list), form)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_name_lands_in_the_chosen_directory() {
        let picker = DestPicker::new(PathBuf::from("/audio/session3"));
        assert_eq!(picker.resolve("take.wav"), PathBuf::from("/audio/session3/take.wav"));
    }

    /// Typing a full path must still win — the picker makes the common case easier without
    /// taking away the escape hatch that already worked.
    #[test]
    fn an_absolute_name_ignores_the_picker() {
        let picker = DestPicker::new(PathBuf::from("/audio/session3"));
        assert_eq!(picker.resolve("/elsewhere/take.wav"), PathBuf::from("/elsewhere/take.wav"));
    }

    /// The tail identifies the folder; the head is what every candidate shares. A right-truncated
    /// path would show `/home/dingus/audio/very-long-…` on every row and distinguish nothing.
    #[test]
    fn a_long_path_keeps_its_tail() {
        assert_eq!(truncate_left("/home/dingus/audio/session3", 12), "\u{2026}io/session3");
        assert_eq!(truncate_left("/short", 12), "/short");
        assert_eq!(truncate_left("anything", 0), "");
    }

    /// A terminal too narrow for both gets a usable form rather than two cramped columns.
    #[test]
    fn a_narrow_popup_drops_the_list_instead_of_overlapping() {
        let (list, form) = split(Rect { x: 0, y: 0, width: 40, height: 12 });
        assert!(list.is_none(), "40 columns cannot hold a {WIDTH}-column list and a form");
        assert_eq!(form.width, 38, "the form still gets the whole inner width");

        let (list, form) = split(Rect { x: 0, y: 0, width: 92, height: 20 });
        let list = list.expect("a wide popup hosts the list");
        assert_eq!(list.width, WIDTH);
        assert_eq!(form.x, list.x + WIDTH, "the two panes must not overlap");
        assert_eq!(list.width + form.width, 90, "and must fill the inner width exactly");
    }
}
