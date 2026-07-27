use crate::model::command::Command;
use crate::model::document::{Document, Marker};

#[derive(Debug)]
pub struct TrimCommand {
    range: (usize, usize),
    before: Option<Vec<Vec<f32>>>,
    after: Option<Vec<Vec<f32>>>,
    markers_before: Option<Vec<Marker>>,
    /// Snapshotted for the same reason as `markers_before`: trimming drops every mark outside
    /// the kept region, which re-basing alone can't undo.
    head_tail_marks_before: Option<Vec<usize>>,
}

impl TrimCommand {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            range: (start.min(end), start.max(end)),
            before: None,
            after: None,
            markers_before: None,
            head_tail_marks_before: None,
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
        // Head/tail marks get the identical treatment. Dropping the marks outside the kept
        // region can leave an odd count, which flips the Head/Tail role of everything after
        // the trim point — that is the honest result (the segments they described are gone),
        // and the DISTMORE dialog's own "needs at least 2 pairs" check is what catches it.
        self.head_tail_marks_before = Some(doc.head_tail_marks.clone());
        doc.head_tail_marks.retain(|&m| m >= start && m <= end);
        for m in &mut doc.head_tail_marks {
            *m -= start;
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
            restored.extend_from_slice(channel);
            restored.extend_from_slice(&after[i]);
            *channel = restored;
        }
        if let Some(markers) = self.markers_before.take() {
            doc.markers = markers;
        }
        if let Some(marks) = self.head_tail_marks_before.take() {
            doc.head_tail_marks = marks;
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

    /// Trim keeps only the marks inside the kept region, re-based to the new origin, and undo
    /// puts the dropped ones back — the same contract ordinary markers already had.
    #[test]
    fn trim_rebases_head_tail_marks_and_undo_restores_the_dropped_ones() {
        let mut doc = Document {
            head_tail_marks: vec![5, 25, 40, 90],
            channels: vec![vec![0.0; 100]],
            ..Default::default()
        };
        let mut cmd = TrimCommand::new(20, 50);
        cmd.execute(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![5, 20], "kept and re-based to the new origin");

        cmd.undo(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![5, 25, 40, 90]);
    }

    #[test]
    fn trim_keeps_only_selection() {
        let mut doc = Document {
            head_tail_marks: Vec::new(),
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
        let mut cmd = TrimCommand::new(1, 4);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![2.0, 3.0, 4.0]]);
        assert!(doc.dirty);
        assert_eq!(doc.cursor, 0);
    }

    #[test]
    fn execute_then_undo_restores_original() {
        let mut doc = Document {
            head_tail_marks: Vec::new(),
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
        let mut cmd = TrimCommand::new(1, 4);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }

    #[test]
    fn trim_entire_file_is_no_op() {
        let mut doc = Document {
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
        };
        let original = doc.channels.clone();
        let mut cmd = TrimCommand::new(0, 3);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, original);
        assert!(doc.dirty);
    }
}
