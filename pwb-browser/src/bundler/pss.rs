//! `.pss` stylesheet parsing and attribute flattening.

use pwb_core::spec::{PlainNode, Tag};

/// Parse a `.pss` stylesheet into `(selector -> ordered list of (property, value))`.
/// Order is preserved so later rules override earlier ones. Handles `/* comments */`
/// and comma-separated selector lists.
/// Apply a PSS text to a live DOM tree at runtime (the `pwb.loadStyle(css)`
/// binding): rules are parsed into raw attributes and inserted onto matching
/// nodes (tag, class, and id selectors — the same matching the compiler uses).
pub fn apply_pss_to_dom(root: &mut crate::dom::DomNode, css_text: &str) -> bool {
    let rules = parse_pss_to_attrs(css_text);
    if rules.is_empty() {
        return false;
    }
    fn apply(node: &mut crate::dom::DomNode, rules: &[(String, Vec<(String, String)>)]) {
        let tag_name = node.tag.name();
        for (selector, attrs) in rules {
            let matches = if selector.starts_with('#') {
                node.attributes
                    .get("id")
                    .map(|id| format!("#{}", id.trim()) == *selector)
                    .unwrap_or(false)
            } else if selector.starts_with('.') {
                let required: Vec<&str> = selector
                    .strip_prefix('.')
                    .unwrap_or(selector)
                    .split('.')
                    .filter(|c| !c.is_empty())
                    .collect();
                if let Some(class_attr) = node.attributes.get("class") {
                    let classes: Vec<&str> = class_attr.split_whitespace().collect();
                    !required.is_empty() && required.iter().all(|c| classes.contains(c))
                } else {
                    false
                }
            } else {
                selector == tag_name
            };
            if matches {
                for (k, v) in attrs {
                    node.attributes.insert(k.clone(), v.clone());
                }
            }
        }
        for child in &mut node.children {
            apply(child, rules);
        }
    }
    apply(root, &rules);
    true
}

pub(crate) fn parse_pss_to_attrs(content: &str) -> Vec<(String, Vec<(String, String)>)> {
    let cleaned = strip_pss_comments(content);
    let mut rules = Vec::new();
    for block in cleaned.split('}') {
        if let Some((selector_part, body)) = block.split_once('{') {
            let mut properties = Vec::new();
            for line in body.split(';') {
                if let Some((prop_k, prop_v)) = line.split_once(':') {
                    let k = prop_k.trim().to_lowercase();
                    let v = prop_v.trim();
                    if !k.is_empty() && !v.is_empty() {
                        properties.push((k, v.to_string()));
                    }
                }
            }
            if properties.is_empty() {
                continue;
            }
            for selector in selector_part.split(',') {
                let selector = selector.trim().to_lowercase();
                if !selector.is_empty() {
                    rules.push((selector, properties.clone()));
                }
            }
        }
    }
    rules
}

fn strip_pss_comments(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(pos) = rest.find("/*") {
        out.push_str(&rest[..pos]);
        if let Some(end) = rest[pos + 2..].find("*/") {
            rest = &rest[pos + 2 + end + 2..];
        } else {
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

pub(super) fn apply_stylesheet_attrs(
    node: &mut PlainNode,
    rules: &[(String, Vec<(String, String)>)],
) {
    for (selector, props) in rules {
        let matched = if let Some(tag_selector) = selector.strip_prefix("tag:") {
            node.tag.name() == tag_selector.trim()
        } else if selector.starts_with('#') {
            node.attributes
                .get("id")
                .map(|id| id == &selector[1..])
                .unwrap_or(false)
        } else if let Some(sel_classes) = selector.strip_prefix('.') {
            let required: Vec<&str> = sel_classes.split('.').filter(|c| !c.is_empty()).collect();
            let classes: Vec<&str> = node
                .attributes
                .get("class")
                .map(|c| c.split_whitespace().collect())
                .unwrap_or_default();
            !required.is_empty() && required.iter().all(|c| classes.contains(c))
        } else {
            node.tag.name() == selector
        };

        if matched {
            for (k, v) in props {
                node.attributes.insert(k.clone(), v.clone());
            }
        }
    }

    for child in &mut node.children {
        apply_stylesheet_attrs(child, rules);
        // Text children inherit the parent's `font-size` when unset (e.g. the
        // devtools tab labels inside a font-sized container), so inline text
        // matches its surroundings instead of jumping to the 3vh default.
        if child.tag == Tag::Text {
            if !child.attributes.contains_key("font-size") {
                if let Some(fs) = node.attributes.get("font-size") {
                    child.attributes.insert("font-size".to_string(), fs.clone());
                }
            }
            if !child.attributes.contains_key("color") {
                if let Some(c) = node.attributes.get("color") {
                    child.attributes.insert("color".to_string(), c.clone());
                }
            }
        }
    }
}
