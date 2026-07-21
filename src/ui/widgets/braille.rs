//! Shared braille dot-matrix glyph primitives — used by `waveform::WaveformWidget::render_dots`
//! and the CDP breakpoint-envelope editor's text-mode curve (`ui::app::render_cdp_envelope_editor`)
//! so both get the same 2 (wide) x 4 (tall) sub-cell resolution from one definition instead of
//! two copies drifting apart.

/// Braille Patterns block base codepoint (U+2800). Each cell is a 2 (wide) x 4 (tall)
/// sub-grid of dots; OR-ing together the bits for whichever dots are "on" and adding that
/// mask to this base yields the glyph. Bit layout is the standard braille dot numbering:
/// ```text
///   dot1 (0x01)  dot4 (0x08)
///   dot2 (0x02)  dot5 (0x10)
///   dot3 (0x04)  dot6 (0x20)
///   dot7 (0x40)  dot8 (0x80)
/// ```
const BRAILLE_BASE: u32 = 0x2800;

/// `DOT_BITS[sub_row][sub_col]` — the bit for the dot at vertical quarter `sub_row` (0=top
/// .. 3=bottom) and horizontal half `sub_col` (0=left, 1=right) of one terminal cell.
pub const DOT_BITS: [[u8; 2]; 4] = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
];

pub fn braille_char(mask: u8) -> char {
    char::from_u32(BRAILLE_BASE + mask as u32).unwrap_or(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_char_covers_empty_and_full_masks() {
        assert_eq!(braille_char(0x00), '\u{2800}');
        assert_eq!(braille_char(0xff), '\u{28ff}');
        assert_eq!(braille_char(0x01), '\u{2801}');
    }
}
