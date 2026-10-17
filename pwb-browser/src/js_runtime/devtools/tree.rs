//! Devtools tree collection, rebuild, and selection highlighting.

use crate::dom::{Dom, DomNode};
use pwb_core::spec::Tag;
use std::collections::HashSet;

use super::super::dom_utils::{
    find_node_by_id_mut, find_node_by_selector, find_node_by_selector_mut, max_node_id,
};
use super::tree_row::{make_close_row, make_leaf_row, make_open_row, resolve_style_width};

/// Walk a DOM subtree (skipping the mounted devtools root) and emit one
/// pipe-separated line per tree row: `node_id|depth|tag|id|text|kind`, where
/// `kind` is `o` (opening-tag row), `c` (closing-tag row, emitted after the
/// children) or `l` (a single leaf row carrying the element's text).
pub(crate) fn collect_dom_tree_lines(node: &DomNode, depth: usize, out: &mut String) {
    if node.attributes.contains_key("data-devtools") {
        return;
    }
    let id_attr = node.attributes.get("id").cloned().unwrap_or_default();
    // Icons show as `<icon-home>` so the tree reflects the actual element.
    let tag_label = if node.tag == Tag::Icon {
        node.attributes
            .get("icon")
            .map(|n| format!("icon-{}", n))
            .unwrap_or_else(|| "icon".to_string())
    } else {
        node.tag.name().to_string()
    };
    if node.children.is_empty() {
        // `<input>` leaves show their live `value` (or their text) so the tree
        // reflects what is typed into them.
        let text_attr = node
            .text
            .as_ref()
            .map(|t| t.trim().chars().take(40).collect::<String>())
            .or_else(|| {
                node.attributes
                    .get("value")
                    .map(|v| v.chars().take(40).collect())
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "{}|{}|{}|{}|{}|l\n",
            node.id, depth, tag_label, id_attr, text_attr
        ));
    } else {
        out.push_str(&format!(
            "{}|{}|{}|{}||o\n",
            node.id, depth, tag_label, id_attr
        ));
        for child in &node.children {
            collect_dom_tree_lines(child, depth + 1, out);
        }
        out.push_str(&format!("{}|{}|{}|||c\n", node.id, depth, tag_label));
    }
}

/// Rebuild the Elements tree from the live DOM (the devtools subtree is
/// excluded), re-applying the selected row's highlight. Fresh rows replace the
/// previous ones entirely, so node ids grow monotonically but the
/// `tree-row-{node_id}` keys stay stable. `vw`/`vh` are the real window
/// dimensions (physical px), used to budget leaf-row content so closing tags
/// never overflow the tree.
pub(crate) fn rebuild_tree_rows(dom: &mut Dom, selected: Option<usize>, vw: f32, vh: f32) {
    let Some(tree_id) = find_node_by_selector(&dom.root, "#elements-tree").map(|n| n.id) else {
        return;
    };
    let mut lines = String::new();
    collect_dom_tree_lines(&dom.root, 0, &mut lines);

    // Truncation budget: the sidebar's content width minus the tab-content,
    // tree and row paddings and the scrollbar track — i.e. the visible width
    // available for one row's text children.
    let sidebar_w = find_node_by_selector(&dom.root, ".devtools-sidebar")
        .and_then(|n| n.attributes.get("width"))
        .and_then(|w| resolve_style_width(w, vw))
        .unwrap_or(0.25 * vw);
    let pad = |v: f32| v * vh;
    let budget = (sidebar_w - pad(0.013) * 2.0 - pad(0.0033) * 4.0 - 12.0).max(60.0);

    // Only the selected element is highlighted — its opening AND closing rows
    // (and the single text row when it's a leaf) — but never its child
    // elements, matching a browser inspector's single-element selection.
    let selected_ids: HashSet<usize> = selected.into_iter().collect();

    let mut next_id = max_node_id(&dom.root) + 1;
    let mut rows = Vec::new();
    for line in lines.lines() {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 6 || parts[0].is_empty() {
            continue;
        }
        let node_id = parts[0];
        let node_id_num: usize = node_id.parse().unwrap_or(0);
        let depth: usize = parts[1].parse().unwrap_or(0);
        let indent = "  ".repeat(depth);
        let tag = parts[2];
        let id = parts.get(3).copied().unwrap_or("");
        let text = parts.get(4).copied().unwrap_or("");
        let kind = parts[5];
        let (mut row, _is_node_row) = match kind {
            "c" => (make_close_row(node_id, &mut next_id, &indent, tag), false),
            "l" => (
                make_leaf_row(
                    &format!("tree-row-{}", node_id),
                    &mut next_id,
                    &indent,
                    tag,
                    id,
                    text,
                    budget,
                    vh,
                ),
                true,
            ),
            _ => (
                make_open_row(
                    &format!("tree-row-{}", node_id),
                    node_id,
                    &mut next_id,
                    &indent,
                    tag,
                    id,
                ),
                true,
            ),
        };
        if selected_ids.contains(&node_id_num) {
            row.attributes
                .insert("background-color".to_string(), "#094771".to_string());
        }
        rows.push(row);
    }
    if let Some(tree) = find_node_by_id_mut(&mut dom.root, &tree_id.to_string()) {
        tree.children = rows;
    }
}

/// Apply the selection highlight to the row for `id` and every row of its
/// descendant elements (the element's contents). Missing rows (e.g. text nodes,
/// which have no rows of their own) are skipped.
pub(crate) fn highlight_selected_node(dom: &mut Dom, id: usize) {
    // Highlight exactly one element in the tree: its opening row, its closing
    // row (multi-line elements), and its single text row (leaf). Every other
    // row is un-highlighted so picking a new element deselects the previous
    // selection. Rows are matched by `data-node` — the attribute every row
    // (opening, closing, and leaf) carries for the element it represents.
    if let Some(sel) = find_node_by_selector_mut(&mut dom.root, "#elements-tree") {
        for row in &mut sel.children {
            let is_selected = row
                .attributes
                .get("data-node")
                .and_then(|v| v.parse::<usize>().ok())
                == Some(id);
            if is_selected {
                row.attributes
                    .insert("background-color".to_string(), "#094771".to_string());
            } else {
                row.attributes.remove("background-color");
            }
        }
    }
}
