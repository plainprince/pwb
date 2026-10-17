//! Color contrast and luminance helpers.

use pwb_core::spec::Color;

/// Relative luminance (simplified sRGB). Returns 0.0 (black) – 1.0 (white).
pub(crate) fn srgb_luminance(c: &Color) -> f32 {
    let r = c.r as f32 / 255.0;
    let g = c.g as f32 / 255.0;
    let b = c.b as f32 / 255.0;
    // Rec. 709 coefficients.
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Pick black or white text to contrast against `bg`.
pub(crate) fn input_auto_text_color(bg: &Color) -> Color {
    if srgb_luminance(bg) > 0.4 {
        Color {
            r: 17,
            g: 17,
            b: 17,
            a: 255,
        } // dark text on light bg
    } else {
        Color {
            r: 240,
            g: 240,
            b: 245,
            a: 255,
        } // light text on dark bg
    }
}
