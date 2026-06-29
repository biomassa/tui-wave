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

    pub fn prev(&self) -> Self {
        match self {
            FadeCurve::Exp => FadeCurve::Linear,
            FadeCurve::Log => FadeCurve::Exp,
            FadeCurve::Linear => FadeCurve::Log,
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

/// The envelope value at normalized position `t` (0.0 at the fade's start, 1.0 at its end)
/// for a given direction/curve — shared by `FadeCommand` and `TechnicalFadesCommand` so the
/// two can never drift apart on what "an exponential fade" actually computes.
fn fade_envelope(t: f32, fade_in: bool, curve: FadeCurve) -> f32 {
    let envelope = match (fade_in, curve) {
        (true, FadeCurve::Exp) => t * t,
        (false, FadeCurve::Exp) => 1.0 - t * t,
        (true, FadeCurve::Log) => (1.0 + t * 9.0).log10(),
        (false, FadeCurve::Log) => 1.0 - (1.0 + t * 9.0).log10(),
        (true, FadeCurve::Linear) => t,
        (false, FadeCurve::Linear) => 1.0 - t,
    };
    envelope.clamp(0.0, 1.0)
}

/// Applies `fade_envelope` over `doc.channels[start..end]` in place.
fn apply_fade_envelope(doc: &mut Document, start: usize, end: usize, fade_in: bool, curve: FadeCurve) {
    let len = end - start;
    for channel in &mut doc.channels {
        let end = end.min(channel.len());
        let start = start.min(end);
        for (i, s) in channel[start..end].iter_mut().enumerate() {
            let t = i as f32 / (len - 1) as f32;
            *s *= fade_envelope(t, fade_in, curve);
        }
    }
}

impl Command for FadeCommand {
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        if start + 1 >= end {
            return;
        }
        let mut original = Vec::with_capacity(doc.channels.len());
        for channel in &doc.channels {
            let end = end.min(channel.len());
            let start = start.min(end);
            original.push(channel[start..end].to_vec());
        }
        apply_fade_envelope(doc, start, end, self.fade_in, self.curve);
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

/// A short exponential fade in at the very start of the file and fade out at the very
/// end — the standard "technical fade" applied before bouncing/exporting to mask the
/// abrupt sample-value discontinuity a hard cut to/from silence would otherwise leave at
/// the file's boundaries (an audible click). Always operates on the whole file regardless
/// of any active selection, and is one undo step for both ends.
#[derive(Debug)]
pub struct TechnicalFadesCommand {
    fade_len: usize,
    original_head: Option<Vec<Vec<f32>>>,
    original_tail: Option<Vec<Vec<f32>>>,
}

impl TechnicalFadesCommand {
    pub fn new(fade_len: usize) -> Self {
        Self { fade_len, original_head: None, original_tail: None }
    }
}

impl Command for TechnicalFadesCommand {
    fn execute(&mut self, doc: &mut Document) {
        let total = doc.len_samples();
        if total == 0 {
            return;
        }
        // Clamp so a file shorter than two fades doesn't try to fade past its own midpoint
        // twice; the two fades may then overlap, which is harmless (their envelopes just
        // multiply) for a file this short.
        let fade_len = self.fade_len.min(total).max(1);
        let head_end = fade_len.min(total);
        let tail_start = total.saturating_sub(fade_len);

        let snapshot = |doc: &Document, start: usize, end: usize| -> Vec<Vec<f32>> {
            doc.channels.iter().map(|c| c[start.min(c.len())..end.min(c.len())].to_vec()).collect()
        };
        self.original_head = Some(snapshot(doc, 0, head_end));
        self.original_tail = Some(snapshot(doc, tail_start, total));

        if head_end > 1 {
            apply_fade_envelope(doc, 0, head_end, true, FadeCurve::Exp);
        }
        if total - tail_start > 1 {
            apply_fade_envelope(doc, tail_start, total, false, FadeCurve::Exp);
        }
        doc.selection = None;
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        let total = doc.len_samples();
        let fade_len = self.fade_len.min(total).max(1);
        let head_end = fade_len.min(total);
        let tail_start = total.saturating_sub(fade_len);

        if let Some(orig) = &self.original_head {
            for (channel, orig) in doc.channels.iter_mut().zip(orig.iter()) {
                let len = head_end.min(channel.len()).min(orig.len());
                channel[..len].copy_from_slice(&orig[..len]);
            }
        }
        if let Some(orig) = &self.original_tail {
            for (channel, orig) in doc.channels.iter_mut().zip(orig.iter()) {
                let end = total.min(channel.len());
                let start = tail_start.min(end);
                let len = (end - start).min(orig.len());
                channel[start..start + len].copy_from_slice(&orig[..len]);
            }
        }
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Technical Fades"
    }
}

pub fn technical_fades_command(fade_len: usize) -> Box<dyn Command> {
    Box::new(TechnicalFadesCommand::new(fade_len))
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
            bits_per_sample: 32,
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
            bits_per_sample: 32,
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
            bits_per_sample: 32,
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
            bits_per_sample: 32,
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
            bits_per_sample: 32,
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
            bits_per_sample: 32,
            bext: None,
        };
        // start + 1 >= end → early return, no change
        let mut cmd = FadeCommand::new(0, 1, true, FadeCurve::Linear);
        cmd.execute(&mut doc);
        assert!((doc.channels[0][0] - 1.0).abs() < 0.01);
    }

    fn loud_doc(len: usize) -> Document {
        Document {
            channels: vec![vec![1.0; len]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        }
    }

    #[test]
    fn technical_fades_ramps_both_ends_and_leaves_the_middle_untouched() {
        let mut doc = loud_doc(1000);
        let mut cmd = TechnicalFadesCommand::new(10);
        cmd.execute(&mut doc);

        assert!((doc.channels[0][0] - 0.0).abs() < 0.01, "head should fade from silence");
        assert!((doc.channels[0][9] - 1.0).abs() < 0.01, "head fade should reach full volume by its end");
        assert!((doc.channels[0][500] - 1.0).abs() < 0.001, "the middle must be untouched");
        assert!((doc.channels[0][990] - 1.0).abs() < 0.01, "tail fade should start at full volume");
        assert!((doc.channels[0][999] - 0.0).abs() < 0.01, "tail should fade to silence");
    }

    #[test]
    fn technical_fades_then_undo_restores_original_exactly() {
        let mut doc = loud_doc(1000);
        let original = doc.channels.clone();
        let mut cmd = TechnicalFadesCommand::new(10);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }

    #[test]
    fn technical_fades_ignores_selection_and_covers_the_whole_file() {
        let mut doc = loud_doc(1000);
        doc.selection = Some(crate::model::selection::Selection { start: 200, end: 300 });
        let mut cmd = TechnicalFadesCommand::new(10);
        cmd.execute(&mut doc);
        assert_eq!(doc.selection, None, "technical fades should clear any selection, not act on it");
        assert!((doc.channels[0][0] - 0.0).abs() < 0.01);
        assert!((doc.channels[0][999] - 0.0).abs() < 0.01);
    }

    #[test]
    fn technical_fades_on_a_very_short_file_does_not_panic() {
        let mut doc = loud_doc(3);
        let mut cmd = TechnicalFadesCommand::new(10); // longer than the whole file
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
    }
}
