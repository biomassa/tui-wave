use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::audio::engine::AudioEngine;
use crate::commands::cut::cut_command;
use crate::commands::delete::delete_command;
use crate::commands::gain::gain_command;
use crate::commands::paste::paste_command;
use crate::commands::normalize::normalize_command;
use crate::commands::reverse::reverse_command;
use crate::model::clipboard::Clipboard;
use crate::model::document::Document;
use crate::model::history::History;
use crate::model::io::save_wav;
use crate::model::selection::Selection;

use super::file_panel::FilePanel;
use super::keymap::{map_key, Action};
use super::layout::split_chrome;
use super::menu::MenuBar;
use super::terminal::Tui;
use super::theme;
use super::toolbar::Toolbar;
use super::viewport::Viewport;
use super::waveform_cache::WaveformCache;
use super::widgets::db_scale::{DbScaleWidget, DB_GUTTER_WIDTH};
use super::widgets::statusbar::StatusBar;
use super::widgets::waveform::WaveformWidget;

enum Dialog {
    Normalize { buffer: String },
    Gain { buffer: String, tanh_clip: bool },
}

pub struct App {
    pub should_quit: bool,
    pub document: Option<Document>,
    pub viewport: Option<Viewport>,
    pub audio: Option<AudioEngine>,
    pub history: History,
    pub clipboard: Clipboard,
    pub menu: MenuBar,
    pub toolbar: Toolbar,
    /// One precomputed min/max cache per channel, rebuilt whenever the document's sample
    /// data changes. Keeps waveform render cost bounded by screen width instead of file
    /// length — see `ui::waveform_cache`.
    pub waveform_caches: Vec<WaveformCache>,
    /// Width/area of the waveform content as of the last render; navigation/zoom/mouse
    /// actions need this and re-reading it from the terminal on every input would require
    /// a redraw, so it's cached here instead.
    pub content_width: u16,
    pub waveform_area: Rect,
    /// Set when Quit is requested with unsaved changes; intercepts the next keypress as a
    /// y/n confirmation instead of routing it through the normal keymap.
    pub quit_confirm: bool,
    /// Sample position where the current mouse-down started (for drag-to-select).
    mouse_down_anchor: Option<usize>,
    /// File panel on the left showing WAV files in the current directory.
    pub file_panel: FilePanel,
    /// When true, the user is typing a Save-As path in a prompt overlay.
    pub save_as_active: bool,
    /// Buffer for the Save-As path being typed.
    pub save_as_path: String,
    /// When true, destructive operations snap selection boundaries to zero crossings.
    pub snap_to_zero: bool,
    /// When true, playback loops — the full file if no selection, or the selection range.
    pub loop_playback: bool,
    /// Active parameter dialog (Normalize or Gain), if any.
    dialog: Option<Dialog>,
    /// The current playback position, set from `AudioEngine.position` during playback.
    /// `None` when playback is stopped. This is the visual playhead only — the cursor
    /// (insertion point) lives on `Document.cursor`.
    playhead_position: Option<usize>,
}

impl App {
    pub fn new(document: Option<Document>, directory: Option<PathBuf>) -> Self {
        let dir = directory
            .or_else(|| document.as_ref().and_then(|d| d.path.as_ref()).and_then(|p| p.parent().map(|p| p.to_path_buf())))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let file_panel = FilePanel::new(dir);

        let audio = document
            .as_ref()
            .and_then(|doc| AudioEngine::try_new(doc.channels.clone(), doc.sample_rate));
        let waveform_caches = document
            .as_ref()
            .map(|doc| doc.channels.iter().map(|c| WaveformCache::build(c)).collect())
            .unwrap_or_default();
        Self {
            should_quit: false,
            document,
            viewport: None,
            audio,
            history: History::new(),
            clipboard: Clipboard::default(),
            menu: MenuBar::new(),
            toolbar: Toolbar::new(),
            waveform_caches,
            content_width: 1,
            waveform_area: Rect::default(),
            quit_confirm: false,
            mouse_down_anchor: None,
            file_panel,
            save_as_active: false,
            save_as_path: String::new(),
            snap_to_zero: true,
            loop_playback: false,
            dialog: None,
            playhead_position: None,
        }
    }

    /// Returns the playback loop range: the current selection if one exists, or the full
    /// document if nothing is selected. Returns `None` when loop playback is disabled.
    fn loop_range(&self) -> Option<(usize, usize)> {
        if !self.loop_playback {
            return None;
        }
        self.document.as_ref().map(|doc| {
            doc.selection
                .map(|sel| sel.normalized())
                .unwrap_or((0, doc.len_samples()))
        })
    }

    /// Highest peak within the current visible window. Computed from the precomputed cache
    /// so it's cheap enough to call every frame.
    fn visible_peak(&self) -> f32 {
        visible_peak_raw(
            self.document.as_ref(),
            self.viewport.as_ref(),
            &self.waveform_caches,
            self.content_width,
        )
    }

    fn rebuild_waveform_caches(&mut self) {
        self.waveform_caches = self
            .document
            .as_ref()
            .map(|doc| doc.channels.iter().map(|c| WaveformCache::build(c)).collect())
            .unwrap_or_default();
    }

    pub fn run(&mut self, terminal: &mut Tui) -> color_eyre::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    Event::Key(key) => self.handle_key(key),
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    _ => {}
                }
            }
            self.sync_playhead_from_audio();
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.quit_confirm {
            self.handle_quit_confirm_key(key);
            return;
        }
        if self.menu.is_open() {
            self.handle_menu_key(key);
            return;
        }
        if self.save_as_active {
            self.handle_save_as_key(key);
            return;
        }
        if self.dialog.is_some() {
            self.handle_dialog_key(key);
            return;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(c) = key.code {
                if self.menu.open_by_mnemonic(c) {
                    return;
                }
            }
        }
        if key.code == KeyCode::F(10) {
            self.menu.open_first();
            return;
        }
        // File panel filtering
        if self.file_panel.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.file_panel.filtering = false;
                    self.file_panel.filter.clear();
                }
                KeyCode::Enter => {
                    self.open_selected_file();
                }
                KeyCode::Backspace => {
                    self.file_panel.filter.pop();
                    self.file_panel.selected = 0;
                }
                KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                    self.file_panel.filter.push(c);
                    self.file_panel.selected = 0;
                }
                _ => {}
            }
            return;
        }
        // File panel keyboard focus
        if self.file_panel.focused {
            match key.code {
                KeyCode::Up => self.file_panel.move_up(),
                KeyCode::Down => self.file_panel.move_down(),
                KeyCode::Home => self.file_panel.move_top(),
                KeyCode::End => self.file_panel.move_bottom(),
                KeyCode::Enter => self.open_selected_file(),
                KeyCode::Char('/') => {
                    self.file_panel.filtering = true;
                    self.file_panel.filter.clear();
                }
                KeyCode::Tab => {
                    self.file_panel.focused = false;
                }
                KeyCode::Esc => {
                    self.file_panel.focused = false;
                }
                _ => {}
            }
            return;
        }
        // Tab or '/' while not focused → focus the panel or start filtering
        if key.code == KeyCode::Tab {
            self.file_panel.focused = true;
            return;
        }
        if key.code == KeyCode::Char('/') {
            self.file_panel.filtering = true;
            self.file_panel.filter.clear();
            return;
        }
        if let Some(action) = map_key(key) {
            self.handle_action(action);
        }
    }

    fn handle_menu_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.menu.move_left(),
            KeyCode::Right => self.menu.move_right(),
            KeyCode::Up => self.menu.move_up(),
            KeyCode::Down => self.menu.move_down(),
            KeyCode::Enter => {
                if let Some(action) = self.menu.activate() {
                    self.handle_action(action);
                }
            }
            KeyCode::Esc => self.menu.close(),
            _ => {}
        }
    }

    fn handle_quit_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.should_quit = true,
            _ => self.quit_confirm = false,
        }
    }

    fn handle_save_as_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let path = PathBuf::from(&self.save_as_path);
                let path = if path.is_absolute() {
                    path
                } else {
                    self.file_panel.directory.join(&self.save_as_path)
                };
                if let Some(document) = self.document.as_mut() {
                    if save_wav(document, &path).is_ok() {
                        document.path = Some(path.clone());
                        document.dirty = false;
                        self.file_panel.mark_dirty(&path, false);
                        self.file_panel.scan();
                    }
                }
                self.save_as_active = false;
                self.save_as_path.clear();
            }
            KeyCode::Esc => {
                self.save_as_active = false;
                self.save_as_path.clear();
            }
            KeyCode::Backspace => {
                self.save_as_path.pop();
            }
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.save_as_path.push(c);
            }
            _ => {}
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let (target_db, tanh_clip) = match self.dialog.take() {
                    Some(Dialog::Normalize { buffer }) => {
                        let db = buffer.parse::<f32>().unwrap_or(-1.0).min(0.0);
                        (Some(db), None)
                    }
                    Some(Dialog::Gain { buffer, tanh_clip }) => {
                        let db = buffer.parse::<f32>().unwrap_or(0.0);
                        (Some(db), Some(tanh_clip))
                    }
                    None => (None, None),
                };
                if let Some(db) = target_db {
                    self.apply_normalize(db);
                } else if let Some(tc) = tanh_clip {
                    self.apply_gain(0.0, tc);
                }
            }
            KeyCode::Esc => {
                self.dialog = None;
            }
            KeyCode::Backspace => {
                if let Some(dialog) = self.dialog.as_mut() {
                    match dialog {
                        Dialog::Normalize { buffer } => { buffer.pop(); }
                        Dialog::Gain { buffer, .. } => { buffer.pop(); }
                    }
                }
            }
            KeyCode::Tab => {
                if let Some(dialog) = self.dialog.as_mut() {
                    if let Dialog::Gain { tanh_clip, .. } = dialog {
                        *tanh_clip = !*tanh_clip;
                    }
                }
            }
            KeyCode::Char(c) if c == '-' || c == '.' || c.is_ascii_digit() => {
                if let Some(dialog) = self.dialog.as_mut() {
                    match dialog {
                        Dialog::Normalize { buffer } => buffer.push(c),
                        Dialog::Gain { buffer, .. } => buffer.push(c),
                    }
                }
            }
            _ => {}
        }
    }

    fn apply_normalize(&mut self, target_db: f32) {
        let Some(document) = self.document.as_mut() else { return };
        let sel = match document.selection {
            Some(s) => s,
            None => return,
        };
        let (start, end) = sel.normalized();
        if start >= end {
            return;
        }
        let (start, end) = if self.snap_to_zero {
            document.snap_range_to_zero_crossing(start, end)
        } else {
            (start, end)
        };
        if start < end {
            self.history.apply(normalize_command(start, end, target_db), document);
            if let Some(audio) = &self.audio {
                audio.reload(document.channels.clone());
            }
            self.rebuild_waveform_caches();
            if self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom) {
                let peak = self.visible_peak();
                if peak > 0.0001 {
                    if let Some(viewport) = self.viewport.as_mut() {
                        viewport.set_amplitude_scale(0.95 / peak);
                    }
                }
            }
        }
    }

    fn apply_gain(&mut self, gain_db: f32, tanh_clip: bool) {
        let Some(document) = self.document.as_mut() else { return };
        let sel = match document.selection {
            Some(s) => s,
            None => return,
        };
        let (start, end) = sel.normalized();
        if start >= end {
            return;
        }
        let (start, end) = if self.snap_to_zero {
            document.snap_range_to_zero_crossing(start, end)
        } else {
            (start, end)
        };
        if start < end {
            self.history.apply(gain_command(start, end, gain_db, tanh_clip), document);
            if let Some(audio) = &self.audio {
                audio.reload(document.channels.clone());
            }
            self.rebuild_waveform_caches();
            if self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom) {
                let peak = self.visible_peak();
                if peak > 0.0001 {
                    if let Some(viewport) = self.viewport.as_mut() {
                        viewport.set_amplitude_scale(0.95 / peak);
                    }
                }
            }
        }
    }

    fn open_selected_file(&mut self) {
        let Some(path) = self.file_panel.selected_path() else {
            return;
        };
        self.load_file(path);
    }

    fn load_file(&mut self, path: PathBuf) {
        // Stop any playback and tear down the old audio engine before replacing the
        // document, so dropping the old engine while it's still playing doesn't cause
        // a "dropping audio sink" error or garbled terminal output.
        if let Some(audio) = self.audio.take() {
            drop(audio);
        }
        // Load the new file and reset all editor state.
        match crate::model::io::load_wav(&path) {
            Ok(mut document) => {
                self.file_panel.focused = false;
                self.file_panel.filtering = false;
                self.file_panel.filter.clear();

                document.dirty = false; // freshly loaded
                self.document = Some(document);
                self.viewport = None;
                self.rebuild_audio();
                self.rebuild_waveform_caches();
            }
            Err(e) => {
                // Log error silently — the file disappeared or is corrupt.
                let _ = e;
            }
        }
    }

    fn rebuild_audio(&mut self) {
        if let Some(document) = self.document.as_ref() {
            self.audio = AudioEngine::try_new(document.channels.clone(), document.sample_rate);
        } else {
            self.audio = None;
        }
    }

    fn sync_playhead_from_audio(&mut self) {
        let Some(audio) = self.audio.as_ref() else { return };
        if audio.playing.load(std::sync::atomic::Ordering::Relaxed) {
            self.playhead_position = Some(audio.position.load(std::sync::atomic::Ordering::Relaxed));
        } else {
            self.playhead_position = None;
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        // File panel: click to focus. A follow-up double-click opens the file.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(path) = self.file_panel.handle_click(mouse.column, mouse.row) {
                self.file_panel.focused = true;
                self.load_file(path);
                return;
            }
        }

        // Menu/toolbar: only handle click (Down) events.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(idx) = self.menu.hit_test_bar(mouse.column, mouse.row) {
                self.menu.toggle_open(idx);
                return;
            }
            if self.menu.is_open() {
                if let Some(entry_idx) = self.menu.hit_test_entry(mouse.column, mouse.row) {
                    self.menu.select_entry(entry_idx);
                    if let Some(action) = self.menu.activate() {
                        self.handle_action(action);
                    }
                } else {
                    self.menu.close();
                }
                return;
            }
            if let Some(action) = self.toolbar.hit_test(mouse.column, mouse.row) {
                self.handle_action(action);
                return;
            }
        }

        // Waveform click/drag → seek + select.
        let area = self.waveform_area;
        if mouse.column < area.x
            || mouse.column >= area.x + area.width
            || mouse.row < area.y
            || mouse.row >= area.y + area.height
        {
            if matches!(mouse.kind, MouseEventKind::Up(_)) {
                self.mouse_down_anchor = None;
            }
            return;
        }
        // Capture loop state before the mutable borrow of self.document below.
        let loop_range = if self.loop_playback {
            self.document.as_ref().and_then(|d| {
                Some(d.selection.map(|sel| sel.normalized()).unwrap_or((0, d.len_samples())))
            })
        } else {
            None
        };

        let (Some(document), Some(viewport)) = (self.document.as_mut(), self.viewport.as_ref())
        else {
            return;
        };
        let total_len = document.len_samples();
        if total_len == 0 {
            return;
        }
        let col = (mouse.column - area.x) as f64;
        let target =
            (viewport.scroll_offset as f64 + col * viewport.samples_per_column) as usize;
        let target = target.min(total_len - 1);

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                document.cursor = target;
                self.mouse_down_anchor = Some(target);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                document.cursor = target;
                if let Some(anchor) = self.mouse_down_anchor {
                    document.selection = Some(Selection {
                        start: anchor,
                        end: target,
                    });
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                document.cursor = target;
                if let Some(anchor) = self.mouse_down_anchor {
                    if anchor != target {
                        document.selection = Some(Selection {
                            start: anchor,
                            end: target,
                        });
                    }
                }
                self.mouse_down_anchor = None;
            }
            _ => return,
        }

        if let Some(audio) = &self.audio {
            if audio.is_playing() {
                if let Some((ls, le)) = loop_range {
                    audio.seek_looped(document.cursor, ls, le);
                } else {
                    audio.seek(document.cursor);
                }
            }
        }
    }

    fn handle_action(&mut self, action: Action) {
        if action == Action::Quit {
            if self.document.as_ref().is_some_and(|doc| doc.dirty) {
                self.quit_confirm = true;
            } else {
                self.should_quit = true;
            }
            return;
        }

        if matches!(action, Action::TogglePlayback | Action::Stop) {
            self.handle_playback_action(action);
            return;
        }

        if matches!(
            action,
            Action::Cut
                | Action::Copy
                | Action::Paste
                | Action::Undo
                | Action::Redo
                | Action::Save
                | Action::SaveAs
                | Action::SaveAll
                | Action::Reverse
                | Action::Delete
        ) {
            self.handle_edit_action(action);
            return;
        }

        if action == Action::ClearSelection {
            if let Some(document) = self.document.as_mut() {
                document.selection = None;
            }
            return;
        }

        if action == Action::ToggleAutoVerticalZoom {
            let peak = self.visible_peak();
            if let Some(viewport) = self.viewport.as_mut() {
                viewport.auto_vertical_zoom = !viewport.auto_vertical_zoom;
                if viewport.auto_vertical_zoom && peak > 0.0001 {
                    viewport.set_amplitude_scale(0.95 / peak);
                } else if !viewport.auto_vertical_zoom {
                    viewport.set_amplitude_scale(1.0);
                }
            }
            return;
        }

        if action == Action::ToggleZeroSnap {
            self.snap_to_zero = !self.snap_to_zero;
            return;
        }

        if action == Action::ToggleLoop {
            self.loop_playback = !self.loop_playback;
            return;
        }

        if action == Action::Normalize {
            if self.document.as_ref().and_then(|d| d.selection).is_some() {
                self.dialog = Some(Dialog::Normalize { buffer: String::from("-1.0") });
            }
            return;
        }

        if action == Action::Gain {
            if self.document.as_ref().and_then(|d| d.selection).is_some() {
                self.dialog = Some(Dialog::Gain { buffer: String::from("0.0"), tanh_clip: false });
            }
            return;
        }

        // Capture loop state before the mutable borrow of self.document below.
        let loop_range = if self.loop_playback {
            self.document.as_ref().and_then(|d| {
                Some(d.selection.map(|sel| sel.normalized()).unwrap_or((0, d.len_samples())))
            })
        } else {
            None
        };

        let (Some(document), Some(viewport)) = (self.document.as_mut(), self.viewport.as_mut())
        else {
            return;
        };
        let total_len = document.len_samples();
        if total_len == 0 {
            return;
        }
        let width = self.content_width;
        let column_step = viewport.samples_per_column.max(1.0) as usize;
        let span = viewport.span(width);
        match action {
            Action::Quit
            | Action::TogglePlayback
            | Action::Stop
            | Action::Cut
            | Action::Copy
            | Action::Paste
            | Action::Undo
            | Action::Redo
            | Action::Save
            | Action::Reverse
            | Action::Normalize
            | Action::Delete
            | Action::ToggleAutoVerticalZoom
            | Action::ToggleZeroSnap
            | Action::ToggleLoop
            | Action::ClearSelection
            | Action::SaveAs
            | Action::SaveAll
            | Action::Gain => unreachable!(),
            Action::MoveCursorLeft => {
                document.cursor = document.cursor.saturating_sub(column_step.max(1));
            }
            Action::MoveCursorRight => {
                document.cursor = (document.cursor + column_step.max(1)).min(total_len - 1);
            }
            Action::MoveCursorLeftFine => {
                document.cursor = document.cursor.saturating_sub(1);
            }
            Action::MoveCursorRightFine => {
                document.cursor = (document.cursor + 1).min(total_len - 1);
            }
            Action::ExtendSelectionLeft => {
                document.cursor = document.cursor.saturating_sub(column_step.max(1));
            }
            Action::ExtendSelectionRight => {
                document.cursor = (document.cursor + column_step.max(1)).min(total_len - 1);
            }
            Action::JumpStart => document.cursor = 0,
            Action::JumpEnd => document.cursor = total_len - 1,
            Action::PageBack => {
                document.cursor = document.cursor.saturating_sub(span.max(1));
            }
            Action::PageForward => {
                document.cursor = (document.cursor + span.max(1)).min(total_len - 1);
            }
            Action::ZoomIn => viewport.zoom_in(document.cursor, width),
            Action::ZoomOut => viewport.zoom_out(document.cursor, width),
            Action::ZoomInVertical => viewport.zoom_in_vertical(),
            Action::ZoomOutVertical => viewport.zoom_out_vertical(),
        }

        // Selection handling: extend updates the selection, cursor movement clears it,
        // zoom preserves it so the user can see their selection at different zoom levels.
        match action {
            Action::ExtendSelectionLeft | Action::ExtendSelectionRight => {
                let anchor = document.selection.map(|s| s.start).unwrap_or(document.cursor);
                let start = anchor.min(document.cursor);
                document.selection = Some(Selection {
                    start: anchor,
                    end: document.cursor,
                });
                // Place the insertion point at the beginning of the selection so paste,
                // type-to-replace, etc. operate on the start of the selected range.
                document.cursor = start;
            }
            Action::MoveCursorLeft
            | Action::MoveCursorRight
            | Action::MoveCursorLeftFine
            | Action::MoveCursorRightFine
            | Action::JumpStart
            | Action::JumpEnd
            | Action::PageBack
            | Action::PageForward
            | Action::ZoomIn
            | Action::ZoomOut
            | Action::ZoomInVertical
            | Action::ZoomOutVertical => {
                // Preserve existing selection.
            }
            Action::SaveAs | Action::SaveAll => {}
            _ => unreachable!(),
        }

        viewport.ensure_visible(document.cursor, width);

        // Nav/zoom actions can move the cursor while audio is mid-playback (e.g. scrubbing
        // with arrow keys) — keep the audio thread's position in sync rather than letting
        // it silently keep playing from the old spot.
        if let Some(audio) = &self.audio {
            if audio.is_playing() {
                if let Some((ls, le)) = loop_range {
                    audio.seek_looped(document.cursor, ls, le);
                } else {
                    audio.seek(document.cursor);
                }
            }
        }
    }

    fn handle_edit_action(&mut self, action: Action) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let mutates_samples = matches!(
            action,
            Action::Cut | Action::Delete | Action::Paste | Action::Undo | Action::Redo | Action::Reverse
        );
        let snap = self.snap_to_zero;
        match action {
            Action::Cut => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    let (start, end) = if snap {
                        document.snap_range_to_zero_crossing(start, end)
                    } else {
                        (start, end)
                    };
                    if start < end {
                        self.clipboard.set(document.slice(start..end), document.sample_rate);
                        self.history.apply(cut_command(start..end), document);
                    }
                }
            }
            Action::Delete => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    let (start, end) = if snap {
                        document.snap_range_to_zero_crossing(start, end)
                    } else {
                        (start, end)
                    };
                    if start < end {
                        self.history.apply(delete_command(start..end), document);
                    }
                }
            }
            Action::Copy => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    if start < end {
                        self.clipboard.set(document.slice(start..end), document.sample_rate);
                    }
                }
            }
            Action::Paste => {
                if !self.clipboard.is_empty() {
                    // Pasting over an active selection replaces it: delete the selection
                    // first, then insert at the spot it occupied.
                    if let Some(sel) = document.selection {
                        let (start, end) = sel.normalized();
                        if start < end {
                            self.history.apply(delete_command(start..end), document);
                        }
                    }
                    let at = document.cursor;
                    let data = self.clipboard.channels.clone();
                    self.history.apply(paste_command(at, data), document);
                }
            }
            Action::Undo => {
                self.history.undo(document);
            }
            Action::Redo => {
                self.history.redo(document);
            }
            Action::Save => {
                if let Some(path) = document.path.clone() {
                    if save_wav(document, &path).is_ok() {
                        document.dirty = false;
                        self.file_panel.mark_dirty(&path, false);
                    }
                }
            }
            Action::SaveAs => {
                self.save_as_path = document
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "untitled.wav".to_string());
                self.save_as_active = true;
            }
            Action::SaveAll => {
                // Save current document if dirty
                if let Some(path) = document.path.clone() {
                    if save_wav(document, &path).is_ok() {
                        document.dirty = false;
                        self.file_panel.mark_dirty(&path, false);
                    }
                }
            }
            Action::Reverse => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    let (start, end) = if snap {
                        document.snap_range_to_zero_crossing(start, end)
                    } else {
                        (start, end)
                    };
                    if start < end {
                        self.history.apply(reverse_command(start, end), document);
                    }
                }
            }
            _ => unreachable!(),
        }

        if let Some(viewport) = self.viewport.as_mut() {
            viewport.ensure_visible(document.cursor, self.content_width);
        }
        if mutates_samples {
            if let Some(path) = document.path.as_ref() {
                self.file_panel.mark_dirty(path, true);
            }
            if let Some(audio) = &self.audio {
                audio.reload(document.channels.clone());
            }
            self.rebuild_waveform_caches();
            // Auto vertical zoom re-fits to the visible peak after edits change the data.
            if self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom) {
                let peak = self.visible_peak();
                if peak > 0.0001 {
                    if let Some(viewport) = self.viewport.as_mut() {
                        viewport.set_amplitude_scale(0.95 / peak);
                    }
                }
            }
        }
    }

    fn handle_playback_action(&mut self, action: Action) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        match action {
            Action::TogglePlayback => {
                if audio.is_playing() {
                    audio.pause();
                } else if let Some((ls, le)) = self.loop_range() {
                    audio.play_looped(document.cursor, ls, le);
                } else {
                    audio.play(document.cursor);
                }
            }
            Action::Stop => {
                audio.stop();
                self.playhead_position = None;
            }
            _ => unreachable!(),
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area: Rect = frame.area();
        let chrome = split_chrome(area);

        // Always render the file panel and chrome.
        self.file_panel.render(frame, chrome.panel);
        self.toolbar.render(frame, chrome.toolbar);
        self.menu.render(frame, chrome.menu);

        let Some(document) = &self.document else {
            let block = Block::default()
                .title(" tui-wave ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .style(Style::default().fg(theme::CHROME_FG).bg(theme::BASE));
            let text = Paragraph::new("Select a file from the panel on the left (Tab to focus, / to search)")
                .alignment(Alignment::Center)
                .block(block);
            frame.render_widget(text, chrome.content);
            return;
        };

        let title_text = format!(
            " tui-wave — {} ",
            document
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".to_string()),
        );
        let title = Line::from(vec![
            Span::styled(title_text, Style::default().fg(theme::BORDER)),
            Span::styled(
                if document.dirty { "* " } else { "" },
                Style::default().fg(theme::DIRTY),
            ),
        ]);
        let outer = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER))
            .style(Style::default().bg(theme::BASE));
        let inner = outer.inner(chrome.content);
        frame.render_widget(outer, chrome.content);

        let [waveform_area, status_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

        let gutter = DB_GUTTER_WIDTH.min(waveform_area.width / 2);
        let inner_waveform_area = Rect {
            x: waveform_area.x + gutter,
            y: waveform_area.y,
            width: waveform_area.width.saturating_sub(gutter * 2),
            height: waveform_area.height,
        };

        self.content_width = inner_waveform_area.width;
        self.waveform_area = inner_waveform_area;
        let viewport = self.viewport.get_or_insert_with(|| {
            Viewport::fit_to_width(document.len_samples(), inner_waveform_area.width as usize)
        });
        viewport.total_len = document.len_samples();

        let channel_count = document.channel_count().max(1);
        let full_chunks =
            Layout::vertical(vec![Constraint::Fill(1); channel_count]).split(waveform_area);
        let selection = document.selection.map(|s| s.normalized());

        // When auto vertical zoom is on, dynamically fit amplitude_scale to the visible
        // window's peak every frame, so scrolling/zooming to a quieter section zooms in to
        // match. The dB scale's reference_amplitude follows the same visible peak.
        let (reference_amplitude, _visible_peak) = if viewport.auto_vertical_zoom {
            let vp = visible_peak_raw(
                self.document.as_ref(),
                Some(viewport),
                &self.waveform_caches,
                self.content_width,
            );
            if vp > 0.0001 {
                viewport.set_amplitude_scale(0.95 / vp);
                (vp, vp)
            } else {
                (0.0001, 0.0)
            }
        } else {
            (1.0, 0.0)
        };

        for (i, channel_full_area) in full_chunks.iter().enumerate() {
            let channel_inner = Rect {
                x: channel_full_area.x + gutter,
                y: channel_full_area.y,
                width: channel_full_area.width.saturating_sub(gutter * 2),
                height: channel_full_area.height,
            };
            let left_gutter = Rect {
                x: channel_full_area.x,
                y: channel_full_area.y,
                width: gutter,
                height: channel_full_area.height,
            };
            let right_gutter = Rect {
                x: channel_full_area.x + channel_full_area.width - gutter,
                y: channel_full_area.y,
                width: gutter,
                height: channel_full_area.height,
            };

            let samples = document
                .channels
                .get(i)
                .map(|c| c.as_slice())
                .unwrap_or(&[]);
            let widget = WaveformWidget {
                samples,
                viewport,
                cache: self.waveform_caches.get(i),
                selection,
                cursor: document.cursor,
                playhead: self.playhead_position,
            };
            frame.render_widget(widget, channel_inner);

            let db_scale = DbScaleWidget {
                amplitude_scale: viewport.amplitude_scale,
                reference_amplitude,
            };
            frame.render_widget(db_scale, left_gutter);
            frame.render_widget(db_scale, right_gutter);
        }

        frame.render_widget(StatusBar { document, viewport }, status_area);

        if self.quit_confirm {
            render_quit_confirm(frame, area);
        }

        if self.save_as_active {
            render_save_as_prompt(frame, area, &self.save_as_path);
        }

        if let Some(ref dialog) = self.dialog {
            render_dialog(frame, area, dialog);
        }
    }
}

/// Peak sample magnitude within the visible window. Takes explicit parameters to avoid
/// borrow conflicts with concurrent mutable access to `self.viewport`.
fn visible_peak_raw(
    document: Option<&Document>,
    viewport: Option<&Viewport>,
    waveform_caches: &[WaveformCache],
    content_width: u16,
) -> f32 {
    let (Some(document), Some(viewport)) = (document, viewport) else {
        return 0.0;
    };
    let visible_end = (viewport.scroll_offset + viewport.span(content_width))
        .min(document.len_samples());
    if viewport.scroll_offset >= visible_end || content_width == 0 {
        return 0.0;
    }
    waveform_caches
        .iter()
        .zip(document.channels.iter())
        .fold(0.0f32, |peak, (cache, samples)| {
            let (mn, mx) = cache.min_max(samples, viewport.scroll_offset, visible_end);
            peak.max(mn.abs()).max(mx.abs())
        })
}

fn render_save_as_prompt(frame: &mut Frame, area: Rect, path: &str) {
    let text = format!(" Save as: {}_ ", path);
    let width = (text.chars().count() as u16 + 2).min(area.width);
    let height = 3.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + area.height.saturating_sub(height + 1),
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = Block::default()
        .title("Save As")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().fg(theme::TEXT).bg(theme::SURFACE0));
    let paragraph = Paragraph::new(text)
        .block(block);
    frame.render_widget(paragraph, popup);
}

fn render_quit_confirm(frame: &mut Frame, area: Rect) {
    let text = " Unsaved changes — quit anyway? (y/n) ";
    let width = (text.chars().count() as u16 + 2).min(area.width);
    let height = 3.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::WARNING_FG))
        .style(Style::default().fg(theme::WARNING_FG).bg(theme::WARNING_BG));
    let paragraph = Paragraph::new(text)
        .alignment(Alignment::Center)
        .block(block);
    frame.render_widget(paragraph, popup);
}

fn render_dialog(frame: &mut Frame, area: Rect, dialog: &Dialog) {
    let (title, content_text) = match dialog {
        Dialog::Normalize { buffer } => {
            ("Normalize", format!(" Target peak (dBFS): {}_ ", buffer))
        }
        Dialog::Gain { buffer, tanh_clip } => {
            let tanh = if *tanh_clip { "ON" } else { "OFF" };
            ("Gain", format!(" Gain (dB): {}_  Tanh: {} (Tab) ", buffer, tanh))
        }
    };
    let width = (content_text.chars().count() as u16 + 2).min(area.width);
    let height = 3.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + area.height.saturating_sub(height + 1),
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().fg(theme::TEXT).bg(theme::SURFACE0));
    let paragraph = Paragraph::new(content_text)
        .block(block);
    frame.render_widget(paragraph, popup);
}
