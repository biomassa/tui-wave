use crate::model::command::Command;
use crate::model::document::Document;

#[derive(Debug)]
pub struct NormalizeCommand {
    range: (usize, usize),
    /// Original sample data for undo.
    original: Option<Vec<Vec<f32>>>,
    target_db: f32,
}

impl NormalizeCommand {
    pub fn new(start: usize, end: usize, target_db: f32) -> Self {
        Self {
            range: (start.min(end), start.max(end)),
            original: None,
            target_db,
        }
    }
}

impl Command for NormalizeCommand {
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        if start >= end {
            return;
        }
        // Find peak across all channels in the range.
        let mut peak = 0.0f32;
        for channel in &doc.channels {
            let end = end.min(channel.len());
            let start = start.min(end);
            for &s in &channel[start..end] {
                peak = peak.max(s.abs());
            }
        }
        if peak < 0.0001 {
            return;
        }
        let target_linear = 10.0f32.powf(self.target_db / 20.0);
        let gain = target_linear / peak;
        let mut original = Vec::with_capacity(doc.channels.len());
        for channel in &mut doc.channels {
            let end = end.min(channel.len());
            let start = start.min(end);
            original.push(channel[start..end].to_vec());
            for s in &mut channel[start..end] {
                *s *= gain;
            }
        }
        self.original = Some(original);
        doc.selection = None;
        doc.cursor = start;
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        if let Some(ref original) = self.original {
            for (channel, orig) in doc.channels.iter_mut().zip(original.iter()) {
                let end = end.min(channel.len());
                let start = start.min(end);
                let len = (end - start).min(orig.len());
                channel[start..start + len].copy_from_slice(&orig[..len]);
            }
        }
        doc.selection = None;
        doc.cursor = start;
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Normalize"
    }
}

pub fn normalize_command(start: usize, end: usize, target_db: f32) -> Box<dyn Command> {
    Box::new(NormalizeCommand::new(start, end, target_db))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_normalizes_to_near_full_scale() {
        let mut doc = Document {
            channels: vec![vec![0.5, 0.3, 0.1, -0.2, -0.4]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        };
        // target_db = -0.446 → target_linear ≈ 0.95
        let mut cmd = NormalizeCommand::new(0, 5, -0.446);
        cmd.execute(&mut doc);
        // Peak was 0.5, so gain = 0.95/0.5 = 1.9
        // Sample 0: 0.5 * 1.9 ≈ 0.95 (should be the new peak)
        assert!((doc.channels[0][0] - 0.95).abs() < 0.001);
        assert!((doc.channels[0][1] - 0.57).abs() < 0.01);
    }

    #[test]
    fn execute_then_undo_restores_original() {
        let mut doc = Document {
            channels: vec![vec![0.5, 0.3, 0.1]],
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
        let mut cmd = NormalizeCommand::new(0, 3, -1.0);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }
}
