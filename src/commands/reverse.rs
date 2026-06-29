use crate::model::command::Command;
use crate::model::document::Document;

#[derive(Debug)]
pub struct ReverseCommand {
    range: (usize, usize),
}

impl ReverseCommand {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            range: (start.min(end), start.max(end)),
        }
    }
}

impl Command for ReverseCommand {
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        for channel in &mut doc.channels {
            if start < end && end <= channel.len() {
                channel[start..end].reverse();
            }
        }
        doc.selection = None;
        doc.cursor = start;
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        // Reversing twice restores the original order.
        let (start, end) = self.range;
        for channel in &mut doc.channels {
            if start < end && end <= channel.len() {
                channel[start..end].reverse();
            }
        }
        doc.selection = None;
        doc.cursor = start;
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Reverse"
    }
}

pub fn reverse_command(start: usize, end: usize) -> Box<dyn Command> {
    Box::new(ReverseCommand::new(start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_reverses_sample_order_in_range() {
        let mut doc = Document {
            channels: vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        };
        let mut cmd = ReverseCommand::new(1, 4);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 4.0, 3.0, 2.0, 5.0]]);
        assert!(doc.dirty);
    }

    #[test]
    fn execute_then_undo_restores_original_buffer() {
        let mut doc = Document {
            channels: vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        };
        let original = doc.channels.clone();
        let mut cmd = ReverseCommand::new(1, 4);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }
}
