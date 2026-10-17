//! Scroll state, clipping, and the `RenderState` threaded through rendering.

use crate::layout::LayoutBox;
use pwb_core::spec::OverflowStyle;
use std::collections::HashMap;
use tiny_skia::Rect;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextSelection {
    pub start_node_id: Option<usize>,
    pub start_char_pos: Option<usize>,
    pub end_node_id: Option<usize>,
    pub end_char_pos: Option<usize>,
    pub is_selecting: bool,
    pub selected_text_buffer: String,
}

/// A box is a scroll container when it asks for scrolling and its content
/// actually overflows its rect.
pub(crate) fn is_scroll_container(b: &LayoutBox) -> bool {
    matches!(b.overflow, OverflowStyle::Scroll | OverflowStyle::Auto)
        && b.content_height > b.rect.height() + 1.0
}

/// The scroll offset for a container, clamped to its current range so a stale
/// offset (e.g. after the content shrank) can never scroll past the end.
pub(crate) fn scroll_offset_for(b: &LayoutBox, scrolls: &HashMap<usize, f32>) -> f32 {
    let max = (b.content_height - b.rect.height()).max(0.0);
    scrolls
        .get(&b.node_id)
        .copied()
        .unwrap_or(0.0)
        .clamp(0.0, max)
}

/// Intersect a screen-space clip with a rect, returning `None` when empty.
pub(crate) fn intersect_opt(clip: Option<Rect>, rect: Rect) -> Option<Rect> {
    let base = clip.unwrap_or(rect);
    let left = base.left().max(rect.left());
    let top = base.top().max(rect.top());
    let right = base.right().min(rect.right());
    let bottom = base.bottom().min(rect.bottom());
    if right > left && bottom > top {
        Rect::from_xywh(left, top, right - left, bottom - top)
    } else {
        None
    }
}

/// Translation + clip state threaded through rendering/hit-testing so content
/// inside scroll containers is drawn and hit at its scrolled position, clipped
/// to the container.
#[derive(Clone, Copy)]
pub struct RenderState {
    pub tx: f32,
    pub ty: f32,
    pub clip: Option<Rect>,
}

impl RenderState {
    pub fn identity() -> Self {
        Self {
            tx: 0.0,
            ty: 0.0,
            clip: None,
        }
    }
}

/// The child state for a box: applies this box's scroll offset and clip to the
/// subtree when the box is a scroll container.
pub fn child_render_state(
    b: &LayoutBox,
    state: RenderState,
    scrolls: &HashMap<usize, f32>,
) -> RenderState {
    let mut child = state;
    if is_scroll_container(b) {
        child.ty -= scroll_offset_for(b, scrolls);
    }
    if matches!(
        b.overflow,
        OverflowStyle::Hidden | OverflowStyle::Scroll | OverflowStyle::Auto
    ) {
        let screen = Rect::from_xywh(
            b.rect.left() + state.tx,
            b.rect.top() + state.ty,
            b.rect.width(),
            b.rect.height(),
        )
        .unwrap();
        child.clip = intersect_opt(child.clip, screen);
    }
    child
}
