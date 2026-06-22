use std::ops::Range;
use std::path::PathBuf;

use super::selection::Selection;

/// An open audio file. Holds no UI/audio-device state — pure data, fully unit-testable
/// without a terminal or audio backend.
pub struct Document {
    /// Deinterleaved samples, one Vec per channel, normalized to f32 in [-1.0, 1.0].
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
    pub selection: Option<Selection>,
    pub playhead: usize,
    pub dirty: bool,
    pub path: Option<PathBuf>,
}

impl Document {
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
            playhead: 0,
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
    fn remove_then_insert_round_trips() {
        let mut document = doc(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        let removed = document.remove_range(1..3);
        assert_eq!(document.channels, vec![vec![1.0, 4.0, 5.0]]);
        document.insert_range(1, removed);
        assert_eq!(document.channels, vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
    }
}
