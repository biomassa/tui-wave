//! Turning the PNG a Praat driver saved into something worth putting on screen.
//!
//! Praat's Picture window is a 12x12-inch canvas and `Save as 300-dpi PNG file:` writes all of
//! it, so what comes back is always 3600x3600 — about 52 MB once decoded to RGBA — of which the
//! drawing typically occupies a corner (measured across the plugin: roughly 2300x1700, though
//! some run out to ~3560 wide). Two things follow, and they are the whole reason this module
//! exists rather than the UI decoding the bytes directly:
//!
//! * **Crop, then downscale.** Blitting the raw canvas would letterbox the figure into a corner
//!   of the popup and waste most of the terminal's pixels on white. Cropping to the drawn
//!   content and then fitting it to [`MAX_DIMENSION`] leaves ~5 MB resident instead of ~52.
//! * **It happens on the runner's worker thread.** Decoding 12.9 million pixels takes long
//!   enough to be visible as a dropped frame, and the render loop is the one place that must not
//!   pay for it. Only the small image crosses the channel to the UI.
//!
//! The blank check falls out of the crop for free, and is load-bearing: a run with no drawing
//! toggle still leaves a *uniformly white* canvas, and `model::praat::plan::draws_picture` is
//! deliberately over-inclusive about which toggles it thinks draw. `None` from
//! [`content_bounds`] is what turns "asked to draw but didn't" into "no picture" rather than
//! into a popup full of nothing.
//!
//! This lives in `src/praat/` rather than `src/ui/` for the threading reason above, and rather
//! than `src/model/` because it is part of running a job. `image` is a decoder, not a terminal
//! dependency, so nothing here weakens the model layer's isolation.

use image::{imageops, RgbaImage};

/// Longest side of the image handed to the UI.
///
/// Sized against the *popup*, not against a guess at what looks like enough. A maximised
/// terminal on a 4K display is around 3800 px across, and these figures are full of hairline
/// strokes and 7-point annotations — anything the blit has to upscale is detail thrown away.
/// 2400 costs at most 23 MB of RGBA (typically about half that, since the drawings are wider
/// than they are tall) and is within a third of Praat's own 3600 px canvas.
///
/// An earlier 1200 was chosen against the size of a *cell* rather than a screen, and produced
/// the reported bug where a figure sat at native size in the corner of a much larger popup —
/// though the proximate cause of that was `Resize::Fit`, which never upscales; see the blit in
/// `App::render`.
pub const MAX_DIMENSION: u32 = 2400;

/// Margin kept around the drawn content, in source pixels (~1/75 inch at 300 dpi). Cropping
/// exactly to the ink puts axis labels and the outermost strokes flush against the popup
/// border, which reads as a rendering fault rather than as a tight crop.
const PADDING: u32 = 24;

/// Everything at or above this on all three channels counts as page white. Praat renders the
/// canvas as pure `#FFFFFF` and antialiases against it, so the threshold only has to survive
/// PNG's lossless round-trip — the slack is for a future format, not for the current one.
const WHITE: u8 = 250;

/// Decode a Praat-written PNG into an image ready to blit: cropped to what was actually drawn
/// and scaled to fit [`MAX_DIMENSION`].
///
/// `None` for anything not worth showing — bytes that do not decode, and, importantly, a canvas
/// that is uniformly blank. Every caller treats that as "this run produced no picture", which is
/// exactly right: a job whose audio succeeded must never be failed over its drawing.
pub fn decode_for_display(png: &[u8]) -> Option<RgbaImage> {
    let image = image::load_from_memory(png).ok()?.to_rgba8();
    let (left, top, right, bottom) = content_bounds(&image)?;

    let width = right - left;
    let height = bottom - top;
    let cropped = imageops::crop_imm(&image, left, top, width, height).to_image();

    let longest = width.max(height);
    if longest <= MAX_DIMENSION {
        return Some(cropped);
    }
    // Integer-scale both sides by the same ratio so the aspect survives; `.max(1)` because a
    // very wide, very short figure would otherwise round its short side to zero, and `resize`
    // to a zero dimension yields an empty image rather than an error.
    let scale = f64::from(MAX_DIMENSION) / f64::from(longest);
    let target_w = ((f64::from(width) * scale).round() as u32).max(1);
    let target_h = ((f64::from(height) * scale).round() as u32).max(1);
    // Lanczos3 rather than the `Nearest` a plain resize would use: these figures are built from
    // hairline strokes and 7-point text, and point-sampling a 3x reduction erases both.
    Some(imageops::resize(&cropped, target_w, target_h, imageops::FilterType::Lanczos3))
}

/// Half-open bounding box (`left`, `top`, `right`, `bottom`) of everything that is not page
/// white, or `None` when there is nothing — which is how a blank canvas is detected.
fn content_bounds(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (mut left, mut top) = (u32::MAX, u32::MAX);
    let (mut right, mut bottom) = (0u32, 0u32);
    for (x, y, pixel) in image.enumerate_pixels() {
        if is_background(pixel) {
            continue;
        }
        left = left.min(x);
        top = top.min(y);
        right = right.max(x + 1);
        bottom = bottom.max(y + 1);
    }
    if left == u32::MAX {
        return None;
    }
    let (w, h) = image.dimensions();
    Some((
        left.saturating_sub(PADDING),
        top.saturating_sub(PADDING),
        (right + PADDING).min(w),
        (bottom + PADDING).min(h),
    ))
}

/// Page white, or anything fully transparent. Praat writes RGB PNGs so the alpha arm never
/// fires today; it is there because a transparent margin would otherwise read as content and
/// defeat the crop entirely.
fn is_background(pixel: &image::Rgba<u8>) -> bool {
    let [r, g, b, a] = pixel.0;
    a == 0 || (r >= WHITE && g >= WHITE && b >= WHITE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn white(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]))
    }

    fn as_png(image: &RgbaImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encode");
        bytes
    }

    /// The case that makes the over-inclusive toggle match safe: Praat saves a full white canvas
    /// whenever the script drew nothing, and that must become "no picture", not an empty popup.
    #[test]
    fn a_blank_canvas_is_not_a_picture() {
        assert!(decode_for_display(&as_png(&white(400, 400))).is_none());
    }

    #[test]
    fn undecodable_bytes_are_not_a_picture() {
        assert!(decode_for_display(b"not a png at all").is_none());
        assert!(decode_for_display(&[]).is_none());
    }

    /// The crop is what stops a figure occupying a fifth of Praat's canvas from being blitted as
    /// a speck in the corner of the popup.
    #[test]
    fn the_white_canvas_around_the_drawing_is_cropped_away() {
        let mut canvas = white(1000, 1000);
        for y in 400..500 {
            for x in 300..600 {
                canvas.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let out = decode_for_display(&as_png(&canvas)).expect("a picture");
        // 300x100 of ink, plus PADDING on all four sides.
        assert_eq!(out.dimensions(), (300 + 2 * PADDING, 100 + 2 * PADDING));
    }

    /// Ink flush against the canvas edge must not push the crop out of bounds.
    #[test]
    fn content_touching_the_edge_clamps_rather_than_overflowing() {
        let mut canvas = white(200, 200);
        for y in 0..200 {
            canvas.put_pixel(0, y, Rgba([0, 0, 0, 255]));
            canvas.put_pixel(199, y, Rgba([0, 0, 0, 255]));
        }
        let out = decode_for_display(&as_png(&canvas)).expect("a picture");
        assert_eq!(out.dimensions(), (200, 200));
    }

    /// A real Praat canvas is 3600x3600; what reaches the UI must be bounded regardless.
    #[test]
    fn an_oversized_drawing_is_scaled_down_with_its_aspect_intact() {
        let mut canvas = white(3000, 1500);
        for y in 0..1500 {
            for x in 0..3000 {
                if (x + y) % 7 == 0 {
                    canvas.put_pixel(x, y, Rgba([20, 40, 60, 255]));
                }
            }
        }
        let (width, height) = decode_for_display(&as_png(&canvas)).expect("a picture").dimensions();
        assert!(width <= MAX_DIMENSION && height <= MAX_DIMENSION, "{width}x{height}");
        assert_eq!(width, MAX_DIMENSION);
        // 2:1 in, 2:1 out (within a pixel of rounding).
        assert!(height.abs_diff(MAX_DIMENSION / 2) <= 1, "aspect changed: {width}x{height}");
    }

    /// A drawing already smaller than the cap is passed through untouched rather than upscaled
    /// — the blit letterboxes it, which beats inventing pixels.
    #[test]
    fn a_small_drawing_is_not_upscaled() {
        let mut canvas = white(300, 300);
        canvas.put_pixel(150, 150, Rgba([0, 0, 0, 255]));
        let out = decode_for_display(&as_png(&canvas)).expect("a picture");
        assert_eq!(out.dimensions(), (1 + 2 * PADDING, 1 + 2 * PADDING));
    }

    /// Antialiased near-white must not be mistaken for the page, or a light-grey panel fill
    /// would crop away the panel it fills.
    #[test]
    fn a_light_grey_fill_counts_as_content() {
        assert!(!is_background(&Rgba([245, 245, 248, 255])));
        assert!(is_background(&Rgba([252, 255, 251, 255])));
        assert!(is_background(&Rgba([0, 0, 0, 0])));
    }
}
