//! `.pss` stylesheet application for the runtime `pwb.loadStyle` binding.

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
