//! Skia 2D Graphical Renderer & Pixel-Exact Hit Testing (`pwb-browser::render`).

mod color;
mod core;
mod entry;
mod highlight;
mod hit;
pub(crate) mod icons;
mod layout_box;
mod scrollbar;
mod text;
mod tooltip;

#[cfg(test)]
mod tests;

pub use core::{child_render_state, RenderState, SelectionPoint, TextSelection};
pub use entry::{render_dom_into_pixmap, render_dom_to_pixmap, ScrollbarInfo};
pub use highlight::{
    draw_hover_highlight, draw_hover_highlight_scrolled, draw_selected_highlight_scrolled,
};
pub use hit::{
    find_scroll_container, hit_test_cursor_from_layout, hit_test_cursor_from_layout_scrolled,
    hit_test_layout_box, hit_test_layout_box_scrolled,
};
pub use tooltip::draw_tooltip;
