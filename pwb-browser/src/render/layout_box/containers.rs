//! `Tag::View` rendering: background fill and CSS border lines.

use crate::layout::LayoutBox;
use crate::render::core::intersect_opt;
use crate::render::text::to_skia_color;
use pwb_core::spec::Color;
use tiny_skia::{Paint, PathBuilder, Rect, Transform};

use super::render::RenderCtx;

pub(super) fn render_view(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut tiny_skia::Pixmap) {
    if box_node.style.background_color != Color::TRANSPARENT {
        if let Some(r) = ctx.shifted(box_node.rect) {
            let mut paint = Paint::default();
            paint.set_color(to_skia_color(box_node.style.background_color));
            let path = PathBuilder::from_rect(r);
            pixmap.fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    // `border` (CSS) draws thin lines on the box rect edges — the
    // separators between devtools sections etc. Widths are exact pixels;
    // each side draws only when its width is non-zero. The box edges are
    // snapped to integer pixels so 1px lines render crisp instead of
    // antialiasing into a 2px blur (boxes can end on fractional edges).
    let bs = &box_node.style;
    if bs.border_top > 0.0
        || bs.border_right > 0.0
        || bs.border_bottom > 0.0
        || bs.border_left > 0.0
    {
        let (l, t, w, h) = (
            (box_node.rect.left() + ctx.sx).round(),
            (box_node.rect.top() + ctx.sy).round(),
            box_node.rect.width().round(),
            box_node.rect.height().round(),
        );
        let mut paint = Paint::default();
        paint.set_color(to_skia_color(bs.border_color));
        for r in [
            Rect::from_xywh(l, t, w, bs.border_top),
            Rect::from_xywh(l, t + h - bs.border_bottom, w, bs.border_bottom),
            Rect::from_xywh(l, t, bs.border_left, h),
            Rect::from_xywh(l + w - bs.border_right, t, bs.border_right, h),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(visible) = intersect_opt(ctx.clip, r) {
                let path = PathBuilder::from_rect(visible);
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
    }
}
