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

/// A channel counts toward its leg's divisor only if its peak reaches this.
///
/// The same -48 dBFS Remove Empty Channels defaults to, and deliberately so: a channel the user
/// would call empty is a channel that must not quieten the ones carrying signal. See
/// [`Fold::from_peaks`] for why that matters so much on these files.
pub const FOLD_ACTIVE_PEAK_DB: f32 = -48.0;

/// How the stereo fold-down sounds: a gain per leg, and the limiter ceiling they feed.
///
/// Bundled rather than passed as three loose floats because these only ever make sense together —
/// the gains are chosen *relative* to the ceiling they are about to be limited against, and the
/// source, the streamed reader and the engine each need to carry all of them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fold {
    pub left_gain: f32,
    pub right_gain: f32,
    pub ceiling: f32,
}

impl Default for Fold {
    /// Unity gain at the standard ceiling — a raw sum. What a document with no measured peaks
    /// gets (the pyramid is still building, or there is nothing to measure), and never wrong so
    /// much as un-tuned: it is exactly the behaviour that existed before gains did.
    fn default() -> Self {
        Self {
            left_gain: 1.0,
            right_gain: 1.0,
            ceiling: db_to_linear(PLAYBACK_CEILING_DB),
        }
    }
}

impl Fold {
    /// Per-leg gains of `1/√n`, where `n` counts only the channels on that leg whose peak reaches
    /// [`FOLD_ACTIVE_PEAK_DB`].
    ///
    /// **Active channels, not channel slots**, and that distinction is the whole design. These
    /// files are mostly empty by construction — a 56-channel rig commonly runs 48 dead inputs —
    /// so a divisor counting slots would attenuate a 56-channel take by 14.5 dB whether it holds
    /// 6 channels of music or 56. The material would come out near-inaudible and the limiter,
    /// which the divisor exists to unburden, would never engage at all. Counting what is actually
    /// contributing gives the same protection on a dense take (14 loud channels a leg go from
    /// ~18 dB into the limiter to ~6) while leaving a sparse one at a sensible monitoring level.
    ///
    /// √n rather than n because separately-miked sources are largely uncorrelated: their sum
    /// grows as √n, so dividing by √n holds the perceived level roughly constant. Correlated
    /// material still sums faster than that, which is what the limiter is left to catch.
    ///
    /// `peaks` is indexed by *logical* channel — what the user sees, and what plays.
    pub fn from_peaks(peaks: &[f32]) -> Self {
        let threshold = db_to_linear(FOLD_ACTIVE_PEAK_DB);
        // At-or-above counts as active, mirroring `channels_below`'s strictly-below test for
        // what Remove Empty Channels drops — so a channel sitting exactly on the threshold is
        // one this app keeps *and* counts, rather than falling between the two rules.
        let active = |leg: usize| {
            peaks
                .iter()
                .skip(leg)
                .step_by(2)
                .filter(|&&peak| peak >= threshold)
                .count()
                .max(1)
        };
        Self {
            left_gain: 1.0 / (active(0) as f32).sqrt(),
            right_gain: 1.0 / (active(1) as f32).sqrt(),
            ..Self::default()
        }
    }

    /// [`Self::from_peaks`] for a document whose samples are in hand, measuring the peaks itself.
    ///
    /// For the callers with no waveform pyramid to read peaks out of — the Files-panel audition
    /// preview and the CDP preview, each of which plays a freshly-loaded buffer of its own. Costs
    /// one pass over the samples, which is why the main engine uses `from_peaks` against the
    /// pyramid's already-measured peaks instead.
    pub fn from_channels(channels: &[Vec<f32>]) -> Self {
        let peaks: Vec<f32> = channels.iter().map(|c| channel_peak(c)).collect();
        Self::from_peaks(&peaks)
    }

    /// One frame folded to stereo: odd-numbered channels (1, 3, 5… — even *indices*) summed into
    /// the left leg, even-numbered ones into the right, each scaled by its leg's gain and then
    /// limited.
    ///
    /// The gain lands *before* the limiter, so it sets both the output level and how hard the
    /// limiter is driven — which is the point: an under-driven limiter is a transparent one.
    ///
    /// Reads defensively (`get`) rather than indexing, since a caller may hand this a block whose
    /// channels are ragged at end of file.
    pub fn frame(&self, channels: &[Vec<f32>], frame: usize) -> (f32, f32) {
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
        (
            tanh_limit(left * self.left_gain, self.ceiling),
            tanh_limit(right * self.right_gain, self.ceiling),
        )
    }

    /// Appends `frames` frames of `channels` to `out` as interleaved playback samples — folded to
    /// stereo when there are 3+ channels, passed through verbatim otherwise.
    ///
    /// The block form of [`Self::frame`], for the streamed reader, which has whole blocks in hand
    /// rather than one frame at a time. `out_channels` is captured once when playback starts (the
    /// count already announced to the device) rather than re-derived per block, so a channel map
    /// edited mid-playback can change what is heard but never how many channels arrive.
    pub fn block(
        &self,
        channels: &[Vec<f32>],
        frames: usize,
        out_channels: usize,
        out: &mut Vec<f32>,
    ) {
        out.reserve(frames * out_channels);
        let downmix = channels.len() >= DOWNMIX_MIN_CHANNELS && out_channels == 2;
        for frame in 0..frames {
            if downmix {
                let (left, right) = self.frame(channels, frame);
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
        let fold = Fold::default();
        let ceiling = fold.ceiling;
        // ch1=0.1 ch2=0.2 ch3=0.3 ch4=0.4 ch5=0.5  ->  L = 0.1+0.3+0.5, R = 0.2+0.4
        let channels = vec![vec![0.1f32], vec![0.2], vec![0.3], vec![0.4], vec![0.5]];
        let (left, right) = fold.frame(&channels, 0);
        assert!((left - tanh_limit(0.9, ceiling)).abs() < 1e-6);
        assert!((right - tanh_limit(0.6, ceiling)).abs() < 1e-6);
        // An odd channel count leaves the last channel on the left leg, unpaired.
        let (left, right) = fold.frame(&[vec![0.5f32], vec![0.0], vec![0.5]], 0);
        assert!((left - tanh_limit(1.0, ceiling)).abs() < 1e-6);
        assert_eq!(right, 0.0);
    }

    /// The gain law: 1/√n per leg, counting only channels that carry signal.
    #[test]
    fn the_divisor_is_the_square_root_of_the_active_channels_on_each_leg() {
        // 6 channels, all loud: 3 per leg, so both legs divide by √3.
        let fold = Fold::from_peaks(&[0.9; 6]);
        assert!((fold.left_gain - 1.0 / 3.0f32.sqrt()).abs() < 1e-6);
        assert!((fold.right_gain - 1.0 / 3.0f32.sqrt()).abs() < 1e-6);

        // Legs are counted independently: 4 active on the left, 1 on the right.
        let fold = Fold::from_peaks(&[0.9, 0.9, 0.9, 0.0, 0.9, 0.0, 0.9, 0.0]);
        assert!((fold.left_gain - 0.5).abs() < 1e-6, "1/√4");
        assert!((fold.right_gain - 1.0).abs() < 1e-6, "1/√1");

        // The ceiling is not something `from_peaks` gets to move.
        assert_eq!(fold.ceiling, Fold::default().ceiling);
    }

    /// The case the whole design turns on: on these files most channels are empty, and an empty
    /// channel must not quieten the ones carrying signal.
    ///
    /// A 56-channel take with 6 live channels would be attenuated 14.5 dB by a divisor counting
    /// slots — inaudible, and the limiter it exists to unburden would never engage anyway.
    #[test]
    fn silent_channels_do_not_dilute_the_ones_with_signal() {
        let mut peaks = vec![0.0f32; 56];
        for ch in [0, 1, 2, 3, 4, 5] {
            peaks[ch] = 0.25;
        }
        let fold = Fold::from_peaks(&peaks);
        // 3 active per leg, not 28.
        assert!((fold.left_gain - 1.0 / 3.0f32.sqrt()).abs() < 1e-6);
        assert!((fold.right_gain - 1.0 / 3.0f32.sqrt()).abs() < 1e-6);

        // Three uncorrelated channels at -12 dBFS peaking together reach 0.75; folded, that must
        // still land at a usable monitoring level rather than 20 dB down.
        let frame: Vec<Vec<f32>> = (0..56).map(|c| vec![if c < 6 { 0.25f32 } else { 0.0 }]).collect();
        let (left, _) = fold.frame(&frame, 0);
        assert!(
            linear_to_db(left) > -14.0,
            "a sparse take must monitor at a usable level, got {} dBFS",
            linear_to_db(left)
        );
    }

    /// A channel below the activity threshold does not count, one at or above it does — the same
    /// boundary rule Remove Empty Channels uses, so the two can never disagree about which
    /// channels are "empty".
    #[test]
    fn the_activity_threshold_matches_what_remove_empty_channels_keeps() {
        let exact = db_to_linear(FOLD_ACTIVE_PEAK_DB);
        // Exactly at the threshold: kept by `channels_below` (strictly-below drops), so counted.
        assert!(channels_below(&[vec![exact; 4]], FOLD_ACTIVE_PEAK_DB).is_empty());
        let fold = Fold::from_peaks(&[exact, exact]);
        assert!((fold.left_gain - 1.0).abs() < 1e-6);

        // Just below it: dropped there, uncounted here. Two active channels become one, so the
        // divisor falls from √2 to √1.
        let under = exact * 0.99;
        let fold = Fold::from_peaks(&[0.9, 0.9, under, under]);
        assert!((fold.left_gain - 1.0).abs() < 1e-6, "only channel 1 is active on the left");
        assert!((fold.right_gain - 1.0).abs() < 1e-6, "only channel 2 is active on the right");
    }

    /// A leg with nothing on it still needs a finite gain — dividing by √0 would be an infinity
    /// straight into the limiter.
    #[test]
    fn a_leg_with_no_active_channels_gets_unity_not_infinity() {
        let fold = Fold::from_peaks(&[0.0; 8]);
        assert_eq!(fold.left_gain, 1.0);
        assert_eq!(fold.right_gain, 1.0);
        let fold = Fold::from_peaks(&[]);
        assert!(fold.left_gain.is_finite() && fold.right_gain.is_finite());
    }

    /// The dense case the law exists to soften: 14 loud channels a leg used to arrive ~18 dB into
    /// the limiter (a fully saturated square wave); √14 brings that down to a few dB while still
    /// reaching a healthy level.
    #[test]
    fn a_dense_take_is_no_longer_slammed_into_the_limiter() {
        let peaks = vec![0.5f32; 28];
        let fold = Fold::from_peaks(&peaks);
        let channels: Vec<Vec<f32>> = (0..28).map(|_| vec![0.5f32]).collect();
        let (left, right) = fold.frame(&channels, 0);

        // 14 × 0.5 = 7.0 raw, ÷√14 = 1.87 — still over the ceiling, so the limiter does engage,
        // but by ~6 dB rather than ~18.
        let drive_db = linear_to_db(7.0 * fold.left_gain / fold.ceiling);
        assert!((4.0..9.0).contains(&drive_db), "drive should be a few dB, got {drive_db}");
        assert!(left <= fold.ceiling, "and it is still bounded");
        assert!(left > 0.7, "while staying at a healthy monitoring level");
        assert!((left - right).abs() < 1e-6, "both legs carry identical material here");
    }

    /// A frame past the end of a ragged block reads as silence rather than panicking — blocks
    /// run short at end of file, and the reader must not be the thing that notices.
    #[test]
    fn a_frame_past_the_end_of_a_ragged_block_is_silence() {
        let fold = Fold::default();
        let channels = vec![vec![0.5f32, 0.5], vec![0.5], vec![]];
        let (left, right) = fold.frame(&channels, 1);
        assert!((left - tanh_limit(0.5, fold.ceiling)).abs() < 1e-6);
        assert_eq!(right, 0.0);
    }

    /// One and two channel blocks are copied through the block path verbatim: no fold-down, no
    /// gain and, critically, no limiting.
    #[test]
    fn one_and_two_channel_blocks_pass_through_untouched() {
        // Gains that would be audible if they were ever applied here.
        let fold = Fold { left_gain: 0.25, right_gain: 0.25, ..Fold::default() };
        let mut out = Vec::new();
        fold.block(&[vec![1.0f32, -1.0]], 2, 1, &mut out);
        assert_eq!(out, vec![1.0, -1.0], "full-scale mono must not be softened");

        out.clear();
        fold.block(&[vec![0.9f32, 0.8], vec![-0.9, -0.8]], 2, 2, &mut out);
        assert_eq!(out, vec![0.9, -0.9, 0.8, -0.8], "interleaved, verbatim");
    }

    /// The block path and the per-frame path must agree exactly — they are the streamed and
    /// resident halves of the same fold-down, and a difference between them would mean the same
    /// audio sounded different depending on how the file happened to be opened.
    #[test]
    fn the_block_path_matches_the_per_frame_path() {
        let fold = Fold::from_peaks(&[0.9, 0.4, 0.0, 0.8, 0.9, 0.0, 0.3]);
        let channels: Vec<Vec<f32>> = (0..7)
            .map(|c| (0..5).map(|f| (c as f32 * 0.13 - f as f32 * 0.07).sin()).collect())
            .collect();
        let mut out = Vec::new();
        fold.block(&channels, 5, 2, &mut out);
        assert_eq!(out.len(), 10);
        for frame in 0..5 {
            let (left, right) = fold.frame(&channels, frame);
            assert_eq!(out[frame * 2], left);
            assert_eq!(out[frame * 2 + 1], right);
        }
    }
}
