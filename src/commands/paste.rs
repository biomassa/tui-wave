use std::sync::Arc;

use crate::model::command::Command;
use crate::model::document::Document;

#[derive(Debug)]
pub struct PasteCommand {
    at: usize,
    /// Shared with the clipboard until the paste is applied, so pasting copies the clipboard
    /// once (into the document) rather than twice.
    data: Arc<Vec<Vec<f32>>>,
    inserted_len: usize,
}

impl PasteCommand {
    pub fn new(at: usize, data: impl Into<Arc<Vec<Vec<f32>>>>) -> Self {
        let data = data.into();
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
        // Taken, not cloned: while the paste is applied its audio lives in the document, and
        // `undo` takes it back out of there. Holding a second copy here would double the undo
        // memory of every paste for as long as it stays on the stack.
        let data = std::mem::take(&mut self.data);
        doc.insert_range(self.at, &data);
        doc.selection = None;
        doc.cursor = (self.at + self.inserted_len).min(doc.len_samples());
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        // What comes back can have more channels than went in (`insert_range` widens mono
        // data to every channel); a redo inserts the same audio either way.
        self.data = Arc::new(doc.remove_range(self.at..self.at + self.inserted_len));
        doc.cursor = self.at;
        doc.dirty = true;
    }

    fn stored_bytes(&self) -> usize {
        crate::model::command::sample_bytes(&self.data)
    }

    fn is_noop(&self) -> bool {
        self.inserted_len == 0
    }

    fn label(&self) -> &str {
        "Paste"
    }
}

pub fn paste_command(at: usize, data: impl Into<Arc<Vec<Vec<f32>>>>) -> Box<dyn Command> {
    Box::new(PasteCommand::new(at, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_then_undo_restores_original_buffer() {
        let mut doc = Document {
            original_channels: Vec::new(),
            head_tail_marks: Vec::new(),
            channels: vec![vec![1.0, 2.0, 3.0]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
            stream: None,
        };
        let original = doc.channels.clone();

        let mut cmd = PasteCommand::new(1, vec![vec![9.0, 9.0]]);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 9.0, 9.0, 2.0, 3.0]]);
        assert_eq!(doc.cursor, 3);

        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }

    /// The pasted audio moves between the command and the document instead of being copied:
    /// none is held while applied, all of it after undo, and a redo (including of a mono paste
    /// into stereo, which comes back two channels wide) puts back the same result.
    #[test]
    fn redo_after_undo_repeats_the_paste_and_the_command_holds_audio_only_while_undone() {
        let mut doc = Document::default();
        doc.channels = vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]];
        let original = doc.channels.clone();
        let mut cmd = PasteCommand::new(1, vec![vec![9.0, 9.0]]);
        cmd.execute(&mut doc);
        let pasted = doc.channels.clone();
        assert_eq!(pasted, vec![vec![1.0, 9.0, 9.0, 2.0, 3.0], vec![4.0, 9.0, 9.0, 5.0, 6.0]]);
        assert_eq!(cmd.stored_bytes(), 0);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
        assert_eq!(cmd.stored_bytes(), 2 * 2 * 4);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, pasted);
    }
}
