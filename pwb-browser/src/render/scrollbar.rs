//! Scrollbar rendering for scroll containers.

use tiny_skia::{Color as SkiaColor, Paint, PathBuilder, Rect, Transform};

pub(crate) use crate::render::core::{is_scroll_container, scroll_offset_for};

/// Draw a vertical scrollbar (track + thumb) for a scroll container at its
/// screen rect. The thumb size/position reflect the visible fraction.
pub(crate) fn draw_scrollbar(
    pixmap: &mut tiny_skia::Pixmap,
    rect: Rect,
    content_height: f32,
    scroll_offset: f32,
) {
    let inner_h = rect.height();
    if content_height <= inner_h + 1.0 {
        return;
    }
    let track_w = 8.0;
    let track_x = rect.right() - track_w;
    let track = Rect::from_xywh(track_x, rect.top(), track_w, inner_h).unwrap();

    let mut track_paint = Paint::default();
    track_paint.set_color(SkiaColor::from_rgba8(38, 38, 40, 255));
    let track_path = PathBuilder::from_rect(track);
    pixmap.fill_path(
        &track_path,
        &track_paint,
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );

    let thumb_h = (inner_h * inner_h / content_height).clamp(16.0, inner_h);
    let max_scroll = (content_height - inner_h).max(0.0);
    let frac = if max_scroll > 0.0 {
        (scroll_offset / max_scroll).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb_y = rect.top() + frac * (inner_h - thumb_h);
    let thumb = Rect::from_xywh(track_x, thumb_y, track_w, thumb_h).unwrap();

    let mut thumb_paint = Paint::default();
    thumb_paint.set_color(SkiaColor::from_rgba8(90, 90, 95, 255));
    let thumb_path = PathBuilder::from_rect(thumb);
    pixmap.fill_path(
        &thumb_path,
        &thumb_paint,
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );
}
