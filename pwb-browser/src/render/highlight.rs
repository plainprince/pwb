//! Inspect/selection highlighting around elements.

use crate::layout::LayoutBox;
use crate::render::core::{child_render_state, RenderState};
use crate::render::hit::hit_test_layout_box_scrolled;
use pwb_core::spec::Tag;
use std::collections::HashMap;
use tiny_skia::{Paint, PathBuilder, Rect, Transform};

/// Draw a devtools-style inspect highlight around the element under a point.
///
/// `strong` (inspect mode) fills the box with a translucent blue; otherwise a
/// subtle 2px outline marks the hovered element in the main section.
pub fn draw_hover_highlight(
    pixmap: &mut tiny_skia::Pixmap,
    layout_root: &LayoutBox,
    x: f32,
    y: f32,
    strong: bool,
) {
    draw_hover_highlight_scrolled(pixmap, layout_root, x, y, strong, &HashMap::new())
}

/// Like [`draw_hover_highlight`], but honors element scroll offsets.
pub fn draw_hover_highlight_scrolled(
    pixmap: &mut tiny_skia::Pixmap,
    layout_root: &LayoutBox,
    x: f32,
    y: f32,
    strong: bool,
    scrolls: &HashMap<usize, f32>,
) {
    let Some(box_node) = hit_test_layout_box_scrolled(layout_root, x, y, scrolls) else {
        return;
    };
    let r = box_node.rect;
    // Nothing useful to mark on the document shell or empty boxes.
    if matches!(box_node.tag, Tag::Document | Tag::Head | Tag::Title)
        || r.width() <= 0.0
        || r.height() <= 0.0
    {
        return;
    }
    // The hit-test already accounts for scroll offsets: the box rect is in
    // content coordinates, so translate by the accumulated scroll state.
    let (tx, ty) = box_screen_translation(box_node, layout_root, scrolls);
    draw_highlight_at(pixmap, r, tx, ty, strong);
}

/// The accumulated scroll translation (content → screen) for a box, computed
/// by walking from the root down to `b`. Used to draw highlights on boxes that
/// were located by id (e.g. a devtools-selected element) rather than by
/// hit-testing, where the scroll state is already folded in.
fn box_screen_translation(
    b: &LayoutBox,
    root: &LayoutBox,
    scrolls: &HashMap<usize, f32>,
) -> (f32, f32) {
    fn walk(
        node: &LayoutBox,
        target: usize,
        tx: f32,
        ty: f32,
        scrolls: &HashMap<usize, f32>,
    ) -> Option<(f32, f32)> {
        if node.node_id == target {
            return Some((tx, ty));
        }
        let child = child_render_state(node, RenderState { tx, ty, clip: None }, scrolls);
        for c in &node.children {
            if let Some(hit) = walk(c, target, child.tx, child.ty, scrolls) {
                return Some(hit);
            }
        }
        None
    }
    walk(root, b.node_id, 0.0, 0.0, scrolls).unwrap_or((0.0, 0.0))
}

/// Draw the inspect highlight (optional translucent fill + 2px blue outline)
/// around a content-space rect shifted by `(tx, ty)` into screen space.
fn draw_highlight_at(pixmap: &mut tiny_skia::Pixmap, r: Rect, tx: f32, ty: f32, strong: bool) {
    let left = r.left() + tx;
    let top = r.top() + ty;
    let rect = Rect::from_xywh(left, top, r.width(), r.height());
    let Some(rect) = rect else { return };

    if strong {
        let mut fill = Paint::default();
        fill.set_color(tiny_skia::Color::from_rgba8(0, 120, 215, 55));
        let path = PathBuilder::from_rect(rect);
        pixmap.fill_path(
            &path,
            &fill,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    // 2px outline: four thin rects around the box.
    let lw = 2.0;
    let mut stroke = Paint::default();
    stroke.set_color(tiny_skia::Color::from_rgba8(0, 120, 215, 235));
    for rect in [
        Rect::from_xywh(
            rect.left() - lw,
            rect.top() - lw,
            rect.width() + lw * 2.0,
            lw,
        ),
        Rect::from_xywh(rect.left() - lw, rect.bottom(), rect.width() + lw * 2.0, lw),
        Rect::from_xywh(rect.left() - lw, rect.top(), lw, rect.height()),
        Rect::from_xywh(rect.right(), rect.top(), lw, rect.height()),
    ]
    .into_iter()
    .flatten()
    {
        let path = PathBuilder::from_rect(rect);
        pixmap.fill_path(
            &path,
            &stroke,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Draw the strong inspect highlight (fill + outline) around the element with
/// the given node id, honoring scroll offsets. Used by the pipeline to show the
/// devtools-selected element in the main section, mirroring pick mode's
/// hover preview.
pub fn draw_selected_highlight_scrolled(
    pixmap: &mut tiny_skia::Pixmap,
    layout_root: &LayoutBox,
    node_id: usize,
    scrolls: &HashMap<usize, f32>,
) {
    let Some(box_node) = crate::layout::find_box_by_id(layout_root, node_id) else {
        return;
    };
    let r = box_node.rect;
    if matches!(box_node.tag, Tag::Document | Tag::Head | Tag::Title)
        || r.width() <= 0.0
        || r.height() <= 0.0
    {
        return;
    }
    let (tx, ty) = box_screen_translation(box_node, layout_root, scrolls);
    draw_highlight_at(pixmap, r, tx, ty, true);
}
