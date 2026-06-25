use std::ops::Range;
use std::path::PathBuf;

/// Search window for zero-crossing snapping, in samples on each side of the boundary.
const ZERO_CROSSING_MAX_OFFSET: usize = 256;

use super::selection::Selection;

/// A named position on the timeline (a WAV `cue ` point with an `adtl`/`labl` label).
/// `position` is a sample frame index. Markers ride along with the audio: editing samples
/// before a marker shifts it so it stays anchored to the same audible point.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub position: usize,
    pub label: String,
}

/// An open audio file. Holds no UI/audio-device state — pure data, fully unit-testable
/// without a terminal or audio backend.
pub struct Document {
    /// Deinterleaved samples, one Vec per channel, normalized to f32 in [-1.0, 1.0].
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
    pub selection: Option<Selection>,
    pub cursor: usize,
    pub dirty: bool,
    pub path: Option<PathBuf>,
    /// Timeline markers, kept sorted by position. Loaded from / saved to WAV cue chunks.
    pub markers: Vec<Marker>,
    /// Raw BWF `bext` chunk bytes, preserved verbatim across a load→save round-trip so
    /// editing a broadcast WAV doesn't strip its metadata. `None` for plain WAVs.
    pub bext: Option<Vec<u8>>,
}

impl Document {
    /// Adjust `pos` to the nearest zero crossing (sign change or near-zero sample)
    /// within a search window. Returns the original position if no crossing is found
    /// within the window or if the channel data is empty/insufficient.
    /// Check if ALL channels are satisfied at position `i` (zero, near-zero, or
    /// crossing) and whether they agree on snapping to `i` or `i+1`. Returns the
    /// consensus snapped position and whether all channels pass.
    fn channel_agreement(&self, i: usize) -> (usize, bool) {
        if self.channels.is_empty() {
            return (i, false);
        }
        // All channels must agree on the same snap target (i or i+1).
        // None: position is unusable.
        // Some(true): snap to i+1.
        // Some(false): snap to i.
        let mut consensus: Option<bool> = None;
        for ch in &self.channels {
            if i >= ch.len() {
                return (i, false);
            }
            if ch[i] == 0.0 {
                // True zero — always snaps to i regardless of other channels.
                consensus = Some(false);
                continue;
            }
            let near_zero = ch[i].abs() < 0.001;
            let is_crossing = i + 1 < ch.len()
                && (ch[i] > 0.0 && ch[i + 1] <= 0.0
                    || ch[i] < 0.0 && ch[i + 1] >= 0.0);
            if is_crossing && !near_zero {
                // This channel wants i+1. If another channel already said i, fail.
                if consensus == Some(false) {
                    return (i, false);
                }
                consensus = Some(true);
            } else if near_zero {
                // Snaps to i. If another channel already said i+1, fail.
                if consensus == Some(true) {
                    return (i, false);
                }
                consensus = Some(false);
            } else {
                return (i, false);
            }
        }
        match consensus {
            Some(true) => (i + 1, true),
            Some(false) => (i, true),
            None => (i, false),
        }
    }

    pub fn snap_to_zero_crossing(&self, pos: usize) -> usize {
        if self.channels.is_empty() || self.channels[0].is_empty()
            || pos >= self.channels[0].len()
        {
            return pos;
        }
        let search_start = pos.saturating_sub(ZERO_CROSSING_MAX_OFFSET);
        let search_end = (pos + ZERO_CROSSING_MAX_OFFSET).min(self.channels[0].len());

        let mut best = pos;
        let mut best_dist = usize::MAX;
        for i in search_start..search_end {
            let (snap_i, valid) = self.channel_agreement(i);
            if valid {
                let dist = snap_i.abs_diff(pos);
                if dist < best_dist {
                    best_dist = dist;
                    best = snap_i;
                }
                if self.channels.iter().all(|ch| ch[i] == 0.0) {
                    return i;
                }
            }
        }
        best
    }

    /// Snap both ends of a normalized (start <= end) range to zero crossings.
    pub fn snap_range_to_zero_crossing(&self, start: usize, end: usize) -> (usize, usize) {
        (self.snap_to_zero_crossing(start), self.snap_to_zero_crossing(end))
    }
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn len_samples(&self) -> usize {
        self.channels.first().map(|c| c.len()).unwrap_or(0)
    }

    /// Finds the next transient at or after `from` and returns the position to stop right
    /// before it, or `None` if none is found before end-of-file.
    ///
    /// A transient (in DSP terms: the percussive onset of a sound — a drum hit, a plucked
    /// string's pluck, a plosive consonant) is characterized by a sudden, fast rise in
    /// amplitude relative to whatever level came just before it. This is a simplified
    /// two-envelope onset detector, the same family of technique as a hardware "transient
    /// designer": the signal is divided into `TRANSIENT_FRAME_MS` analysis frames, each
    /// frame's RMS level (the loudest channel's) is compared in dB against a slow-moving
    /// background average of recent frames (an exponential moving average with a
    /// `TRANSIENT_BACKGROUND_TIME_CONSTANT_MS` time constant); the first frame whose level
    /// exceeds the background by `threshold_db` or more is the transient's onset, and its
    /// first sample is "right before" the transient — the finest precision a frame-based
    /// scan can offer without the cost of a per-sample analysis on long files.
    pub fn find_next_rising_edge(&self, from: usize, threshold_db: f32) -> Option<usize> {
        const TRANSIENT_FRAME_MS: f64 = 10.0;
        const TRANSIENT_BACKGROUND_TIME_CONSTANT_MS: f64 = 150.0;
        const EPS: f32 = 1e-6;

        let total = self.len_samples();
        if self.channels.is_empty() || from >= total {
            return None;
        }
        let frame_len = ((self.sample_rate as f64 * TRANSIENT_FRAME_MS / 1000.0).round() as usize).max(1);
        let alpha = (TRANSIENT_FRAME_MS / TRANSIENT_BACKGROUND_TIME_CONSTANT_MS).clamp(0.0, 1.0) as f32;

        let mut pos = from;
        let mut background: Option<f32> = None;
        while pos < total {
            let end = (pos + frame_len).min(total);
            let frame_level = self.frame_rms(pos, end).max(EPS);
            match background {
                None => background = Some(frame_level),
                Some(bg) => {
                    let rise_db = 20.0 * (frame_level / bg.max(EPS)).log10();
                    if rise_db >= threshold_db {
                        return Some(pos);
                    }
                    background = Some(bg * (1.0 - alpha) + frame_level * alpha);
                }
            }
            pos = end;
        }
        None
    }

    /// Finds every transient in the file by repeatedly applying `find_next_rising_edge`
    /// from each detected position onward — each call starts its background average fresh
    /// at the position it's given, so resuming from a found edge correctly looks for the
    /// *next* rise rather than re-triggering on the one just found. Used by "Auto-Insert
    /// Markers at Transients".
    pub fn find_all_rising_edges(&self, threshold_db: f32) -> Vec<usize> {
        let mut edges = Vec::new();
        let mut pos = 0;
        while let Some(edge) = self.find_next_rising_edge(pos, threshold_db) {
            edges.push(edge);
            pos = edge;
        }
        edges
    }

    /// RMS amplitude within `[start, end)`, taking the loudest channel — a transient in any
    /// one channel should be found, not averaged away by quieter channels.
    fn frame_rms(&self, start: usize, end: usize) -> f32 {
        self.channels
            .iter()
            .map(|channel| {
                let end = end.min(channel.len());
                if start >= end {
                    return 0.0;
                }
                let slice = &channel[start..end];
                let sum_sq: f32 = slice.iter().map(|&s| s * s).sum();
                (sum_sq / slice.len() as f32).sqrt()
            })
            .fold(0.0f32, f32::max)
    }

    /// Non-destructive copy of `range` across all channels, clamped to bounds.
    pub fn slice(&self, range: Range<usize>) -> Vec<Vec<f32>> {
        self.channels
            .iter()
            .map(|channel| {
                let end = range.end.min(channel.len());
                let start = range.start.min(end);
                channel[start..end].to_vec()
            })
            .collect()
    }

    /// Removes `range` from every channel in place and returns the removed samples (one
    /// Vec per channel), so the caller can store them for undo. Markers shift with the cut:
    /// those after the range move left, those inside it collapse to the cut point.
    pub fn remove_range(&mut self, range: Range<usize>) -> Vec<Vec<f32>> {
        let len = self.len_samples();
        let start = range.start.min(len);
        let end = range.end.min(len);
        let removed = end.saturating_sub(start);
        let out = self
            .channels
            .iter_mut()
            .map(|channel| {
                let end = range.end.min(channel.len());
                let start = range.start.min(end);
                channel.splice(start..end, std::iter::empty()).collect()
            })
            .collect();
        for m in &mut self.markers {
            if m.position >= end {
                m.position -= removed;
            } else if m.position > start {
                m.position = start;
            }
        }
        out
    }

    /// Inserts `data` (one Vec per channel) at `at` in every channel. Channels beyond
    /// `data`'s length are left untouched. Markers at or after `at` shift right by the
    /// inserted length so they stay anchored to the same audio.
    pub fn insert_range(&mut self, at: usize, data: Vec<Vec<f32>>) {
        let count = data.first().map(|c| c.len()).unwrap_or(0);
        for (channel, new_samples) in self.channels.iter_mut().zip(data) {
            let at = at.min(channel.len());
            channel.splice(at..at, new_samples);
        }
        for m in &mut self.markers {
            if m.position >= at {
                m.position += count;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(samples: Vec<f32>) -> Document {
        Document {
            channels: vec![samples],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
        }
    }

    /// Builds a signal of constant-amplitude segments, each `frames` analysis frames long
    /// (frame = 441 samples at the test's 44100 sample rate / 10ms), so transient tests can
    /// reason in whole frames instead of raw sample counts.
    fn segments(segments: &[(f32, usize)]) -> Vec<f32> {
        const FRAME_LEN: usize = 441;
        segments
            .iter()
            .flat_map(|&(level, frames)| std::iter::repeat(level).take(frames * FRAME_LEN))
            .collect()
    }

    #[test]
    fn find_next_rising_edge_detects_a_loud_transient_after_quiet() {
        // 20 quiet frames, then a sudden jump to 0.5 — a ~34dB rise, well past the 6dB
        // default threshold.
        let d = doc(segments(&[(0.01, 20), (0.5, 30)]));
        let pos = d.find_next_rising_edge(0, 6.0).expect("should find the transient");
        assert_eq!(pos, 20 * 441, "should stop right at the start of the loud frame");
    }

    #[test]
    fn find_next_rising_edge_ignores_the_starting_level_and_finds_a_later_rise() {
        // Searching from the start of the medium-loud section: no transient is reported
        // for entering it (there's no prior baseline to compare against at the search
        // start), but the later jump to very-loud (a ~12dB rise) is found.
        let d = doc(segments(&[(0.01, 20), (0.5, 20), (2.0, 30)]));
        let start_of_medium = 20 * 441;
        let pos = d.find_next_rising_edge(start_of_medium, 6.0).expect("should find the second rise");
        assert_eq!(pos, 40 * 441);
    }

    #[test]
    fn find_next_rising_edge_returns_none_for_constant_level() {
        let d = doc(segments(&[(0.3, 50)]));
        assert_eq!(d.find_next_rising_edge(0, 6.0), None);
    }

    #[test]
    fn find_next_rising_edge_respects_the_threshold() {
        // A ~6dB rise (0.5 -> ~1.0) should clear a 3dB threshold but not a 9dB one.
        let d = doc(segments(&[(0.5, 20), (1.0, 20)]));
        assert!(d.find_next_rising_edge(0, 3.0).is_some());
        assert_eq!(d.find_next_rising_edge(0, 9.0), None);
    }

    #[test]
    fn find_next_rising_edge_from_past_end_is_none() {
        let d = doc(segments(&[(0.5, 5)]));
        assert_eq!(d.find_next_rising_edge(d.len_samples(), 6.0), None);
    }

    #[test]
    fn find_all_rising_edges_finds_every_transient_in_order() {
        // Three distinct rises: quiet -> medium (20), medium -> loud (40), loud -> very
        // loud (60), each comfortably above the 6dB default threshold.
        let d = doc(segments(&[(0.01, 20), (0.1, 20), (1.0, 20), (8.0, 20)]));
        let edges = d.find_all_rising_edges(6.0);
        assert_eq!(edges, vec![20 * 441, 40 * 441, 60 * 441]);
    }

    #[test]
    fn find_all_rising_edges_is_empty_for_constant_level() {
        let d = doc(segments(&[(0.3, 50)]));
        assert!(d.find_all_rising_edges(6.0).is_empty());
    }

    #[test]
    fn slice_is_non_destructive() {
        let document = doc(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        let s = document.slice(1..3);
        assert_eq!(s, vec![vec![2.0, 3.0]]);
        assert_eq!(document.len_samples(), 5);
    }

    #[test]
    fn snap_to_zero_crossing_finds_sign_change() {
        let d = doc(vec![0.5, 0.3, 0.1, -0.1, -0.3, -0.5]);
        // Pos 2 (value 0.1) is just before the crossing at 2→3.
        // Snapping should find the zero crossing between 2 and 3.
        let snapped = d.snap_to_zero_crossing(2);
        assert_eq!(snapped, 3);
    }

    #[test]
    fn snap_to_zero_crossing_stays_at_zero() {
        let d = doc(vec![0.5, 0.0, -0.3, -0.5]);
        assert_eq!(d.snap_to_zero_crossing(1), 1);
    }

    #[test]
    fn snap_range_to_zero_crossing_adjusts_both_ends() {
        // A sine wave that crosses zero every ~2205 samples at 44.1kHz (10 Hz).
        let samples: Vec<f32> = (0..5000).map(|i| ((i as f32) * 0.001).sin()).collect();
        let d = doc(samples);
        // Pick a range that starts/ends away from zero crossings.
        let (snapped_start, snapped_end) = d.snap_range_to_zero_crossing(100, 4900);
        // The snapped range should still produce a valid non-empty range.
        assert!(snapped_start < snapped_end);
        assert!(snapped_end <= 5000);
    }

    #[test]
    fn remove_range_shifts_markers() {
        let mut document = doc(vec![0.0; 100]);
        document.markers = vec![
            Marker { position: 10, label: "a".into() },  // before cut
            Marker { position: 30, label: "b".into() },  // inside cut [20,40)
            Marker { position: 60, label: "c".into() },  // after cut
        ];
        document.remove_range(20..40); // removes 20 samples
        assert_eq!(document.markers[0].position, 10); // unchanged
        assert_eq!(document.markers[1].position, 20); // collapsed to cut point
        assert_eq!(document.markers[2].position, 40); // shifted left by 20
    }

    #[test]
    fn insert_range_shifts_markers() {
        let mut document = doc(vec![0.0; 50]);
        document.markers = vec![
            Marker { position: 10, label: "a".into() },
            Marker { position: 30, label: "b".into() },
        ];
        document.insert_range(20, vec![vec![0.0; 5]]); // insert 5 at 20
        assert_eq!(document.markers[0].position, 10); // before insert, unchanged
        assert_eq!(document.markers[1].position, 35); // after insert, +5
    }

    #[test]
    fn remove_then_insert_round_trips() {
        let mut document = doc(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        let removed = document.remove_range(1..3);
        assert_eq!(document.channels, vec![vec![1.0, 4.0, 5.0]]);
        document.insert_range(1, removed);
        assert_eq!(document.channels, vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
    }
}
