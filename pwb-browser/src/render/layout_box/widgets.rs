//! `Tag::Button`, `Tag::Icon`, `Tag::Input`, and `Tag::Image` rendering.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::layout::{measure_text, LayoutBox};
use crate::render::color::input_auto_text_color;
use crate::render::core::intersect_opt;
use crate::render::icons::cached_icon_path;
use crate::render::text::{caret_text_width, draw_text_ab_glyph, to_skia_color};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use pwb_core::spec::{Color, UnitValue};
use tiny_skia::{
    Color as SkiaColor, FilterQuality, Paint, PathBuilder, Pattern, Pixmap, Rect, SpreadMode,
    Transform,
};

use super::render::RenderCtx;

/// Decoded images keyed by `src`, so scrolling/repainting a page does not
/// re-decode the same PNG every frame.
fn image_cache() -> &'static Mutex<HashMap<String, Option<Arc<Pixmap>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Pixmap>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether an image source is acceptable: local paths and `file://` URLs for
/// on-disk PNGs, plus `data:image/png;base64,...` for bundles that embed the
/// bitmap inline. Remote `http(s)` sources are not fetched by the renderer.
fn decode_image(src: &str) -> Option<Pixmap> {
    if let Some(rest) = src.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        if !meta.ends_with(";base64") {
            return None;
        }
        let bytes = BASE64.decode(data).ok()?;
        return Pixmap::decode_png(&bytes).ok();
    }
    let path = src.strip_prefix("file://").unwrap_or(src);
    Pixmap::load_png(path).ok()
}

fn load_image(src: &str) -> Option<Arc<Pixmap>> {
    let mut cache = image_cache().lock().unwrap();
    if let Some(cached) = cache.get(src) {
        return cached.clone();
    }
    let decoded = decode_image(src).map(Arc::new);
    cache.insert(src.to_string(), decoded.clone());
    decoded
}

/// Paint a `<image>`: the decoded PNG is scaled (bilinear) to the box rect, so
/// `width`/`height` attributes and CSS sizing behave like a real `<img>`. A
/// missing or unreadable source paints a neutral placeholder instead of
/// leaving a hole in the layout.
pub(super) fn render_image(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut Pixmap) {
    let Some(src) = box_node.image_src.as_deref().filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(visible) = ctx.shifted(box_node.rect) else {
        return;
    };
    let Some(image) = load_image(src) else {
        let mut paint = Paint::default();
        paint.set_color(SkiaColor::from_rgba8(38, 38, 50, 255));
        pixmap.fill_rect(visible, &paint, Transform::identity(), None);
        return;
    };
    let (image_w, image_h) = (image.width() as f32, image.height() as f32);
    if image_w <= 0.0 || image_h <= 0.0 {
        return;
    }
    let scale_x = box_node.rect.width() / image_w;
    let scale_y = box_node.rect.height() / image_h;
    let origin_x = box_node.rect.left() + ctx.sx;
    let origin_y = box_node.rect.top() + ctx.sy;
    let paint = Paint {
        shader: Pattern::new(
            image.as_ref().as_ref(),
            SpreadMode::Pad,
            FilterQuality::Bilinear,
            1.0,
            Transform::from_scale(scale_x, scale_y).post_translate(origin_x, origin_y),
        ),
        ..Default::default()
    };
    // `visible` is already scroll-translated and clipped to the viewport, so a
    // partially off-screen image only paints the part that is on screen.
    pixmap.fill_rect(visible, &paint, Transform::identity(), None);
}

/// Paint a `<link href="…">` label: scroll-translated text in the link color
/// (accent blue unless the page styles it) with a 1px underline, without any
/// background chrome.
pub(super) fn render_link(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut Pixmap) {
    let Some(label) = box_node.text.as_deref().filter(|t| !t.is_empty()) else {
        return;
    };
    let font_size = ctx.resolve(box_node, box_node.style.font_size, UnitValue::Vh(0.025));
    // Links are inline text: no box padding unless the page sets one.
    let pad = ctx.resolve(box_node, box_node.style.padding, UnitValue::Px(0.0));
    let color = if box_node.style.text_color == Color::WHITE {
        Color {
            r: 122,
            g: 162,
            b: 247,
            a: 255,
        }
    } else {
        box_node.style.text_color
    };
    let x = box_node.rect.left() + pad + ctx.sx;
    let y = box_node.rect.top() + ctx.sy;
    let text_width = measure_text(label, font_size);
    let underline = Rect::from_xywh(x, y + font_size + 1.0, text_width, 1.0);
    if let Some(underline) = underline.and_then(|r| intersect_opt(ctx.clip, r)) {
        let mut paint = Paint::default();
        paint.set_color(to_skia_color(color));
        pixmap.fill_rect(underline, &paint, Transform::identity(), None);
    }
    draw_text_ab_glyph(
        pixmap,
        label,
        x,
        y,
        font_size,
        color,
        box_node.node_id,
        ctx.selection,
        ctx.state,
    );
}

pub(super) fn render_button(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut tiny_skia::Pixmap) {
    // Empty when the button has no label: never invent a placeholder word.
    let btn_txt = box_node.text.as_deref().unwrap_or("");
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

    // Center text both horizontally and vertically within the button rect.
    // Clip to the rect so a label wider than its (explicit-width) button is cut
    // at the border instead of painting across the page.
    let text_w = measure_text(btn_txt, font_size);
    let text_left = box_node.rect.left() + (box_node.rect.width() - text_w) / 2.0;
    let text_top = box_node.rect.top() + (box_node.rect.height() - font_size) / 2.0;
    let mut text_state = ctx.state;
    if let Some(rect) = ctx.shifted(box_node.rect) {
        text_state.clip = intersect_opt(text_state.clip, rect);
    }

    draw_text_ab_glyph(
        pixmap,
        btn_txt,
        text_left,
        text_top,
        font_size,
        label_color,
        box_node.node_id,
        ctx.selection,
        text_state,
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
        // A soft outline: the old 120,120,130 read as a bright focus ring on
        // dark fields even when the input was not focused.
        let border_color = SkiaColor::from_rgba8(78, 78, 92, 255);
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
