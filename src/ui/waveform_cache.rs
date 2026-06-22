/// Precomputed multi-resolution min/max bins for one channel's samples, rebuilt once
/// whenever the document's sample data changes (load, cut, paste, undo, redo) rather than
/// scanned raw on every render. Without this, viewing a large file at a zoomed-out level
/// rescans the entire visible sample range every single frame — for a multi-minute file
/// that's tens of millions of float comparisons per redraw, which is what made the editor
/// feel "extremely slow" on large files. With the cache, render cost is bounded by screen
/// width, not file length or zoom level.
const BASE_BIN: usize = 64;
const REDUCTION: usize = 16;

struct MinMaxLevel {
    bin_size: usize,
    mins: Vec<f32>,
    maxs: Vec<f32>,
}

impl MinMaxLevel {
    fn from_samples(samples: &[f32], bin_size: usize) -> Self {
        let mut mins = Vec::with_capacity(samples.len() / bin_size + 1);
        let mut maxs = Vec::with_capacity(mins.capacity());
        for chunk in samples.chunks(bin_size) {
            let (mn, mx) = raw_min_max(chunk);
            mins.push(mn);
            maxs.push(mx);
        }
        Self {
            bin_size,
            mins,
            maxs,
        }
    }

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

impl WaveformCache {
    pub fn build(samples: &[f32]) -> Self {
        if samples.is_empty() {
            return Self {
                levels: Vec::new(),
                peak: 0.0,
            };
        }

        let mut levels = vec![MinMaxLevel::from_samples(samples, BASE_BIN)];
        loop {
            let prev = levels.last().unwrap();
            if prev.mins.len() <= 1 {
                break;
            }
            levels.push(MinMaxLevel::reduced(prev, REDUCTION));
        }

        let top = &levels[0];
        let peak = top
            .mins
            .iter()
            .zip(top.maxs.iter())
            .fold(0.0f32, |p, (&mn, &mx)| p.max(mn.abs()).max(mx.abs()));

        Self { levels, peak }
    }

    /// Highest absolute sample value in the channel — used to auto-fit the initial
    /// vertical zoom so a quiet file doesn't render using only a sliver of the available
    /// height.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// min/max over `samples[start..end)`. Falls back to a raw scan for short ranges
    /// (zoomed in close) where consulting the cache costs more than just reading the
    /// samples directly.
    pub fn min_max(&self, samples: &[f32], start: usize, end: usize) -> (f32, f32) {
        if samples.is_empty() || start >= end {
            return (0.0, 0.0);
        }
        let end = end.min(samples.len());
        let start = start.min(end);
        if start >= end {
            return (0.0, 0.0);
        }
        let span = end - start;

        let Some(base) = self.levels.first() else {
            return raw_min_max(&samples[start..end]);
        };
        if span < base.bin_size * 2 {
            return raw_min_max(&samples[start..end]);
        }

        let level = self
            .levels
            .iter()
            .rev()
            .find(|l| l.bin_size <= span)
            .unwrap_or(base);
        let first_bin = start / level.bin_size;
        let last_bin = ((end - 1) / level.bin_size).min(level.mins.len() - 1);

        let mut mn = f32::MAX;
        let mut mx = f32::MIN;
        for bin in first_bin..=last_bin {
            mn = mn.min(level.mins[bin]);
            mx = mx.max(level.maxs[bin]);
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
        assert_eq!(cache.min_max(&[], 0, 10), (0.0, 0.0));
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
            let cached = cache.min_max(&samples, start, end);
            let raw = raw_min_max(&samples[start..end]);
            // Bin-aligned lookups can include a little extra range at the edges, so the
            // cached result may be slightly wider, never narrower, than the precise scan.
            assert!(cached.0 <= raw.0 + 1e-6, "cached min should be <= raw min");
            assert!(cached.1 >= raw.1 - 1e-6, "cached max should be >= raw max");
        }
    }

    #[test]
    fn small_ranges_match_exactly_via_raw_fallback() {
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32) * 0.01).collect();
        let cache = WaveformCache::build(&samples);
        assert_eq!(cache.min_max(&samples, 10, 20), raw_min_max(&samples[10..20]));
    }
}
