//! `Tag::Text` and `Tag::Code` rendering.

use crate::layout::LayoutBox;
use crate::render::text::draw_text_ab_glyph;
use pwb_core::spec::{Color, UnitValue};

use super::render::RenderCtx;

pub(super) fn render_text(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut tiny_skia::Pixmap) {
    if let Some(txt) = &box_node.text {
        let font_size = ctx.resolve(box_node, box_node.style.font_size, UnitValue::Vh(0.03));
        draw_text_ab_glyph(
            pixmap,
            txt,
            box_node.rect.left(),
            box_node.rect.top(),
            font_size,
            box_node.style.text_color,
            box_node.node_id,
            ctx.selection,
            ctx.state,
        );
    }
}

pub(super) fn render_code(ctx: &RenderCtx, box_node: &LayoutBox, pixmap: &mut tiny_skia::Pixmap) {
    // `<code>` draws its text with real JS syntax highlighting: the
    // text is tokenized and each token is drawn with its own color at
    // its own advance (strings orange, numbers green, keywords blue,
    // comments gray). The box's own `color` (if set) becomes the base
    // color for identifiers/operators — e.g. red error lines in the
    // devtools console stay red while their tokens are highlighted.
    if let Some(txt) = &box_node.text {
        let font_size = ctx.resolve(box_node, box_node.style.font_size, UnitValue::Vh(0.03));
        let base = if box_node.style.text_color != Color::WHITE {
            box_node.style.text_color
        } else {
            Color {
                r: 212,
                g: 212,
                b: 212,
                a: 255,
            }
        };
        let mut x = box_node.rect.left();
        let y = box_node.rect.top();
        for (token, color) in crate::render::text::js_tokenize(txt, base) {
            if token.is_empty() {
                continue;
            }
            x += draw_text_ab_glyph(
                pixmap,
                &token,
                x,
                y,
                font_size,
                color,
                box_node.node_id,
                ctx.selection,
                ctx.state,
            );
        }
    }
}
