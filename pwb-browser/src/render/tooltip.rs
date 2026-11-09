//! Tooltip rendering for hovered elements with a `title` attribute.

use crate::layout::measure_text;
use crate::render::core::RenderState;
use crate::render::text::draw_text_ab_glyph;
use pwb_core::spec::Color;
use tiny_skia::{Color as SkiaColor, Paint, Pixmap, Rect};

/// Draw a small tooltip box near the mouse when the hovered element has a
/// `title` attribute.  Renders last (on top of everything).
pub fn draw_tooltip(
    pixmap: &mut Pixmap,
    title: &str,
    mx: f32,
    my: f32,
    viewport_w: u32,
    viewport_h: u32,
) {
    if title.is_empty() {
        return;
    }
    // The tooltip is browser chrome: a constant size in CSS pixels that is
    // rasterized at the display scale, so it stays crisp rather than growing
    // with (or staying tiny on) a high-density viewport.
    let (_, css_height) = crate::dpi::logical_size(viewport_w, viewport_h);
    let font_size = crate::dpi::px((css_height as f32 * 0.03).clamp(16.0, 34.0));
    let text_w = measure_text(title, font_size);
    let pad = font_size * 0.5;
    let tip_w = (text_w + pad * 2.0).min(viewport_w as f32 - crate::dpi::px(20.0));
    let tip_h = font_size + pad * 2.0;
    // Position below and to the right of the cursor, clamped to the viewport.
    let mut tip_x = mx + crate::dpi::px(12.0);
    let mut tip_y = my + crate::dpi::px(20.0);
    let margin = crate::dpi::px(4.0);
    if tip_x + tip_w > viewport_w as f32 - margin {
        tip_x = (viewport_w as f32 - tip_w - margin).max(margin);
    }
    if tip_y + tip_h > viewport_h as f32 - margin {
        tip_y = (my - tip_h - margin).max(margin);
    }
    let tip_rect = Rect::from_xywh(tip_x, tip_y, tip_w, tip_h).unwrap();
    // Background.
    {
        let mut paint = Paint::default();
        paint.set_color(SkiaColor::from_rgba8(40, 40, 40, 240));
        pixmap.fill_rect(tip_rect, &paint, tiny_skia::Transform::identity(), None);
    }
    // Border (four 1px rects — tiny_skia Paint has no stroke mode).
    {
        let mut paint = Paint::default();
        paint.set_color(SkiaColor::from_rgba8(80, 80, 80, 255));
        let t = 1.0;
        // top
        if let Some(r) = Rect::from_xywh(tip_x, tip_y, tip_w, t) {
            pixmap.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
        }
        // bottom
        if let Some(r) = Rect::from_xywh(tip_x, tip_y + tip_h - t, tip_w, t) {
            pixmap.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
        }
        // left
        if let Some(r) = Rect::from_xywh(tip_x, tip_y, t, tip_h) {
            pixmap.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
        }
        // right
        if let Some(r) = Rect::from_xywh(tip_x + tip_w - t, tip_y, t, tip_h) {
            pixmap.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
        }
    }
    // Text.
    draw_text_ab_glyph(
        pixmap,
        title,
        tip_x + pad,
        tip_y + pad,
        font_size,
        Color {
            r: 230,
            g: 230,
            b: 230,
            a: 255,
        },
        0,
        &crate::render::TextSelection::default(),
        RenderState::identity(),
    );
}
