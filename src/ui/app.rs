use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use std::time::Duration;

use crate::model::document::Document;

use super::terminal::Tui;
use super::viewport::Viewport;
use super::widgets::waveform::WaveformWidget;

pub struct App {
    pub should_quit: bool,
    pub document: Option<Document>,
    pub viewport: Option<Viewport>,
}

impl App {
    pub fn new(document: Option<Document>) -> Self {
        Self {
            should_quit: false,
            document,
            viewport: None,
        }
    }

    pub fn run(&mut self, terminal: &mut Tui) -> color_eyre::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(16))? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key);
                }
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
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

        let viewport = self
            .viewport
            .get_or_insert_with(|| Viewport::fit_to_width(document.len_samples(), inner.width as usize));

        let channel_count = document.channel_count().max(1);
        let chunks = Layout::vertical(vec![Constraint::Fill(1); channel_count]).split(inner);

        for (i, channel_area) in chunks.iter().enumerate() {
            let samples = document.channels.get(i).map(|c| c.as_slice()).unwrap_or(&[]);
            let widget = WaveformWidget {
                samples,
                viewport,
            };
            frame.render_widget(widget, *channel_area);
        }
    }
}
