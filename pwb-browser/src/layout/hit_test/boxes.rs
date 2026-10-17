//! Box hit-testing: deepest box under a point and lookup by DOM node id.

use pwb_core::spec::OverflowStyle;
use std::collections::HashMap;
use tiny_skia::Rect;

use super::super::box_types::LayoutBox;

/// Find the deepest layout box containing the point (scroll-aware), preferring
/// later-painted (overlay) boxes. Returns the box and the accumulated
/// horizontal scroll translation at that box.
pub fn deepest_box_scrolled<'a>(
    node: &'a LayoutBox,
    x: f32,
    y: f32,
    scrolls: &HashMap<usize, f32>,
) -> Option<(&'a LayoutBox, f32)> {
    fn walk<'a>(
        node: &'a LayoutBox,
        x: f32,
        y: f32,
        tx: f32,
        ty: f32,
        clip: Option<Rect>,
        scrolls: &HashMap<usize, f32>,
    ) -> Option<(&'a LayoutBox, f32)> {
        if let Some(c) = clip {
            if x < c.left() || x > c.right() || y < c.top() || y > c.bottom() {
                return None;
            }
        }

        let is_scroll = matches!(node.overflow, OverflowStyle::Scroll | OverflowStyle::Auto)
            && node.content_height > node.rect.height() + 1.0;

        let (ctx_tx, ctx_ty) = if is_scroll {
            let off = scrolls.get(&node.node_id).copied().unwrap_or(0.0);
            (tx, ty - off)
        } else {
            (tx, ty)
        };

        let screen = Rect::from_xywh(
            node.rect.left() + ctx_tx,
            node.rect.top() + ctx_ty,
            node.rect.width(),
            node.rect.height(),
        )
        .unwrap();

        let child_clip = if matches!(
            node.overflow,
            OverflowStyle::Hidden | OverflowStyle::Scroll | OverflowStyle::Auto
        ) {
            Some(match clip {
                Some(c) => {
                    let left = c.left().max(screen.left());
                    let top = c.top().max(screen.top());
                    let right = c.right().min(screen.right());
                    let bottom = c.bottom().min(screen.bottom());
                    Rect::from_xywh(left, top, right - left, bottom - top).unwrap()
                }
                None => screen,
            })
        } else {
            clip
        };

        // Children are painted in order, so we walk them in reverse to find
        // the topmost hit (last painted wins).
        for child in node.children.iter().rev() {
            if let Some(hit) = walk(child, x, y, ctx_tx, ctx_ty, child_clip, scrolls) {
                return Some(hit);
            }
        }

        // Hit the current box itself.
        if x >= screen.left() && x < screen.right() && y >= screen.top() && y < screen.bottom() {
            return Some((node, ctx_tx));
        }
        None
    }

    walk(node, x, y, 0.0, 0.0, None, scrolls)
}

/// Find a layout box by its DOM node id.
pub fn find_box_by_id(root: &LayoutBox, node_id: usize) -> Option<&LayoutBox> {
    if root.node_id == node_id {
        return Some(root);
    }
    for child in &root.children {
        if let Some(found) = find_box_by_id(child, node_id) {
            return Some(found);
        }
    }
    None
}
