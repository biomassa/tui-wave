use std::ops::Range;

use crate::model::command::Command;
use crate::model::document::{Document, Marker};

/// Shared by `cut` and `delete` — both just remove a range and restore it on undo; cut's
/// only difference is that the caller also stashes the removed data in the clipboard before
/// applying this command.
#[derive(Debug)]
pub struct RemoveRangeCommand {
    range: Range<usize>,
    removed: Option<Vec<Vec<f32>>>,
    /// Marker snapshot from before the cut. `remove_range` shifts markers live, but a marker
    /// that fell inside the cut can't be reconstructed from the shift alone, so undo restores
    /// the exact prior set.
    markers_before: Option<Vec<Marker>>,
    label: &'static str,
}

impl RemoveRangeCommand {
    pub fn new(range: Range<usize>, label: &'static str) -> Self {
        Self {
            range,
            removed: None,
            markers_before: None,
            label,
        }
    }
}

impl Command for RemoveRangeCommand {
    fn execute(&mut self, doc: &mut Document) {
        self.markers_before = Some(doc.markers.clone());
        self.removed = Some(doc.remove_range(self.range.clone()));
        doc.selection = None;
        doc.cursor = self.range.start.min(doc.len_samples());
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        let removed = self.removed.take().expect("undo called before execute");
        doc.insert_range(self.range.start, removed);
        if let Some(markers) = self.markers_before.take() {
            doc.markers = markers;
        }
        doc.cursor = self.range.start;
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        self.label
    }
}

pub fn delete_command(range: Range<usize>) -> Box<dyn Command> {
    Box::new(RemoveRangeCommand::new(range, "Delete"))
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let mut cmd = RemoveRangeCommand::new(1..3, "Delete");
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 4.0, 5.0]]);

        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }
}
