use crate::model::command::Command;
use crate::model::document::Document;

#[derive(Debug)]
pub struct PasteCommand {
    at: usize,
    data: Vec<Vec<f32>>,
    inserted_len: usize,
}

impl PasteCommand {
    pub fn new(at: usize, data: Vec<Vec<f32>>) -> Self {
        let inserted_len = data.first().map(|c| c.len()).unwrap_or(0);
        Self {
            at,
            data,
            inserted_len,
        }
    }
}

impl Command for PasteCommand {
    fn execute(&mut self, doc: &mut Document) {
        doc.insert_range(self.at, self.data.clone());
        doc.selection = None;
        doc.cursor = (self.at + self.inserted_len).min(doc.len_samples());
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        doc.remove_range(self.at..self.at + self.inserted_len);
        doc.cursor = self.at;
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Paste"
    }
}

pub fn paste_command(at: usize, data: Vec<Vec<f32>>) -> Box<dyn Command> {
    Box::new(PasteCommand::new(at, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_then_undo_restores_original_buffer() {
        let mut doc = Document {
            channels: vec![vec![1.0, 2.0, 3.0]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
        };
        let original = doc.channels.clone();

        let mut cmd = PasteCommand::new(1, vec![vec![9.0, 9.0]]);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 9.0, 9.0, 2.0, 3.0]]);
        assert_eq!(doc.cursor, 3);

        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }
}
