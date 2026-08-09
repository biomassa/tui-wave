//! High-Pass Filter — zero-phase 2nd-order Butterworth, applied to the operation range.
//!
//! The counterpart to `RemoveDcCommand`: where that subtracts a single constant and so fixes
//! a fixed capture-chain bias exactly, this removes everything below a cutoff and so follows
//! a baseline that *wanders* (thermal drift, wind loading, a slow crawl across a long take),
//! and doubles as a rumble filter. Two commands rather than one with a mode, so each name
//! describes exactly what it did to the audio.

use crate::model::command::Command;
use crate::model::document::Document;
use crate::model::dsp;

#[derive(Debug)]
pub struct HighPassCommand {
    range: (usize, usize),
    cutoff_hz: f32,
    /// The filtered range's original samples, per channel.
    ///
    /// A full copy, unlike `RemoveDcCommand`'s one-f32-per-channel state, and unavoidably so:
    /// filtering is not exactly invertible in f32 (the inverse filter amplifies by exactly the
    /// factor the forward one attenuated, so the rounding error at DC is unbounded), and a
    /// user's undo has to give back the take they had. The cost is the same one
    /// `NormalizeCommand` and `GainCommand` already pay, bounded by the resident-file budget.
    original: Option<Vec<Vec<f32>>>,
}

impl HighPassCommand {
    pub fn new(start: usize, end: usize, cutoff_hz: f32) -> Self {
        Self {
            range: (start.min(end), start.max(end)),
            cutoff_hz,
            original: None,
        }
    }
}

impl Command for HighPassCommand {
    /// Honors the selection through `operation_range`, like Normalize/Gain/Fade — filtering a
    /// region is a legitimate edit in its own right, unlike measuring a bias over one.
    ///
    /// Each channel is filtered **independently over the range only**, which means the filter
    /// sees the range as the whole signal: `dsp::high_pass_zero_phase` primes its state from
    /// the first sample rather than from silence, so the range's own opening level is not read
    /// as a step edge. What it cannot know is the audio on the far side of the boundary, so a
    /// selection edge that sits mid-waveform still lands a discontinuity there — the ordinary
    /// reason to make a selection at a zero crossing, which Snap to Zero already handles.
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        if start >= end {
            return;
        }
        // Bail before touching anything if the cutoff can't produce a filter, rather than
        // recording an undo entry for an edit that did nothing.
        let sample_rate = doc.sample_rate;
        if !self.cutoff_hz.is_finite()
            || self.cutoff_hz <= 0.0
            || self.cutoff_hz >= sample_rate as f32 / 2.0
        {
            return;
        }
        let mut original = Vec::with_capacity(doc.channels.len());
        for channel in &mut doc.channels {
            let end = end.min(channel.len());
            let start = start.min(end);
            original.push(channel[start..end].to_vec());
            dsp::high_pass_zero_phase(&mut channel[start..end], self.cutoff_hz, sample_rate);
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
        "High-Pass Filter"
    }
}

pub fn high_pass_command(start: usize, end: usize, cutoff_hz: f32) -> Box<dyn Command> {
    Box::new(HighPassCommand::new(start, end, cutoff_hz))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn doc_with(channels: Vec<Vec<f32>>) -> Document {
        Document { channels, sample_rate: RATE, ..Default::default() }
    }

    fn sine(freq: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                (2.0 * std::f32::consts::PI * freq * i as f32 / RATE as f32).sin() * 0.5
            })
            .collect()
    }

    /// The point of the filter: a constant bias goes, and it goes even when it *drifts*, which
    /// is the case mean-subtraction cannot answer.
    #[test]
    fn a_drifting_baseline_is_removed() {
        let len = 48_000;
        // A ramp from -0.3 to +0.3 — mean zero, so Remove DC Offset would find nothing to do.
        let drift: Vec<f32> =
            (0..len).map(|i| -0.3 + 0.6 * i as f32 / len as f32).collect();
        let mut doc = doc_with(vec![drift]);
        let mut cmd = HighPassCommand::new(0, len, 20.0);
        cmd.execute(&mut doc);

        // Sample the interior, away from the range edges where the filter has no context.
        let interior = &doc.channels[0][2_000..len - 2_000];
        let worst = interior.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(worst < 0.01, "drift should be gone, worst residual was {worst}");
    }

    /// A tone well inside the passband must come through essentially untouched — both in level
    /// and, because the filter is zero-phase, in alignment.
    #[test]
    fn a_passband_tone_survives_in_level_and_in_phase() {
        let len = 24_000;
        let input = sine(1_000.0, len);
        let mut doc = doc_with(vec![input.clone()]);
        let mut cmd = HighPassCommand::new(0, len, 20.0);
        cmd.execute(&mut doc);

        let out = &doc.channels[0];
        // Compared sample-by-sample, not by peak: a phase shift would leave the peak intact
        // while moving every sample, so this is what actually pins "zero-phase".
        let worst = input[2_000..len - 2_000]
            .iter()
            .zip(&out[2_000..len - 2_000])
            .fold(0.0f32, |m, (&a, &b)| m.max((a - b).abs()));
        assert!(worst < 0.01, "1 kHz should pass unchanged, worst deviation was {worst}");
    }

    #[test]
    fn a_tone_below_the_cutoff_is_attenuated() {
        let len = 48_000;
        let mut doc = doc_with(vec![sine(5.0, len)]);
        let mut cmd = HighPassCommand::new(0, len, 100.0);
        cmd.execute(&mut doc);

        let interior = &doc.channels[0][4_000..len - 4_000];
        let peak = interior.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        // 5 Hz against a 100 Hz corner is well over four octaves down at 24 dB/oct.
        assert!(peak < 0.01, "5 Hz should be far down, peak was {peak}");
    }

    /// Priming the filter state from the first sample (rather than from zero) is what keeps a
    /// DC-biased range from answering with a decaying thump at its head. Without it the first
    /// output sample would be roughly the full bias.
    #[test]
    fn a_dc_biased_range_does_not_open_with_a_transient() {
        let len = 10_000;
        let mut doc = doc_with(vec![vec![0.5; len]]);
        let mut cmd = HighPassCommand::new(0, len, 20.0);
        cmd.execute(&mut doc);

        let head = doc.channels[0][..64].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(head < 1e-3, "no startup transient expected, head peaked at {head}");
    }

    #[test]
    fn undo_restores_the_range_exactly() {
        let len = 4_096;
        let original = sine(50.0, len);
        let mut doc = doc_with(vec![original.clone()]);
        let mut cmd = HighPassCommand::new(0, len, 200.0);
        cmd.execute(&mut doc);
        assert_ne!(doc.channels[0], original, "the filter should have changed something");
        cmd.undo(&mut doc);
        assert_eq!(doc.channels[0], original, "undo is a copy-back, so it is bit-exact");
    }

    #[test]
    fn only_the_selected_range_is_touched() {
        let mut doc = doc_with(vec![vec![0.5; 1_000]]);
        let mut cmd = HighPassCommand::new(200, 800, 20.0);
        cmd.execute(&mut doc);
        assert!(doc.channels[0][..200].iter().all(|&s| s == 0.5));
        assert!(doc.channels[0][800..].iter().all(|&s| s == 0.5));
        assert!(doc.channels[0][400].abs() < 1e-3, "the range itself was filtered");
    }

    /// A cutoff at or above Nyquist has no passband. Declining leaves the audio alone; the
    /// alternative — building a degenerate filter — would zero the take.
    #[test]
    fn a_cutoff_at_or_above_nyquist_changes_nothing() {
        let original = sine(1_000.0, 1_024);
        for cutoff in [RATE as f32 / 2.0, RATE as f32, 0.0, -5.0] {
            let mut doc = doc_with(vec![original.clone()]);
            let mut cmd = HighPassCommand::new(0, 1_024, cutoff);
            cmd.execute(&mut doc);
            assert_eq!(doc.channels[0], original, "cutoff {cutoff} should be refused");
            assert!(cmd.original.is_none(), "and should record no undo state");
        }
    }

    /// Each channel is filtered on its own data — a shared filter state would bleed one
    /// channel into the next, which on a 30-mic rig would be catastrophic and silent.
    ///
    /// Asserted as an equivalence rather than by tolerance against the input: a channel
    /// filtered alongside a wildly different neighbour must come out **bit-identical** to the
    /// same channel filtered by itself. That is the actual property, and unlike a
    /// "close to the original" check it needs no margin for the filter's own edge behaviour.
    #[test]
    fn channels_are_filtered_independently() {
        let len = 8_000;
        let victim = sine(1_000.0, len);

        let mut alone = doc_with(vec![victim.clone()]);
        HighPassCommand::new(0, len, 20.0).execute(&mut alone);

        let mut alongside = doc_with(vec![vec![0.5; len], victim]);
        HighPassCommand::new(0, len, 20.0).execute(&mut alongside);

        assert_eq!(
            alongside.channels[1], alone.channels[0],
            "a neighbour's DC bias must not reach this channel at all"
        );
    }
}
