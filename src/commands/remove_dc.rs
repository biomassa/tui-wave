//! Remove DC Offset — subtract the level each channel is centred on.
//!
//! A DC offset is a *constant* bias contributed by the capture chain (ADC reference, a
//! coupling capacitor, a preamp), so the correction is a constant too: measure the level,
//! subtract it. That makes this exact for what it targets and phase-transparent. It cannot
//! follow a baseline that *drifts* — that is what `HighPassCommand` is for, and the two are
//! deliberately separate commands rather than one with a mode, so the name always describes
//! what happened.
//!
//! What "the level" means is the one real choice here, and it is `dsp::DcEstimator`: the
//! median by default, because the mean is dominated by a waveform's own asymmetry on real
//! material. That distinction is not theoretical in this app — it is a fixed user report,
//! recorded on the enum.

use crate::model::command::Command;
use crate::model::document::Document;
use crate::model::dsp::{self, DcEstimator};

#[derive(Debug)]
pub struct RemoveDcCommand {
    estimator: DcEstimator,
    /// The offset subtracted from each channel, indexed by channel. Recorded on execute and
    /// added back on undo.
    ///
    /// This is the whole undo state — **no sample data is copied**, unlike every other
    /// amplitude command here (`NormalizeCommand`, `GainCommand`). On a 56-channel take that
    /// is 224 bytes against the hundreds of megabytes a range copy would cost, which is the
    /// reason this command is worth having as a subtraction rather than as a filter.
    ///
    /// The price is that undo restores the samples to within a rounding error rather than
    /// bit-exactly: `(s - m) + m` is not identically `s` in f32. The error is at most one ULP
    /// per sample (~6e-8 near full scale, below a 24-bit LSB) and does not accumulate across
    /// repeated undo/redo, since each redo re-measures from the current samples.
    offsets: Option<Vec<f32>>,
}

impl RemoveDcCommand {
    pub fn new(estimator: DcEstimator) -> Self {
        Self { estimator, offsets: None }
    }
}

impl Default for RemoveDcCommand {
    fn default() -> Self {
        Self::new(DcEstimator::default())
    }
}

impl Command for RemoveDcCommand {
    /// Operates on the **whole file**, never the selection — the second command in the app to
    /// deliberately ignore `operation_range`, after Remove Empty Channels, and for the same
    /// kind of reason: a bias is a property of the recording chain, not of a moment in the
    /// take. Measuring a selection would also make the correction a step edge at each
    /// boundary, which is an audible click; and on a short selection of low-frequency content
    /// the measured "offset" is largely the waveform itself, so it would subtract real signal.
    ///
    /// Per channel, not one figure across all of them: two channels biased in opposite
    /// directions average toward zero, and a single shared correction would then fix neither.
    /// (`dc_offset_estimate`, which seeds the CDP process of the same name, deliberately pools
    /// instead — that binary shifts the whole file by one value, so a per-channel reading
    /// would describe an edit it cannot make.)
    fn execute(&mut self, doc: &mut Document) {
        // One scratch buffer for the whole sweep rather than one per channel — on a 56-channel
        // file that is 55 allocations of a channel's full length that never happen.
        let mut scratch: Vec<f32> = Vec::new();
        let offsets: Vec<f32> = doc
            .channels
            .iter()
            .map(|ch| dsp::channel_dc_offset(ch, self.estimator, &mut scratch))
            .collect();
        for (channel, &offset) in doc.channels.iter_mut().zip(offsets.iter()) {
            if offset == 0.0 {
                continue;
            }
            for s in channel.iter_mut() {
                *s -= offset;
            }
        }
        self.offsets = Some(offsets);
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        if let Some(ref offsets) = self.offsets {
            for (channel, &offset) in doc.channels.iter_mut().zip(offsets.iter()) {
                if offset == 0.0 {
                    continue;
                }
                for s in channel.iter_mut() {
                    *s += offset;
                }
            }
        }
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Remove DC Offset"
    }
}

pub fn remove_dc_command(estimator: DcEstimator) -> Box<dyn Command> {
    Box::new(RemoveDcCommand::new(estimator))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(channels: Vec<Vec<f32>>) -> Document {
        Document { channels, sample_rate: 48000, ..Default::default() }
    }

    #[test]
    fn each_channel_loses_its_own_offset() {
        // Opposite biases: the single pooled figure barber computes would be 0.0 here and
        // correct neither channel. Per-channel measurement fixes both.
        let mut doc = doc_with(vec![
            vec![0.2, 0.4, 0.2, 0.4],
            vec![-0.3, -0.1, -0.3, -0.1],
        ]);
        let mut cmd = RemoveDcCommand::new(DcEstimator::Mean);
        cmd.execute(&mut doc);

        for channel in &doc.channels {
            let mean = dsp::channel_mean(channel);
            assert!(mean.abs() < 1e-6, "channel mean should be ~0 after the edit, was {mean}");
        }
        // The shape is untouched — only the offset moved. Compared with a tolerance because
        // subtracting an f32 offset is not exact; see `offsets`.
        for (&got, &want) in doc.channels[0].iter().zip(&[-0.1f32, 0.1, -0.1, 0.1]) {
            assert!((got - want).abs() < 1e-6, "expected ~{want}, got {got}");
        }
        assert!(doc.dirty);
    }

    /// The reason the median is the default, as a test rather than only as a comment: an
    /// asymmetric waveform with **no** DC offset at all. Short positive lobes against long,
    /// deep negative ones drag the mean well below zero, so subtracting it lifts the whole
    /// file — silence included — off the zero line. This is the shape behind the original user
    /// report; the median leaves it where it is.
    #[test]
    fn an_asymmetric_waveform_with_no_offset_is_left_alone_by_the_median() {
        // One period in ten samples: a brief +0.9 spike, a stretch of rest, then a long deep
        // negative lobe. Most samples sit at zero, so that is where the signal is centred —
        // but the deep negative side outweighs the narrow spike, dragging the mean to -0.11.
        let period: Vec<f32> = vec![0.9, 0.0, 0.0, 0.0, 0.0, 0.0, -0.5, -0.5, -0.5, -0.5];
        let samples: Vec<f32> = period.iter().cycle().take(10_000).copied().collect();

        let mean = dsp::channel_mean(&samples);
        assert!(mean < -0.01, "the shape must actually fool the mean, which read {mean}");

        let mut doc = doc_with(vec![samples.clone()]);
        RemoveDcCommand::new(DcEstimator::Median).execute(&mut doc);
        assert_eq!(
            doc.channels[0], samples,
            "the median sees no offset here, so nothing should move"
        );

        // And the mean estimator demonstrably does move it — the behaviour the default avoids.
        let mut doc = doc_with(vec![samples.clone()]);
        RemoveDcCommand::new(DcEstimator::Mean).execute(&mut doc);
        assert_ne!(doc.channels[0], samples, "the mean shifts a file that had no offset");
    }

    /// ...while a real offset is still found, by both estimators. The median is not simply
    /// less sensitive — it is measuring the right thing.
    #[test]
    fn a_real_offset_is_found_by_both_estimators() {
        let base: Vec<f32> = (0..10_000)
            .map(|i| (i as f32 * 0.01).sin() * 0.5 + 0.3)
            .collect();
        for estimator in [DcEstimator::Median, DcEstimator::Mean] {
            let mut doc = doc_with(vec![base.clone()]);
            RemoveDcCommand::new(estimator).execute(&mut doc);
            let centre = dsp::channel_mean(&doc.channels[0]);
            assert!(
                centre.abs() < 0.05,
                "{estimator:?} should have removed the +0.3 bias, left {centre}"
            );
        }
    }

    #[test]
    fn undo_restores_the_samples() {
        let original = vec![vec![0.2, 0.4, 0.2, 0.4], vec![-0.3, -0.1, -0.3, -0.1]];
        let mut doc = doc_with(original.clone());
        let mut cmd = RemoveDcCommand::new(DcEstimator::Mean);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);

        for (restored, orig) in doc.channels.iter().zip(original.iter()) {
            for (&r, &o) in restored.iter().zip(orig.iter()) {
                assert!((r - o).abs() < 1e-6, "expected ~{o}, got {r}");
            }
        }
    }

    /// The undo state is one f32 per channel and nothing else — the property that makes this
    /// affordable on the 30+ channel files the app is built for. A regression to storing
    /// sample copies would be invisible in behaviour and expensive in memory, so it is
    /// asserted rather than only documented.
    #[test]
    fn undo_state_does_not_grow_with_the_file() {
        let mut short = doc_with(vec![vec![0.5; 16]]);
        let mut long = doc_with(vec![vec![0.5; 1_000_000]]);
        let mut a = RemoveDcCommand::new(DcEstimator::Median);
        let mut b = RemoveDcCommand::new(DcEstimator::Median);
        a.execute(&mut short);
        b.execute(&mut long);
        assert_eq!(a.offsets.as_ref().map(Vec::len), Some(1));
        assert_eq!(b.offsets.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn an_already_centred_channel_is_left_alone() {
        let original = vec![vec![-0.5, 0.5, -0.5, 0.5]];
        let mut doc = doc_with(original.clone());
        let mut cmd = RemoveDcCommand::new(DcEstimator::Median);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, original);
    }

    /// An empty document has no channel to measure; the command must not panic or leave
    /// `offsets` unset in a way undo would then trip over.
    #[test]
    fn an_empty_document_is_a_no_op() {
        let mut doc = doc_with(Vec::new());
        let mut cmd = RemoveDcCommand::new(DcEstimator::Median);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert!(doc.channels.is_empty());
    }
}
