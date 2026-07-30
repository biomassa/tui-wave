/// Precomputed multi-resolution min/max bins for one channel's samples, rebuilt once
/// whenever the document's sample data changes (load, cut, paste, undo, redo) rather than
/// scanned raw on every render. Without this, viewing a large file at a zoomed-out level
/// rescans the entire visible sample range every single frame — for a multi-minute file
/// that's tens of millions of float comparisons per redraw, which is what made the editor
/// feel "extremely slow" on large files. With the cache, render cost is bounded by screen
/// width, not file length or zoom level.
const BASE_BIN: usize = 64;
const REDUCTION: usize = 16;

use crate::model::stream::SampleSource;

struct MinMaxLevel {
    bin_size: usize,
    mins: Vec<f32>,
    maxs: Vec<f32>,
}

impl MinMaxLevel {
    fn reduced(prev: &MinMaxLevel, factor: usize) -> Self {
        let bin_size = prev.bin_size * factor;
        let mut mins = Vec::with_capacity(prev.mins.len() / factor + 1);
        let mut maxs = Vec::with_capacity(mins.capacity());
        let mut i = 0;
        while i < prev.mins.len() {
            let end = (i + factor).min(prev.mins.len());
            mins.push(prev.mins[i..end].iter().copied().fold(f32::MAX, f32::min));
            maxs.push(prev.maxs[i..end].iter().copied().fold(f32::MIN, f32::max));
            i = end;
        }
        Self {
            bin_size,
            mins,
            maxs,
        }
    }
}

pub struct WaveformCache {
    levels: Vec<MinMaxLevel>,
    peak: f32,
}

/// Accumulates base bins from samples arriving in arbitrarily-sized blocks, then folds the
/// pyramid on top of them.
///
/// Exists so a channel can be binned without ever being fully resident: `model::stream` reads a
/// large file in 64Ki-frame blocks and pushes each one through here, and the pyramid — ~1/30 the
/// size of the samples — is all that is kept. [`WaveformCache::build`] is this same builder fed
/// one block, so there is only ever one binning implementation to get right.
pub struct Builder {
    mins: Vec<f32>,
    maxs: Vec<f32>,
    /// The bin still being filled. Blocks do not have to align to `BASE_BIN`, so a bin
    /// routinely straddles a block boundary; carrying it here is what keeps every bin exactly
    /// `BASE_BIN` samples wide regardless of how the samples were delivered.
    acc_min: f32,
    acc_max: f32,
    acc_len: usize,
}

impl Default for Builder {
    fn default() -> Self {
        Builder::new()
    }
}

impl Builder {
    pub fn new() -> Self {
        Builder { mins: Vec::new(), maxs: Vec::new(), acc_min: f32::MAX, acc_max: f32::MIN, acc_len: 0 }
    }

    /// A builder pre-sized for a channel of `samples` samples.
    ///
    /// Worth doing whenever the length is known, which it is for every file: the base level is
    /// 1/32 the size of the samples, so a 30GB file's pyramid is ~1GB — and letting two `Vec`s
    /// that large grow by doubling costs about half as much again at the moment they reallocate.
    /// Measured: peak RSS opening a 30GB 56-channel file fell from 1477MB to near the pyramid's
    /// own size.
    pub fn with_capacity(samples: usize) -> Self {
        let bins = samples / BASE_BIN + 1;
        Builder {
            mins: Vec::with_capacity(bins),
            maxs: Vec::with_capacity(bins),
            acc_min: f32::MAX,
            acc_max: f32::MIN,
            acc_len: 0,
        }
    }

    pub fn push(&mut self, block: &[f32]) {
        let mut rest = block;
        while !rest.is_empty() {
            let want = BASE_BIN - self.acc_len;
            let take = want.min(rest.len());
            let (chunk, tail) = rest.split_at(take);
            let (mn, mx) = raw_min_max(chunk);
            self.acc_min = self.acc_min.min(mn);
            self.acc_max = self.acc_max.max(mx);
            self.acc_len += take;
            if self.acc_len == BASE_BIN {
                self.mins.push(self.acc_min);
                self.maxs.push(self.acc_max);
                self.acc_min = f32::MAX;
                self.acc_max = f32::MIN;
                self.acc_len = 0;
            }
            rest = tail;
        }
    }

    pub fn finish(mut self) -> WaveformCache {
        // A trailing partial bin is a real bin — the file's last few samples live in it.
        if self.acc_len > 0 {
            self.mins.push(self.acc_min);
            self.maxs.push(self.acc_max);
        }
        if self.mins.is_empty() {
            return WaveformCache { levels: Vec::new(), peak: 0.0 };
        }

        let mut levels = vec![MinMaxLevel { bin_size: BASE_BIN, mins: self.mins, maxs: self.maxs }];
        loop {
            let prev = levels.last().unwrap();
            if prev.mins.len() <= 1 {
                break;
            }
            levels.push(MinMaxLevel::reduced(prev, REDUCTION));
        }

        let base = &levels[0];
        let peak = base
            .mins
            .iter()
            .zip(base.maxs.iter())
            .fold(0.0f32, |p, (&mn, &mx)| p.max(mn.abs()).max(mx.abs()));

        WaveformCache { levels, peak }
    }
}

impl WaveformCache {
    pub fn build(samples: &[f32]) -> Self {
        let mut builder = Builder::new();
        builder.push(samples);
        builder.finish()
    }

    /// Highest absolute sample value in the channel — used to auto-fit the initial
    /// vertical zoom so a quiet file doesn't render using only a sliver of the available
    /// height, and by Remove Empty Channels (via `dsp::channels_below_peaks`) to decide which
    /// channels are empty. The latter is what makes that operation free on a streamed
    /// document: the peak already fell out of building the pyramid, so no extra pass is needed.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// Exact min/max over `samples[start..end)`. Falls back to a raw scan for short ranges
    /// (zoomed in close) where consulting the cache costs more than just reading the
    /// samples directly.
    ///
    /// For longer ranges, only the bins *fully* contained in `[start, end)` are taken from
    /// the precomputed levels; the partial bin at each edge (at most `bin_size` samples,
    /// however large the overall query is) is raw-scanned instead of pulled in whole. A
    /// fully-bin-aligned lookup is cheap to write but inexact — it reports the union of
    /// every bin merely *touched* by the query, which can include samples well outside it.
    /// That bled-in range is usually invisible (most of a waveform doesn't change sharply
    /// from one bin to the next), but it's a real, visible glitch right at a sharp, short
    /// transient — e.g. a 5ms Technical Fade is only ~3-4 base bins long, so at typical zoom
    /// a column straddling the fade's end could report the *post-fade* bin's max (already
    /// back at full volume) as if it were still within the fade, making the ramp look like
    /// it jumps to full volume a column early.
    pub fn min_max(&self, samples: SampleSource<'_>, start: usize, end: usize) -> (f32, f32) {
        let total = samples.len();
        if total == 0 || start >= end {
            return (0.0, 0.0);
        }
        let end = end.min(total);
        let start = start.min(end);
        if start >= end {
            return (0.0, 0.0);
        }
        let span = end - start;

        let Some(base) = self.levels.first() else {
            return samples.with_slice(start, end, raw_min_max);
        };
        if span < base.bin_size * 2 {
            return samples.with_slice(start, end, raw_min_max);
        }

        // Pick the level that minimizes total work, not the coarsest one that fits.
        //
        // A query costs one comparison per whole bin inside the range *plus* a raw scan of up
        // to `bin_size` samples at each edge, so those two pull in opposite directions and the
        // cheapest level sits near `sqrt(span/2)` — not at either extreme. Taking the coarsest
        // level that fits (which is what this did) minimizes the bin count and maximizes the
        // edge scans, and the edges dominate by orders of magnitude once a file is large enough
        // to have deep levels: a 20GB file zoomed right out queries ~450k samples per column,
        // where the coarsest fitting level (262144) raw-scans up to 262143 samples per edge but
        // level 1024 scans at most 1024 — the same answer for ~1% of the work, and around 100x
        // less for a resident file too.
        let level = self
            .levels
            .iter()
            .filter(|l| l.bin_size <= span)
            .min_by_key(|l| span / l.bin_size + 2 * l.bin_size)
            .unwrap_or(base);
        let bin_size = level.bin_size;

        // Bins fully inside [start, end): from the first bin starting at or after `start`,
        // up to (excluding) the bin containing `end`.
        let first_full_bin = start.div_ceil(bin_size);
        let last_full_bin_excl = (end / bin_size).min(level.mins.len());

        let mut mn = f32::MAX;
        let mut mx = f32::MIN;
        if first_full_bin < last_full_bin_excl {
            for bin in first_full_bin..last_full_bin_excl {
                mn = mn.min(level.mins[bin]);
                mx = mx.max(level.maxs[bin]);
            }
        }

        let covered_start = (first_full_bin * bin_size).min(end);
        let covered_end = ((last_full_bin_excl * bin_size).max(covered_start)).min(end);
        let edge_samples =
            covered_start.saturating_sub(start) + end.saturating_sub(covered_end);
        // With no whole bin inside the range there is nothing else to answer from, so the edges
        // must be read whatever they cost. That only arises for spans under `2 * bin_size`,
        // i.e. when zoomed in and the read is small anyway.
        let no_bins = first_full_bin >= last_full_bin_excl;

        if no_bins || samples.affords_exact_edges(edge_samples) {
            // Raw-scan whatever's left over at each edge — at most `bin_size` samples per side,
            // regardless of how large the query itself is.
            if start < covered_start {
                let (rmn, rmx) = samples.with_slice(start, covered_start, raw_min_max);
                mn = mn.min(rmn);
                mx = mx.max(rmx);
            }
            if covered_end < end {
                let (rmn, rmx) = samples.with_slice(covered_end, end, raw_min_max);
                mn = mn.min(rmn);
                mx = mx.max(rmx);
            }
        } else {
            // Streamed source, edges too costly to read: fold in the bins that merely *touch*
            // each edge instead. That widens the answer by at most one bin per side and needs no
            // I/O at all — see `SampleSource::affords_exact_edges` for why that trade only
            // applies to disk-backed samples, and never to a resident file.
            if covered_start > start {
                let head_bin = start / bin_size;
                if head_bin < level.mins.len() {
                    mn = mn.min(level.mins[head_bin]);
                    mx = mx.max(level.maxs[head_bin]);
                }
            }
            if end > covered_end {
                let tail_bin = (end - 1) / bin_size;
                if tail_bin < level.mins.len() {
                    mn = mn.min(level.mins[tail_bin]);
                    mx = mx.max(level.maxs[tail_bin]);
                }
            }
        }
        (mn, mx)
    }
}

pub fn raw_min_max(slice: &[f32]) -> (f32, f32) {
    slice
        .iter()
        .fold((f32::MAX, f32::MIN), |(mn, mx), &s| (mn.min(s), mx.max(s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_samples_give_zero() {
        let cache = WaveformCache::build(&[]);
        assert_eq!(cache.peak(), 0.0);
        assert_eq!(cache.min_max(SampleSource::Resident(&[]), 0, 10), (0.0, 0.0));
    }

    #[test]
    fn peak_matches_actual_extremes() {
        let mut samples = vec![0.0f32; 10_000];
        samples[1234] = 0.73;
        samples[5678] = -0.91;
        let cache = WaveformCache::build(&samples);
        assert!((cache.peak() - 0.91).abs() < 1e-6);
    }

    #[test]
    fn cached_min_max_matches_raw_scan_for_large_ranges() {
        let samples: Vec<f32> = (0..200_000)
            .map(|i| ((i as f32) * 0.001).sin())
            .collect();
        let cache = WaveformCache::build(&samples);

        for &(start, end) in &[(0, 200_000), (1000, 50_000), (137, 199_999)] {
            let cached = cache.min_max(SampleSource::Resident(&samples), start, end);
            let raw = raw_min_max(&samples[start..end]);
            // Only bins fully inside [start, end) come from the cache; the partial bin at
            // each edge is raw-scanned, so the result is exact — not just "wider, never
            // narrower" — regardless of how the query happens to land on bin boundaries.
            assert!((cached.0 - raw.0).abs() < 1e-6, "cached min should exactly match raw min");
            assert!((cached.1 - raw.1).abs() < 1e-6, "cached max should exactly match raw max");
        }
    }

    /// The actual bug this guards against: a short, sharp transient (here, mimicking a 5ms
    /// Technical Fade's exp envelope) sitting inside a single bin-cache bin must not leak
    /// the *next* bin's already-full-volume content into a query that doesn't reach it.
    /// Before the fix, querying [0, 200) over a fade that only reaches ~0.83 by sample 199
    /// reported a cached max of 1.0 — the post-fade bin's level bleeding in.
    #[test]
    fn cached_min_max_does_not_bleed_across_a_sharp_transient() {
        let fade_len = 220usize; // ~5ms at 44100Hz
        let total = 10_000usize;
        let mut samples = vec![1.0f32; total];
        for i in 0..fade_len {
            let t = i as f32 / (fade_len - 1) as f32;
            samples[i] = t * t; // exp fade-in envelope, same as TechnicalFadesCommand
        }
        let cache = WaveformCache::build(&samples);

        for &(start, end) in &[(0usize, 200usize), (200, 400), (50, 150)] {
            let cached = cache.min_max(SampleSource::Resident(&samples), start, end);
            let raw = raw_min_max(&samples[start..end]);
            assert!(
                (cached.1 - raw.1).abs() < 1e-6,
                "[{start},{end}): cached max {} should exactly match raw max {} (no bleed from outside the range)",
                cached.1,
                raw.1
            );
        }
    }

    /// Blocks pushed at sizes that don't divide `BASE_BIN` must still produce exactly the same
    /// pyramid as one contiguous push. This is what lets a large file be binned as it streams
    /// off disk in 64Ki-frame reads — a bin straddling a block boundary is the normal case, not
    /// an edge one, and getting it wrong would shift every later bin.
    #[test]
    fn block_fed_building_matches_a_single_contiguous_push() {
        let samples: Vec<f32> = (0..10_000).map(|i| ((i as f32) * 0.017).sin()).collect();
        let whole = WaveformCache::build(&samples);

        for block in [1usize, 7, 63, 64, 65, 100, 1000, 4096] {
            let mut builder = Builder::new();
            for chunk in samples.chunks(block) {
                builder.push(chunk);
            }
            let streamed = builder.finish();
            assert!(
                (streamed.peak() - whole.peak()).abs() < 1e-9,
                "block size {block}: peak differs"
            );
            for &(start, end) in &[(0usize, 10_000usize), (137, 9_871), (500, 700)] {
                assert_eq!(
                    streamed.min_max(SampleSource::Resident(&samples), start, end),
                    whole.min_max(SampleSource::Resident(&samples), start, end),
                    "block size {block}, range [{start},{end})"
                );
            }
        }
    }

    /// The level chosen must be the one that minimizes total work, not the coarsest that fits.
    /// Stated as a test because the difference is invisible in the *answer* — both are exact —
    /// and only shows up as a ~100x change in how many raw samples get scanned, which is what
    /// makes a 20GB file renderable at all.
    #[test]
    fn picks_the_level_that_minimizes_work_not_the_coarsest_that_fits() {
        // Deep enough to have levels 64, 1024, 16384 and 262144.
        let samples: Vec<f32> = (0..4_000_000).map(|i| ((i as f32) * 0.0001).sin()).collect();
        let cache = WaveformCache::build(&samples);
        assert!(cache.levels.len() >= 4, "need a deep pyramid for this to mean anything");

        // A zoomed-right-out column: ~450k samples, as a 20GB file gives at 200 columns.
        let span = 450_000usize;
        let coarsest = cache
            .levels
            .iter()
            .rev()
            .find(|l| l.bin_size <= span)
            .map(|l| l.bin_size)
            .unwrap();
        let chosen = cache
            .levels
            .iter()
            .filter(|l| l.bin_size <= span)
            .min_by_key(|l| span / l.bin_size + 2 * l.bin_size)
            .map(|l| l.bin_size)
            .unwrap();
        assert_eq!(coarsest, 262_144, "sanity: the old rule would have picked this");
        assert!(
            chosen < coarsest / 16,
            "the chosen level ({chosen}) must be far finer than the coarsest that fits ({coarsest})"
        );

        // And the answer is still exact, which is the property that must not have been traded.
        for start in [0usize, 1, 137, 1_000_000] {
            let end = (start + span).min(samples.len());
            let cached = cache.min_max(SampleSource::Resident(&samples), start, end);
            let raw = raw_min_max(&samples[start..end]);
            assert!((cached.0 - raw.0).abs() < 1e-6, "min at {start}");
            assert!((cached.1 - raw.1).abs() < 1e-6, "max at {start}");
        }
    }

    #[test]
    fn small_ranges_match_exactly_via_raw_fallback() {
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32) * 0.01).collect();
        let cache = WaveformCache::build(&samples);
        assert_eq!(cache.min_max(SampleSource::Resident(&samples), 10, 20), raw_min_max(&samples[10..20]));
    }
}
