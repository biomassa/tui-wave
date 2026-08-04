//! Mix a multichannel document down to stereo under explicit per-channel control.
//!
//! The playback fold (`dsp::Fold`) answers a different question: what should 56 channels sound
//! like *right now*, with no one to ask. It is automatic, fixed (odd indices left, even right)
//! and derives its gains from measured peaks. This is the deliberate counterpart — the routing
//! and the level of every channel are stated by the user, and the result is a document rather
//! than something the device hears and forgets.
//!
//! Kept in `model` and free of any UI type so the whole mixdown is unit-testable without a
//! terminal, in line with the layering rule.

use crate::model::dsp;

/// Where one source channel is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StereoMixDest {
    Left,
    Right,
    Both,
    Skip,
}

impl StereoMixDest {
    /// The label the dialog shows for this destination. Lives here rather than in the widget so
    /// the render, the click hit-test and any test all name a destination identically.
    pub fn label(self) -> &'static str {
        match self {
            StereoMixDest::Left => "Left",
            StereoMixDest::Right => "Right",
            StereoMixDest::Both => "Both",
            StereoMixDest::Skip => "Skip",
        }
    }
}

/// Per-channel attenuation the dialog starts every row at, in dB.
///
/// Not 0: a mixdown sums many channels into two, so unity on every channel is the one setting
/// guaranteed to clip on anything but a sparse take. -6 dB is two channels' worth of headroom per
/// leg before the sum reaches full scale, which is the common case, and it is a round number to
/// undo by eye.
pub const DEFAULT_ATTENUATION_DB: f32 = -6.0;

/// Extra attenuation applied to each leg of a channel routed to [`StereoMixDest::Both`], in dB.
///
/// The -3 dB pan law. A channel sent to both legs at its stated gain would sum, for a listener,
/// to roughly twice the power of the same channel hard-panned at that gain, so centred material
/// would sit audibly louder than anything panned — and the fix has to be here rather than in the
/// user's own numbers, since it is a property of being centred rather than of any one channel.
pub const BOTH_LEG_DB: f32 = -3.0;

/// The ceiling of the optional output limiter, in dBFS.
///
/// The same [`dsp::PLAYBACK_CEILING_DB`] the playback fold limits against, and deliberately the
/// same: the fold is what the user was monitoring while deciding these routings, so a mix that
/// limited to a different ceiling would not sound like the thing being mixed. It also leaves a
/// dB of headroom for whatever lossy encoder the result is eventually handed to.
pub const LIMIT_CEILING_DB: f32 = dsp::PLAYBACK_CEILING_DB;

/// Value a gain field holds to mean "contributes nothing", matching `Dialog::MixToMono`.
pub const SILENCE_TOKEN: &str = "-inf";

/// Opening routing: channel 1 left, channel 2 right, channel 3 left, and so on.
///
/// Stated in *channel numbers* because that is what the user sees and what the request was
/// phrased in — odd-numbered channels left, even-numbered right — which is even indices left and
/// odd indices right. The same interleaving `dsp::Fold` uses, so the dialog opens on the routing
/// the file was already playing back through and every edit is a departure from a known state.
pub fn default_dests(channel_count: usize) -> Vec<StereoMixDest> {
    (0..channel_count)
        .map(|i| {
            if i % 2 == 0 {
                StereoMixDest::Left
            } else {
                StereoMixDest::Right
            }
        })
        .collect()
}

/// ←/→: cycle Left → Right → Both → Skip → Left.
///
/// Every destination is legal on every row — unlike `channel_export::cycle_mode`, which has to
/// skip a pairing that isn't available — so this is a plain rotation with nothing to veto.
pub fn cycle_dest(dests: &mut [StereoMixDest], i: usize, forward: bool) {
    let Some(slot) = dests.get_mut(i) else { return };
    let order = [
        StereoMixDest::Left,
        StereoMixDest::Right,
        StereoMixDest::Both,
        StereoMixDest::Skip,
    ];
    let at = order.iter().position(|d| d == slot).unwrap_or(0);
    let next = if forward {
        (at + 1) % order.len()
    } else {
        (at + order.len() - 1) % order.len()
    };
    *slot = order[next];
}

/// Parses one gain field into a linear factor, `None` when the channel contributes nothing.
///
/// Accepts [`SILENCE_TOKEN`] and the empty field as silence, matching the Mix to Mono field this
/// borrows its editing behaviour from. A value that doesn't parse is *also* silence here, which
/// differs from Mix to Mono's "fall back to unity": unity is the loudest thing a field can mean,
/// so a typo would be the one outcome that clips, and on a 56-row dialog a bad row is easy to
/// miss. Failing quiet is recoverable by ear; failing loud is not.
pub fn parse_gain_db(raw: &str) -> Option<f32> {
    let trimmed = raw.trim().to_lowercase();
    if trimmed.is_empty() || trimmed == SILENCE_TOKEN {
        return None;
    }
    trimmed.parse::<f32>().ok()
}

/// Linear gains `(left, right)` one channel contributes, given its destination and dB attenuation.
///
/// `Both` picks up [`BOTH_LEG_DB`] on each leg; `Skip` and an unparseable/silent gain contribute
/// nothing at all. Separated from the mixing loop so the pan law is one statement that a test can
/// address directly, rather than something only observable through summed audio.
pub fn leg_gains(dest: StereoMixDest, gain_db: Option<f32>) -> (f32, f32) {
    let Some(db) = gain_db else { return (0.0, 0.0) };
    match dest {
        StereoMixDest::Skip => (0.0, 0.0),
        StereoMixDest::Left => (dsp::db_to_linear(db), 0.0),
        StereoMixDest::Right => (0.0, dsp::db_to_linear(db)),
        StereoMixDest::Both => {
            let g = dsp::db_to_linear(db + BOTH_LEG_DB);
            (g, g)
        }
    }
}

/// Whether this routing sends anything anywhere — what the dialog dims Apply on.
///
/// A mix of nothing is two channels of digital silence, which is never what was intended and is
/// indistinguishable from a broken process once it lands in a new buffer.
pub fn is_silent(dests: &[StereoMixDest], gains_db: &[Option<f32>]) -> bool {
    dests.iter().enumerate().all(|(i, &dest)| {
        let gain = gains_db.get(i).copied().flatten();
        leg_gains(dest, gain) == (0.0, 0.0)
    })
}

/// Mixes `[start, start + len)` of `channels` down to two channels.
///
/// `dests` and `gains_db` are indexed by source channel; a channel past the end of either
/// contributes nothing, so a ragged call is silent rather than panicking. `ceiling` is the
/// limiter's linear ceiling, `None` to leave the sum untouched.
///
/// The limiter runs *after* the whole sum, not per contribution — limiting each channel on the
/// way in would bound each one individually and still let the sum run past full scale, which is
/// the opposite of the point.
pub fn mix_to_stereo(
    channels: &[Vec<f32>],
    start: usize,
    len: usize,
    dests: &[StereoMixDest],
    gains_db: &[Option<f32>],
    ceiling: Option<f32>,
) -> Vec<Vec<f32>> {
    let mut left = vec![0.0f32; len];
    let mut right = vec![0.0f32; len];

    for (index, channel) in channels.iter().enumerate() {
        let dest = dests.get(index).copied().unwrap_or(StereoMixDest::Skip);
        let gain = gains_db.get(index).copied().flatten();
        let (lg, rg) = leg_gains(dest, gain);
        if lg == 0.0 && rg == 0.0 {
            continue;
        }
        // Clamped rather than assumed in range: a document's channels are all the same length in
        // practice, but this is handed slices from callers that computed the range from channel 0.
        let end = (start + len).min(channel.len());
        if start >= end {
            continue;
        }
        let slice = &channel[start..end];
        if lg != 0.0 {
            for (out, &v) in left.iter_mut().zip(slice) {
                *out += v * lg;
            }
        }
        if rg != 0.0 {
            for (out, &v) in right.iter_mut().zip(slice) {
                *out += v * rg;
            }
        }
    }

    if let Some(ceiling) = ceiling {
        for out in left.iter_mut().chain(right.iter_mut()) {
            *out = dsp::tanh_limit(*out, ceiling);
        }
    }

    vec![left, right]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn defaults_send_odd_numbered_channels_left_and_even_numbered_right() {
        let dests = default_dests(5);
        // Channel numbers 1,3,5 (indices 0,2,4) left; channels 2,4 (indices 1,3) right.
        assert_eq!(
            dests,
            vec![
                StereoMixDest::Left,
                StereoMixDest::Right,
                StereoMixDest::Left,
                StereoMixDest::Right,
                StereoMixDest::Left,
            ]
        );
    }

    #[test]
    fn the_default_routing_matches_the_playback_folds_own_interleave() {
        // The fold sums even *indices* into the left leg; the dialog must open on the same
        // split or the mix would not resemble what the user was monitoring.
        for (index, dest) in default_dests(56).into_iter().enumerate() {
            let fold_leg_is_left = index % 2 == 0;
            assert_eq!(dest == StereoMixDest::Left, fold_leg_is_left, "channel index {index}");
        }
    }

    #[test]
    fn cycling_rotates_in_both_directions_and_wraps() {
        let mut dests = vec![StereoMixDest::Left];
        for expected in [
            StereoMixDest::Right,
            StereoMixDest::Both,
            StereoMixDest::Skip,
            StereoMixDest::Left,
        ] {
            cycle_dest(&mut dests, 0, true);
            assert_eq!(dests[0], expected);
        }
        for expected in [
            StereoMixDest::Skip,
            StereoMixDest::Both,
            StereoMixDest::Right,
            StereoMixDest::Left,
        ] {
            cycle_dest(&mut dests, 0, false);
            assert_eq!(dests[0], expected);
        }
    }

    #[test]
    fn cycling_an_out_of_range_row_is_a_no_op_rather_than_a_panic() {
        let mut dests = vec![StereoMixDest::Left];
        cycle_dest(&mut dests, 9, true);
        assert_eq!(dests, vec![StereoMixDest::Left]);
    }

    #[test]
    fn a_blank_or_inf_field_means_silence_and_a_typo_does_too() {
        assert_eq!(parse_gain_db(""), None);
        assert_eq!(parse_gain_db("  "), None);
        assert_eq!(parse_gain_db("-inf"), None);
        assert_eq!(parse_gain_db("-INF"), None);
        // Deliberately silent rather than unity — see `parse_gain_db`.
        assert_eq!(parse_gain_db("nonsense"), None);
        assert_eq!(parse_gain_db("-6"), Some(-6.0));
        assert_eq!(parse_gain_db(" -6.5 "), Some(-6.5));
        assert_eq!(parse_gain_db("3"), Some(3.0));
    }

    #[test]
    fn hard_panning_sends_the_stated_gain_to_one_leg_and_nothing_to_the_other() {
        let (l, r) = leg_gains(StereoMixDest::Left, Some(-6.0));
        assert!(approx(l, dsp::db_to_linear(-6.0)));
        assert_eq!(r, 0.0);

        let (l, r) = leg_gains(StereoMixDest::Right, Some(-6.0));
        assert_eq!(l, 0.0);
        assert!(approx(r, dsp::db_to_linear(-6.0)));
    }

    #[test]
    fn both_costs_an_extra_3db_per_leg_so_centred_material_is_not_louder_than_panned() {
        let (l, r) = leg_gains(StereoMixDest::Both, Some(-6.0));
        assert!(approx(l, r), "both legs must be equal");
        assert!(approx(l, dsp::db_to_linear(-9.0)));

        // The point of the law: total *power* of a centred channel matches that of a panned one.
        //
        // Only to within 0.02 dB, and deliberately so. Exact half power is -3.0103 dB; this uses
        // the conventional -3, because that is the number the pan law is known by and the number
        // a user reading the constant expects to find. The 0.24% power error it buys is four
        // orders of magnitude below audibility, so naming the law correctly is worth more than
        // matching it exactly.
        let (panned, _) = leg_gains(StereoMixDest::Left, Some(-6.0));
        let centred_power = l * l + r * r;
        let error_db = 10.0 * (centred_power / (panned * panned)).log10();
        assert!(
            error_db.abs() < 0.02,
            "centred power is {error_db} dB off panned power, beyond the rounding of -3.0103 to -3"
        );
    }

    #[test]
    fn skip_and_silence_contribute_nothing() {
        assert_eq!(leg_gains(StereoMixDest::Skip, Some(0.0)), (0.0, 0.0));
        assert_eq!(leg_gains(StereoMixDest::Left, None), (0.0, 0.0));
        assert_eq!(leg_gains(StereoMixDest::Both, None), (0.0, 0.0));
    }

    #[test]
    fn a_routing_that_sends_nothing_anywhere_is_reported_silent() {
        assert!(is_silent(
            &[StereoMixDest::Skip, StereoMixDest::Skip],
            &[Some(0.0), Some(0.0)]
        ));
        assert!(is_silent(
            &[StereoMixDest::Left, StereoMixDest::Right],
            &[None, None]
        ));
        assert!(!is_silent(
            &[StereoMixDest::Skip, StereoMixDest::Right],
            &[Some(0.0), Some(-6.0)]
        ));
    }

    #[test]
    fn mixing_routes_each_channel_to_the_leg_it_was_assigned() {
        let channels = vec![vec![1.0, 1.0], vec![0.5, 0.5], vec![0.25, 0.25]];
        let dests = [StereoMixDest::Left, StereoMixDest::Right, StereoMixDest::Skip];
        let gains = [Some(0.0), Some(0.0), Some(0.0)];

        let out = mix_to_stereo(&channels, 0, 2, &dests, &gains, None);
        assert_eq!(out.len(), 2);
        // Channel 3 is skipped, so it appears in neither leg.
        assert!(approx(out[0][0], 1.0));
        assert!(approx(out[1][0], 0.5));
    }

    #[test]
    fn channels_on_the_same_leg_sum() {
        let channels = vec![vec![0.25], vec![0.25], vec![0.25]];
        let dests = [StereoMixDest::Left; 3];
        let gains = [Some(0.0); 3];

        let out = mix_to_stereo(&channels, 0, 1, &dests, &gains, None);
        assert!(approx(out[0][0], 0.75));
        assert_eq!(out[1][0], 0.0);
    }

    #[test]
    fn attenuation_is_applied_per_channel() {
        let channels = vec![vec![1.0], vec![1.0]];
        let dests = [StereoMixDest::Left, StereoMixDest::Left];
        // One channel at unity, one 6 dB down.
        let gains = [Some(0.0), Some(-6.0)];

        let out = mix_to_stereo(&channels, 0, 1, &dests, &gains, None);
        assert!(approx(out[0][0], 1.0 + dsp::db_to_linear(-6.0)));
    }

    #[test]
    fn only_the_requested_range_is_mixed() {
        let channels = vec![vec![0.0, 1.0, 1.0, 0.0]];
        let dests = [StereoMixDest::Left];
        let gains = [Some(0.0)];

        let out = mix_to_stereo(&channels, 1, 2, &dests, &gains, None);
        assert_eq!(out[0], vec![1.0, 1.0]);
    }

    #[test]
    fn the_limiter_bounds_the_sum_rather_than_each_contribution() {
        // Four channels at unity into one leg: a raw sum of 4.0, far past full scale.
        let channels = vec![vec![1.0]; 4];
        let dests = [StereoMixDest::Left; 4];
        let gains = [Some(0.0); 4];

        let ceiling = dsp::db_to_linear(LIMIT_CEILING_DB);
        let limited = mix_to_stereo(&channels, 0, 1, &dests, &gains, Some(ceiling));
        assert!(
            limited[0][0] <= ceiling,
            "limited sum {} exceeded the ceiling {ceiling}",
            limited[0][0]
        );

        let raw = mix_to_stereo(&channels, 0, 1, &dests, &gains, None);
        assert!(approx(raw[0][0], 4.0), "unlimited sum should be the raw 4.0");
    }

    #[test]
    fn the_limiter_leaves_a_quiet_mix_essentially_untouched() {
        let channels = vec![vec![0.05]];
        let dests = [StereoMixDest::Left];
        let gains = [Some(0.0)];

        let ceiling = dsp::db_to_linear(LIMIT_CEILING_DB);
        let out = mix_to_stereo(&channels, 0, 1, &dests, &gains, Some(ceiling));
        // tanh is unity-gain for small signals, which is what makes enabling it safe by default.
        assert!((out[0][0] - 0.05).abs() < 1e-3);
    }

    #[test]
    fn a_channel_shorter_than_the_range_contributes_what_it_has_without_panicking() {
        let channels = vec![vec![1.0, 1.0, 1.0, 1.0], vec![1.0]];
        let dests = [StereoMixDest::Left, StereoMixDest::Left];
        let gains = [Some(0.0), Some(0.0)];

        let out = mix_to_stereo(&channels, 0, 4, &dests, &gains, None);
        assert_eq!(out[0].len(), 4);
        assert!(approx(out[0][0], 2.0));
        assert!(approx(out[0][3], 1.0));
    }

    #[test]
    fn a_channel_with_no_entry_in_dests_contributes_nothing() {
        let channels = vec![vec![1.0], vec![1.0]];
        // Only one destination for two channels.
        let out = mix_to_stereo(&channels, 0, 1, &[StereoMixDest::Left], &[Some(0.0)], None);
        assert!(approx(out[0][0], 1.0));
    }

    #[test]
    fn the_output_is_always_exactly_two_channels_of_the_requested_length() {
        let channels = vec![vec![0.0; 8]; 30];
        let dests = default_dests(30);
        let gains = vec![Some(DEFAULT_ATTENUATION_DB); 30];
        let out = mix_to_stereo(&channels, 2, 4, &dests, &gains, None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 4);
        assert_eq!(out[1].len(), 4);
    }
}
