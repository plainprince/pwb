//! Drag-resize state and geometry for `resize`-enabled elements.

use crate::js_runtime::DomHandle;
use crate::layout::{resolve_unit, ResizeEdge};
use crate::style::compute_style;
use winit::window::CursorIcon;

/// An in-progress drag-resize of a `resize`-enabled element.
pub(super) struct ResizeDrag {
    pub(super) node_id: usize,
    pub(super) edge: ResizeEdge,
    pub(super) start_w: f32,
    pub(super) start_h: f32,
    pub(super) start_mx: f32,
    pub(super) start_my: f32,
    /// Width clamp for the drag: the element's `min-width`/`max-width` (only
    /// meaningful for Left/Right edges; vertical drags use 40/MAX).
    pub(super) min_w: f32,
    pub(super) max_w: f32,
}

impl ResizeDrag {
    pub(super) fn cursor(&self) -> CursorIcon {
        match self.edge {
            ResizeEdge::Left | ResizeEdge::Right => CursorIcon::EwResize,
            ResizeEdge::Top | ResizeEdge::Bottom => CursorIcon::NsResize,
        }
    }
}

/// Compute the new width/height (physical px) from a resize drag. `start_*` are the
/// physical dimensions/position at grab time; the result clamps to `[min, max]`
/// (the element's `min-width`/`max-width` for horizontal edges; vertical edges
/// only clamp at `min` since `max` is MAX for them).
#[allow(clippy::too_many_arguments)]
pub(super) fn resize_new_dimensions(
    edge: ResizeEdge,
    start_w: f32,
    start_h: f32,
    start_mx: f32,
    start_my: f32,
    mx: f32,
    my: f32,
    min: f32,
    max: f32,
) -> (f32, f32) {
    let mut new_w = start_w;
    let mut new_h = start_h;
    match edge {
        ResizeEdge::Left => new_w = (start_w - (mx - start_mx)).clamp(min, max),
        ResizeEdge::Right => new_w = (start_w + (mx - start_mx)).clamp(min, max),
        ResizeEdge::Top => new_h = (start_h - (my - start_my)).max(min),
        ResizeEdge::Bottom => new_h = (start_h + (my - start_my)).max(min),
    }
    (new_w, new_h)
}

/// The `[min, max]` width clamp for a resize drag on `node_id`, from the
/// element's `min-width`/`max-width` style (horizontal edges) or the default
/// 40px floor (vertical edges).
pub(super) fn resize_bounds(
    handle: &DomHandle,
    node_id: usize,
    edge: ResizeEdge,
    vw: f32,
    vh: f32,
) -> (f32, f32) {
    let style = handle.dom.borrow();
    let style = crate::dom::find_node_by_id_ref(&style.root, &node_id.to_string())
        .map(compute_style)
        .unwrap_or_default();
    match edge {
        ResizeEdge::Left | ResizeEdge::Right => (
            style
                .min_width
                .and_then(|uv| resolve_unit(uv, vw, vw, vh))
                .unwrap_or(40.0),
            style
                .max_width
                .and_then(|uv| resolve_unit(uv, vw, vw, vh))
                .unwrap_or(f32::MAX),
        ),
        ResizeEdge::Top | ResizeEdge::Bottom => (40.0, f32::MAX),
    }
}
