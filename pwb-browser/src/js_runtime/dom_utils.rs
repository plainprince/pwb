//! DOM traversal and manipulation helpers shared across the JS runtime bindings.

use crate::dom::DomNode;
use std::collections::HashMap;

/// Parse a tiny JSON object (`{"k": "v", ...}`) into string params. Values
/// are unquoted; a bare `{}`/empty string yields an empty map.
pub(crate) fn parse_props_json(props_json: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let trimmed = props_json.trim();
    let body = trimmed
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(trimmed);
    for part in body.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once(':') {
            let key = k.trim().trim_matches('"').trim_matches('\'').to_string();
            let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !key.is_empty() {
                out.insert(key, val);
            }
        }
    }
    out
}

/// Replace `{param}` placeholders in a DOM subtree's attributes and text.
pub(crate) fn substitute_params_dom(node: &mut DomNode, params: &HashMap<String, String>) {
    for v in node.attributes.values_mut() {
        for (key, value) in params {
            *v = v.replace(&format!("{{{}}}", key), value);
        }
    }
    if let Some(text) = &mut node.text {
        for (key, value) in params {
            *text = text.replace(&format!("{{{}}}", key), value);
        }
    }
    for child in &mut node.children {
        substitute_params_dom(child, params);
    }
}

/// Reassign node ids in a subtree starting at `base` (document order).
pub(crate) fn reassign_ids(node: &mut DomNode, base: usize) -> usize {
    let mut next = base;
    fn walk(node: &mut DomNode, next: &mut usize) {
        node.id = *next;
        *next += 1;
        for child in &mut node.children {
            walk(child, next);
        }
    }
    walk(node, &mut next);
    next - 1
}

pub(crate) fn max_node_id(node: &DomNode) -> usize {
    let mut max = node.id;
    for child in &node.children {
        max = max.max(max_node_id(child));
    }
    max
}

/// Escape a string for safe interpolation into a JS string literal.
pub(crate) fn js_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "")
}

pub(crate) fn find_node_by_id<'a>(node: &'a DomNode, id: &str) -> Option<&'a DomNode> {
    if node.id.to_string() == id || node.attributes.get("id").map(|v| v == id).unwrap_or(false) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_by_id(child, id) {
            return Some(found);
        }
    }
    None
}

pub(crate) fn find_node_by_selector<'a>(node: &'a DomNode, selector: &str) -> Option<&'a DomNode> {
    // Attribute selector: [attr] or [attr=value]
    if selector.starts_with('[') && selector.ends_with(']') {
        let inner = &selector[1..selector.len() - 1];
        let (attr, expected) = inner
            .split_once('=')
            .map(|(a, v)| {
                (
                    a.trim(),
                    Some(v.trim().trim_matches('"').trim_matches('\'')),
                )
            })
            .unwrap_or((inner, None));
        let matches = match node.attributes.get(attr) {
            Some(v) => expected.map(|e| v == e).unwrap_or(true),
            None => false,
        };
        if matches {
            return Some(node);
        }
    }
    if selector.starts_with('#') {
        // ID selector: matches the `id` attribute or the numeric node id.
        return find_node_by_id(node, selector.strip_prefix('#').unwrap_or(selector));
    }
    if selector.starts_with('.') {
        // Class selector - check attributes
        if let Some(class_attr) = node.attributes.get("class") {
            if class_attr.split_whitespace().any(|c| c == &selector[1..]) {
                return Some(node);
            }
        }
    }
    // Tag selector or plain numeric node id.
    if node.tag.name() == selector || node.id.to_string() == selector {
        return Some(node);
    }
    // Check children
    for child in &node.children {
        if let Some(found) = find_node_by_selector(child, selector) {
            return Some(found);
        }
    }
    None
}

pub(crate) fn find_node_by_selector_mut<'a>(
    node: &'a mut DomNode,
    selector: &str,
) -> Option<&'a mut DomNode> {
    if selector.starts_with('[') && selector.ends_with(']') {
        let inner = &selector[1..selector.len() - 1];
        let (attr, expected) = inner
            .split_once('=')
            .map(|(a, v)| {
                (
                    a.trim(),
                    Some(v.trim().trim_matches('"').trim_matches('\'')),
                )
            })
            .unwrap_or((inner, None));
        let matches = match node.attributes.get(attr) {
            Some(v) => expected.map(|e| v == e).unwrap_or(true),
            None => false,
        };
        if matches {
            return Some(node);
        }
    }

    let id = selector.strip_prefix('#').unwrap_or(selector);

    if selector.starts_with('#') {
        return find_node_by_id_mut(node, id);
    }

    if selector.starts_with('.') {
        if let Some(class_attr) = node.attributes.get("class") {
            if class_attr.split_whitespace().any(|c| c == &selector[1..]) {
                return Some(node);
            }
        }
    }

    if node.tag.name() == selector || node.id.to_string() == selector {
        return Some(node);
    }

    for child in node.children.iter_mut() {
        if let Some(found) = find_node_by_selector_mut(child, selector) {
            return Some(found);
        }
    }
    None
}

pub(crate) fn find_node_by_id_mut<'a>(node: &'a mut DomNode, id: &str) -> Option<&'a mut DomNode> {
    if node.id.to_string() == id || node.attributes.get("id").map(|v| v == id).unwrap_or(false) {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_node_by_id_mut(child, id) {
            return Some(found);
        }
    }
    None
}

/// Remove the node with `target_id` from the tree (searching every level) and
/// return it, so it can be kept alive as a detached node (`el.remove()`).
pub(crate) fn remove_node_from_tree(node: &mut DomNode, target_id: usize) -> Option<DomNode> {
    if let Some(pos) = node.children.iter().position(|c| c.id == target_id) {
        return Some(node.children.remove(pos));
    }
    for child in &mut node.children {
        if let Some(found) = remove_node_from_tree(child, target_id) {
            return Some(found);
        }
    }
    None
}
