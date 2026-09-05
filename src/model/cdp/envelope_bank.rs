//! A chain's own bank of named breakpoint envelopes, and the one definition of what a bank
//! curve *means*.
//!
//! A parameter envelope as edited (`ui::app`'s `CdpField::Number { envelope }`) is in absolute
//! units on both axes: X in seconds against whatever selection was live when it was drawn, Y in
//! the parameter's own declared range. That is fine for a single process — you draw it, you
//! apply it, the selection has not moved — and it is exactly wrong for a chain, for two
//! independent reasons:
//!
//! 1. **A chain outlives its selection.** A chain saved against a 10-second selection and re-run
//!    against 3 seconds writes automation that runs off the end of the file; against 30 seconds
//!    it finishes in the first third. Nothing rescaled it, because the duration it was authored
//!    against was recorded nowhere.
//! 2. **Steps in a chain do not share a duration.** A time-stretch changes length and a reverb
//!    tail extends it, so "3.0 seconds" is a different place at step 2 than at step 7. One shape
//!    driving parameters in both steps cannot be in seconds and be true in both.
//!
//! So a bank curve is **normalized to 0..1 on both axes**, and it is projected onto a real axis
//! only at the moment it is used — against that step's actual input duration and that
//! parameter's own range. That is the whole reason the bank exists as a type rather than as a
//! `Vec<(f64, f64)>` in a map: normalization is a property of the storage, stated once here, so
//! no caller can put seconds in it.
//!
//! Y being normalized too is what makes one shape reusable across parameters that share no
//! units at all — `neighbours` is 1..100, a mixer leg's gain is -60..+12 dB. An
//! [`EnvelopeRef`] carries the window each reference reads the shape through.

use serde::{Deserialize, Serialize};

/// Both axes of a bank curve are clamped to this range. Not configurable: a bank curve that
/// could leave 0..1 would be carrying units again, which is the thing this module exists to
/// prevent.
pub const BANK_MIN: f64 = 0.0;
pub const BANK_MAX: f64 = 1.0;

/// Fewer points than this is not a curve — the editor enforces the same floor
/// (`handle_cdp_envelope_key`'s Delete arm), and CDP's own breakpoint parser needs at least a
/// start and an end.
pub const MIN_POINTS: usize = 2;

/// One named shape in a chain's bank. `points` are `(time, value)` with **both** normalized to
/// `BANK_MIN..=BANK_MAX`, sorted by time ascending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BankEnvelope {
    pub name: String,
    pub points: Vec<(f64, f64)>,
}

/// Every envelope belonging to one chain. Serialized inside the chain's own preset file, so a
/// chain is self-contained: copying the file copies its curves, and there is no global library
/// whose rename could leave a saved chain pointing at nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvelopeBank {
    #[serde(default)]
    pub envelopes: Vec<BankEnvelope>,
}

/// How one parameter reads one bank curve. `min`/`max` are the window the normalized shape is
/// projected into — auto-filled from the parameter's own declared range on attach, then
/// narrowed by hand to get a gentler reading of the same shape. `invert` flips it, so one curve
/// can push one parameter up as it pushes another down.
///
/// Note `min > max` is *not* an error and not how inversion is spelled: the window is applied
/// as `min + (max - min) * y`, so an author who swaps the two gets an inverted reading either
/// way, and `invert` stays the explicit spelling that survives editing the numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvelopeRef {
    pub name: String,
    pub min: f64,
    pub max: f64,
    #[serde(default)]
    pub invert: bool,
}

/// Why a bank curve or a reference to one is not usable. Surfaced as a plain sentence the way
/// `ChainError` is, rather than matched on.
#[derive(Debug, Clone, PartialEq)]
pub enum BankError {
    /// An `EnvelopeRef` names a curve the bank does not hold — the dangling-reference case a
    /// per-chain bank is *supposed* to make impossible, kept as a real error because a
    /// hand-edited preset file can still produce it.
    UnknownEnvelope { name: String },
    /// Fewer than [`MIN_POINTS`] points.
    TooFewPoints { name: String, count: usize },
    /// A point outside `BANK_MIN..=BANK_MAX` on either axis — i.e. something wrote absolute
    /// units into the bank, which is the class of bug this module exists to catch.
    OutOfRange { name: String, point: (f64, f64) },
    /// Times not ascending. CDP's breakpoint parser requires it, and the editor maintains it.
    TimesNotAscending { name: String },
    /// Two curves sharing a name — references address by name, so this would be ambiguous.
    DuplicateName { name: String },
}

impl BankEnvelope {
    /// Lifts an absolute, as-authored envelope into normalized bank form.
    ///
    /// `authored_time_max` is the duration the curve's X axis was drawn against — the caller
    /// must supply it, because it is precisely the information the old per-parameter storage
    /// failed to record and whose absence is why a saved chain's automation drifted. `min`/`max`
    /// are the parameter's declared range, which is what the Y values are in.
    ///
    /// `invert` must be the flag of the reference the points were read through, so that this
    /// undoes what [`BankEnvelope::project`] applied. Normalizing an inverted reading without it
    /// stores the *flipped* shape, which then flips again on the next read and flips for every
    /// other parameter sharing the curve.
    ///
    /// Degenerate axes collapse rather than divide by zero: a zero-length span puts everything
    /// at time 0, a zero-width range puts everything at value 0.
    pub fn normalized(
        name: impl Into<String>,
        points: &[(f64, f64)],
        authored_time_max: f64,
        min: f64,
        max: f64,
        invert: bool,
    ) -> Self {
        let span = if authored_time_max > 0.0 { authored_time_max } else { 0.0 };
        let width = max - min;
        let points = points
            .iter()
            .map(|&(t, v)| {
                let nt = if span > 0.0 { (t / span).clamp(BANK_MIN, BANK_MAX) } else { BANK_MIN };
                let nv = if width != 0.0 { ((v - min) / width).clamp(BANK_MIN, BANK_MAX) } else { BANK_MIN };
                (nt, if invert { BANK_MAX - nv } else { nv })
            })
            .collect();
        Self { name: name.into(), points }
    }

    /// Projects this normalized shape back onto a real axis: `time_max` seconds wide, values
    /// spanning `window` (`min`..`max`, flipped when `invert`). The inverse of
    /// [`BankEnvelope::normalized`] when handed the same range, and the single definition of
    /// what a reference *evaluates to* — used both to seed the graphical editor when a chain
    /// step is opened and to build the `.brk` file when the chain runs, so the curve on screen
    /// and the curve CDP receives cannot disagree.
    pub fn project(&self, time_max: f64, min: f64, max: f64, invert: bool) -> Vec<(f64, f64)> {
        let width = max - min;
        self.points
            .iter()
            .map(|&(t, v)| {
                let y = if invert { BANK_MAX - v } else { v };
                (t * time_max, min + width * y)
            })
            .collect()
    }

    /// Checks this curve is really in bank form. Called by `EnvelopeBank::validate`, and worth
    /// calling directly on anything arriving from a file.
    pub fn validate(&self) -> Result<(), BankError> {
        if self.points.len() < MIN_POINTS {
            return Err(BankError::TooFewPoints { name: self.name.clone(), count: self.points.len() });
        }
        for &p in &self.points {
            if p.0 < BANK_MIN || p.0 > BANK_MAX || p.1 < BANK_MIN || p.1 > BANK_MAX {
                return Err(BankError::OutOfRange { name: self.name.clone(), point: p });
            }
        }
        if self.points.windows(2).any(|w| w[1].0 < w[0].0) {
            return Err(BankError::TimesNotAscending { name: self.name.clone() });
        }
        Ok(())
    }
}

impl EnvelopeBank {
    pub fn get(&self, name: &str) -> Option<&BankEnvelope> {
        self.envelopes.iter().find(|e| e.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut BankEnvelope> {
        self.envelopes.iter_mut().find(|e| e.name == name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// The next free `Env N` name.
    ///
    /// Curves are numbered rather than named after the parameter they were first drawn on,
    /// because a bank curve is *shared*: the moment a second parameter references it, a name
    /// like "Neighbours" describes only where it happened to start and actively misleads about
    /// everywhere else it is used. A number claims nothing.
    pub fn next_name(&self) -> String {
        (1..).map(|n| format!("Env {n}")).find(|c| !self.contains(c)).expect("infinite range")
    }

    /// A name not already taken, formed from `base` by appending ` 2`, ` 3`, … — the same shape
    /// the buffer list uses for duplicate filenames. Used when a caller has a name in mind
    /// (a rename, an imported curve) rather than wanting the next number.
    pub fn unique_name(&self, base: &str) -> String {
        let base = if base.trim().is_empty() { "envelope" } else { base.trim() };
        if !self.contains(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base} {n}")).find(|c| !self.contains(c)).expect("infinite range")
    }

    /// Inserts under a name guaranteed not to collide, returning the name actually used —
    /// callers store that in the `EnvelopeRef` they build.
    pub fn insert_unique(&mut self, mut envelope: BankEnvelope) -> String {
        let name = self.unique_name(&envelope.name);
        envelope.name = name.clone();
        self.envelopes.push(envelope);
        name
    }

    /// Resolves a reference to real points on a real axis, or says why it cannot. The one place
    /// a dangling name becomes an error.
    pub fn resolve(&self, reference: &EnvelopeRef, time_max: f64) -> Result<Vec<(f64, f64)>, BankError> {
        let envelope = self
            .get(&reference.name)
            .ok_or_else(|| BankError::UnknownEnvelope { name: reference.name.clone() })?;
        Ok(envelope.project(time_max, reference.min, reference.max, reference.invert))
    }

    /// The lowest and highest values a reference actually *produces*: this curve's own extremes
    /// projected through that reference's window.
    ///
    /// Not the window itself, which is what a parameter row showed first. The window is what the
    /// curve *could* reach; a curve spanning 0.2..0.8 read through a 1..200 window only ever
    /// reaches 40.8..160.2, and stating the window there overstates what will happen — the more
    /// so once one shape is shared by parameters whose windows differ. Piecewise-linear
    /// interpolation puts every extreme on a breakpoint, so scanning the points is exact rather
    /// than a sampling.
    pub fn produced_span(&self, reference: &EnvelopeRef) -> Option<(f64, f64)> {
        let envelope = self.get(&reference.name)?;
        let points = envelope.project(1.0, reference.min, reference.max, reference.invert);
        let lo = points.iter().map(|&(_, v)| v).fold(f64::INFINITY, f64::min);
        let hi = points.iter().map(|&(_, v)| v).fold(f64::NEG_INFINITY, f64::max);
        (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
    }

    pub fn validate(&self) -> Result<(), BankError> {
        for (i, envelope) in self.envelopes.iter().enumerate() {
            if self.envelopes[..i].iter().any(|e| e.name == envelope.name) {
                return Err(BankError::DuplicateName { name: envelope.name.clone() });
            }
            envelope.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str, points: &[(f64, f64)]) -> BankEnvelope {
        BankEnvelope { name: name.into(), points: points.to_vec() }
    }

    #[test]
    fn normalizing_then_projecting_the_same_range_round_trips() {
        let authored = [(0.0, 1.0), (5.0, 100.0), (10.0, 40.0)];
        let bank_env = BankEnvelope::normalized("swell", &authored, 10.0, 1.0, 100.0, false);
        let back = bank_env.project(10.0, 1.0, 100.0, false);
        for (a, b) in authored.iter().zip(&back) {
            assert!((a.0 - b.0).abs() < 1e-9, "time {a:?} vs {b:?}");
            assert!((a.1 - b.1).abs() < 1e-9, "value {a:?} vs {b:?}");
        }
    }

    /// The whole point of the module: the *same* stored shape read against a different duration
    /// spans that duration, instead of running off the end or finishing early.
    #[test]
    fn one_shape_spans_whatever_duration_it_is_projected_onto() {
        let bank_env = BankEnvelope::normalized("swell", &[(0.0, 0.0), (10.0, 1.0)], 10.0, 0.0, 1.0, false);
        assert_eq!(bank_env.project(3.0, 0.0, 1.0, false).last().unwrap().0, 3.0);
        assert_eq!(bank_env.project(30.0, 0.0, 1.0, false).last().unwrap().0, 30.0);
    }

    /// One curve, two parameters with unrelated units — which is what a shared bank is for.
    #[test]
    fn one_shape_reads_through_each_references_own_window() {
        let bank_env = env("swell", &[(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]);
        assert_eq!(bank_env.project(1.0, 1.0, 100.0, false)[1].1, 100.0);
        assert_eq!(bank_env.project(1.0, -60.0, 12.0, false)[1].1, 12.0);
        // A narrowed window is a gentler reading of the identical shape.
        assert_eq!(bank_env.project(1.0, 4.0, 40.0, false)[1].1, 40.0);
        assert_eq!(bank_env.project(1.0, 4.0, 40.0, false)[0].1, 4.0);
    }

    #[test]
    fn invert_flips_the_shape_within_the_same_window() {
        let bank_env = env("ramp", &[(0.0, 0.0), (1.0, 1.0)]);
        let plain = bank_env.project(1.0, 10.0, 20.0, false);
        let flipped = bank_env.project(1.0, 10.0, 20.0, true);
        assert_eq!(plain, vec![(0.0, 10.0), (1.0, 20.0)]);
        assert_eq!(flipped, vec![(0.0, 20.0), (1.0, 10.0)]);
    }

    #[test]
    fn normalizing_clamps_rather_than_escaping_the_bank_range() {
        // A point past the authored duration, and a value outside the declared range.
        let bank_env = BankEnvelope::normalized("odd", &[(0.0, -5.0), (99.0, 500.0)], 10.0, 0.0, 100.0, false);
        assert_eq!(bank_env.points, vec![(0.0, 0.0), (1.0, 1.0)]);
        assert_eq!(bank_env.validate(), Ok(()));
    }

    #[test]
    fn degenerate_axes_collapse_instead_of_dividing_by_zero() {
        let zero_span = BankEnvelope::normalized("z", &[(0.0, 5.0), (1.0, 7.0)], 0.0, 0.0, 10.0, false);
        assert!(zero_span.points.iter().all(|p| p.0 == 0.0));
        let zero_width = BankEnvelope::normalized("w", &[(0.0, 5.0), (1.0, 7.0)], 1.0, 3.0, 3.0, false);
        assert!(zero_width.points.iter().all(|p| p.1 == 0.0));
    }

    #[test]
    fn validate_rejects_absolute_units_smuggled_into_the_bank() {
        let seconds = env("seconds", &[(0.0, 0.0), (10.0, 1.0)]);
        assert_eq!(
            seconds.validate(),
            Err(BankError::OutOfRange { name: "seconds".into(), point: (10.0, 1.0) })
        );
    }

    #[test]
    fn validate_rejects_too_few_points_and_descending_times() {
        assert_eq!(
            env("one", &[(0.0, 0.0)]).validate(),
            Err(BankError::TooFewPoints { name: "one".into(), count: 1 })
        );
        assert_eq!(
            env("back", &[(0.0, 0.0), (0.8, 1.0), (0.4, 0.5)]).validate(),
            Err(BankError::TimesNotAscending { name: "back".into() })
        );
    }

    /// What a parameter row states: what the curve will do, not what its window allows.
    #[test]
    fn produced_span_is_the_curves_own_reach_not_the_whole_window() {
        let bank = EnvelopeBank { envelopes: vec![env("Env 1", &[(0.0, 0.2), (0.5, 0.8), (1.0, 0.2)])] };
        let wide = EnvelopeRef { name: "Env 1".into(), min: 1.0, max: 200.0, invert: false };
        let (lo, hi) = bank.produced_span(&wide).expect("resolves");
        assert!((lo - 40.8).abs() < 1e-9, "got {lo}");
        assert!((hi - 160.2).abs() < 1e-9, "got {hi}");

        // The same shape on a parameter with unrelated units reads in those units.
        let leg = EnvelopeRef { name: "Env 1".into(), min: -60.0, max: 12.0, invert: false };
        let (lo, hi) = bank.produced_span(&leg).expect("resolves");
        assert!((lo - -45.6).abs() < 1e-9, "got {lo}");
        assert!((hi - -2.4).abs() < 1e-9, "got {hi}");

        // Inverting swaps which end of the window the curve's own extremes land on.
        let flipped = EnvelopeRef { invert: true, ..leg };
        let (lo, hi) = bank.produced_span(&flipped).expect("resolves");
        assert!((lo - -45.6).abs() < 1e-9 && (hi - -2.4).abs() < 1e-9, "got {lo}..{hi}");

        assert!(bank
            .produced_span(&EnvelopeRef { name: "nope".into(), min: 0.0, max: 1.0, invert: false })
            .is_none());
    }

    #[test]
    fn resolve_reports_a_dangling_name() {
        let bank = EnvelopeBank { envelopes: vec![env("swell", &[(0.0, 0.0), (1.0, 1.0)])] };
        let missing = EnvelopeRef { name: "nope".into(), min: 0.0, max: 1.0, invert: false };
        assert_eq!(
            bank.resolve(&missing, 4.0),
            Err(BankError::UnknownEnvelope { name: "nope".into() })
        );
        let good = EnvelopeRef { name: "swell".into(), min: 0.0, max: 2.0, invert: false };
        assert_eq!(bank.resolve(&good, 4.0), Ok(vec![(0.0, 0.0), (4.0, 2.0)]));
    }

    /// Numbered, not named after a parameter: the curve outlives the parameter it started on.
    #[test]
    fn auto_names_are_sequential_and_skip_the_ones_in_use() {
        let mut bank = EnvelopeBank::default();
        assert_eq!(bank.next_name(), "Env 1");
        bank.envelopes.push(env("Env 1", &[(0.0, 0.0), (1.0, 1.0)]));
        assert_eq!(bank.next_name(), "Env 2");
        // A gap is reused rather than skipped, so deleting one does not push numbering upward
        // forever.
        bank.envelopes.push(env("Env 3", &[(0.0, 0.0), (1.0, 1.0)]));
        assert_eq!(bank.next_name(), "Env 2");
    }

    #[test]
    fn unique_name_and_insert_unique_never_collide() {
        let mut bank = EnvelopeBank::default();
        let a = bank.insert_unique(env("swell", &[(0.0, 0.0), (1.0, 1.0)]));
        let b = bank.insert_unique(env("swell", &[(0.0, 1.0), (1.0, 0.0)]));
        assert_eq!(a, "swell");
        assert_eq!(b, "swell 2");
        assert_eq!(bank.unique_name("  "), "envelope");
        assert_eq!(bank.validate(), Ok(()));
    }

    #[test]
    fn duplicate_names_are_rejected_because_references_address_by_name() {
        let bank = EnvelopeBank {
            envelopes: vec![env("a", &[(0.0, 0.0), (1.0, 1.0)]), env("a", &[(0.0, 1.0), (1.0, 0.0)])],
        };
        assert_eq!(bank.validate(), Err(BankError::DuplicateName { name: "a".into() }));
    }

    #[test]
    fn bank_round_trips_through_toml() {
        let bank = EnvelopeBank {
            envelopes: vec![env("swell", &[(0.0, 0.0), (0.5, 1.0), (1.0, 0.2)])],
        };
        let text = toml::to_string(&bank).expect("serialize");
        let back: EnvelopeBank = toml::from_str(&text).expect("deserialize");
        assert_eq!(bank, back);
    }
}
