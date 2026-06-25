use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::audio::engine::AudioEngine;
use crate::config::Config;
use crate::commands::cut::cut_command;
use crate::commands::delete::delete_command;
use crate::commands::fade::{fade_command, technical_fades_command, FadeCurve};
use crate::commands::gain::gain_command;
use crate::commands::marker::{
    auto_insert_markers_command, delete_marker_command, insert_marker_command, move_marker_command, rename_marker_command,
};
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
use super::file_panel::{EntryKind as FileEntryKind, FilePanel};
use super::keymap::{map_key, Action};
use super::layout::split_chrome;
use super::menu::MenuBar;
use super::terminal::Tui;
use super::text_input::TextInput;
use super::theme;
use super::toolbar::Toolbar;
use super::viewport::Viewport;
use super::waveform_cache::WaveformCache;
use super::widgets::db_scale::{DbScaleWidget, DB_GUTTER_WIDTH};
use super::widgets::statusbar::StatusBar;
use super::widgets::waveform::WaveformWidget;
use super::widgets::waveform_image;

enum Dialog {
    Normalize { input: TextInput },
    Gain { input: TextInput, tanh_clip: bool },
    FadeIn { curve: FadeCurve },
    FadeOut { curve: FadeCurve },
    Resample { input: TextInput, current_rate: u32 },
    RenameMarker { position: usize, input: TextInput },
    OpenDirectory { input: TextInput },
    RenameBuffer { index: usize, input: TextInput },
}

/// Which panel currently has focus — the single source of truth for the modal command
/// panel, contextual key handling, and the accent on the active panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Waveform,
    Files,
    Buffers,
}

/// How long the Files-panel selection must sit still on a file before Audition decodes
/// and plays it — long enough that arrowing quickly through a list doesn't trigger a
/// decode-and-play per keystroke, short enough to still feel immediate when browsing.
const AUDITION_DEBOUNCE: Duration = Duration::from_millis(200);

/// Clamp range for the Next Rising Edge transient threshold (`+`/`-`), in dB.
const TRANSIENT_THRESHOLD_MIN_DB: f32 = 1.0;
const TRANSIENT_THRESHOLD_MAX_DB: f32 = 24.0;

/// A pending y/n confirmation modal. Generalizes the old quit-only prompt so closing a
/// dirty buffer can reuse the same flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Quit,
    CloseBuffer(usize),
}

/// What to do once `App::save_as_queue` (buffers waiting for a filename before some other
/// action can proceed) is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveAsQueueThen {
    Quit,
    CloseBuffer(usize),
}

pub struct App {
    pub should_quit: bool,
    /// Set once at startup via `set_picker` (queried in `terminal::init`, which needs raw
    /// mode already enabled — before `App::new` runs) if the terminal supports a real
    /// image-graphics protocol (kitty, Sixel, or iTerm2's). `None` on any terminal that
    /// doesn't, including inside a detected multiplexer. Never re-queried after startup.
    picker: Option<ratatui_image::picker::Picker>,
    /// When true (and `picker` is `Some`), render the waveform via the detected graphics
    /// protocol instead of character glyphs. Persisted, defaults to `true` — see
    /// `Config.graphics_mode`. Toggled with `Action::ToggleGraphicsMode`.
    pub graphics_mode: bool,
    /// Per-channel graphics-mode image state, index-parallel to the active document's
    /// channels. Rebuilt fresh every frame from the live `viewport`/`selection`/`cursor`/
    /// `playhead` (via `Picker::new_resize_protocol`, the crate's intended way to swap in
    /// new image content — there's no in-place "update this image" method on
    /// `StatefulProtocol`), since the waveform's pixel content genuinely changes on
    /// essentially every redraw during scrolling/zooming/playback. Cleared whenever the
    /// channel count changes (e.g. switching to a document with a different channel
    /// count), so stale per-channel state from a previous document is never reused.
    graphics_protocols: Vec<ratatui_image::protocol::StatefulProtocol>,
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
    /// Rendered area of the Files panel, for mouse-click focus hit-testing.
    file_panel_area: Rect,
    /// Rendered area of the Buffers panel, for mouse-click focus hit-testing.
    buffer_panel_area: Rect,
    /// A pending y/n confirmation (quit, or closing a dirty buffer). Intercepts the next
    /// keypress as a confirmation instead of routing it through the normal keymap.
    confirm: Option<Confirm>,
    /// Sample position where the current mouse-down started (for drag-to-select).
    mouse_down_anchor: Option<usize>,
    /// Index of the marker currently being dragged with the mouse, if any.
    dragging_marker: Option<usize>,
    /// The dragged marker's position when the drag started, so the whole gesture (not each
    /// intermediate mouse-move) becomes a single undoable `MoveMarkerCommand` at drag-end.
    dragging_marker_start_position: Option<usize>,
    /// Rendered marker-label rects (label box + marker index) for mouse hit-testing.
    marker_label_rects: Vec<(Rect, usize)>,
    /// Time/cell of the last left mouse-down, used to detect double-clicks.
    last_click: Option<(Instant, u16, u16)>,
    /// Time/cell of the last left mouse-down *in the waveform background* (not on a marker
    /// label, which has its own double-click-to-rename handling via `last_click`) — used to
    /// detect a double-click that should select the region between adjacent markers.
    last_waveform_click: Option<(Instant, u16, u16)>,
    /// File panel on the left showing WAV files in the current directory.
    pub file_panel: FilePanel,
    /// Buffer panel showing all open documents.
    pub buffer_panel: BufferPanel,
    /// When true, the user is typing a Save-As path in a prompt overlay.
    pub save_as_active: bool,
    /// The Save-As filename field being edited.
    save_as_input: TextInput,
    /// Output bit depth for the pending Save As (Tab cycles it in the prompt).
    pub save_as_depth: BitDepth,
    /// Whether to dither the pending Save As (Ctrl+D toggles; only meaningful for int depths).
    pub save_as_dither: bool,
    /// Buffer indices still waiting for a Save-As filename before `save_as_queue_then` can
    /// run — e.g. quitting with several never-saved buffers walks through one Save As
    /// prompt per buffer rather than silently skipping (and losing) them. Popped from the
    /// back, so it's pushed already reversed (see `queue_save_as`).
    save_as_queue: Vec<usize>,
    /// What to do once `save_as_queue` is empty. `None` means the current Save-As prompt
    /// (if any) is just a plain one-off, not part of a queued sequence.
    save_as_queue_then: Option<SaveAsQueueThen>,
    /// When true, destructive operations snap selection boundaries to zero crossings.
    pub snap_to_zero: bool,
    /// When true, playback loops — the full file if no selection, or the selection range.
    pub loop_playback: bool,
    /// When true, arrows (and Shift+arrows) move/extend by a single sample instead of a whole
    /// column. Toggled with `~` — a modifier-free fine-step mode, since every Ctrl/Alt+arrow
    /// combo is intercepted by some terminal or desktop before the app sees it.
    pub fine_mode: bool,
    /// When true, navigating to a file in the Files panel (Up/Down or a single click)
    /// previews it by playing straight from disk, without loading it into a buffer.
    /// Toggled with `p`.
    pub audition: bool,
    /// When true, pausing playback (Space while playing) snaps the insertion point to
    /// wherever playback stopped, scrolling it into view. Toggled with `i`.
    pub cursor_follows_playback: bool,
    /// When true, once the playhead reaches the right edge of the view during playback,
    /// the viewport recenters on it and keeps scrolling so the playhead stays in view for
    /// the rest of that playback run. Toggled with `f`.
    pub viewport_follows_playback: bool,
    /// Sticky flag: once the playhead has reached the right edge during the current
    /// playback run, the viewport keeps recentering on it every frame rather than waiting
    /// for the edge to be hit again (which would otherwise produce a jumpy step-scroll
    /// instead of a continuous one). Reset whenever playback stops.
    viewport_following: bool,
    /// dB threshold a frame's level must rise above the recent background by to count as a
    /// transient for "Next Rising Edge" (`/`). Adjusted with `+`/`-`, persisted.
    pub transient_threshold_db: f32,
    /// The audition playback engine, separate from `audio` (the active document's engine)
    /// since auditioning must not disturb whatever's actually loaded/playing. `None` when
    /// nothing is being auditioned.
    audition_audio: Option<AudioEngine>,
    /// Path of the file `audition_audio` is currently playing, if any.
    audition_playing_path: Option<PathBuf>,
    /// A file waiting to start auditioning once `AUDITION_DEBOUNCE` has elapsed since the
    /// selection landed on it — avoids decoding/playing every file the user arrows past
    /// while skimming the list quickly.
    audition_pending: Option<(PathBuf, Instant)>,
    /// Time/cell of the last left mouse-down on a file-panel entry, used to detect a
    /// double-click (which opens the file) versus a single click (which only selects it,
    /// auditioning it if Audition is on).
    last_file_click: Option<(Instant, u16, u16)>,
    /// Persisted toggles, loaded at startup and rewritten whenever one changes. The
    /// snapshot here is what gets written to disk — see `save_config`.
    config: Config,
    /// The nav action currently building up a fast-repeat streak (see `nav_step_multiplier`).
    nav_hold_action: Option<Action>,
    /// How many consecutive repeats of `nav_hold_action` have landed less than
    /// `NAV_FAST_REPEAT_GAP` apart. This — not elapsed wall-clock time — is what
    /// acceleration ramps on, specifically because elapsed time can't tell a held key from
    /// someone tapping it steadily for a while: both rack up the same wall-clock duration.
    /// A tight per-event gap requirement is what only a genuine hold (terminal auto-repeat
    /// fires every ~20-50ms) can sustain for many consecutive events; manual tapping can't.
    nav_repeat_count: u32,
    /// Time of the most recent nav-step keypress, used to measure the gap to the next one.
    last_nav_time: Option<Instant>,
    /// Active parameter dialog (Normalize or Gain), if any.
    dialog: Option<Dialog>,
    /// The current playback position, set from `AudioEngine.position` during playback.
    /// `None` when playback is stopped. This is the visual playhead only — the cursor
    /// (insertion point) lives on `Document.cursor`.
    playhead_position: Option<usize>,
}

impl App {
    pub fn new(document: Option<Document>, directory: Option<PathBuf>) -> Self {
        Self::new_with_config(document, directory, Config::load())
    }

    /// Sets the graphics-protocol capability detected by `terminal::init()` — called once
    /// from `main` right after construction, since the detection query itself needs raw
    /// mode already enabled (done in `terminal::init`, which runs before `App::new`).
    pub fn set_picker(&mut self, picker: Option<ratatui_image::picker::Picker>) {
        self.picker = picker;
    }

    /// The real constructor body, parameterized on `Config` so tests can pass
    /// `Config::default()` instead of `Config::load()` — tests must never depend on
    /// whatever happens to be in the user's real `~/.config/tui-wave/config.toml` (or race
    /// against other tests that temporarily redirect `XDG_CONFIG_HOME`).
    fn new_with_config(document: Option<Document>, directory: Option<PathBuf>, config: Config) -> Self {
        let dir = directory
            .or_else(|| document.as_ref().and_then(|d| d.path.as_ref()).and_then(|p| p.parent().map(|p| p.to_path_buf())))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let mut file_panel = FilePanel::new(dir);
        // The Files panel starts focused so the first thing a user does is pick a file to
        // load, rather than landing on an empty waveform view with nothing to act on.
        file_panel.focused = true;

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
            picker: None,
            graphics_mode: config.graphics_mode,
            graphics_protocols: Vec::new(),
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
            file_panel_area: Rect::default(),
            buffer_panel_area: Rect::default(),
            confirm: None,
            mouse_down_anchor: None,
            dragging_marker: None,
            dragging_marker_start_position: None,
            marker_label_rects: Vec::new(),
            last_click: None,
            last_waveform_click: None,
            file_panel,
            buffer_panel: BufferPanel::new(),
            save_as_active: false,
            save_as_input: TextInput::new(""),
            save_as_depth: BitDepth::Float32,
            save_as_dither: false,
            save_as_queue: Vec::new(),
            save_as_queue_then: None,
            snap_to_zero: config.snap_to_zero,
            loop_playback: config.loop_playback,
            fine_mode: config.fine_mode,
            audition: config.audition,
            cursor_follows_playback: config.cursor_follows_playback,
            viewport_follows_playback: config.viewport_follows_playback,
            viewport_following: false,
            transient_threshold_db: config.transient_threshold_db,
            audition_audio: None,
            audition_playing_path: None,
            audition_pending: None,
            last_file_click: None,
            config,
            nav_hold_action: None,
            nav_repeat_count: 0,
            last_nav_time: None,
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

    /// Document indices whose buffer name passes the Buffers-panel search filter.
    fn filtered_buffer_indices(&self) -> Vec<usize> {
        let names = self.buffer_names();
        (0..names.len())
            .filter(|&i| self.buffer_panel.matches(&names[i]))
            .collect()
    }

    /// Moves the Buffers-panel selection cursor by `delta` within the filtered subset.
    fn move_buffer_selection(&mut self, delta: isize) {
        let filtered = self.filtered_buffer_indices();
        if filtered.is_empty() {
            return;
        }
        let cur = filtered
            .iter()
            .position(|&i| i == self.buffer_panel.selected)
            .unwrap_or(0);
        let next = (cur as isize + delta).clamp(0, filtered.len() as isize - 1) as usize;
        self.buffer_panel.selected = filtered[next];
        // Navigating to a buffer loads it immediately — like the mouse click handler
        // already does — so Up/Down previews audio without a separate Enter to commit.
        self.switch_to_buffer(self.buffer_panel.selected);
    }

    /// After the buffer filter changes, keep the selection on a still-visible buffer.
    fn snap_buffer_selection_to_filter(&mut self) {
        let filtered = self.filtered_buffer_indices();
        if !filtered.iter().any(|&i| i == self.buffer_panel.selected) {
            self.buffer_panel.selected = filtered.first().copied().unwrap_or(0);
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
            self.tick_audition();
            self.tick_viewport_follow();
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.confirm.is_some() {
            self.handle_confirm_key(key);
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
        // File panel filtering — arrows still navigate the filtered sublist.
        if self.file_panel.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.file_panel.filtering = false;
                    self.file_panel.filter.clear();
                }
                KeyCode::Enter => {
                    self.open_selected_file();
                }
                KeyCode::Up => self.file_panel.move_up(),
                KeyCode::Down => self.file_panel.move_down(),
                KeyCode::Home => self.file_panel.move_top(),
                KeyCode::End => self.file_panel.move_bottom(),
                KeyCode::PageUp => self.file_panel.move_page_up(),
                KeyCode::PageDown => self.file_panel.move_page_down(),
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
                KeyCode::PageUp => { self.file_panel.move_page_up(); true }
                KeyCode::PageDown => { self.file_panel.move_page_down(); true }
                KeyCode::Enter => { self.open_selected_file(); true }
                KeyCode::Char('/') => {
                    self.file_panel.filtering = true;
                    self.file_panel.filter.clear();
                    true
                }
                // ^o opens the directory dialog (in waveform focus ^o is Fade Out).
                KeyCode::Char('o') | KeyCode::Char('O')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.handle_action(Action::OpenDirectory);
                    true
                }
                // Plain 'a' toggles Audition here (in waveform focus, plain 'a' is Auto
                // Vertical Zoom instead) — the same contextual-override pattern as ^o above.
                KeyCode::Char('a') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.handle_action(Action::ToggleAudition);
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
        // Buffer panel filtering — arrows still navigate the filtered sublist.
        if self.buffer_panel.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.buffer_panel.filtering = false;
                    self.buffer_panel.filter.clear();
                }
                KeyCode::Enter => {
                    self.handle_action(Action::SwitchBuffer);
                    self.buffer_panel.filtering = false;
                    self.buffer_panel.filter.clear();
                }
                KeyCode::Up => self.move_buffer_selection(-1),
                KeyCode::Down => self.move_buffer_selection(1),
                KeyCode::Backspace => {
                    self.buffer_panel.filter.pop();
                    self.snap_buffer_selection_to_filter();
                }
                KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                    self.buffer_panel.filter.push(c);
                    self.snap_buffer_selection_to_filter();
                }
                _ => {}
            }
            return;
        }
        // Buffer panel keyboard focus
        if self.buffer_panel.focused {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let handled = match key.code {
                // Up/Dn move the selection cursor and immediately switch to it (loading the
                // buffer's audio as you navigate); Enter is a no-op once already switched,
                // kept as a binding so it still does something sensible if pressed.
                KeyCode::Up => { self.move_buffer_selection(-1); true }
                KeyCode::Down => { self.move_buffer_selection(1); true }
                KeyCode::Enter => { self.handle_action(Action::SwitchBuffer); true }
                KeyCode::Char('/') => { self.handle_action(Action::SearchBuffers); true }
                // Contextual buffer commands (^r/^a differ from the global Reverse/SaveAll).
                KeyCode::Char('s') | KeyCode::Char('S') if ctrl => { self.handle_action(Action::Save); true }
                KeyCode::Char('w') | KeyCode::Char('W') if ctrl => { self.handle_action(Action::CloseBuffer); true }
                KeyCode::Char('r') | KeyCode::Char('R') if ctrl => { self.handle_action(Action::RenameBuffer); true }
                KeyCode::Char('a') | KeyCode::Char('A') if ctrl => { self.handle_action(Action::SaveAll); true }
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

    fn handle_confirm_key(&mut self, key: KeyEvent) {
        let Some(confirm) = self.confirm else { return };
        let save = matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'));
        let proceed = save || matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y'));
        if !proceed {
            // Any other key cancels.
            self.confirm = None;
            return;
        }
        self.confirm = None;
        match confirm {
            Confirm::Quit => {
                // (s)ave saves every dirty buffer with a path first, then walks any
                // never-saved ones through a Save As prompt each before actually quitting;
                // (y) quits regardless, discarding unsaved changes.
                if save {
                    self.begin_save_all_then_quit();
                } else {
                    self.should_quit = true;
                }
            }
            Confirm::CloseBuffer(idx) => {
                if save {
                    if self.documents.get(idx).is_some_and(|d| d.path.is_none()) {
                        // Never saved — needs a filename before it can actually be saved,
                        // so defer closing until that Save As prompt is done.
                        self.queue_save_as(vec![idx], SaveAsQueueThen::CloseBuffer(idx));
                        return;
                    }
                    self.save_buffer(idx);
                }
                self.close_buffer(idx);
            }
        }
    }

    fn handle_save_as_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                // Ensure a .wav extension before resolving the path.
                let name = ensure_wav_extension(self.save_as_input.value().trim());
                if !name.is_empty() {
                    let path = PathBuf::from(&name);
                    let path = if path.is_absolute() {
                        path
                    } else {
                        self.file_panel.directory.join(&name)
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
                }
                // Plain one-off Save As (no pending queue) just closes; mid-queue, this
                // moves on to the next never-saved buffer, or finishes (e.g. actually quits).
                self.advance_save_as_queue();
            }
            KeyCode::Esc => {
                // Backing out cancels the whole pending sequence, not just this one buffer
                // — if the user meant to quit/close anyway, (y)/(s) without saving is right
                // there in the confirmation that started this.
                self.save_as_active = false;
                self.save_as_queue.clear();
                self.save_as_queue_then = None;
            }
            // Tab cycles bit depth; Ctrl+D toggles dither (Ctrl keeps it out of the path text).
            KeyCode::Tab => self.save_as_depth = self.save_as_depth.next(),
            KeyCode::Char('d') | KeyCode::Char('D') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_as_dither = !self.save_as_dither;
            }
            KeyCode::Left => self.save_as_input.left(),
            KeyCode::Right => self.save_as_input.right(),
            KeyCode::Home => self.save_as_input.home(),
            KeyCode::End => self.save_as_input.end(),
            KeyCode::Backspace => self.save_as_input.backspace(),
            KeyCode::Delete => self.save_as_input.delete(),
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.save_as_input.insert(c);
            }
            _ => {}
        }
    }

    /// `&mut TextInput` for the active dialog, if it's a text-bearing one.
    fn dialog_input(&mut self) -> Option<&mut TextInput> {
        match self.dialog.as_mut()? {
            Dialog::Normalize { input }
            | Dialog::Gain { input, .. }
            | Dialog::Resample { input, .. }
            | Dialog::RenameMarker { input, .. }
            | Dialog::OpenDirectory { input }
            | Dialog::RenameBuffer { input, .. } => Some(input),
            Dialog::FadeIn { .. } | Dialog::FadeOut { .. } => None,
        }
    }

    /// Whether a typed `c` is accepted by the active dialog (numeric dialogs restrict input).
    fn dialog_accepts(&self, c: char) -> bool {
        match self.dialog {
            Some(Dialog::Normalize { .. }) | Some(Dialog::Gain { .. }) => {
                c.is_ascii_digit() || c == '-' || c == '.'
            }
            Some(Dialog::Resample { .. }) => c.is_ascii_digit(),
            _ => true, // rename / directory dialogs: free text
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => match self.dialog.take() {
                Some(Dialog::Normalize { input }) => {
                    let db = input.value().parse::<f32>().unwrap_or(-1.0).min(0.0);
                    self.apply_normalize(db);
                }
                Some(Dialog::Gain { input, tanh_clip }) => {
                    let db = input.value().parse::<f32>().unwrap_or(0.0);
                    self.apply_gain(db, tanh_clip);
                }
                Some(Dialog::FadeIn { curve }) => self.apply_fade(true, 100.0, curve),
                Some(Dialog::FadeOut { curve }) => self.apply_fade(false, 100.0, curve),
                Some(Dialog::Resample { input, current_rate }) => {
                    let rate = input.value().trim().parse::<u32>().unwrap_or(current_rate);
                    self.apply_resample(rate);
                }
                Some(Dialog::RenameMarker { position, input }) => {
                    let idx = self.active_document;
                    if let Some(document) = self.documents.get_mut(idx) {
                        let new_label = input.value().to_string();
                        self.histories[idx].apply(rename_marker_command(position, new_label), document);
                        if let Some(path) = document.path.clone() {
                            self.file_panel.mark_dirty(&path, true);
                        }
                    }
                }
                Some(Dialog::OpenDirectory { input }) => self.open_directory(input.value()),
                Some(Dialog::RenameBuffer { index, input }) => {
                    self.rename_buffer(index, &ensure_wav_extension(input.value().trim()));
                }
                None => {}
            },
            KeyCode::Esc => self.dialog = None,
            KeyCode::Left => {
                if let Some(input) = self.dialog_input() {
                    input.left();
                } else {
                    self.cycle_dialog_curve(false);
                }
            }
            KeyCode::Right => {
                if let Some(input) = self.dialog_input() {
                    input.right();
                } else {
                    self.cycle_dialog_curve(true);
                }
            }
            KeyCode::Home => {
                if let Some(input) = self.dialog_input() {
                    input.home();
                }
            }
            KeyCode::End => {
                if let Some(input) = self.dialog_input() {
                    input.end();
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.dialog_input() {
                    input.backspace();
                }
            }
            KeyCode::Delete => {
                if let Some(input) = self.dialog_input() {
                    input.delete();
                }
            }
            KeyCode::Tab => match self.dialog.as_mut() {
                Some(Dialog::Gain { tanh_clip, .. }) => *tanh_clip = !*tanh_clip,
                Some(Dialog::FadeIn { curve }) | Some(Dialog::FadeOut { curve }) => {
                    *curve = curve.next()
                }
                _ => {}
            },
            KeyCode::Char(c)
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.dialog_accepts(c) =>
            {
                if let Some(input) = self.dialog_input() {
                    input.insert(c);
                }
            }
            _ => {}
        }
    }

    /// Cycles the fade curve of the active Fade dialog (used by Left/Right when there's no
    /// text field). `forward` is currently the only direction the curve enum exposes.
    fn cycle_dialog_curve(&mut self, _forward: bool) {
        if let Some(Dialog::FadeIn { curve }) | Some(Dialog::FadeOut { curve }) = self.dialog.as_mut() {
            *curve = curve.next();
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

    /// Snapshots the current toggle state into `self.config` and writes it to disk.
    /// Called right after any toggle action so the persisted file never lags behind
    /// what's actually in effect.
    fn save_config(&mut self) {
        self.config = Config {
            snap_to_zero: self.snap_to_zero,
            auto_vertical_zoom: self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom),
            fine_mode: self.fine_mode,
            loop_playback: self.loop_playback,
            audition: self.audition,
            cursor_follows_playback: self.cursor_follows_playback,
            viewport_follows_playback: self.viewport_follows_playback,
            transient_threshold_db: self.transient_threshold_db,
            graphics_mode: self.graphics_mode,
        };
        self.config.save();
    }

    /// Step multiplier for a held arrow key. Ramps on the *count* of consecutive repeats
    /// landing less than `NAV_FAST_REPEAT_GAP` apart — not on elapsed wall-clock time, which
    /// can't tell a held key apart from someone tapping it steadily: both rack up the same
    /// duration if the gaps just happen to all be short enough. A real hold's terminal
    /// auto-repeat fires every ~20-50ms and easily clears `NAV_ACCEL_START_REPS` within a
    /// fraction of a second; manual tapping can't sustain that many sub-gap repeats in a
    /// row, so it never accumulates enough count to ramp. Any repeat with a longer gap (or a
    /// different action) resets the count to 0. Always 1x in fine mode — fine stepping is
    /// for slow, precise movement, not covering ground quickly.
    fn nav_step_multiplier(&mut self, action: Action) -> f64 {
        const NAV_FAST_REPEAT_GAP: Duration = Duration::from_millis(120);
        const NAV_ACCEL_START_REPS: u32 = 5;
        const NAV_ACCEL_RAMP_REPS: u32 = 20;
        const NAV_MAX_MULTIPLIER: f64 = 8.0;

        let now = Instant::now();
        let is_fast_repeat = self.nav_hold_action == Some(action)
            && self.last_nav_time.is_some_and(|t| now.duration_since(t) < NAV_FAST_REPEAT_GAP);
        if is_fast_repeat {
            self.nav_repeat_count = self.nav_repeat_count.saturating_add(1);
        } else {
            self.nav_hold_action = Some(action);
            self.nav_repeat_count = 0;
        }
        self.last_nav_time = Some(now);

        if self.fine_mode || self.nav_repeat_count < NAV_ACCEL_START_REPS {
            return 1.0;
        }
        let t = ((self.nav_repeat_count - NAV_ACCEL_START_REPS) as f64 / NAV_ACCEL_RAMP_REPS as f64).min(1.0);
        1.0 + t * (NAV_MAX_MULTIPLIER - 1.0)
    }

    /// The panel that currently has focus — the single source of truth for the modal
    /// command panel, contextual keys, and the active-panel accent.
    fn focus(&self) -> Focus {
        if self.file_panel.focused {
            Focus::Files
        } else if self.buffer_panel.focused {
            Focus::Buffers
        } else {
            Focus::Waveform
        }
    }

    /// Cycles focus Waveform → Files → Buffers → Waveform.
    fn cycle_focus(&mut self) {
        match self.focus() {
            Focus::Waveform => {
                self.file_panel.focused = true;
                self.buffer_panel.focused = false;
            }
            Focus::Files => {
                self.file_panel.focused = false;
                self.buffer_panel.focused = true;
            }
            Focus::Buffers => {
                self.file_panel.focused = false;
                self.buffer_panel.focused = false;
            }
        }
    }

    /// Saves buffer `idx` to its existing path (no-op if it has none).
    fn save_buffer(&mut self, idx: usize) {
        if let Some(doc) = self.documents.get_mut(idx) {
            if let Some(path) = doc.path.clone() {
                if save_wav(doc, &path).is_ok() {
                    doc.dirty = false;
                    self.file_panel.mark_dirty(&path, false);
                }
            }
        }
    }

    /// Closes buffer `idx`, confirming first if it has unsaved changes.
    fn request_close_buffer(&mut self, idx: usize) {
        if self.documents.get(idx).is_some_and(|d| d.dirty) {
            self.confirm = Some(Confirm::CloseBuffer(idx));
        } else {
            self.close_buffer(idx);
        }
    }

    /// Removes buffer `idx` (and its parallel history), fixes the active index, and rebuilds
    /// derived state. Closing the last buffer leaves the empty state.
    fn close_buffer(&mut self, idx: usize) {
        if idx >= self.documents.len() {
            return;
        }
        self.documents.remove(idx);
        self.histories.remove(idx);
        if self.documents.is_empty() {
            self.active_document = 0;
            self.viewport = None;
            self.rebuild_audio();
            self.rebuild_waveform_caches();
            return;
        }
        // Keep the active index valid; bias toward the buffer that shifted into this slot.
        if self.active_document >= self.documents.len() {
            self.active_document = self.documents.len() - 1;
        } else if self.active_document > idx {
            self.active_document -= 1;
        }
        self.viewport = None;
        self.rebuild_audio();
        self.rebuild_waveform_caches();
    }

    /// Points the file panel at `input` (a directory path; `~` expands to $HOME). No-op if
    /// the path isn't an existing directory.
    fn open_directory(&mut self, input: &str) {
        let input = input.trim();
        if input.is_empty() {
            return;
        }
        let path = if input == "~" {
            dirs_home().map(PathBuf::from)
        } else if let Some(rest) = input.strip_prefix("~/") {
            dirs_home().map(|h| PathBuf::from(h).join(rest))
        } else {
            Some(PathBuf::from(input))
        };
        if let Some(path) = path {
            if path.is_dir() {
                self.file_panel.set_directory(path);
                self.file_panel.focused = true;
            }
        }
    }

    /// Renames buffer `idx` to `new_name`, renaming the file on disk if it has one (kept in
    /// the same directory). For an unsaved buffer it just sets the path for the next save.
    fn rename_buffer(&mut self, idx: usize, new_name: &str) {
        if new_name.is_empty() || idx >= self.documents.len() {
            return;
        }
        let old_path = self.documents[idx].path.clone();
        let parent = old_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.file_panel.directory.clone());
        let new_path = parent.join(new_name);
        if let Some(old) = old_path.as_ref() {
            if old.exists() && std::fs::rename(old, &new_path).is_err() {
                return; // leave the buffer untouched if the disk rename failed
            }
            self.file_panel.mark_dirty(old, false);
        }
        let dirty = self.documents[idx].dirty;
        self.documents[idx].path = Some(new_path.clone());
        self.file_panel.mark_dirty(&new_path, dirty);
        self.file_panel.scan();
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

    /// Activates the selected file-panel entry: navigate into a directory (or `..`), or open
    /// a `.wav` file.
    fn open_selected_file(&mut self) {
        let Some((path, kind)) = self.file_panel.selected_entry() else {
            return;
        };
        match kind {
            FileEntryKind::Parent | FileEntryKind::Dir => self.file_panel.set_directory(path),
            FileEntryKind::File => self.load_file(path),
        }
    }

    /// Drops any audition playback/pending state. Dropping `AudioEngine` sends it a `Stop`
    /// and tears down its thread, so this is enough to silence it immediately.
    fn stop_audition(&mut self) {
        self.audition_audio = None;
        self.audition_playing_path = None;
        self.audition_pending = None;
    }

    /// Drives the Audition feature: called once per main-loop tick (same cadence as
    /// `sync_playhead_from_audio`). Watches the Files panel's selected entry and, after it
    /// settles on a `.wav` file for `AUDITION_DEBOUNCE`, plays that file straight from disk
    /// without loading it into a buffer — so skimming the list with Up/Down previews each
    /// file without a full decode-and-play on every single keypress.
    fn tick_audition(&mut self) {
        if !self.audition {
            if self.audition_audio.is_some() || self.audition_pending.is_some() {
                self.stop_audition();
            }
            return;
        }

        let current = if self.file_panel.focused {
            self.file_panel
                .selected_entry()
                .filter(|(_, kind)| *kind == FileEntryKind::File)
                .map(|(path, _)| path)
        } else {
            None
        };

        let already_on_target = self.audition_playing_path == current
            || self.audition_pending.as_ref().map(|(p, _)| p) == current.as_ref();
        if !already_on_target {
            // Selection moved to a different file (or off the file panel entirely) — stop
            // whatever was playing/pending right away; only the *new* target gets debounced.
            self.audition_audio = None;
            self.audition_playing_path = None;
            self.audition_pending = current.clone().map(|path| (path, Instant::now()));
        }

        if let Some((path, started)) = self.audition_pending.clone() {
            if Instant::now().duration_since(started) >= AUDITION_DEBOUNCE {
                self.audition_pending = None;
                if let Ok(document) = crate::model::io::load_wav(&path) {
                    self.audition_audio = AudioEngine::try_new(document.channels, document.sample_rate);
                    if let Some(engine) = &self.audition_audio {
                        engine.play(0);
                    }
                    self.audition_playing_path = Some(path);
                }
            }
        }
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

    /// Moves the cursor to `pos` (the result of a Next/Previous Rising Edge search) and
    /// re-centers the viewport on it, rather than just nudging it into view — at any
    /// meaningful zoom level the edge would otherwise land right at the screen's margin,
    /// not given the surrounding context a transient-finding jump is actually for.
    fn jump_to_transient(&mut self, pos: usize) {
        if let Some(document) = self.active_doc_mut() {
            document.cursor = pos;
        }
        let width = self.content_width;
        if let Some(viewport) = self.viewport.as_mut() {
            viewport.center_on(pos, width);
        }
        if let Some(audio) = &self.audio {
            if audio.is_playing() {
                audio.seek(pos);
            }
        }
    }

    /// A short exponential fade in at the very start of the file and fade out at the very
    /// end (the standard pre-export "technical fade" to mask the click a hard cut to/from
    /// silence would otherwise leave at the file's boundaries) — fixed at 5ms, no dialog,
    /// always the whole file regardless of any active selection.
    fn apply_technical_fades(&mut self) {
        const TECHNICAL_FADE_MS: f64 = 5.0;
        let idx = self.active_document;
        let Some(document) = self.active_doc() else { return };
        let fade_len = ((document.sample_rate as f64 * TECHNICAL_FADE_MS / 1000.0).round() as usize).max(1);
        self.histories[idx].apply(technical_fades_command(fade_len), &mut self.documents[idx]);
        self.after_sample_mutation(idx);
    }

    fn load_file(&mut self, path: PathBuf) {
        self.stop_audition();
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

    /// Saves every dirty buffer that already has a path immediately, then walks any
    /// never-saved dirty buffers through a Save As prompt each, one at a time, before
    /// actually quitting — `save_all` alone would otherwise silently skip (and lose) them.
    fn begin_save_all_then_quit(&mut self) {
        self.save_all();
        let unnamed: Vec<usize> =
            self.documents.iter().enumerate().filter(|(_, d)| d.dirty && d.path.is_none()).map(|(i, _)| i).collect();
        if unnamed.is_empty() {
            self.should_quit = true;
            return;
        }
        self.queue_save_as(unnamed, SaveAsQueueThen::Quit);
    }

    /// Starts (or continues) a queued Save-As sequence: `indices` in the order they should
    /// be prompted, `then` run once they're all done.
    fn queue_save_as(&mut self, mut indices: Vec<usize>, then: SaveAsQueueThen) {
        indices.reverse(); // popped from the back, so store back-to-front for prompt order
        self.save_as_queue = indices;
        self.save_as_queue_then = Some(then);
        self.advance_save_as_queue();
    }

    /// Opens the Save As prompt for the next buffer in `save_as_queue`, or — once it's
    /// empty — closes the prompt and runs whatever `save_as_queue_then` says to do next.
    fn advance_save_as_queue(&mut self) {
        let Some(idx) = self.save_as_queue.pop() else {
            self.save_as_active = false;
            match self.save_as_queue_then.take() {
                Some(SaveAsQueueThen::Quit) => self.should_quit = true,
                Some(SaveAsQueueThen::CloseBuffer(idx)) => self.close_buffer(idx),
                None => {}
            }
            return;
        };
        self.active_document = idx;
        let name = self
            .documents
            .get(idx)
            .and_then(|d| d.path.as_ref())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled.wav".to_string());
        self.save_as_input = TextInput::fresh(name);
        self.save_as_active = true;
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

    /// Moves the insertion point (cursor) to `pos` and scrolls it into view — the "Insertion
    /// Point Follows Playback" snap, factored out so it's testable without a real
    /// `AudioEngine` (the only other caller, `handle_playback_action`, is gated on one).
    fn snap_cursor_to(&mut self, pos: usize) {
        if let Some(document) = self.active_doc_mut() {
            document.cursor = pos;
        }
        let width = self.content_width;
        if let Some(viewport) = self.viewport.as_mut() {
            viewport.ensure_visible(pos, width);
        }
    }

    /// Drives "Viewport Follows Playback": while playing, once the playhead reaches the
    /// right edge of the view, recenter on it and keep recentering every frame from then
    /// on — `viewport_following` is what makes that sticky (continuous scrolling) instead
    /// of a one-off snap that would otherwise only refire each time the recentered edge is
    /// reached again. Works at any zoom level since it operates purely in sample space via
    /// `Viewport::center_on`, not in fixed pixel/column terms.
    fn tick_viewport_follow(&mut self) {
        if !self.viewport_follows_playback {
            self.viewport_following = false;
            return;
        }
        let Some(playhead) = self.playhead_position else {
            self.viewport_following = false;
            return;
        };
        let width = self.content_width;
        if !self.viewport_following {
            let Some(viewport) = self.viewport.as_ref() else { return };
            let col = (playhead.saturating_sub(viewport.scroll_offset)) as f64 / viewport.samples_per_column;
            if col + 1.0 < width as f64 {
                return; // still comfortably inside the view — nothing to do yet
            }
            self.viewport_following = true;
        }
        if let Some(viewport) = self.viewport.as_mut() {
            viewport.center_on(playhead, width);
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        // A left click anywhere focuses whichever panel it landed in — including the
        // waveform, which has no toggle/key of its own to focus it (Tab cycles forward
        // through panels, but a direct click should jump straight to the one under the
        // cursor). Checked before any other handling below so every click path (menu,
        // toolbar, panel entries, waveform seek/select) starts from the right focus.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let pos = Position::new(mouse.column, mouse.row);
            if self.file_panel_area.contains(pos) {
                self.file_panel.focused = true;
                self.buffer_panel.focused = false;
            } else if self.buffer_panel_area.contains(pos) {
                self.buffer_panel.focused = true;
                self.file_panel.focused = false;
            } else if self.waveform_area.contains(pos) {
                self.file_panel.focused = false;
                self.buffer_panel.focused = false;
            }
        }

        // File panel: a single click only selects (auditioning it, if Audition is on, via
        // `tick_audition`); a double-click activates it (navigate dir / open file) — mirrors
        // the double-click-to-rename convention used for marker labels elsewhere.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if self.file_panel.handle_click(mouse.column, mouse.row) {
                self.file_panel.focused = true;
                let now = Instant::now();
                let is_double_click = self.last_file_click.is_some_and(|(t, x, y)| {
                    now.duration_since(t) < Duration::from_millis(400)
                        && x.abs_diff(mouse.column) <= 1
                        && y == mouse.row
                });
                self.last_file_click = Some((now, mouse.column, mouse.row));
                if is_double_click {
                    self.last_file_click = None;
                    self.open_selected_file();
                }
                return;
            }
        }

        // Buffer panel: click to switch active buffer.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(idx) = self.buffer_panel.hit_test(mouse.column, mouse.row) {
                self.buffer_panel.selected = idx;
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
                let now = Instant::now();
                let is_double_click = self.last_waveform_click.is_some_and(|(t, x, y)| {
                    now.duration_since(t) < Duration::from_millis(400)
                        && x.abs_diff(mouse.column) <= 1
                        && y == mouse.row
                });
                self.last_waveform_click = Some((now, mouse.column, mouse.row));

                if is_double_click && !mouse.modifiers.contains(KeyModifiers::CONTROL) {
                    // Select the region bounded by the nearest marker at-or-before the click
                    // and the nearest marker after it — or the start/end of the file when
                    // there's no marker on that side — same as Audacity/Sound Forge's
                    // double-click-between-markers gesture.
                    self.last_waveform_click = None;
                    let region_start = document
                        .markers
                        .iter()
                        .map(|m| m.position)
                        .filter(|&p| p <= target)
                        .max()
                        .unwrap_or(0);
                    let region_end = document
                        .markers
                        .iter()
                        .map(|m| m.position)
                        .filter(|&p| p > target)
                        .min()
                        .unwrap_or(total_len);
                    let (region_start, region_end) = if snap {
                        document.snap_range_to_zero_crossing(region_start, region_end)
                    } else {
                        (region_start, region_end)
                    };
                    if region_start < region_end {
                        document.selection = Some(Selection { start: region_start, end: region_end });
                        document.cursor = region_start;
                    }
                    self.mouse_down_anchor = None;
                } else if mouse.modifiers.contains(KeyModifiers::CONTROL) {
                    // Ctrl extends the existing selection: pin the *far* edge as the drag
                    // anchor and move the near edge to the click — then the normal Drag branch
                    // (which reads `mouse_down_anchor`) keeps extending as the mouse moves.
                    // `target` is already zero-crossing-snapped above.
                    let anchor = if let Some(sel) = document.selection {
                        let (sel_start, sel_end) = sel.normalized();
                        // Keep whichever edge is farther from the click fixed.
                        if target.abs_diff(sel_start) <= target.abs_diff(sel_end) { sel_end } else { sel_start }
                    } else {
                        document.cursor
                    };
                    document.selection = Some(Selection { start: anchor, end: target });
                    document.cursor = anchor.min(target);
                    self.mouse_down_anchor = Some(anchor);
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
                    if let Some(marker) = self.documents.get(idx).and_then(|d| d.markers.get(mi)) {
                        self.dialog = Some(Dialog::RenameMarker {
                            position: marker.position,
                            input: TextInput::fresh(marker.label.clone()),
                        });
                    }
                    self.last_click = None;
                    self.dragging_marker = None;
                } else {
                    self.dragging_marker = Some(mi);
                    self.dragging_marker_start_position =
                        self.documents.get(idx).and_then(|d| d.markers.get(mi)).map(|m| m.position);
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
                if let Some(mi) = self.dragging_marker.take() {
                    let start_pos = self.dragging_marker_start_position.take();
                    if let Some(doc) = self.documents.get_mut(idx) {
                        // Capture the live-dragged-to position before sorting reshuffles
                        // indices, then collapse the whole drag gesture into one undoable
                        // `MoveMarkerCommand` — skipped entirely if nothing actually moved
                        // (e.g. a plain click with no drag in between).
                        let end_pos = doc.markers.get(mi).map(|m| m.position);
                        doc.markers.sort_by_key(|m| m.position);
                        if let (Some(from), Some(to)) = (start_pos, end_pos) {
                            if from != to {
                                self.histories[idx].apply(move_marker_command(from, to), doc);
                            }
                        }
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
                self.confirm = Some(Confirm::Quit);
            } else {
                self.should_quit = true;
            }
            return;
        }

        // Panel/modal commands — work regardless of focus (e.g. a toolbar click).
        match action {
            Action::Noop => return,
            Action::OpenSelected => {
                self.open_selected_file();
                return;
            }
            Action::OpenDirectory => {
                let default = dirs_home().unwrap_or_else(|| "~".to_string());
                self.dialog = Some(Dialog::OpenDirectory { input: TextInput::fresh(default) });
                return;
            }
            Action::SearchFiles => {
                self.file_panel.focused = true;
                self.file_panel.filtering = true;
                self.file_panel.filter.clear();
                return;
            }
            Action::FocusNext => {
                self.cycle_focus();
                return;
            }
            Action::SwitchBuffer => {
                self.switch_to_buffer(self.buffer_panel.selected);
                return;
            }
            Action::SearchBuffers => {
                self.file_panel.focused = false;
                self.buffer_panel.focused = true;
                self.buffer_panel.filtering = true;
                self.buffer_panel.filter.clear();
                return;
            }
            Action::CloseBuffer => {
                self.request_close_buffer(self.active_document);
                return;
            }
            Action::RenameBuffer => {
                let idx = self.active_document;
                if let Some(doc) = self.documents.get(idx) {
                    let name = doc
                        .path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.dialog = Some(Dialog::RenameBuffer { index: idx, input: TextInput::fresh(name) });
                }
                return;
            }
            _ => {}
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

        if action == Action::SelectAll {
            if let Some(document) = self.active_doc_mut() {
                let len = document.len_samples();
                if len > 0 {
                    document.selection = Some(Selection { start: 0, end: len });
                    document.cursor = len - 1;
                }
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
            self.save_config();
            return;
        }

        if action == Action::ToggleZeroSnap {
            self.snap_to_zero = !self.snap_to_zero;
            self.save_config();
            return;
        }

        if action == Action::ToggleLoop {
            self.loop_playback = !self.loop_playback;
            self.save_config();
            return;
        }

        if action == Action::ToggleFineMode {
            self.fine_mode = !self.fine_mode;
            self.save_config();
            return;
        }

        if action == Action::ToggleAudition {
            self.audition = !self.audition;
            if !self.audition {
                self.stop_audition();
            }
            self.save_config();
            return;
        }

        if action == Action::ToggleCursorFollowsPlayback {
            self.cursor_follows_playback = !self.cursor_follows_playback;
            self.save_config();
            return;
        }

        if action == Action::ToggleViewportFollowsPlayback {
            self.viewport_follows_playback = !self.viewport_follows_playback;
            if !self.viewport_follows_playback {
                self.viewport_following = false;
            }
            self.save_config();
            return;
        }

        if action == Action::ToggleGraphicsMode {
            self.graphics_mode = !self.graphics_mode;
            self.save_config();
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

        if action == Action::IncreaseTransientThreshold {
            self.transient_threshold_db = (self.transient_threshold_db + 1.0).min(TRANSIENT_THRESHOLD_MAX_DB);
            self.save_config();
            return;
        }

        if action == Action::DecreaseTransientThreshold {
            self.transient_threshold_db = (self.transient_threshold_db - 1.0).max(TRANSIENT_THRESHOLD_MIN_DB);
            self.save_config();
            return;
        }

        if action == Action::NextRisingEdge {
            let idx = self.active_document;
            let threshold = self.transient_threshold_db;
            let edge = self.documents.get(idx).and_then(|d| d.find_next_rising_edge(d.cursor, threshold));
            if let Some(pos) = edge {
                self.jump_to_transient(pos);
            }
            return;
        }

        if action == Action::PrevRisingEdge {
            let idx = self.active_document;
            let threshold = self.transient_threshold_db;
            let edge = self.documents.get(idx).and_then(|d| d.find_previous_rising_edge(d.cursor, threshold));
            if let Some(pos) = edge {
                self.jump_to_transient(pos);
            }
            return;
        }

        if action == Action::AutoInsertMarkers {
            self.handle_auto_insert_markers();
            return;
        }

        if action == Action::Normalize {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::Normalize { input: TextInput::fresh("0.0") });
            }
            return;
        }

        if action == Action::Gain {
            if self.active_doc().is_some() {
                self.dialog = Some(Dialog::Gain { input: TextInput::fresh("0.0"), tanh_clip: false });
            }
            return;
        }

        if action == Action::Resample {
            if let Some(rate) = self.active_doc().map(|d| d.sample_rate) {
                self.dialog = Some(Dialog::Resample { input: TextInput::new(""), current_rate: rate });
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

        if action == Action::TechnicalFades {
            self.apply_technical_fades();
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
                    // A copy-to-new buffer holds unsaved data with no path, so it's dirty —
                    // this makes the quit/close confirmation fire for it.
                    dirty: true,
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

        // Holding an arrow key ramps the step up so crossing a long file doesn't mean
        // hundreds of keypresses; fine mode disables this entirely since its whole point
        // is slow, precise movement. `nav_step_multiplier` also resets/tracks hold state,
        // so it must run (exactly once) for every nav action even when not used below.
        // Computed before the viewport/document borrows below since it needs `&mut self`.
        let nav_multiplier = matches!(
            action,
            Action::MoveCursorLeft
                | Action::MoveCursorRight
                | Action::ExtendSelectionLeft
                | Action::ExtendSelectionRight
        )
        .then(|| self.nav_step_multiplier(action))
        .unwrap_or(1.0);

        let idx = self.active_document;
        let Some(viewport) = self.viewport.as_mut() else { return };
        let Some(document) = self.documents.get_mut(idx) else { return };
        let total_len = document.len_samples();
        if total_len == 0 {
            return;
        }
        let width = self.content_width;
        // Cursor/selection step: one whole column normally, or ~1/8th of one while fine mode
        // (toggled with backtick) is on — fine enough for precise edits but still faster than
        // crawling one sample per keypress, except when zoomed in so far that an eighth-column
        // already rounds down to a single sample. Modifier-free fine stepping replaces the old
        // Ctrl/Alt+arrow scheme, which no terminal/DE would reliably pass through.
        let column_step = (viewport.samples_per_column.max(1.0) as usize).max(1);
        let base_step = if self.fine_mode { (column_step / 8).max(1) } else { column_step };
        let step = ((base_step as f64 * nav_multiplier).round() as usize).max(1);
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
            | Action::ToggleFineMode
            | Action::ToggleAudition
            | Action::ToggleCursorFollowsPlayback
            | Action::ToggleViewportFollowsPlayback
            | Action::ToggleGraphicsMode
            | Action::ClearSelection
            | Action::SelectAll
            | Action::SaveAs
            | Action::SaveAll
            | Action::Gain
            | Action::CopyToNew
            | Action::FadeIn
            | Action::FadeOut
            | Action::TechnicalFades
            | Action::InsertMarker
            | Action::DeleteMarker
            | Action::JumpPrevMarker
            | Action::JumpNextMarker
            | Action::NextRisingEdge
            | Action::PrevRisingEdge
            | Action::AutoInsertMarkers
            | Action::IncreaseTransientThreshold
            | Action::DecreaseTransientThreshold
            | Action::Noop
            | Action::OpenSelected
            | Action::OpenDirectory
            | Action::SearchFiles
            | Action::FocusNext
            | Action::CloseBuffer
            | Action::RenameBuffer
            | Action::SwitchBuffer
            | Action::SearchBuffers
            | Action::Trim => unreachable!(),
            // Cursor movement is identical whether or not it extends a selection; the
            // selection side-effect is applied in the second match below.
            Action::MoveCursorLeft | Action::ExtendSelectionLeft => {
                document.cursor = document.cursor.saturating_sub(step);
            }
            Action::MoveCursorRight | Action::ExtendSelectionRight => {
                document.cursor = (document.cursor + step).min(total_len - 1);
            }
            Action::JumpStart | Action::ExtendSelectionToStart => document.cursor = 0,
            Action::JumpEnd | Action::ExtendSelectionToEnd => document.cursor = total_len - 1,
            Action::ExtendSelectionToPrevMarker => {
                document.cursor = document
                    .markers
                    .iter()
                    .rev()
                    .find(|m| m.position < old_cursor)
                    .map(|m| m.position)
                    .unwrap_or(0);
            }
            Action::ExtendSelectionToNextMarker => {
                document.cursor = document
                    .markers
                    .iter()
                    .find(|m| m.position > old_cursor)
                    .map(|m| m.position)
                    .unwrap_or(total_len - 1);
            }
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
            | Action::ExtendSelectionRight => {
                // Snap the active edge to a zero crossing, but *directionally*: a plain
                // nearest-crossing snap pulls a small step (when zoomed in, column_step is one
                // sample) straight back to the crossing it just left, so the selection appears
                // frozen. If snapping would erase the step's progress, keep the literal cursor.
                let raw = document.cursor;
                let cursor = if snap {
                    let snapped = document.snap_to_zero_crossing(raw);
                    let advanced = if raw >= old_cursor { snapped > old_cursor } else { snapped < old_cursor };
                    if advanced { snapped } else { raw }
                } else {
                    raw
                };
                document.selection = Some(Selection::extended(document.selection, old_cursor, cursor));
                document.cursor = cursor;
            }
            Action::ExtendSelectionToStart
            | Action::ExtendSelectionToEnd
            | Action::ExtendSelectionToPrevMarker
            | Action::ExtendSelectionToNextMarker => {
                // cursor is already at the target (0 / end / marker position); keep it there.
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
                let name = document
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "untitled.wav".to_string());
                self.save_as_input = TextInput::fresh(name);
                self.save_as_active = true;
            }
            Action::Reverse => {
                let (start, end) = match document.selection {
                    Some(sel) => sel.normalized(),
                    None => (0, document.len_samples()),
                };
                let (start, end) = if snap {
                    document.snap_range_to_zero_crossing(start, end)
                } else {
                    (start, end)
                };
                if start < end {
                    self.histories[idx].apply(reverse_command(start, end), document);
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

    /// Insert/delete a marker at/near the cursor (both undoable, like any other document
    /// mutation), or jump the cursor to an adjacent marker (not a mutation, so not
    /// undoable).
    /// Scans the whole file for transients (`Document::find_all_rising_edges`, same
    /// algorithm and threshold as Next Rising Edge) and inserts a marker right before each
    /// one not already marked — one undo step for the whole batch, not one per marker.
    fn handle_auto_insert_markers(&mut self) {
        let idx = self.active_document;
        let Some(document) = self.documents.get(idx) else {
            return;
        };
        let edges = document.find_all_rising_edges(self.transient_threshold_db);
        let mut next_n = document.markers.len() + 1;
        let mut to_insert: Vec<Marker> = Vec::new();
        for pos in edges {
            let already_marked =
                document.markers.iter().any(|m| m.position == pos) || to_insert.iter().any(|m| m.position == pos);
            if already_marked {
                continue;
            }
            to_insert.push(Marker { position: pos, label: format!("Marker {next_n}") });
            next_n += 1;
        }
        if to_insert.is_empty() {
            return;
        }
        let document = &mut self.documents[idx];
        self.histories[idx].apply(auto_insert_markers_command(to_insert), document);
        if let Some(path) = document.path.clone() {
            self.file_panel.mark_dirty(&path, true);
        }
    }

    fn handle_marker_action(&mut self, action: Action) {
        let idx = self.active_document;
        if idx >= self.documents.len() {
            return;
        }
        let mut moved_cursor = false;
        let mut changed = false;
        match action {
            Action::InsertMarker => {
                let doc = &self.documents[idx];
                let pos = doc.cursor;
                if !doc.markers.iter().any(|m| m.position == pos) {
                    let label = format!("Marker {}", doc.markers.len() + 1);
                    self.histories[idx].apply(insert_marker_command(pos, label), &mut self.documents[idx]);
                    changed = true;
                }
            }
            Action::DeleteMarker => {
                let doc = &self.documents[idx];
                if let Some(i) = nearest_marker(&doc.markers, doc.cursor) {
                    let pos = doc.markers[i].position;
                    self.histories[idx].apply(delete_marker_command(pos), &mut self.documents[idx]);
                    changed = true;
                }
            }
            Action::JumpPrevMarker => {
                let doc = &mut self.documents[idx];
                if let Some(p) =
                    doc.markers.iter().rev().find(|m| m.position < doc.cursor).map(|m| m.position)
                {
                    doc.cursor = p;
                    moved_cursor = true;
                }
            }
            Action::JumpNextMarker => {
                let doc = &mut self.documents[idx];
                if let Some(p) = doc.markers.iter().find(|m| m.position > doc.cursor).map(|m| m.position) {
                    doc.cursor = p;
                    moved_cursor = true;
                }
            }
            _ => {}
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
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        // Space is the only transport command: play from the cursor, or pause if playing.
        if audio.is_playing() {
            audio.pause();
            self.viewport_following = false;
            // "Insertion Point Follows Playback": snap the cursor to wherever playback
            // actually stopped and scroll it into view, rather than leaving the cursor
            // wherever it was when playback started.
            if self.cursor_follows_playback {
                if let Some(stopped_at) = self.playhead_position {
                    self.snap_cursor_to(stopped_at);
                }
            }
        } else {
            let Some(document) = self.active_doc() else {
                return;
            };
            if let Some((ls, le)) = self.loop_range() {
                audio.play_looped(document.cursor, ls, le);
            } else {
                audio.play(document.cursor);
            }
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area: Rect = frame.area();
        let focus = self.focus();
        // Reserve the tallest set's height for every mode so the layout doesn't jump on Tab.
        let toolbar_height = self.toolbar.reserved_rows(area.width);
        let chrome = split_chrome(area, toolbar_height);

        // Render chrome panels.
        self.file_panel_area = chrome.panel;
        self.buffer_panel_area = chrome.buffers;
        self.file_panel.render(frame, chrome.panel);
        let buf_names = self.buffer_names();
        self.buffer_panel.render(frame, chrome.buffers, &buf_names, self.active_document);
        self.toolbar.active_actions.clear();
        self.toolbar.is_playing = self.audio.as_ref().is_some_and(|a| a.is_playing());
        self.toolbar.transient_threshold_db = self.transient_threshold_db;
        if self.snap_to_zero {
            self.toolbar.active_actions.insert(Action::ToggleZeroSnap);
        }
        if self.loop_playback {
            self.toolbar.active_actions.insert(Action::ToggleLoop);
        }
        if self.fine_mode {
            self.toolbar.active_actions.insert(Action::ToggleFineMode);
        }
        if self.viewport.as_ref().is_some_and(|v| v.auto_vertical_zoom) {
            self.toolbar.active_actions.insert(Action::ToggleAutoVerticalZoom);
        }
        if self.audition {
            self.toolbar.active_actions.insert(Action::ToggleAudition);
        }
        if self.cursor_follows_playback {
            self.toolbar.active_actions.insert(Action::ToggleCursorFollowsPlayback);
        }
        if self.viewport_follows_playback {
            self.toolbar.active_actions.insert(Action::ToggleViewportFollowsPlayback);
        }
        if self.graphics_mode {
            self.toolbar.active_actions.insert(Action::ToggleGraphicsMode);
        }
        self.toolbar.render(frame, chrome.toolbar, focus);
        // Fill the spacer row with the base background so it matches the toolbar below it
        // (rather than showing through to the terminal default).
        frame.render_widget(
            Block::default().style(Style::default().bg(theme::BASE)),
            chrome.spacer,
        );

        // The waveform pane is "focused" (and gets the accent color) when neither side panel
        // is — true for both the empty placeholder and a loaded document.
        let waveform_focused = !self.file_panel.focused && !self.buffer_panel.focused;
        let border_color = if waveform_focused { theme::FOCUS } else { theme::BORDER };

        let doc_idx = self.active_document;
        let no_doc = self.documents.get(doc_idx).is_none();
        if no_doc {
            let block = Block::default()
                .title(Span::styled(" tui-wave ", Style::default().fg(border_color)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .style(Style::default().fg(theme::CHROME_FG).bg(theme::BASE));
            let text = Paragraph::new("Select a file from the panel on the left (Tab to focus, / to search)")
                .alignment(Alignment::Center)
                .block(block);
            frame.render_widget(text, chrome.content);
            // Rendered last so an open dropdown (which extends below the menu bar, into
            // the content area) draws on top of everything instead of being overdrawn by
            // it — same ordering as the loaded-document path below.
            self.menu.render(frame, chrome.menu);
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
            Span::styled(title_text, Style::default().fg(border_color)),
            Span::styled(
                if self.documents[doc_idx].dirty { "* " } else { "" },
                Style::default().fg(theme::DIRTY),
            ),
        ]);
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
        let auto_vertical_zoom_default = self.config.auto_vertical_zoom;
        let viewport = self.viewport.get_or_insert_with(|| {
            let mut v = Viewport::fit_to_width(total_len, inner_waveform_area.width as usize);
            v.auto_vertical_zoom = auto_vertical_zoom_default;
            v
        });
        viewport.total_len = total_len;

        let channel_count = self.documents[doc_idx].channel_count().max(1);
        // Drop stale per-channel image state from a previous document with more channels
        // — never reuse it for a channel index that no longer exists.
        self.graphics_protocols.truncate(channel_count);
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

        let overlay_active =
            self.confirm.is_some() || self.save_as_active || self.dialog.is_some() || self.menu.is_open();
        let marker_refs: Vec<(usize, &str)> =
            self.documents[doc_idx].markers.iter().map(|m| (m.position, m.label.as_str())).collect();
        // Per-channel terminal row range actually covered by a rendered graphics image this
        // frame, so the marker overlay below knows which rows already have marker lines baked
        // into the bitmap (and must not also draw a buffer-cell line there — see the comment
        // by `overlay_active` above for why mixing the two corrupts the terminal display) vs.
        // which rows still need the legacy buffer-cell line (text-mode/no-picker/overlay-open
        // channels, and channel 0's reserved top row — see below).
        let mut channel_image_rows: Vec<Option<(u16, u16)>> = vec![None; channel_count];

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

            // Graphics mode: when a graphics-capable terminal was detected at startup,
            // rasterize this channel's waveform into a real bitmap and display it via the
            // detected protocol (kitty/Sixel/iTerm2), drawn on top of the character-glyph
            // WaveformWidget just rendered above. Rebuilt fresh every frame from the same
            // live viewport/selection/cursor/playhead the text widget just used —
            // `StatefulProtocol` has no in-place "swap this image" method, so
            // `Picker::new_resize_protocol` (the crate's intended way to give it new
            // content) is called every frame rather than reused, since the waveform's
            // pixel content genuinely changes on essentially every redraw during
            // scrolling/zooming/playback anyway.
            //
            // Skipped whenever a menu/dialog overlay is showing: the kitty unicode-placeholder
            // protocol embeds one escape sequence per row that, once (re-)transmitted, paints
            // the *entire* row's width directly on the real terminal screen — independent of
            // ratatui's own cell-diffing, which only knows about the single buffer cell holding
            // that sequence. Re-transmitting every frame (a fresh id each time, since we never
            // reuse the previous `StatefulProtocol`) repaints that full row on the real terminal
            // even where an overlay drew plain text moments earlier in the same buffer, which is
            // what made dialogs flash and vanish a frame later. Skipping the retransmit while an
            // overlay is open leaves the text-mode `WaveformWidget` rendered above as the visible
            // fallback in that area instead.
            if self.graphics_mode && !overlay_active {
                if let Some(picker) = &self.picker {
                    channel_image_rows[i] = Some((channel_inner.y, channel_inner.y + channel_inner.height));
                    let font = picker.font_size();
                    let pixel_width = channel_inner.width as u32 * font.width.max(1) as u32;
                    let pixel_height = channel_inner.height as u32 * font.height.max(1) as u32;
                    let img = waveform_image::rasterize_waveform(
                        samples,
                        viewport,
                        self.waveform_caches.get(i),
                        selection,
                        self.documents[doc_idx].cursor,
                        self.playhead_position,
                        &marker_refs,
                        i == 0,
                        channel_inner.width,
                        pixel_width,
                        pixel_height,
                    );
                    let protocol = picker.new_resize_protocol(image::DynamicImage::ImageRgba8(img));
                    if i < self.graphics_protocols.len() {
                        self.graphics_protocols[i] = protocol;
                    } else {
                        self.graphics_protocols.push(protocol);
                    }
                    frame.render_stateful_widget(ratatui_image::StatefulImage::default(), channel_inner, &mut self.graphics_protocols[i]);
                }
            }

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
        // A marker sitting exactly on the insertion point would otherwise hide it — the
        // marker's dashed line is drawn after (and on top of) the waveform's cursor line in
        // the same column. Recoloring that one marker to the cursor's accent keeps "the
        // insertion point is here" visible instead of silently losing it.
        let cursor = self.documents[doc_idx].cursor;
        let marker_at_cursor_style = Style::default().fg(theme::CURSOR).bg(theme::BASE);
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
            let style = if self.documents[doc_idx].markers[mi].position == cursor {
                marker_at_cursor_style
            } else {
                marker_style
            };
            for y in wf.y..wf.y + wf.height {
                // Rows actually covered by a rendered graphics image already have this
                // marker's line baked into the bitmap (see `rasterize_waveform`'s `markers`
                // param) — drawing it again here as a plain character cell would fight the
                // kitty unicode-placeholder image for control of that row's escape sequence
                // and corrupt the terminal's cursor-position bookkeeping for the whole row,
                // which is what caused markers to glitch the display in graphics mode.
                if channel_image_rows.iter().flatten().any(|&(start, end)| y >= start && y < end) {
                    continue;
                }
                buf[(x, y)].set_char('┊').set_style(style).set_diff_option(CellDiffOption::AlwaysUpdate);
            }
            let lx = x + 1;
            // Stop the label before the next marker's line (or the pane's right edge).
            let limit = visible.get(k + 1).map(|&(nx, _)| nx).unwrap_or(wf.x + wf.width);
            let avail = limit.saturating_sub(lx) as usize;
            let shown: String = self.documents[doc_idx].markers[mi].label.chars().take(avail).collect();
            let shown_w = shown.chars().count() as u16;
            // The label row is covered by channel 0's image whenever graphics mode rendered
            // it (the label text is then rasterized directly into the bitmap instead — see
            // `show_marker_labels` in `rasterize_waveform`); only draw the buffer-cell text
            // when that row genuinely has no image underneath it.
            let label_row_has_image = channel_image_rows.iter().flatten().any(|&(start, end)| wf.y >= start && wf.y < end);
            if shown_w > 0 && !label_row_has_image {
                buf.set_string(lx, wf.y, &shown, style);
                for cx in lx..lx + shown_w {
                    buf[(cx, wf.y)].set_diff_option(CellDiffOption::AlwaysUpdate);
                }
            }
            self.marker_label_rects.push((
                Rect { x, y: wf.y, width: shown_w + 1, height: 1 },
                mi,
            ));
        }

        frame.render_widget(StatusBar { document: &self.documents[doc_idx], viewport, snap_to_zero: self.snap_to_zero, loop_playback: self.loop_playback, fine_mode: self.fine_mode, transient_threshold_db: self.transient_threshold_db, last_action: self.histories[doc_idx].last_label() }, status_area);

        // Rendered last (after the waveform, panels, and marker labels) so an open
        // dropdown — which extends below the menu bar into the content area — draws on
        // top of everything instead of being overdrawn by it.
        self.menu.render(frame, chrome.menu);

        if let Some(confirm) = self.confirm {
            let text = match confirm {
                Confirm::Quit => {
                    let n = self.documents.iter().filter(|d| d.dirty).count();
                    let noun = if n == 1 { "buffer" } else { "buffers" };
                    format!(" {n} unsaved {noun} — (s)ave all & quit · (y) quit anyway · (n) cancel ")
                }
                Confirm::CloseBuffer(_) => {
                    " Unsaved buffer — (s)ave & close · (y) close anyway · (n) cancel ".to_string()
                }
            };
            render_confirm(frame, area, &text);
        }

        if self.save_as_active {
            render_save_as_prompt(frame, area, &self.save_as_input, self.save_as_depth, self.save_as_dither);
        }

        if let Some(ref dialog) = self.dialog {
            render_dialog(frame, area, dialog);
        }
    }
}

/// Best-effort home directory as a string (from $HOME), for the Open Directory default.
fn dirs_home() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty())
}

/// Ensures a save/rename target ends in `.wav` (case-insensitive), appending it otherwise.
/// Empty input is returned unchanged (callers treat empty as "don't save").
fn ensure_wav_extension(name: &str) -> String {
    if name.is_empty()
        || std::path::Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
    {
        name.to_string()
    } else {
        format!("{name}.wav")
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

fn render_save_as_prompt(frame: &mut Frame, area: Rect, input: &TextInput, depth: BitDepth, dither: bool) {
    let dither_text = if depth.supports_dither() {
        format!("  Dither: {} (^D)", if dither { "on" } else { "off" })
    } else {
        String::new()
    };
    let suffix = format!("   Format: {} (Tab){} ", depth.label(), dither_text);
    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let (before, under, after) = input.split_at_cursor();
    let content_len = " Save as: ".chars().count()
        + before.chars().count() + under.chars().count() + after.chars().count()
        + suffix.chars().count();
    let spans = vec![
        Span::styled(" Save as: ", base),
        Span::styled(before, base),
        Span::styled(under, cursor_style),
        Span::styled(after, base),
        Span::styled(suffix, base),
    ];
    let width = (content_len as u16 + 2).min(area.width);
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
        .style(base);
    frame.render_widget(Paragraph::new(Line::from(spans)).block(block), popup);
}

fn render_confirm(frame: &mut Frame, area: Rect, text: &str) {
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
    // (title, label before the field, optional text field, suffix after the field).
    let (title, prefix, input, suffix): (&str, String, Option<&TextInput>, String) = match dialog {
        Dialog::Normalize { input } => {
            ("Normalize", " Target peak (dBFS): ".into(), Some(input), " ".into())
        }
        Dialog::Gain { input, tanh_clip } => {
            let tanh = if *tanh_clip { "ON" } else { "OFF" };
            ("Gain", " Gain (dB): ".into(), Some(input), format!("  Tanh: {tanh} (Tab) "))
        }
        Dialog::FadeIn { curve } => ("Fade In", format!(" Curve: {} (Tab/←→) ", curve.label()), None, String::new()),
        Dialog::FadeOut { curve } => ("Fade Out", format!(" Curve: {} (Tab/←→) ", curve.label()), None, String::new()),
        Dialog::Resample { input, current_rate } => {
            ("Resample", format!(" New rate (current {current_rate} Hz): "), Some(input), " ".into())
        }
        Dialog::RenameMarker { input, .. } => ("Rename Marker", " Label: ".into(), Some(input), " ".into()),
        Dialog::OpenDirectory { input } => ("Open Directory", " Path: ".into(), Some(input), " ".into()),
        Dialog::RenameBuffer { input, .. } => ("Rename Buffer", " New name: ".into(), Some(input), " ".into()),
    };

    let base = Style::default().fg(theme::TEXT).bg(theme::SURFACE0);
    let cursor_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);
    let mut spans = vec![Span::styled(prefix.clone(), base)];
    let mut content_len = prefix.chars().count();
    if let Some(input) = input {
        let (before, under, after) = input.split_at_cursor();
        content_len += before.chars().count() + under.chars().count() + after.chars().count();
        spans.push(Span::styled(before, base));
        spans.push(Span::styled(under, cursor_style));
        spans.push(Span::styled(after, base));
    }
    spans.push(Span::styled(suffix.clone(), base));
    content_len += suffix.chars().count();

    let width = (content_len as u16 + 2).min(area.width);
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
        .style(base);
    frame.render_widget(Paragraph::new(Line::from(spans)).block(block), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::delete::delete_command;

    /// Builds an `App` with deterministic settings (`Config::default()`), never touching
    /// the real `~/.config/tui-wave/config.toml` or risking a race against tests elsewhere
    /// that temporarily redirect `XDG_CONFIG_HOME`. Every test below must use this instead
    /// of `App::new` directly.
    fn new_app(document: Option<Document>, directory: Option<PathBuf>) -> App {
        App::new_with_config(document, directory, Config::default())
    }

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

    /// A regression test for a real bug: the menu used to render *before* the waveform
    /// content, so an open dropdown (which extends below the menu bar into the content
    /// area) got overdrawn by it — the dropdown's own text never survived to the screen.
    /// The menu must render last so it stays on top.
    #[test]
    fn open_menu_dropdown_survives_on_top_of_waveform_content() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = new_app(Some(doc(0.5, 10_000)), None);
        app.menu.open_first(); // "File" menu, whose first entry is "Save"
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        // The dropdown's first entry ("Save", the File menu's first item) renders inside
        // its bordered popup at (popup.x + 1, popup.y + 1) = (1, 2) — row 2 being the
        // *toolbar's* first row, which is exactly what would have overwritten it under the
        // old "menu renders before content" ordering. A loose "does 'Save' appear
        // anywhere on screen" check wouldn't catch that bug: the toolbar has its own Save
        // button with the same text regardless.
        let buffer = terminal.backend().buffer();
        let row: String = (1..6u16).map(|x| buffer[(x, 2)].symbol()).collect();
        assert_eq!(row, "Save ", "the dropdown's first entry must survive on top of the toolbar row beneath it");
    }

    /// Builds a mono doc with a quiet section followed by a sudden loud one — a clear
    /// transient — at 44100Hz, matching `Document`'s own transient test fixtures (441
    /// samples per 10ms analysis frame).
    fn doc_with_transient(quiet_frames: usize, loud_frames: usize) -> Document {
        const FRAME_LEN: usize = 441;
        let mut channel = vec![0.01f32; quiet_frames * FRAME_LEN];
        channel.extend(std::iter::repeat(0.5f32).take(loud_frames * FRAME_LEN));
        Document {
            channels: vec![channel],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        }
    }

    /// Next Rising Edge moves the cursor to right before the transient and scrolls it into
    /// view.
    #[test]
    fn next_rising_edge_moves_cursor_to_the_transient() {
        let mut app = new_app(Some(doc_with_transient(20, 30)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(app.documents[0].len_samples(), 80));

        app.handle_action(Action::NextRisingEdge);

        assert_eq!(app.documents[0].cursor, 20 * 441);
    }

    /// When zoomed in, jumping to a transient must center the viewport on it (not just
    /// nudge it into view at the screen's edge) so there's context on both sides of the
    /// new cursor position.
    #[test]
    fn next_rising_edge_centers_the_viewport_when_zoomed_in() {
        let mut app = new_app(Some(doc_with_transient(20, 30)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport {
            samples_per_column: 10.0, // zoomed in: span(80) = 800, far smaller than the file
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: 1_000_000.0,
            total_len: app.documents[0].len_samples(),
            auto_vertical_zoom: false,
        });

        app.handle_action(Action::NextRisingEdge);

        let edge = 20 * 441;
        assert_eq!(app.documents[0].cursor, edge);
        let viewport = app.viewport.as_ref().unwrap();
        let half_span = viewport.span(80) / 2;
        assert_eq!(viewport.scroll_offset + half_span, edge, "the edge should sit at the center column");
    }

    /// Previous Rising Edge searches backward and also centers the viewport.
    #[test]
    fn prev_rising_edge_moves_cursor_backward_and_centers_viewport() {
        let mut app = new_app(Some(doc_with_segments(&[(0.01, 20), (0.5, 20), (5.0, 20)])), None);
        app.content_width = 80;
        app.viewport = Some(Viewport {
            samples_per_column: 10.0,
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: 1_000_000.0,
            total_len: app.documents[0].len_samples(),
            auto_vertical_zoom: false,
        });
        app.documents[0].cursor = 45 * 441; // inside the loudest segment

        app.handle_action(Action::PrevRisingEdge);

        let edge = 40 * 441; // the closer of the two earlier transients
        assert_eq!(app.documents[0].cursor, edge);
        let viewport = app.viewport.as_ref().unwrap();
        let half_span = viewport.span(80) / 2;
        assert_eq!(viewport.scroll_offset + half_span, edge);
    }

    /// With no transient before the cursor, Previous Rising Edge leaves it untouched.
    #[test]
    fn prev_rising_edge_does_nothing_when_none_found() {
        let mut app = new_app(Some(doc_with_segments(&[(0.3, 50)])), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(app.documents[0].len_samples(), 80));
        app.documents[0].cursor = 100;

        app.handle_action(Action::PrevRisingEdge);

        assert_eq!(app.documents[0].cursor, 100);
    }

    /// With no transient ahead of the cursor, Next Rising Edge leaves the cursor untouched.
    #[test]
    fn next_rising_edge_does_nothing_when_none_found() {
        let mut app = new_app(Some(doc_with_transient(0, 30)), None); // constant level throughout
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(app.documents[0].len_samples(), 80));
        app.documents[0].cursor = 100;

        app.handle_action(Action::NextRisingEdge);

        assert_eq!(app.documents[0].cursor, 100);
    }

    /// Builds a mono doc with several constant-level segments (each `frames` analysis
    /// frames of 441 samples at 44100Hz), for tests with more than one transient.
    fn doc_with_segments(segments: &[(f32, usize)]) -> Document {
        const FRAME_LEN: usize = 441;
        let channel: Vec<f32> =
            segments.iter().flat_map(|&(level, frames)| std::iter::repeat(level).take(frames * FRAME_LEN)).collect();
        Document {
            channels: vec![channel],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        }
    }

    /// Auto-Insert Markers adds one marker right before each detected transient, all as a
    /// single undo step (one `Undo` removes the whole batch, not just the last one).
    #[test]
    fn auto_insert_markers_adds_one_per_transient_as_a_single_undo_step() {
        let mut app = new_app(Some(doc_with_segments(&[(0.01, 20), (0.5, 20), (5.0, 20)])), None);

        app.handle_action(Action::AutoInsertMarkers);

        let positions: Vec<usize> = app.documents[0].markers.iter().map(|m| m.position).collect();
        assert_eq!(positions, vec![20 * 441, 40 * 441]);

        app.handle_action(Action::Undo);
        assert!(app.documents[0].markers.is_empty(), "one undo should remove the whole batch");
    }

    /// A transient that already has a marker on it must not get a second, duplicate one.
    #[test]
    fn auto_insert_markers_skips_positions_already_marked() {
        let mut app = new_app(Some(doc_with_segments(&[(0.01, 20), (0.5, 20), (5.0, 20)])), None);
        app.documents[0].markers = vec![Marker { position: 20 * 441, label: "Already here".to_string() }];

        app.handle_action(Action::AutoInsertMarkers);

        let positions: Vec<usize> = app.documents[0].markers.iter().map(|m| m.position).collect();
        assert_eq!(positions, vec![20 * 441, 40 * 441]);
        assert_eq!(app.documents[0].markers[0].label, "Already here", "the existing marker must be untouched");
    }

    /// With no transients in the file, Auto-Insert Markers does nothing (and records no
    /// undo step).
    #[test]
    fn auto_insert_markers_does_nothing_when_none_found() {
        let mut app = new_app(Some(doc_with_segments(&[(0.3, 50)])), None);

        app.handle_action(Action::AutoInsertMarkers);

        assert!(app.documents[0].markers.is_empty());
        assert!(!app.histories[0].undo(&mut app.documents[0]), "no history entry should have been recorded");
    }

    /// Technical Fades applies a fixed 5ms exp fade in/out to the whole file in one
    /// undoable step, regardless of any active selection.
    #[test]
    fn technical_fades_applies_5ms_fades_to_the_whole_file() {
        let mut d = doc(1.0, 44100); // 1 second at 44100Hz
        d.selection = Some(Selection { start: 1000, end: 2000 });
        let mut app = new_app(Some(d), None);

        app.handle_action(Action::TechnicalFades);

        let expected_fade_len = (44100.0 * 0.005f64).round() as usize; // 5ms
        assert!((app.documents[0].channels[0][0]).abs() < 0.01, "should fade in from silence");
        assert!(
            (app.documents[0].channels[0][expected_fade_len - 1] - 1.0).abs() < 0.01,
            "head fade should reach full volume by its end"
        );
        assert!((app.documents[0].channels[0][22050] - 1.0).abs() < 0.001, "the middle must be untouched");
        assert!((*app.documents[0].channels[0].last().unwrap()).abs() < 0.01, "should fade out to silence");
        assert_eq!(app.documents[0].selection, None, "should clear the selection, not act on it");

        app.handle_action(Action::Undo);
        assert!((app.documents[0].channels[0][0] - 1.0).abs() < 0.001, "undo should restore the original head");
    }

    /// `+`/`-` adjust the transient threshold within the clamp range and persist it.
    #[test]
    fn transient_threshold_adjusts_and_clamps() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        assert_eq!(app.transient_threshold_db, 6.0);

        app.handle_action(Action::IncreaseTransientThreshold);
        assert_eq!(app.transient_threshold_db, 7.0);
        assert_eq!(app.config.transient_threshold_db, 7.0, "should persist immediately");

        app.handle_action(Action::DecreaseTransientThreshold);
        app.handle_action(Action::DecreaseTransientThreshold);
        assert_eq!(app.transient_threshold_db, 5.0);

        for _ in 0..40 {
            app.handle_action(Action::DecreaseTransientThreshold);
        }
        assert_eq!(app.transient_threshold_db, TRANSIENT_THRESHOLD_MIN_DB);

        for _ in 0..40 {
            app.handle_action(Action::IncreaseTransientThreshold);
        }
        assert_eq!(app.transient_threshold_db, TRANSIENT_THRESHOLD_MAX_DB);
    }

    /// Inserting a marker is undoable, like any other document mutation.
    #[test]
    fn insert_marker_is_undoable() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].cursor = 100;
        app.handle_action(Action::InsertMarker);
        assert_eq!(app.documents[0].markers.len(), 1);
        assert_eq!(app.documents[0].markers[0].position, 100);

        app.handle_action(Action::Undo);
        assert!(app.documents[0].markers.is_empty());

        app.handle_action(Action::Redo);
        assert_eq!(app.documents[0].markers.len(), 1);
    }

    /// Deleting a marker is undoable — the removed marker (position and label) comes back
    /// exactly as it was.
    #[test]
    fn delete_marker_is_undoable() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].markers = vec![Marker { position: 200, label: "Verse".to_string() }];
        app.documents[0].cursor = 200;

        app.handle_action(Action::DeleteMarker);
        assert!(app.documents[0].markers.is_empty());

        app.handle_action(Action::Undo);
        assert_eq!(app.documents[0].markers, vec![Marker { position: 200, label: "Verse".to_string() }]);
    }

    /// Quitting with no never-saved buffers (just dirty ones that already have a path)
    /// saves them all and quits immediately — no Save As prompt needed.
    #[test]
    fn save_and_quit_with_only_named_dirty_buffers_quits_immediately() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.documents[0].path = Some(PathBuf::from("/tmp/tui_wave_test_named_only.wav"));
        app.documents[0].dirty = true;
        app.confirm = Some(Confirm::Quit);

        app.handle_confirm_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        assert!(app.should_quit);
        assert!(!app.save_as_active);
        std::fs::remove_file("/tmp/tui_wave_test_named_only.wav").ok();
    }

    /// Quitting with several never-saved (no-path) dirty buffers must prompt for a
    /// filename for each one in turn — not silently skip and lose them — before actually
    /// quitting.
    #[test]
    fn save_and_quit_with_unnamed_buffers_prompts_for_each_name_in_order() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.documents[0].dirty = true; // idx 0: never saved
        app.push_document(doc(0.2, 10)); // idx 1: never saved
        app.documents[1].dirty = true;
        app.confirm = Some(Confirm::Quit);

        app.handle_confirm_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        // Must not quit yet — two buffers still need a filename.
        assert!(!app.should_quit);
        assert!(app.save_as_active);
        assert_eq!(app.active_document, 0, "should prompt for the first buffer first");

        let dir = std::env::temp_dir().join(format!("tui_wave_quit_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        app.file_panel.set_directory(dir.clone());

        app.save_as_input = TextInput::fresh("first.wav".to_string());
        app.handle_save_as_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // One buffer named and saved; still not done — the second one is up next.
        assert!(!app.should_quit);
        assert!(app.save_as_active);
        assert_eq!(app.active_document, 1);
        assert!(!app.documents[0].dirty);
        assert!(app.documents[0].path.is_some());

        app.save_as_input = TextInput::fresh("second.wav".to_string());
        app.handle_save_as_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Both named and saved — now it actually quits.
        assert!(app.should_quit);
        assert!(!app.save_as_active);
        assert!(!app.documents[1].dirty);
        assert!(app.documents[1].path.is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Backing out (Esc) of a queued Save-As prompt cancels the whole pending sequence —
    /// it must not quit, and must not silently move on to the next buffer either.
    #[test]
    fn escaping_a_queued_save_as_cancels_the_whole_sequence() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.documents[0].dirty = true;
        app.push_document(doc(0.2, 10));
        app.documents[1].dirty = true;
        app.confirm = Some(Confirm::Quit);
        app.handle_confirm_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(app.save_as_active);

        app.handle_save_as_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(!app.save_as_active);
        assert!(!app.should_quit);
        assert!(app.save_as_queue.is_empty(), "the pending sequence must be cleared, not just paused");
    }

    /// Closing a single never-saved buffer (with "save") must also prompt for a filename
    /// rather than silently discarding it — the buffer isn't closed until that's done.
    #[test]
    fn close_buffer_with_save_on_a_never_saved_buffer_prompts_for_a_name_first() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10)); // idx 1, never saved
        app.documents[1].dirty = true;
        app.confirm = Some(Confirm::CloseBuffer(1));

        app.handle_confirm_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        assert_eq!(app.documents.len(), 2, "must not close until the name is given");
        assert!(app.save_as_active);
        assert_eq!(app.active_document, 1);

        let dir = std::env::temp_dir().join(format!("tui_wave_close_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        app.file_panel.set_directory(dir.clone());
        app.save_as_input = TextInput::fresh("named.wav".to_string());
        app.handle_save_as_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.documents.len(), 1, "should close only after being named and saved");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Renaming a marker (via the double-click dialog) is undoable.
    #[test]
    fn rename_marker_is_undoable() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].markers = vec![Marker { position: 200, label: "Old Name".to_string() }];
        app.dialog =
            Some(Dialog::RenameMarker { position: 200, input: TextInput::fresh("New Name".to_string()) });

        app.handle_dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.documents[0].markers[0].label, "New Name");

        app.handle_action(Action::Undo);
        assert_eq!(app.documents[0].markers[0].label, "Old Name");
    }

    /// Dragging a marker (mouse down on its label, drag, release) collapses into a single
    /// undoable move — undo restores the pre-drag position.
    #[test]
    fn dragging_a_marker_is_undoable_as_one_move() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].markers = vec![Marker { position: 100, label: "M".to_string() }];
        app.content_width = 80;
        app.waveform_area = Rect { x: 0, y: 0, width: 80, height: 4 };
        app.viewport = Some(Viewport { samples_per_column: 1.0, scroll_offset: 0, amplitude_scale: 1.0, min_samples_per_column: 1.0, max_samples_per_column: 1_000.0, total_len: 1_000, auto_vertical_zoom: false });
        app.marker_label_rects = vec![(Rect { x: 5, y: 0, width: 5, height: 1 }, 0)];

        let mouse_at = |col: u16, kind: MouseEventKind| MouseEvent { kind, column: col, row: 0, modifiers: KeyModifiers::NONE };
        app.handle_mouse(mouse_at(6, MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(mouse_at(50, MouseEventKind::Drag(MouseButton::Left)));
        app.handle_mouse(mouse_at(50, MouseEventKind::Up(MouseButton::Left)));

        assert_eq!(app.documents[0].markers[0].position, 50);
        app.handle_action(Action::Undo);
        assert_eq!(app.documents[0].markers[0].position, 100);
    }

    /// A plain click on a marker label (mouse down + up, no drag in between) must not push
    /// a no-op undo entry — there was no actual movement to undo.
    #[test]
    fn clicking_a_marker_without_dragging_does_not_record_history() {
        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.documents[0].markers = vec![Marker { position: 100, label: "M".to_string() }];
        app.content_width = 80;
        app.waveform_area = Rect { x: 0, y: 0, width: 80, height: 4 };
        app.viewport = Some(Viewport { samples_per_column: 1.0, scroll_offset: 0, amplitude_scale: 1.0, min_samples_per_column: 1.0, max_samples_per_column: 1_000.0, total_len: 1_000, auto_vertical_zoom: false });
        app.marker_label_rects = vec![(Rect { x: 5, y: 0, width: 5, height: 1 }, 0)];

        let mouse_at = |col: u16, kind: MouseEventKind| MouseEvent { kind, column: col, row: 0, modifiers: KeyModifiers::NONE };
        app.handle_mouse(mouse_at(6, MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(mouse_at(6, MouseEventKind::Up(MouseButton::Left)));

        assert_eq!(app.documents[0].markers[0].position, 100);
        assert!(!app.histories[0].undo(&mut app.documents[0]), "no history entry should have been recorded");
    }

    /// Double-clicking the waveform background selects the region bounded by the nearest
    /// A marker sitting exactly at the insertion point must render in the cursor's accent
    /// color, not the normal marker color — otherwise its dashed line (drawn after, and
    /// so on top of, the waveform's cursor line) silently hides where the cursor actually
    /// Shift+] (rendered here as '}') selects from the cursor to the next marker, advances
    /// the cursor to the end of that selection, and scrolls it into view.
    #[test]
    fn extend_selection_to_next_marker_selects_and_advances_cursor() {
        let mut app = new_app(Some(doc(0.1, 10_000)), None);
        app.documents[0].markers = vec![
            Marker { position: 1_000, label: "A".to_string() },
            Marker { position: 5_000, label: "B".to_string() },
        ];
        app.documents[0].cursor = 1_000;
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(10_000, 80));

        app.handle_action(Action::ExtendSelectionToNextMarker);

        assert_eq!(app.documents[0].selection, Some(Selection { start: 1_000, end: 5_000 }));
        assert_eq!(app.documents[0].cursor, 5_000, "cursor should advance to the end of the selection");
    }

    /// With no marker ahead of the cursor, it selects to the end of the file instead.
    #[test]
    fn extend_selection_to_next_marker_falls_back_to_end_of_file() {
        let mut app = new_app(Some(doc(0.1, 10_000)), None);
        app.documents[0].markers = vec![Marker { position: 1_000, label: "A".to_string() }];
        app.documents[0].cursor = 1_000;
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(10_000, 80));

        app.handle_action(Action::ExtendSelectionToNextMarker);

        assert_eq!(app.documents[0].selection, Some(Selection { start: 1_000, end: 9_999 }));
        assert_eq!(app.documents[0].cursor, 9_999);
    }

    /// Shift+[ (rendered here as '{') selects backward to the previous marker, or the
    /// start of the file if there's none — and also advances the cursor to the active
    /// (now leftmost) edge of the selection.
    #[test]
    fn extend_selection_to_prev_marker_selects_and_falls_back_to_start_of_file() {
        let mut app = new_app(Some(doc(0.1, 10_000)), None);
        app.documents[0].markers = vec![
            Marker { position: 1_000, label: "A".to_string() },
            Marker { position: 5_000, label: "B".to_string() },
        ];
        app.documents[0].cursor = 5_000;
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(10_000, 80));

        app.handle_action(Action::ExtendSelectionToPrevMarker);
        // The anchor (the edge held fixed) is where the cursor started — 5000 — with the
        // active edge following the cursor backward to 1000; `Selection` isn't normalized.
        assert_eq!(app.documents[0].selection, Some(Selection { start: 5_000, end: 1_000 }));
        assert_eq!(app.documents[0].cursor, 1_000);

        // Repeating from there, with no earlier marker, falls back to the start of the
        // file — the anchor stays at the original 5000 (Selection::extended keeps the
        // existing selection's start, not the now-stale `old_cursor`).
        app.handle_action(Action::ExtendSelectionToPrevMarker);
        assert_eq!(app.documents[0].selection, Some(Selection { start: 5_000, end: 0 }));
        assert_eq!(app.documents[0].cursor, 0);
    }

    /// is. A marker elsewhere must keep the normal marker color.
    #[test]
    fn marker_at_cursor_position_uses_cursor_accent_color() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut doc = doc(0.1, 10_000);
        doc.cursor = 500;
        doc.markers = vec![
            Marker { position: 500, label: "Here".to_string() },
            Marker { position: 2000, label: "Elsewhere".to_string() },
        ];
        let mut app = new_app(Some(doc), None);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let wf = app.waveform_area;
        let at_cursor_col = wf.x + (500.0 / app.viewport.as_ref().unwrap().samples_per_column) as u16;
        let elsewhere_col = wf.x + (2000.0 / app.viewport.as_ref().unwrap().samples_per_column) as u16;

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(at_cursor_col, wf.y)].fg, theme::CURSOR, "marker at the cursor must use the cursor accent");
        assert_eq!(buffer[(elsewhere_col, wf.y)].fg, theme::MARKER, "a marker elsewhere must keep the normal marker color");
    }

    /// marker at-or-before the click and the nearest marker after it.
    #[test]
    fn double_click_selects_region_between_markers() {
        let mut app = new_app(Some(doc(0.5, 1_000)), None);
        app.snap_to_zero = false;
        app.documents[0].markers = vec![
            Marker { position: 200, label: "A".into() },
            Marker { position: 600, label: "B".into() },
        ];
        app.waveform_area = Rect { x: 0, y: 0, width: 1_000, height: 4 };
        app.viewport = Some(Viewport::fit_to_width(1_000, 1_000)); // 1 sample per column

        let click = |col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        // Double-click between the two markers selects exactly the span between them.
        app.handle_mouse(click(400));
        app.handle_mouse(click(400));
        assert_eq!(app.documents[0].selection, Some(Selection { start: 200, end: 600 }));

        // Double-click before the first marker selects from the start of the file.
        app.handle_mouse(click(50));
        app.handle_mouse(click(50));
        assert_eq!(app.documents[0].selection, Some(Selection { start: 0, end: 200 }));

        // Double-click past the last marker selects to the end of the file.
        app.handle_mouse(click(800));
        app.handle_mouse(click(800));
        assert_eq!(app.documents[0].selection, Some(Selection { start: 600, end: 1_000 }));
    }

    /// A left click in the waveform area should focus it (and defocus the Files/Buffers
    /// panels), even though the waveform has no toggle key of its own to focus it directly.
    #[test]
    fn clicking_waveform_focuses_it_and_defocuses_panels() {
        let mut app = new_app(Some(doc(0.5, 1_000)), None);
        app.file_panel.focused = true;
        app.waveform_area = Rect { x: 10, y: 0, width: 50, height: 10 };

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });

        assert!(!app.file_panel.focused);
        assert!(!app.buffer_panel.focused);
    }

    /// A left click inside the Files panel's rendered area focuses it, even when the click
    /// doesn't land on a specific file entry (e.g. empty space below the list).
    #[test]
    fn clicking_files_panel_area_focuses_it() {
        let mut app = new_app(Some(doc(0.5, 1_000)), None);
        app.buffer_panel.focused = true;
        app.file_panel_area = Rect { x: 0, y: 0, width: 20, height: 30 };
        app.waveform_area = Rect { x: 20, y: 0, width: 50, height: 30 };

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 25,
            modifiers: KeyModifiers::NONE,
        });

        assert!(app.file_panel.focused);
        assert!(!app.buffer_panel.focused);
    }

    /// Ctrl+A selects the whole document's audio, regardless of any prior selection.
    #[test]
    fn select_all_selects_the_whole_document() {
        let mut app = new_app(Some(doc(0.5, 1_000)), None);
        app.documents[0].selection = Some(Selection { start: 10, end: 20 });
        app.handle_action(Action::SelectAll);
        assert_eq!(app.documents[0].selection, Some(Selection { start: 0, end: 1_000 }));
    }

    /// On startup the Files panel should be focused so the first thing a user does is pick
    /// Builds an app rooted at the fixtures directory and selects `mono_sine.wav` in the
    /// Files panel (entries are ordered Parent, then dirs, then files — `tests/fixtures`
    /// has no subdirectories, so index 1 is the first file alphabetically).
    fn app_with_fixture_selected() -> App {
        let mut app = new_app(None, Some(PathBuf::from("tests/fixtures")));
        app.file_panel.focused = true;
        app.file_panel.selected = 1;
        assert!(
            app.file_panel.selected_entry().unwrap().0.ends_with("mono_sine.wav"),
            "fixture directory layout changed — update the expected index"
        );
        app
    }

    /// The app is modal: plain 'a' toggles Audition while the Files panel is focused, but
    /// the very same key toggles Auto Vertical Zoom when the Waveform is focused instead —
    /// each panel's command set can reuse a letter the other panel already claimed.
    #[test]
    fn plain_a_is_audition_in_files_focus_but_auto_vzoom_in_waveform_focus() {
        let mut app = app_with_fixture_selected();
        assert!(app.file_panel.focused);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.audition, "plain 'a' in Files focus should toggle Audition");

        let mut app = new_app(Some(doc(0.1, 1_000)), None);
        app.file_panel.focused = false;
        app.viewport = Some(Viewport::fit_to_width(1_000, 80));
        assert!(!app.audition);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(!app.audition, "plain 'a' in Waveform focus must not touch Audition");
        assert!(
            app.viewport.as_ref().unwrap().auto_vertical_zoom,
            "plain 'a' in Waveform focus should toggle Auto Vertical Zoom instead"
        );
    }

    /// With Audition off, navigating the Files panel must never start decoding/playing —
    /// the feature should be fully inert until toggled on.
    #[test]
    fn audition_off_never_starts_playback() {
        let mut app = app_with_fixture_selected();
        assert!(!app.audition);
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_playing_path.is_none());
        assert!(app.audition_pending.is_none());
    }

    /// Landing on a file debounces before playing: immediately after selecting it, nothing
    /// should be considered "playing" yet, only "pending".
    #[test]
    fn audition_debounces_before_playing() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        assert!(app.audition_playing_path.is_none(), "must not play before the debounce elapses");
        assert!(app.audition_pending.is_some());
    }

    /// After the debounce window elapses, Audition commits to the selected file —
    /// `audition_playing_path` switches over even if no audio device is available in this
    /// test environment (engine construction itself is best-effort).
    #[test]
    fn audition_plays_after_debounce_elapses() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_pending.is_none());
        assert_eq!(app.audition_playing_path, app.file_panel.selected_entry().map(|(p, _)| p));
    }

    /// Navigating to a different file stops whatever was playing/pending for the old one
    /// immediately, restarting the debounce for the new selection.
    #[test]
    fn audition_switches_targets_on_navigation() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_playing_path.is_some());

        app.file_panel.selected = 2; // stereo_sine.wav
        app.tick_audition();
        assert!(app.audition_playing_path.is_none(), "switching targets should stop the old one right away");
        assert!(app.audition_pending.is_some());
    }

    /// Toggling Audition off must immediately silence anything currently playing/pending.
    #[test]
    fn toggling_audition_off_stops_playback() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_playing_path.is_some());

        app.handle_action(Action::ToggleAudition);
        assert!(!app.audition);
        assert!(app.audition_playing_path.is_none());
        assert!(app.audition_pending.is_none());
    }

    /// Actually opening a file (the real "load it") must stop any audition in progress —
    /// auditioning and the loaded document's own playback must never overlap.
    #[test]
    fn opening_a_file_stops_audition() {
        let mut app = app_with_fixture_selected();
        app.audition = true;
        app.tick_audition();
        std::thread::sleep(Duration::from_millis(250));
        app.tick_audition();
        assert!(app.audition_playing_path.is_some());

        app.open_selected_file();
        assert!(app.audition_playing_path.is_none());
        assert!(app.audition_pending.is_none());
    }

    /// A single click on a file-panel entry only selects it; a double-click is required to
    /// actually open/load it. Mouse hit-testing reads `FilePanel`'s rendered row rects, so
    /// this renders once first (via a `TestBackend`) to populate them for real.
    #[test]
    fn single_click_selects_double_click_opens() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = new_app(None, Some(PathBuf::from("tests/fixtures")));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        // Find the rendered row for mono_sine.wav (index 1: Parent, then files alphabetically)
        // by hit-testing every cell in the panel without mutating real state.
        let area = app.file_panel_area;
        let (col, row) = (area.x..area.x + area.width)
            .flat_map(|x| (area.y..area.y + area.height).map(move |y| (x, y)))
            .find(|&(x, y)| app.file_panel.hit_test(x, y) == Some(1))
            .expect("mono_sine.wav row not found in rendered panel");

        let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE };

        // Single click only selects — no document gets loaded.
        app.handle_mouse(click);
        assert_eq!(app.documents.len(), 0, "a single click must not open the file");
        assert!(app.file_panel.selected_entry().unwrap().0.ends_with("mono_sine.wav"));

        // A second click on the same cell within the double-click window opens it.
        app.handle_mouse(click);
        assert_eq!(app.documents.len(), 1, "a double-click must open the file");
    }

    /// On startup the Files panel should be focused so the first thing a user does is pick
    /// a file, rather than landing on an empty waveform with nothing to act on.
    #[test]
    fn files_panel_is_focused_on_startup() {
        let app = new_app(None, None);
        assert!(app.file_panel.focused);
        assert!(!app.buffer_panel.focused);
    }

    /// A genuine hold — repeats landing tightly spaced (simulating terminal auto-repeat,
    /// which fires every ~20-50ms) — must ramp the multiplier above 1x once enough of them
    /// land in a row, and a gap long enough to be a fresh keypress resets the count.
    #[test]
    fn nav_step_multiplier_ramps_on_a_genuine_hold() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);

        let first = app.nav_step_multiplier(Action::MoveCursorRight);
        assert_eq!(first, 1.0, "a fresh press should not be accelerated");

        // Simulate a held key: many repeats at a tight (~30ms) gap, well under the
        // 120ms fast-repeat threshold. Acceleration only kicks in once the streak count
        // clears the start threshold (5).
        let mut multiplier = 1.0;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(30));
            multiplier = app.nav_step_multiplier(Action::MoveCursorRight);
        }
        assert!(multiplier > 1.0, "a sustained tight-gap hold should accelerate");

        std::thread::sleep(Duration::from_millis(100));
        let switched = app.nav_step_multiplier(Action::MoveCursorLeft);
        assert_eq!(switched, 1.0, "switching to a different action should reset the streak");

        std::thread::sleep(Duration::from_millis(400));
        let after_gap = app.nav_step_multiplier(Action::MoveCursorLeft);
        assert_eq!(after_gap, 1.0, "a gap past the fast-repeat threshold should be treated as a fresh press");
    }

    /// The actual bug report this guards against: tapping the same arrow key repeatedly
    /// *by hand* (not holding it) must never accelerate, no matter how long the tapping is
    /// sustained — elapsed wall-clock time alone must not be what acceleration ramps on.
    #[test]
    fn nav_step_multiplier_never_accelerates_from_manual_tapping() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);

        // Each tap is 150ms apart — past the 120ms fast-repeat gap — repeated many times
        // (1.35s of sustained tapping). Every single one must stay at 1x.
        for _ in 0..9 {
            std::thread::sleep(Duration::from_millis(150));
            let multiplier = app.nav_step_multiplier(Action::MoveCursorRight);
            assert_eq!(multiplier, 1.0, "a manual tap, however sustained, must never accelerate");
        }
    }

    /// Fine mode must never accelerate, even mid a genuine tight-gap hold.
    #[test]
    fn nav_step_multiplier_disabled_in_fine_mode() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.fine_mode = true;
        let mut multiplier = 1.0;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(30));
            multiplier = app.nav_step_multiplier(Action::MoveCursorRight);
        }
        assert_eq!(multiplier, 1.0, "fine mode must never accelerate");
    }

    /// "Insertion Point Follows Playback": snapping moves the cursor to the given position
    /// and scrolls it into view, regardless of where the cursor was before.
    #[test]
    fn snap_cursor_to_moves_cursor_and_scrolls_into_view() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(1_000_000, 80));
        app.documents[0].cursor = 0;

        app.snap_cursor_to(500_000);

        assert_eq!(app.documents[0].cursor, 500_000);
        let viewport = app.viewport.as_ref().unwrap();
        let span = viewport.span(80);
        assert!(
            viewport.scroll_offset <= 500_000 && 500_000 < viewport.scroll_offset + span,
            "the snapped-to position must be visible in the viewport"
        );
    }

    /// "Viewport Follows Playback": while the playhead is comfortably inside the view,
    /// nothing happens; once it reaches the right edge, the view recenters on it and keeps
    /// recentering every subsequent tick (continuous scroll, not a one-off snap).
    #[test]
    fn tick_viewport_follow_recenters_once_playhead_reaches_the_edge() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.content_width = 80;
        // A deliberately zoomed-in viewport (span(80) = 800, far smaller than the
        // 1,000,000-sample file) so there's real room to scroll, unlike `fit_to_width`
        // which would fit the whole file into one screen and leave no room to test with.
        app.viewport = Some(Viewport {
            samples_per_column: 10.0,
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: 1_000_000.0,
            total_len: 1_000_000,
            auto_vertical_zoom: false,
        });
        app.viewport_follows_playback = true;

        // Playhead near the start, comfortably inside the view: no recenter yet.
        app.playhead_position = Some(100);
        app.tick_viewport_follow();
        assert!(!app.viewport_following);
        assert_eq!(app.viewport.as_ref().unwrap().scroll_offset, 0);

        // Move the playhead to the right edge of the current view — this should trigger
        // the sticky "following" mode and recenter.
        let span = app.viewport.as_ref().unwrap().span(80);
        app.playhead_position = Some(span - 1);
        app.tick_viewport_follow();
        assert!(app.viewport_following, "reaching the right edge should engage following");
        let half = app.viewport.as_ref().unwrap().span(80) / 2;
        assert_eq!(app.viewport.as_ref().unwrap().scroll_offset, (span - 1).saturating_sub(half));

        // Once following, it keeps recentering on every subsequent tick, even though the
        // playhead is no longer literally at the edge (it's at the new center).
        let playhead_2 = span - 1 + 1000;
        app.playhead_position = Some(playhead_2);
        app.tick_viewport_follow();
        let viewport = app.viewport.as_ref().unwrap();
        assert_eq!(viewport.scroll_offset + viewport.span(80) / 2, playhead_2);
    }

    /// Pausing playback (handled via `tick_viewport_follow` seeing no playhead) must drop
    /// out of following mode.
    #[test]
    fn viewport_follow_resets_when_playhead_disappears() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(1_000_000, 80));
        app.viewport_follows_playback = true;
        app.viewport_following = true;
        app.playhead_position = None; // playback stopped

        app.tick_viewport_follow();
        assert!(!app.viewport_following);
    }

    /// Toggling the feature off must drop out of following mode immediately, even mid-follow.
    #[test]
    fn viewport_follow_resets_when_toggled_off() {
        let mut app = new_app(Some(doc(0.1, 1_000_000)), None);
        app.content_width = 80;
        app.viewport = Some(Viewport::fit_to_width(1_000_000, 80));
        app.viewport_follows_playback = true;
        app.viewport_following = true;
        app.playhead_position = Some(500);

        app.viewport_follows_playback = false;
        app.tick_viewport_follow();
        assert!(!app.viewport_following);
    }

    /// Copy-to-New must create a *dirty* buffer (unsaved data, no path), so the quit/close
    /// confirmation fires for it instead of the app exiting silently.
    #[test]
    fn copy_to_new_marks_buffer_dirty() {
        let mut d = doc(0.5, 100);
        d.selection = Some(Selection { start: 10, end: 40 });
        let mut app = new_app(Some(d), None);
        app.handle_action(Action::CopyToNew);
        assert_eq!(app.documents.len(), 2);
        assert!(app.documents[1].dirty, "copy-to-new buffer should be dirty");
        assert!(app.documents[1].path.is_none());
        assert_eq!(app.documents[1].len_samples(), 30);
    }

    /// Closing a buffer removes its parallel history and keeps `active_document` valid.
    #[test]
    fn close_buffer_fixes_active_index_and_history() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10)); // idx 1
        app.push_document(doc(0.3, 10)); // idx 2
        assert_eq!(app.documents.len(), 3);
        assert_eq!(app.histories.len(), 3);

        app.active_document = 1;
        app.close_buffer(1); // remove the middle buffer
        assert_eq!(app.documents.len(), 2);
        assert_eq!(app.histories.len(), 2, "history must stay index-parallel");
        assert!(app.active_document < app.documents.len());
        // Remaining buffers are [0.1, 0.3]; active (still index 1) now points at 0.3.
        assert_eq!(app.documents[1].channels[0][0], 0.3);

        // Closing down to empty leaves a valid empty state.
        app.close_buffer(1);
        app.close_buffer(0);
        assert!(app.documents.is_empty());
        assert_eq!(app.active_document, 0);
    }

    /// Buffer search filters which buffers Up/Dn navigate, skipping non-matches.
    #[test]
    fn buffer_search_filters_navigation() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10));
        app.push_document(doc(0.3, 10));
        app.documents[0].path = Some(PathBuf::from("/x/alpha.wav"));
        app.documents[1].path = Some(PathBuf::from("/x/beta.wav"));
        app.documents[2].path = Some(PathBuf::from("/x/alphabet.wav"));

        app.buffer_panel.filter = "alpha".to_string();
        assert_eq!(app.filtered_buffer_indices(), vec![0, 2]); // beta filtered out

        app.buffer_panel.selected = 0;
        app.move_buffer_selection(1);
        assert_eq!(app.buffer_panel.selected, 2); // skipped index 1
        app.move_buffer_selection(1);
        assert_eq!(app.buffer_panel.selected, 2); // clamped at the last match
        app.move_buffer_selection(-1);
        assert_eq!(app.buffer_panel.selected, 0);
    }

    /// Navigating the Buffers panel with Up/Down must load the buffer immediately —
    /// no separate Enter keypress required to actually switch to it.
    #[test]
    fn moving_buffer_selection_switches_the_active_document_immediately() {
        let mut app = new_app(Some(doc(0.1, 10)), None);
        app.push_document(doc(0.2, 10));
        app.push_document(doc(0.3, 10));
        app.active_document = 0;
        app.buffer_panel.selected = 0;

        app.move_buffer_selection(1);
        assert_eq!(app.active_document, 1, "Down should switch to buffer 1 right away");

        app.move_buffer_selection(1);
        assert_eq!(app.active_document, 2, "Down should switch to buffer 2 right away");

        app.move_buffer_selection(-1);
        assert_eq!(app.active_document, 1, "Up should switch back to buffer 1 right away");
    }

    /// Undo/redo must never cross buffers — applying an edit to one document and then
    /// undoing while a *different* document is active must not touch the other document.
    #[test]
    fn undo_history_is_isolated_per_buffer() {
        let mut app = new_app(Some(doc(1.0, 10)), None);
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
        let mut app = new_app(Some(doc(1.0, 20)), None);
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
