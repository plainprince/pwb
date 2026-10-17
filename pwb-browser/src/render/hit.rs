//! Layout-box hit-testing and cursor-from-layout helpers.

use crate::layout::LayoutBox;
use crate::render::core::{child_render_state, is_scroll_container, RenderState};
use pwb_core::spec::CursorStyle;
use std::collections::HashMap;
use winit::window::CursorIcon;

pub fn hit_test_layout_box(box_node: &LayoutBox, mx: f32, my: f32) -> Option<&LayoutBox> {
    hit_test_layout_box_scrolled(box_node, mx, my, &HashMap::new())
}

/// Like [`hit_test_layout_box`], but honors element scroll offsets: content is
/// hit at its scrolled position and clipped to scroll containers.
pub fn hit_test_layout_box_scrolled<'a>(
    box_node: &'a LayoutBox,
    mx: f32,
    my: f32,
    scrolls: &HashMap<usize, f32>,
) -> Option<&'a LayoutBox> {
    fn walk<'a>(
        b: &'a LayoutBox,
        mx: f32,
        my: f32,
        state: RenderState,
        scrolls: &HashMap<usize, f32>,
    ) -> Option<&'a LayoutBox> {
        if let Some(c) = state.clip {
            if mx < c.left() || mx > c.right() || my < c.top() || my > c.bottom() {
                return None;
            }
        }
        let child = child_render_state(b, state, scrolls);
        for child_box in b.children.iter().rev() {
            if let Some(hit) = walk(child_box, mx, my, child, scrolls) {
                return Some(hit);
            }
        }
        let r = b.rect;
        let rx = r.left() + state.tx;
        let ry = r.top() + state.ty;
        if mx >= rx && mx <= rx + r.width() && my >= ry && my <= ry + r.height() {
            Some(b)
        } else {
            None
        }
    }
    walk(box_node, mx, my, RenderState::identity(), scrolls)
}

/// Find the innermost scroll container under the cursor (for mouse-wheel
/// scrolling). Honors nested containers and scroll offsets.
pub fn find_scroll_container<'a>(
    root: &'a LayoutBox,
    mx: f32,
    my: f32,
    scrolls: &HashMap<usize, f32>,
) -> Option<&'a LayoutBox> {
    fn walk<'a>(
        b: &'a LayoutBox,
        mx: f32,
        my: f32,
        state: RenderState,
        scrolls: &HashMap<usize, f32>,
    ) -> Option<&'a LayoutBox> {
        if let Some(c) = state.clip {
            if mx < c.left() || mx > c.right() || my < c.top() || my > c.bottom() {
                return None;
            }
        }
        let child = child_render_state(b, state, scrolls);
        for child_box in b.children.iter().rev() {
            if let Some(hit) = walk(child_box, mx, my, child, scrolls) {
                return Some(hit);
            }
        }
        let r = b.rect;
        let rx = r.left() + state.tx;
        let ry = r.top() + state.ty;
        if is_scroll_container(b)
            && mx >= rx
            && mx <= rx + r.width()
            && my >= ry
            && my <= ry + r.height()
        {
            return Some(b);
        }
        None
    }
    walk(root, mx, my, RenderState::identity(), scrolls)
}

pub fn hit_test_cursor_from_layout(
    layout_root: &LayoutBox,
    mouse_x: f32,
    mouse_y: f32,
) -> CursorIcon {
    hit_test_cursor_from_layout_scrolled(layout_root, mouse_x, mouse_y, &HashMap::new())
}

/// Like [`hit_test_cursor_from_layout`], but honors element scroll offsets.
pub fn hit_test_cursor_from_layout_scrolled(
    layout_root: &LayoutBox,
    mouse_x: f32,
    mouse_y: f32,
    scrolls: &HashMap<usize, f32>,
) -> CursorIcon {
    if let Some(hovered_box) = hit_test_layout_box_scrolled(layout_root, mouse_x, mouse_y, scrolls)
    {
        if let Some(cursor_style) = hovered_box.style.cursor {
            match cursor_style {
                CursorStyle::Pointer => CursorIcon::Pointer,
                CursorStyle::Text => CursorIcon::Text,
                CursorStyle::Grab => CursorIcon::Grab,
                CursorStyle::Crosshair => CursorIcon::Crosshair,
                CursorStyle::NotAllowed => CursorIcon::NotAllowed,
                CursorStyle::Wait => CursorIcon::Wait,
                CursorStyle::Help => CursorIcon::Help,
                CursorStyle::ColResize => CursorIcon::ColResize,
                CursorStyle::RowResize => CursorIcon::RowResize,
                _ => CursorIcon::Default,
            }
        } else {
            CursorIcon::Default
        }
    } else {
        CursorIcon::Default
    }
}
