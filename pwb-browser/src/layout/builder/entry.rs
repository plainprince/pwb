//! Layout tree entry point: viewport setup and cursor inheritance.

use super::positioning::layout_node;
use crate::dom::Dom;
use crate::layout::box_types::{LayoutBox, LayoutCtx};
use crate::layout::text::docked_devtools_width;
use pwb_core::spec::CursorStyle;

/// Build a unified Layout Tree for the DOM.
pub fn build_layout_tree(
    dom: &Dom,
    viewport_width: u32,
    viewport_height: u32,
    scroll_y: f32,
) -> LayoutBox {
    // Page margins are viewport-relative (5vh ≈ 30px at a 600px-tall window,
    // ~65px at 1300px) so pages keep their proportions at any window size.
    let margin_x = viewport_height as f32 * 0.05;
    let mut current_y = margin_x - scroll_y;

    // When the devtools are docked (visible) they occupy their own column on
    // the right: the page lays out beside them and the site content shrinks to
    // make room instead of being covered by the overlay.
    let dock_w =
        docked_devtools_width(dom, viewport_width as f32, viewport_height as f32).unwrap_or(0.0);
    let max_w = (viewport_width as f32 - (margin_x * 2.0) - dock_w).max(100.0);

    let ctx = LayoutCtx {
        viewport_w: viewport_width as f32,
        viewport_h: viewport_height as f32,
    };
    let viewport = ctx.viewport_rect();
    let mut root = layout_node(
        &dom.root,
        &ctx,
        margin_x,
        &mut current_y,
        max_w,
        viewport,
        false,
        None,
    );
    // Inherit cursor from parent: children with no explicit cursor adopt
    // their parent's cursor (matching CSS `cursor: inherit` behavior).
    propagate_cursor(&mut root, None);
    root
}

/// Recursively set `cursor` on children that have `None`, using the
/// parent's cursor (CSS `cursor: inherit` behavior).
fn propagate_cursor(box_node: &mut LayoutBox, parent_cursor: Option<CursorStyle>) {
    if box_node.style.cursor.is_none() {
        box_node.style.cursor = parent_cursor;
    }
    let my_cursor = box_node.style.cursor;
    for child in &mut box_node.children {
        propagate_cursor(child, my_cursor);
    }
}
