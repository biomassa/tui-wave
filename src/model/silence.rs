//! Silence detection for Auto-Trim: where a take actually starts and stops, and where it
//! goes quiet in between.
//!
//! Everything here is pure analysis over already-resident samples — no I/O, no UI. The command
//! that acts on it is `commands::auto_trim`; the dialog that shows its numbers is
//! `Dialog::AutoTrim`.
//!
//! **Frames, not samples.** A single sample sits at zero on every zero crossing of every
//! waveform, so a per-sample threshold reports silence thousands of times a second in the
//! middle of a loud note. Detection therefore runs on short frames (`FRAME_MS`), each reduced
//! to one number, and only then compared against a threshold. 20ms is the usual speech/music
//! analysis frame: long enough to span a cycle of anything above 50Hz, short enough that the
//! boundary it resolves is inaudible against the padding applied afterwards.
//!
//! **The loudest channel decides.** A frame counts as sounding when *any* channel is above the
//! threshold, which is the only safe reduction for the multichannel case this app exists for:
//! averaging across 56 channels lets 48 dead inputs bury a live one, and a take would be
//! trimmed into its own first note. Same reasoning as `Document::snap_to_zero_crossing`'s cost
//! function, which takes the max across channels for the mirror-image reason.

use crate::model::dsp::{db_to_linear, linear_to_db};

/// Analysis frame length. See the module docs for why detection is framed at all.
pub const FRAME_MS: f64 = 20.0;

/// Percentile of frame levels taken as the noise floor by [`ThresholdMethod::NoiseFloor`].
///
/// The 10th rather than the minimum: a single anomalously quiet frame — a dropout, a gap
/// between two words, the moment a fader passed through zero — is not the noise floor, and
/// keying off the minimum would put the threshold far below anything the recording actually
/// sits at, so nothing would ever read as silent. A percentile asks "what level is this
/// recording quiet *at*", which is the question.
pub const NOISE_FLOOR_PERCENTILE: f64 = 0.10;

/// How far above the measured noise floor [`ThresholdMethod::NoiseFloor`] puts the threshold.
///
/// The floor itself is not a usable threshold: half the silent frames sit above it by
/// construction, so trimming at exactly the floor leaves most of the silence in place. 6dB is
/// the smallest margin that reliably clears ordinary floor variation without reaching into
/// quiet material.
pub const NOISE_FLOOR_MARGIN_DB: f32 = 6.0;

/// How far below the peak [`ThresholdMethod::PeakRelative`] puts the threshold — the figure
/// praatAudioTools' own `Auto-Trim_Silence` defaults to, kept so the two agree on the same file.
pub const PEAK_RELATIVE_DB: f32 = 35.0;

/// The threshold is never allowed closer to the peak than this.
///
/// A noise floor within 12dB of the peak means the recording has no quiet part to speak of —
/// a sustained tone, a limited master, a mis-set gain — and a threshold derived from it would
/// sit inside the material and cut it. [`auto_threshold_db`] clamps rather than refuses, and
/// [`plan`] then finds nothing to trim, which is the honest outcome: "there is no silence
/// here" rather than "here is a trim that eats your first note".
pub const MIN_PEAK_TO_THRESHOLD_DB: f32 = 12.0;

/// How the threshold is derived from the material when the user has not typed one.
///
/// Two methods rather than one because they answer different questions and disagree on exactly
/// the material where it matters. `PeakRelative` asks "how far below the loudest moment", which
/// is predictable and ignores the recording's own floor — on a hissy take with a loud peak it
/// puts the threshold well above the hiss and trims into the signal. `NoiseFloor` asks "what is
/// this recording's quiet level", which adapts, but needs the take to *have* a quiet part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThresholdMethod {
    /// A margin above the measured noise floor. The default: it is the one that adapts.
    #[default]
    NoiseFloor,
    /// A fixed distance below the peak, like praatAudioTools' `Threshold_dB_below_peak`.
    PeakRelative,
}

impl ThresholdMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::NoiseFloor => "Noise floor",
            Self::PeakRelative => "Peak-relative",
        }
    }

    /// Cycles for ←/→ in the dialog. Two variants, so both directions are the same step; kept
    /// as two methods anyway so the call sites read like every other cycler in the app.
    pub fn next(self) -> Self {
        match self {
            Self::NoiseFloor => Self::PeakRelative,
            Self::PeakRelative => Self::NoiseFloor,
        }
    }

    pub fn prev(self) -> Self {
        self.next()
    }
}

/// Frame count for a range of `len` samples at `sample_rate`.
pub fn frame_len(sample_rate: u32) -> usize {
    ((sample_rate as f64 * FRAME_MS / 1000.0) as usize).max(1)
}

/// One level per analysis frame, in **linear** amplitude: the loudest channel's RMS over that
/// frame. Linear rather than dB so a digitally-silent frame is 0.0 rather than a clamped
/// sentinel — `linear_to_db` floors at -120dB, and a floor percentile computed over clamped
/// values would report -120 for any file with a few truly silent frames.
///
/// `range` is the region to analyse, so a selection is measured on its own terms rather than
/// inheriting a threshold from audio outside it.
pub fn frame_levels(channels: &[Vec<f32>], range: (usize, usize), sample_rate: u32) -> Vec<f32> {
    let (start, end) = range;
    let frame = frame_len(sample_rate);
    let mut out = Vec::with_capacity((end.saturating_sub(start) / frame) + 1);
    let mut at = start;
    while at < end {
        let stop = (at + frame).min(end);
        let mut loudest = 0.0f32;
        for channel in channels {
            let Some(slice) = channel.get(at..stop) else { continue };
            if slice.is_empty() {
                continue;
            }
            let sum: f64 = slice.iter().map(|s| (*s as f64) * (*s as f64)).sum();
            let rms = (sum / slice.len() as f64).sqrt() as f32;
            loudest = loudest.max(rms);
        }
        out.push(loudest);
        at = stop;
    }
    out
}

/// The level below which a frame counts as silent, in dBFS, derived from the material.
///
/// `None` when there is nothing to measure. The result is always at least
/// [`MIN_PEAK_TO_THRESHOLD_DB`] below the loudest frame — see that constant for why clamping
/// beats refusing.
pub fn auto_threshold_db(levels: &[f32], method: ThresholdMethod) -> Option<f32> {
    if levels.is_empty() {
        return None;
    }
    let peak = levels.iter().copied().fold(0.0f32, f32::max);
    if peak <= 0.0 {
        return None; // digital silence throughout: nothing to find a threshold between
    }
    let peak_db = linear_to_db(peak);
    let raw = match method {
        ThresholdMethod::PeakRelative => peak_db - PEAK_RELATIVE_DB,
        ThresholdMethod::NoiseFloor => {
            let mut sorted = levels.to_vec();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let at = ((sorted.len() - 1) as f64 * NOISE_FLOOR_PERCENTILE).round() as usize;
            linear_to_db(sorted[at]) + NOISE_FLOOR_MARGIN_DB
        }
    };
    Some(raw.min(peak_db - MIN_PEAK_TO_THRESHOLD_DB))
}

/// What Auto-Trim proposes to do: the region to keep, and the quiet stretches inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilencePlan {
    /// First sounding sample, before padding is applied.
    pub start: usize,
    /// One past the last sounding sample, before padding.
    pub end: usize,
    /// Silent runs strictly inside `start..end`, as absolute sample ranges. Long enough to
    /// clear `min_silence`; the ends are not listed here, being what `start`/`end` express.
    pub gaps: Vec<(usize, usize)>,
}

/// Locate the sounding region and the internal gaps.
///
/// `min_silence_secs` is how long a quiet stretch must last to count. Without it every pause
/// between two words ends the take: the first frame below threshold after the last note would
/// be read as the end, and a phrase with any internal rest would be cut at the rest. It applies
/// to the internal gaps too, which is what keeps the gap markers down to stretches worth
/// looking at rather than one per syllable.
pub fn plan(
    levels: &[f32],
    range: (usize, usize),
    sample_rate: u32,
    threshold_db: f32,
    min_silence_secs: f64,
) -> Option<SilencePlan> {
    let (range_start, range_end) = range;
    if levels.is_empty() || range_start >= range_end {
        return None;
    }
    let frame = frame_len(sample_rate);
    let threshold = db_to_linear(threshold_db);
    // A frame index maps back to an absolute sample span; the last frame is short whenever the
    // range is not a whole number of frames, so its end is clamped rather than computed.
    let frame_span = |i: usize| {
        let s = range_start + i * frame;
        (s, (s + frame).min(range_end))
    };

    let sounding: Vec<bool> = levels.iter().map(|l| *l > threshold).collect();
    let first = sounding.iter().position(|s| *s)?;
    let last = sounding.iter().rposition(|s| *s)?;

    let start = frame_span(first).0;
    let end = frame_span(last).1;

    // Internal gaps: runs of silent frames strictly between the first and last sounding frame.
    // `min_frames` is a count rather than a duration so the comparison happens in the units the
    // scan already works in; rounding up means a gap must *clear* the requested duration.
    let min_frames = ((min_silence_secs * sample_rate as f64) / frame as f64).ceil() as usize;
    let min_frames = min_frames.max(1);
    let mut gaps = Vec::new();
    let mut run: Option<usize> = None;
    for i in first..=last {
        if sounding[i] {
            if let Some(run_start) = run.take() {
                if i - run_start >= min_frames {
                    gaps.push((frame_span(run_start).0, frame_span(i - 1).1));
                }
            }
        } else if run.is_none() {
            run = Some(i);
        }
    }

    Some(SilencePlan { start, end, gaps })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    /// A tone between two silences, with the tone's own zero crossings inside it — the case a
    /// per-sample threshold gets wrong and a framed one gets right.
    fn tone_between_silences(lead: usize, tone: usize, tail: usize) -> Vec<Vec<f32>> {
        let mut ch = vec![0.0f32; lead];
        for i in 0..tone {
            ch.push((i as f32 * 440.0 * std::f32::consts::TAU / SR as f32).sin() * 0.5);
        }
        ch.extend(std::iter::repeat_n(0.0, tail));
        vec![ch]
    }

    #[test]
    fn a_tone_between_silences_is_located() {
        let ch = tone_between_silences(SR as usize, SR as usize, SR as usize);
        let range = (0, ch[0].len());
        let levels = frame_levels(&ch, range, SR);
        let threshold = auto_threshold_db(&levels, ThresholdMethod::NoiseFloor).expect("threshold");
        let plan = plan(&levels, range, SR, threshold, 0.1).expect("plan");
        // Within one frame of the real boundaries, which is all a framed scan can promise.
        let frame = frame_len(SR);
        assert!(plan.start.abs_diff(SR as usize) <= frame, "start {}", plan.start);
        assert!(plan.end.abs_diff(2 * SR as usize) <= frame, "end {}", plan.end);
        assert!(plan.gaps.is_empty(), "no internal gap in a single tone: {:?}", plan.gaps);
    }

    /// The property the whole framing exists for: a loud waveform passes through zero
    /// constantly, and none of those crossings may read as silence.
    #[test]
    fn zero_crossings_inside_a_tone_are_not_gaps() {
        let ch = tone_between_silences(0, SR as usize, 0);
        let range = (0, ch[0].len());
        let levels = frame_levels(&ch, range, SR);
        let threshold = auto_threshold_db(&levels, ThresholdMethod::PeakRelative).expect("t");
        let plan = plan(&levels, range, SR, threshold, 0.05).expect("plan");
        assert!(plan.gaps.is_empty(), "a continuous tone has no gaps: {:?}", plan.gaps);
    }

    #[test]
    fn an_internal_gap_is_reported_but_a_short_one_is_not() {
        // tone | 0.5s silence | tone  — half a second clears a 0.1s minimum.
        let mut ch = tone_between_silences(0, SR as usize / 2, SR as usize / 2);
        let second = tone_between_silences(0, SR as usize / 2, 0);
        ch[0].extend_from_slice(&second[0]);
        let range = (0, ch[0].len());
        let levels = frame_levels(&ch, range, SR);
        let threshold = auto_threshold_db(&levels, ThresholdMethod::NoiseFloor).expect("t");

        let found = plan(&levels, range, SR, threshold, 0.1).expect("plan");
        assert_eq!(found.gaps.len(), 1, "the half-second gap: {:?}", found.gaps);

        // The same audio with a one-second minimum has no gap worth reporting.
        let coarse = plan(&levels, range, SR, threshold, 1.0).expect("plan");
        assert!(coarse.gaps.is_empty(), "0.5s does not clear a 1s minimum: {:?}", coarse.gaps);
    }

    /// The clamp in `auto_threshold_db`. A file with no quiet part must not produce a threshold
    /// that reaches into the material — see `MIN_PEAK_TO_THRESHOLD_DB`.
    #[test]
    fn a_file_with_no_silence_gets_a_threshold_below_its_own_peak() {
        let ch = tone_between_silences(0, SR as usize, 0);
        let levels = frame_levels(&ch, (0, ch[0].len()), SR);
        let peak_db = linear_to_db(levels.iter().copied().fold(0.0f32, f32::max));
        for method in [ThresholdMethod::NoiseFloor, ThresholdMethod::PeakRelative] {
            let t = auto_threshold_db(&levels, method).expect("threshold");
            assert!(
                t <= peak_db - MIN_PEAK_TO_THRESHOLD_DB + 0.001,
                "{method:?}: {t} is not {MIN_PEAK_TO_THRESHOLD_DB}dB below peak {peak_db}"
            );
        }
    }

    /// The two methods must actually differ on the material that motivated having both: a loud
    /// take over an audible noise floor. Peak-relative ignores the floor; noise-floor tracks it.
    #[test]
    fn the_two_methods_disagree_on_a_hissy_take() {
        let mut ch = vec![0.0f32; 0];
        // 1s of -30dBFS hiss, 1s of loud tone, 1s of hiss.
        let hiss = |n: usize, seed: &mut u32| -> Vec<f32> {
            (0..n)
                .map(|_| {
                    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((*seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.03
                })
                .collect()
        };
        let mut seed = 1u32;
        ch.extend(hiss(SR as usize, &mut seed));
        for i in 0..SR as usize {
            ch.push((i as f32 * 440.0 * std::f32::consts::TAU / SR as f32).sin() * 0.9);
        }
        ch.extend(hiss(SR as usize, &mut seed));
        let channels = vec![ch];
        let range = (0, channels[0].len());
        let levels = frame_levels(&channels, range, SR);

        let floor = auto_threshold_db(&levels, ThresholdMethod::NoiseFloor).expect("floor");
        let peak_rel = auto_threshold_db(&levels, ThresholdMethod::PeakRelative).expect("peak");
        assert!(
            floor > peak_rel,
            "noise-floor ({floor}) must sit above peak-relative ({peak_rel}) on a hissy take, \
             or it is not tracking the floor"
        );
        // And the noise-floor threshold must actually separate hiss from tone.
        let found = plan(&levels, range, SR, floor, 0.1).expect("plan");
        let frame = frame_len(SR) * 2;
        assert!(found.start.abs_diff(SR as usize) <= frame, "start {}", found.start);
        assert!(found.end.abs_diff(2 * SR as usize) <= frame, "end {}", found.end);
    }

    /// A silent channel alongside a live one must not drag the take's level down — the
    /// multichannel case this app exists for. See the module docs.
    #[test]
    fn a_dead_channel_does_not_bury_a_live_one() {
        let live = tone_between_silences(SR as usize, SR as usize, 0);
        let mut channels = live.clone();
        for _ in 0..8 {
            channels.push(vec![0.0f32; live[0].len()]);
        }
        let range = (0, channels[0].len());
        let mono = frame_levels(&live, range, SR);
        let wide = frame_levels(&channels, range, SR);
        assert_eq!(mono, wide, "eight dead channels must not change the measured levels");
    }

    #[test]
    fn digital_silence_throughout_has_no_threshold() {
        let channels = vec![vec![0.0f32; SR as usize]];
        let levels = frame_levels(&channels, (0, SR as usize), SR);
        assert_eq!(auto_threshold_db(&levels, ThresholdMethod::NoiseFloor), None);
        assert_eq!(auto_threshold_db(&levels, ThresholdMethod::PeakRelative), None);
    }

    /// A selection is analysed on its own terms — the threshold must not be inherited from
    /// audio outside it, which is what makes Auto-Trim on a selection mean anything.
    #[test]
    fn levels_cover_only_the_requested_range() {
        let channels = tone_between_silences(SR as usize, SR as usize, SR as usize);
        let whole = frame_levels(&channels, (0, channels[0].len()), SR);
        let tail_only = frame_levels(&channels, (2 * SR as usize, channels[0].len()), SR);
        assert!(whole.iter().any(|l| *l > 0.1), "the whole file has the tone in it");
        assert!(
            tail_only.iter().all(|l| *l < 1e-6),
            "the trailing second is silent on its own terms"
        );
    }
}
