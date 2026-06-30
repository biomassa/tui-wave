use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};

/// Plays a `Document`'s sample data directly (no decode step needed — it's already f32),
/// incrementing a shared atomic frame counter as it yields samples. The counter runs on
/// rodio's internal mixing thread, which lets the UI thread poll sample-accurate playback
/// position lock-free, without a channel round-trip per redraw.
///
/// When `loop_start`/`loop_end` are `Some`, playback wraps from `loop_end` back to
/// `loop_start` indefinitely instead of stopping at the end of the data.
pub struct DocumentSource {
    data: Arc<Vec<Vec<f32>>>,
    sample_rate: SampleRate,
    channel_count: ChannelCount,
    frame_index: usize,
    channel_cursor: usize,
    position: Arc<AtomicUsize>,
    /// Shared playback flag. Cleared when this source reaches its natural end (rodio has no
    /// end-of-source callback, and otherwise the flag would stay `true` after a non-looping
    /// track finished, so the UI thought playback was still running — Space then "paused" a
    /// stopped track instead of replaying it).
    playing: Arc<AtomicBool>,
    loop_start: Option<usize>,
    loop_end: Option<usize>,
}

impl DocumentSource {
    pub fn new_looped(
        data: Arc<Vec<Vec<f32>>>,
        sample_rate: u32,
        start_frame: usize,
        position: Arc<AtomicUsize>,
        playing: Arc<AtomicBool>,
        loop_start: Option<usize>,
        loop_end: Option<usize>,
    ) -> Self {
        let channel_count =
            NonZero::new(data.len().max(1) as u16).unwrap_or(NonZero::<u16>::MIN);
        let sample_rate = NonZero::new(sample_rate.max(1)).unwrap_or(NonZero::<u32>::MIN);
        position.store(start_frame, Ordering::Relaxed);
        Self {
            data,
            sample_rate,
            channel_count,
            frame_index: start_frame,
            channel_cursor: 0,
            position,
            playing,
            loop_start,
            loop_end,
        }
    }
}

impl Iterator for DocumentSource {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<rodio::Sample> {
        let total_frames = self.data.first().map(|c| c.len()).unwrap_or(0);

        if self.frame_index >= total_frames
            || self.loop_end.is_some_and(|le| self.frame_index >= le)
        {
            if let (Some(ls), Some(le)) = (self.loop_start, self.loop_end) {
                if total_frames > 0 && ls < le && le <= total_frames {
                    self.frame_index = ls;
                } else {
                    self.playing.store(false, Ordering::Relaxed);
                    return None;
                }
            } else {
                self.playing.store(false, Ordering::Relaxed);
                return None;
            }
        }

        let value = self.data[self.channel_cursor][self.frame_index];
        self.channel_cursor += 1;
        if self.channel_cursor >= self.data.len() {
            self.channel_cursor = 0;
            self.frame_index += 1;
            self.position.store(self.frame_index, Ordering::Relaxed);
        }
        Some(value as rodio::Sample)
    }
}

impl Source for DocumentSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channel_count
    }

    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_playing_at_natural_end() {
        let data = Arc::new(vec![vec![0.1f32, 0.2, 0.3]]);
        let position = Arc::new(AtomicUsize::new(0));
        let playing = Arc::new(AtomicBool::new(true));
        let mut source =
            DocumentSource::new_looped(data, 44100, 0, position, playing.clone(), None, None);
        let yielded = std::iter::from_fn(|| source.next()).count();
        assert_eq!(yielded, 3);
        assert!(
            !playing.load(Ordering::Relaxed),
            "a non-looping source must clear `playing` when it reaches the end"
        );
    }

    #[test]
    fn looping_source_keeps_playing_set() {
        let data = Arc::new(vec![vec![0.1f32, 0.2, 0.3, 0.4]]);
        let position = Arc::new(AtomicUsize::new(0));
        let playing = Arc::new(AtomicBool::new(true));
        let mut source =
            DocumentSource::new_looped(data, 44100, 0, position, playing.clone(), Some(1), Some(3));
        // A valid loop never ends; pulling well past the loop region must keep yielding and
        // must never clear `playing` (the natural-end signal must not fire on a loop wrap).
        for _ in 0..1000 {
            assert!(source.next().is_some(), "a looping source should never return None");
        }
        assert!(playing.load(Ordering::Relaxed), "looping must leave `playing` true");
    }
}
