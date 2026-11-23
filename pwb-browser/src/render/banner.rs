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

/// Longest prefix kept before the message is truncated with an ellipsis.
fn fit_text(text: &str, font_size: f32, max_width: f32) -> String {
    if measure_text(text, font_size) <= max_width {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push('…');
        if measure_text(&candidate, font_size) > max_width {
            break;
        }
        out.push(ch);
    }
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

    let label = format!("JavaScript error: {message}");
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
}
