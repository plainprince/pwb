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

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::spec::Tag;

    fn node(id: usize, tag: Tag) -> DomNode {
        DomNode {
            id,
            tag,
            attributes: HashMap::new(),
            text: None,
            children: Vec::new(),
        }
    }

    fn with_attr(mut n: DomNode, key: &str, value: &str) -> DomNode {
        n.attributes.insert(key.to_string(), value.to_string());
        n
    }

    /// A two-level tree: root(1) -> mid(2) -> leaf(3), plus sibling(4).
    fn tree() -> DomNode {
        let mut mid = node(2, Tag::View);
        mid.children
            .push(with_attr(node(3, Tag::Text), "id", "leaf"));
        let mut root = node(1, Tag::Document);
        root.children.push(mid);
        root.children
            .push(with_attr(node(4, Tag::View), "class", "box active"));
        root
    }

    #[test]
    fn parse_props_json_handles_quotes_blanks_and_shape() {
        assert_eq!(
            parse_props_json(r#"{"title": "Hello", "count": "5"}"#),
            HashMap::from([
                ("title".to_string(), "Hello".to_string()),
                ("count".to_string(), "5".to_string()),
            ])
        );
        // Bare / empty bodies and stray fragments yield an empty map.
        assert!(parse_props_json("{}").is_empty());
        assert!(parse_props_json("").is_empty());
        assert!(parse_props_json("   ").is_empty());
        assert!(
            parse_props_json("{ , , }").is_empty(),
            "blank parts skipped"
        );
        assert!(
            parse_props_json(r#"{ "": "x" }"#).is_empty(),
            "empty key skipped"
        );
        assert!(
            parse_props_json("{ novalue }").is_empty(),
            "part without ':' skipped"
        );
        // Unbraced input still parses (the braces are optional).
        assert_eq!(
            parse_props_json("k: v"),
            HashMap::from([("k".to_string(), "v".to_string())])
        );
    }

    #[test]
    fn substitute_params_dom_walks_attributes_text_and_children() {
        let mut root = with_attr(node(1, Tag::View), "class", "{theme}-card");
        root.text = Some("Hi {name}".to_string());
        let mut child = node(2, Tag::Text);
        child.text = Some("{name} / {name}".to_string());
        root.children.push(child);

        let params = HashMap::from([
            ("theme".to_string(), "dark".to_string()),
            ("name".to_string(), "Ada".to_string()),
        ]);
        substitute_params_dom(&mut root, &params);

        assert_eq!(
            root.attributes.get("class").map(String::as_str),
            Some("dark-card")
        );
        assert_eq!(root.text.as_deref(), Some("Hi Ada"));
        assert_eq!(root.children[0].text.as_deref(), Some("Ada / Ada"));
    }

    #[test]
    fn reassign_ids_renumbers_in_document_order_and_max_node_id_reads_the_tree() {
        let mut root = tree();
        assert_eq!(max_node_id(&root), 4);
        let last = reassign_ids(&mut root, 10);
        assert_eq!(last, 13, "four nodes renumbered 10..=13");
        let ids: Vec<usize> = {
            fn walk(n: &DomNode, out: &mut Vec<usize>) {
                out.push(n.id);
                for c in &n.children {
                    walk(c, out);
                }
            }
            let mut ids = Vec::new();
            walk(&root, &mut ids);
            ids
        };
        assert_eq!(ids, vec![10, 11, 12, 13], "document order preserved");
        assert_eq!(max_node_id(&root), 13);
    }

    #[test]
    fn js_escape_escapes_quotes_backslashes_and_newlines() {
        assert_eq!(js_escape("plain"), "plain");
        assert_eq!(js_escape("a\\b"), "a\\\\b");
        assert_eq!(js_escape("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(js_escape("line\nbreak"), "line\\nbreak");
        assert_eq!(js_escape("c\r"), "c", "carriage returns are stripped");
    }

    #[test]
    fn find_node_by_id_matches_numeric_and_attribute_ids() {
        let root = tree();
        assert_eq!(find_node_by_id(&root, "3").map(|n| n.id), Some(3));
        assert_eq!(find_node_by_id(&root, "leaf").map(|n| n.id), Some(3));
        assert_eq!(find_node_by_id(&root, "1").map(|n| n.id), Some(1));
        assert!(find_node_by_id(&root, "99").is_none());
    }

    #[test]
    fn find_node_by_selector_supports_attr_id_class_and_tag() {
        let root = tree();
        // Attribute selectors, with and without an expected value.
        assert_eq!(find_node_by_selector(&root, "[id]").map(|n| n.id), Some(3));
        assert_eq!(
            find_node_by_selector(&root, "[id=leaf]").map(|n| n.id),
            Some(3)
        );
        assert!(find_node_by_selector(&root, "[id=nope]").is_none());
        assert!(find_node_by_selector(&root, "[missing-attr]").is_none());
        // ID, class, tag and numeric selectors.
        assert_eq!(find_node_by_selector(&root, "#leaf").map(|n| n.id), Some(3));
        assert_eq!(find_node_by_selector(&root, "#4").map(|n| n.id), Some(4));
        assert_eq!(
            find_node_by_selector(&root, ".active").map(|n| n.id),
            Some(4)
        );
        assert!(find_node_by_selector(&root, ".inactive").is_none());
        assert_eq!(find_node_by_selector(&root, "text").map(|n| n.id), Some(3));
        assert_eq!(find_node_by_selector(&root, "1").map(|n| n.id), Some(1));
    }

    #[test]
    fn find_node_by_selector_mut_returns_mutable_hits() {
        let mut root = tree();
        assert_eq!(
            find_node_by_selector_mut(&mut root, "[id=leaf]").map(|n| n.id),
            Some(3)
        );
        assert_eq!(
            find_node_by_selector_mut(&mut root, "[id]").map(|n| n.id),
            Some(3)
        );
        assert!(find_node_by_selector_mut(&mut root, "[id=nope]").is_none());
        assert!(find_node_by_selector_mut(&mut root, "[missing]").is_none());
        assert_eq!(
            find_node_by_selector_mut(&mut root, "#3").map(|n| n.id),
            Some(3)
        );
        assert_eq!(
            find_node_by_selector_mut(&mut root, ".active").map(|n| n.id),
            Some(4)
        );
        assert!(find_node_by_selector_mut(&mut root, ".inactive").is_none());
        // Tag selectors use the PML tag name: the document root is `<plain>`.
        assert_eq!(
            find_node_by_selector_mut(&mut root, "plain").map(|n| n.id),
            Some(1)
        );
        // A matching tag on a nested node.
        assert_eq!(
            find_node_by_selector_mut(&mut root, "text").map(|n| n.id),
            Some(3)
        );
        // Mutability: the hit can be modified in place.
        if let Some(n) = find_node_by_selector_mut(&mut root, "#leaf") {
            n.text = Some("changed".to_string());
        }
        assert_eq!(
            find_node_by_selector(&root, "#leaf")
                .unwrap()
                .text
                .as_deref(),
            Some("changed")
        );
    }

    #[test]
    fn find_node_by_id_mut_returns_a_mutable_hit() {
        let mut root = tree();
        assert_eq!(find_node_by_id_mut(&mut root, "2").map(|n| n.id), Some(2));
        assert_eq!(
            find_node_by_id_mut(&mut root, "leaf").map(|n| n.id),
            Some(3)
        );
        assert!(find_node_by_id_mut(&mut root, "99").is_none());
    }

    #[test]
    fn remove_node_from_tree_pulls_direct_and_deep_children() {
        let mut root = tree();
        // Direct child.
        let removed = remove_node_from_tree(&mut root, 4).expect("sibling removed");
        assert_eq!(removed.id, 4);
        assert!(root.children.iter().all(|c| c.id != 4));
        // Deep child (inside mid).
        let removed = remove_node_from_tree(&mut root, 3).expect("leaf removed");
        assert_eq!(removed.id, 3);
        assert!(find_node_by_id(&root, "3").is_none());
        // Absent id.
        assert!(remove_node_from_tree(&mut root, 99).is_none());
    }
}
