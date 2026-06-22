use std::num::NonZero;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    loop_start: Option<usize>,
    loop_end: Option<usize>,
}

impl DocumentSource {
    pub fn new_looped(
        data: Arc<Vec<Vec<f32>>>,
        sample_rate: u32,
        start_frame: usize,
        position: Arc<AtomicUsize>,
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
                    return None;
                }
            } else {
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
