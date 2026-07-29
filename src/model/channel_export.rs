//! Planning for File ▸ Export Channels: turning a per-channel mode list into the set of WAV
//! files to write. Pure logic, no ratatui and no I/O, so the pairing rules are unit-testable
//! without a terminal — the same constraint the rest of `src/model` is held to.

/// What one source channel becomes on export.
///
/// `PairWithNext` claims the channel *below* it, whose own mode then becomes inert — see
/// [`is_consumed`]. The pairing state is deliberately stored only on the upper channel and
/// derived for the lower one, so the two halves of a pair can never disagree; the same
/// reasoning that makes Head/Tail marks' roles an even/odd index property rather than a
/// stored field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelExportMode {
    Mono,
    Skip,
    PairWithNext,
}

/// One output file: the source channels it draws from (1 for mono, 2 for a pair) and the
/// suffix appended to the source file's stem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelExportFile {
    pub channels: Vec<usize>,
    pub suffix: String,
}

/// Digits needed for the highest 1-based channel number: 1..=9 → 1, 10..=99 → 2, 100+ → 3.
///
/// Used for both the file-name suffixes and the dialog's own `Ch` column, so the row number
/// and the file it produces always read identically. Zero-padding is what keeps a listing in
/// channel order — unpadded, a 30-channel export sorts `ch1`, `ch10`, `ch11`, … `ch2`.
pub fn digit_width(channel_count: usize) -> usize {
    channel_count.max(1).to_string().len()
}

/// True when `i` is the lower half of a pair, i.e. its own stored mode is inert.
///
/// Only the immediately preceding channel can claim it, and only if that channel is not
/// itself consumed — which is what stops a run of `PairWithNext` from chaining into
/// overlapping pairs.
pub fn is_consumed(modes: &[ChannelExportMode], i: usize) -> bool {
    i > 0 && modes[i - 1] == ChannelExportMode::PairWithNext && !is_consumed(modes, i - 1)
}

/// Whether `i` may be set to `PairWithNext`: it needs a channel below it, that channel must
/// not already be claimed, and `i` itself must be free.
fn can_pair(modes: &[ChannelExportMode], i: usize) -> bool {
    i + 1 < modes.len() && !is_consumed(modes, i) && !is_consumed(modes, i + 1)
}

/// The files a mode list produces, top to bottom. Consumed channels never appear on their own
/// — they are folded into the pair above them regardless of what their own mode says.
pub fn plan(modes: &[ChannelExportMode]) -> Vec<ChannelExportFile> {
    let w = digit_width(modes.len());
    let mut files = Vec::new();
    for (i, mode) in modes.iter().enumerate() {
        if is_consumed(modes, i) {
            continue;
        }
        match mode {
            ChannelExportMode::Skip => {}
            ChannelExportMode::Mono => files.push(ChannelExportFile {
                channels: vec![i],
                suffix: format!("ch{:0w$}", i + 1, w = w),
            }),
            ChannelExportMode::PairWithNext if i + 1 < modes.len() => {
                files.push(ChannelExportFile {
                    channels: vec![i, i + 1],
                    suffix: format!("ch{:0w$}-{:0w$}", i + 1, i + 2, w = w),
                })
            }
            // `PairWithNext` on the last channel has nothing to pair with. `cycle_mode` and
            // `toggle_pair` both refuse to create it, so this is only reachable from a
            // hand-built mode list; treat it as mono rather than dropping the channel.
            ChannelExportMode::PairWithNext => files.push(ChannelExportFile {
                channels: vec![i],
                suffix: format!("ch{:0w$}", i + 1, w = w),
            }),
        }
    }
    files
}

/// Space: pair `i` with the channel below it, or break the pair it already heads. A no-op on
/// a consumed row, on the last channel, or when the channel below is itself already paired.
pub fn toggle_pair(modes: &mut [ChannelExportMode], i: usize) {
    if i >= modes.len() || is_consumed(modes, i) {
        return;
    }
    if modes[i] == ChannelExportMode::PairWithNext {
        modes[i] = ChannelExportMode::Mono;
    } else if can_pair(modes, i) {
        modes[i] = ChannelExportMode::PairWithNext;
    }
}

/// ←/→: cycle Mono → Skip → PairWithNext → Mono. `PairWithNext` is skipped over whenever it
/// isn't legal here, so cycling can never land on a pair that doesn't exist. A no-op on a
/// consumed row — its mode has no effect, so changing it would be a silent nothing.
pub fn cycle_mode(modes: &mut [ChannelExportMode], i: usize, forward: bool) {
    if i >= modes.len() || is_consumed(modes, i) {
        return;
    }
    let order = [ChannelExportMode::Mono, ChannelExportMode::Skip, ChannelExportMode::PairWithNext];
    let at = order.iter().position(|m| *m == modes[i]).unwrap_or(0);
    let step = |k: usize| if forward { (k + 1) % order.len() } else { (k + order.len() - 1) % order.len() };
    let mut next = step(at);
    // At most one skip is ever needed (only `PairWithNext` can be illegal), but the loop
    // states the rule rather than relying on that.
    while order[next] == ChannelExportMode::PairWithNext && !can_pair(modes, i) {
        next = step(next);
    }
    modes[i] = order[next];
}

/// Opening state: stereo pairs from the top, with a trailing odd channel as its own mono
/// file. Pairs are the overwhelmingly common intent for a multichannel capture, and an odd
/// channel left over has nothing to pair with.
pub fn default_modes(channel_count: usize) -> Vec<ChannelExportMode> {
    (0..channel_count)
        .map(|i| {
            if i % 2 == 0 && i + 1 < channel_count {
                ChannelExportMode::PairWithNext
            } else {
                ChannelExportMode::Mono
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::ChannelExportMode::*;
    use super::*;

    fn suffixes(modes: &[ChannelExportMode]) -> Vec<String> {
        plan(modes).into_iter().map(|f| f.suffix).collect()
    }

    #[test]
    fn digit_width_follows_the_channel_count() {
        assert_eq!(digit_width(1), 1);
        assert_eq!(digit_width(6), 1);
        assert_eq!(digit_width(30), 2);
        assert_eq!(digit_width(99), 2);
        assert_eq!(digit_width(120), 3);
    }

    #[test]
    fn plan_folds_pairs_and_drops_skipped_channels() {
        let modes = [PairWithNext, Mono, Mono, Skip, PairWithNext, Mono];
        let files = plan(&modes);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0], ChannelExportFile { channels: vec![0, 1], suffix: "ch1-2".into() });
        assert_eq!(files[1], ChannelExportFile { channels: vec![2], suffix: "ch3".into() });
        assert_eq!(files[2], ChannelExportFile { channels: vec![4, 5], suffix: "ch5-6".into() });
    }

    /// A consumed channel's own stored mode must never reach `plan` — it belongs to the pair
    /// above it whatever it says locally.
    #[test]
    fn a_consumed_channels_own_mode_is_ignored() {
        for lower in [Mono, Skip, PairWithNext] {
            let modes = [PairWithNext, lower];
            assert_eq!(suffixes(&modes), vec!["ch1-2"], "lower mode {lower:?} leaked through");
        }
    }

    /// Consecutive `PairWithNext` must not chain into overlapping pairs: channel 1 is claimed
    /// by channel 0, so its own `PairWithNext` is inert and channel 2 stays free.
    #[test]
    fn pairing_does_not_chain_across_a_consumed_channel() {
        let modes = [PairWithNext, PairWithNext, Mono];
        assert!(!is_consumed(&modes, 0));
        assert!(is_consumed(&modes, 1));
        assert!(!is_consumed(&modes, 2));
        assert_eq!(suffixes(&modes), vec!["ch1-2", "ch3"]);
    }

    #[test]
    fn suffixes_are_zero_padded_to_the_channel_count() {
        let thirty = default_modes(30);
        let s = suffixes(&thirty);
        assert_eq!(s.first().unwrap(), "ch01-02");
        assert_eq!(s.last().unwrap(), "ch29-30");

        let six = default_modes(6);
        assert_eq!(suffixes(&six), vec!["ch1-2", "ch3-4", "ch5-6"], "no padding needed below ten");
    }

    #[test]
    fn sixty_four_mono_channels_get_sixty_four_distinct_suffixes() {
        let modes = vec![Mono; 64];
        let s = suffixes(&modes);
        assert_eq!(s.len(), 64);
        assert_eq!(s[0], "ch01");
        assert_eq!(s[63], "ch64");
        let unique: std::collections::HashSet<_> = s.iter().collect();
        assert_eq!(unique.len(), 64);
    }

    #[test]
    fn default_modes_pairs_from_the_top_and_leaves_an_odd_channel_mono() {
        assert_eq!(suffixes(&default_modes(6)), vec!["ch1-2", "ch3-4", "ch5-6"]);
        assert_eq!(suffixes(&default_modes(5)), vec!["ch1-2", "ch3-4", "ch5"]);
        assert_eq!(suffixes(&default_modes(1)), vec!["ch1"]);
        assert!(plan(&default_modes(0)).is_empty());
    }

    #[test]
    fn toggle_pair_is_a_no_op_on_the_last_channel_and_on_a_consumed_row() {
        let mut modes = vec![Mono, Mono];
        toggle_pair(&mut modes, 1);
        assert_eq!(modes, vec![Mono, Mono], "nothing below the last channel to pair with");

        let mut modes = vec![PairWithNext, Mono, Mono];
        toggle_pair(&mut modes, 1);
        assert_eq!(modes, vec![PairWithNext, Mono, Mono], "a consumed row can't be paired");
    }

    #[test]
    fn toggle_pair_breaks_an_existing_pair() {
        let mut modes = vec![PairWithNext, Mono];
        toggle_pair(&mut modes, 0);
        assert_eq!(modes, vec![Mono, Mono]);
        toggle_pair(&mut modes, 0);
        assert_eq!(modes, vec![PairWithNext, Mono]);
    }

    /// Cycling must never produce a `PairWithNext` that `plan` would have to treat as mono.
    /// The last channel is the only case where that can arise.
    #[test]
    fn cycling_never_lands_on_an_illegal_pair() {
        let mut modes = vec![Mono, Mono];
        for _ in 0..6 {
            cycle_mode(&mut modes, 1, true);
            assert_ne!(modes[1], PairWithNext, "the last channel can't head a pair");
        }
    }

    /// A channel below can't be claimed if it is already the *lower* half of a pair…
    #[test]
    fn a_channel_already_consumed_cannot_be_claimed_again() {
        let mut modes = vec![PairWithNext, Mono, Mono, Mono];
        toggle_pair(&mut modes, 1);
        assert_eq!(modes[1], Mono, "channel 1 is consumed by channel 0, so it heads nothing");
        assert_eq!(suffixes(&modes), vec!["ch1-2", "ch3", "ch4"]);
    }

    /// …but claiming a channel that *heads* a pair is legal, and dissolves that pair: it
    /// becomes consumed, its own mode goes inert, and the channel it held is freed. Stated as
    /// a test because it's the one case where one edit visibly changes two other rows.
    #[test]
    fn pairing_with_a_channel_that_heads_a_pair_dissolves_it() {
        let mut modes = vec![Mono, PairWithNext, Mono];
        assert_eq!(suffixes(&modes), vec!["ch1", "ch2-3"]);
        toggle_pair(&mut modes, 0);
        assert!(is_consumed(&modes, 1));
        assert_eq!(suffixes(&modes), vec!["ch1-2", "ch3"]);
    }

    #[test]
    fn cycling_walks_the_three_modes_both_ways() {
        let mut modes = vec![Mono, Mono, Mono];
        cycle_mode(&mut modes, 0, true);
        assert_eq!(modes[0], Skip);
        cycle_mode(&mut modes, 0, true);
        assert_eq!(modes[0], PairWithNext);
        cycle_mode(&mut modes, 0, true);
        assert_eq!(modes[0], Mono);
        cycle_mode(&mut modes, 0, false);
        assert_eq!(modes[0], PairWithNext);
    }

    #[test]
    fn cycling_a_consumed_row_changes_nothing() {
        let mut modes = vec![PairWithNext, Mono, Mono];
        cycle_mode(&mut modes, 1, true);
        assert_eq!(modes, vec![PairWithNext, Mono, Mono]);
    }
}
