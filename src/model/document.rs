use std::path::PathBuf;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}

impl Document {
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn len_samples(&self) -> usize {
        self.channels.first().map(|c| c.len()).unwrap_or(0)
    }
}
