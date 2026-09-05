//! The horizontal stop-slider drawn beside a bounded numeric parameter.
//!
//! Every process parameter with a **closed** range — both bounds finite — gets one of these to
//! the left of its number field, in all three backends. Airwindows declares `[0-1]` on every one
//! of its parameters and CDP declares real ranges on most of its own, which is what makes the
//! control worth having: those values were previously reachable only by typing a number whose
//! meaningful range you had to read off the label first.
//!
//! A parameter bounded on one side only (Praat's `[>0]`, `[≥20]`) deliberately gets **no**
//! slider. A slider's whole claim is that its two ends are the extremes of the parameter, and
//! there is no honest cell to put the knob in when one end runs to infinity — the control would
//! have to invent a ceiling the catalog never declared. Those stay number-only, which is also
//! most of the Praat catalog.
//!
//! ## The value is free; only the knob snaps
//!
//! `CELLS` caps what the *arrow keys* step through; it does not quantize the parameter. A typed
//! 0.4271 is submitted as 0.4271, and `stop_for_value` merely picks the cell whose stop it sits
//! nearest so the knob can be drawn. Snapping the stored value instead would mean the dialog
//! silently rewrote numbers the user typed deliberately, and plenty of CDP params are
//! meaningfully sensitive well below 1/14th of their range.

use super::super::theme;
use ratatui::style::Style;
use ratatui::text::Span;

/// Cells of track. Also the maximum number of stops, so in the common case of a continuous
/// parameter every cell *is* a stop and the knob moves exactly one cell per keypress.
///
/// 15 is a round enough number of steps to cross a range without holding the key, while still
/// landing on the tidy fractions (0, 1/2, 1/4) that a parameter's useful settings cluster on.
pub const CELLS: usize = 15;

/// Unfilled track.
pub const TRACK_CHAR: char = '\u{00b7}';
/// The knob.
pub const KNOB_CHAR: char = '\u{25cf}';

/// Decimal places a stop's value is rounded to.
///
/// A stop lands wherever the arithmetic puts it — one fifteenth of `[0-1]` is 0.0714285714… —
/// and printing that raw gives a field like `0.214286` sitting beside neighbours reading `0.2`
/// and `1.0`, which pushes everything right of it out of line (user report, with a screenshot of
/// exactly that row). Three is enough for every parameter in all three catalogs: the narrowest
/// range any of them declares still puts adjacent stops further than 0.001 apart, so no two stops
/// can collapse onto the same number — `no_catalog_parameter_has_stops_closer_than_the_rounding`
/// is what keeps that true as catalogs grow.
///
/// This rounds what the *slider* produces. A typed value is still submitted exactly as typed —
/// see this module's header.
pub const DECIMALS: i32 = 3;

/// How many stops a parameter with this range gets.
///
/// A float range always gets the full `CELLS`. An **integer** range gets one stop per legal
/// value once it has fewer than that: on a `[0-3]` mode selector, 15 stops would put four of
/// them on 0, four on 1 and so on, so most arrow presses would visibly move the knob without
/// changing the number — which reads as the control being broken rather than as the parameter
/// being coarse. Capped rather than replaced, so a `[0-100]` integer still gets 15.
pub fn stop_count(min: f64, max: f64, integer: bool) -> usize {
    if !min.is_finite() || !max.is_finite() || max <= min {
        return 1;
    }
    if integer {
        // `+ 1` counts both endpoints: [0-3] holds four legal values, not three.
        let legal = (max.floor() - min.ceil() + 1.0).max(1.0);
        if legal < CELLS as f64 {
            return legal as usize;
        }
    }
    CELLS
}

/// Whether the stops are spaced geometrically rather than evenly.
///
/// Driven by the catalog's own `exponential` flag, which 85 CDP params set and which nothing
/// read until this control existed. Geometric spacing is what makes the middle of the slider
/// the *perceptual* middle on a frequency- or time-shaped parameter: evenly spaced stops on a
/// 20-20000 Hz range put thirteen of the fifteen below 2 kHz, so the entire top of the audible
/// range would live in the last two cells.
///
/// Requires a strictly positive floor, because the spacing is a ratio and there is no geometric
/// path from zero to anything. A parameter declaring `exponential` across zero (or from it)
/// falls back to even spacing rather than being refused a slider — one of the two has to give,
/// and a linear slider is merely coarse at one end where a refused one is not there at all.
fn geometric(min: f64, max: f64, exponential: bool) -> bool {
    exponential && min > 0.0 && max > min
}

/// The value at `stop`, counting from 0.
pub fn value_for_stop(stop: usize, min: f64, max: f64, integer: bool, exponential: bool) -> f64 {
    let stops = stop_count(min, max, integer);
    if stops <= 1 {
        return min;
    }
    let t = (stop.min(stops - 1) as f64) / (stops - 1) as f64;
    let raw = if geometric(min, max, exponential) {
        // Interpolating the *logs* is what spaces the stops by a constant ratio.
        (min.ln() + t * (max.ln() - min.ln())).exp()
    } else {
        min + t * (max - min)
    };
    let raw = if integer { raw.round() } else { raw };
    // The endpoints are pinned rather than left to arithmetic: `exp(ln(max))` and
    // `min + 1.0 * (max - min)` can each land a rounding error off, and the last stop of a
    // slider must be exactly the parameter's declared maximum — that is the value the user
    // reached for by pressing Right until it stopped, and CDP binaries do reject an
    // out-of-range value rather than clamping it.
    if stop == 0 {
        min
    } else if stop >= stops - 1 {
        max
    } else {
        // Rounded to `DECIMALS` so a stop reads as a number someone might have typed, rather
        // than as 0.214285714285714 — see that constant. This also kills the binary-fraction
        // noise (0.30000000000000004) that the arithmetic above would otherwise leave behind.
        let scale = 10f64.powi(DECIMALS);
        (raw * scale).round() / scale
    }
}

/// The stop whose value sits nearest `value` — where the knob is drawn.
///
/// Nearest **in the slider's own space**, so on a geometric slider 200 Hz out of 20-20000 sits
/// near the middle rather than a cell off the left end. Comparing in linear space would put the
/// knob somewhere the arrow keys could never leave it.
pub fn stop_for_value(value: f64, min: f64, max: f64, integer: bool, exponential: bool) -> usize {
    let stops = stop_count(min, max, integer);
    if stops <= 1 || !value.is_finite() {
        return 0;
    }
    let clamped = value.clamp(min, max);
    let t = if geometric(min, max, exponential) {
        (clamped.ln() - min.ln()) / (max.ln() - min.ln())
    } else {
        (clamped - min) / (max - min)
    };
    (t * (stops - 1) as f64).round().clamp(0.0, (stops - 1) as f64) as usize
}

/// Which track cell `stop` is drawn in.
///
/// Stops are spread across the full track width even when there are fewer than `CELLS` of them,
/// so a 4-stop slider still runs edge to edge and its ends still mean the parameter's ends.
pub fn cell_for_stop(stop: usize, stops: usize) -> usize {
    if stops <= 1 {
        return 0;
    }
    let stop = stop.min(stops - 1);
    ((stop as f64) * (CELLS - 1) as f64 / (stops - 1) as f64).round() as usize
}

/// The stop a click in track cell `cell` selects — the inverse of `cell_for_stop`, rounded so
/// every cell belongs to exactly one stop and no click falls between two.
pub fn stop_for_cell(cell: usize, stops: usize) -> usize {
    if stops <= 1 {
        return 0;
    }
    let cell = cell.min(CELLS - 1);
    let t = cell as f64 / (CELLS - 1) as f64;
    (t * (stops - 1) as f64).round() as usize
}

/// The track as a plain string, knob included.
///
/// The unstyled track. `spans` is what most rendering uses, since it needs the knob styled
/// separately from the dots; this is the same track as one plain string, for the two places
/// that must not style it — a *selected* row in the chain editor, which is one uniform
/// highlight by convention (`theme.rs`) so a second accent on the knob would fight it — and for
/// tests, which is what it originally existed for.
pub fn track(value: f64, min: f64, max: f64, integer: bool, exponential: bool) -> String {
    let stops = stop_count(min, max, integer);
    let knob = cell_for_stop(stop_for_value(value, min, max, integer, exponential), stops);
    (0..CELLS)
        .map(|c| if c == knob { KNOB_CHAR } else { TRACK_CHAR })
        .collect()
}

/// The track as spans, so the knob can carry the focus accent while the dots stay chrome.
///
/// Split at the knob rather than styled uniformly because the knob is the only part that says
/// anything — the dots are a ruler. On the focused row it takes `theme::FOCUS`, the same peach
/// every other "this is where you are" marker in the app uses.
pub fn spans(
    value: f64,
    min: f64,
    max: f64,
    integer: bool,
    exponential: bool,
    focused: bool,
) -> Vec<Span<'static>> {
    let stops = stop_count(min, max, integer);
    let knob = cell_for_stop(stop_for_value(value, min, max, integer, exponential), stops);
    let track_style = Style::default().fg(theme::SURFACE2).bg(theme::SURFACE0);
    let knob_style = Style::default()
        .fg(if focused { theme::FOCUS } else { theme::SUBTEXT0 })
        .bg(theme::SURFACE0);
    vec![
        Span::styled(TRACK_CHAR.to_string().repeat(knob), track_style),
        Span::styled(KNOB_CHAR.to_string(), knob_style),
        Span::styled(TRACK_CHAR.to_string().repeat(CELLS - knob - 1), track_style),
    ]
}

/// Whether this parameter gets a slider at all — see the module comment on one-sided ranges.
pub fn applies(min: f64, max: f64) -> bool {
    min.is_finite() && max.is_finite() && max > min
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_sided_range_gets_no_slider() {
        assert!(!applies(0.0, f64::INFINITY), "Praat's [>0] has no honest right-hand end");
        assert!(!applies(f64::NEG_INFINITY, 1.0));
        assert!(!applies(f64::NEG_INFINITY, f64::INFINITY));
        // A degenerate range is a constant, not a control.
        assert!(!applies(1.0, 1.0));
        assert!(applies(0.0, 1.0));
    }

    #[test]
    fn a_continuous_range_uses_every_cell() {
        assert_eq!(stop_count(0.0, 1.0, false), CELLS);
        for stop in 0..CELLS {
            assert_eq!(cell_for_stop(stop, CELLS), stop, "one stop per cell");
        }
    }

    #[test]
    fn a_small_integer_range_gets_one_stop_per_legal_value() {
        // The [0-3] mode selector: four values, four stops, so every press changes the number.
        assert_eq!(stop_count(0.0, 3.0, true), 4);
        let values: Vec<f64> = (0..4).map(|s| value_for_stop(s, 0.0, 3.0, true, false)).collect();
        assert_eq!(values, vec![0.0, 1.0, 2.0, 3.0]);
        // ...and it still spans the whole track.
        assert_eq!(cell_for_stop(0, 4), 0);
        assert_eq!(cell_for_stop(3, 4), CELLS - 1);
    }

    #[test]
    fn a_large_integer_range_is_still_capped_at_fifteen_stops() {
        assert_eq!(stop_count(0.0, 100.0, true), CELLS);
    }

    #[test]
    fn the_ends_are_exactly_the_declared_bounds() {
        // Pinned rather than computed — see `value_for_stop`. A slider whose right end is
        // 0.9999999999 instead of 1.0 can be rejected by the very binary it feeds.
        for &(min, max, exp) in &[(0.0, 1.0, false), (20.0, 20000.0, true), (-1.0, 1.0, false)] {
            let stops = stop_count(min, max, false);
            assert_eq!(value_for_stop(0, min, max, false, exp), min);
            assert_eq!(value_for_stop(stops - 1, min, max, false, exp), max);
        }
    }

    #[test]
    fn an_exponential_range_puts_the_perceptual_middle_in_the_middle() {
        // 20-20000 Hz: the geometric centre is ~632 Hz, not the linear 10010.
        let mid = value_for_stop(7, 20.0, 20000.0, false, true);
        assert!((mid - 632.0).abs() < 5.0, "geometric middle, got {mid}");
        // Every step is the same ratio — to within the `DECIMALS` rounding, which perturbs it
        // slightly and deliberately: a readable number beats a mathematically exact one on a
        // control whose steps are already a coarse approximation of a continuous range.
        let a = value_for_stop(3, 20.0, 20000.0, false, true);
        let b = value_for_stop(4, 20.0, 20000.0, false, true);
        let c = value_for_stop(5, 20.0, 20000.0, false, true);
        assert!(((b / a) - (c / b)).abs() < 1e-3, "ratios {} vs {}", b / a, c / b);
    }

    #[test]
    fn an_exponential_range_touching_zero_falls_back_to_even_spacing() {
        // There is no geometric path from 0, so this must not produce a NaN knob position.
        let v = value_for_stop(7, 0.0, 1.0, false, true);
        assert!(v.is_finite());
        assert!((v - 0.5).abs() < 1e-9, "even spacing, got {v}");
        assert_eq!(stop_for_value(0.5, 0.0, 1.0, false, true), 7);
    }

    #[test]
    fn a_typed_off_stop_value_draws_at_the_nearest_stop_without_being_changed() {
        // The whole point of the "value is free" rule: 0.4271 renders on stop 6 and stays
        // 0.4271. This function only ever reports where to draw.
        assert_eq!(stop_for_value(0.4271, 0.0, 1.0, false, false), 6);
        assert_eq!(stop_for_value(0.0, 0.0, 1.0, false, false), 0);
        assert_eq!(stop_for_value(1.0, 0.0, 1.0, false, false), CELLS - 1);
        // Out of range clamps to an end rather than drawing off the track.
        assert_eq!(stop_for_value(-5.0, 0.0, 1.0, false, false), 0);
        assert_eq!(stop_for_value(99.0, 0.0, 1.0, false, false), CELLS - 1);
    }

    #[test]
    fn a_click_lands_on_the_stop_whose_knob_that_cell_holds() {
        // Round-trip: the cell a stop draws in must click back to that same stop, or the knob
        // would jump on a click that landed exactly on it.
        for stops in [2, 4, 7, CELLS] {
            for stop in 0..stops {
                let cell = cell_for_stop(stop, stops);
                assert_eq!(stop_for_cell(cell, stops), stop, "stops={stops} stop={stop}");
            }
        }
    }

    #[test]
    fn the_track_is_always_one_knob_and_otherwise_dots() {
        for &v in &[0.0, 0.3, 0.5, 1.0] {
            let t = track(v, 0.0, 1.0, false, false);
            assert_eq!(t.chars().count(), CELLS);
            assert_eq!(t.chars().filter(|c| *c == KNOB_CHAR).count(), 1);
        }
    }

    /// Rounding to `DECIMALS` must never make two adjacent stops the same number, or part of
    /// the track would be dead — pressing Right would move the knob and change nothing.
    #[test]
    fn rounding_never_collapses_two_adjacent_stops() {
        // A range whose stops sit exactly at the rounding limit, and one comfortably above it.
        for &(min, max) in &[(0.0, 0.014), (0.0, 1.0), (2.0, 16.0), (20.0, 20000.0)] {
            for exp in [false, true] {
                let stops = stop_count(min, max, false);
                let values: Vec<f64> =
                    (0..stops).map(|s| value_for_stop(s, min, max, false, exp)).collect();
                for pair in values.windows(2) {
                    assert!(
                        pair[1] > pair[0],
                        "[{min}, {max}] exp={exp}: stops collapsed at {pair:?}"
                    );
                }
            }
        }
    }

    /// No stop ever prints more than `DECIMALS` places — the alignment guarantee the params
    /// dialog's column widths are built on.
    #[test]
    fn a_stop_never_prints_more_than_three_decimals() {
        for &(min, max) in &[(0.0, 1.0), (2.0, 16.0), (-1.0, 1.0), (20.0, 20000.0)] {
            for exp in [false, true] {
                for s in 0..stop_count(min, max, false) {
                    let v = value_for_stop(s, min, max, false, exp);
                    let text = format!("{v}");
                    let decimals = text.split('.').nth(1).map(|f| f.len()).unwrap_or(0);
                    assert!(
                        decimals <= DECIMALS as usize,
                        "[{min}, {max}] exp={exp} stop {s} printed {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_spans_reassemble_into_the_track() {
        let v = 0.7;
        let joined: String = spans(v, 0.0, 1.0, false, false, true)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(joined, track(v, 0.0, 1.0, false, false));
    }
}
