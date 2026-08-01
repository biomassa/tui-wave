//! Disk-backed, read-only samples for a file too large to hold in RAM, and the
//! [`SampleSource`] abstraction that lets the render path read either kind without caring
//! which it has.
//!
//! A 58-channel 32-bit-float take runs at 11.1 MB/s, so a 32-minute one is 20GB — and since
//! the working format is f32, that is 20GB resident, before the second copy the audio engine
//! would want and the pyramid built on top. What makes such a file viewable anyway is that the
//! waveform is drawn from a min/max pyramid (`ui::waveform_cache`) that is ~1/30 the size of
//! the samples: under 1GB for a 20GB file, which fits comfortably. Raw samples are then needed
//! only for the small exact edge scans at each column, and their volume falls as you zoom *out*
//! — so the reads this serves are always small.
//!
//! Read-only, deliberately. Every `Command` impl stores whole sample copies for undo, so an
//! edit at this scale would hold most of the file in the undo stack; `App::handle_action` gates
//! streamed documents to an allowlist rather than letting those paths near one.

use std::sync::{Mutex, RwLock};

use super::wavread::{WavFrames, WavInfo};

/// Frames one cached window covers.
///
/// Sized so a *whole visible range* fits in one window at any zoom where raw samples are read at
/// all. That is the property that matters, not the size itself: the render loop walks pane by pane
/// and each pane re-reads the same frame range, so a window too small to span the view is reloaded
/// once per pane. With six panes on screen that was 52MB per redraw of the same bytes.
///
/// 65536 frames of a 56-channel float file is 14.7MB of raw bytes, read once per scroll position
/// and then reused by every pane and every column.
pub const WINDOW_FRAMES: usize = 65_536;

/// How many windows are kept.
///
/// More than one because a visible range can straddle a window boundary, and the render walks that
/// range once per pane: with a single window the two halves would evict each other on every pane,
/// turning the boundary into a cliff far worse than the case it was meant to fix. Three covers
/// ~196k frames — any visible range at a zoom where raw samples are read at all — for ~44MB of raw
/// bytes on a 56-channel file.
const WINDOW_COUNT: usize = 3;

/// One cached window: the raw interleaved bytes, plus whichever channels have been decoded out of
/// them so far.
///
/// Raw bytes are cached rather than decoded samples because a read carries every channel whether
/// or not it is wanted, while *decoding* all 56 to draw six would cost more than the read. So the
/// transfer is shared and the decode is lazy — a window load decodes only the panes on screen.
struct WindowSet {
    /// Absolute frame index of the first frame in `raw`.
    base: usize,
    frames: usize,
    raw: Vec<u8>,
    /// Per *logical* channel, in channel-map order; `None` until that channel is first asked for.
    decoded: Vec<Option<Vec<f32>>>,
}

impl WindowSet {
    fn covers(&self, start: usize, end: usize) -> bool {
        start >= self.base && end <= self.base + self.frames
    }
}

/// A read-only document's samples, left on disk and read in windows on demand.
///
/// `channel_map` is what Remove Empty Channels edits: logical channel *i* reads source channel
/// `channel_map[i]`. Storing the indirection rather than rewriting anything means dropping 48
/// of 58 channels costs a `Vec<usize>` edit, not a 20GB rewrite — at the price of not being
/// undoable through `History`, which stores sample data it cannot have here.
///
/// Every field is interior-mutable so that all of this works through `&self`. That is not
/// incidental: `Document` holds an `Arc` of this, the render path borrows it while the document
/// is also consulted elsewhere, and requiring `&mut` would mean `Arc::get_mut` — which returns
/// `None` whenever any other handle happens to exist, turning "a clone is alive somewhere" into
/// a silent no-op at the exact moment the user asked for something.
pub struct StreamedSamples {
    /// `Mutex` because the render path holds `&Document` while reading, and reading needs to seek.
    ///
    /// Contended only lightly, and only during playback: the streamed playback reader
    /// (`audio::stream_source`) pulls one block every ~170ms on its own thread, so a redraw can
    /// queue behind one block read. Everything else that touches this is the single-threaded UI.
    frames: Mutex<WavFrames>,
    info: WavInfo,
    channel_map: RwLock<Vec<usize>>,
    /// Most-recently-used first; at most [`WINDOW_COUNT`].
    windows: Mutex<Vec<WindowSet>>,
    /// Scratch for reads too large to cache, kept alive so a per-frame read doesn't reallocate.
    scratch: Mutex<Vec<f32>>,
    /// Deinterleave target for [`Self::read_all_channels`], kept between calls. A 58-channel
    /// 64Ki-frame block is ~15MB; reallocating it per block over a 20GB file is ~1400 rounds of
    /// churn for no reason.
    demux: Mutex<Vec<Vec<f32>>>,
}

impl StreamedSamples {
    pub fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<StreamedSamples> {
        let frames = WavFrames::open(path)?;
        let info = frames.info();
        Ok(StreamedSamples {
            frames: Mutex::new(frames),
            info,
            channel_map: RwLock::new((0..info.channels).collect()),
            windows: Mutex::new(Vec::new()),
            scratch: Mutex::new(Vec::new()),
            demux: Mutex::new(Vec::new()),
        })
    }

    pub fn info(&self) -> WavInfo {
        self.info
    }

    /// Bytes this document has pulled off disk so far. See `WavFrames::bytes_read` — a read costs
    /// the whole interleaved frame however few channels are wanted, so this is the number that
    /// governs whether scrolling feels instant.
    #[cfg(test)]
    pub fn bytes_read(&self) -> u64 {
        self.frames.lock().map(|f| f.bytes_read()).unwrap_or(0)
    }

    /// Channels currently presented, i.e. after any Remove Empty Channels.
    pub fn channel_count(&self) -> usize {
        self.channel_map.read().map(|m| m.len()).unwrap_or(0)
    }

    /// The source channel logical channel `channel` reads from.
    fn source_channel(&self, channel: usize) -> Option<usize> {
        self.channel_map.read().ok()?.get(channel).copied()
    }

    pub fn len_samples(&self) -> usize {
        // A frame count that doesn't fit in `usize` can't be addressed by the viewport either;
        // saturating keeps the file viewable up to that bound instead of failing to open.
        usize::try_from(self.info.frame_count).unwrap_or(usize::MAX)
    }

    /// Source channel per logical channel, in order.
    pub fn channel_map(&self) -> Vec<usize> {
        self.channel_map.read().map(|m| m.clone()).unwrap_or_default()
    }

    /// Drops logical channels by index, keeping the rest in order.
    ///
    /// `keep` is called with the *logical* index, matching what the caller was just shown and what
    /// `dsp::channels_below_peaks` returns.
    ///
    /// Nothing about the audio changes — the file is open read-only and is never written. That is
    /// what makes this cheap to undo: the whole operation is an edit to a `Vec<usize>`, so
    /// `RemoveStreamedChannelsCommand` restores it by handing the old map back to
    /// [`Self::set_channel_map`]. (`RemoveChannelsCommand`, the resident equivalent, has to stash
    /// every removed channel's *samples* instead, which at these sizes would be most of the file —
    /// that constraint simply doesn't exist here.)
    pub fn retain_channels(&self, keep: impl Fn(usize) -> bool) {
        if let Ok(mut map) = self.channel_map.write() {
            let mut i = 0;
            map.retain(|_| {
                let k = keep(i);
                i += 1;
                k
            });
        }
        // Decoded channels are indexed logically, so they no longer mean what they did.
        self.windows.lock().map(|mut w| w.clear()).ok();
    }

    /// Replaces the whole logical → source channel mapping. The undo half of
    /// [`Self::retain_channels`].
    ///
    /// Entries are clamped to channels the file actually has, so a stale map (from a command
    /// replayed against a different file) can only ever narrow the view, never point past the end
    /// of the data and read garbage.
    pub fn set_channel_map(&self, map: Vec<usize>) {
        if let Ok(mut current) = self.channel_map.write() {
            *current = map.into_iter().filter(|&c| c < self.info.channels).collect();
        }
        self.windows.lock().map(|mut w| w.clear()).ok();
    }

    /// Runs `f` on `[start, end)` of logical channel `channel`.
    ///
    /// The slice handed to `f` is exactly the requested range, or shorter at end of file — and
    /// empty for an out-of-range channel or a degenerate range, which callers must tolerate
    /// since that is also what an empty document yields.
    fn with_slice<R>(
        &self,
        channel: usize,
        start: usize,
        end: usize,
        f: impl FnOnce(&[f32]) -> R,
    ) -> R {
        let total = self.len_samples();
        let end = end.min(total);
        let start = start.min(end);
        let Some(source_channel) = self.source_channel(channel) else {
            return f(&[]);
        };
        if start >= end {
            return f(&[]);
        }
        let want = end - start;

        if want > WINDOW_FRAMES {
            // Larger than a window; read straight into scratch without caching. Nothing in the
            // render path asks for this — `affords_exact_edges` and the raw-scan threshold both
            // keep requests far below a window — but a caller that did would get a correct answer
            // rather than a truncated one.
            let mut scratch = self.scratch.lock().unwrap();
            scratch.clear();
            if let Ok(mut frames) = self.frames.lock() {
                let _ = frames.read_channel_into(source_channel, start as u64, want, &mut scratch);
            }
            return f(&scratch);
        }

        let mut windows = self.windows.lock().unwrap();
        match windows.iter().position(|w| w.covers(start, end)) {
            // Move the hit to the front so the least recently used is the one evicted.
            Some(0) => {}
            Some(i) => {
                let hit = windows.remove(i);
                windows.insert(0, hit);
            }
            None => {
                // Anchored at `start` and running forward: requests walk that way, so the columns
                // and panes that follow land inside this window rather than re-reading its bytes.
                let base = start;
                let len = WINDOW_FRAMES.min(total - base);
                let mut set = if windows.len() >= WINDOW_COUNT {
                    // Reuse the evicted window's allocations rather than freeing and re-growing
                    // ~15MB every time the view moves past a boundary.
                    windows.pop().unwrap()
                } else {
                    WindowSet { base: 0, frames: 0, raw: Vec::new(), decoded: Vec::new() }
                };
                let read = self
                    .frames
                    .lock()
                    .ok()
                    .and_then(|mut f| f.read_raw(base as u64, len, &mut set.raw).ok())
                    .unwrap_or(0);
                set.base = base;
                set.frames = read;
                // Every decoded channel now describes different frames.
                set.decoded.clear();
                windows.insert(0, set);
            }
        }

        let map = self.channel_map();
        let set = &mut windows[0];
        if set.decoded.len() < map.len() {
            set.decoded.resize_with(map.len(), || None);
        }
        if set.decoded.get(channel).is_some_and(Option::is_none) {
            let mut out = Vec::new();
            if let Ok(frames) = self.frames.lock() {
                frames.decode_channel_from(&set.raw, source_channel, &mut out);
            }
            set.decoded[channel] = Some(out);
        }
        let Some(Some(data)) = set.decoded.get(channel) else { return f(&[]) };
        let from = (start - set.base).min(data.len());
        let to = (end - set.base).min(data.len());
        f(&data[from..to.max(from)])
    }

    /// Reads `[first_frame, first_frame + frames)` of every logical channel at once, into
    /// `out[logical]`. Clears `out` first. Returns frames actually read.
    ///
    /// One pass over the interleaved data serving every channel, which is what makes Export
    /// Channels a single read of the source rather than one per output file. Splitting a 20GB
    /// 58-channel take into stereo pairs is 29 outputs — re-reading per output would be 580GB.
    pub fn read_all_channels(
        &self,
        first_frame: u64,
        frames: usize,
        out: &mut Vec<Vec<f32>>,
    ) -> std::io::Result<usize> {
        let map = self.channel_map();
        out.resize_with(map.len(), Vec::new);
        for ch in out.iter_mut() {
            ch.clear();
        }
        let mut source_buf = self.demux.lock().unwrap();
        source_buf.resize_with(self.info.channels, Vec::new);
        for ch in source_buf.iter_mut() {
            ch.clear();
        }
        let frames_read = {
            let mut wav = self.frames.lock().unwrap();
            wav.read_into(first_frame, frames, &mut source_buf)?
        };
        for (logical, &source) in map.iter().enumerate() {
            if let Some(src) = source_buf.get(source) {
                out[logical].extend_from_slice(src);
            }
        }
        Ok(frames_read)
    }

}

/// How the render path reads samples, over either storage.
///
/// `Resident` is a plain subslice with no copy, so a fully-loaded document behaves exactly as
/// it did before this existed — that equivalence is the point, and it is what keeps the
/// streaming mode from being able to change how ordinary files look.
#[derive(Clone, Copy)]
pub enum SampleSource<'a> {
    Resident(&'a [f32]),
    Streamed { stream: &'a StreamedSamples, channel: usize },
}

/// Widest query span for which a streamed source reads raw samples to make a `min_max` answer
/// *exact* rather than bin-approximate.
///
/// A cache query raw-scans the partial bin at each edge of its range; that is what makes the answer
/// exact instead of the union of every bin the range merely touches. Resident samples make it free.
/// Disk-backed ones do not, and the cost is not what the sample counts suggest: the edges of
/// adjacent columns sit `span` apart, so once `span` outgrows the cached window they stop sharing
/// one and a redraw transfers the entire visible range — **505MB per frame** as measured on a
/// 56-channel file (user report: scrolling a 30GB file "feels VERY sluggish").
///
/// The fix is a limit, not an abolition. Dropping the scans altogether was tried and changed the
/// picture too much: at ~360 samples per column, 11% of screen cells differed from the same audio
/// loaded resident, because a bin is a third of a column there and the bleed is plainly visible.
/// The bleed shrinks as the span grows — a bin stays ~64 samples while a column grows to thousands
/// — so exactness is bought where it shows and skipped where it does not.
///
/// The value is bounded by what the window cache can hold: `span` x screen columns must fit within
/// [`WINDOW_COUNT`] x [`WINDOW_FRAMES`], or the reads stop sharing windows and the original problem
/// returns. 512 leaves room for a very wide terminal.
const STREAMED_EXACT_SPAN: usize = 512;

impl<'a> SampleSource<'a> {
    pub const EMPTY: SampleSource<'static> = SampleSource::Resident(&[]);

    pub fn len(&self) -> usize {
        match self {
            SampleSource::Resident(s) => s.len(),
            SampleSource::Streamed { stream, .. } => stream.len_samples(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Runs `f` on `[start, end)`, clamped to what exists.
    pub fn with_slice<R>(&self, start: usize, end: usize, f: impl FnOnce(&[f32]) -> R) -> R {
        match self {
            SampleSource::Resident(s) => {
                let end = end.min(s.len());
                let start = start.min(end);
                f(&s[start..end])
            }
            SampleSource::Streamed { stream, channel } => {
                stream.with_slice(*channel, start, end, f)
            }
        }
    }

    /// One sample, or 0.0 past the end. The per-pixel primitive the graphics renderer's
    /// interpolating polyline path uses, which only runs when zoomed in far enough that the
    /// whole visible span sits inside one cached window.
    pub fn sample(&self, index: usize) -> f32 {
        match self {
            SampleSource::Resident(s) => s.get(index).copied().unwrap_or(0.0),
            SampleSource::Streamed { .. } => {
                self.with_slice(index, index + 1, |s| s.first().copied().unwrap_or(0.0))
            }
        }
    }

    /// Whether reading raw samples to make a query over `span` *exact* is worth it here.
    ///
    /// Always yes when they are already in memory, so a resident file's rendering is untouched by
    /// any of this. For a streamed one it turns on the span — see [`STREAMED_EXACT_SPAN`].
    pub fn affords_exact_edges(&self, span: usize) -> bool {
        match self {
            SampleSource::Resident(_) => true,
            SampleSource::Streamed { .. } => span <= STREAMED_EXACT_SPAN,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes a float WAV whose channel `c`, frame `f` sample is `c * 1000 + f`, so any
    /// mis-indexed read is unmistakable rather than merely wrong.
    fn indexed_wav(dir: &std::path::Path, channels: usize, frames: usize) -> std::path::PathBuf {
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
                body.extend_from_slice(&((c * 1000 + f) as f32).to_le_bytes());
            }
        }
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        let p = dir.join(format!("idx{channels}x{frames}.wav"));
        std::fs::File::create(&p).unwrap().write_all(&out).unwrap();
        p
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tuiwave_stream_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_the_requested_window_of_the_requested_channel() {
        let dir = tmp("window");
        let s = StreamedSamples::open(indexed_wav(&dir, 4, 20_000)).unwrap();
        assert_eq!(s.channel_count(), 4);
        assert_eq!(s.len_samples(), 20_000);

        for ch in 0..4 {
            let src = SampleSource::Streamed { stream: &s, channel: ch };
            src.with_slice(100, 105, |got| {
                let want: Vec<f32> = (100..105).map(|f| (ch * 1000 + f) as f32).collect();
                assert_eq!(got, want.as_slice(), "channel {ch}");
            });
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A range wider than one cached window must still come back whole and correct — that is
    /// the path the bounded edge scans and the pyramid build both take.
    #[test]
    fn serves_a_range_larger_than_one_window() {
        let dir = tmp("big");
        // Deliberately longer than a window, so the uncached straight-to-scratch path is taken.
        let s = StreamedSamples::open(indexed_wav(&dir, 2, WINDOW_FRAMES + 5_000)).unwrap();
        let src = SampleSource::Streamed { stream: &s, channel: 1 };
        let len = WINDOW_FRAMES + 500;
        src.with_slice(10, 10 + len, |got| {
            assert_eq!(got.len(), len);
            assert_eq!(got[0], 1010.0);
            assert_eq!(got[len - 1], (1000 + 10 + len - 1) as f32);
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Sequential single-sample reads are what the graphics polyline path does; they must be
    /// correct and must reuse one window rather than re-reading per sample.
    #[test]
    fn sequential_single_sample_reads_are_correct() {
        let dir = tmp("seq");
        let s = StreamedSamples::open(indexed_wav(&dir, 3, 5_000)).unwrap();
        let src = SampleSource::Streamed { stream: &s, channel: 2 };
        for f in 0..500 {
            assert_eq!(src.sample(f), (2000 + f) as f32, "sample {f}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_clamp_at_the_ends_and_out_of_range_yields_nothing() {
        let dir = tmp("clamp");
        let s = StreamedSamples::open(indexed_wav(&dir, 2, 100)).unwrap();
        let src = SampleSource::Streamed { stream: &s, channel: 0 };
        src.with_slice(95, 200, |got| assert_eq!(got.len(), 5, "clamped to end of file"));
        src.with_slice(100, 200, |got| assert!(got.is_empty()));
        src.with_slice(50, 50, |got| assert!(got.is_empty(), "degenerate range"));
        assert_eq!(src.sample(1000), 0.0, "past the end reads as silence");

        let bad = SampleSource::Streamed { stream: &s, channel: 99 };
        bad.with_slice(0, 10, |got| assert!(got.is_empty(), "no such channel"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Dropping channels must re-map every later index, which is the whole reason the map
    /// exists — logical channel 0 reading source channel 2 is the case that matters.
    #[test]
    fn retain_channels_remaps_logical_indices_to_source_channels() {
        let dir = tmp("retain");
        let s = StreamedSamples::open(indexed_wav(&dir, 5, 1_000)).unwrap();
        // Keep source channels 2 and 4.
        s.retain_channels(|i| i == 2 || i == 4);
        assert_eq!(s.channel_count(), 2);
        assert_eq!(s.channel_map(), vec![2, 4]);

        let first = SampleSource::Streamed { stream: &s, channel: 0 };
        first.with_slice(7, 8, |got| assert_eq!(got, &[2007.0], "logical 0 is source 2"));
        let second = SampleSource::Streamed { stream: &s, channel: 1 };
        second.with_slice(7, 8, |got| assert_eq!(got, &[4007.0], "logical 1 is source 4"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Removing channels twice must compose, since Remove Empty Channels can be run again on
    /// an already-trimmed buffer.
    #[test]
    fn retain_channels_composes() {
        let dir = tmp("twice");
        let s = StreamedSamples::open(indexed_wav(&dir, 6, 100)).unwrap();
        s.retain_channels(|i| i % 2 == 1); // sources 1,3,5
        s.retain_channels(|i| i == 2); // of those, the last -> source 5
        assert_eq!(s.channel_map(), vec![5]);
        let only = SampleSource::Streamed { stream: &s, channel: 0 };
        only.with_slice(3, 4, |got| assert_eq!(got, &[5003.0]));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `read_all_channels` must walk the file in order and serve *every* channel from one read.
    /// Reading per channel would be the obvious shape and is catastrophic: a read fetches whole
    /// interleaved frames regardless, so N channels would mean N passes over the file.
    #[test]
    fn read_all_channels_walks_the_file_once_and_serves_every_channel() {
        let dir = tmp("blocks");
        let channels = 4usize;
        let frames = 200_000usize;
        let s = StreamedSamples::open(indexed_wav(&dir, channels, frames)).unwrap();

        let mut block: Vec<Vec<f32>> = Vec::new();
        let mut seen = 0usize;
        let mut at = 0u64;
        while at < frames as u64 {
            let n = s.read_all_channels(at, 7_000, &mut block).unwrap();
            assert!(n > 0, "must make progress");
            assert_eq!(block.len(), channels, "every channel arrives from one read");
            for (c, data) in block.iter().enumerate() {
                assert_eq!(data.len(), n, "channel {c} block length");
                for (i, &v) in data.iter().enumerate() {
                    assert_eq!(v, (c * 1000 + seen + i) as f32, "ch{c} frame {}", seen + i);
                }
            }
            seen += n;
            at += n as u64;
        }
        assert_eq!(seen, frames);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Resident and streamed must answer identically for the same audio — the equivalence the
    /// whole abstraction rests on.
    #[test]
    fn resident_and_streamed_agree_on_every_window() {
        let dir = tmp("agree");
        let path = indexed_wav(&dir, 3, 12_000);
        let doc = crate::model::io::load_wav(&path).unwrap();
        let s = StreamedSamples::open(&path).unwrap();

        for ch in 0..3 {
            let resident = SampleSource::Resident(&doc.channels[ch]);
            let streamed = SampleSource::Streamed { stream: &s, channel: ch };
            assert_eq!(resident.len(), streamed.len());
            for &(start, end) in &[(0usize, 1usize), (0, 12_000), (500, 9_000), (11_999, 12_000)] {
                let a = resident.with_slice(start, end, |s| s.to_vec());
                let b = streamed.with_slice(start, end, |s| s.to_vec());
                assert_eq!(a, b, "ch{ch} [{start},{end})");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The budget must bind only on the streamed side; resident samples are always scanned
    /// exactly, so an ordinary file's rendering cannot change.
    #[test]
    fn only_streamed_sources_ration_exact_edge_scans() {
        let dir = tmp("budget");
        let s = StreamedSamples::open(indexed_wav(&dir, 1, 100)).unwrap();
        let streamed = SampleSource::Streamed { stream: &s, channel: 0 };
        let resident = SampleSource::Resident(&[0.0; 4]);

        assert!(resident.affords_exact_edges(usize::MAX), "resident samples are always free to scan");
        assert!(streamed.affords_exact_edges(STREAMED_EXACT_SPAN));
        assert!(
            !streamed.affords_exact_edges(STREAMED_EXACT_SPAN + 1),
            "past the span limit a streamed query must answer from bins alone"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
