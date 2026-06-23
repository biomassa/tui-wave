use crate::model::command::Command;
use crate::model::document::{Document, Marker};

#[derive(Debug)]
pub struct TrimCommand {
    range: (usize, usize),
    before: Option<Vec<Vec<f32>>>,
    after: Option<Vec<Vec<f32>>>,
    markers_before: Option<Vec<Marker>>,
}

impl TrimCommand {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            range: (start.min(end), start.max(end)),
            before: None,
            after: None,
            markers_before: None,
        }
    }
}

impl Command for TrimCommand {
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        if start >= end || end > doc.len_samples() {
            return;
        }
        self.before = Some(doc.channels.iter().map(|c| c[..start].to_vec()).collect());
        self.after = Some(doc.channels.iter().map(|c| c[end..].to_vec()).collect());
        for channel in &mut doc.channels {
            let trimmed = channel[start..end].to_vec();
            *channel = trimmed;
        }
        // Keep only markers inside the kept region, re-based to the new origin.
        self.markers_before = Some(doc.markers.clone());
        doc.markers.retain(|m| m.position >= start && m.position <= end);
        for m in &mut doc.markers {
            m.position -= start;
        }
        doc.selection = None;
        doc.cursor = 0;
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        let (start, _end) = self.range;
        let before = self.before.take().expect("undo called before execute");
        let after = self.after.take().expect("undo called before execute");
        for (i, channel) in doc.channels.iter_mut().enumerate() {
            let mut restored = before[i].clone();
            restored.extend_from_slice(&channel);
            restored.extend_from_slice(&after[i]);
            *channel = restored;
        }
        if let Some(markers) = self.markers_before.take() {
            doc.markers = markers;
        }
        doc.cursor = start;
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Trim"
    }
}

pub fn trim_command(start: usize, end: usize) -> Box<dyn Command> {
    Box::new(TrimCommand::new(start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_keeps_only_selection() {
        let mut doc = Document {
            channels: vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        let mut cmd = TrimCommand::new(1, 4);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![2.0, 3.0, 4.0]]);
        assert!(doc.dirty);
        assert_eq!(doc.cursor, 0);
    }

    #[test]
    fn execute_then_undo_restores_original() {
        let mut doc = Document {
            channels: vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        let original = doc.channels.clone();
        let mut cmd = TrimCommand::new(1, 4);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }

    #[test]
    fn trim_entire_file_is_no_op() {
        let mut doc = Document {
            channels: vec![vec![1.0, 2.0, 3.0]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        let original = doc.channels.clone();
        let mut cmd = TrimCommand::new(0, 3);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, original);
        assert!(doc.dirty);
    }
}
