//! The **native** backend: combiners that join a step's parallel branches, rendered in-process.
//!
//! This is the fourth backend and, like Airwindows, not an external program — but where
//! Airwindows vendors someone else's DSP, these two are a few lines of arithmetic each. They
//! exist because combining branches must not depend on having CDP installed.
//!
//! Before this, joining two signals meant a CDP `DualWav`/`DualAna` process. Those are real
//! musical tools and remain available as two-leg joins, but as *the* combining mechanism they
//! were wrong on three counts: most of them are spectral, so a plain mix cost a pvoc round
//! trip; they cap out at two inputs; and without CDP on the machine there was no way to
//! recombine anything at all.
//!
//! # What a combiner is, structurally
//!
//! A native combiner takes **every** input from a [`super::chain::Branch`]
//! ([`super::ProcessDef::consumes_running_buffer`] is false for it), unlike every other chain
//! step, which takes input 0 from the running buffer. That is what removes the need for a
//! separate "split" node: each leg is a branch whose source is
//! [`super::chain::BranchSource::Tap`] — a copy of the signal arriving at the combiner — so an
//! empty leg is the dry signal and a leg with steps in it is a processed one, both starting
//! from the same audio.
//!
//! # Reconciliation
//!
//! Branches are whole sub-chains, so they can disagree about length, channel count and sample
//! rate. [`mix`] and [`crossfade`] state one rule for each, here rather than at the call site,
//! so both combiners answer identically:
//!
//! - **Length**: pad to the longest with silence. Truncating to the shortest would discard a
//!   reverb tail or a time-stretch, which is usually the reason a leg is longer.
//! - **Channels**: widen to the widest leg, leaving the extra channels of a narrower one
//!   *silent*. Deliberately not `Document::insert_range`'s fill-from-channel-0 behaviour, which
//!   would smear a mono leg across every channel of a wider one.
//! - **Sample rate**: not handled here. Rate conversion needs the resampler, which lives in
//!   `commands::resample`, so the caller reconciles rates before calling and these functions
//!   take plain sample data.

use super::def::{
    Category, IoKind, NumberScale, ParamDef, ParamKind, ProcessDef,
};

/// How many branches a [`mix`] node takes: exactly two.
///
/// Two rather than "as many as you like" because a split you can read is a split you can see,
/// and past two the columns stop fitting side by side at a width that still shows their own
/// parameters — which is the whole point of drawing them as columns. Deeper structure is
/// expressed by *nesting* a split inside a branch (bounded in turn by `chain::MAX_SPLIT_DEPTH`),
/// which keeps every combiner two-way and the picture legible.
///
/// Being fixed also means the ceiling and the floor coincide, so a combiner never has a branch
/// count to configure — there is no "+ Add branch" anywhere. `mix` still takes a slice, because
/// the arithmetic has no opinion about how many.
pub const MAX_MIX_LEGS: usize = 2;

/// Per-leg gain range, in dB. `-inf` would be the honest bottom of a fader, but a closed range
/// is what earns the parameter a slider (`param_slider::applies`), and -60 dB is inaudible
/// against anything else in a mix.
pub const LEG_GAIN_MIN_DB: f64 = -60.0;
pub const LEG_GAIN_MAX_DB: f64 = 12.0;

/// What a leg opens at. Unity on every leg of an N-way mix is the one setting guaranteed to
/// clip, the same reasoning `stereo_mix` uses for its own -6 dB default.
pub const LEG_GAIN_DEFAULT_DB: f64 = -6.0;

/// Where the limiter works towards by default.
///
/// Just under full scale rather than `dsp::PLAYBACK_CEILING_DB`'s -1: this is a *mix*, whose
/// result is going back into the document, and giving away a whole decibel of headroom on every
/// join would accumulate down a chain. -0.1 dB is enough to stay off the rail while costing
/// nothing audible, and the limiter is unity-gain for anything below it anyway.
pub const MIX_CEILING_DEFAULT_DB: f64 = -0.1;

/// Settings for one leg of a [`mix`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegSettings {
    pub gain_db: f64,
    pub invert: bool,
}

impl Default for LegSettings {
    fn default() -> Self {
        Self { gain_db: LEG_GAIN_DEFAULT_DB, invert: false }
    }
}

/// Deinterleaved audio, one `Vec` per channel — the shape a chain's running buffer already has.
pub type Channels = Vec<Vec<f32>>;

fn frame_count(legs: &[(&Channels, LegSettings)]) -> usize {
    legs.iter().flat_map(|(ch, _)| ch.iter()).map(|c| c.len()).max().unwrap_or(0)
}

fn channel_count(legs: &[(&Channels, LegSettings)]) -> usize {
    legs.iter().map(|(ch, _)| ch.len()).max().unwrap_or(0)
}

/// Sums `legs`, each scaled by its own gain and optionally phase-inverted, then optionally runs
/// the summed result through the shared tanh limiter.
///
/// The limiter is applied to the **sum**, not per leg — limiting each contribution separately
/// would let a chorus of quiet legs still sum past the ceiling, which is the same reason
/// `stereo_mix` limits its summed legs rather than each routed channel.
pub fn mix(legs: &[(&Channels, LegSettings)], limit: bool, ceiling_db: f64) -> Channels {
    let channels = channel_count(legs);
    let frames = frame_count(legs);
    let mut out: Channels = vec![vec![0.0; frames]; channels];

    for (source, settings) in legs {
        let scale = crate::model::dsp::db_to_linear(settings.gain_db as f32)
            * if settings.invert { -1.0 } else { 1.0 };
        for (c, channel) in source.iter().enumerate() {
            let dest = &mut out[c];
            for (i, &sample) in channel.iter().enumerate() {
                dest[i] += sample * scale;
            }
        }
    }

    if limit {
        let ceiling = crate::model::dsp::db_to_linear(ceiling_db as f32);
        for channel in &mut out {
            for sample in channel.iter_mut() {
                *sample = crate::model::dsp::tanh_limit(*sample, ceiling);
            }
        }
    }
    out
}

/// Blends two legs by `blend`: 0.0 is all `a`, 1.0 is all `b`.
///
/// **Equal-power**, not linear: a linear crossfade of two uncorrelated signals dips ~3 dB in the
/// middle, which is audible as a hole exactly where the interesting part of a blend is. Legs of
/// a chain are usually processed versions of the same material, so they sit somewhere between
/// correlated and not — equal-power is the conventional choice for that and is what a DAW
/// crossfade does.
///
/// `blend` may be a single value or one per frame (an automation curve already evaluated
/// against the output length); a shorter curve holds its last value, and an empty one reads as
/// a constant 0.5.
pub fn crossfade(a: &Channels, b: &Channels, blend: &[f64]) -> Channels {
    let legs = [(a, LegSettings::default()), (b, LegSettings::default())];
    let channels = channel_count(&legs);
    let frames = frame_count(&legs);
    let mut out: Channels = vec![vec![0.0; frames]; channels];

    for i in 0..frames {
        let t = match blend.len() {
            0 => 0.5,
            n => blend[i.min(n - 1)],
        }
        .clamp(0.0, 1.0);
        // sin/cos of a quarter turn rather than sqrt(t)/sqrt(1-t) — both hold a^2 + b^2 = 1,
        // and this form is the conventional spelling. The ends are special-cased because
        // `cos(PI/2)` is 6.1e-17 rather than 0: inaudible, but "blend = 1 is leg B" should be
        // exactly true rather than true to -324 dBFS, so that a fully-faded leg really is gone.
        let (gain_a, gain_b) = if t == 0.0 {
            (1.0, 0.0)
        } else if t == 1.0 {
            (0.0, 1.0)
        } else {
            let (sin, cos) = (t * std::f64::consts::FRAC_PI_2).sin_cos();
            (cos, sin)
        };
        for c in 0..channels {
            let sample_a = a.get(c).and_then(|ch| ch.get(i)).copied().unwrap_or(0.0);
            let sample_b = b.get(c).and_then(|ch| ch.get(i)).copied().unwrap_or(0.0);
            out[c][i] = sample_a * gain_a as f32 + sample_b * gain_b as f32;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Catalog entries
// ---------------------------------------------------------------------------------------------

/// Catalog key of the N-leg mixer.
pub const MIXER_KEY: &str = "native_mixer";
/// Catalog key of the two-leg crossfade.
pub const CROSSFADE_KEY: &str = "native_crossfade";

/// The mixer's two whole-node parameters come first, before any branch's.
///
/// Not for reading order — the editor lists the branches above them either way — but so that the
/// parameters a mixer actually uses form a contiguous prefix of the declared list: a one-branch
/// mixer uses indices `0..4` and the branch it does not have is past the end. A dialog can then
/// be handed fewer fields instead of showing faders for branches that do not exist.
pub const MIX_LIMIT_PARAM: usize = 0;
pub const MIX_CEILING_PARAM: usize = 1;

/// How many parameters a mixer with `branches` branches actually uses — see the note above.
pub fn mix_param_count(branches: usize) -> usize {
    2 + 2 * branches.min(MAX_MIX_LEGS)
}

/// Parameter index of branch `leg`'s gain, and of its phase invert.
pub fn leg_gain_param(leg: usize) -> usize {
    2 + leg * 2
}
pub fn leg_invert_param(leg: usize) -> usize {
    3 + leg * 2
}

fn number(name: &str, description: &str, min: f64, max: f64, step: f64, default: f64, automatable: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        description: description.into(),
        flag: None,
        automatable,
        required_envelope: false,
        required_list: false,
        list_is_time_sequence: false,
        before_outfile: false,
        praat_pause_block: None,
        praat_directory_var: None,
        key_value_group: None,
        key_value_key: None,
        range_scales_with_input_duration: false,
        default_from_dc_offset: false,
        rows_match_input_count: false,
        kind: ParamKind::Number {
            min,
            max,
            step,
            default,
            exponential: false,
            scale: NumberScale::Plain,
            integer: false,
        },
    }
}

fn toggle(name: &str, description: &str, default: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        description: description.into(),
        flag: None,
        automatable: false,
        required_envelope: false,
        required_list: false,
        list_is_time_sequence: false,
        before_outfile: false,
        praat_pause_block: None,
        praat_directory_var: None,
        key_value_group: None,
        key_value_key: None,
        range_scales_with_input_duration: false,
        default_from_dc_offset: false,
        rows_match_input_count: false,
        kind: ParamKind::Toggle { default },
    }
}

fn native_def(key: &str, bin: &str, title: &str, short: &str, description: &str, params: Vec<ParamDef>) -> ProcessDef {
    ProcessDef {
        key: key.into(),
        bin: bin.into(),
        subprog: None,
        mode: None,
        title: title.into(),
        category: Category::Native,
        subcategory: "combine".into(),
        short_description: short.into(),
        description: description.into(),
        // Plain audio in and out, which is what makes a combiner an ordinary chain step
        // (`chain::process_is_chainable`). Its parallel inputs arrive as branches, not as a
        // wider `IoKind` — see this module's header.
        input: IoKind::Wav,
        output: IoKind::Wav,
        stereo_native: true,
        output_is_stereo: false,
        input_channels: None,
        output_channels: None,
        output_new_buffer: false,
        interactive: false,
        praat_form_locks: Vec::new(),
        praat_builtin: false,
        praat_python_rewrite: false,
        praat_pause_button: None,
        requires_simple_wav_input: false,
        sidecar_extension: None,
        min_inputs: None,
        needs_head_tail_marks: false,
        head_tail_marks_unpaired: false,
        flags_before_infile: false,
        channel_split: None,
        spec_grab_prepass: false,
        preset_param: None,
        preset_custom_option: 0,
        script_presets: Vec::new(),
        params,
        param_notes: Vec::new(),
    }
}

/// The native catalog: built in code rather than read from a TOML, because the mixer's
/// parameter list is formulaic (`MAX_MIX_LEGS` legs of two parameters each, then two more) and
/// a hand-written table would be a hundred lines that must be kept in step with that constant
/// by hand. Every other catalog is machine-generated for the same underlying reason.
pub fn process_defs() -> Vec<ProcessDef> {
    let mut mixer_params = Vec::with_capacity(MAX_MIX_LEGS * 2 + 2);
    mixer_params.push(toggle(
        "Limiter",
        "Soft-limit the summed result, so a dense mix saturates instead of clipping.",
        true,
    ));
    mixer_params.push(number(
        "Ceiling",
        "Level the limiter works towards, in dBFS.",
        -24.0,
        0.0,
        0.1,
        MIX_CEILING_DEFAULT_DB,
        false,
    ));
    for leg in 0..MAX_MIX_LEGS {
        let letter = char::from(b'A' + leg as u8);
        mixer_params.push(number(
            &format!("Branch {letter} gain"),
            "Level of this parallel branch in the mix, in dB.",
            LEG_GAIN_MIN_DB,
            LEG_GAIN_MAX_DB,
            0.1,
            LEG_GAIN_DEFAULT_DB,
            true,
        ));
        mixer_params.push(toggle(
            &format!("Branch {letter} invert"),
            "Flip this branch's phase before summing — cancels what it shares with the others.",
            false,
        ));
    }

    let crossfade_params = vec![number(
        "Blend",
        "0 is all of branch A, 1 is all of branch B. Equal-power, so the middle doesn't dip.",
        0.0,
        1.0,
        0.01,
        0.5,
        true,
    )];

    vec![
        native_def(
            MIXER_KEY,
            "Combine/Mixer",
            "Mixer",
            "Sum this step's parallel branches, each at its own level.",
            "Sums every parallel branch feeding this step. Each branch has its own gain and \
             phase invert, and the summed result can be soft-limited so a dense mix saturates \
             rather than clipping. Branches of unequal length are padded to the longest, and a \
             narrower branch's missing channels stay silent rather than being filled from its \
             channel 1. Needs nothing installed.",
            mixer_params,
        ),
        native_def(
            CROSSFADE_KEY,
            "Combine/Crossfade",
            "Crossfade",
            "Blend two parallel branches, optionally over time.",
            "Blends branch A into branch B. The blend is equal-power, so two uncorrelated \
             branches don't dip in level halfway across, and it is automatable — give it an \
             envelope to fade from one branch to the other over the selection. Needs nothing \
             installed.",
            crossfade_params,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono(samples: &[f32]) -> Channels {
        vec![samples.to_vec()]
    }

    fn unity(gain_db: f64) -> LegSettings {
        LegSettings { gain_db, invert: false }
    }

    #[test]
    fn mixing_sums_legs_at_their_own_gains() {
        let a = mono(&[1.0, 1.0]);
        let b = mono(&[1.0, 1.0]);
        let out = mix(&[(&a, unity(0.0)), (&b, unity(-6.0))], false, 0.0);
        // 1.0 + ~0.5012 (-6 dB)
        assert!((out[0][0] - 1.5012).abs() < 1e-3, "got {}", out[0][0]);
    }

    #[test]
    fn inverting_a_leg_cancels_what_it_shares_with_another() {
        let a = mono(&[0.5, -0.25, 1.0]);
        let out = mix(
            &[(&a, unity(0.0)), (&a, LegSettings { gain_db: 0.0, invert: true })],
            false,
            0.0,
        );
        assert_eq!(out[0], vec![0.0, 0.0, 0.0], "a leg against itself, inverted, is silence");
    }

    /// Truncating to the shortest leg would throw away exactly what makes a leg longer — a
    /// reverb tail, a time-stretch — so the rule is pad-to-longest.
    #[test]
    fn legs_of_unequal_length_pad_to_the_longest() {
        let short = mono(&[1.0]);
        let long = mono(&[1.0, 1.0, 1.0]);
        let out = mix(&[(&short, unity(0.0)), (&long, unity(0.0))], false, 0.0);
        assert_eq!(out[0].len(), 3);
        assert_eq!(out[0][0], 2.0);
        assert_eq!(out[0][2], 1.0, "past the short leg, only the long one contributes");
    }

    /// A narrower leg must not be smeared across the wider one's channels — the mistake
    /// `Document::insert_range`'s fill-from-channel-0 behaviour would make here.
    #[test]
    fn a_narrower_leg_leaves_the_extra_channels_silent() {
        let mono_leg = mono(&[1.0, 1.0]);
        let stereo_leg = vec![vec![0.25, 0.25], vec![0.5, 0.5]];
        let out = mix(&[(&mono_leg, unity(0.0)), (&stereo_leg, unity(0.0))], false, 0.0);
        assert_eq!(out.len(), 2, "widened to the widest leg");
        assert_eq!(out[0], vec![1.25, 1.25], "channel 1 gets both legs");
        assert_eq!(out[1], vec![0.5, 0.5], "channel 2 gets only the stereo leg, not a copy");
    }

    #[test]
    fn the_limiter_acts_on_the_sum_not_on_each_leg() {
        // Four legs, each well under the ceiling alone, together far over it.
        let leg = mono(&[0.5]);
        let legs: Vec<_> = (0..4).map(|_| (&leg, unity(0.0))).collect();
        let limited = mix(&legs, true, -1.0);
        let raw = mix(&legs, false, -1.0);
        assert!(raw[0][0] > 1.9, "unlimited sum really does run over");
        assert!(limited[0][0] < 1.0, "limited sum is brought under full scale");
    }

    #[test]
    fn mixing_nothing_at_all_is_empty_rather_than_a_panic() {
        assert!(mix(&[], false, 0.0).is_empty());
    }

    #[test]
    fn crossfade_ends_are_exactly_one_leg_or_the_other() {
        let a = mono(&[1.0]);
        let b = mono(&[0.5]);
        assert_eq!(crossfade(&a, &b, &[0.0])[0][0], 1.0);
        assert_eq!(crossfade(&a, &b, &[1.0])[0][0], 0.5);
    }

    /// The reason for equal-power: two uncorrelated legs must not dip in the middle. Summing
    /// the squared gains is the property that guarantees it.
    #[test]
    fn crossfade_holds_constant_power_across_the_blend() {
        let a = mono(&[1.0]);
        let b = mono(&[0.0]);
        for step in 0..=10 {
            let t = step as f64 / 10.0;
            let gain_a = crossfade(&a, &b, &[t])[0][0] as f64;
            let gain_b = crossfade(&b, &a, &[t])[0][0] as f64;
            // f32 samples, so the tolerance is f32's, not f64's.
            assert!(
                (gain_a * gain_a + gain_b * gain_b - 1.0).abs() < 1e-6,
                "power dips at t={t}: {gain_a}^2 + {gain_b}^2"
            );
        }
        // A linear fade would read 0.5 here; equal-power reads ~0.707.
        assert!((crossfade(&a, &b, &[0.5])[0][0] - 0.7071).abs() < 1e-3);
    }

    #[test]
    fn a_per_frame_blend_curve_drives_the_fade_and_holds_its_last_value() {
        let a = mono(&[1.0, 1.0, 1.0, 1.0]);
        let b = mono(&[0.0, 0.0, 0.0, 0.0]);
        let out = crossfade(&a, &b, &[0.0, 1.0]);
        assert_eq!(out[0][0], 1.0, "frame 0 follows the curve");
        assert_eq!(out[0][1], 0.0, "frame 1 follows the curve");
        assert_eq!(out[0][3], 0.0, "past the curve, its last value holds");
        // An empty curve is a plain half-and-half blend rather than silence.
        assert!((crossfade(&a, &b, &[])[0][0] - 0.7071).abs() < 1e-3);
    }

    #[test]
    fn the_mixer_declares_one_gain_and_one_invert_per_leg_plus_the_limiter_pair() {
        let defs = process_defs();
        let mixer = defs.iter().find(|d| d.key == MIXER_KEY).expect("mixer in the native catalog");
        assert_eq!(mixer.params.len(), MAX_MIX_LEGS * 2 + 2);
        assert_eq!(mixer.params[leg_gain_param(0)].name, "Branch A gain");
        assert_eq!(mixer.params[leg_invert_param(0)].name, "Branch A invert");
        assert_eq!(mixer.params[leg_gain_param(1)].name, "Branch B gain");
        assert_eq!(mixer.params[MIX_LIMIT_PARAM].name, "Limiter");
        assert_eq!(mixer.params[MIX_CEILING_PARAM].name, "Ceiling");
        // The whole point of the ordering: what a mixer uses is a prefix of the declared list.
        assert_eq!(mix_param_count(MAX_MIX_LEGS), mixer.params.len());
        assert_eq!(leg_invert_param(MAX_MIX_LEGS - 1), mixer.params.len() - 1);
    }

    /// A leg's gain and the crossfade's blend are the first non-CDP parameters in the app to
    /// declare `automatable` — the whole point of a combiner you can fade over time.
    #[test]
    fn the_automatable_parameters_are_the_ones_worth_a_curve() {
        let defs = process_defs();
        let mixer = defs.iter().find(|d| d.key == MIXER_KEY).unwrap();
        assert!(mixer.params[leg_gain_param(0)].automatable);
        assert!(!mixer.params[MIX_CEILING_PARAM].automatable, "a ceiling is a bound, not a signal");
        let xfade = defs.iter().find(|d| d.key == CROSSFADE_KEY).unwrap();
        assert!(xfade.params[0].automatable);
    }

    /// Both combiners must be usable as chain steps at all, and must require at least two
    /// branches — a combiner with one leg is not combining anything.
    #[test]
    fn both_combiners_are_chainable_and_require_two_branches() {
        for def in process_defs() {
            assert!(super::super::chain::process_is_chainable(&def), "{} not chainable", def.key);
            assert_eq!(def.branch_arity_min(), 2, "{} should demand two legs", def.key);
            assert_eq!(def.backend(), super::super::def::Backend::Native);
        }
    }

    /// Both combiners take exactly two branches — ceiling equal to floor — which is what makes
    /// a branch count something a chain never has to configure.
    #[test]
    fn both_combiners_take_exactly_two_branches() {
        for def in process_defs() {
            assert_eq!(def.branch_arity(), 2, "{}", def.key);
            assert_eq!(def.branch_arity_min(), 2, "{}", def.key);
        }
    }
}
