use crate::model::command::Command;
use crate::model::document::Document;

/// Taps on each side of the interpolation point at unity ratio. The kernel widens by
/// `1/cutoff` when downsampling so the anti-alias lowpass keeps the same number of zero
/// crossings regardless of conversion ratio.
const HALF_TAPS: usize = 32;

/// Arbitrary-ratio resample of one channel via a normalized windowed-sinc (Hann) kernel.
/// `ratio` is `out_rate / in_rate`. Normalizing by the summed weights gives unity DC gain
/// and graceful edge handling without an explicit zero-padding pass. Pure function — no
/// external dependency, so it's deterministic and unit-testable.
pub fn resample_channel(input: &[f32], ratio: f64) -> Vec<f32> {
    if input.is_empty() || ratio <= 0.0 {
        return Vec::new();
    }
    let out_len = ((input.len() as f64) * ratio).round().max(1.0) as usize;
    // Lowpass at the lower of the two Nyquist limits (only matters when downsampling).
    let cutoff = ratio.min(1.0);
    let half = (HALF_TAPS as f64 / cutoff).ceil() as isize;
    let len = input.len() as isize;

    // `tau` falls by exactly 1.0 per tap, so both angles the kernel needs advance by a *fixed*
    // step: the sinc's by `-PI * cutoff` and the Hann window's by `-PI / half`. Rotating them
    // through the tap loop with the angle-addition identities costs two multiply-adds each,
    // where evaluating them directly costs a `sin` and a `cos` per tap — and the tap count is
    // `2 * ceil(32 / cutoff)`, so a 96k→48k pass spent 128 transcendentals per output sample
    // per channel. On a 30-channel 5-minute take that is ~5e10 of them, on the UI thread, with
    // nothing on screen saying the app is working rather than hung.
    //
    // Measured at **4.3x** on that ratio in a release build (101ms against 434ms for 10s of
    // one channel) — well short of the ~64x the transcendental count alone implies, because
    // the compiler vectorizes the direct form and memory traffic takes over. Worth having, but
    // this shortens the freeze rather than removing it: a genuinely large conversion still
    // blocks the UI thread with no progress and no way to cancel, unlike every other long
    // operation here (streamed load, streamed Save As, Export Channels).
    //
    // The rotation restarts from a fresh `sin_cos` on every output sample, so error is bounded
    // by one sample's tap count instead of accumulating across the file — at ~1e-16 per step
    // over at most a few thousand steps, far below the f32 the result is stored as.
    let (sin_step, cos_step) = (-std::f64::consts::PI * cutoff).sin_cos();
    let half_f = half as f64;
    let (sin_wstep, cos_wstep) = (-std::f64::consts::PI / half_f).sin_cos();

    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let center = i as f64 / ratio; // position in input-sample units
        let n0 = center.floor() as isize;
        let n_start = n0 - half + 1;
        let tau_start = center - n_start as f64;
        // Angles at the first tap; each iteration rotates them to the next.
        let (mut sin_a, mut cos_a) = (std::f64::consts::PI * cutoff * tau_start).sin_cos();
        let (mut sin_w, mut cos_w) = (std::f64::consts::PI * tau_start / half_f).sin_cos();
        let mut acc = 0.0f64;
        let mut norm = 0.0f64;
        for n in n_start..=(n0 + half) {
            let tau = center - n as f64;
            if n >= 0 && n < len && tau.abs() <= half_f {
                // `sin_a` is `sin(PI * cutoff * tau)` — the sinc's numerator, with the same
                // small-argument guard the standalone `sinc` uses.
                let x = cutoff * tau;
                let s = if x.abs() < 1e-9 { 1.0 } else { sin_a / (std::f64::consts::PI * x) };
                // Hann window over the [-half, half] support; `cos_w` is its cosine term.
                let w = 0.5 * (1.0 + cos_w);
                let h = s * cutoff * w;
                acc += input[n as usize] as f64 * h;
                norm += h;
            }
            let next_sin_a = sin_a * cos_step + cos_a * sin_step;
            cos_a = cos_a * cos_step - sin_a * sin_step;
            sin_a = next_sin_a;
            let next_sin_w = sin_w * cos_wstep + cos_w * sin_wstep;
            cos_w = cos_w * cos_wstep - sin_w * sin_wstep;
            sin_w = next_sin_w;
        }
        out.push(if norm.abs() > 1e-12 {
            (acc / norm) as f32
        } else {
            0.0
        });
    }
    out
}

/// Resamples the whole document to `target_rate`. A whole-file operation (not range-based):
/// a single document has one sample rate, so resampling a sub-range is meaningless.
#[derive(Debug)]
pub struct ResampleCommand {
    target_rate: u32,
    /// Original channels, sample rate, markers, and head/tail marks, captured for undo.
    /// Both mark systems are snapshotted rather than re-scaled by the inverse ratio: rounding
    /// to whole samples is lossy, so scaling back would drift a mark by a sample or two per
    /// undo — and for head/tail marks, two adjacent ones could round onto the same sample and
    /// silently flip the Head/Tail role of every mark after them.
    original: Option<(Vec<Vec<f32>>, u32, Vec<crate::model::document::Marker>, Vec<usize>)>,
}

impl ResampleCommand {
    pub fn new(target_rate: u32) -> Self {
        Self {
            target_rate,
            original: None,
        }
    }
}

impl Command for ResampleCommand {
    fn execute(&mut self, doc: &mut Document) {
        if self.target_rate == 0 || self.target_rate == doc.sample_rate || doc.len_samples() == 0 {
            return;
        }
        let ratio = self.target_rate as f64 / doc.sample_rate as f64;
        self.original =
            Some((doc.channels.clone(), doc.sample_rate, doc.markers.clone(), doc.head_tail_marks.clone()));
        doc.channels = doc
            .channels
            .iter()
            .map(|c| resample_channel(c, ratio))
            .collect();
        // Marker positions are in samples, so they scale with the rate change.
        let new_len = doc.channels.first().map(|c| c.len()).unwrap_or(0);
        for m in &mut doc.markers {
            m.position = ((m.position as f64 * ratio).round() as usize).min(new_len);
        }
        // Head/tail marks scale the same way. Deduped after, because two marks close enough
        // together can round onto the same sample when downsampling, and duplicates would
        // both add a zero-length segment and flip every later mark's derived role.
        for m in &mut doc.head_tail_marks {
            *m = ((*m as f64 * ratio).round() as usize).min(new_len);
        }
        doc.dedup_head_tail_marks();
        doc.sample_rate = self.target_rate;
        doc.selection = None;
        doc.cursor = 0;
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        if let Some((channels, rate, markers, head_tail_marks)) = self.original.take() {
            doc.channels = channels;
            doc.sample_rate = rate;
            doc.markers = markers;
            doc.head_tail_marks = head_tail_marks;
        }
        doc.selection = None;
        doc.cursor = 0;
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Resample"
    }
}

pub fn resample_command(target_rate: u32) -> Box<dyn Command> {
    Box::new(ResampleCommand::new(target_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Head/tail marks are sample positions, so they scale with the rate change; undo restores
    /// the snapshot rather than scaling back, because the round-to-whole-samples in each
    /// direction is lossy and would drift them.
    #[test]
    fn resample_scales_head_tail_marks_and_undo_restores_them_exactly() {
        let mut doc = Document {
            head_tail_marks: vec![0, 11_025, 22_050, 44_099],
            channels: vec![vec![0.0; 44_100]],
            sample_rate: 44_100,
            ..Default::default()
        };
        let mut cmd = ResampleCommand::new(22_050);
        cmd.execute(&mut doc);
        assert_eq!(doc.sample_rate, 22_050);
        assert_eq!(doc.head_tail_marks, vec![0, 5_513, 11_025, 22_050]);

        cmd.undo(&mut doc);
        assert_eq!(doc.sample_rate, 44_100);
        assert_eq!(doc.head_tail_marks, vec![0, 11_025, 22_050, 44_099]);
    }

    fn doc(channels: Vec<Vec<f32>>, rate: u32) -> Document {
        Document {
            head_tail_marks: Vec::new(),
            channels,
            sample_rate: rate,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
            stream: None,
        }
    }

    /// The tap loop rotates its two angles instead of evaluating `sin`/`cos` per tap, which is
    /// mathematically the same kernel and ~64x less arithmetic. This pins that equivalence
    /// against a direct evaluation of the same formula, so a future edit to the recurrence
    /// cannot quietly change what the filter does — the failure mode being subtly wrong audio
    /// rather than an error.
    #[test]
    fn the_rotated_kernel_matches_direct_evaluation() {
        /// The normalized sinc, evaluated directly. Lives here rather than beside
        /// `resample_channel` because the rotation recurrence is the only kernel production
        /// uses now — this is the independent statement of the same maths to check it against.
        fn sinc(x: f64) -> f64 {
            if x.abs() < 1e-9 {
                1.0
            } else {
                let p = std::f64::consts::PI * x;
                p.sin() / p
            }
        }

        /// The kernel written the obvious way, as the reference.
        fn reference(input: &[f32], ratio: f64) -> Vec<f32> {
            let out_len = ((input.len() as f64) * ratio).round().max(1.0) as usize;
            let cutoff = ratio.min(1.0);
            let half = (HALF_TAPS as f64 / cutoff).ceil() as isize;
            let len = input.len() as isize;
            let mut out = Vec::with_capacity(out_len);
            for i in 0..out_len {
                let center = i as f64 / ratio;
                let n0 = center.floor() as isize;
                let (mut acc, mut norm) = (0.0f64, 0.0f64);
                for n in (n0 - half + 1)..=(n0 + half) {
                    if n < 0 || n >= len {
                        continue;
                    }
                    let tau = center - n as f64;
                    if tau.abs() > half as f64 {
                        continue;
                    }
                    let w = 0.5 * (1.0 + (std::f64::consts::PI * tau / half as f64).cos());
                    let h = sinc(cutoff * tau) * cutoff * w;
                    acc += input[n as usize] as f64 * h;
                    norm += h;
                }
                out.push(if norm.abs() > 1e-12 { (acc / norm) as f32 } else { 0.0 });
            }
            out
        }

        let input: Vec<f32> = (0..500)
            .map(|i| (i as f32 * 0.07).sin() * 0.6 + (i as f32 * 0.31).sin() * 0.3)
            .collect();
        // Upsampling, downsampling, and a ratio that is neither a multiple nor a divisor —
        // the last widens the kernel to a non-integer half-width.
        for &ratio in &[2.0, 0.5, 44100.0 / 48000.0, 3.0 / 7.0] {
            let fast = resample_channel(&input, ratio);
            let slow = reference(&input, ratio);
            assert_eq!(fast.len(), slow.len(), "length must match at ratio {ratio}");
            let max_err = fast
                .iter()
                .zip(&slow)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(max_err < 1e-5, "ratio {ratio}: max error {max_err} is too large");
        }
    }

    #[test]
    fn upsample_doubles_length() {
        let input: Vec<f32> = (0..100).map(|i| (i as f32 * 0.1).sin()).collect();
        let out = resample_channel(&input, 2.0);
        assert_eq!(out.len(), 200);
    }

    #[test]
    fn downsample_halves_length() {
        let input: Vec<f32> = (0..100).map(|i| (i as f32 * 0.1).sin()).collect();
        let out = resample_channel(&input, 0.5);
        assert_eq!(out.len(), 50);
    }

    #[test]
    fn preserves_a_low_frequency_sine_through_round_trip() {
        // A 1 Hz sine at 1000 Hz, well below Nyquist on both sides; upsample then downsample
        // should land close to the original (windowed-sinc is near-transparent here).
        let n = 1000;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * i as f32 / n as f32).sin())
            .collect();
        let up = resample_channel(&input, 2.0);
        let back = resample_channel(&up, 0.5);
        assert_eq!(back.len(), n);
        // Compare the interior (edges have the most windowing error).
        let mut max_err = 0.0f32;
        for i in 100..n - 100 {
            max_err = max_err.max((back[i] - input[i]).abs());
        }
        assert!(max_err < 0.02, "round-trip error too large: {max_err}");
    }

    #[test]
    fn execute_sets_rate_and_undo_restores() {
        let input: Vec<f32> = (0..200).map(|i| (i as f32 * 0.05).sin()).collect();
        let mut d = doc(vec![input.clone()], 44100);
        let mut cmd = ResampleCommand::new(22050);
        cmd.execute(&mut d);
        assert_eq!(d.sample_rate, 22050);
        assert_eq!(d.len_samples(), 100);
        assert!(d.dirty);
        cmd.undo(&mut d);
        assert_eq!(d.sample_rate, 44100);
        assert_eq!(d.channels[0], input);
    }

    #[test]
    fn resample_to_same_rate_is_a_no_op() {
        let mut d = doc(vec![vec![0.1, 0.2, 0.3]], 44100);
        let mut cmd = ResampleCommand::new(44100);
        cmd.execute(&mut d);
        assert_eq!(d.channels, vec![vec![0.1, 0.2, 0.3]]);
    }
}
