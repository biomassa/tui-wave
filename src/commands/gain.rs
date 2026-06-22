use crate::model::command::Command;
use crate::model::document::Document;

#[derive(Debug)]
pub struct GainCommand {
    range: (usize, usize),
    original: Option<Vec<Vec<f32>>>,
    gain_db: f32,
    tanh_clip: bool,
}

impl GainCommand {
    pub fn new(start: usize, end: usize, gain_db: f32, tanh_clip: bool) -> Self {
        Self {
            range: (start.min(end), start.max(end)),
            original: None,
            gain_db,
            tanh_clip,
        }
    }
}

impl Command for GainCommand {
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        if start >= end {
            return;
        }
        let linear = 10.0f32.powf(self.gain_db / 20.0);
        let mut original = Vec::with_capacity(doc.channels.len());
        for channel in &mut doc.channels {
            let end = end.min(channel.len());
            let start = start.min(end);
            original.push(channel[start..end].to_vec());
            for s in &mut channel[start..end] {
                *s *= linear;
                if self.tanh_clip {
                    *s = s.tanh();
                }
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
        "Gain"
    }
}

pub fn gain_command(start: usize, end: usize, gain_db: f32, tanh_clip: bool) -> Box<dyn Command> {
    Box::new(GainCommand::new(start, end, gain_db, tanh_clip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_applies_linear_gain() {
        let mut doc = Document {
            channels: vec![vec![0.5, 0.3, 0.1]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
        };
        // +6.0206 dB → linear 2.0x → 0.5 → 1.0
        let mut cmd = GainCommand::new(0, 3, 6.0206, false);
        cmd.execute(&mut doc);
        assert!((doc.channels[0][0] - 1.0).abs() < 0.001);
        assert!((doc.channels[0][1] - 0.6).abs() < 0.001);
    }

    #[test]
    fn execute_tanh_clip_saturates() {
        let mut doc = Document {
            channels: vec![vec![2.0, -2.0, 0.5]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
        };
        // 0 dB = unity gain, but with tanh clip
        let mut cmd = GainCommand::new(0, 3, 0.0, true);
        cmd.execute(&mut doc);
        // tanh(2.0) ≈ 0.964, tanh(-2.0) ≈ -0.964, tanh(0.5) ≈ 0.462
        assert!((doc.channels[0][0] - 0.964).abs() < 0.001);
        assert!((doc.channels[0][1] - (-0.964)).abs() < 0.001);
        assert!((doc.channels[0][2] - 0.462).abs() < 0.002);
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
        };
        let original = doc.channels.clone();
        let mut cmd = GainCommand::new(0, 3, 6.0, false);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }
}
