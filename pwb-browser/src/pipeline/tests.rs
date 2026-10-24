use super::app::insert_text_at_caret;
use super::devtools::apply_scroll_to_bottom;
use super::dom::{
    ancestor_node_ids, compute_selected_text, delete_selected_text, element_node_id,
    find_title_for_node, link_href, node_in_subtree, tree_row_target_node,
};
use super::resize::{resize_bounds, resize_new_dimensions};
use crate::dom::DomNode;
use crate::js_runtime::DomHandle;
use crate::layout::ResizeEdge;
use pwb_core::spec::Tag;
use std::collections::HashMap;

fn tn(id: usize, text: &str) -> DomNode {
    DomNode {
        id,
        tag: Tag::Text,
        attributes: HashMap::new(),
        text: Some(text.to_string()),
        children: vec![],
    }
}

#[test]
fn test_link_href_resolves_a_click_to_the_nearest_anchor() {
    let mut attributes = HashMap::new();
    attributes.insert("href".to_string(), "welcome.plain".to_string());
    let link = DomNode {
        id: 2,
        tag: Tag::Link,
        attributes,
        text: None,
        children: vec![tn(3, "Home")],
    };
    let root = root_with(vec![link]);

    // Clicking the link itself or the text run inside it follows `href`.
    assert_eq!(link_href(&root, 2).as_deref(), Some("welcome.plain"));
    assert_eq!(link_href(&root, 3).as_deref(), Some("welcome.plain"));
    // Clicking something that is not a link does not navigate.
    assert_eq!(link_href(&root, 1), None);
}

#[test]
fn test_link_href_ignores_empty_and_fragment_targets() {
    for href in ["", "   ", "#section"] {
        let mut attributes = HashMap::new();
        attributes.insert("href".to_string(), href.to_string());
        let link = DomNode {
            id: 2,
            tag: Tag::Link,
            attributes,
            text: Some("x".to_string()),
            children: vec![],
        };
        let root = root_with(vec![link]);
        assert_eq!(
            link_href(&root, 2),
            None,
            "href {:?} must not navigate",
            href
        );
    }
}

fn root_with(children: Vec<DomNode>) -> DomNode {
    DomNode {
        id: 1,
        tag: Tag::View,
        attributes: HashMap::new(),
        text: None,
        children,
    }
}

#[test]
fn test_ancestor_chain_and_subtree() {
    // Text (3) inside a button (2) inside the root (1); a devtools subtree (4).
    let root = root_with(vec![
        DomNode {
            id: 2,
            tag: Tag::Button,
            attributes: HashMap::new(),
            text: None,
            children: vec![tn(3, "Pick")],
        },
        DomNode {
            id: 4,
            tag: Tag::View,
            attributes: HashMap::from([("data-devtools".to_string(), "root".to_string())]),
            text: None,
            children: vec![tn(5, "row")],
        },
    ]);
    // Clicking the label bubbles: nearest ancestor first, excluding itself.
    assert_eq!(ancestor_node_ids(&root, 3), vec![2, 1]);
    assert_eq!(ancestor_node_ids(&root, 5), vec![4, 1]);
    assert_eq!(ancestor_node_ids(&root, 1), Vec::<usize>::new());

    // Devtools-membership checks (used to keep devtools "on top").
    assert!(node_in_subtree(&root.children[1], 5));
    assert!(!node_in_subtree(&root.children[1], 3));
}

#[test]
fn test_tree_row_target_node_finds_element() {
    // A devtools tree row (View with `data-node`) holding text children.
    // Clicking the row itself, a text child, or a close row (no id, but
    // `data-node`) all resolve to the represented element's node id.
    let row = DomNode {
        id: 10,
        tag: Tag::View,
        attributes: HashMap::from([("data-node".to_string(), "7".to_string())]),
        text: None,
        children: vec![
            tn(11, "  <button"),
            tn(12, "> Click me"),
            tn(13, "</button>"),
        ],
    };
    let close_row = DomNode {
        id: 20,
        tag: Tag::View,
        attributes: HashMap::from([("data-node".to_string(), "7".to_string())]),
        text: None,
        children: vec![tn(21, "  </view>")],
    };
    let root = root_with(vec![row, close_row]);
    assert_eq!(
        tree_row_target_node(&root, 10),
        Some(7),
        "row container itself"
    );
    assert_eq!(
        tree_row_target_node(&root, 12),
        Some(7),
        "text child of the row"
    );
    assert_eq!(tree_row_target_node(&root, 21), Some(7), "closing row");
    // Non-row nodes (the root) resolve to nothing.
    assert_eq!(tree_row_target_node(&root, 1), None);
}

#[test]
fn test_resize_new_dimensions() {
    let (w, h) = resize_new_dimensions(
        ResizeEdge::Left,
        866.0,
        1300.0,
        934.0,
        100.0,
        834.0,
        100.0,
        40.0,
        f32::MAX,
    );
    assert_eq!(w, 966.0); // dragged left 100px → wider
    assert_eq!(h, 1300.0);
    let (w2, _) = resize_new_dimensions(
        ResizeEdge::Right,
        400.0,
        100.0,
        500.0,
        0.0,
        550.0,
        0.0,
        40.0,
        f32::MAX,
    );
    assert_eq!(w2, 450.0);
    // Clamped to the minimum (dragging the left edge right past the width).
    let (w3, _) = resize_new_dimensions(
        ResizeEdge::Left,
        100.0,
        100.0,
        100.0,
        0.0,
        300.0,
        0.0,
        40.0,
        f32::MAX,
    );
    assert_eq!(w3, 40.0);
    // Clamped to the maximum (the devtools sidebar's `max-width`).
    let (w4, _) = resize_new_dimensions(
        ResizeEdge::Right,
        300.0,
        100.0,
        500.0,
        0.0,
        900.0,
        0.0,
        40.0,
        450.0,
    );
    assert_eq!(w4, 450.0);
}

#[test]
fn test_resize_bounds_reads_min_max_width() {
    // The drag clamp comes from the element's `min-width`/`max-width`.
    let root = DomNode {
        id: 1,
        tag: Tag::View,
        attributes: HashMap::from([
            ("min-width".to_string(), "300px".to_string()),
            ("max-width".to_string(), "450px".to_string()),
        ]),
        text: None,
        children: vec![],
    };
    let handle = DomHandle::new(crate::dom::Dom {
        title: "t".to_string(),
        root,
        components: HashMap::new(),
        modules: HashMap::new(),
    });
    let (min, max) = resize_bounds(&handle, 1, ResizeEdge::Left, 900.0, 600.0);
    assert_eq!(min, 300.0);
    assert_eq!(max, 450.0);
    // Vertical edges keep the default 40px floor / no cap.
    let (min, max) = resize_bounds(&handle, 1, ResizeEdge::Top, 900.0, 600.0);
    assert_eq!(min, 40.0);
    assert_eq!(max, f32::MAX);
}

#[test]
fn test_element_node_id_resolves_text_to_container() {
    // Clicking a Text run inside a button resolves to the button (the
    // element), not the text node — pick mode selects exactly one element.
    let root = root_with(vec![DomNode {
        id: 2,
        tag: Tag::Button,
        attributes: HashMap::new(),
        text: None,
        children: vec![tn(3, "Click me")],
    }]);
    assert_eq!(
        element_node_id(&root, 3),
        2,
        "text run resolves to its container"
    );
    assert_eq!(element_node_id(&root, 2), 2, "element resolves to itself");
}

#[test]
fn test_compute_selected_text_same_node() {
    let root = root_with(vec![tn(2, "hello world")]);
    assert_eq!(compute_selected_text(&root, 2, 6, 2, 11), "world");
}

#[test]
fn test_compute_selected_text_reversed_range() {
    let root = root_with(vec![tn(2, "hello world")]);
    // Drag left: end before start still selects the same characters.
    assert_eq!(compute_selected_text(&root, 2, 11, 2, 6), "world");
}

#[test]
fn test_compute_selected_text_across_nodes() {
    let root = root_with(vec![tn(2, "foo"), tn(3, "bar"), tn(4, "baz")]);
    assert_eq!(compute_selected_text(&root, 2, 1, 4, 2), "oobarba");
}

#[test]
fn test_compute_selected_text_unknown_node() {
    let root = root_with(vec![tn(2, "hello")]);
    assert_eq!(compute_selected_text(&root, 2, 0, 99, 3), "");
}

#[test]
fn test_insert_text_at_caret() {
    assert_eq!(
        insert_text_at_caret("hello", 5, " world"),
        ("hello world".to_string(), 11)
    );
    // Caret is clamped and control characters are dropped.
    assert_eq!(
        insert_text_at_caret("ab", 99, "\nX\t"),
        ("abX".to_string(), 3)
    );
    assert_eq!(insert_text_at_caret("ab", 1, "Z"), ("aZb".to_string(), 2));
}

#[test]
fn test_delete_selected_text_same_node() {
    let mut root = root_with(vec![tn(2, "hello world")]);
    assert!(delete_selected_text(&mut root, 2, 5, 2, 11));
    assert_eq!(root.children[0].text.as_deref(), Some("hello"));
}

#[test]
fn test_delete_selected_text_across_nodes() {
    let mut root = root_with(vec![tn(2, "foo"), tn(3, "bar"), tn(4, "baz")]);
    assert!(delete_selected_text(&mut root, 2, 1, 4, 2));
    assert_eq!(root.children[0].text.as_deref(), Some("f"));
    assert_eq!(root.children[1].text, None, "intermediate text removed");
    assert_eq!(root.children[2].text.as_deref(), Some("z"));
}

#[test]
fn test_delete_selected_text_reversed_and_unknown() {
    let mut root = root_with(vec![tn(2, "hello world")]);
    // Reversed drag deletes the same forward range (chars 6..11 = "world").
    assert!(delete_selected_text(&mut root, 2, 11, 2, 6));
    assert_eq!(root.children[0].text.as_deref(), Some("hello "));
    assert!(!delete_selected_text(&mut root, 99, 0, 2, 1));
}

#[test]
fn test_apply_scroll_to_bottom() {
    use crate::dom::{Dom, DomNode};
    use pwb_core::spec::Tag;
    // A scroll container (overflow: scroll) with tall content.
    let scroll_box = DomNode {
        id: 5,
        tag: Tag::View,
        attributes: HashMap::from([
            ("overflow".to_string(), "scroll".to_string()),
            ("height".to_string(), "100px".to_string()),
        ]),
        text: None,
        children: vec![DomNode {
            id: 6,
            tag: Tag::View,
            attributes: HashMap::from([("height".to_string(), "400px".to_string())]),
            text: None,
            children: vec![],
        }],
    };
    let root = DomNode {
        id: 1,
        tag: Tag::View,
        attributes: HashMap::new(),
        text: None,
        children: vec![scroll_box],
    };
    let dom = Dom {
        title: "t".to_string(),
        root,
        components: HashMap::new(),
        modules: HashMap::new(),
    };
    let layout = crate::layout::build_layout_tree(&dom, 900, 600, 0.0);
    let mut scrolls = std::collections::HashMap::new();
    apply_scroll_to_bottom(&layout, &mut scrolls, &[5]);
    // 400px content in a 100px viewport → scrolled to (near) the max
    // (block-flow spacing adds a little to the content height).
    let scrolled = *scrolls.get(&5).expect("scrolled");
    assert!(
        (300.0..=400.0).contains(&scrolled),
        "scrolled to bottom of the container (got {})",
        scrolled
    );
    // Unknown id: ignored.
    apply_scroll_to_bottom(&layout, &mut scrolls, &[999]);
    assert!(!scrolls.contains_key(&999));
}

#[test]
fn test_find_title_for_node() {
    use crate::dom::DomNode;
    use std::collections::HashMap;

    let child = DomNode {
        id: 42,
        tag: pwb_core::spec::Tag::Button,
        attributes: {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "Click me".to_string());
            m
        },
        text: Some("Go".to_string()),
        children: vec![],
    };
    let root = DomNode {
        id: 1,
        tag: pwb_core::spec::Tag::View,
        attributes: HashMap::new(),
        text: None,
        children: vec![child],
    };
    assert_eq!(find_title_for_node(&root, 42), Some("Click me".to_string()));
    assert_eq!(find_title_for_node(&root, 1), None);
    assert_eq!(find_title_for_node(&root, 99), None);
}
