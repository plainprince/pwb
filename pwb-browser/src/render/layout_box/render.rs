//! The recursive `render_layout_box` dispatcher and its shared render context.

use crate::layout::{resolve_unit, LayoutBox};
use crate::render::core::{child_render_state, intersect_opt, RenderState};
use crate::render::scrollbar::{draw_scrollbar, is_scroll_container, scroll_offset_for};
use crate::render::TextSelection;
use pwb_core::spec::{Tag, UnitValue};
use std::collections::HashMap;
use tiny_skia::Rect;

use super::containers::render_view;
use super::text::{render_code, render_text};
use super::widgets::{render_button, render_icon, render_image, render_input, render_link};

/// Per-box rendering context shared by the per-tag render functions: the
/// viewport dimensions, the accumulated scroll translation/clip, and the
/// render inputs threaded through the tree.
pub(super) struct RenderCtx<'a> {
    pub(super) vw: f32,
    pub(super) vh: f32,
    pub(super) sx: f32,
    pub(super) sy: f32,
    pub(super) clip: Option<Rect>,
    pub(super) selection: &'a TextSelection,
    pub(super) input_focus: Option<(usize, usize)>,
    pub(super) state: RenderState,
}

impl RenderCtx<'_> {
    /// Resolve a style length (`%` against the box width, `vw`/`vh` against
    /// the viewport), mirroring the layout engine's defaults.
    pub(super) fn resolve(
        &self,
        box_node: &LayoutBox,
        uv: Option<UnitValue>,
        default: UnitValue,
    ) -> f32 {
        resolve_unit(
            uv.unwrap_or(default),
            box_node.rect.width(),
            self.vw,
            self.vh,
        )
        .unwrap_or(0.0)
    }

    /// A screen-space helper that shifts a content rect by the accumulated
    /// scroll translation and clips it to the current clip region.
    pub(super) fn shifted(&self, r: Rect) -> Option<Rect> {
        let moved =
            Rect::from_xywh(r.left() + self.sx, r.top() + self.sy, r.width(), r.height()).unwrap();
        intersect_opt(self.clip, moved)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_layout_box(
    box_node: &LayoutBox,
    pixmap: &mut tiny_skia::Pixmap,
    viewport_width: u32,
    viewport_height: u32,
    selection: &crate::render::TextSelection,
    scrolls: &HashMap<usize, f32>,
    input_focus: Option<(usize, usize)>,
    state: RenderState,
) {
    let vw = viewport_width as f32;
    let vh = viewport_height as f32;
    // All lengths are exact device pixels — no viewport/DPR scaling.
    let ctx = RenderCtx {
        vw,
        vh,
        sx: state.tx,
        sy: state.ty,
        clip: state.clip,
        selection,
        input_focus,
        state,
    };

    match box_node.tag {
        Tag::Document | Tag::Head | Tag::Title => {}
        Tag::View => render_view(&ctx, box_node, pixmap),
        Tag::Text => render_text(&ctx, box_node, pixmap),
        Tag::Code => render_code(&ctx, box_node, pixmap),
        Tag::Button => render_button(&ctx, box_node, pixmap),
        Tag::Icon => render_icon(&ctx, box_node, pixmap),
        Tag::Input => render_input(&ctx, box_node, pixmap),
        Tag::Image => render_image(&ctx, box_node, pixmap),
        Tag::Link => render_link(&ctx, box_node, pixmap),
        _ => {}
    }

    // Children inherit the scroll translate + clip of this box.
    let child_state = child_render_state(box_node, state, scrolls);
    for child in &box_node.children {
        render_layout_box(
            child,
            pixmap,
            viewport_width,
            viewport_height,
            selection,
            scrolls,
            input_focus,
            child_state,
        );
    }

    // The scrollbar draws last, on top of the (clipped) content.
    if is_scroll_container(box_node) {
        if let Some(r) = ctx.shifted(box_node.rect) {
            draw_scrollbar(
                pixmap,
                r,
                box_node.content_height,
                scroll_offset_for(box_node, scrolls),
            );
        }
    }
}
