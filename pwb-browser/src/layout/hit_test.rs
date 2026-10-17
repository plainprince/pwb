//! Hit testing utilities: point-in-box, text character, resize handle.

mod boxes;
mod resize;
mod text_char;

#[cfg(test)]
mod tests;

pub use boxes::{deepest_box_scrolled, find_box_by_id};
pub use resize::hit_test_resize_handle;
pub use text_char::{caret_index_at_x, hit_test_text_char, hit_test_text_char_scrolled};
