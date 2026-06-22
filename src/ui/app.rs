use ratatui::crossterm::event::{self, Event};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use std::time::Duration;

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
    /// Width of the waveform content area as of the last render; navigation/zoom actions
    /// need this to compute spans and scroll, and re-reading it from the terminal on every
    /// keypress would require a redraw, so it's cached here instead.
    pub content_width: u16,
}

impl App {
    pub fn new(document: Option<Document>) -> Self {
        Self {
            should_quit: false,
            document,
            viewport: None,
            content_width: 1,
        }
    }

    pub fn run(&mut self, terminal: &mut Tui) -> color_eyre::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(16))? {
                if let Event::Key(key) = event::read()? {
                    if let Some(action) = map_key(key) {
                        self.handle_action(action);
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_action(&mut self, action: Action) {
        if action == Action::Quit {
            self.should_quit = true;
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
            Action::Quit => unreachable!(),
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
        let viewport = self
            .viewport
            .get_or_insert_with(|| Viewport::fit_to_width(document.len_samples(), waveform_area.width as usize));

        let channel_count = document.channel_count().max(1);
        let chunks = Layout::vertical(vec![Constraint::Fill(1); channel_count]).split(waveform_area);

        for (i, channel_area) in chunks.iter().enumerate() {
            let samples = document.channels.get(i).map(|c| c.as_slice()).unwrap_or(&[]);
            let widget = WaveformWidget { samples, viewport };
            frame.render_widget(widget, *channel_area);
        }

        frame.render_widget(StatusBar { document, viewport }, status_area);
    }
}
