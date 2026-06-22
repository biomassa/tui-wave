use ratatui::crossterm::event::{self, Event, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::audio::engine::AudioEngine;
use crate::model::document::Document;

use super::keymap::{map_key, Action};
use super::terminal::Tui;
use super::viewport::Viewport;
use super::widgets::statusbar::StatusBar;
use super::widgets::waveform::WaveformWidget;

pub struct App {
    pub should_quit: bool,
    pub document: Option<Document>,
    pub viewport: Option<Viewport>,
    pub audio: Option<AudioEngine>,
    /// Width/area of the waveform content as of the last render; navigation/zoom/mouse
    /// actions need this and re-reading it from the terminal on every input would require
    /// a redraw, so it's cached here instead.
    pub content_width: u16,
    pub waveform_area: Rect,
}

impl App {
    pub fn new(document: Option<Document>) -> Self {
        let audio = document
            .as_ref()
            .and_then(|doc| AudioEngine::try_new(doc.channels.clone(), doc.sample_rate));
        Self {
            should_quit: false,
            document,
            viewport: None,
            audio,
            content_width: 1,
            waveform_area: Rect::default(),
        }
    }

    pub fn run(&mut self, terminal: &mut Tui) -> color_eyre::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    Event::Key(key) => {
                        if let Some(action) = map_key(key) {
                            self.handle_action(action);
                        }
                    }
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    _ => {}
                }
            }
            self.sync_playhead_from_audio();
        }
        Ok(())
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
            self.should_quit = true;
            return;
        }

        if matches!(action, Action::TogglePlayback | Action::Stop) {
            self.handle_playback_action(action);
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

        match action {
            Action::Quit | Action::TogglePlayback | Action::Stop => unreachable!(),
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

        let Some(document) = &self.document else {
            let block = Block::default()
                .title(" tui-wave ")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White));
            let text = Paragraph::new("No file loaded — usage: tui-wave <file.wav>")
                .alignment(Alignment::Center)
                .block(block);
            frame.render_widget(text, area);
            return;
        };

        let title = format!(
            " tui-wave — {} ",
            document
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".to_string())
        );
        let outer = Block::default().title(title).borders(Borders::ALL);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);

        let [waveform_area, status_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

        self.content_width = waveform_area.width;
        self.waveform_area = waveform_area;
        let viewport = self.viewport.get_or_insert_with(|| {
            Viewport::fit_to_width(document.len_samples(), waveform_area.width as usize)
        });

        let channel_count = document.channel_count().max(1);
        let chunks =
            Layout::vertical(vec![Constraint::Fill(1); channel_count]).split(waveform_area);

        for (i, channel_area) in chunks.iter().enumerate() {
            let samples = document
                .channels
                .get(i)
                .map(|c| c.as_slice())
                .unwrap_or(&[]);
            let widget = WaveformWidget { samples, viewport };
            frame.render_widget(widget, *channel_area);
        }

        frame.render_widget(StatusBar { document, viewport }, status_area);
    }
}
