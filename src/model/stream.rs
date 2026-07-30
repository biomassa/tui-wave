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

use std::sync::Mutex;

use super::wavread::{WavFrames, WavInfo};

/// Samples a single cached window holds, when the request is small enough to cache at all.
///
/// Sized for the zoomed-in case, where consecutive columns (and the graphics renderer's
/// per-pixel polyline path) read overlapping or adjacent ranges: one block then serves a whole
/// screen's worth of lookups. 8192 f32 is 32KB per channel, so even all 58 channels resident
/// at once is under 2MB.
const WINDOW_SAMPLES: usize = 8192;

/// One channel's most recently read window.
struct Window {
    /// Absolute index of `data[0]`.
    base: usize,
    data: Vec<f32>,
}

impl Window {
    fn covers(&self, start: usize, end: usize) -> bool {
        start >= self.base && end <= self.base + self.data.len()
    }
}

/// A read-only document's samples, left on disk and read in windows on demand.
///
/// `channel_map` is what Remove Empty Channels edits: logical channel *i* reads source channel
/// `channel_map[i]`. Storing the indirection rather than rewriting anything means dropping 48
/// of 58 channels costs a `Vec<usize>` edit, not a 20GB rewrite — at the price of not being
/// undoable through `History`, which stores sample data it cannot have here.
pub struct StreamedSamples {
    /// `Mutex` purely because the render path holds `&Document` while reading, and reading
    /// needs to seek. Never contended — the UI is single-threaded.
    frames: Mutex<WavFrames>,
    info: WavInfo,
    channel_map: Vec<usize>,
    windows: Mutex<Vec<Option<Window>>>,
    /// Scratch for reads too large to cache, kept alive so a per-frame read doesn't reallocate.
    scratch: Mutex<Vec<f32>>,
}

impl StreamedSamples {
    pub fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<StreamedSamples> {
        let frames = WavFrames::open(path)?;
        let info = frames.info();
        Ok(StreamedSamples {
            frames: Mutex::new(frames),
            info,
            channel_map: (0..info.channels).collect(),
            windows: Mutex::new(Vec::new()),
            scratch: Mutex::new(Vec::new()),
        })
    }

    pub fn info(&self) -> WavInfo {
        self.info
    }

    /// Channels currently presented, i.e. after any Remove Empty Channels.
    pub fn channel_count(&self) -> usize {
        self.channel_map.len()
    }

    pub fn len_samples(&self) -> usize {
        // A frame count that doesn't fit in `usize` can't be addressed by the viewport either;
        // saturating keeps the file viewable up to that bound instead of failing to open.
        usize::try_from(self.info.frame_count).unwrap_or(usize::MAX)
    }

    pub fn channel_map(&self) -> &[usize] {
        &self.channel_map
    }

    /// Drops logical channels by index, keeping the rest in order. The streaming counterpart of
    /// `RemoveChannelsCommand`, which cannot be used here — it stashes every removed channel's
    /// samples so `undo` can put them back, which at these sizes is most of the file.
    pub fn retain_channels(&mut self, keep: impl Fn(usize) -> bool) {
        let mut i = 0;
        self.channel_map.retain(|_| {
            let k = keep(i);
            i += 1;
            k
        });
        // Window indices are logical, so they no longer mean what they did.
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
        let Some(&source_channel) = self.channel_map.get(channel) else {
            return f(&[]);
        };
        if start >= end {
            return f(&[]);
        }
        let want = end - start;

        if want > WINDOW_SAMPLES {
            // Too large to be worth caching; read straight into scratch. Only the deliberately
            // bounded paths reach here (see `SampleSource::exact_edge_budget`), so this is not
            // the hot case it would be if the whole visible span came through it.
            let mut scratch = self.scratch.lock().unwrap();
            scratch.clear();
            if let Ok(mut frames) = self.frames.lock() {
                let _ = frames.read_channel_into(source_channel, start as u64, want, &mut scratch);
            }
            return f(&scratch);
        }

        let mut windows = self.windows.lock().unwrap();
        if windows.len() < self.channel_map.len() {
            windows.resize_with(self.channel_map.len(), || None);
        }
        let needs_load = !windows[channel].as_ref().is_some_and(|w| w.covers(start, end));
        if needs_load {
            // Anchor the window at `start` rather than on a fixed grid: reads walk forward
            // (column by column, pixel by pixel), so starting here means the following
            // requests fall inside it. A grid would split half of them across two blocks.
            let base = start;
            let len = WINDOW_SAMPLES.min(total - base);
            let mut data = Vec::with_capacity(len);
            if let Ok(mut frames) = self.frames.lock() {
                let _ = frames.read_channel_into(source_channel, base as u64, len, &mut data);
            }
            windows[channel] = Some(Window { base, data });
        }
        let window = windows[channel].as_ref().expect("just populated");
        let from = start - window.base;
        let to = (end - window.base).min(window.data.len());
        f(&window.data[from..to.max(from)])
    }

    /// Reads a whole channel in blocks, handing each block to `f`. Used to build the pyramid at
    /// open time and to write channels back out on export, neither of which wants the file
    /// resident.
    pub fn for_each_block(
        &self,
        channel: usize,
        mut f: impl FnMut(&[f32]),
    ) -> std::io::Result<()> {
        let Some(&source_channel) = self.channel_map.get(channel) else {
            return Ok(());
        };
        let total = self.info.frame_count;
        let mut frames = self.frames.lock().unwrap();
        let mut buf: Vec<f32> = Vec::new();
        let mut at = 0u64;
        while at < total {
            buf.clear();
            let n = frames.read_channel_into(
                source_channel,
                at,
                super::wavread::READ_BLOCK_FRAMES,
                &mut buf,
            )?;
            if n == 0 {
                break;
            }
            f(&buf);
            at += n as u64;
        }
        Ok(())
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

/// Most raw samples a single `min_max` call will read from disk to make its answer *exact*
/// rather than bin-approximate.
///
/// Every cache query raw-scans the partial bin at each edge of its range, which is what makes
/// the result exact instead of the union of every bin the range merely touches. Resident
/// samples make that free. Over a 20GB file on disk it is not: a screen of 200 columns across
/// 6 visible channels is 2400 such scans per redraw, and at extreme zoom-out each edge is most
/// of a bin. Past this budget the edges are skipped and the bins alone answer, which costs at
/// most one bin of bleed — sub-pixel at the zoom levels where the budget actually binds, and
/// invisible next to the alternative of a redraw that takes hundreds of milliseconds.
const STREAMED_EXACT_EDGE_SAMPLES: usize = 4096;

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

    /// Whether reading `edge_samples` raw samples to make a query exact is worth it here.
    /// Always yes when they are already in memory — see [`STREAMED_EXACT_EDGE_SAMPLES`].
    pub fn affords_exact_edges(&self, edge_samples: usize) -> bool {
        match self {
            SampleSource::Resident(_) => true,
            SampleSource::Streamed { .. } => edge_samples <= STREAMED_EXACT_EDGE_SAMPLES,
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
        let s = StreamedSamples::open(indexed_wav(&dir, 2, 30_000)).unwrap();
        let src = SampleSource::Streamed { stream: &s, channel: 1 };
        let len = WINDOW_SAMPLES + 500;
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
        let mut s = StreamedSamples::open(indexed_wav(&dir, 5, 1_000)).unwrap();
        // Keep source channels 2 and 4.
        s.retain_channels(|i| i == 2 || i == 4);
        assert_eq!(s.channel_count(), 2);
        assert_eq!(s.channel_map(), &[2, 4]);

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
        let mut s = StreamedSamples::open(indexed_wav(&dir, 6, 100)).unwrap();
        s.retain_channels(|i| i % 2 == 1); // sources 1,3,5
        s.retain_channels(|i| i == 2); // of those, the last -> source 5
        assert_eq!(s.channel_map(), &[5]);
        let only = SampleSource::Streamed { stream: &s, channel: 0 };
        only.with_slice(3, 4, |got| assert_eq!(got, &[5003.0]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn for_each_block_walks_a_whole_channel_in_order() {
        let dir = tmp("blocks");
        let s = StreamedSamples::open(indexed_wav(&dir, 2, 200_000)).unwrap();
        let mut seen = 0usize;
        let mut ok = true;
        s.for_each_block(1, |block| {
            for (i, &v) in block.iter().enumerate() {
                if v != (1000 + seen + i) as f32 {
                    ok = false;
                }
            }
            seen += block.len();
        })
        .unwrap();
        assert!(ok, "blocks must arrive in order with the right values");
        assert_eq!(seen, 200_000);
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

        assert!(resident.affords_exact_edges(usize::MAX));
        assert!(streamed.affords_exact_edges(STREAMED_EXACT_EDGE_SAMPLES));
        assert!(!streamed.affords_exact_edges(STREAMED_EXACT_EDGE_SAMPLES + 1));
        std::fs::remove_dir_all(&dir).ok();
    }
}
