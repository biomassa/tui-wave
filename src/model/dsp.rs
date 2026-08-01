//! Small shared DSP helpers. Normalization semantics (peak scan, silence threshold,
//! dB-to-linear gain) used to be re-implemented at each call site — `NormalizeCommand`,
//! `GainCommand`, and the per-region export path each had their own copy, and two of them
//! had already drifted apart at the silence-threshold boundary. Keeping the definitions
//! here means a change to the measure (e.g. switching to true peak) applies everywhere.

/// Converts a dBFS value to a linear amplitude factor (0 dB → 1.0, -6 dB → ~0.5).
pub fn db_to_linear(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

/// Converts a linear amplitude factor to dBFS (1.0 → 0 dB, ~0.5 → -6 dB) — the inverse of
/// [`db_to_linear`]. `amplitude` is clamped away from zero first so literal silence maps to
/// a large-but-finite negative number instead of `-inf`.
pub fn linear_to_db(amplitude: f32) -> f32 {
    20.0 * amplitude.abs().max(1e-6).log10()
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

/// Highest absolute sample value in one channel — the per-channel form of [`peak`], which
/// folds across all of them at once.
pub fn channel_peak(channel: &[f32]) -> f32 {
    channel.iter().fold(0.0f32, |p, &s| p.max(s.abs()))
}

/// Indices of the channels whose peak is *strictly below* `threshold_db`, ascending — the
/// channels Remove Empty Channels drops.
///
/// The comparison happens in the **linear** domain, against `db_to_linear(threshold_db)`,
/// rather than converting each channel's peak to dB first. A digitally silent channel peaks
/// at exactly 0.0, and that is precisely the case this exists to find: `linear_to_db` only
/// avoids returning `-inf` for it by clamping at 1e-6 (-120 dB), which would then silently
/// *keep* such a channel at any threshold below -120. Comparing linear amplitudes has no such
/// floor.
///
/// Strictly-below means a channel peaking exactly at the threshold is kept — at a boundary,
/// keeping audio is the recoverable choice.
pub fn channels_below(channels: &[Vec<f32>], threshold_db: f32) -> Vec<usize> {
    let peaks: Vec<f32> = channels.iter().map(|ch| channel_peak(ch)).collect();
    channels_below_peaks(&peaks, threshold_db)
}

/// [`channels_below`] from peaks that have already been measured.
///
/// The waveform pyramid (`ui::waveform_cache::WaveformCache`) records each channel's peak as a
/// by-product of being built, so a streamed document — which has no resident `Vec<Vec<f32>>` to
/// hand to `channels_below` — can answer this for a 20GB file with **no additional I/O at all**.
/// Both entry points share this body so the linear-domain comparison and the strictly-below
/// boundary rule documented above can only ever be defined once.
pub fn channels_below_peaks(peaks: &[f32], threshold_db: f32) -> Vec<usize> {
    let threshold = db_to_linear(threshold_db);
    peaks
        .iter()
        .enumerate()
        .filter(|&(_, &peak)| peak < threshold)
        .map(|(i, _)| i)
        .collect()
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

/// Ceiling of the playback limiter, in dBFS.
///
/// Only ever applied on the multichannel fold-down (see [`playback_channels`]) — a mono or stereo
/// buffer plays bit-exact, exactly as it did before this existed, because there is no summing to
/// contain and softening peaks the user did not ask to soften would misrepresent the file.
pub const PLAYBACK_CEILING_DB: f32 = -1.0;

/// Channel counts at or above this are folded to stereo for playback.
///
/// Below it the file already *is* what a stereo device wants (or is mono), so it passes through
/// untouched. This threshold is what keeps every ordinary file's playback path unchanged.
pub const DOWNMIX_MIN_CHANNELS: usize = 3;

/// A soft-knee limiter: unity for small signals, asymptotically bounded by `ceiling`.
///
/// `tanh` rather than a hard clip because the fold-down sums *every* channel raw — with 28
/// channels summed into one leg the input routinely runs far past full scale, and hard clipping
/// that produces the harsh broadband splatter of a squared-off wave where saturation produces a
/// gradual, musically legible compression. Scaling the input by the ceiling before the curve and
/// back out after is what keeps the small-signal region at unity gain rather than at `ceiling`.
pub fn tanh_limit(sample: f32, ceiling: f32) -> f32 {
    if ceiling <= 0.0 {
        return 0.0;
    }
    ceiling * (sample / ceiling).tanh()
}

/// Channels playback actually emits for a source of `source_channels`.
///
/// Stereo for anything with 3 or more, otherwise the count itself. The single definition of the
/// fold-down rule: the resident source, the streamed reader and the channel count each declares to
/// rodio all ask this, so a source can never emit a different number of channels than it announced.
pub fn playback_channels(source_channels: usize) -> usize {
    if source_channels >= DOWNMIX_MIN_CHANNELS {
        2
    } else {
        source_channels.max(1)
    }
}

/// One frame folded to stereo: odd-numbered channels (1, 3, 5… — even *indices*) summed into the
/// left leg, even-numbered ones into the right, each then limited to `ceiling`.
///
/// The sums are raw, with no 1/N or 1/√N attenuation: what the limiter is for is exactly to
/// contain them, and dividing by the channel count would make a 30-channel take play far quieter
/// than the same material as a stereo file.
///
/// Reads defensively (`get`) rather than indexing, since a caller may hand this a block whose
/// channels are ragged at end of file.
pub fn downmix_frame(channels: &[Vec<f32>], frame: usize, ceiling: f32) -> (f32, f32) {
    let mut left = 0.0f32;
    let mut right = 0.0f32;
    for (index, channel) in channels.iter().enumerate() {
        let sample = channel.get(frame).copied().unwrap_or(0.0);
        if index % 2 == 0 {
            left += sample;
        } else {
            right += sample;
        }
    }
    (tanh_limit(left, ceiling), tanh_limit(right, ceiling))
}

/// Appends `frames` frames of `channels` to `out` as interleaved playback samples — folded to
/// stereo and limited when there are 3+ channels, passed through verbatim otherwise.
///
/// The block form of [`downmix_frame`], for the streamed reader, which has whole blocks in hand
/// rather than one frame at a time. `out_channels` is captured once when playback starts (the
/// count already announced to the device) rather than re-derived per block, so a channel map
/// edited mid-playback can change what is heard but never how many channels arrive.
pub fn playback_block(
    channels: &[Vec<f32>],
    frames: usize,
    out_channels: usize,
    ceiling: f32,
    out: &mut Vec<f32>,
) {
    out.reserve(frames * out_channels);
    let downmix = channels.len() >= DOWNMIX_MIN_CHANNELS && out_channels == 2;
    for frame in 0..frames {
        if downmix {
            let (left, right) = downmix_frame(channels, frame, ceiling);
            out.push(left);
            out.push(right);
        } else {
            for channel in 0..out_channels {
                out.push(
                    channels.get(channel).and_then(|c| c.get(frame)).copied().unwrap_or(0.0),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_peak_of_silence_and_of_nothing_is_zero() {
        assert_eq!(channel_peak(&[0.0; 16]), 0.0);
        assert_eq!(channel_peak(&[]), 0.0);
        assert_eq!(channel_peak(&[0.1, -0.7, 0.3]), 0.7);
    }

    /// The case the linear-domain comparison exists for: a digitally silent channel must be
    /// found at *any* threshold, including ones below `linear_to_db`'s -120 dB clamp.
    #[test]
    fn a_digitally_silent_channel_is_below_every_threshold() {
        let channels = vec![vec![0.0f32; 8]];
        for threshold in [-6.0, -48.0, -96.0, -150.0, -300.0] {
            assert_eq!(
                channels_below(&channels, threshold),
                vec![0],
                "silence must count as below {threshold} dBFS",
            );
        }
    }

    #[test]
    fn channels_below_finds_the_quiet_ones_in_ascending_order() {
        let channels = vec![
            vec![0.0f32; 4],           // silent
            vec![0.5f32; 4],           // -6 dBFS
            vec![0.001f32; 4],         // -60 dBFS
            vec![0.9f32; 4],           // loud
        ];
        assert_eq!(channels_below(&channels, -48.0), vec![0, 2]);
    }

    /// Strictly below: a channel sitting exactly on the threshold is kept, because at a
    /// boundary keeping audio is the recoverable choice.
    #[test]
    fn a_channel_exactly_at_the_threshold_is_kept() {
        let exact = db_to_linear(-48.0);
        let channels = vec![vec![exact; 4]];
        assert!(channels_below(&channels, -48.0).is_empty());
    }

    #[test]
    fn db_to_linear_maps_known_points() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_linear(-6.0) - 0.5012).abs() < 1e-3);
        assert!((db_to_linear(-20.0) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn linear_to_db_maps_known_points() {
        assert!((linear_to_db(1.0) - 0.0).abs() < 1e-3);
        assert!((linear_to_db(0.5) - (-6.02)).abs() < 1e-1);
        assert!((linear_to_db(0.1) - (-20.0)).abs() < 1e-3);
    }

    #[test]
    fn linear_to_db_of_zero_is_finite() {
        assert!(linear_to_db(0.0).is_finite());
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

    /// The fold-down threshold, stated as the rule every playback path reads it as.
    #[test]
    fn only_three_or_more_channels_fold_down() {
        assert_eq!(playback_channels(1), 1);
        assert_eq!(playback_channels(2), 2);
        assert_eq!(playback_channels(3), 2);
        assert_eq!(playback_channels(56), 2);
        // A document with no channels still has to announce a legal count to the device.
        assert_eq!(playback_channels(0), 1);
    }

    /// Unity in the small-signal region (so quiet material is untouched), and bounded by the
    /// ceiling however hard it is driven — the two properties the fold-down depends on.
    #[test]
    fn the_limiter_is_transparent_when_quiet_and_bounded_when_not() {
        let ceiling = db_to_linear(PLAYBACK_CEILING_DB);
        assert!((tanh_limit(0.01, ceiling) - 0.01).abs() < 1e-4, "quiet signals pass through");
        assert_eq!(tanh_limit(0.0, ceiling), 0.0);
        // At or below the ceiling, never past it. `tanh` saturates to exactly 1.0 in f32 well
        // before the drive a 28-channel sum reaches, so heavy overdrive lands *on* the ceiling
        // rather than approaching it — which is the bound this is here to guarantee.
        for drive in [1.0f32, 4.0, 30.0, 1000.0] {
            assert!(tanh_limit(drive, ceiling) <= ceiling, "{drive} must stay under the ceiling");
            assert!(tanh_limit(-drive, ceiling) >= -ceiling, "and so must -{drive}");
        }
        assert!(tanh_limit(1.0, ceiling) < ceiling, "a mild overdrive still has room to move");
        // -1 dBFS, not 0: the point of the ceiling is that it leaves a dB of headroom.
        assert!((ceiling - 0.8913).abs() < 1e-3);
        // Odd-symmetric, so the limiter cannot introduce a DC offset.
        assert!((tanh_limit(2.0, ceiling) + tanh_limit(-2.0, ceiling)).abs() < 1e-6);
    }

    /// Odd-numbered channels left, even-numbered right — stated in the 1-based terms the UI and
    /// the user use, against the 0-based indices the code uses.
    #[test]
    fn odd_numbered_channels_go_left_and_even_numbered_right() {
        let ceiling = db_to_linear(PLAYBACK_CEILING_DB);
        // ch1=0.1 ch2=0.2 ch3=0.3 ch4=0.4 ch5=0.5  ->  L = 0.1+0.3+0.5, R = 0.2+0.4
        let channels = vec![vec![0.1f32], vec![0.2], vec![0.3], vec![0.4], vec![0.5]];
        let (left, right) = downmix_frame(&channels, 0, ceiling);
        assert!((left - tanh_limit(0.9, ceiling)).abs() < 1e-6);
        assert!((right - tanh_limit(0.6, ceiling)).abs() < 1e-6);
        // An odd channel count leaves the last channel on the left leg, unpaired.
        let (left, right) = downmix_frame(&[vec![0.5f32], vec![0.0], vec![0.5]], 0, ceiling);
        assert!((left - tanh_limit(1.0, ceiling)).abs() < 1e-6);
        assert_eq!(right, 0.0);
    }

    /// The sum is raw — no 1/N — which is what makes a many-channel take play at a comparable
    /// level to a stereo one instead of far quieter, and what the limiter then contains.
    #[test]
    fn the_fold_down_sums_raw_and_leans_on_the_limiter() {
        let ceiling = db_to_linear(PLAYBACK_CEILING_DB);
        let channels: Vec<Vec<f32>> = (0..28).map(|_| vec![0.5f32]).collect();
        let (left, right) = downmix_frame(&channels, 0, ceiling);
        // 14 channels of 0.5 per leg = 7.0 raw, driven hard into the limiter but still bounded.
        assert!(left > 0.88 && left < ceiling, "left leg sits just under the ceiling: {left}");
        assert!((left - right).abs() < 1e-6, "both legs carry identical material here");
        // An average would have landed at 0.5; a raw sum plus limiting lands near full scale.
        assert!(left > 0.5);
    }

    /// A frame past the end of a ragged block reads as silence rather than panicking — blocks
    /// run short at end of file, and the reader must not be the thing that notices.
    #[test]
    fn a_frame_past_the_end_of_a_ragged_block_is_silence() {
        let ceiling = db_to_linear(PLAYBACK_CEILING_DB);
        let channels = vec![vec![0.5f32, 0.5], vec![0.5], vec![]];
        let (left, right) = downmix_frame(&channels, 1, ceiling);
        assert!((left - tanh_limit(0.5, ceiling)).abs() < 1e-6);
        assert_eq!(right, 0.0);
    }

    /// One and two channel blocks are copied through the block path verbatim: no fold-down and,
    /// critically, no limiting.
    #[test]
    fn one_and_two_channel_blocks_pass_through_untouched() {
        let ceiling = db_to_linear(PLAYBACK_CEILING_DB);
        let mut out = Vec::new();
        playback_block(&[vec![1.0f32, -1.0]], 2, 1, ceiling, &mut out);
        assert_eq!(out, vec![1.0, -1.0], "full-scale mono must not be softened");

        out.clear();
        playback_block(&[vec![0.9f32, 0.8], vec![-0.9, -0.8]], 2, 2, ceiling, &mut out);
        assert_eq!(out, vec![0.9, -0.9, 0.8, -0.8], "interleaved, verbatim");
    }

    /// The block path and the per-frame path must agree exactly — they are the streamed and
    /// resident halves of the same fold-down, and a difference between them would mean the same
    /// audio sounded different depending on how the file happened to be opened.
    #[test]
    fn the_block_path_matches_the_per_frame_path() {
        let ceiling = db_to_linear(PLAYBACK_CEILING_DB);
        let channels: Vec<Vec<f32>> = (0..7)
            .map(|c| (0..5).map(|f| (c as f32 * 0.13 - f as f32 * 0.07).sin()).collect())
            .collect();
        let mut out = Vec::new();
        playback_block(&channels, 5, 2, ceiling, &mut out);
        assert_eq!(out.len(), 10);
        for frame in 0..5 {
            let (left, right) = downmix_frame(&channels, frame, ceiling);
            assert_eq!(out[frame * 2], left);
            assert_eq!(out[frame * 2 + 1], right);
        }
    }
}
