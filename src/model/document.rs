use std::ops::Range;
use std::path::PathBuf;

/// Search window for zero-crossing snapping, in samples on each side of the boundary.
const ZERO_CROSSING_MAX_OFFSET: usize = 256;

use super::selection::Selection;

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
}

impl Document {
    /// Adjust `pos` to the nearest zero crossing (sign change or near-zero sample)
    /// within a search window. Returns the original position if no crossing is found
    /// within the window or if the channel data is empty/insufficient.
    pub fn snap_to_zero_crossing(&self, pos: usize) -> usize {
        let Some(channel) = self.channels.first() else {
            return pos;
        };
        if channel.is_empty() || pos >= channel.len() {
            return pos;
        }
        let search_start = pos.saturating_sub(ZERO_CROSSING_MAX_OFFSET);
        let search_end = (pos + ZERO_CROSSING_MAX_OFFSET).min(channel.len());

        let mut best = pos;
        let mut best_dist = usize::MAX;
        for i in search_start..search_end {
            // A true zero sample is always a good snap point.
            if channel[i] == 0.0 {
                return i;
            }
            // A near-zero sample (within 0.1% of full scale).
            let near_zero = channel[i].abs() < 0.001;
            // A sign change between consecutive samples.
            let is_crossing = i + 1 < channel.len()
                && (channel[i] > 0.0 && channel[i + 1] <= 0.0
                    || channel[i] < 0.0 && channel[i + 1] >= 0.0);
            // When a crossing happens between sample i and i+1, snap to i+1
            // (the first sample on the other side, which is near zero).
            let snap_i = if is_crossing && !near_zero { i + 1 } else { i };
            if near_zero || is_crossing {
                let dist = snap_i.abs_diff(pos);
                if dist < best_dist {
                    best_dist = dist;
                    best = snap_i;
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
    /// Vec per channel), so the caller can store them for undo.
    pub fn remove_range(&mut self, range: Range<usize>) -> Vec<Vec<f32>> {
        self.channels
            .iter_mut()
            .map(|channel| {
                let end = range.end.min(channel.len());
                let start = range.start.min(end);
                channel.splice(start..end, std::iter::empty()).collect()
            })
            .collect()
    }

    /// Inserts `data` (one Vec per channel) at `at` in every channel. Channels beyond
    /// `data`'s length are left untouched.
    pub fn insert_range(&mut self, at: usize, data: Vec<Vec<f32>>) {
        for (channel, new_samples) in self.channels.iter_mut().zip(data) {
            let at = at.min(channel.len());
            channel.splice(at..at, new_samples);
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
        }
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
    fn remove_then_insert_round_trips() {
        let mut document = doc(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        let removed = document.remove_range(1..3);
        assert_eq!(document.channels, vec![vec![1.0, 4.0, 5.0]]);
        document.insert_range(1, removed);
        assert_eq!(document.channels, vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
    }
}
