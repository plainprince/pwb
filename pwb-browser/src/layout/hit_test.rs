//! Hit testing utilities: point-in-box, text character, resize handle.

use super::box_types::{get_font, LayoutBox, ResizeEdge};
use ab_glyph::Font;
use pwb_core::spec::{OverflowStyle, Tag};
use std::collections::HashMap;
use tiny_skia::Rect;

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

// Re-export the text measurement functions from text.rs for hit_testing
use super::text::{glyph_advance_px, resolve_len};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Dom;
    use crate::layout::box_types::test_font;
    use crate::layout::build_layout_tree;
    use crate::style::ComputedStyle;
    use std::collections::HashMap;

    #[test]
    fn test_caret_index_at_x() {
        let _ = test_font();
        let text = "hello";
        // At x=0 → index 0 (before the first char)
        assert_eq!(caret_index_at_x(text, 16.0, 0.0), 0);
        // Past end → gap position AFTER the last char (5), not before it (4)
        assert_eq!(caret_index_at_x(text, 16.0, 1000.0), 5);
    }

    #[test]
    fn test_find_box_by_id() {
        let box_node = LayoutBox {
            node_id: 42,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0).unwrap(),
            style: ComputedStyle::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: OverflowStyle::Visible,
            content_height: 0.0,
        };
        assert_eq!(find_box_by_id(&box_node, 42).map(|b| b.node_id), Some(42));
        assert!(find_box_by_id(&box_node, 99).is_none());
    }

    #[test]
    fn test_hit_test_resize_handle() {
        let layout = build_layout_tree(
            &Dom {
                title: "test".into(),
                root: crate::dom::DomNode {
                    id: 1,
                    tag: Tag::Document,
                    attributes: HashMap::new(),
                    text: None,
                    children: vec![crate::dom::DomNode {
                        id: 2,
                        tag: Tag::View,
                        attributes: HashMap::from([
                            ("position".into(), "fixed".into()),
                            ("right".into(), "0".into()),
                            ("top".into(), "0".into()),
                            ("width".into(), "400px".into()),
                            ("height".into(), "100%".into()),
                            ("resize".into(), "horizontal".into()),
                        ]),
                        text: None,
                        children: vec![],
                    }],
                },
                components: HashMap::new(),
                modules: HashMap::new(),
            },
            900,
            600,
            0.0,
        );

        // Near the left edge (500) → Left handle.
        let (node_id, edge) = hit_test_resize_handle(&layout, 496.0, 300.0).unwrap();
        assert_eq!(edge, ResizeEdge::Left);
        assert!(node_id > 0);

        // Near the right edge (900) → Right handle.
        assert_eq!(
            hit_test_resize_handle(&layout, 900.0, 300.0).unwrap().1,
            ResizeEdge::Right
        );

        // Center of the panel → no handle.
        assert!(hit_test_resize_handle(&layout, 700.0, 300.0).is_none());

        // Outside the panel → no handle.
        assert!(hit_test_resize_handle(&layout, 400.0, 300.0).is_none());
    }

    fn simple_box(node_id: usize, x: f32, y: f32, w: f32, h: f32) -> LayoutBox {
        LayoutBox {
            node_id,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(x, y, w, h).unwrap(),
            style: ComputedStyle::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: OverflowStyle::Visible,
            content_height: 0.0,
        }
    }

    #[test]
    fn test_deepest_box_prefers_children_and_reports_misses() {
        let mut parent = simple_box(1, 0.0, 0.0, 200.0, 200.0);
        parent.children.push(simple_box(2, 50.0, 50.0, 50.0, 50.0));
        let scrolls = HashMap::new();

        // Inside the child → the child wins (deepest box).
        let (hit, tx) = deepest_box_scrolled(&parent, 60.0, 60.0, &scrolls).unwrap();
        assert_eq!(hit.node_id, 2);
        assert_eq!(tx, 0.0);

        // Inside the parent but outside the child → the parent.
        let (hit, _) = deepest_box_scrolled(&parent, 10.0, 10.0, &scrolls).unwrap();
        assert_eq!(hit.node_id, 1);

        // Completely outside → miss.
        assert!(deepest_box_scrolled(&parent, 500.0, 500.0, &scrolls).is_none());
    }

    #[test]
    fn test_deepest_box_later_siblings_paint_on_top() {
        let mut parent = simple_box(1, 0.0, 0.0, 100.0, 100.0);
        parent.children.push(simple_box(2, 0.0, 0.0, 100.0, 100.0));
        parent.children.push(simple_box(3, 0.0, 0.0, 100.0, 100.0));
        let scrolls = HashMap::new();
        // Both children overlap fully; the later-painted sibling wins.
        let (hit, _) = deepest_box_scrolled(&parent, 50.0, 50.0, &scrolls).unwrap();
        assert_eq!(hit.node_id, 3);
    }

    #[test]
    fn test_deepest_box_honors_scroll_offset() {
        // A scroll container whose content is taller than its box: scrolling
        // translates the content up, so a point just above the child's
        // unscrolled top edge lands on the child only when scrolled.
        let mut scroller = simple_box(10, 0.0, 0.0, 100.0, 100.0);
        scroller.overflow = OverflowStyle::Scroll;
        scroller.content_height = 300.0;
        scroller
            .children
            .push(simple_box(11, 0.0, 20.0, 100.0, 200.0));

        // With a 20px offset the child's screen rect starts at y=0, so y=10
        // hits it; unscrolled the child starts at y=20 and y=10 falls on the
        // container itself.
        let scrolls = HashMap::from([(10, 20.0)]);
        let (hit, tx) = deepest_box_scrolled(&scroller, 50.0, 10.0, &scrolls).unwrap();
        assert_eq!(hit.node_id, 11);
        assert_eq!(tx, 0.0);

        let plain = HashMap::new();
        let hit = deepest_box_scrolled(&scroller, 50.0, 10.0, &plain)
            .unwrap()
            .0;
        assert_eq!(hit.node_id, 10);
    }

    #[test]
    fn test_deepest_box_clips_children_of_hidden_overflow() {
        let mut container = simple_box(1, 0.0, 0.0, 100.0, 100.0);
        container.overflow = OverflowStyle::Hidden;
        // Child extends beyond the container's clip.
        container
            .children
            .push(simple_box(2, 80.0, 80.0, 100.0, 100.0));
        let scrolls = HashMap::new();

        // Inside the overlap → child.
        let (hit, _) = deepest_box_scrolled(&container, 90.0, 90.0, &scrolls).unwrap();
        assert_eq!(hit.node_id, 2);
        // Beyond the container's clip → the overflowing child is clipped away.
        assert!(deepest_box_scrolled(&container, 150.0, 150.0, &scrolls).is_none());
    }

    #[test]
    fn test_hit_test_text_char_reports_char_indices() {
        let _ = test_font();
        // A wide text box so a point can sit past the line's advance width
        // but still inside the box.
        let mut text_box = simple_box(2, 0.0, 0.0, 500.0, 20.0);
        text_box.tag = Tag::Text;
        text_box.text = Some("hello".into());

        // Near the left edge → first character.
        assert_eq!(
            hit_test_text_char(&text_box, 1.0, 10.0, 900.0, 600.0),
            Some((2, 0))
        );
        // Past the line end but inside the box → clamps to the last character.
        assert_eq!(
            hit_test_text_char(&text_box, 400.0, 10.0, 900.0, 600.0),
            Some((2, 4))
        );
        // Outside any box → miss.
        assert!(hit_test_text_char(&text_box, -50.0, -50.0, 900.0, 600.0).is_none());

        // A non-text box hit yields None even though a box was found.
        let view = simple_box(9, 0.0, 0.0, 50.0, 50.0);
        assert_eq!(hit_test_text_char(&view, 10.0, 10.0, 900.0, 600.0), None);
    }

    #[test]
    fn test_hit_test_text_char_empty_text_returns_zero() {
        let mut text_box = simple_box(5, 0.0, 0.0, 100.0, 20.0);
        text_box.tag = Tag::Text;
        text_box.text = Some(String::new());
        assert_eq!(
            hit_test_text_char(&text_box, 10.0, 10.0, 900.0, 600.0),
            Some((5, 0))
        );
    }

    #[test]
    fn test_caret_index_at_x_mid_character_rounding() {
        let _ = test_font();
        // Advances are positive; half-advance boundaries pick the nearer edge.
        let adv = {
            let font = get_font().expect("test font");
            glyph_advance_px(font, font.glyph_id('h'), 16.0)
        };
        assert_eq!(caret_index_at_x("hello", 16.0, adv * 0.4), 0);
        assert_eq!(caret_index_at_x("hello", 16.0, adv * 0.6), 1);
        assert_eq!(caret_index_at_x("", 16.0, 5.0), 0);
    }

    #[test]
    fn test_find_box_by_id_walks_nested_children() {
        let mut root = simple_box(1, 0.0, 0.0, 100.0, 100.0);
        let mut mid = simple_box(2, 0.0, 0.0, 100.0, 100.0);
        mid.children.push(simple_box(3, 0.0, 0.0, 10.0, 10.0));
        root.children.push(mid);
        assert_eq!(find_box_by_id(&root, 3).map(|b| b.node_id), Some(3));
        assert_eq!(find_box_by_id(&root, 2).map(|b| b.node_id), Some(2));
        assert!(find_box_by_id(&root, 99).is_none());
    }
}
