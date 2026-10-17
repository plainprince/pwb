//! `Tag::Button`, `Tag::Icon`, and `Tag::Input` rendering.

use crate::layout::{measure_text, LayoutBox};
use crate::render::color::input_auto_text_color;
use crate::render::core::intersect_opt;
use crate::render::icons::cached_icon_path;
use crate::render::text::{caret_text_width, draw_text_ab_glyph, to_skia_color};
use pwb_core::spec::{Color, UnitValue};
use tiny_skia::{Color as SkiaColor, Paint, PathBuilder, Rect, Transform};

use super::render::RenderCtx;

pub(super) fn render_button(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut tiny_skia::Pixmap) {
    let btn_txt = box_node.text.as_deref().unwrap_or("Button");
    // Mirror the layout: buttons size from `font-size`/`padding`.
    let font_size = ctx.resolve(box_node, box_node.style.font_size, UnitValue::Vh(0.025));
    let _pad = ctx.resolve(box_node, box_node.style.padding, UnitValue::Vh(0.0333));

    let mut paint = Paint::default();
    let bg = if box_node.style.background_color != Color::TRANSPARENT {
        box_node.style.background_color
    } else {
        Color {
            r: 166,
            g: 227,
            b: 161,
            a: 255,
        }
    };
    paint.set_color(to_skia_color(bg));

    if let Some(r) = ctx.shifted(box_node.rect) {
        let path = PathBuilder::from_rect(r);
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    let label_color = if box_node.style.text_color != Color::WHITE {
        box_node.style.text_color
    } else {
        Color::BLACK
    };

    // Center text both horizontally and vertically within the button rect
    let text_w = measure_text(btn_txt, font_size);
    let text_left = box_node.rect.left() + (box_node.rect.width() - text_w) / 2.0;
    let text_top = box_node.rect.top() + (box_node.rect.height() - font_size) / 2.0;

    draw_text_ab_glyph(
        pixmap,
        btn_txt,
        text_left,
        text_top,
        font_size,
        label_color,
        box_node.node_id,
        ctx.selection,
        ctx.state,
    );
}

pub(super) fn render_icon(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut tiny_skia::Pixmap) {
    // Feather icons render as strokes (round cap/join) in the element's
    // `color`, scaled from the 24x24 viewBox to the box size.
    let Some(name) = box_node.icon.as_deref() else {
        return;
    };
    let Some(path) = cached_icon_path(name) else {
        return;
    };
    let size = box_node.rect.width().min(box_node.rect.height());
    if size <= 0.0 {
        return;
    }
    let scale = size / 24.0;
    // Center the icon within its box
    let offset_x = (box_node.rect.width() - size) / 2.0;
    let offset_y = (box_node.rect.height() - size) / 2.0;
    let ts = Transform::from_scale(scale, scale).post_translate(
        box_node.rect.left() + ctx.sx + offset_x,
        box_node.rect.top() + ctx.sy + offset_y,
    );
    let stroke = tiny_skia::Stroke {
        width: (2.0 * scale).max(0.5),
        miter_limit: 4.0,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        dash: None,
    };
    let color = if box_node.style.text_color != Color::WHITE {
        box_node.style.text_color
    } else {
        Color {
            r: 166,
            g: 227,
            b: 161,
            a: 255,
        }
    };
    if let Some(stroked) = path.stroke(&stroke, 1.0) {
        if let Some(moved) = stroked.transform(ts) {
            let mut paint = Paint::default();
            paint.set_color(to_skia_color(color));
            // Clip the icon to its box like every other element.
            let box_rect = Rect::from_xywh(
                box_node.rect.left() + ctx.sx,
                box_node.rect.top() + ctx.sy,
                box_node.rect.width(),
                box_node.rect.height(),
            )
            .unwrap();
            let clip_rect = intersect_opt(ctx.clip, box_rect);
            // `moved` is already in screen/pixel space (the box rect +
            // scroll translation were baked in by `ts`), so the fill
            // transform is identity — otherwise the path is scaled and
            // shifted a second time and the icon lands off-position.
            if let Some(r) = clip_rect {
                let mut mask = tiny_skia::Mask::new(pixmap.width(), pixmap.height()).unwrap();
                let cp = PathBuilder::from_rect(r);
                mask.fill_path(
                    &cp,
                    tiny_skia::FillRule::Winding,
                    false,
                    Transform::identity(),
                );
                pixmap.fill_path(
                    &moved,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    Some(&mask),
                );
            } else {
                pixmap.fill_path(
                    &moved,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
    }
}

pub(super) fn render_input(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut tiny_skia::Pixmap) {
    if let Some(r) = ctx.shifted(box_node.rect) {
        let border_color = SkiaColor::from_rgba8(120, 120, 130, 255);
        let bg = if box_node.style.background_color != Color::TRANSPARENT {
            to_skia_color(box_node.style.background_color)
        } else {
            SkiaColor::from_rgba8(250, 250, 252, 255)
        };
        let mut paint = Paint::default();
        paint.set_color(border_color);
        pixmap.fill_rect(r, &paint, Transform::identity(), None);
        // Inner fill inset by 1px = the border.
        let inner = Rect::from_xywh(
            r.left() + 1.0,
            r.top() + 1.0,
            (r.width() - 2.0).max(1.0),
            (r.height() - 2.0).max(1.0),
        )
        .unwrap();
        paint.set_color(bg);
        pixmap.fill_rect(inner, &paint, Transform::identity(), None);

        // Value (or gray placeholder) drawn inside, clipped to the box so
        // long values scroll visually instead of overflowing the field.
        let font_size = ctx.resolve(box_node, box_node.style.font_size, UnitValue::Vh(0.025));
        let pad = ctx.resolve(box_node, box_node.style.padding, UnitValue::Vh(0.012));
        let value = box_node.value.as_deref().unwrap_or("");
        let placeholder = box_node.placeholder.as_deref().unwrap_or("");
        let is_focused = ctx
            .input_focus
            .map(|(id, _)| id == box_node.node_id)
            .unwrap_or(false);

        let mut text_state = ctx.state;
        text_state.clip = intersect_opt(text_state.clip, inner);
        let (text, color) = if !value.is_empty() {
            let bg_color = Color {
                r: (bg.red() * 255.0) as u8,
                g: (bg.green() * 255.0) as u8,
                b: (bg.blue() * 255.0) as u8,
                a: 255,
            };
            let c = if box_node.style.text_color != Color::WHITE {
                box_node.style.text_color
            } else {
                input_auto_text_color(&bg_color)
            };
            (value, c)
        } else if !placeholder.is_empty() {
            (
                placeholder,
                Color {
                    r: 130,
                    g: 130,
                    b: 140,
                    a: 255,
                },
            )
        } else {
            ("", box_node.style.text_color)
        };
        let text_left = r.left() + pad + 1.0;
        let text_top = r.top() + pad;
        if !text.is_empty() {
            draw_text_ab_glyph(
                pixmap,
                text,
                text_left,
                text_top,
                font_size,
                color,
                box_node.node_id,
                ctx.selection,
                text_state,
            );
        }
        // Caret: a 2px bar at the caret index (blinking is skipped to
        // avoid a per-frame timer waking the app).
        if is_focused {
            let caret = ctx.input_focus.map(|(_, c)| c).unwrap_or(0);
            let caret_x = text_left + caret_text_width(value, font_size, caret);
            let caret_h = (font_size * 1.2).max(8.0);
            let caret_top = r.top() + ((r.height() - caret_h) / 2.0).max(0.0);
            let caret_rect = Rect::from_xywh(caret_x, caret_top, 2.0, caret_h).unwrap();
            let mut paint = Paint::default();
            let bg_color = Color {
                r: (bg.red() * 255.0) as u8,
                g: (bg.green() * 255.0) as u8,
                b: (bg.blue() * 255.0) as u8,
                a: 255,
            };
            paint.set_color(to_skia_color(input_auto_text_color(&bg_color)));
            if let Some(visible) = intersect_opt(text_state.clip, caret_rect) {
                pixmap.fill_rect(visible, &paint, Transform::identity(), None);
            }
        }
    }
}
