//! Small shared DSP helpers. Normalization semantics (peak scan, silence threshold,
//! dB-to-linear gain) used to be re-implemented at each call site — `NormalizeCommand`,
//! `GainCommand`, and the per-region export path each had their own copy, and two of them
//! had already drifted apart at the silence-threshold boundary. Keeping the definitions
//! here means a change to the measure (e.g. switching to true peak) applies everywhere.

/// Converts a dBFS value to a linear amplitude factor (0 dB → 1.0, -6 dB → ~0.5).
pub fn db_to_linear(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

/// Peak levels at or below this are treated as silence: normalizing them would amplify
/// noise (or divide by zero for actual digital silence) instead of anything audible.
pub const SILENCE_PEAK: f32 = 0.0001;

/// Highest absolute sample value across all channels.
pub fn peak(channels: &[Vec<f32>]) -> f32 {
    channels
        .iter()
        .flat_map(|ch| ch.iter())
        .fold(0.0f32, |p, &s| p.max(s.abs()))
}

/// The linear gain that brings `peak` up (or down) to `target_db` dBFS, or `None` when the
/// material is effectively silent (see [`SILENCE_PEAK`]) and must be left untouched.
pub fn normalize_gain(peak: f32, target_db: f32) -> Option<f32> {
    if peak < SILENCE_PEAK {
        None
    } else {
        Some(db_to_linear(target_db) / peak)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_to_linear_maps_known_points() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_linear(-6.0) - 0.5012).abs() < 1e-3);
        assert!((db_to_linear(-20.0) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn peak_scans_all_channels_and_uses_absolute_values() {
        let channels = vec![vec![0.1, -0.7, 0.2], vec![0.3, 0.4, -0.5]];
        assert!((peak(&channels) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn normalize_gain_reaches_the_target() {
        let gain = normalize_gain(0.5, 0.0).unwrap();
        assert!((gain - 2.0).abs() < 1e-6);
        let gain = normalize_gain(0.5, -6.0).unwrap();
        assert!((0.5 * gain - db_to_linear(-6.0)).abs() < 1e-6);
    }

    /// The boundary the two former copies disagreed on: exactly SILENCE_PEAK still
    /// normalizes (NormalizeCommand's `< threshold` semantics win), below it is silence.
    #[test]
    fn normalize_gain_treats_only_sub_threshold_peaks_as_silence() {
        assert!(normalize_gain(SILENCE_PEAK, 0.0).is_some());
        assert!(normalize_gain(SILENCE_PEAK * 0.5, 0.0).is_none());
        assert!(normalize_gain(0.0, 0.0).is_none());
    }
}
