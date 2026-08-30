//! Auto-Trim Silence: keep the sounding parts of a take and drop the silence — at the ends
//! always, and between phrases when asked.
//!
//! The analysis lives in `model::silence`; this is the edit that acts on its result. Structured
//! as one `Command` rather than a Trim plus N Deletes plus a Fade because the three are one
//! decision to the user and must be one press of Ctrl+Z.
//!
//! **Every boundary is faded, not just the outer two.** Removing an internal gap creates a
//! splice in the middle of the take, and a splice between two pieces of a recording is exactly
//! where a click comes from. The same `Edge fade` that softens the head and tail therefore
//! applies at each join, which is why the fade is part of this command rather than something
//! the caller applies afterwards to the two ends it can see.

use crate::model::command::Command;
use crate::model::document::{Document, Marker};

#[derive(Debug)]
pub struct AutoTrimCommand {
    /// The outer region to keep, already padded and snapped by the caller — this command does
    /// no analysis of its own, so what it does is exactly what the dialog described.
    keep: (usize, usize),
    /// Absolute ranges to excise from inside `keep`, ascending and non-overlapping. Empty when
    /// internal gaps are being left alone.
    removed: Vec<(usize, usize)>,
    fade_samples: usize,
    /// Everything before `keep.0`, per channel.
    before: Option<Vec<Vec<f32>>>,
    /// Everything after `keep.1`, per channel.
    after: Option<Vec<Vec<f32>>>,
    /// The audio inside each excised range, per gap, per channel — what undo splices back.
    gap_audio: Option<Vec<Vec<Vec<f32>>>>,
    /// Each retained segment's leading and trailing samples as they were *before* the fade, per
    /// segment, per channel. The fade writes inside audio that survives, so unlike the removed
    /// pieces it cannot be undone by reassembling: these are the samples it overwrote.
    fade_edges: Option<Vec<(Vec<Vec<f32>>, Vec<Vec<f32>>)>>,
    markers_before: Option<Vec<Marker>>,
    head_tail_marks_before: Option<Vec<usize>>,
}

impl AutoTrimCommand {
    pub fn new(keep: (usize, usize), removed: Vec<(usize, usize)>, fade_samples: usize) -> Self {
        Self {
            keep: (keep.0.min(keep.1), keep.0.max(keep.1)),
            removed,
            fade_samples,
            before: None,
            after: None,
            gap_audio: None,
            fade_edges: None,
            markers_before: None,
            head_tail_marks_before: None,
        }
    }

    /// The retained pieces of `keep`, in order — `keep` minus every excised range.
    ///
    /// A gap touching either end of `keep` yields no empty segment, so a take whose first phrase
    /// is immediately followed by a long rest does not gain a zero-length piece to fade.
    fn segments(&self) -> Vec<(usize, usize)> {
        let (start, end) = self.keep;
        let mut out = Vec::with_capacity(self.removed.len() + 1);
        let mut at = start;
        for &(gs, ge) in &self.removed {
            if gs > at {
                out.push((at, gs));
            }
            at = at.max(ge);
        }
        if at < end {
            out.push((at, end));
        }
        out
    }

    /// Where each retained sample lands after the splice, for moving a marker.
    ///
    /// `None` for a position inside a removed gap: the audio it named is gone, so the mark has
    /// nothing left to point at. Same contract Trim already set for marks outside the kept
    /// region — a mark survives exactly when the sample it named does.
    fn remap(&self, pos: usize, segments: &[(usize, usize)]) -> Option<usize> {
        let mut offset = 0usize;
        for &(s, e) in segments {
            if pos >= s && pos <= e {
                return Some(offset + (pos - s));
            }
            offset += e - s;
        }
        None
    }
}

impl Command for AutoTrimCommand {
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.keep;
        if start >= end || end > doc.len_samples() || doc.channels.is_empty() {
            return;
        }
        let segments = self.segments();
        if segments.is_empty() {
            return;
        }

        self.before = Some(doc.channels.iter().map(|c| c[..start].to_vec()).collect());
        self.after = Some(doc.channels.iter().map(|c| c[end..].to_vec()).collect());
        self.gap_audio = Some(
            self.removed
                .iter()
                .map(|&(gs, ge)| doc.channels.iter().map(|c| c[gs..ge].to_vec()).collect())
                .collect(),
        );

        // Per segment, so a short phrase between two long rests cannot have its two ramps run
        // into each other and multiply its middle by both.
        let fades: Vec<usize> =
            segments.iter().map(|&(s, e)| self.fade_samples.min((e - s) / 2)).collect();
        self.fade_edges = Some(
            segments
                .iter()
                .zip(&fades)
                .map(|(&(s, e), &f)| {
                    (
                        doc.channels.iter().map(|c| c[s..s + f].to_vec()).collect(),
                        doc.channels.iter().map(|c| c[e - f..e].to_vec()).collect(),
                    )
                })
                .collect(),
        );

        for channel in &mut doc.channels {
            let mut out: Vec<f32> = Vec::with_capacity(
                segments.iter().map(|&(s, e)| e - s).sum::<usize>(),
            );
            for (&(s, e), &fade) in segments.iter().zip(&fades) {
                let at = out.len();
                out.extend_from_slice(&channel[s..e]);
                let len = e - s;
                for i in 0..fade {
                    // Linear in amplitude: over a few milliseconds at a boundary the snap has
                    // already placed at the quietest nearby sample, no curve is distinguishable
                    // from another.
                    let gain = i as f32 / fade as f32;
                    out[at + i] *= gain;
                    out[at + len - 1 - i] *= gain;
                }
            }
            *channel = out;
        }

        self.markers_before = Some(doc.markers.clone());
        self.head_tail_marks_before = Some(doc.head_tail_marks.clone());
        doc.markers = std::mem::take(&mut doc.markers)
            .into_iter()
            .filter_map(|m| {
                self.remap(m.position, &segments)
                    .map(|position| Marker { position, label: m.label })
            })
            .collect();
        doc.head_tail_marks = std::mem::take(&mut doc.head_tail_marks)
            .into_iter()
            .filter_map(|m| self.remap(m, &segments))
            .collect();

        doc.selection = None;
        doc.cursor = 0;
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        // Same early-return contract as `TrimCommand`: a no-op execute leaves these unset and
        // `History::apply` pushes the command regardless, so undo must be a no-op too rather
        // than a panic out of raw mode.
        let (Some(before), Some(after)) = (self.before.take(), self.after.take()) else { return };
        let gaps = self.gap_audio.take().unwrap_or_default();
        let edges = self.fade_edges.take().unwrap_or_default();
        let segments = self.segments();

        // Reassemble by walking the *original* timeline, interleaving the retained segments and
        // the removed gaps in position order. Emitting "segment then the gap after it" is wrong
        // whenever a gap sits flush against the start of `keep`: there is no segment before it,
        // so the first gap would be spliced in after the first segment instead of ahead of it,
        // and undo silently returned the audio reordered rather than restored. Found by an
        // ascending-ramp fixture, which is why the tests use one — with a constant-valued
        // buffer every wrong ordering compares equal.
        enum Piece {
            Segment(usize),
            Gap(usize),
        }
        let mut order: Vec<(usize, Piece)> = segments
            .iter()
            .enumerate()
            .map(|(i, &(s, _))| (s, Piece::Segment(i)))
            .chain(self.removed.iter().enumerate().map(|(i, &(s, _))| (s, Piece::Gap(i))))
            .collect();
        order.sort_by_key(|(start, _)| *start);

        for (ch, channel) in doc.channels.iter_mut().enumerate() {
            let mut restored: Vec<f32> = before.get(ch).cloned().unwrap_or_default();
            // Where each segment starts in the *spliced* buffer, so a segment's samples can be
            // found however the pieces are ordered on the original timeline.
            let mut segment_at = Vec::with_capacity(segments.len());
            let mut at = 0usize;
            for &(s, e) in &segments {
                segment_at.push(at);
                at += e - s;
            }
            for (_, piece) in &order {
                match piece {
                    Piece::Segment(i) => {
                        let (s, e) = segments[*i];
                        let from = segment_at[*i];
                        let mut samples = channel[from..from + (e - s)].to_vec();
                        // The fade is undone here, while this piece is still exactly the region
                        // the fade was applied to.
                        if let Some((head, tail)) = edges.get(*i) {
                            if let Some(h) = head.get(ch) {
                                samples[..h.len()].copy_from_slice(h);
                            }
                            if let Some(t) = tail.get(ch) {
                                let from = samples.len() - t.len();
                                samples[from..].copy_from_slice(t);
                            }
                        }
                        restored.extend_from_slice(&samples);
                    }
                    Piece::Gap(i) => {
                        if let Some(gap) = gaps.get(*i).and_then(|g| g.get(ch)) {
                            restored.extend_from_slice(gap);
                        }
                    }
                }
            }
            if let Some(tail) = after.get(ch) {
                restored.extend_from_slice(tail);
            }
            *channel = restored;
        }
        if let Some(markers) = self.markers_before.take() {
            doc.markers = markers;
        }
        if let Some(marks) = self.head_tail_marks_before.take() {
            doc.head_tail_marks = marks;
        }
        doc.cursor = self.keep.0;
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Auto-Trim Silence"
    }
}

pub fn auto_trim_command(
    keep: (usize, usize),
    removed: Vec<(usize, usize)>,
    fade_samples: usize,
) -> Box<dyn Command> {
    Box::new(AutoTrimCommand::new(keep, removed, fade_samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ramp, so every sample is distinguishable and a mis-assembled undo cannot pass by luck.
    fn ramp_doc(len: usize) -> Document {
        let a: Vec<f32> = (0..len).map(|i| i as f32 / len as f32).collect();
        let b: Vec<f32> = a.iter().map(|v| -v).collect();
        Document { channels: vec![a, b], sample_rate: 44_100, ..Default::default() }
    }

    #[test]
    fn it_keeps_the_region_and_undo_restores_every_sample() {
        let mut doc = ramp_doc(1000);
        let original = doc.channels.clone();
        let mut cmd = AutoTrimCommand::new((200, 800), vec![], 0);
        cmd.execute(&mut doc);
        assert_eq!(doc.len_samples(), 600);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original, "undo must restore the audio exactly");
    }

    /// The headline behaviour: an internal gap is excised and the take closes up.
    #[test]
    fn an_internal_gap_is_removed_and_undo_puts_it_back() {
        let mut doc = ramp_doc(1000);
        let original = doc.channels.clone();
        let mut cmd = AutoTrimCommand::new((100, 900), vec![(400, 500)], 0);
        cmd.execute(&mut doc);
        assert_eq!(doc.len_samples(), 800 - 100, "800 kept minus the 100-sample gap");
        // The join is seamless: sample 299 is source 399, sample 300 is source 500.
        assert!((doc.channels[0][299] - original[0][399]).abs() < 1e-6);
        assert!((doc.channels[0][300] - original[0][500]).abs() < 1e-6);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original, "undo restores the gap and both ends");
    }

    #[test]
    fn several_gaps_are_removed_in_one_edit() {
        let mut doc = ramp_doc(2000);
        let original = doc.channels.clone();
        let mut cmd = AutoTrimCommand::new((0, 2000), vec![(200, 300), (800, 1000), (1500, 1600)], 0);
        cmd.execute(&mut doc);
        assert_eq!(doc.len_samples(), 2000 - 100 - 200 - 100);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }

    /// Every join is faded, not only the outer two — a splice mid-take is exactly where a click
    /// comes from. See the module docs.
    #[test]
    fn each_retained_segment_gets_its_own_edge_fade() {
        let mut doc = ramp_doc(2000);
        let original = doc.channels.clone();
        let mut cmd = AutoTrimCommand::new((100, 1900), vec![(900, 1100)], 32);
        cmd.execute(&mut doc);
        let len = doc.len_samples();
        // Segment 1 is 100..900 (800 long), segment 2 is 1100..1900.
        assert!(doc.channels[0][0].abs() < 1e-6, "first segment fades in");
        assert!(doc.channels[0][799].abs() < 1e-6, "first segment fades out at the join");
        assert!(doc.channels[0][800].abs() < 1e-6, "second segment fades in from the join");
        assert!(doc.channels[0][len - 1].abs() < 1e-6, "last segment fades out");
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original, "undo restores every faded edge");
    }

    #[test]
    fn an_over_long_fade_is_clamped_per_segment() {
        let mut doc = ramp_doc(1000);
        let mut cmd = AutoTrimCommand::new((400, 500), vec![], 10_000);
        cmd.execute(&mut doc);
        let peak = doc.channels[0].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.2, "the middle must survive a silly fade length, peak {peak}");
    }

    /// A marker keeps pointing at the audio it named, wherever that audio moved to — and a
    /// marker whose audio was removed goes with it.
    #[test]
    fn markers_follow_their_audio_across_the_splice() {
        let mut doc = ramp_doc(1000);
        doc.markers = vec![
            Marker { position: 50, label: "before the range".into() },
            Marker { position: 300, label: "first segment".into() },
            Marker { position: 450, label: "inside the gap".into() },
            Marker { position: 700, label: "second segment".into() },
        ];
        let mut cmd = AutoTrimCommand::new((100, 900), vec![(400, 500)], 0);
        cmd.execute(&mut doc);
        let got: Vec<_> =
            doc.markers.iter().map(|m| (m.label.as_str(), m.position)).collect();
        assert_eq!(
            got,
            vec![("first segment", 200), ("second segment", 500)],
            "kept marks are remapped; the one inside the gap is gone with its audio"
        );
        cmd.undo(&mut doc);
        assert_eq!(doc.markers.len(), 4, "undo restores every mark");
        assert_eq!(doc.markers[0].position, 50);
    }

    #[test]
    fn head_tail_marks_follow_the_same_contract() {
        let mut doc = ramp_doc(1000);
        doc.head_tail_marks = vec![50, 300, 450, 700, 950];
        let mut cmd = AutoTrimCommand::new((100, 900), vec![(400, 500)], 0);
        cmd.execute(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![200, 500], "outside and in-gap marks dropped");
        cmd.undo(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![50, 300, 450, 700, 950]);
    }

    /// A gap flush against the start of the kept region must not produce an empty leading
    /// segment — an empty piece would be faded, measured and spliced for no reason.
    #[test]
    fn a_gap_touching_an_end_yields_no_empty_segment() {
        let mut doc = ramp_doc(1000);
        let original = doc.channels.clone();
        let mut cmd = AutoTrimCommand::new((100, 900), vec![(100, 200), (800, 900)], 8);
        cmd.execute(&mut doc);
        assert_eq!(cmd.segments(), vec![(200, 800)], "one segment, no empties");
        assert_eq!(doc.len_samples(), 600);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }

    #[test]
    fn a_degenerate_range_is_a_no_op_in_both_directions() {
        let mut doc = ramp_doc(100);
        let original = doc.channels.clone();
        let mut cmd = AutoTrimCommand::new((50, 50), vec![], 8);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, original);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
    }
}
