//! Playback for a disk-backed document — the streaming counterpart to
//! [`super::source::DocumentSource`].
//!
//! A streamed buffer has no `Vec<Vec<f32>>` to hand the mixer (that is the whole point: a 30GB
//! take does not fit), so the audio has to come off disk *while* it plays. Doing that read inside
//! the mixer's `next()` would put blocking disk I/O on the audio callback — one 56-channel block
//! is ~1.8MB, and a seek that lands badly is an audible dropout — and it would contend with the
//! render path for the same file handle on every frame.
//!
//! So the read happens on its own thread, one block at a time, and what crosses to the mixer is
//! already-folded interleaved stereo in a bounded queue. The queue is the shock absorber: a slow
//! read eats into the read-ahead rather than into the output, and the mixer only ever moves
//! samples out of memory. It is bounded so that playing a 30GB file cannot slowly buffer a 30GB
//! ring — read-ahead is fixed at a couple of seconds regardless of file length.
//!
//! The fold-down itself is shared with the resident path (`dsp::playback_block`), so the same
//! audio sounds identical whichever way the file was opened.

use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, SendTimeoutError, Sender};
use rodio::{ChannelCount, SampleRate, Source};

use crate::model::dsp;
use crate::model::stream::StreamedSamples;

/// Frames read from disk, folded, and queued as one block.
///
/// 8192 frames is ~170ms at 48kHz — long enough that the per-read overhead (a seek, a mutex, one
/// `read_all_channels` pass) is negligible against the transfer, short enough that the ring holds
/// several of them and that stopping playback discards very little work. On a 56-channel float
/// file one block is 1.8MB off disk and 64KB after the fold.
const BLOCK_FRAMES: usize = 8_192;

/// Blocks the ring holds before the reader has to wait.
///
/// 12 blocks is ~2 seconds of read-ahead — enough to ride out a slow seek or a competing read
/// from the render path, and ~786KB of folded stereo, which is nothing beside the pyramid this
/// mode already keeps. Note the *raw* read is never buffered: only the folded result is, so the
/// ring's size is independent of the source's channel count.
const RING_BLOCKS: usize = 12;

/// How long `next()` waits for a block before giving up and ending playback.
///
/// The reader keeps ~2s ahead, so reaching this means it is genuinely stuck (a disk that stopped
/// answering, a file pulled out from under us) rather than merely behind. Ending playback is the
/// right response: blocking the mixer thread indefinitely would take the audio device down with
/// it and leave `Stop` with nothing to interrupt.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the reader waits on a full ring before re-checking whether it has been cancelled.
///
/// Playback that is paused leaves the ring full and the reader parked in `send`; without a
/// timeout it would only notice a `Stop` when the mixer next drained a block, which for a paused
/// player is never.
const SEND_POLL: Duration = Duration::from_millis(100);

/// One block of playback-ready audio: interleaved, folded, and tagged with where it came from.
///
/// `start_frame` travels with the samples rather than being tracked by the mixer, because the
/// reader is the only side that knows about loop wraparound — carrying it means the playhead
/// follows a loop exactly, with no second copy of the wrap logic to keep in step.
struct PlaybackBlock {
    start_frame: usize,
    samples: Vec<f32>,
}

/// Drains [`PlaybackBlock`]s produced by the reader thread, yielding samples to rodio.
pub struct StreamedSource {
    rx: Receiver<PlaybackBlock>,
    current: Option<PlaybackBlock>,
    /// Index into `current.samples`, in samples (not frames).
    cursor: usize,
    channel_count: ChannelCount,
    out_channels: usize,
    sample_rate: SampleRate,
    position: Arc<AtomicUsize>,
    playing: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

impl StreamedSource {
    /// Spawns the reader for `stream` and returns the source that drains it.
    ///
    /// `loop_start`/`loop_end` mean exactly what they do for `DocumentSource`: `Some`/`Some`
    /// wraps indefinitely, a lone `loop_end` stops there, neither plays to end of file.
    pub fn start(
        stream: Arc<StreamedSamples>,
        sample_rate: u32,
        start_frame: usize,
        position: Arc<AtomicUsize>,
        playing: Arc<AtomicBool>,
        loop_start: Option<usize>,
        loop_end: Option<usize>,
    ) -> Self {
        // Captured once, here: it is announced to the device and cannot change afterwards, so a
        // channel map edited mid-playback (Remove Empty Channels) alters what is heard on the
        // *next* play rather than desynchronising this one. Same rule the resident engine's
        // `reload` already follows.
        let out_channels = dsp::playback_channels(stream.channel_count());
        let channel_count = NonZero::new(out_channels as u16).unwrap_or(NonZero::<u16>::MIN);
        let sample_rate_nz = NonZero::new(sample_rate.max(1)).unwrap_or(NonZero::<u32>::MIN);
        position.store(start_frame, Ordering::Relaxed);

        let (tx, rx) = bounded(RING_BLOCKS);
        let cancel = Arc::new(AtomicBool::new(false));
        spawn_reader(stream, out_channels, start_frame, loop_start, loop_end, tx, cancel.clone());

        Self {
            rx,
            current: None,
            cursor: 0,
            channel_count,
            out_channels,
            sample_rate: sample_rate_nz,
            position,
            playing,
            cancel,
        }
    }
}

impl StreamedSource {
    /// The flag that stops this source's reader thread, so the engine can cancel it *before*
    /// handing the source to rodio's player and losing its own reference to it.
    ///
    /// Dropping the source sets the same flag, but nothing here controls when rodio actually
    /// drops a cleared source — and until it does, the old reader would go on competing with the
    /// new one for the same file handle.
    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}

impl Iterator for StreamedSource {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<rodio::Sample> {
        loop {
            if let Some(block) = self.current.as_ref() {
                if let Some(&value) = block.samples.get(self.cursor) {
                    self.cursor += 1;
                    // Once per whole frame, so the playhead counts frames however many channels
                    // the fold-down emits.
                    if self.cursor % self.out_channels == 0 {
                        let frame = block.start_frame + self.cursor / self.out_channels;
                        self.position.store(frame, Ordering::Relaxed);
                    }
                    return Some(value as rodio::Sample);
                }
            }
            match self.rx.recv_timeout(STALL_TIMEOUT) {
                Ok(block) => {
                    self.current = Some(block);
                    self.cursor = 0;
                }
                // Disconnected is the ordinary end of playback: the reader finished the file (or
                // the bounded range) and dropped its sender. Timeout is the stall guard.
                Err(RecvTimeoutError::Disconnected) | Err(RecvTimeoutError::Timeout) => {
                    self.current = None;
                    self.playing.store(false, Ordering::Relaxed);
                    return None;
                }
            }
        }
    }
}

impl Source for StreamedSource {
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

impl Drop for StreamedSource {
    /// Dropping the source drops the receiver, which already makes the reader's next `send`
    /// fail — but a reader parked on a full ring would not learn that until its send timed out,
    /// and one blocked mid-read would keep the file handle busy meanwhile. Setting the flag
    /// tells it directly.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Reads `stream` block by block from `start_frame`, folds each block to `out_channels`, and
/// hands it to `tx` until the range is exhausted, the receiver goes away, or `cancel` is set.
fn spawn_reader(
    stream: Arc<StreamedSamples>,
    out_channels: usize,
    start_frame: usize,
    loop_start: Option<usize>,
    loop_end: Option<usize>,
    tx: Sender<PlaybackBlock>,
    cancel: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let ceiling = dsp::db_to_linear(dsp::PLAYBACK_CEILING_DB);
        let total = stream.len_samples();
        let end = loop_end.unwrap_or(total).min(total);
        // A loop is only honoured if it describes a real range; otherwise this falls through to
        // playing once and stopping, rather than spinning on a zero-length wrap.
        let wrap_to = match (loop_start, loop_end) {
            (Some(ls), Some(le)) if ls < le && le <= total => Some(ls),
            _ => None,
        };
        let mut frame = start_frame;
        // Both reused across blocks: `read_all_channels` refills `decoded` in place, and a
        // 56-channel block is ~1.8MB that would otherwise be reallocated several times a second.
        let mut decoded: Vec<Vec<f32>> = Vec::new();
        let mut interleaved: Vec<f32> = Vec::with_capacity(BLOCK_FRAMES * out_channels);

        while !cancel.load(Ordering::Relaxed) {
            if frame >= end {
                match wrap_to {
                    Some(ls) => frame = ls,
                    None => break,
                }
            }
            let want = BLOCK_FRAMES.min(end - frame);
            let read = match stream.read_all_channels(frame as u64, want, &mut decoded) {
                Ok(read) => read,
                // A read that fails mid-playback ends it: there is no partial answer worth
                // playing, and the alternative is a thread spinning on a broken handle.
                Err(_) => break,
            };
            if read == 0 {
                break;
            }
            interleaved.clear();
            dsp::playback_block(&decoded, read, out_channels, ceiling, &mut interleaved);
            let mut block = PlaybackBlock {
                start_frame: frame,
                samples: std::mem::take(&mut interleaved),
            };
            loop {
                match tx.send_timeout(block, SEND_POLL) {
                    Ok(()) => break,
                    Err(SendTimeoutError::Timeout(returned)) => {
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        block = returned;
                    }
                    Err(SendTimeoutError::Disconnected(_)) => return,
                }
            }
            interleaved = Vec::with_capacity(BLOCK_FRAMES * out_channels);
            frame += read;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A float WAV whose channel `c`, frame `f` sample is `(c + 1) / 100 + f / 1000` — small
    /// enough that a fold-down of a few channels stays under the limiter's knee, so a test can
    /// assert on plain sums.
    fn wav(dir: &std::path::Path, channels: usize, frames: usize) -> std::path::PathBuf {
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&3u16.to_le_bytes());
        body.extend_from_slice(&(channels as u16).to_le_bytes());
        body.extend_from_slice(&48000u32.to_le_bytes());
        body.extend_from_slice(&((48000 * channels * 4) as u32).to_le_bytes());
        body.extend_from_slice(&((channels * 4) as u16).to_le_bytes());
        body.extend_from_slice(&32u16.to_le_bytes());
        body.extend_from_slice(b"data");
        body.extend_from_slice(&((frames * channels * 4) as u32).to_le_bytes());
        for f in 0..frames {
            for c in 0..channels {
                body.extend_from_slice(&sample(c, f).to_le_bytes());
            }
        }
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        let p = dir.join(format!("play{channels}x{frames}.wav"));
        std::fs::File::create(&p).unwrap().write_all(&out).unwrap();
        p
    }

    fn sample(channel: usize, frame: usize) -> f32 {
        (channel as f32 + 1.0) / 100.0 + frame as f32 / 1000.0
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tuiwave_play_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn start(
        path: &std::path::Path,
        from: usize,
        loop_start: Option<usize>,
        loop_end: Option<usize>,
    ) -> (StreamedSource, Arc<AtomicUsize>, Arc<AtomicBool>) {
        let stream = Arc::new(StreamedSamples::open(path).unwrap());
        let position = Arc::new(AtomicUsize::new(0));
        let playing = Arc::new(AtomicBool::new(true));
        let source = StreamedSource::start(
            stream,
            48000,
            from,
            position.clone(),
            playing.clone(),
            loop_start,
            loop_end,
        );
        (source, position, playing)
    }

    /// The core equivalence: a streamed multichannel file plays exactly the samples the same
    /// audio would play resident. The two paths share `dsp::playback_block`, and this is what
    /// pins that they also agree on channel count, frame order and range.
    #[test]
    fn a_streamed_file_plays_the_same_fold_down_a_resident_one_would() {
        let dir = tmp("equiv");
        let frames = 3_000;
        let channels = 7;
        let path = wav(&dir, channels, frames);

        let (source, _, playing) = start(&path, 0, None, None);
        assert_eq!(source.channels().get(), 2, "7 channels are announced as stereo");
        let got: Vec<f32> = source.collect();

        // The same audio as a resident document, folded by the per-frame path.
        let resident: Vec<Vec<f32>> =
            (0..channels).map(|c| (0..frames).map(|f| sample(c, f)).collect()).collect();
        let ceiling = dsp::db_to_linear(dsp::PLAYBACK_CEILING_DB);
        let mut want: Vec<f32> = Vec::new();
        for f in 0..frames {
            let (l, r) = dsp::downmix_frame(&resident, f, ceiling);
            want.push(l);
            want.push(r);
        }

        assert_eq!(got.len(), frames * 2, "one stereo frame per source frame");
        assert_eq!(got, want, "streamed playback must be sample-identical to resident");
        assert!(!playing.load(Ordering::Relaxed), "reaching the end clears `playing`");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Playback spans many blocks (3000 frames is well past `BLOCK_FRAMES` when it is small, and
    /// this file is deliberately larger than one), and the playhead must count *frames* across
    /// every one of them — not samples, and not restart per block.
    #[test]
    fn the_playhead_counts_frames_across_block_boundaries() {
        let dir = tmp("pos");
        let frames = BLOCK_FRAMES * 2 + 137;
        let path = wav(&dir, 5, frames);

        let (mut source, position, _) = start(&path, 0, None, None);
        // Pull one whole frame at a time and check the counter tracks it.
        for expected in 1..=(BLOCK_FRAMES + 10) {
            source.next().unwrap();
            source.next().unwrap();
            assert_eq!(
                position.load(Ordering::Relaxed),
                expected,
                "after {expected} stereo frames the playhead must read {expected}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Starting part-way in reads from there, and the playhead is that absolute frame — not zero,
    /// and not an offset into the block that happened to be read first.
    #[test]
    fn playback_starts_at_the_requested_frame() {
        let dir = tmp("from");
        let path = wav(&dir, 4, 2_000);
        let (mut source, position, _) = start(&path, 1_500, None, None);
        assert_eq!(position.load(Ordering::Relaxed), 1_500, "before a single sample is pulled");

        let ceiling = dsp::db_to_linear(dsp::PLAYBACK_CEILING_DB);
        let left = source.next().unwrap();
        let right = source.next().unwrap();
        assert!((left - dsp::tanh_limit(sample(0, 1500) + sample(2, 1500), ceiling)).abs() < 1e-6);
        assert!((right - dsp::tanh_limit(sample(1, 1500) + sample(3, 1500), ceiling)).abs() < 1e-6);
        assert_eq!(position.load(Ordering::Relaxed), 1_501);

        let rest: Vec<f32> = source.collect();
        assert_eq!(rest.len(), (2_000 - 1_501) * 2, "and it plays to the end of the file");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A bounded range (`loop_end` with no `loop_start`) stops there instead of running to the
    /// end of the file — the "play the selection once" case.
    #[test]
    fn a_bounded_range_stops_at_its_end() {
        let dir = tmp("bounded");
        let path = wav(&dir, 6, 5_000);
        let (source, _, playing) = start(&path, 100, None, Some(600));
        assert_eq!(source.count(), 500 * 2, "frames 100..600, as stereo");
        assert!(!playing.load(Ordering::Relaxed));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A loop wraps forever, and the playhead follows the wrap rather than running past the loop
    /// end — the reader tags every block with its true start frame precisely so it can.
    #[test]
    fn a_loop_wraps_and_the_playhead_follows_it() {
        let dir = tmp("loop");
        let path = wav(&dir, 8, 4_000);
        let (mut source, position, playing) = start(&path, 1_000, Some(1_000), Some(1_100));

        // Three times round a 100-frame loop, plus a bit.
        for _ in 0..(100 * 3 + 25) * 2 {
            assert!(source.next().is_some(), "a looping source never ends");
        }
        assert!(playing.load(Ordering::Relaxed), "and never clears `playing`");
        let at = position.load(Ordering::Relaxed);
        assert!(
            (1_000..=1_100).contains(&at),
            "the playhead stays inside the loop, got {at}"
        );
        assert_eq!(at, 1_025, "and lands exactly where the wrap count puts it");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A 1- or 2-channel streamed file is not folded and not limited — same rule as the resident
    /// path, checked here because the streamed side decides its channel count separately.
    #[test]
    fn a_stereo_streamed_file_is_not_folded() {
        let dir = tmp("stereo");
        let path = wav(&dir, 2, 500);
        let (source, _, _) = start(&path, 0, None, None);
        assert_eq!(source.channels().get(), 2);
        let got: Vec<f32> = source.collect();
        let want: Vec<f32> =
            (0..500).flat_map(|f| [sample(0, f), sample(1, f)]).collect();
        assert_eq!(got, want, "stereo passes through verbatim, interleaved");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Dropping the source must stop the reader rather than leaving a thread pulling a 30GB file
    /// through a queue nobody is draining. Observable through the file handle: once the reader is
    /// gone, `bytes_read` stops climbing.
    #[test]
    fn dropping_the_source_stops_the_reader() {
        let dir = tmp("cancel");
        let path = wav(&dir, 4, BLOCK_FRAMES * 40);
        let stream = Arc::new(StreamedSamples::open(&path).unwrap());
        let position = Arc::new(AtomicUsize::new(0));
        let playing = Arc::new(AtomicBool::new(true));
        let mut source = StreamedSource::start(
            stream.clone(),
            48000,
            0,
            position,
            playing,
            None,
            None,
        );
        source.next();
        drop(source);

        // The reader may be mid-block; give it a moment to notice, then confirm it has stopped
        // for good rather than merely paused.
        thread::sleep(Duration::from_millis(300));
        let settled = stream.bytes_read();
        thread::sleep(Duration::from_millis(300));
        assert_eq!(stream.bytes_read(), settled, "a dropped source's reader must not read on");
        std::fs::remove_dir_all(&dir).ok();
    }
}
