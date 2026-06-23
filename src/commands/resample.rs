use crate::model::command::Command;
use crate::model::document::Document;

/// Taps on each side of the interpolation point at unity ratio. The kernel widens by
/// `1/cutoff` when downsampling so the anti-alias lowpass keeps the same number of zero
/// crossings regardless of conversion ratio.
const HALF_TAPS: usize = 32;

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let p = std::f64::consts::PI * x;
        p.sin() / p
    }
}

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

    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let center = i as f64 / ratio; // position in input-sample units
        let n0 = center.floor() as isize;
        let mut acc = 0.0f64;
        let mut norm = 0.0f64;
        for n in (n0 - half + 1)..=(n0 + half) {
            if n < 0 || n >= len {
                continue;
            }
            let tau = center - n as f64;
            if tau.abs() > half as f64 {
                continue;
            }
            // Hann window over the [-half, half] support.
            let w = 0.5 * (1.0 + (std::f64::consts::PI * tau / half as f64).cos());
            let h = sinc(cutoff * tau) * cutoff * w;
            acc += input[n as usize] as f64 * h;
            norm += h;
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
    /// Original channels, sample rate, and markers, captured for undo.
    original: Option<(Vec<Vec<f32>>, u32, Vec<crate::model::document::Marker>)>,
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
        self.original = Some((doc.channels.clone(), doc.sample_rate, doc.markers.clone()));
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
        doc.sample_rate = self.target_rate;
        doc.selection = None;
        doc.cursor = 0;
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        if let Some((channels, rate, markers)) = self.original.take() {
            doc.channels = channels;
            doc.sample_rate = rate;
            doc.markers = markers;
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

    fn doc(channels: Vec<Vec<f32>>, rate: u32) -> Document {
        Document {
            channels,
            sample_rate: rate,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bext: None,
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
