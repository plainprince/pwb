//! Rounded-rectangle paths for themed surfaces (tabs, buttons, fields).

use pwb_core::spec::Color;
use tiny_skia::{Paint, Path, PathBuilder, Pixmap, Rect, Transform};

use crate::render::text::to_skia_color;

/// Build a rounded-rect path. `radius` is clamped to half the smaller side;
/// values below 0.5px produce a plain rectangle.
pub(crate) fn rounded_rect_path(rect: Rect, radius: f32) -> Path {
    let r = radius
        .max(0.0)
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0);
    if r <= 0.5 {
        return PathBuilder::from_rect(rect);
    }
    let (l, t, right, bottom) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let k = r * 0.552_285;
    let mut pb = PathBuilder::new();
    pb.move_to(l + r, t);
    pb.line_to(right - r, t);
    pb.cubic_to(right - r + k, t, right, t + r - k, right, t + r);
    pb.line_to(right, bottom - r);
    pb.cubic_to(
        right,
        bottom - r + k,
        right - r + k,
        bottom,
        right - r,
        bottom,
    );
    pb.line_to(l + r, bottom);
    pb.cubic_to(l + r - k, bottom, l, bottom - r + k, l, bottom - r);
    pb.line_to(l, t + r);
    pb.cubic_to(l, t + r - k, l + r - k, t, l + r, t);
    pb.close();
    pb.finish().unwrap_or_else(|| PathBuilder::from_rect(rect))
}

/// Fill `rect` with `color`, rounding the corners when `radius > 0`.
pub(crate) fn fill_rounded_rect(pixmap: &mut Pixmap, rect: Rect, radius: f32, color: Color) {
    let path = rounded_rect_path(rect, radius);
    let mut paint = Paint::default();
    paint.set_color(to_skia_color(color));
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );
}
