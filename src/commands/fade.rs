use crate::model::command::Command;
use crate::model::document::Document;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeCurve {
    Exp,
    Log,
    Linear,
}

impl FadeCurve {
    pub fn next(&self) -> Self {
        match self {
            FadeCurve::Exp => FadeCurve::Log,
            FadeCurve::Log => FadeCurve::Linear,
            FadeCurve::Linear => FadeCurve::Exp,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            FadeCurve::Exp => "Exp",
            FadeCurve::Log => "Log",
            FadeCurve::Linear => "Linear",
        }
    }
}

#[derive(Debug)]
pub struct FadeCommand {
    range: (usize, usize),
    fade_in: bool,
    curve: FadeCurve,
    original: Option<Vec<Vec<f32>>>,
}

impl FadeCommand {
    pub fn new(start: usize, end: usize, fade_in: bool, curve: FadeCurve) -> Self {
        Self {
            range: (start.min(end), start.max(end)),
            fade_in,
            curve,
            original: None,
        }
    }
}

impl Command for FadeCommand {
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        if start + 1 >= end {
            return;
        }
        let len = end - start;
        let mut original = Vec::with_capacity(doc.channels.len());
        for channel in &mut doc.channels {
            let end = end.min(channel.len());
            let start = start.min(end);
            original.push(channel[start..end].to_vec());
            for (i, s) in channel[start..end].iter_mut().enumerate() {
                let t = i as f32 / (len - 1) as f32;
                let envelope = match (self.fade_in, self.curve) {
                    (true, FadeCurve::Exp) => {
                        t * t
                    }
                    (false, FadeCurve::Exp) => {
                        1.0 - t * t
                    }
                    (true, FadeCurve::Log) => {
                        (1.0 + t * 9.0).log10()
                    }
                    (false, FadeCurve::Log) => {
                        1.0 - (1.0 + t * 9.0).log10()
                    }
                    (true, FadeCurve::Linear) => t,
                    (false, FadeCurve::Linear) => 1.0 - t,
                };
                *s = envelope.max(0.0).min(1.0) * *s;
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
        if self.fade_in { "Fade In" } else { "Fade Out" }
    }
}

pub fn fade_command(start: usize, end: usize, fade_in: bool, curve: FadeCurve) -> Box<dyn Command> {
    Box::new(FadeCommand::new(start, end, fade_in, curve))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_in_linear_ramps_up() {
        let mut doc = Document {
            channels: vec![vec![1.0; 100]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        let mut cmd = FadeCommand::new(0, 100, true, FadeCurve::Linear);
        cmd.execute(&mut doc);
        assert!((doc.channels[0][0] - 0.0).abs() < 0.01);
        assert!((doc.channels[0][99] - 1.0).abs() < 0.01);
        assert!(doc.channels[0][50] > 0.45 && doc.channels[0][50] < 0.55);
    }

    #[test]
    fn fade_out_linear_ramps_down() {
        let mut doc = Document {
            channels: vec![vec![1.0; 100]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        let mut cmd = FadeCommand::new(0, 100, false, FadeCurve::Linear);
        cmd.execute(&mut doc);
        assert!((doc.channels[0][0] - 1.0).abs() < 0.01);
        assert!((doc.channels[0][99] - 0.0).abs() < 0.01);
    }

    #[test]
    fn fade_then_undo_restores_original() {
        let mut doc = Document {
            channels: vec![vec![1.0; 50]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        let original = doc.channels.clone();
        let mut cmd = FadeCommand::new(0, 50, true, FadeCurve::Linear);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }

    #[test]
    fn fade_exp_curve_bounds() {
        let mut doc = Document {
            channels: vec![vec![1.0; 50]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        let mut cmd = FadeCommand::new(0, 50, true, FadeCurve::Exp);
        cmd.execute(&mut doc);
        for s in &doc.channels[0] {
            assert!(*s >= 0.0 && *s <= 1.0);
        }
    }

    #[test]
    fn fade_log_curve_bounds() {
        let mut doc = Document {
            channels: vec![vec![0.8; 30]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        let mut cmd = FadeCommand::new(0, 30, false, FadeCurve::Log);
        cmd.execute(&mut doc);
        for s in &doc.channels[0] {
            assert!(*s >= 0.0 && *s <= 0.8);
        }
    }

    #[test]
    fn fade_with_zero_escapes() {
        let mut doc = Document {
            channels: vec![vec![1.0; 5]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        };
        // start + 1 >= end → early return, no change
        let mut cmd = FadeCommand::new(0, 1, true, FadeCurve::Linear);
        cmd.execute(&mut doc);
        assert!((doc.channels[0][0] - 1.0).abs() < 0.01);
    }
}
