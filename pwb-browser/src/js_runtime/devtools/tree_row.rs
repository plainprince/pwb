//! Syntax-highlighted devtools tree-row construction.

use crate::dom::DomNode;
use crate::layout::measure_text;
use pwb_core::spec::Tag;
use std::collections::HashMap;

/// A colored `Text` node used for syntax-highlighted devtools tree rows.
fn text_child(id: usize, text: String, color: &str) -> DomNode {
    text_child_sized(id, text, color, "1.8vh")
}

/// Like [`text_child`] but with an explicit font size (styles panel uses 2vh).
pub(crate) fn text_child_sized(id: usize, text: String, color: &str, font_size: &str) -> DomNode {
    let mut attrs = HashMap::new();
    attrs.insert("color".to_string(), color.to_string());
    attrs.insert("font-size".to_string(), font_size.to_string());
    DomNode {
        id,
        tag: Tag::Text,
        attributes: attrs,
        text: Some(text),
        children: Vec::new(),
    }
}

/// One syntax-highlighted Elements row: a flex-row View whose `id` is
/// `tree-row-{node_id}` (so picking an element can highlight its row), with
/// colored `<tag`, `#id`, `> text`, `</tag>` parts. Used by the console's
/// `__pwb_appendTreeRow` binding (a leaf-style row without width budgeting).
pub(crate) fn make_tree_row(
    row_id: &str,
    next_id: &mut usize,
    indent: &str,
    tag: &str,
    id: &str,
    text: &str,
) -> DomNode {
    let mut attrs = HashMap::new();
    attrs.insert("id".to_string(), row_id.to_string());
    attrs.insert("flex-direction".to_string(), "row".to_string());
    attrs.insert("gap".to_string(), "0".to_string());
    attrs.insert("padding".to_string(), "0.33vh".to_string());
    let mut row_children = Vec::new();
    // Opening tag with closing `>` baked in: `<tag>` or `<tag #id>`.
    let open = if id.is_empty() {
        format!("{}<{}>", indent, tag)
    } else {
        format!("{}<{} #{}>", indent, tag, id)
    };
    row_children.push(text_child(*next_id, open, "#569cd6"));
    *next_id += 1;
    if !text.is_empty() {
        // One space before content, matching Chrome DevTools.
        row_children.push(text_child(*next_id, format!(" {}", text), "#d4d4d4"));
        *next_id += 1;
    }
    row_children.push(text_child(*next_id, format!("</{}>", tag), "#569cd6"));
    *next_id += 1;
    // Same unique-id rule as `tree_row_container`: take the current slot for
    // the container and advance, so it never collides with another node.
    let container_id = *next_id;
    *next_id += 1;
    DomNode {
        id: container_id,
        tag: Tag::View,
        attributes: attrs,
        text: None,
        children: row_children,
    }
}

/// Shared row scaffolding: a flex-row View with the given text children.
/// `row_id` becomes the row's `id` (empty for closing-tag rows).
fn tree_row_container(
    row_id: &str,
    data_node: Option<&str>,
    next_id: &mut usize,
    children: Vec<DomNode>,
) -> DomNode {
    let mut attrs = HashMap::new();
    if !row_id.is_empty() {
        attrs.insert("id".to_string(), row_id.to_string());
    }
    // Every row — opening, closing, and leaf alike — carries the original DOM
    // node it represents, so the pipeline can select the element when any part
    // of the row (including the `</tag>` line) is clicked.
    if let Some(node_id) = data_node {
        attrs.insert("data-node".to_string(), node_id.to_string());
    }
    attrs.insert("flex-direction".to_string(), "row".to_string());
    attrs.insert("gap".to_string(), "0".to_string());
    attrs.insert("padding".to_string(), "0.33vh".to_string());
    // Give the container a node id from the SAME monotonic counter as its text
    // children and advance past it, so sibling rows never share ids. (The old
    // `id: *next_id + 1` without advancing left the container's id equal to the
    // next row's first text child; the pipeline resolves a clicked row's
    // `data-node` by walking the DOM for the hit node's id, so a duplicate id
    // made clicking a row's text select the wrong element — e.g. clicking
    // `<input #demo-input>` selected the `<icon-github>` row above it.)
    let container_id = *next_id;
    *next_id += 1;
    DomNode {
        id: container_id,
        tag: Tag::View,
        attributes: attrs,
        text: None,
        children,
    }
}

/// Opening-tag row for a container element: `<tag` + optional `#id`. The
/// closing tag is its own row, emitted after the children (see
/// [`collect_dom_tree_lines`]), so containers read like a browser's tree:
/// `<head>` on one row, children below, `</head>` after them.
pub(crate) fn make_open_row(
    row_id: &str,
    data_node: &str,
    next_id: &mut usize,
    indent: &str,
    tag: &str,
    id: &str,
) -> DomNode {
    let mut children = Vec::new();
    // Opening tag with closing `>` baked in: `<tag>` or `<tag #id>`.
    let open = if id.is_empty() {
        format!("{}<{}>", indent, tag)
    } else {
        format!("{}<{} #{}>", indent, tag, id)
    };
    children.push(text_child(*next_id, open, "#569cd6"));
    *next_id += 1;
    tree_row_container(row_id, Some(data_node), next_id, children)
}

/// Closing-tag row: `</tag>` alone on its own line, indented like its opening
/// row, placed after the element's children. Carries `data-node` (but no
/// `tree-row-*` id) so clicking it selects the same element as its opening row.
pub(crate) fn make_close_row(
    data_node: &str,
    next_id: &mut usize,
    indent: &str,
    tag: &str,
) -> DomNode {
    let mut children = Vec::new();
    children.push(text_child(
        *next_id,
        format!("{}</{}>", indent, tag),
        "#569cd6",
    ));
    *next_id += 1;
    tree_row_container("", Some(data_node), next_id, children)
}

/// Leaf row: `<tag` `#id` `> text` `</tag>` on one line. The content text is
/// width-truncated with a `…` ellipsis so the closing tag always stays visible
/// within `budget_px` (the tree's content width minus the scrollbar).
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_leaf_row(
    row_id: &str,
    next_id: &mut usize,
    indent: &str,
    tag: &str,
    id: &str,
    text: &str,
    budget_px: f32,
    vh: f32,
) -> DomNode {
    let font_px = 0.018 * vh; // 1.8vh tree font
                              // Opening tag with `>` baked in: `<tag>` or `<tag #id>`.
    let open_txt = if id.is_empty() {
        format!("{}<{}>", indent, tag)
    } else {
        format!("{}<{} #{}>", indent, tag, id)
    };
    let close_txt = format!("</{}>", tag);
    // Fixed parts: `<tag>` and `</tag>` — no gap padding.
    let fixed_w = measure_text(&open_txt, font_px) + measure_text(&close_txt, font_px);
    let content_avail = (budget_px - fixed_w).max(12.0);
    let content = fit_text_width(text, content_avail, font_px);

    let mut children = Vec::new();
    children.push(text_child(*next_id, open_txt, "#569cd6"));
    *next_id += 1;
    if !content.is_empty() {
        // One space before content, matching Chrome DevTools.
        children.push(text_child(*next_id, format!(" {}", content), "#d4d4d4"));
        *next_id += 1;
    }
    children.push(text_child(*next_id, close_txt, "#569cd6"));
    *next_id += 1;
    let data_node = row_id.strip_prefix("tree-row-").unwrap_or("");
    tree_row_container(row_id, Some(data_node), next_id, children)
}

/// Shorten `text` (with a trailing `…`) so it fits within `avail_px` at
/// `font_px`, matching how the row is drawn. Returns the text unchanged when
/// it already fits. (The opening tag includes `>`, so only the content text
/// is budgeted here.)
fn fit_text_width(text: &str, avail_px: f32, font_px: f32) -> String {
    if text.is_empty() {
        return String::new();
    }
    if measure_text(text, font_px) <= avail_px {
        return text.to_string();
    }
    let mut out: String = text.chars().collect();
    while out.chars().count() > 1 {
        out.pop();
        if measure_text(&format!("{}…", out), font_px) <= avail_px {
            return format!("{}…", out);
        }
    }
    "…".to_string()
}

/// Resolve a style `width` attribute (`25vw` or `450px`) into physical pixels
/// for the given viewport width. `vw` values are percentages of the viewport
/// (the style engine stores `25vw` as the fraction 25), matching the layout's
/// `UnitValue::Vw` resolution.
pub(crate) fn resolve_style_width(width: &str, vw: f32) -> Option<f32> {
    let w = width.trim();
    if let Some(rest) = w.strip_suffix("vw") {
        rest.trim().parse::<f32>().ok().map(|f| (f / 100.0) * vw)
    } else if let Some(rest) = w.strip_suffix("px") {
        rest.trim().parse::<f32>().ok()
    } else {
        w.parse::<f32>().ok()
    }
}
