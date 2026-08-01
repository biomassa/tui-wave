use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};

use crate::model::dsp;

/// Plays a `Document`'s sample data directly (no decode step needed — it's already f32),
/// incrementing a shared atomic frame counter as it yields samples. The counter runs on
/// rodio's internal mixing thread, which lets the UI thread poll sample-accurate playback
/// position lock-free, without a channel round-trip per redraw.
///
/// When `loop_start`/`loop_end` are `Some`, playback wraps from `loop_end` back to
/// `loop_start` indefinitely instead of stopping at the end of the data.
///
/// A document with 3 or more channels is **folded down to stereo** as it plays (see
/// `dsp::downmix_frame`) rather than handed to the device as-is: a 56-channel source on a stereo
/// device is not something the device can render, and what a mixer does with the surplus channels
/// is its own business — dropping them, wrapping them, or refusing. Folding here means what is
/// heard is defined by this app and is the same on every device.
pub struct DocumentSource {
    data: Arc<Vec<Vec<f32>>>,
    sample_rate: SampleRate,
    channel_count: ChannelCount,
    /// Channels actually emitted — 2 when folding down, else the source's own count. Kept beside
    /// `channel_count` as a `usize` because every index and frame-advance test uses it.
    out_channels: usize,
    /// The current frame's fold-down, computed once when its first leg is asked for. `None` when
    /// this source passes channels through and there is nothing to compute.
    folded: Option<(f32, f32)>,
    /// Linear form of `dsp::PLAYBACK_CEILING_DB`, resolved once here rather than per frame —
    /// `db_to_linear` is a `powf`, and this runs on the mixing thread at the sample rate.
    ceiling: f32,
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
        let out_channels = dsp::playback_channels(data.len());
        let channel_count =
            NonZero::new(out_channels as u16).unwrap_or(NonZero::<u16>::MIN);
        let sample_rate = NonZero::new(sample_rate.max(1)).unwrap_or(NonZero::<u32>::MIN);
        position.store(start_frame, Ordering::Relaxed);
        Self {
            data,
            sample_rate,
            channel_count,
            out_channels,
            folded: None,
            ceiling: dsp::db_to_linear(dsp::PLAYBACK_CEILING_DB),
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

        let value = if self.out_channels < self.data.len() {
            // Folding down: compute both legs when the left one is asked for, so a frame's sum
            // is walked once rather than once per leg.
            let folded = match (self.channel_cursor, self.folded) {
                (0, _) | (_, None) => {
                    let pair = dsp::downmix_frame(&self.data, self.frame_index, self.ceiling);
                    self.folded = Some(pair);
                    pair
                }
                (_, Some(pair)) => pair,
            };
            if self.channel_cursor == 0 {
                folded.0
            } else {
                folded.1
            }
        } else {
            self.data[self.channel_cursor][self.frame_index]
        };
        self.channel_cursor += 1;
        if self.channel_cursor >= self.out_channels {
            self.channel_cursor = 0;
            self.frame_index += 1;
            self.folded = None;
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

    /// `loop_end` with no `loop_start` (what `AudioEngine::play_bounded`/`seek_bounded` send)
    /// must stop exactly at `loop_end` rather than wrapping or continuing to the file's
    /// actual end — this is what keeps Space from playing a selection past its own end when
    /// loop playback is off.
    #[test]
    fn loop_end_without_loop_start_stops_there_instead_of_wrapping_or_continuing() {
        let data = Arc::new(vec![vec![0.1f32, 0.2, 0.3, 0.4, 0.5]]);
        let position = Arc::new(AtomicUsize::new(0));
        let playing = Arc::new(AtomicBool::new(true));
        let mut source =
            DocumentSource::new_looped(data, 44100, 0, position, playing.clone(), None, Some(3));
        let yielded = std::iter::from_fn(|| source.next()).count();
        assert_eq!(yielded, 3, "must stop at loop_end, not continue to the file's actual end (5 frames)");
        assert!(
            !playing.load(Ordering::Relaxed),
            "a bounded (non-looping) source must clear `playing` once it hits loop_end"
        );
    }

    /// A 1- or 2-channel document is handed to the device exactly as it always was: same channel
    /// count, same samples, no limiter. This is the guarantee that the fold-down cannot change how
    /// an ordinary file sounds.
    #[test]
    fn mono_and_stereo_play_through_unchanged() {
        for data in [
            vec![vec![1.0f32, -1.0, 0.5]],
            vec![vec![1.0f32, 0.5], vec![-1.0, -0.5]],
        ] {
            let channels = data.len();
            let frames = data[0].len();
            let expected: Vec<f32> =
                (0..frames).flat_map(|f| (0..channels).map(move |c| (c, f))).map(|(c, f)| data[c][f]).collect();
            let source = DocumentSource::new_looped(
                Arc::new(data),
                44100,
                0,
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicBool::new(true)),
                None,
                None,
            );
            assert_eq!(source.channels().get() as usize, channels);
            let yielded: Vec<f32> = source.collect();
            assert_eq!(yielded, expected, "{channels}-channel audio must play back verbatim");
        }
    }

    /// 3+ channels are announced to the device as stereo and interleave as left, right, left…
    /// — odd-numbered channels summed left, even-numbered summed right.
    #[test]
    fn a_multichannel_document_plays_as_a_limited_stereo_fold_down() {
        let ceiling = crate::model::dsp::db_to_linear(crate::model::dsp::PLAYBACK_CEILING_DB);
        // ch1..ch5 constant, two frames each.
        let data = vec![
            vec![0.1f32, 0.1],
            vec![0.2, 0.2],
            vec![0.3, 0.3],
            vec![0.4, 0.4],
            vec![0.5, 0.5],
        ];
        let position = Arc::new(AtomicUsize::new(0));
        let source = DocumentSource::new_looped(
            Arc::new(data),
            44100,
            0,
            position.clone(),
            Arc::new(AtomicBool::new(true)),
            None,
            None,
        );
        assert_eq!(source.channels().get(), 2, "a 5-channel file is announced as stereo");
        let yielded: Vec<f32> = source.collect();

        let left = crate::model::dsp::tanh_limit(0.1 + 0.3 + 0.5, ceiling);
        let right = crate::model::dsp::tanh_limit(0.2 + 0.4, ceiling);
        assert_eq!(yielded.len(), 4, "two frames of stereo, not two frames of five channels");
        assert!((yielded[0] - left).abs() < 1e-6);
        assert!((yielded[1] - right).abs() < 1e-6);
        assert!((yielded[2] - left).abs() < 1e-6);
        assert!((yielded[3] - right).abs() < 1e-6);
        // The playhead still counts *frames*, so it stays in step with the waveform rather than
        // running at 5/2 speed (or 2/5) because the channel count changed under it.
        assert_eq!(position.load(Ordering::Relaxed), 2);
    }

    /// The fold-down must not disturb the loop/bounds logic, which is expressed in frames.
    #[test]
    fn a_folded_down_source_still_honours_its_bounds() {
        let data: Vec<Vec<f32>> = (0..6).map(|_| vec![0.1f32; 100]).collect();
        let mut source = DocumentSource::new_looped(
            Arc::new(data),
            44100,
            10,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(true)),
            None,
            Some(20),
        );
        let yielded = std::iter::from_fn(|| source.next()).count();
        assert_eq!(yielded, 20, "10 frames (10..20) of stereo = 20 samples");
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
