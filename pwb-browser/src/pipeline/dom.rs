//! DOM traversal, hit-test resolution and text-selection helpers.

use crate::render::hit_test_layout_box_scrolled;
use pwb_core::spec::Tag;

/// Find a DOM node by its numeric node id (used by the F12 devtools toggle).
pub(super) fn find_node_by_id<'a>(
    node: &'a mut crate::dom::DomNode,
    id: &str,
) -> Option<&'a mut crate::dom::DomNode> {
    if node.id.to_string() == id || node.attributes.get("id").map(|v| v == id).unwrap_or(false) {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_node_by_id(child, id) {
            return Some(found);
        }
    }
    None
}

/// Find the first node carrying an attribute (e.g. `data-devtools`).
pub(super) fn find_node_with_attr<'a>(
    node: &'a crate::dom::DomNode,
    attr: &str,
) -> Option<&'a crate::dom::DomNode> {
    if node.attributes.contains_key(attr) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_with_attr(child, attr) {
            return Some(found);
        }
    }
    None
}

/// Whether `node_id` lies inside the subtree rooted at `subtree_root`.
pub(super) fn node_in_subtree(subtree_root: &crate::dom::DomNode, node_id: usize) -> bool {
    fn walk(node: &crate::dom::DomNode, id: usize) -> bool {
        if node.id == id {
            return true;
        }
        node.children.iter().any(|c| walk(c, id))
    }
    walk(subtree_root, node_id)
}

/// The `id` attribute of a node by numeric id, if any ("" otherwise).
pub(super) fn node_id_attr(root: &crate::dom::DomNode, id: usize) -> Option<String> {
    fn walk(node: &crate::dom::DomNode, id: usize) -> Option<&crate::dom::DomNode> {
        if node.id == id {
            return Some(node);
        }
        node.children.iter().find_map(|c| walk(c, id))
    }
    walk(root, id).and_then(|n| n.attributes.get("id").cloned())
}

/// Walk the subtree depth-first looking for a node with `node_id` that has a
/// `title` attribute. Returns the title string (or `None`).
pub(super) fn find_title_for_node(node: &crate::dom::DomNode, node_id: usize) -> Option<String> {
    if node.id == node_id {
        return node.attributes.get("title").cloned();
    }
    node.children
        .iter()
        .find_map(|c| find_title_for_node(c, node_id))
}

/// The `title` attribute of the element under the cursor, for the hover
/// tooltip. Returns `None` when over devtools internals or when nothing
/// tooltip-worthy is hovered.
pub(super) fn hovered_title(
    dom: &crate::dom::Dom,
    layout_root: &crate::layout::LayoutBox,
    mx: f32,
    my: f32,
    scrolls: &std::collections::HashMap<usize, f32>,
) -> Option<String> {
    let hit = hit_test_layout_box_scrolled(layout_root, mx, my, scrolls)?;
    let over_devtools = find_node_with_attr(&dom.root, "data-devtools")
        .map(|root| node_in_subtree(root, hit.node_id))
        .unwrap_or(false);
    if over_devtools {
        return None;
    }
    dom.root
        .children
        .iter()
        .find_map(|c| find_title_for_node(c, hit.node_id))
        .filter(|t| !t.is_empty())
}

/// How long the pointer must rest on an element before its `title` tooltip
/// appears — native tooltips never pop instantly on first hover.
pub(super) const TOOLTIP_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// If `node_id` (or any of its ancestors) is a devtools Elements tree row,
/// return the original DOM node it represents (from the row's `data-node`
/// attribute). Clicking any part of a row — the opening tag, the content, or
/// the closing `</tag>` line — selects that element.
pub(crate) fn tree_row_target_node(root: &crate::dom::DomNode, node_id: usize) -> Option<usize> {
    fn walk(node: &crate::dom::DomNode, id: usize) -> Option<&crate::dom::DomNode> {
        if node.id == id {
            return Some(node);
        }
        node.children.iter().find_map(|c| walk(c, id))
    }
    let mut ids = vec![node_id];
    ids.extend(ancestor_node_ids(root, node_id));
    for id in ids {
        if let Some(node) = walk(root, id) {
            if let Some(target) = node.attributes.get("data-node") {
                if let Ok(parsed) = target.parse::<usize>() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

/// Resolve a hit node id to the element it belongs to: walks up from a Text
/// run (or the document shell) to the nearest real element, so picking a text
/// run selects the container that holds it — exactly one element.
pub(super) fn element_node_id(root: &crate::dom::DomNode, hit: usize) -> usize {
    let mut cur = hit;
    loop {
        let is_shell = crate::dom::find_node_by_id_ref(root, &cur.to_string())
            .map(|n| matches!(n.tag, Tag::Text | Tag::Document | Tag::Head | Tag::Title))
            .unwrap_or(false);
        if !is_shell {
            return cur;
        }
        let parent = ancestor_node_ids(root, cur).first().copied().unwrap_or(cur);
        if parent == cur {
            return cur;
        }
        cur = parent;
    }
}

/// The `href` a click on `node_id` (or on text inside it) should follow.
///
/// Walks up from the hit node to the nearest `<a>`/Link, exactly like a real
/// browser's click target resolution, and returns `None` for non-links, empty
/// hrefs, and `href="#…"` fragments (which stay on the current page).
pub(crate) fn link_href(root: &crate::dom::DomNode, node_id: usize) -> Option<String> {
    let mut ids = vec![node_id];
    ids.extend(ancestor_node_ids(root, node_id));
    for id in ids {
        let Some(node) = crate::dom::find_node_by_id_ref(root, &id.to_string()) else {
            continue;
        };
        if node.tag != Tag::Link {
            continue;
        }
        return node
            .attributes
            .get("href")
            .map(|href| href.trim().to_string())
            .filter(|href| !href.is_empty() && !href.starts_with('#'));
    }
    None
}

/// Ancestor node ids of `node_id`, nearest first (excluding the node itself).
/// Used to bubble clicks from a text run up to the element containing it
/// (e.g. a `<div class="tab">Label</div>` where the label is a Text child).
pub(super) fn ancestor_node_ids(root: &crate::dom::DomNode, node_id: usize) -> Vec<usize> {
    fn walk(node: &crate::dom::DomNode, id: usize, chain: &mut Vec<usize>) -> bool {
        if node.id == id {
            return true;
        }
        for child in &node.children {
            if walk(child, id, chain) {
                chain.push(node.id);
                return true;
            }
        }
        false
    }
    let mut chain = Vec::new();
    walk(root, node_id, &mut chain);
    chain
}

/// Collect `(node_id, text)` for every text-bearing node in document order.
pub(super) fn collect_text_spans(node: &crate::dom::DomNode, out: &mut Vec<(usize, String)>) {
    if let Some(text) = &node.text {
        if !text.is_empty() {
            out.push((node.id, text.clone()));
        }
    }
    for child in &node.children {
        collect_text_spans(child, out);
    }
}

/// Extract the selected text between `(start_node, start_char)` and
/// `(end_node, end_char)`, walking the DOM in document order. Handles reversed
/// ranges and multi-node selections.
pub(super) fn compute_selected_text(
    root: &crate::dom::DomNode,
    start_node: usize,
    start_char: usize,
    end_node: usize,
    end_char: usize,
) -> String {
    let mut spans = Vec::new();
    collect_text_spans(root, &mut spans);

    let start_idx = spans.iter().position(|s| s.0 == start_node);
    let end_idx = spans.iter().position(|s| s.0 == end_node);
    let (Some(si), Some(ei)) = (start_idx, end_idx) else {
        return String::new();
    };

    // Normalize to a forward range.
    let (s_idx, s_char, e_idx, e_char) = if (si, start_char) <= (ei, end_char) {
        (si, start_char, ei, end_char)
    } else {
        (ei, end_char, si, start_char)
    };

    let mut out = String::new();
    if s_idx == e_idx {
        let text = &spans[s_idx].1;
        out.push_str(
            &text
                .chars()
                .skip(s_char)
                .take(e_char.saturating_sub(s_char))
                .collect::<String>(),
        );
    } else {
        out.push_str(&spans[s_idx].1.chars().skip(s_char).collect::<String>());
        for span in &spans[s_idx + 1..e_idx] {
            out.push_str(&span.1);
        }
        out.push_str(&spans[e_idx].1.chars().take(e_char).collect::<String>());
    }
    out
}

/// Delete the text between `(start_node, start_char)` and
/// `(end_node, end_char)`, walking the DOM in document order (the same range
/// [`compute_selected_text`] reads). Empty text nodes are removed. Returns
/// `false` when either endpoint is no longer a text-bearing node.
pub(super) fn delete_selected_text(
    root: &mut crate::dom::DomNode,
    start_node: usize,
    start_char: usize,
    end_node: usize,
    end_char: usize,
) -> bool {
    let mut spans = Vec::new();
    collect_text_spans(root, &mut spans);

    let Some(si) = spans.iter().position(|s| s.0 == start_node) else {
        return false;
    };
    let Some(ei) = spans.iter().position(|s| s.0 == end_node) else {
        return false;
    };

    // Normalize to a forward range, exactly like compute_selected_text.
    let (s_idx, s_char, e_idx, e_char) = if (si, start_char) <= (ei, end_char) {
        (si, start_char, ei, end_char)
    } else {
        (ei, end_char, si, start_char)
    };

    let mut edits: Vec<(usize, Option<String>)> = Vec::new();
    for (idx, (node_id, text)) in spans.iter().enumerate().take(e_idx + 1).skip(s_idx) {
        let chars: Vec<char> = text.chars().collect();
        let kept: Vec<char> = if s_idx == e_idx {
            let from = s_char.min(chars.len());
            let to = e_char.min(chars.len());
            chars[..from].iter().chain(&chars[to..]).copied().collect()
        } else if idx == s_idx {
            chars[..s_char.min(chars.len())].to_vec()
        } else if idx == e_idx {
            chars[e_char.min(chars.len())..].to_vec()
        } else {
            Vec::new()
        };
        let new_text: String = kept.into_iter().collect();
        edits.push((
            *node_id,
            if new_text.is_empty() {
                None
            } else {
                Some(new_text)
            },
        ));
    }

    for (node_id, text) in edits {
        if let Some(node) = find_node_by_id(root, &node_id.to_string()) {
            node.text = text;
        }
    }
    true
}
