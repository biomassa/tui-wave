use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::audio::engine::AudioEngine;
use crate::commands::cut::cut_command;
use crate::commands::delete::delete_command;
use crate::commands::paste::paste_command;
use crate::model::clipboard::Clipboard;
use crate::model::document::Document;
use crate::model::history::History;
use crate::model::io::save_wav;
use crate::model::selection::Selection;

use super::keymap::{map_key, Action};
use super::layout::split_chrome;
use super::menu::MenuBar;
use super::terminal::Tui;
use super::toolbar::Toolbar;
use super::viewport::Viewport;
use super::waveform_cache::WaveformCache;
use super::widgets::statusbar::StatusBar;
use super::widgets::waveform::WaveformWidget;

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
}

impl App {
    pub fn new(document: Option<Document>) -> Self {
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
        }
    }

    /// Highest peak across all channels — used to auto-fit the initial vertical zoom.
    fn waveform_peak(&self) -> f32 {
        self.waveform_caches
            .iter()
            .fold(0.0f32, |p, c| p.max(c.peak()))
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

    fn sync_playhead_from_audio(&mut self) {
        let (Some(document), Some(audio)) = (self.document.as_mut(), self.audio.as_ref()) else {
            return;
        };
        if audio.playing.load(Ordering::Relaxed) {
            document.playhead = audio.position.load(Ordering::Relaxed);
            if let Some(viewport) = self.viewport.as_mut() {
                viewport.ensure_visible(document.playhead, self.content_width);
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }

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

        let area = self.waveform_area;
        if mouse.column < area.x
            || mouse.column >= area.x + area.width
            || mouse.row < area.y
            || mouse.row >= area.y + area.height
        {
            return;
        }
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
        document.playhead = target.min(total_len - 1);

        if let Some(audio) = &self.audio {
            if audio.is_playing() {
                audio.seek(document.playhead);
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
        ) {
            self.handle_edit_action(action);
            return;
        }

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
        let is_extend = matches!(
            action,
            Action::ExtendSelectionLeft | Action::ExtendSelectionRight
        );
        let anchor = if is_extend {
            Some(document.selection.map(|s| s.start).unwrap_or(document.playhead))
        } else {
            None
        };

        match action {
            Action::Quit
            | Action::TogglePlayback
            | Action::Stop
            | Action::Cut
            | Action::Copy
            | Action::Paste
            | Action::Undo
            | Action::Redo
            | Action::Save => unreachable!(),
            Action::MoveCursorLeft => {
                document.playhead = document.playhead.saturating_sub(column_step.max(1));
            }
            Action::MoveCursorRight => {
                document.playhead = (document.playhead + column_step.max(1)).min(total_len - 1);
            }
            Action::MoveCursorLeftFine => {
                document.playhead = document.playhead.saturating_sub(1);
            }
            Action::MoveCursorRightFine => {
                document.playhead = (document.playhead + 1).min(total_len - 1);
            }
            Action::ExtendSelectionLeft => {
                document.playhead = document.playhead.saturating_sub(column_step.max(1));
            }
            Action::ExtendSelectionRight => {
                document.playhead = (document.playhead + column_step.max(1)).min(total_len - 1);
            }
            Action::JumpStart => document.playhead = 0,
            Action::JumpEnd => document.playhead = total_len - 1,
            Action::PageBack => {
                document.playhead = document.playhead.saturating_sub(span.max(1));
            }
            Action::PageForward => {
                document.playhead = (document.playhead + span.max(1)).min(total_len - 1);
            }
            Action::ZoomIn => viewport.zoom_in(document.playhead, width),
            Action::ZoomOut => viewport.zoom_out(document.playhead, width),
            Action::ZoomInVertical => viewport.zoom_in_vertical(),
            Action::ZoomOutVertical => viewport.zoom_out_vertical(),
        }

        document.selection = match anchor {
            Some(anchor) => Some(Selection {
                start: anchor,
                end: document.playhead,
            }),
            None => None,
        };

        viewport.ensure_visible(document.playhead, width);

        // Nav/zoom actions can move the cursor while audio is mid-playback (e.g. scrubbing
        // with arrow keys) — keep the audio thread's position in sync rather than letting
        // it silently keep playing from the old spot.
        if let Some(audio) = &self.audio {
            if audio.is_playing() {
                audio.seek(document.playhead);
            }
        }
    }

    fn handle_edit_action(&mut self, action: Action) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let mutates_samples = matches!(
            action,
            Action::Cut | Action::Paste | Action::Undo | Action::Redo
        );
        match action {
            Action::Cut => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    if start < end {
                        self.clipboard.set(document.slice(start..end), document.sample_rate);
                        self.history.apply(cut_command(start..end), document);
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
                    let at = document.playhead;
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
                    }
                }
            }
            _ => unreachable!(),
        }

        if let Some(viewport) = self.viewport.as_mut() {
            viewport.ensure_visible(document.playhead, self.content_width);
        }
        if mutates_samples {
            if let Some(audio) = &self.audio {
                audio.reload(document.channels.clone());
            }
            self.rebuild_waveform_caches();
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
                } else {
                    audio.play(document.playhead);
                }
            }
            Action::Stop => {
                audio.stop();
                if let Some(document) = self.document.as_mut() {
                    document.playhead = 0;
                }
            }
            _ => unreachable!(),
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area: Rect = frame.area();
        let chrome = split_chrome(area);

        let Some(document) = &self.document else {
            let block = Block::default()
                .title(" tui-wave ")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White));
            let text = Paragraph::new("No file loaded — usage: tui-wave <file.wav>")
                .alignment(Alignment::Center)
                .block(block);
            frame.render_widget(text, chrome.content);
            self.toolbar.render(frame, chrome.toolbar);
            self.menu.render(frame, chrome.menu);
            return;
        };

        let title = format!(
            " tui-wave — {}{} ",
            document
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".to_string()),
            if document.dirty { " *" } else { "" }
        );
        let outer = Block::default().title(title).borders(Borders::ALL);
        let inner = outer.inner(chrome.content);
        frame.render_widget(outer, chrome.content);

        let [waveform_area, status_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

        self.content_width = waveform_area.width;
        self.waveform_area = waveform_area;
        let peak = self.waveform_peak();
        let viewport = self.viewport.get_or_insert_with(|| {
            let mut v = Viewport::fit_to_width(document.len_samples(), waveform_area.width as usize);
            // Auto-fit vertical zoom to the file's actual peak so a quiet recording doesn't
            // render using only a sliver of the available height.
            if peak > 0.0001 {
                v.set_amplitude_scale(0.95 / peak);
            }
            v
        });

        let channel_count = document.channel_count().max(1);
        let chunks =
            Layout::vertical(vec![Constraint::Fill(1); channel_count]).split(waveform_area);
        let selection = document.selection.map(|s| s.normalized());

        for (i, channel_area) in chunks.iter().enumerate() {
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
            };
            frame.render_widget(widget, *channel_area);
        }

        frame.render_widget(StatusBar { document, viewport }, status_area);

        self.toolbar.render(frame, chrome.toolbar);
        self.menu.render(frame, chrome.menu);

        if self.quit_confirm {
            render_quit_confirm(frame, area);
        }
    }
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
        .style(Style::default().fg(Color::Black).bg(Color::Yellow));
    let paragraph = Paragraph::new(text)
        .alignment(Alignment::Center)
        .block(block);
    frame.render_widget(paragraph, popup);
}
