use std::path::PathBuf;
use std::time::{Duration, Instant};

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
use crate::commands::fade::{fade_command, FadeCurve};
use crate::commands::gain::gain_command;
use crate::commands::paste::paste_command;
use crate::commands::normalize::normalize_command;
use crate::commands::resample::resample_command;
use crate::commands::reverse::reverse_command;
use crate::commands::trim::trim_command;
use crate::model::clipboard::Clipboard;
use crate::model::document::{Document, Marker};
use crate::model::history::History;
use crate::model::io::{save_wav, save_wav_with, BitDepth};
use crate::model::selection::Selection;

use super::buffer_panel::BufferPanel;
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
    FadeIn { curve: FadeCurve },
    FadeOut { curve: FadeCurve },
    Resample { buffer: String, current_rate: u32 },
    RenameMarker { index: usize, buffer: String },
}

pub struct App {
    pub should_quit: bool,
    /// All open documents (buffers). Index 0 is always the first file loaded; subsequent
    /// entries are created by "Copy to New" or loading additional files.
    pub documents: Vec<Document>,
    /// Index into `documents` for the currently-active buffer.
    pub active_document: usize,
    pub viewport: Option<Viewport>,
    pub audio: Option<AudioEngine>,
    /// Sample rate the current audio engine was built with. The engine captures the rate at
    /// construction, so an operation that changes `Document.sample_rate` (resample, and its
    /// undo/redo) must rebuild the engine rather than just `reload` it.
    audio_sample_rate: Option<u32>,
    /// One undo/redo stack per open document, kept index-parallel to `documents`. Undo
    /// must never cross buffers — each `Command` stores sample data from the document it
    /// was applied to, so replaying it against a different document would corrupt it.
    pub histories: Vec<History>,
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
    /// Index of the marker currently being dragged with the mouse, if any.
    dragging_marker: Option<usize>,
    /// Rendered marker-label rects (label box + marker index) for mouse hit-testing.
    marker_label_rects: Vec<(Rect, usize)>,
    /// Time/cell of the last left mouse-down, used to detect double-clicks.
    last_click: Option<(Instant, u16, u16)>,
    /// File panel on the left showing WAV files in the current directory.
    pub file_panel: FilePanel,
    /// Buffer panel showing all open documents.
    pub buffer_panel: BufferPanel,
    /// When true, the user is typing a Save-As path in a prompt overlay.
    pub save_as_active: bool,
    /// Buffer for the Save-As path being typed.
    pub save_as_path: String,
    /// Output bit depth for the pending Save As (Tab cycles it in the prompt).
    pub save_as_depth: BitDepth,
    /// Whether to dither the pending Save As (Ctrl+D toggles; only meaningful for int depths).
    pub save_as_dither: bool,
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

        let documents = match document {
            Some(doc) => vec![doc],
            None => Vec::new(),
        };
        let audio = documents.first()
            .and_then(|doc| AudioEngine::try_new(doc.channels.clone(), doc.sample_rate));
        let audio_sample_rate = documents.first().map(|doc| doc.sample_rate);
        let waveform_caches = documents.first()
            .map(|doc| doc.channels.iter().map(|c| WaveformCache::build(c)).collect())
            .unwrap_or_default();
        let histories = documents.iter().map(|_| History::new()).collect();
        Self {
            should_quit: false,
            documents,
            active_document: 0,
            viewport: None,
            audio,
            audio_sample_rate,
            histories,
            clipboard: Clipboard::default(),
            menu: MenuBar::new(),
            toolbar: Toolbar::new(),
            waveform_caches,
            content_width: 1,
            waveform_area: Rect::default(),
            quit_confirm: false,
            mouse_down_anchor: None,
            dragging_marker: None,
            marker_label_rects: Vec::new(),
            last_click: None,
            file_panel,
            buffer_panel: BufferPanel::new(),
            save_as_active: false,
            save_as_path: String::new(),
            save_as_depth: BitDepth::Float32,
            save_as_dither: false,
            snap_to_zero: true,
            loop_playback: false,
            dialog: None,
            playhead_position: None,
        }
    }

    fn active_doc(&self) -> Option<&Document> {
        self.documents.get(self.active_document)
    }

    fn active_doc_mut(&mut self) -> Option<&mut Document> {
        self.documents.get_mut(self.active_document)
    }

    /// Pushes a freshly-opened document and its (empty) history, keeping the two vecs
    /// index-parallel, and makes it the active buffer.
    fn push_document(&mut self, document: Document) {
        self.documents.push(document);
        self.histories.push(History::new());
        self.active_document = self.documents.len() - 1;
    }

    fn buffer_names(&self) -> Vec<String> {
        self.documents.iter().enumerate().map(|(i, doc)| {
            let prefix = if doc.dirty { "*" } else { "" };
            let name = match doc.path.as_ref() {
                Some(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "untitled".to_string()),
                None => format!("_UNSAVED_{:03}", i + 1),
            };
            format!("{}{}", prefix, name)
        }).collect()
    }

    fn switch_to_buffer(&mut self, index: usize) {
        if index >= self.documents.len() || index == self.active_document {
            return;
        }
        self.active_document = index;
        if self.active_doc().is_some() {
            self.rebuild_audio();
            self.rebuild_waveform_caches();
            self.viewport = None;
        }
    }

    /// Returns the playback loop range: the current selection if one exists, or the full
    /// document if nothing is selected. Returns `None` when loop playback is disabled.
    fn loop_range(&self) -> Option<(usize, usize)> {
        if !self.loop_playback {
            return None;
        }
        self.active_doc().map(|doc| {
            doc.selection
                .map(|sel| sel.normalized())
                .unwrap_or((0, doc.len_samples()))
        })
    }

    /// Highest peak within the current visible window. Computed from the precomputed cache
    /// so it's cheap enough to call every frame.
    fn visible_peak(&self) -> f32 {
        visible_peak_raw(
            self.active_doc(),
            self.viewport.as_ref(),
            &self.waveform_caches,
            self.content_width,
        )
    }

    fn rebuild_waveform_caches(&mut self) {
        self.waveform_caches = self
            .active_doc()
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
            let handled = match key.code {
                KeyCode::Up => { self.file_panel.move_up(); true }
                KeyCode::Down => { self.file_panel.move_down(); true }
                KeyCode::Home => { self.file_panel.move_top(); true }
                KeyCode::End => { self.file_panel.move_bottom(); true }
                KeyCode::Enter => { self.open_selected_file(); true }
                KeyCode::Char('/') => {
                    self.file_panel.filtering = true;
                    self.file_panel.filter.clear();
                    true
                }
                KeyCode::Tab => {
                    self.file_panel.focused = false;
                    self.buffer_panel.focused = true;
                    true
                }
                KeyCode::Esc => { self.file_panel.focused = false; true }
                _ => false,
            };
            if handled {
                return;
            }
        }
        // Buffer panel keyboard focus
        if self.buffer_panel.focused {
            let handled = match key.code {
                KeyCode::Up => {
                    self.switch_to_buffer(self.active_document.saturating_sub(1));
                    true
                }
                KeyCode::Down => {
                    let max = self.documents.len().saturating_sub(1);
                    self.switch_to_buffer((self.active_document + 1).min(max));
                    true
                }
                KeyCode::Tab => { self.buffer_panel.focused = false; true }
                KeyCode::Esc => { self.buffer_panel.focused = false; true }
                _ => false,
            };
            if handled {
                return;
            }
        }
        // Tab when nothing is focused → focus the file panel
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
            // Save every dirty buffer that has a path, then quit. Buffers without a path are
            // left unsaved (Save All can't choose names) — but we still quit.
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.save_all();
                self.should_quit = true;
            }
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
                let depth = self.save_as_depth;
                let dither = self.save_as_dither && depth.supports_dither();
                if let Some(document) = self.active_doc_mut() {
                    if save_wav_with(document, &path, depth, dither).is_ok() {
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
            // Tab cycles bit depth; Ctrl+D toggles dither (Ctrl keeps it out of the path text).
            KeyCode::Tab => {
                self.save_as_depth = self.save_as_depth.next();
            }
            KeyCode::Char('d') | KeyCode::Char('D') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_as_dither = !self.save_as_dither;
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
                match self.dialog.take() {
                    Some(Dialog::Normalize { buffer }) => {
                        let db = buffer.parse::<f32>().unwrap_or(-1.0).min(0.0);
                        self.apply_normalize(db);
                    }
                    Some(Dialog::Gain { buffer, tanh_clip }) => {
                        let db = buffer.parse::<f32>().unwrap_or(0.0);
                        self.apply_gain(db, tanh_clip);
                    }
                    Some(Dialog::FadeIn { curve }) => self.apply_fade(true, 100.0, curve),
                    Some(Dialog::FadeOut { curve }) => self.apply_fade(false, 100.0, curve),
                    Some(Dialog::Resample { buffer, current_rate }) => {
                        let rate = buffer.trim().parse::<u32>().unwrap_or(current_rate);
                        self.apply_resample(rate);
                    }
                    Some(Dialog::RenameMarker { index, buffer }) => {
                        if let Some(doc) = self.documents.get_mut(self.active_document) {
                            if let Some(marker) = doc.markers.get_mut(index) {
                                marker.label = buffer;
                                doc.dirty = true;
                                if let Some(path) = doc.path.clone() {
                                    self.file_panel.mark_dirty(&path, true);
                                }
                            }
                        }
                    }
                    None => {}
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
                        Dialog::Resample { buffer, .. } => { buffer.pop(); }
                        Dialog::RenameMarker { buffer, .. } => { buffer.pop(); }
                        Dialog::FadeIn { .. } => {}
                        Dialog::FadeOut { .. } => {}
                    }
                }
            }
            KeyCode::Tab => {
                if let Some(dialog) = self.dialog.as_mut() {
                    match dialog {
                        Dialog::Gain { tanh_clip, .. } => *tanh_clip = !*tanh_clip,
                        Dialog::FadeIn { curve, .. } => *curve = curve.next(),
                        Dialog::FadeOut { curve, .. } => *curve = curve.next(),
                        _ => {}
                    }
                }
            }
            // Marker rename is free text — accept any printable character.
            KeyCode::Char(c)
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && matches!(self.dialog, Some(Dialog::RenameMarker { .. })) =>
            {
                if let Some(Dialog::RenameMarker { buffer, .. }) = self.dialog.as_mut() {
                    buffer.push(c);
                }
            }
            KeyCode::Char(c) if c == '-' || c == '.' || c.is_ascii_digit() => {
                if let Some(dialog) = self.dialog.as_mut() {
                    match dialog {
                        Dialog::Normalize { buffer } => {
                            if *buffer == "-1.0" { buffer.clear(); }
                            buffer.push(c);
                        }
                        Dialog::Gain { buffer, .. } => {
                            if *buffer == "0.0" { buffer.clear(); }
                            buffer.push(c);
                        }
                        // Sample rate is a positive integer — accept digits only.
                        Dialog::Resample { buffer, .. } if c.is_ascii_digit() => {
                            buffer.push(c);
                        }
                        Dialog::Resample { .. } => {}
                        Dialog::RenameMarker { .. } => {} // handled by the free-text arm above
                        Dialog::FadeIn { .. } => {}
                        Dialog::FadeOut { .. } => {},
                    }
                }
            }
            _ => {}
        }
    }

    /// The sample range an operation should act on: the current selection if one exists,
    /// otherwise the whole document. Optionally snapped to zero crossings. Returns `None`
    /// for an empty document or a degenerate (empty) range.
    fn operation_range(&self, idx: usize, snap: bool) -> Option<(usize, usize)> {
        let doc = self.documents.get(idx)?;
        let total_len = doc.len_samples();
        if total_len == 0 {
            return None;
        }
        let (start, end) = doc
            .selection
            .map(|sel| sel.normalized())
            .unwrap_or((0, total_len));
        let (start, end) = if snap {
            doc.snap_range_to_zero_crossing(start, end)
        } else {
            (start, end)
        };
        (start < end).then_some((start, end))
    }

    /// Shared tail for every operation that mutates sample data on `idx` (which is always
    /// the active document): mark the file dirty, hand the new buffer to the audio engine,
    /// rebuild the waveform caches, and re-fit auto vertical zoom if it's on.
    fn after_sample_mutation(&mut self, idx: usize) {
        if self.documents[idx].dirty {
            if let Some(path) = self.documents[idx].path.clone() {
                self.file_panel.mark_dirty(&path, true);
            }
        }
        // A rate change (resample, or its undo/redo) needs a fresh engine since the rate is
        // captured at construction; otherwise a cheap data reload is enough.
        if self.audio_sample_rate != Some(self.documents[idx].sample_rate) {
            self.rebuild_audio();
        } else if let Some(audio) = &self.audio {
            audio.reload(self.documents[idx].channels.clone());
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

    fn apply_normalize(&mut self, target_db: f32) {
        let idx = self.active_document;
        let Some((start, end)) = self.operation_range(idx, self.snap_to_zero) else { return };
        self.histories[idx].apply(normalize_command(start, end, target_db), &mut self.documents[idx]);
        self.after_sample_mutation(idx);
    }

    fn apply_gain(&mut self, gain_db: f32, tanh_clip: bool) {
        let idx = self.active_document;
        let Some((start, end)) = self.operation_range(idx, self.snap_to_zero) else { return };
        self.histories[idx].apply(gain_command(start, end, gain_db, tanh_clip), &mut self.documents[idx]);
        self.after_sample_mutation(idx);
    }

    /// Resamples the whole active document to `target_rate`. The sample count changes
    /// drastically, so the viewport is dropped to refit; `after_sample_mutation` notices the
    /// rate change and rebuilds the audio engine.
    fn apply_resample(&mut self, target_rate: u32) {
        let idx = self.active_document;
        let Some(doc) = self.documents.get(idx) else { return };
        if target_rate == 0 || target_rate == doc.sample_rate || doc.len_samples() == 0 {
            return;
        }
        self.histories[idx].apply(resample_command(target_rate), &mut self.documents[idx]);
        self.viewport = None;
        self.after_sample_mutation(idx);
    }

    fn open_selected_file(&mut self) {
        let Some(path) = self.file_panel.selected_path() else {
            return;
        };
        self.load_file(path);
    }

    fn apply_fade(&mut self, fade_in: bool, pct: f32, curve: FadeCurve) {
        let idx = self.active_document;
        // Fade deliberately does not snap: the curve is shaped to start/end at zero anyway,
        // so a hard zero crossing at the boundary buys nothing.
        let Some((start, end)) = self.operation_range(idx, false) else { return };
        let fade_samples = ((end - start) as f32 * pct / 100.0).round() as usize;
        let fade_samples = fade_samples.max(1).min(end - start);
        let (fade_start, fade_end) = if fade_in {
            (start, start + fade_samples)
        } else {
            (end - fade_samples, end)
        };
        if fade_start >= fade_end || fade_end > end { return; }
        self.histories[idx].apply(fade_command(fade_start, fade_end, fade_in, curve), &mut self.documents[idx]);
        self.after_sample_mutation(idx);
    }

    fn load_file(&mut self, path: PathBuf) {
        if let Some(audio) = self.audio.take() {
            drop(audio);
        }
        match crate::model::io::load_wav(&path) {
            Ok(mut document) => {
                self.file_panel.focused = false;
                self.file_panel.filtering = false;
                self.file_panel.filter.clear();

                document.dirty = false;
                // Check if this path is already open
                if let Some(pos) = self.documents.iter().position(|d| d.path == Some(path.clone())) {
                    self.active_document = pos;
                } else {
                    self.push_document(document);
                }
                self.viewport = None;
                self.rebuild_audio();
                self.rebuild_waveform_caches();
            }
            Err(e) => {
                let _ = e;
            }
        }
    }

    /// Saves every dirty document that already has a path. Documents that were never
    /// saved (no path) are skipped — Save All can't choose a filename for each; those still
    /// need an explicit Save As.
    fn save_all(&mut self) {
        for document in &mut self.documents {
            if !document.dirty {
                continue;
            }
            if let Some(path) = document.path.clone() {
                if save_wav(document, &path).is_ok() {
                    document.dirty = false;
                    self.file_panel.mark_dirty(&path, false);
                }
            }
        }
    }

    fn rebuild_audio(&mut self) {
        if let Some(document) = self.active_doc() {
            let rate = document.sample_rate;
            self.audio = AudioEngine::try_new(document.channels.clone(), rate);
            self.audio_sample_rate = Some(rate);
        } else {
            self.audio = None;
            self.audio_sample_rate = None;
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

        // Buffer panel: click to switch active buffer.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(idx) = self.buffer_panel.hit_test(mouse.column, mouse.row) {
                self.switch_to_buffer(idx);
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

        // Marker interaction (drag a line to move it, double-click a label to rename) takes
        // priority over selection when the press lands on a marker.
        if self.try_handle_marker_mouse(mouse) {
            return;
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
        let loop_range = if self.loop_playback {
            self.active_doc().map(|d| {
                d.selection.map(|sel| sel.normalized()).unwrap_or((0, d.len_samples()))
            })
        } else {
            None
        };

        let idx = self.active_document;
        let Some(viewport) = self.viewport.as_ref() else { return };
        let Some(document) = self.documents.get_mut(idx) else { return };
        let total_len = document.len_samples();
        if total_len == 0 {
            return;
        }
        let col = (mouse.column - area.x) as f64;
        let target =
            (viewport.scroll_offset as f64 + col * viewport.samples_per_column) as usize;
        let target = target.min(total_len - 1);
        let snap = self.snap_to_zero;
        let target = if snap { document.snap_to_zero_crossing(target) } else { target };

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if mouse.modifiers.contains(KeyModifiers::CONTROL) {
                    let t = if snap { document.snap_to_zero_crossing(target) } else { target };
                    if let Some(sel) = document.selection {
                        let (sel_start, sel_end) = sel.normalized();
                        if t < sel_start {
                            document.selection = Some(Selection { start: t, end: sel_end });
                            document.cursor = t;
                        } else if t > sel_end {
                            document.selection = Some(Selection { start: sel_start, end: t });
                            document.cursor = sel_start;
                        }
                    } else {
                        let anchor = document.cursor;
                        document.selection = Some(Selection { start: anchor, end: t });
                        document.cursor = anchor.min(t);
                    }
                } else {
                    document.selection = None;
                    let anchor = if snap { document.snap_to_zero_crossing(target) } else { target };
                    document.cursor = anchor;
                    self.mouse_down_anchor = Some(anchor);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(anchor) = self.mouse_down_anchor {
                    let start = anchor.min(target);
                    document.cursor = start;
                    document.selection = Some(Selection {
                        start: anchor,
                        end: target,
                    });
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(anchor) = self.mouse_down_anchor {
                    if anchor != target {
                        let start = anchor.min(target);
                        document.cursor = start;
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

    /// Handles mouse events that land on a marker: double-click a label to rename, or
    /// press-and-drag a marker to move it. Returns `true` if the event was consumed (so the
    /// caller skips the normal seek/select handling).
    fn try_handle_marker_mouse(&mut self, mouse: MouseEvent) -> bool {
        let area = self.waveform_area;
        let in_area = mouse.column >= area.x
            && mouse.column < area.x + area.width
            && mouse.row >= area.y
            && mouse.row < area.y + area.height;
        let idx = self.active_document;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Ctrl+click is reserved for selection extension.
                if !in_area || mouse.modifiers.contains(KeyModifiers::CONTROL) {
                    return false;
                }
                let hit = self
                    .marker_label_rects
                    .iter()
                    .find(|(r, _)| {
                        r.x <= mouse.column
                            && mouse.column < r.x + r.width
                            && r.y <= mouse.row
                            && mouse.row < r.y + r.height
                    })
                    .map(|(_, mi)| *mi);
                let now = Instant::now();
                let is_double = self.last_click.is_some_and(|(t, x, y)| {
                    now.duration_since(t) < Duration::from_millis(400)
                        && x.abs_diff(mouse.column) <= 1
                        && y == mouse.row
                });
                self.last_click = Some((now, mouse.column, mouse.row));
                let Some(mi) = hit else { return false };
                if is_double {
                    if let Some(label) = self
                        .documents
                        .get(idx)
                        .and_then(|d| d.markers.get(mi))
                        .map(|m| m.label.clone())
                    {
                        self.dialog = Some(Dialog::RenameMarker { index: mi, buffer: label });
                    }
                    self.last_click = None;
                    self.dragging_marker = None;
                } else {
                    self.dragging_marker = Some(mi);
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(mi) = self.dragging_marker else { return false };
                let Some(viewport) = self.viewport.as_ref() else { return true };
                let scroll = viewport.scroll_offset;
                let spc = viewport.samples_per_column;
                let Some(doc) = self.documents.get_mut(idx) else { return true };
                let total = doc.len_samples();
                if total == 0 {
                    return true;
                }
                let colx = mouse.column.clamp(area.x, area.x + area.width - 1);
                let col = (colx - area.x) as f64;
                let pos = ((scroll as f64 + col * spc) as usize).min(total - 1);
                let mut path = None;
                if let Some(m) = doc.markers.get_mut(mi) {
                    m.position = pos;
                    doc.dirty = true;
                    path = doc.path.clone();
                }
                if let Some(p) = path {
                    self.file_panel.mark_dirty(&p, true);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.dragging_marker.take().is_some() {
                    if let Some(doc) = self.documents.get_mut(idx) {
                        doc.markers.sort_by_key(|m| m.position);
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn handle_action(&mut self, action: Action) {
        if action == Action::Quit {
            // Warn if *any* open buffer is dirty, not just the active one.
            if self.documents.iter().any(|doc| doc.dirty) {
                self.quit_confirm = true;
            } else {
                self.should_quit = true;
            }
            return;
        }

        if action == Action::TogglePlayback {
            self.handle_playback_action(action);
            return;
        }

        if action == Action::SaveAll {
            self.save_all();
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
                | Action::Reverse
                | Action::Delete
                | Action::Trim
        ) {
            self.handle_edit_action(action);
            return;
        }

        if action == Action::ClearSelection {
            if let Some(document) = self.active_doc_mut() {
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

        if matches!(
            action,
            Action::InsertMarker
                | Action::DeleteMarker
                | Action::JumpPrevMarker
                | Action::JumpNextMarker
        ) {
            self.handle_marker_action(action);
            return;
        }

        if action == Action::Normalize {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::Normalize { buffer: String::from("-1.0") });
            }
            return;
        }

        if action == Action::Gain {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::Gain { buffer: String::from("0.0"), tanh_clip: false });
            }
            return;
        }

        if action == Action::Resample {
            if let Some(rate) = self.active_doc().map(|d| d.sample_rate) {
                self.dialog = Some(Dialog::Resample { buffer: String::new(), current_rate: rate });
            }
            return;
        }

        if action == Action::FadeIn {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::FadeIn { curve: FadeCurve::Exp });
            }
            return;
        }

        if action == Action::FadeOut {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::FadeOut { curve: FadeCurve::Exp });
            }
            return;
        }

        if action == Action::CopyToNew {
            let data = self.active_doc().and_then(|d| {
                d.selection.map(|sel| {
                    let (start, end) = sel.normalized();
                    d.slice(start..end)
                })
            });
            if let Some(samples) = data {
                let sample_rate = self.active_doc().map(|d| d.sample_rate).unwrap_or(44100);
                let new_doc = Document {
                    channels: samples,
                    sample_rate,
                    selection: None,
                    cursor: 0,
                    dirty: false,
                    path: None,
                    markers: Vec::new(),
                    bext: None,
                };
                self.push_document(new_doc);
                self.viewport = None;
                self.rebuild_audio();
                self.rebuild_waveform_caches();
            }
            return;
        }

        let idx = self.active_document;
        let Some(viewport) = self.viewport.as_mut() else { return };
        let Some(document) = self.documents.get_mut(idx) else { return };
        let total_len = document.len_samples();
        if total_len == 0 {
            return;
        }
        let width = self.content_width;
        let column_step = viewport.samples_per_column.max(1.0) as usize;
        let fine_step = (column_step / 4).max(1);
        let span = viewport.span(width);
        let loop_range = if self.loop_playback {
            Some(document.selection.map(|sel| sel.normalized()).unwrap_or((0, total_len)))
        } else {
            None
        };
        let old_cursor = document.cursor;
        match action {
            Action::Quit
            | Action::TogglePlayback
            | Action::Cut
            | Action::Copy
            | Action::Paste
            | Action::Undo
            | Action::Redo
            | Action::Save
            | Action::Reverse
            | Action::Normalize
            | Action::Resample
            | Action::Delete
            | Action::ToggleAutoVerticalZoom
            | Action::ToggleZeroSnap
            | Action::ToggleLoop
            | Action::ClearSelection
            | Action::SaveAs
            | Action::SaveAll
            | Action::Gain
            | Action::CopyToNew
            | Action::FadeIn
            | Action::FadeOut
            | Action::InsertMarker
            | Action::DeleteMarker
            | Action::JumpPrevMarker
            | Action::JumpNextMarker
            | Action::Trim => unreachable!(),
            // Cursor movement is identical whether or not it extends a selection; the
            // selection side-effect is applied in the second match below.
            Action::MoveCursorLeft | Action::ExtendSelectionLeft => {
                document.cursor = document.cursor.saturating_sub(column_step.max(1));
            }
            Action::MoveCursorRight | Action::ExtendSelectionRight => {
                document.cursor = (document.cursor + column_step.max(1)).min(total_len - 1);
            }
            Action::MoveCursorLeftFine | Action::ExtendSelectionLeftFine => {
                document.cursor = document.cursor.saturating_sub(fine_step);
            }
            Action::MoveCursorRightFine | Action::ExtendSelectionRightFine => {
                document.cursor = (document.cursor + fine_step).min(total_len - 1);
            }
            Action::JumpStart | Action::ExtendSelectionToStart => document.cursor = 0,
            Action::JumpEnd | Action::ExtendSelectionToEnd => document.cursor = total_len - 1,
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

        let snap = self.snap_to_zero;
        match action {
            // Extend in either direction with the anchor held fixed (see Selection::extended):
            // the active edge follows the cursor, so reversing direction shrinks rather than
            // flips the selection.
            Action::ExtendSelectionLeft
            | Action::ExtendSelectionLeftFine
            | Action::ExtendSelectionRight
            | Action::ExtendSelectionRightFine => {
                let cursor = if snap { document.snap_to_zero_crossing(document.cursor) } else { document.cursor };
                document.selection = Some(Selection::extended(document.selection, old_cursor, cursor));
                document.cursor = cursor;
            }
            Action::ExtendSelectionToStart | Action::ExtendSelectionToEnd => {
                // cursor is already at 0 / end (the active edge); keep it there.
                document.selection = Some(Selection::extended(document.selection, old_cursor, document.cursor));
            }
            // Plain cursor moves, jumps, paging and zoom leave the selection untouched.
            _ => {}
        }

        viewport.ensure_visible(document.cursor, width);

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
        let idx = self.active_document;
        if idx >= self.documents.len() {
            return;
        }
        // Preread self fields before the mutable document borrow.
        let mutates_samples = matches!(
            action,
            Action::Cut | Action::Delete | Action::Paste | Action::Undo | Action::Redo | Action::Reverse | Action::Trim
        );
        let snap = self.snap_to_zero;
        let content_width = self.content_width;
        let has_selection = self.documents[idx].selection.is_some();

        match action {
            Action::Save => {
                let doc = &self.documents[idx];
                if doc.path.is_some() {
                    // Has a path — saved through the mutable path below.
                } else {
                    return self.handle_action(Action::SaveAs);
                }
            }
            _ => {}
        }

        let document = self.documents.get_mut(idx).unwrap();

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
                        self.histories[idx].apply(cut_command(start..end), document);
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
                        self.histories[idx].apply(delete_command(start..end), document);
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
                    if has_selection {
                        if let Some(sel) = document.selection {
                            let (start, end) = sel.normalized();
                            if start < end {
                                self.histories[idx].apply(delete_command(start..end), document);
                            }
                        }
                    }
                    let at = document.cursor;
                    let data = self.clipboard.channels.clone();
                    self.histories[idx].apply(paste_command(at, data), document);
                }
            }
            Action::Undo => {
                self.histories[idx].undo(document);
            }
            Action::Redo => {
                self.histories[idx].redo(document);
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
            Action::Reverse => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    let (start, end) = if snap {
                        document.snap_range_to_zero_crossing(start, end)
                    } else {
                        (start, end)
                    };
                    if start < end {
                        self.histories[idx].apply(reverse_command(start, end), document);
                    }
                }
            }
            Action::Trim => {
                if let Some(sel) = document.selection {
                    let (start, end) = sel.normalized();
                    if start < end {
                        self.histories[idx].apply(trim_command(start, end), document);
                        self.viewport = None;
                    }
                }
            }
            _ => unreachable!(),
        }

        let cursor = document.cursor;
        if let Some(viewport) = self.viewport.as_mut() {
            viewport.ensure_visible(cursor, content_width);
        }
        if mutates_samples {
            self.after_sample_mutation(idx);
        }
    }

    /// Insert/delete a marker at/near the cursor, or jump the cursor to an adjacent marker.
    /// Markers are document metadata, not part of the sample-edit undo history.
    fn handle_marker_action(&mut self, action: Action) {
        let idx = self.active_document;
        if idx >= self.documents.len() {
            return;
        }
        let mut moved_cursor = false;
        let mut changed = false;
        {
            let doc = &mut self.documents[idx];
            match action {
                Action::InsertMarker => {
                    let pos = doc.cursor;
                    if !doc.markers.iter().any(|m| m.position == pos) {
                        let n = doc.markers.len() + 1;
                        doc.markers.push(Marker { position: pos, label: format!("Marker {n}") });
                        doc.markers.sort_by_key(|m| m.position);
                        doc.dirty = true;
                        changed = true;
                    }
                }
                Action::DeleteMarker => {
                    if let Some(i) = nearest_marker(&doc.markers, doc.cursor) {
                        doc.markers.remove(i);
                        doc.dirty = true;
                        changed = true;
                    }
                }
                Action::JumpPrevMarker => {
                    if let Some(p) = doc
                        .markers
                        .iter()
                        .rev()
                        .find(|m| m.position < doc.cursor)
                        .map(|m| m.position)
                    {
                        doc.cursor = p;
                        moved_cursor = true;
                    }
                }
                Action::JumpNextMarker => {
                    if let Some(p) = doc
                        .markers
                        .iter()
                        .find(|m| m.position > doc.cursor)
                        .map(|m| m.position)
                    {
                        doc.cursor = p;
                        moved_cursor = true;
                    }
                }
                _ => {}
            }
        }
        if changed {
            if let Some(path) = self.documents[idx].path.clone() {
                self.file_panel.mark_dirty(&path, true);
            }
        }
        if moved_cursor {
            let cursor = self.documents[idx].cursor;
            if let Some(viewport) = self.viewport.as_mut() {
                viewport.ensure_visible(cursor, self.content_width);
            }
            if let Some(audio) = &self.audio {
                if audio.is_playing() {
                    audio.seek(cursor);
                }
            }
        }
    }

    fn handle_playback_action(&mut self, _action: Action) {
        let Some(document) = self.active_doc() else {
            return;
        };
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        // Space is the only transport command: play from the cursor, or pause if playing.
        if audio.is_playing() {
            audio.pause();
        } else if let Some((ls, le)) = self.loop_range() {
            audio.play_looped(document.cursor, ls, le);
        } else {
            audio.play(document.cursor);
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area: Rect = frame.area();
        let toolbar_height = self.toolbar.rows_needed(area.width);
        let chrome = split_chrome(area, toolbar_height);

        // Render chrome panels.
        self.file_panel.render(frame, chrome.panel);
        let buf_names = self.buffer_names();
        self.buffer_panel.render(frame, chrome.buffers, &buf_names, self.active_document);
        self.toolbar.active_actions.clear();
        self.toolbar.is_playing = self.audio.as_ref().is_some_and(|a| a.is_playing());
        if self.snap_to_zero {
            self.toolbar.active_actions.insert(Action::ToggleZeroSnap);
        }
        if self.loop_playback {
            self.toolbar.active_actions.insert(Action::ToggleLoop);
        }
        if self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom) {
            self.toolbar.active_actions.insert(Action::ToggleAutoVerticalZoom);
        }
        self.toolbar.render(frame, chrome.toolbar);
        self.menu.render(frame, chrome.menu);
        // Fill the spacer row with the base background so it matches the toolbar below it
        // (rather than showing through to the terminal default).
        frame.render_widget(
            Block::default().style(Style::default().bg(theme::BASE)),
            chrome.spacer,
        );

        let doc_idx = self.active_document;
        let no_doc = self.documents.get(doc_idx).is_none();
        if no_doc {
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
            self.documents[doc_idx]
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".to_string()),
        );
        let title = Line::from(vec![
            Span::styled(title_text, Style::default().fg(theme::BORDER)),
            Span::styled(
                if self.documents[doc_idx].dirty { "* " } else { "" },
                Style::default().fg(theme::DIRTY),
            ),
        ]);
        // The waveform is "focused" (and gets the accent border) when neither side panel is.
        let waveform_focused = !self.file_panel.focused && !self.buffer_panel.focused;
        let border_color = if waveform_focused { theme::FOCUS } else { theme::BORDER };
        let outer = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
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
        let total_len = self.documents[doc_idx].len_samples();
        let viewport = self.viewport.get_or_insert_with(|| {
            Viewport::fit_to_width(total_len, inner_waveform_area.width as usize)
        });
        viewport.total_len = total_len;

        let channel_count = self.documents[doc_idx].channel_count().max(1);
        let full_chunks =
            Layout::vertical(vec![Constraint::Fill(1); channel_count]).split(waveform_area);
        let selection = self.documents[doc_idx].selection.map(|s| s.normalized());

        // When auto vertical zoom is on, dynamically fit amplitude_scale to the visible
        // window's peak every frame, so scrolling/zooming to a quieter section zooms in to
        // match. The dB scale's reference_amplitude follows the same visible peak.
        let (reference_amplitude, _visible_peak) = if viewport.auto_vertical_zoom {
            let vp = visible_peak_raw(
                self.documents.get(doc_idx),
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

            let samples = self.documents[doc_idx]
                .channels
                .get(i)
                .map(|c| c.as_slice())
                .unwrap_or(&[]);
            let widget = WaveformWidget {
                samples,
                viewport,
                cache: self.waveform_caches.get(i),
                selection,
                cursor: self.documents[doc_idx].cursor,
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

        // Marker overlay: a dashed vertical line spanning all channels at each marker's
        // column, with its label on the top row. Label rects are recorded for double-click
        // (rename) and the lines for drag (move) hit-testing in `handle_mouse`.
        let scroll = viewport.scroll_offset;
        let spc = viewport.samples_per_column.max(f64::MIN_POSITIVE);
        let wf = self.waveform_area;
        self.marker_label_rects.clear();
        let marker_style = Style::default().fg(theme::MARKER).bg(theme::BASE);
        // Visible markers as (screen x, index), sorted left-to-right so each label can be
        // clipped at the next marker's line instead of overprinting it.
        let mut visible: Vec<(u16, usize)> = self.documents[doc_idx]
            .markers
            .iter()
            .enumerate()
            .filter_map(|(mi, m)| {
                if m.position < scroll {
                    return None;
                }
                let col = ((m.position - scroll) as f64 / spc) as i64;
                (0..wf.width as i64).contains(&col).then(|| (wf.x + col as u16, mi))
            })
            .collect();
        visible.sort_by_key(|&(x, _)| x);
        let buf = frame.buffer_mut();
        for (k, &(x, mi)) in visible.iter().enumerate() {
            for y in wf.y..wf.y + wf.height {
                buf[(x, y)].set_char('┊').set_style(marker_style);
            }
            let lx = x + 1;
            // Stop the label before the next marker's line (or the pane's right edge).
            let limit = visible.get(k + 1).map(|&(nx, _)| nx).unwrap_or(wf.x + wf.width);
            let avail = limit.saturating_sub(lx) as usize;
            let shown: String = self.documents[doc_idx].markers[mi].label.chars().take(avail).collect();
            let shown_w = shown.chars().count() as u16;
            if shown_w > 0 {
                buf.set_string(lx, wf.y, &shown, marker_style);
            }
            self.marker_label_rects.push((
                Rect { x, y: wf.y, width: shown_w + 1, height: 1 },
                mi,
            ));
        }

        frame.render_widget(StatusBar { document: &self.documents[doc_idx], viewport, snap_to_zero: self.snap_to_zero, loop_playback: self.loop_playback, last_action: self.histories[doc_idx].last_label() }, status_area);

        if self.quit_confirm {
            let dirty_count = self.documents.iter().filter(|d| d.dirty).count();
            render_quit_confirm(frame, area, dirty_count);
        }

        if self.save_as_active {
            render_save_as_prompt(frame, area, &self.save_as_path, self.save_as_depth, self.save_as_dither);
        }

        if let Some(ref dialog) = self.dialog {
            render_dialog(frame, area, dialog);
        }
    }
}

/// Index of the marker closest to `pos`, or `None` if there are no markers.
fn nearest_marker(markers: &[Marker], pos: usize) -> Option<usize> {
    markers
        .iter()
        .enumerate()
        .min_by_key(|(_, m)| m.position.abs_diff(pos))
        .map(|(i, _)| i)
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

fn render_save_as_prompt(frame: &mut Frame, area: Rect, path: &str, depth: BitDepth, dither: bool) {
    let dither_text = if depth.supports_dither() {
        format!("  Dither: {} (^D)", if dither { "on" } else { "off" })
    } else {
        String::new()
    };
    let text = format!(
        " Save as: {}_   Format: {} (Tab){} ",
        path,
        depth.label(),
        dither_text,
    );
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
        .title("Save As")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().fg(theme::TEXT).bg(theme::SURFACE0));
    let paragraph = Paragraph::new(text)
        .block(block);
    frame.render_widget(paragraph, popup);
}

fn render_quit_confirm(frame: &mut Frame, area: Rect, dirty_count: usize) {
    let noun = if dirty_count == 1 { "buffer" } else { "buffers" };
    let text = format!(
        " {dirty_count} unsaved {noun} — (s)ave all & quit · (y) quit anyway · (n) cancel ",
    );
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
        Dialog::FadeIn { curve } => {
            ("Fade In", format!(" Curve: {} (Tab) ", curve.label()))
        }
        Dialog::FadeOut { curve } => {
            ("Fade Out", format!(" Curve: {} (Tab) ", curve.label()))
        }
        Dialog::Resample { buffer, current_rate } => {
            ("Resample", format!(" New rate (current {} Hz): {}_ ", current_rate, buffer))
        }
        Dialog::RenameMarker { buffer, .. } => {
            ("Rename Marker", format!(" Label: {}_ ", buffer))
        }
    };
    let width = (content_text.chars().count() as u16 + 2).min(area.width);
    let height = 3.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::delete::delete_command;

    fn doc(val: f32, len: usize) -> Document {
        Document {
            channels: vec![vec![val; len]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        }
    }

    /// Undo/redo must never cross buffers — applying an edit to one document and then
    /// undoing while a *different* document is active must not touch the other document.
    #[test]
    fn undo_history_is_isolated_per_buffer() {
        let mut app = App::new(Some(doc(1.0, 10)), None);
        app.push_document(doc(2.0, 10)); // becomes buffer 1, now active

        // Edit only buffer 1.
        let idx = 1;
        app.histories[idx].apply(delete_command(0..5), &mut app.documents[idx]);
        assert_eq!(app.documents[1].len_samples(), 5);
        assert_eq!(app.documents[0].len_samples(), 10);

        // Switching to buffer 0 and undoing must be a no-op: its history is empty.
        app.active_document = 0;
        assert!(!app.histories[0].undo(&mut app.documents[0]));
        assert_eq!(app.documents[0].len_samples(), 10);

        // Buffer 1's own undo still restores its edit.
        assert!(app.histories[1].undo(&mut app.documents[1]));
        assert_eq!(app.documents[1].len_samples(), 10);
    }

    /// Several edits on one buffer undo in reverse order, one level at a time.
    #[test]
    fn multiple_undo_levels_unwind_in_order() {
        let mut app = App::new(Some(doc(1.0, 20)), None);
        let idx = 0;
        app.histories[idx].apply(delete_command(0..5), &mut app.documents[idx]); // 20 -> 15
        app.histories[idx].apply(delete_command(0..5), &mut app.documents[idx]); // 15 -> 10
        app.histories[idx].apply(delete_command(0..5), &mut app.documents[idx]); // 10 -> 5
        assert_eq!(app.documents[0].len_samples(), 5);

        assert!(app.histories[0].undo(&mut app.documents[0]));
        assert_eq!(app.documents[0].len_samples(), 10);
        assert!(app.histories[0].undo(&mut app.documents[0]));
        assert_eq!(app.documents[0].len_samples(), 15);
        assert!(app.histories[0].undo(&mut app.documents[0]));
        assert_eq!(app.documents[0].len_samples(), 20);
        assert!(!app.histories[0].undo(&mut app.documents[0]));
    }
}
