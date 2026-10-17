//! Resize-handle hit-testing.

use super::super::box_types::{LayoutBox, ResizeEdge};

/// Hit-test a resize handle on a `resize`-enabled box.
/// Returns `(node_id, ResizeEdge)` when the point is within 8px of an edge.
pub fn hit_test_resize_handle(root: &LayoutBox, x: f32, y: f32) -> Option<(usize, ResizeEdge)> {
    fn walk(node: &LayoutBox, x: f32, y: f32) -> Option<(usize, ResizeEdge)> {
        // Only check boxes that actually have `resize` set.
        if node.style.resize == crate::style::ComputedStyle::default().resize {
            // No resize on this box, but check children
        } else {
            // Check if point is near an edge of this box.
            let r = node.rect;
            const HANDLE: f32 = 8.0;
            // Left handle
            if x >= r.left() - HANDLE && x <= r.left() + HANDLE && y >= r.top() && y <= r.bottom() {
                return Some((node.node_id, ResizeEdge::Left));
            }
            // Right handle
            if x >= r.right() - HANDLE && x <= r.right() + HANDLE && y >= r.top() && y <= r.bottom()
            {
                return Some((node.node_id, ResizeEdge::Right));
            }
            // Top handle
            if y >= r.top() - HANDLE && y <= r.top() + HANDLE && x >= r.left() && x <= r.right() {
                return Some((node.node_id, ResizeEdge::Top));
            }
            // Bottom handle
            if y >= r.bottom() - HANDLE
                && y <= r.bottom() + HANDLE
                && x >= r.left()
                && x <= r.right()
            {
                return Some((node.node_id, ResizeEdge::Bottom));
            }
        }
        // Recurse into children
        for child in &node.children {
            if let Some(hit) = walk(child, x, y) {
                return Some(hit);
            }
        }
        None
    }
    walk(root, x, y)
}
