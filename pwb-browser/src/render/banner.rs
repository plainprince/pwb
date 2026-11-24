//! In-window banner for uncaught page-script errors.
//!
//! The devtools console records every uncaught error, but the console is
//! closed by default: a broken click handler otherwise looks like a dead
//! button. The pipeline reads the page's last error from the JS DOM handle and
//! paints it here, directly below the browser chrome, so failures are visible
//! without opening devtools. The banner is browser chrome, not page content:
//! it never enters the DOM or the hit-test tree.

use crate::layout::measure_text;
use crate::render::core::RenderState;
use crate::render::text::draw_text_ab_glyph;
use pwb_core::spec::Color;
use tiny_skia::{Color as SkiaColor, Paint, Pixmap, Rect};

/// Longest summary kept before layout. A page can throw an error whose message
/// is megabytes long; measuring every character prefix of that (each measure
/// walks the text) would freeze the UI.
const MAX_MESSAGE_CHARS: usize = 512;

/// Flatten a diagnostic summary onto one line and bound its length. Error
/// messages may contain newlines and tabs (`throw new Error("a\nb")`), which a
/// single-line banner cannot lay out.
fn banner_label(message: &str) -> String {
    let mut label = String::from("JavaScript error: ");
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() > MAX_MESSAGE_CHARS {
        label.extend(normalized.chars().take(MAX_MESSAGE_CHARS));
        label.push('…');
    } else {
        label.push_str(&normalized);
    }
    label
}

/// Longest prefix of `text` that fits `max_width` with an ellipsis. Binary
/// search keeps the number of `measure_text` calls logarithmic in the label
/// length instead of measuring every prefix.
fn fit_text(text: &str, font_size: f32, max_width: f32) -> String {
    if measure_text(text, font_size) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let mut candidate: String = chars[..mid].iter().collect();
        candidate.push('…');
        if measure_text(&candidate, font_size) <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let mut out: String = chars[..low].iter().collect();
    out.push('…');
    out
}

/// Paint the error banner across the window below `top_css` (the chrome
/// height in CSS pixels). Returns its physical-pixel rectangle, or `None`
/// when there is no message or no room below the chrome.
pub fn draw_error_banner(
    pixmap: &mut Pixmap,
    message: &str,
    viewport_w: u32,
    viewport_h: u32,
    top_css: f32,
) -> Option<Rect> {
    let message = message.trim();
    if message.is_empty() {
        return None;
    }
    let (_, css_height) = crate::dpi::logical_size(viewport_w, viewport_h);
    let font_size = crate::dpi::px((css_height as f32 * 0.024).clamp(13.0, 22.0));
    let pad = font_size * 0.6;
    let banner_height = (font_size + pad * 2.0).max(crate::dpi::px(30.0));
    let top = crate::dpi::px(top_css);
    if top >= viewport_h as f32 {
        return None;
    }
    let height = banner_height.min(viewport_h as f32 - top);
    let rect = Rect::from_xywh(0.0, top, viewport_w as f32, height)?;

    let theme = crate::theme::active_theme();
    let background = theme.colors.danger;
    let foreground = crate::render::color::input_auto_text_color(&background);
    let mut paint = Paint::default();
    paint.set_color(SkiaColor::from_rgba8(
        background.r,
        background.g,
        background.b,
        255,
    ));
    pixmap.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);

    // A dim 1px rule separates the banner from the page beneath it.
    if let Some(rule) = Rect::from_xywh(0.0, top + height - 1.0, viewport_w as f32, 1.0) {
        let mut edge = Paint::default();
        let border = theme.colors.border;
        edge.set_color(SkiaColor::from_rgba8(border.r, border.g, border.b, 255));
        pixmap.fill_rect(rule, &edge, tiny_skia::Transform::identity(), None);
    }

    let label = banner_label(message);
    let fitted = fit_text(&label, font_size, viewport_w as f32 - pad * 2.0);
    draw_text_ab_glyph(
        pixmap,
        &fitted,
        pad,
        top + (height - font_size) * 0.5,
        font_size,
        Color {
            r: foreground.r,
            g: foreground.g,
            b: foreground.b,
            a: 255,
        },
        0,
        &crate::render::TextSelection::default(),
        RenderState::identity(),
    );

    Some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_non_background(pixmap: &Pixmap, rect: Rect, background: Color) -> usize {
        let mut count = 0;
        let (x0, y0) = (rect.left() as u32, rect.top() as u32);
        let (x1, y1) = (rect.right() as u32, rect.bottom() as u32 - 1);
        for y in y0..y1 {
            for x in x0..x1 {
                let pixel = pixmap.pixel(x, y).expect("in bounds");
                if (pixel.red(), pixel.green(), pixel.blue())
                    != (background.r, background.g, background.b)
                {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn banner_paints_below_the_chrome_with_visible_text() {
        let background = crate::theme::active_theme().colors.danger;
        let mut pixmap = tiny_skia::Pixmap::new(500, 300).unwrap();
        let rect = draw_error_banner(
            &mut pixmap,
            "TypeError: x is not a function",
            500,
            300,
            60.0,
        )
        .expect("banner is painted");

        assert_eq!(rect.left(), 0.0, "the banner spans the window");
        assert_eq!(rect.width(), 500.0);
        assert!(rect.top() >= 60.0, "below the chrome bar: {:?}", rect);
        assert!(rect.height() > 0.0);
        // Text is rendered inside the bar: at least some pixels differ from
        // the danger-colored background (glyph coverage, antialiasing).
        assert!(
            count_non_background(&pixmap, rect, background) > 20,
            "the message must be visible, not just a colored bar"
        );
    }

    #[test]
    fn long_messages_are_truncated_instead_of_overflowing() {
        let mut pixmap = tiny_skia::Pixmap::new(320, 200).unwrap();
        let message = "TypeError: ".to_string() + &"very long value ".repeat(200);
        let rect =
            draw_error_banner(&mut pixmap, &message, 320, 200, 40.0).expect("banner is painted");
        assert_eq!(rect.width(), 320.0);
        assert!(
            fit_text(&message, 14.0, 300.0).ends_with('…'),
            "an over-wide message is ellipsized"
        );
    }

    #[test]
    fn empty_messages_draw_nothing() {
        let mut pixmap = tiny_skia::Pixmap::new(200, 100).unwrap();
        assert!(draw_error_banner(&mut pixmap, "   ", 200, 100, 30.0).is_none());
    }

    #[test]
    fn multiline_messages_are_flattened_to_one_line() {
        assert_eq!(
            banner_label("Uncaught Error: first\n\tsecond  line"),
            "JavaScript error: Uncaught Error: first second line"
        );
    }

    #[test]
    fn huge_messages_are_capped_before_layout() {
        let message = "x".repeat(100_000);
        let label = banner_label(&message);
        assert_eq!(
            label.chars().count(),
            "JavaScript error: ".chars().count() + MAX_MESSAGE_CHARS + 1,
            "the label is capped and ellipsized"
        );
        assert!(label.ends_with('…'));
        // The fit pass still runs on the bounded label.
        assert!(fit_text(&label, 14.0, 300.0).ends_with('…'));
    }
}
