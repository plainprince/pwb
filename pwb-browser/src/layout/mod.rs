//! Unified Layout Engine (`pwb-browser::layout`).
//!
//! Generates a precise Layout Box Tree with computed bounding rectangles (`x`, `y`, `w`, `h`)
//! for rendering, pixel-exact cursor hit-testing, and event handling.
//! Supports the full PSS positioning model:
//! - `static`: normal document flow.
//! - `relative`: normal flow, then offset by `top`/`left`/`right`/`bottom`.
//! - `absolute`: positioned against the nearest positioned ancestor (or the viewport),
//!   removed from flow.
//! - `fixed`: positioned against the viewport, unaffected by scrolling.
//! - `sticky`: normal flow, but clamps to `top` once scrolled past it.

pub mod box_types;
pub mod builder;
pub mod hit_test;
pub mod text;

// Re-export public API
pub use box_types::{get_font, LayoutBox, ResizeEdge};
pub use builder::build_layout_tree;
pub use hit_test::{
    caret_index_at_x, deepest_box_scrolled, find_box_by_id, hit_test_resize_handle,
    hit_test_text_char, hit_test_text_char_scrolled,
};
pub use text::docked_devtools_width;
pub use text::{
    glyph_advance_px, measure_text, measure_text_width, resolve_clamped_width, resolve_len,
    resolve_unit,
};
