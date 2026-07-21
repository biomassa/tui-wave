//! Catppuccin Mocha palette, restricted to the handful of colors actually used and given
//! semantic names at the bottom — change a role's color here rather than touching the
//! individual hex values scattered across widgets.

use ratatui::style::Color;

pub const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
pub const SURFACE0: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const SURFACE1: Color = Color::Rgb(0x45, 0x47, 0x5a);
pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);
pub const SUBTEXT1: Color = Color::Rgb(0xba, 0xc2, 0xde);
/// Catppuccin Mocha's "Overlay0" — visibly dimmer than `SUBTEXT0`, for de-emphasized
/// annotations that should read as a step below regular muted text, not just a slightly
/// duller shade of it.
pub const OVERLAY0: Color = Color::Rgb(0x6c, 0x70, 0x86);
pub const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
pub const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
pub const SKY: Color = Color::Rgb(0x89, 0xdc, 0xeb);
pub const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
pub const LAVENDER: Color = Color::Rgb(0xb4, 0xbe, 0xfe);

/// Normal (unselected) waveform fill.
pub const WAVEFORM: Color = SKY;
/// Waveform fill within the active selection range. Dark (BASE) for maximum contrast
/// against the SKY selection background — the inverted pair reads clearly.
pub const WAVEFORM_SELECTED: Color = BASE;
/// The cursor marker (insertion point / playback start).
pub const CURSOR: Color = YELLOW;
/// The playhead marker (current playback position, only visible during playback).
pub const PLAYHEAD: Color = Color::Rgb(0xff, 0xff, 0xff);
/// dB scale gutter labels.
pub const DB_SCALE: Color = SUBTEXT0;
/// Timeline markers (cue points) — vertical line and label.
pub const MARKER: Color = MAUVE;
/// Window/pane borders and titles.
pub const BORDER: Color = LAVENDER;
/// Border accent for the focused panel (file list, buffers, or the waveform when active).
pub const FOCUS: Color = PEACH;
/// The unsaved-changes "*" in the title bar.
pub const DIRTY: Color = RED;
/// Keyboard shortcut hints in the menu and toolbar — a distinct accent from the action
/// labels they're attached to, so a shortcut always reads as "this is the key," not part
/// of the label.
pub const SHORTCUT: Color = PEACH;
/// Active / enabled toggle state in the toolbar.
pub const ACTIVE: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
/// Toolbar section labels (EDIT:, VIEW:, …) — same hue as the selected-menu highlight
/// (`HIGHLIGHT_BG`), so the panel's section accents and the menu selection read as one accent.
pub const TOOLBAR_GROUP: Color = HIGHLIGHT_BG;
/// Default text/background for the menu bar and toolbar chrome.
pub const CHROME_FG: Color = TEXT;
pub const CHROME_BG: Color = SURFACE0;
/// The currently open menu / highlighted entry.
pub const HIGHLIGHT_FG: Color = BASE;
pub const HIGHLIGHT_BG: Color = MAUVE;
/// Status bar.
pub const STATUS_FG: Color = SUBTEXT1;
pub const STATUS_BG: Color = SURFACE0;
/// Quit-confirmation warning modal.
pub const WARNING_FG: Color = PEACH;
pub const WARNING_BG: Color = SURFACE1;
/// Inline heads-up annotations that should read as a step below regular muted text (e.g.
/// the CDP browser's ">1 inputs" note) — deliberately dimmer than `SUBTEXT0`/`DB_SCALE`,
/// which are for text that's still meant to be read at a glance.
pub const ANNOTATION: Color = OVERLAY0;

/// Dot-matrix waveform gradient stops (quiet -> loud): green -> yellow -> red, echoing
/// btop's height-graded braille graphs, where dots near 0 dBFS read as "louder" than dots
/// near the centerline. Used by both `waveform::WaveformWidget` (character glyphs) and
/// `waveform_image::rasterize_waveform` (graphics-mode bitmap) when dot-matrix mode is
/// enabled — the eighth-block renderer, and dot-matrix mode with gradient off, stay
/// flat-colored at `WAVEFORM_DOT_LOW`. Graded by dB (via `dsp::linear_to_db`, see
/// `gradient_color`), not raw linear position — most of a waveform's on-screen height is
/// quiet in linear terms, so a linear-position gradient was nearly invisible in practice.
pub const WAVEFORM_DOT_LOW: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const WAVEFORM_DOT_MID: Color = YELLOW;
pub const WAVEFORM_DOT_HIGH: Color = RED;

/// Below this dB level, [`gradient_color`] returns `WAVEFORM_DOT_LOW` outright (no further
/// gradation) — a linear-amplitude gradient was nearly invisible in practice, since most of
/// a typical waveform's on-screen height sits at very quiet linear amplitudes even though it
/// spans a wide, perceptually significant dB range (e.g. -18dB is already 12.5% of
/// full-scale linearly). Chosen to roughly match the deepest dB gutter mark normally visible
/// (`db_scale::DB_MARKS`), so the gradient's full range lines up with what's shown.
pub const GRADIENT_FLOOR_DB: f32 = -30.0;

/// dB level of the gradient's middle stop (`WAVEFORM_DOT_MID`, yellow) — the
/// green→yellow→red ramp is two separate lerps meeting here, not one lerp across the full
/// range, so yellow actually lands at -6dB instead of being skipped over on the way from
/// green to red.
pub const GRADIENT_MID_DB: f32 = -6.0;

/// Maps a dB level to a point along the green (`GRADIENT_FLOOR_DB`) -> yellow
/// (`GRADIENT_MID_DB`) -> red (0dB) dot-matrix gradient.
pub fn gradient_color(db: f32) -> Color {
    if db >= GRADIENT_MID_DB {
        let t = ((db - GRADIENT_MID_DB) / -GRADIENT_MID_DB).clamp(0.0, 1.0);
        lerp_color(WAVEFORM_DOT_MID, WAVEFORM_DOT_HIGH, t)
    } else {
        let t = ((db - GRADIENT_FLOOR_DB) / (GRADIENT_MID_DB - GRADIENT_FLOOR_DB)).clamp(0.0, 1.0);
        lerp_color(WAVEFORM_DOT_LOW, WAVEFORM_DOT_MID, t)
    }
}

/// Formant-heatmap gradient stops (quiet -> loud): dark surface -> mauve -> red -> peach ->
/// yellow, styled after the magma/inferno colormap family real spectrogram/heatmap tools
/// use — a wide hue *and* lightness sweep, not just a hue sweep between similarly-light
/// pastels. Deliberately a different palette from `WAVEFORM_DOT_LOW/MID/HIGH`'s green→
/// yellow→red (user report, 2026-07-21: "this is barely visible" — a formant heatmap packs
/// far more distinct cells on screen at once than a waveform's dot trace does, and green and
/// yellow are close enough in lightness in this pastel palette that adjacent heatmap cells
/// were hard to tell apart; the waveform gradient's own green floor never has this problem
/// since it's read as one continuous trace, not a grid of individually-compared cells).
/// Starting from `SURFACE1` (close to the popup's own background) rather than a bright color
/// also gives "quiet" a genuinely low-contrast reading — "barely there" — freeing the rest
/// of the range for real contrast among everything louder.
const FORMANT_GRADIENT_STOPS: [Color; 5] = [SURFACE1, MAUVE, RED, PEACH, YELLOW];

/// Maps `t` (a plain `[0, 1]` fraction — the caller owns whatever domain-specific
/// normalization produced it, e.g. `App::formant_db_range`'s per-buffer dB range) to a point
/// along `FORMANT_GRADIENT_STOPS`. Piecewise-linear across equally-spaced stops, the same
/// "lerp between the two neighboring stops" approach `gradient_color` uses for its own three
/// stops, generalized to however many `FORMANT_GRADIENT_STOPS` has.
pub fn formant_gradient_color(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let segments = FORMANT_GRADIENT_STOPS.len() - 1;
    let scaled = t * segments as f32;
    let idx = (scaled as usize).min(segments - 1);
    let local_t = scaled - idx as f32;
    lerp_color(FORMANT_GRADIENT_STOPS[idx], FORMANT_GRADIENT_STOPS[idx + 1], local_t)
}

/// Background fill for a selected column/span in dot-matrix mode: `WAVEFORM_DOT_LOW` dimmed
/// toward `BASE`. A full-pane fill of the gradient's own saturated green reads far brighter
/// than the same color used sparingly on individual dots (green dominates perceived
/// luminance more than the bars renderer's pastel sky-blue selection does at the same
/// saturation), so it's toned down to read as "selection," not a wall of neon green.
pub fn dot_matrix_selection_bg() -> Color {
    lerp_color(BASE, WAVEFORM_DOT_LOW, 0.45)
}

/// Linearly interpolates two `Color::Rgb` values by `t` (clamped to `[0.0, 1.0]`). Every
/// color in this module is `Color::Rgb`, so the non-RGB branch is unreachable in practice —
/// it falls back to `a` rather than panicking, since a themed color is never worth crashing
/// the renderer over.
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return a;
    };
    let lerp = |x: u8, y: u8| -> u8 { (x as f32 + (y as f32 - x as f32) * t).round() as u8 };
    Color::Rgb(lerp(ar, br), lerp(ag, bg), lerp(ab, bb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_color_hits_both_endpoints_and_the_midpoint() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(100, 200, 50);
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
        assert_eq!(lerp_color(a, b, 0.5), Color::Rgb(50, 100, 25));
    }

    #[test]
    fn lerp_color_clamps_out_of_range_t() {
        let a = Color::Rgb(10, 10, 10);
        let b = Color::Rgb(20, 20, 20);
        assert_eq!(lerp_color(a, b, -1.0), a);
        assert_eq!(lerp_color(a, b, 2.0), b);
    }

    #[test]
    fn formant_gradient_color_hits_the_first_and_last_stops_at_the_endpoints() {
        assert_eq!(formant_gradient_color(0.0), FORMANT_GRADIENT_STOPS[0]);
        assert_eq!(formant_gradient_color(1.0), FORMANT_GRADIENT_STOPS[FORMANT_GRADIENT_STOPS.len() - 1]);
    }

    #[test]
    fn formant_gradient_color_clamps_out_of_range_t() {
        assert_eq!(formant_gradient_color(-1.0), FORMANT_GRADIENT_STOPS[0]);
        assert_eq!(formant_gradient_color(2.0), FORMANT_GRADIENT_STOPS[FORMANT_GRADIENT_STOPS.len() - 1]);
    }

    /// Every consecutive pair of stops must differ enough to actually read as distinct
    /// colors on screen — regression guard for the original bug this palette replaced
    /// (`WAVEFORM_DOT_LOW`/`WAVEFORM_DOT_MID` were both light pastels, "barely visible" next
    /// to each other in a densely-packed heatmap).
    #[test]
    fn formant_gradient_stops_are_perceptually_distinct_from_their_neighbors() {
        for pair in FORMANT_GRADIENT_STOPS.windows(2) {
            let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (pair[0], pair[1]) else { panic!("expected Rgb") };
            let dist_sq = (ar as i32 - br as i32).pow(2) + (ag as i32 - bg as i32).pow(2) + (ab as i32 - bb as i32).pow(2);
            assert!(dist_sq > 40i32.pow(2), "stops {:?} and {:?} are too close together", pair[0], pair[1]);
        }
    }
}
