//! `Tag::Button`, `Tag::Icon`, `Tag::Input`, and `Tag::Image` rendering.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::layout::{measure_text, LayoutBox};
use crate::render::color::input_auto_text_color;
use crate::render::core::intersect_opt;
use crate::render::icons::cached_icon_path;
use crate::render::rounded::fill_rounded_rect;
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
    let font_size = ctx.resolve(box_node, box_node.style.font_size, UnitValue::Px(17.0));
    // Links are inline text: no box padding unless the page sets one.
    let pad = ctx.resolve(box_node, box_node.style.padding, UnitValue::Px(0.0));
    // The page default comes from the theme; authored colors override it.
    let color = box_node.style.text_color;
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
    let font_size = ctx.resolve(box_node, box_node.style.font_size, UnitValue::Px(16.0));

    // Background is always set by the theme defaults unless the page overrides
    // it, so there is no "invent a green button" fallback anymore.
    if let Some(r) = ctx.shifted(box_node.rect) {
        fill_rounded_rect(
            pixmap,
            r,
            box_node.style.border_radius,
            box_node.style.background_color,
        );
    }

    let label_color = box_node.style.text_color;

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
    // Chrome icons are colored by their PSS class; page icons use the theme's
    // default icon color.
    let color = box_node.style.text_color;
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
    let Some(r) = ctx.shifted(box_node.rect) else {
        return;
    };
    let (placeholder_color, fallback_border, default_bg, default_text) =
        crate::theme::with_active(|theme| {
            (
                theme.page.placeholder,
                theme.page.input_border,
                theme.page.input_bg,
                theme.page.input_text,
            )
        });
    // A page that themes the field background but not its text gets automatic
    // contrast instead of light-on-light (or dark-on-dark).
    let text_color = if box_node.style.text_color == default_text
        && box_node.style.background_color != default_bg
        && box_node.style.background_color != Color::TRANSPARENT
    {
        input_auto_text_color(&box_node.style.background_color)
    } else {
        box_node.style.text_color
    };
    // 1px border + rounded fill, both themed. The old hardcoded purple-grey
    // outline looked like a focus ring on every field.
    let border_color = if box_node.style.border_color != Color::TRANSPARENT {
        box_node.style.border_color
    } else {
        fallback_border
    };
    let radius = box_node.style.border_radius;
    fill_rounded_rect(pixmap, r, radius, border_color);
    let inner = Rect::from_xywh(
        r.left() + 1.0,
        r.top() + 1.0,
        (r.width() - 2.0).max(1.0),
        (r.height() - 2.0).max(1.0),
    )
    .unwrap();
    fill_rounded_rect(
        pixmap,
        inner,
        (radius - 1.0).max(0.0),
        box_node.style.background_color,
    );

    // Value (or gray placeholder) drawn inside, clipped to the box so long
    // values scroll visually instead of overflowing the field. An unfocused
    // chrome address field shows its shortened URL; focusing it reveals the
    // full one (`value`).
    let font_size = ctx.resolve(box_node, box_node.style.font_size, UnitValue::Px(15.0));
    let pad = ctx.resolve(box_node, box_node.style.padding, UnitValue::Px(7.0));
    let value = box_node.value.as_deref().unwrap_or("");
    let short = box_node.display_value.as_deref().unwrap_or("");
    let placeholder = box_node.placeholder.as_deref().unwrap_or("");
    let is_focused = ctx
        .input_focus
        .map(|(id, _)| id == box_node.node_id)
        .unwrap_or(false);
    let shown = if !is_focused && !short.is_empty() {
        short
    } else {
        value
    };

    let mut text_state = ctx.state;
    text_state.clip = intersect_opt(text_state.clip, inner);
    let (text, color) = if !shown.is_empty() {
        (shown, text_color)
    } else if !placeholder.is_empty() {
        (placeholder, placeholder_color)
    } else {
        ("", text_color)
    };
    // The chrome address bar centers its shortened host while unfocused (like
    // Chrome); focusing it left-aligns the full URL so the caret has room.
    let is_chrome_address = box_node.element_id.as_deref() == Some("__pwb_address");
    let text_left = if is_chrome_address && !is_focused && !text.is_empty() {
        (r.left() + (r.width() - measure_text(text, font_size)) / 2.0).max(r.left() + pad + 1.0)
    } else {
        r.left() + pad + 1.0
    };
    // Vertically center single-line text; with an explicit height (the address
    // field) padding-from-the-top left the text riding high.
    let text_top = r.top() + ((r.height() - font_size) / 2.0).max(0.0);
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
    // Caret: a 2px bar at the caret index (blinking is skipped to avoid a
    // per-frame timer waking the app).
    if is_focused {
        let caret = ctx.input_focus.map(|(_, c)| c).unwrap_or(0);
        let caret_x = text_left + caret_text_width(value, font_size, caret);
        let caret_h = (font_size * 1.2).max(8.0);
        let caret_top = r.top() + ((r.height() - caret_h) / 2.0).max(0.0);
        let caret_rect = Rect::from_xywh(caret_x, caret_top, 2.0, caret_h).unwrap();
        if let Some(visible) = intersect_opt(text_state.clip, caret_rect) {
            let mut paint = Paint::default();
            paint.set_color(to_skia_color(text_color));
            pixmap.fill_rect(visible, &paint, Transform::identity(), None);
        }
    }
}
