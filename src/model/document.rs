use std::ops::Range;
use std::path::PathBuf;

/// Search window for zero-crossing snapping, in seconds on each side of the boundary — see
/// `Document::zero_crossing_window`.
const ZERO_CROSSING_WINDOW_SECS: f64 = 0.005;

/// Amplitude at or below which a boundary counts as click-free outright (-40 dBFS) — the
/// floor under `snap_to_zero_crossing`'s "as good as the best nearby" tolerance, so that a
/// window whose best candidate is essentially zero still accepts the ordinary crossings
/// around it instead of demanding one just as perfect.
const ZERO_CROSSING_NEAR_ZERO: f32 = 0.01;

use super::selection::Selection;

/// A named position on the timeline (a WAV `cue ` point with an `adtl`/`labl` label).
/// `position` is a sample frame index. Markers ride along with the audio: editing samples
/// before a marker shifts it so it stays anchored to the same audible point.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub position: usize,
    pub label: String,
}

/// An open audio file. Holds no UI/audio-device state — pure data, fully unit-testable
/// without a terminal or audio backend.
pub struct Document {
    /// Deinterleaved samples, one Vec per channel, normalized to f32 in [-1.0, 1.0].
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
    /// Bit depth from the source WAV header (16, 24, or 32). Always 32 for synthesized
    /// buffers (CopyToNew, MixToMono, etc.) since the working format is f32. Does not
    /// change when the file is saved at a different depth — it reflects the source.
    pub bits_per_sample: u16,
    pub selection: Option<Selection>,
    pub cursor: usize,
    pub dirty: bool,
    pub path: Option<PathBuf>,
    /// Timeline markers, kept sorted by position. Loaded from / saved to WAV cue chunks.
    pub markers: Vec<Marker>,
    /// Head/Tail marks for the CDP DISTMORE family, kept sorted by position. A **separate**
    /// system from `markers`, not a flavor of it: they mean something specific to CDP, they
    /// alternate in a way ordinary markers don't, and they persist to their own `.headstails`
    /// sidecar rather than the WAV's cue chunks (see `model::headstails`).
    ///
    /// **Flat and alternating** — even index = Head, odd index = Tail — which is CDP's own
    /// convention: *"the first mark is assumed to be at a Head segment"*. A Head is typically
    /// a consonant onset and its Tail the vowel continuation; for melodic material, note
    /// starts; for drums, stroke starts. Position-only, because both the role and the label
    /// (`H1`/`T1`/`H2`/`T2`…) fall out of the index, leaving nothing to store per mark.
    ///
    /// DISTMORE needs at least two complete pairs — see [`Document::head_tail_pairs`].
    pub head_tail_marks: Vec<usize>,
    /// Raw BWF `bext` chunk bytes, preserved verbatim across a load→save round-trip so
    /// editing a broadcast WAV doesn't strip its metadata. `None` for plain WAVs.
    pub bext: Option<Vec<u8>>,
}

impl Default for Document {
    fn default() -> Self {
        Document {
            channels: Vec::new(),
            sample_rate: 44100,
            bits_per_sample: 32,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            head_tail_marks: Vec::new(),
            bext: None,
        }
    }
}

/// Which half of a Head/Tail pair a mark is, derived from its index in
/// `Document.head_tail_marks` — see that field's doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadTailRole {
    Head,
    Tail,
}

impl HeadTailRole {
    /// The role of the mark at `index`, per CDP's "first mark is a Head" convention.
    pub fn at(index: usize) -> Self {
        if index % 2 == 0 {
            HeadTailRole::Head
        } else {
            HeadTailRole::Tail
        }
    }

    /// The single-letter prefix used in the on-screen label (`H3`, `T3`).
    pub fn letter(self) -> char {
        match self {
            HeadTailRole::Head => 'H',
            HeadTailRole::Tail => 'T',
        }
    }
}

/// The on-screen label for the head/tail mark at `index`: `H1`, `T1`, `H2`, `T2`, … Both
/// halves of a pair carry the same number, so a glance at the waveform shows which Head goes
/// with which Tail.
pub fn head_tail_label(index: usize) -> String {
    format!("{}{}", HeadTailRole::at(index).letter(), index / 2 + 1)
}

impl Document {
    /// How many *complete* Head/Tail pairs the mark list holds. A trailing unpaired Head
    /// doesn't count — CDP reads the list strictly in pairs.
    pub fn head_tail_pairs(&self) -> usize {
        self.head_tail_marks.len() / 2
    }

    /// Collapses head/tail marks that have landed on the same sample down to one, keeping the
    /// list sorted. Duplicates are never valid: two marks on one sample describe a zero-length
    /// segment *and*, since the Head-or-Tail role is derived from list index, flip the role of
    /// every mark after them.
    ///
    /// A separate step rather than something `remove_range` does for itself, because that
    /// primitive must preserve entry count and order for `commands::cdp`'s positional restore
    /// — see the comment there. Every command that moves marks calls this once it has finished.
    pub fn dedup_head_tail_marks(&mut self) {
        self.head_tail_marks.sort_unstable();
        self.head_tail_marks.dedup();
    }

    /// Inserts a head/tail mark at `position`, keeping the list sorted, and returns its new
    /// index. A duplicate position is rejected (returns `None`) rather than inserted: two
    /// marks at the same sample would make a zero-length segment, and since roles are derived
    /// from index, it would also silently flip the role of every later mark.
    pub fn insert_head_tail_mark(&mut self, position: usize) -> Option<usize> {
        match self.head_tail_marks.binary_search(&position) {
            Ok(_) => None,
            Err(index) => {
                self.head_tail_marks.insert(index, position);
                Some(index)
            }
        }
    }

    /// Adjusts `pos` to the nearby point of **least discontinuity** — the sample at which
    /// every channel is simultaneously closest to zero — so a cut, paste or loop boundary
    /// there produces no click.
    ///
    /// Scored, not matched: each candidate's cost is the loudest channel at that sample
    /// (`max |ch[i]|`), and the smallest cost in the window wins, ties going to the candidate
    /// nearest `pos`. The previous rule instead required *every* channel to be near-zero or
    /// sign-changing at the exact same index and to agree on rounding to `i` or `i+1`, which
    /// two channels even three samples out of phase never satisfy — so on any real stereo
    /// file the search found no valid candidate and returned `pos` untouched, i.e. zero-snap
    /// silently did nothing at all (user report: "i can't see it working", with a screenshot
    /// of a stereo selection whose edges sat mid-slope). Measured on a 220Hz tone at 96kHz:
    /// mono snapped 5000 to 5019, stereo returned 5000 unchanged.
    ///
    /// A candidate always exists, so this always lands somewhere; in a loud passage that is
    /// the quietest nearby sample rather than a true crossing, which is still the least
    /// clicking place to cut in that window.
    ///
    /// Among *equally serviceable* candidates the nearest one wins, which is what keeps the
    /// snap from wandering: at 96kHz a 220Hz tone crosses zero roughly every 220 samples, so
    /// a ±480-sample window holds several crossings that are all click-free, and taking the
    /// numerically smallest sample among them moved the edge by 200 samples for no audible
    /// gain. "Serviceable" is twice the window's best cost, floored at
    /// [`ZERO_CROSSING_NEAR_ZERO`] so a near-perfect best doesn't make the tolerance
    /// vanishingly tight.
    pub fn snap_to_zero_crossing(&self, pos: usize) -> usize {
        if self.channels.is_empty() || self.channels[0].is_empty()
            || pos >= self.channels[0].len()
        {
            return pos;
        }
        let window = self.zero_crossing_window();
        let search_start = pos.saturating_sub(window);
        let search_end = (pos + window).min(self.channels[0].len());

        // The worst channel at a sample: a boundary is only click-free if *all* of them are
        // quiet there, so the loudest one is what decides.
        let cost = |i: usize| -> f32 {
            self.channels
                .iter()
                .map(|ch| ch.get(i).map_or(f32::INFINITY, |s| s.abs()))
                .fold(0.0f32, f32::max)
        };

        let best_cost = (search_start..search_end).map(cost).fold(f32::INFINITY, f32::min);
        if !best_cost.is_finite() {
            return pos;
        }
        let tolerance = (best_cost * 2.0).max(ZERO_CROSSING_NEAR_ZERO);
        (search_start..search_end)
            .filter(|&i| cost(i) <= tolerance)
            .min_by_key(|&i| i.abs_diff(pos))
            .unwrap_or(pos)
    }

    /// How far either side of a boundary [`snap_to_zero_crossing`] looks, in samples.
    ///
    /// Defined as a span of *time* rather than a sample count so it covers the same amount of
    /// audio at any rate: a fixed 256 samples was 5.8ms at 44.1kHz but only 2.7ms at 96kHz,
    /// narrow enough there to miss the crossings of anything low-frequency.
    fn zero_crossing_window(&self) -> usize {
        ((self.sample_rate as f64 * ZERO_CROSSING_WINDOW_SECS) as usize).max(1)
    }

    /// Snap both ends of a normalized (start <= end) range to zero crossings.
    pub fn snap_range_to_zero_crossing(&self, start: usize, end: usize) -> (usize, usize) {
        (self.snap_to_zero_crossing(start), self.snap_to_zero_crossing(end))
    }
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn len_samples(&self) -> usize {
        self.channels.first().map(|c| c.len()).unwrap_or(0)
    }

    /// Finds the next transient at or after `from` and returns the position to stop right
    /// before it, or `None` if none is found before end-of-file.
    ///
    /// A transient (in DSP terms: the percussive onset of a sound — a drum hit, a plucked
    /// string's pluck, a plosive consonant) is characterized by a sudden, fast rise in
    /// amplitude relative to whatever level came just before it. This is a simplified
    /// two-envelope onset detector, the same family of technique as a hardware "transient
    /// designer": the signal is divided into `TRANSIENT_FRAME_MS` analysis frames, each
    /// frame's RMS level (the loudest channel's) is compared in dB against a slow-moving
    /// background average of recent frames (an exponential moving average with a
    /// `TRANSIENT_BACKGROUND_TIME_CONSTANT_MS` time constant); the first frame whose level
    /// exceeds the background by `threshold_db` or more is the transient's onset, and its
    /// first sample is "right before" the transient — the finest precision a frame-based
    /// scan can offer without the cost of a per-sample analysis on long files.
    pub fn find_next_rising_edge(&self, from: usize, threshold_db: f32) -> Option<usize> {
        const TRANSIENT_FRAME_MS: f64 = 10.0;
        const TRANSIENT_BACKGROUND_TIME_CONSTANT_MS: f64 = 150.0;
        const EPS: f32 = 1e-6;
        // A frame quieter than this never counts as a transient's onset, no matter how
        // large its *relative* jump over an even quieter background is. Without this gate,
        // a faint puff of pre-roll noise rising out of near-digital-silence registers as a
        // huge nominal dB jump (since the background started near zero) and gets flagged
        // long before the actual, audibly loud transient — stopping well short of it.
        const TRANSIENT_MIN_LEVEL: f32 = 0.01; // ~ -40dBFS

        let total = self.len_samples();
        if self.channels.is_empty() || from >= total {
            return None;
        }
        let frame_len = ((self.sample_rate as f64 * TRANSIENT_FRAME_MS / 1000.0).round() as usize).max(1);
        let alpha = (TRANSIENT_FRAME_MS / TRANSIENT_BACKGROUND_TIME_CONSTANT_MS).clamp(0.0, 1.0) as f32;

        let mut pos = from;
        let mut background: Option<f32> = None;
        while pos < total {
            let end = (pos + frame_len).min(total);
            let frame_level = self.frame_rms(pos, end).max(EPS);
            match background {
                None => background = Some(frame_level),
                Some(bg) => {
                    let rise_db = 20.0 * (frame_level / bg.max(EPS)).log10();
                    if rise_db >= threshold_db && frame_level >= TRANSIENT_MIN_LEVEL {
                        return Some(pos);
                    }
                    background = Some(bg * (1.0 - alpha) + frame_level * alpha);
                }
            }
            pos = end;
        }
        None
    }

    /// Finds every transient in the file by repeatedly applying `find_next_rising_edge`
    /// from each detected position onward — each call starts its background average fresh
    /// at the position it's given, so resuming from a found edge correctly looks for the
    /// *next* rise rather than re-triggering on the one just found. Used by "Auto-Insert
    /// Markers at Transients".
    pub fn find_all_rising_edges(&self, threshold_db: f32) -> Vec<usize> {
        const TRANSIENT_FRAME_MS: f64 = 10.0;
        const TRANSIENT_MIN_LEVEL: f32 = 0.01;
        const EPS: f32 = 1e-6;
        let mut edges = Vec::new();

        // `find_next_rising_edge` can never return position 0 because it unconditionally
        // uses the first frame to seed the background level before comparing anything.
        // For a file that opens with a transient (the signal peaks at frame 0 then decays),
        // detect this by comparing frame 0 against frame 1: a real transient onset has a
        // significant drop from one frame to the next; constant-level content doesn't.
        if !self.channels.is_empty() && self.len_samples() > 0 {
            let frame_len = ((self.sample_rate as f64 * TRANSIENT_FRAME_MS / 1000.0).round() as usize).max(1);
            let total = self.len_samples();
            if total >= 2 * frame_len {
                let frame0 = self.frame_rms(0, frame_len).max(EPS);
                let frame1 = self.frame_rms(frame_len, (2 * frame_len).min(total)).max(EPS);
                let decay_db = 20.0 * (frame0 / frame1).log10();
                if frame0 >= TRANSIENT_MIN_LEVEL && decay_db >= threshold_db {
                    edges.push(0);
                }
            }
        }

        let mut pos = 0;
        while let Some(edge) = self.find_next_rising_edge(pos, threshold_db) {
            edges.push(edge);
            pos = edge;
        }
        edges
    }

    /// Finds the transient immediately before `before` (searching backward), for "Previous
    /// Transient" navigation — the same definition of "transient" as `find_next_rising_edge`,
    /// just picking the closest one behind the cursor rather than ahead of it.
    pub fn find_previous_rising_edge(&self, before: usize, threshold_db: f32) -> Option<usize> {
        self.find_all_rising_edges(threshold_db).into_iter().filter(|&pos| pos < before).max()
    }

    /// RMS amplitude within `[start, end)`, taking the loudest channel — a transient in any
    /// one channel should be found, not averaged away by quieter channels.
    fn frame_rms(&self, start: usize, end: usize) -> f32 {
        self.channels
            .iter()
            .map(|channel| {
                let end = end.min(channel.len());
                if start >= end {
                    return 0.0;
                }
                let slice = &channel[start..end];
                let sum_sq: f32 = slice.iter().map(|&s| s * s).sum();
                (sum_sq / slice.len() as f32).sqrt()
            })
            .fold(0.0f32, f32::max)
    }

    /// Non-destructive copy of `range` across all channels, clamped to bounds.
    pub fn slice(&self, range: Range<usize>) -> Vec<Vec<f32>> {
        self.channels
            .iter()
            .map(|channel| {
                let end = range.end.min(channel.len());
                let start = range.start.min(end);
                channel[start..end].to_vec()
            })
            .collect()
    }

    /// Removes `range` from every channel in place and returns the removed samples (one
    /// Vec per channel), so the caller can store them for undo. Markers shift with the cut:
    /// those after the range move left, those inside it collapse to the cut point.
    pub fn remove_range(&mut self, range: Range<usize>) -> Vec<Vec<f32>> {
        let len = self.len_samples();
        let start = range.start.min(len);
        let end = range.end.min(len);
        let removed = end.saturating_sub(start);
        let out = self
            .channels
            .iter_mut()
            .map(|channel| {
                let end = range.end.min(channel.len());
                let start = range.start.min(end);
                channel.splice(start..end, std::iter::empty()).collect()
            })
            .collect();
        for m in &mut self.markers {
            if m.position >= end {
                m.position -= removed;
            } else if m.position > start {
                m.position = start;
            }
        }
        // Head/tail marks shift identically — they are anchored to audio just as ordinary
        // markers are, and a DISTMORE marklist that drifted after an edit would cut the wrong
        // segments.
        //
        // Several marks inside the cut all collapse onto the cut point, leaving duplicates.
        // They are deliberately **not** deduped here: like the `markers` loop above, this
        // primitive never reorders or removes an entry, and `commands::cdp`'s splice relies
        // on that — it restores in-range marks by zipping the post-edit list positionally
        // against a pre-edit snapshot, which silently drops entries the moment the two
        // lengths diverge. Deduping is the caller's job, once it has finished moving things
        // around: see `Document::dedup_head_tail_marks`.
        for m in &mut self.head_tail_marks {
            if *m >= end {
                *m -= removed;
            } else if *m > start {
                *m = start;
            }
        }
        out
    }

    /// Inserts `data` (one Vec per channel) at `at` in every channel of `self`, adapting a
    /// mono/stereo `data.len()` mismatch rather than leaving channels desynced in length:
    /// a mono `data` inserted into a stereo document duplicates its one channel into both
    /// (so the inserted audio is audible on both, not silent on one); a stereo/multi-channel
    /// `data` inserted into a document with fewer channels drops the extra source channels.
    /// Every one of `self.channels` always receives *something* the same length as the
    /// others, which is the actual invariant this exists to protect — `DocumentSource`
    /// (`audio/source.rs`) derives its total-frame count from channel 0 alone but indexes
    /// every channel with it, so a caller that left one channel shorter than another (the
    /// original `.zip(data)` here silently dropped the insert for any channel beyond
    /// `data.len()`) would panic on playback the moment `frame_index` ran past that
    /// channel's real length — reachable in practice via a mono CDP synthesis result
    /// (`commands::cdp`) spliced into a stereo document. Markers at or after `at` shift
    /// right by the inserted length so they stay anchored to the same audio.
    pub fn insert_range(&mut self, at: usize, data: Vec<Vec<f32>>) {
        let Some(first) = data.first() else { return };
        let count = first.len();
        for (i, channel) in self.channels.iter_mut().enumerate() {
            let source = data.get(i).unwrap_or(first);
            let at = at.min(channel.len());
            channel.splice(at..at, source.iter().copied());
        }
        for m in &mut self.markers {
            if m.position >= at {
                m.position += count;
            }
        }
        for m in &mut self.head_tail_marks {
            if *m >= at {
                *m += count;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(samples: Vec<f32>) -> Document {
        Document {
            head_tail_marks: Vec::new(),
            channels: vec![samples],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        }
    }

    /// Builds a signal of constant-amplitude segments, each `frames` analysis frames long
    /// (frame = 441 samples at the test's 44100 sample rate / 10ms), so transient tests can
    /// reason in whole frames instead of raw sample counts.
    fn segments(segments: &[(f32, usize)]) -> Vec<f32> {
        const FRAME_LEN: usize = 441;
        segments
            .iter()
            .flat_map(|&(level, frames)| std::iter::repeat(level).take(frames * FRAME_LEN))
            .collect()
    }

    #[test]
    fn find_next_rising_edge_detects_a_loud_transient_after_quiet() {
        // 20 quiet frames, then a sudden jump to 0.5 — a ~34dB rise, well past the 6dB
        // default threshold.
        let d = doc(segments(&[(0.01, 20), (0.5, 30)]));
        let pos = d.find_next_rising_edge(0, 6.0).expect("should find the transient");
        assert_eq!(pos, 20 * 441, "should stop right at the start of the loud frame");
    }

    #[test]
    fn find_next_rising_edge_ignores_the_starting_level_and_finds_a_later_rise() {
        // Searching from the start of the medium-loud section: no transient is reported
        // for entering it (there's no prior baseline to compare against at the search
        // start), but the later jump to very-loud (a ~12dB rise) is found.
        let d = doc(segments(&[(0.01, 20), (0.5, 20), (2.0, 30)]));
        let start_of_medium = 20 * 441;
        let pos = d.find_next_rising_edge(start_of_medium, 6.0).expect("should find the second rise");
        assert_eq!(pos, 40 * 441);
    }

    #[test]
    fn find_next_rising_edge_returns_none_for_constant_level() {
        let d = doc(segments(&[(0.3, 50)]));
        assert_eq!(d.find_next_rising_edge(0, 6.0), None);
    }

    #[test]
    fn find_next_rising_edge_respects_the_threshold() {
        // A ~6dB rise (0.5 -> ~1.0) should clear a 3dB threshold but not a 9dB one.
        let d = doc(segments(&[(0.5, 20), (1.0, 20)]));
        assert!(d.find_next_rising_edge(0, 3.0).is_some());
        assert_eq!(d.find_next_rising_edge(0, 9.0), None);
    }

    #[test]
    fn find_next_rising_edge_skips_quiet_pre_roll_noise_and_finds_the_real_transient() {
        // The bug this guards against: starting from near-digital-silence, a faint puff of
        // pre-roll noise (well below -40dBFS) is a *huge* relative jump from near-zero, but
        // must not be flagged — only the actual loud transient further along should be.
        let d = doc(segments(&[(0.0, 5), (0.005, 30), (0.3, 20)]));
        let pos = d.find_next_rising_edge(0, 6.0).expect("should find the real transient");
        assert_eq!(pos, 35 * 441, "should land on the loud transient, not the quiet pre-roll");
    }

    #[test]
    fn find_previous_rising_edge_finds_the_closest_one_behind() {
        let d = doc(segments(&[(0.01, 20), (0.5, 20), (5.0, 20)]));
        // From inside the loudest segment, both earlier transients (frame 20 and frame 40)
        // qualify as "behind" — the closer one (frame 40, the 0.5 -> 5.0 rise) must win,
        // not the more distant frame-20 one.
        assert_eq!(d.find_previous_rising_edge(45 * 441, 6.0), Some(40 * 441));
        // From inside the medium segment, only the frame-20 transient is behind.
        assert_eq!(d.find_previous_rising_edge(25 * 441, 6.0), Some(20 * 441));
        // From right at the first transient itself, there's nothing earlier.
        assert_eq!(d.find_previous_rising_edge(20 * 441, 6.0), None);
    }

    #[test]
    fn find_next_rising_edge_from_past_end_is_none() {
        let d = doc(segments(&[(0.5, 5)]));
        assert_eq!(d.find_next_rising_edge(d.len_samples(), 6.0), None);
    }

    #[test]
    fn find_all_rising_edges_finds_every_transient_in_order() {
        // Three distinct rises: quiet -> medium (20), medium -> loud (40), loud -> very
        // loud (60), each comfortably above the 6dB default threshold.
        let d = doc(segments(&[(0.01, 20), (0.1, 20), (1.0, 20), (8.0, 20)]));
        let edges = d.find_all_rising_edges(6.0);
        assert_eq!(edges, vec![20 * 441, 40 * 441, 60 * 441]);
    }

    #[test]
    fn find_all_rising_edges_is_empty_for_constant_level() {
        let d = doc(segments(&[(0.3, 50)]));
        assert!(d.find_all_rising_edges(6.0).is_empty());
    }

    #[test]
    fn find_all_rising_edges_inserts_position_zero_for_file_that_opens_with_transient() {
        // A loud hit at sample 0 that decays into the second frame by more than the
        // threshold — the algorithm's normal scan can't catch this because the first
        // frame is unconditionally used as the background seed, never compared.
        let d = doc(segments(&[(0.5, 1), (0.05, 49)])); // ~14dB decay in frame 1
        let edges = d.find_all_rising_edges(6.0);
        assert_eq!(edges[0], 0, "transient at position 0 must produce a marker at 0");
    }

    #[test]
    fn slice_is_non_destructive() {
        let document = doc(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        let s = document.slice(1..3);
        assert_eq!(s, vec![vec![2.0, 3.0]]);
        assert_eq!(document.len_samples(), 5);
    }

    #[test]
    fn snap_to_zero_crossing_finds_sign_change() {
        let d = doc(vec![0.5, 0.3, 0.1, -0.1, -0.3, -0.5]);
        // The crossing lies between samples 2 and 3, which are equally close to zero (±0.1)
        // and therefore equally click-free. The nearer of the two to the requested position
        // wins, so asking at 2 stays at 2 and asking at 3 stays at 3 — either is "the"
        // crossing, and moving the boundary a sample for no gain is what the distance
        // tie-break exists to prevent.
        assert_eq!(d.snap_to_zero_crossing(2), 2);
        assert_eq!(d.snap_to_zero_crossing(3), 3);
        // From further out, it lands on the near side of the crossing.
        assert_eq!(d.snap_to_zero_crossing(0), 2);
        assert_eq!(d.snap_to_zero_crossing(5), 3);
    }

    /// The bug this scoring replaced: `channel_agreement` required every channel to be
    /// near-zero or sign-changing at the *same* sample index and to agree on rounding, which
    /// two channels even a few samples out of phase never satisfy — so on a real stereo file
    /// the snap silently did nothing at all (user report: "i can't see it working").
    #[test]
    fn snap_to_zero_crossing_works_on_stereo_channels_that_are_out_of_phase() {
        let sr = 96_000.0f32;
        let tone = |phase: f32| -> Vec<f32> {
            (0..20_000)
                .map(|i| 0.4 * (2.0 * std::f32::consts::PI * 220.0 * (i as f32 + phase) / sr).sin())
                .collect()
        };
        let stereo = Document {
            channels: vec![tone(0.0), tone(3.0)],
            sample_rate: 96_000,
            ..Default::default()
        };
        let mono = Document { channels: vec![tone(0.0)], sample_rate: 96_000, ..Default::default() };

        for pos in [5_000usize, 5_100, 5_200, 5_300] {
            let snapped = stereo.snap_to_zero_crossing(pos);
            assert_ne!(snapped, pos, "stereo snapping must actually move the boundary at {pos}");
            // And it lands essentially where the mono file's own snap does — within a few
            // samples, since that is all the two channels are apart.
            assert!(
                snapped.abs_diff(mono.snap_to_zero_crossing(pos)) <= 8,
                "stereo snapped to {snapped}, mono to {}",
                mono.snap_to_zero_crossing(pos)
            );
            // Both channels really are quiet there — that is the whole point.
            for ch in &stereo.channels {
                assert!(ch[snapped].abs() < 0.05, "channel is at {} at the snapped point", ch[snapped]);
            }
        }
    }

    /// The snap must not wander: several crossings inside the window are all click-free, so
    /// the nearest one wins rather than whichever sample is numerically smallest.
    #[test]
    fn snap_to_zero_crossing_prefers_the_nearest_serviceable_point() {
        let sr = 96_000.0f32;
        let samples: Vec<f32> = (0..20_000)
            .map(|i| 0.4 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / sr).sin())
            .collect();
        let d = Document { channels: vec![samples], sample_rate: 96_000, ..Default::default() };
        // A 220Hz tone at 96kHz crosses zero every ~218 samples, so a ±480-sample window
        // holds four of them; the snap must pick one within about half a period.
        for pos in [5_000usize, 5_100, 5_200, 5_300] {
            let snapped = d.snap_to_zero_crossing(pos);
            assert!(
                snapped.abs_diff(pos) < 120,
                "snapped from {pos} to {snapped} — further than the nearest crossing"
            );
        }
    }

    #[test]
    fn snap_to_zero_crossing_stays_at_zero() {
        let d = doc(vec![0.5, 0.0, -0.3, -0.5]);
        assert_eq!(d.snap_to_zero_crossing(1), 1);
    }

    #[test]
    fn snap_range_to_zero_crossing_adjusts_both_ends() {
        // A sine wave that crosses zero every ~2205 samples at 44.1kHz (10 Hz).
        let samples: Vec<f32> = (0..5000).map(|i| ((i as f32) * 0.001).sin()).collect();
        let d = doc(samples);
        // Pick a range that starts/ends away from zero crossings.
        let (snapped_start, snapped_end) = d.snap_range_to_zero_crossing(100, 4900);
        // The snapped range should still produce a valid non-empty range.
        assert!(snapped_start < snapped_end);
        assert!(snapped_end <= 5000);
    }

    #[test]
    fn remove_range_shifts_markers() {
        let mut document = doc(vec![0.0; 100]);
        document.markers = vec![
            Marker { position: 10, label: "a".into() },  // before cut
            Marker { position: 30, label: "b".into() },  // inside cut [20,40)
            Marker { position: 60, label: "c".into() },  // after cut
        ];
        document.remove_range(20..40); // removes 20 samples
        assert_eq!(document.markers[0].position, 10); // unchanged
        assert_eq!(document.markers[1].position, 20); // collapsed to cut point
        assert_eq!(document.markers[2].position, 40); // shifted left by 20
    }

    #[test]
    fn insert_range_shifts_markers() {
        let mut document = doc(vec![0.0; 50]);
        document.markers = vec![
            Marker { position: 10, label: "a".into() },
            Marker { position: 30, label: "b".into() },
        ];
        document.insert_range(20, vec![vec![0.0; 5]]); // insert 5 at 20
        assert_eq!(document.markers[0].position, 10); // before insert, unchanged
        assert_eq!(document.markers[1].position, 35); // after insert, +5
    }

    /// Head/tail marks are anchored to audio exactly as ordinary markers are — a DISTMORE
    /// marklist that drifted after a cut would slice the wrong segments. Mirrors
    /// `remove_range_shifts_markers` position for position.
    #[test]
    fn remove_range_shifts_head_tail_marks() {
        let mut document = doc(vec![0.0; 100]);
        document.head_tail_marks = vec![10, 30, 60, 80];
        document.remove_range(20..40); // removes 20 samples
        assert_eq!(document.head_tail_marks, vec![10, 20, 40, 60]);
    }

    /// Several marks inside one cut all collapse onto the cut point. `remove_range` itself
    /// leaves them stacked — it must preserve entry count and order for `commands::cdp`'s
    /// positional restore — and `dedup_head_tail_marks` is what collapses them, which is what
    /// every command calls once it has finished moving marks around.
    #[test]
    fn marks_collapsing_onto_one_cut_point_stack_until_deduped() {
        let mut document = doc(vec![0.0; 100]);
        document.head_tail_marks = vec![10, 25, 30, 35, 60];
        document.remove_range(20..40);
        assert_eq!(
            document.head_tail_marks,
            vec![10, 20, 20, 20, 40],
            "the primitive preserves one entry per original mark"
        );

        document.dedup_head_tail_marks();
        assert_eq!(document.head_tail_marks, vec![10, 20, 40]);
        assert_eq!(document.head_tail_pairs(), 1, "one complete pair plus a spare Head");
    }

    #[test]
    fn insert_range_shifts_head_tail_marks() {
        let mut document = doc(vec![0.0; 50]);
        document.head_tail_marks = vec![10, 30];
        document.insert_range(20, vec![vec![0.0; 5]]);
        assert_eq!(document.head_tail_marks, vec![10, 35]);
    }

    #[test]
    fn head_tail_roles_and_labels_alternate_from_a_head() {
        assert_eq!(HeadTailRole::at(0), HeadTailRole::Head);
        assert_eq!(HeadTailRole::at(1), HeadTailRole::Tail);
        assert_eq!(HeadTailRole::at(2), HeadTailRole::Head);
        assert_eq!(head_tail_label(0), "H1");
        assert_eq!(head_tail_label(1), "T1", "both halves of a pair share a number");
        assert_eq!(head_tail_label(2), "H2");
        assert_eq!(head_tail_label(3), "T2");
    }

    #[test]
    fn inserting_head_tail_marks_keeps_them_sorted_and_rejects_duplicates() {
        let mut document = doc(vec![0.0; 100]);
        assert_eq!(document.insert_head_tail_mark(50), Some(0));
        assert_eq!(document.insert_head_tail_mark(20), Some(0), "sorted, so 20 lands first");
        assert_eq!(document.insert_head_tail_mark(80), Some(2));
        assert_eq!(document.head_tail_marks, vec![20, 50, 80]);
        assert_eq!(document.insert_head_tail_mark(50), None, "a duplicate position is refused");
        assert_eq!(document.head_tail_marks, vec![20, 50, 80]);
    }

    /// A trailing unpaired Head doesn't count — CDP reads the marklist strictly in pairs, and
    /// DISTMORE's floor of "at least two pairs" is checked against this.
    #[test]
    fn an_odd_mark_count_reports_only_the_complete_pairs() {
        let mut document = doc(vec![0.0; 100]);
        document.head_tail_marks = vec![10, 20, 30];
        assert_eq!(document.head_tail_pairs(), 1);
        document.head_tail_marks.push(40);
        assert_eq!(document.head_tail_pairs(), 2);
    }

    #[test]
    fn remove_then_insert_round_trips() {
        let mut document = doc(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        let removed = document.remove_range(1..3);
        assert_eq!(document.channels, vec![vec![1.0, 4.0, 5.0]]);
        document.insert_range(1, removed);
        assert_eq!(document.channels, vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
    }

    /// Regression test for a real crash: inserting mono `data` into a stereo document used
    /// to leave the second channel untouched (`self.channels.iter_mut().zip(data)` silently
    /// drops any `self.channels` entry beyond `data.len()`), desyncing channel lengths —
    /// `DocumentSource` (`audio/source.rs`) derives its total-frame count from channel 0
    /// alone but indexes every channel with it, so playing a document in this state panics
    /// on an out-of-bounds index the moment playback runs past the shorter channel's real
    /// length. Reachable via a mono CDP synthesis result spliced into a stereo document
    /// (`commands::cdp`). The fix duplicates the one source channel into every destination
    /// channel instead, so both stay the same length and the inserted audio is audible on
    /// both rather than silent on one.
    #[test]
    fn inserting_mono_data_into_a_stereo_document_duplicates_it_into_both_channels() {
        let mut document = Document {
            channels: vec![vec![1.0, 2.0, 3.0], vec![10.0, 20.0, 30.0]],
            sample_rate: 44100,
            ..doc(vec![])
        };
        document.insert_range(1, vec![vec![9.0, 9.0]]);
        assert_eq!(document.channels[0], vec![1.0, 9.0, 9.0, 2.0, 3.0]);
        assert_eq!(document.channels[1], vec![10.0, 9.0, 9.0, 20.0, 30.0]);
        assert_eq!(
            document.channels[0].len(),
            document.channels[1].len(),
            "channels must stay the same length or playback panics indexing the shorter one"
        );
    }

    /// The inverse mismatch: inserting stereo (or wider) `data` into a document with fewer
    /// channels drops the extra source channels rather than growing `self.channels` or
    /// panicking — only as many of `data`'s channels as `self.channels` has are used.
    #[test]
    fn inserting_stereo_data_into_a_mono_document_drops_the_extra_channel() {
        let mut document = doc(vec![1.0, 2.0, 3.0]);
        document.insert_range(1, vec![vec![9.0, 9.0], vec![99.0, 99.0]]);
        assert_eq!(document.channels, vec![vec![1.0, 9.0, 9.0, 2.0, 3.0]]);
    }
}
