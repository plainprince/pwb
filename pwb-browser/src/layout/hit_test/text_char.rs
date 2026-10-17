//! Text character and caret hit-testing.

use ab_glyph::Font;
use pwb_core::spec::Tag;
use std::collections::HashMap;

use super::super::box_types::{get_font, LayoutBox};
use super::super::text::{glyph_advance_px, resolve_len};
use super::boxes::deepest_box_scrolled;

/// Hit-test a specific character inside a `Tag::Text` box.
/// Returns `(node_id, char_index)` when the point is inside a text box,
/// `None` otherwise (including when the point is not inside a `Tag::Text` box).
pub fn hit_test_text_char(
    root: &LayoutBox,
    x: f32,
    y: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(usize, usize)> {
    hit_test_text_char_scrolled(root, x, y, viewport_w, viewport_h, &HashMap::new())
}

/// Like [`hit_test_text_char`], but honors element scroll offsets (content is
/// hit at its scrolled position, clipped to scroll containers).
pub fn hit_test_text_char_scrolled(
    root: &LayoutBox,
    x: f32,
    y: f32,
    viewport_w: f32,
    viewport_h: f32,
    scrolls: &HashMap<usize, f32>,
) -> Option<(usize, usize)> {
    let (box_node, tx) = deepest_box_scrolled(root, x, y, scrolls)?;

    if box_node.tag != Tag::Text {
        return None;
    }

    let node_id = box_node.node_id;
    let text = box_node.text.as_deref().unwrap_or("");
    if text.is_empty() {
        return Some((node_id, 0));
    }

    // Convert the screen point into content coordinates for the char advance
    // math below (the box itself was already hit at its scrolled position).
    let x = x - tx;

    // Default text is 3vh, matching the layout/renderer (≈18px at 600px tall).
    let font_size = resolve_len(
        box_node.style.font_size,
        pwb_core::spec::UnitValue::Vh(0.03),
        box_node.rect.width(), // dim
        viewport_w,
        viewport_h,
    );

    // Per-character advances matching the renderer's draw stepping exactly
    // (see [`glyph_advance_px`]).
    let advances: Vec<f32> = if let Some(font) = get_font() {
        text.chars()
            .map(|ch| glyph_advance_px(font, font.glyph_id(ch), font_size))
            .collect()
    } else {
        text.chars()
            .map(|ch| {
                if ch.is_whitespace() {
                    font_size * 0.4
                } else {
                    font_size * 0.55
                }
            })
            .collect()
    };

    let left = box_node.rect.left();
    let mut pos = 0.0;
    for (i, adv) in advances.iter().enumerate() {
        let cx = left + pos;
        if x >= cx && x < cx + adv {
            return Some((node_id, i));
        }
        pos += adv;
    }

    // Past the end of the line: clamp to the last character.
    Some((node_id, advances.len().saturating_sub(1)))
}

/// Compute the caret index at a given local X offset within a text value.
/// Used for keyboard navigation and click-to-position within inputs.
pub fn caret_index_at_x(value: &str, font_size: f32, x_local: f32) -> usize {
    if value.is_empty() {
        return 0;
    }
    let advances: Vec<f32> = if let Some(font) = get_font() {
        value
            .chars()
            .map(|ch| glyph_advance_px(font, font.glyph_id(ch), font_size))
            .collect()
    } else {
        value
            .chars()
            .map(|ch| {
                if ch.is_whitespace() {
                    font_size * 0.4
                } else {
                    font_size * 0.55
                }
            })
            .collect()
    };

    let mut pos = 0.0;
    for (i, adv) in advances.iter().enumerate() {
        if x_local < pos + adv / 2.0 {
            return i;
        }
        pos += adv;
    }
    // Past the last character: the caret goes to the END of the value (a gap
    // position, `count`), not before the last character.
    value.chars().count()
}
