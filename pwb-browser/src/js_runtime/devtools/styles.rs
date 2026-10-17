//! Devtools styles panel rendering.

use crate::dom::{Dom, DomNode};
use pwb_core::spec::Tag;
use std::collections::HashMap;

use super::super::dom_utils::{
    find_node_by_id, find_node_by_id_mut, find_node_by_selector, max_node_id,
};
use super::tree_row::text_child_sized;

/// Render the bottom styles panel: the selected element's attributes (minus
/// structural `id`/`class`/`data-*`) as `prop: value` rows, or a hint when
/// nothing is selected.
pub(crate) fn render_styles_panel(dom: &mut Dom, selected: Option<usize>) {
    let Some(panel_id) = find_node_by_selector(&dom.root, "#styles-panel").map(|n| n.id) else {
        return;
    };
    let attrs: Vec<(String, String)> =
        match selected.and_then(|id| find_node_by_id(&dom.root, &id.to_string())) {
            Some(node) => {
                let mut a: Vec<(String, String)> = node
                    .attributes
                    .iter()
                    .filter(|(k, _)| {
                        k.as_str() != "id" && k.as_str() != "class" && !k.starts_with("data-")
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                a.sort();
                a
            }
            None => Vec::new(),
        };

    let mut next_id = max_node_id(&dom.root) + 1;
    let mut children = Vec::new();
    children.push(text_child_sized(
        next_id,
        "Styles".to_string(),
        "#cccccc",
        "2vh",
    ));
    next_id += 1;
    if attrs.is_empty() {
        let hint = if selected.is_some() {
            "No styles".to_string()
        } else {
            "Select an element to inspect its styles.".to_string()
        };
        children.push(text_child_sized(next_id, hint, "#6a9955", "2vh"));
    } else {
        for (k, v) in attrs {
            let mut row_attrs = HashMap::new();
            row_attrs.insert("flex-direction".to_string(), "row".to_string());
            row_attrs.insert("gap".to_string(), "0.5vh".to_string());
            row_attrs.insert("padding".to_string(), "0.2vh".to_string());
            children.push(DomNode {
                id: next_id,
                tag: Tag::View,
                attributes: row_attrs,
                text: None,
                children: vec![
                    text_child_sized(next_id + 1, k, "#9cdcfe", "2vh"),
                    text_child_sized(next_id + 2, format!(": {}", v), "#d4d4d4", "2vh"),
                ],
            });
            next_id += 3;
        }
    }
    if let Some(panel) = find_node_by_id_mut(&mut dom.root, &panel_id.to_string()) {
        panel.children = children;
    }
}
