//! PlainWeb Bundler Utility (`pwb-browser::bundler`)
//!
//! Creates `.pwb` bundles from source directories containing `.pml`, `.pss`, and `.pjs` files.
//! Used to load the built-in `devtools` directory into the running page as a mounted DOM.

use pwb_core::spec::{PlainNode, Tag};
use pwb_core::wire::Bundle;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum BundlerError {
    Io(std::io::Error),
    InvalidManifest(String),
}

impl From<std::io::Error> for BundlerError {
    fn from(err: std::io::Error) -> Self {
        BundlerError::Io(err)
    }
}

/// Node ids for the devtools subtree start here so they can never collide with ids
/// assigned by the wire decoder for the main bundle (which start at 1).
const DEVTOOLS_ID_OFFSET: usize = 100_000;

/// Create a simple PlainWeb bundle from a devtools directory.
pub fn bundle_from_dir(source_dir: &Path) -> Result<Bundle, BundlerError> {
    // Read the PML markup file
    let pml_path = source_dir.join("index.pml");
    if !pml_path.exists() {
        return Err(BundlerError::InvalidManifest(
            "index.pml not found".to_string(),
        ));
    }
    let pml_content = fs::read_to_string(pml_path)?;

    // Parse PML into DOM nodes (simplified parser)
    let mut root_node = parse_pml(&pml_content);

    // Apply PSS stylesheet rules (flattened into attributes so the runtime style
    // engine picks them up).
    let pss_path = source_dir.join("style.pss");
    if pss_path.exists() {
        if let Ok(pss_content) = fs::read_to_string(pss_path) {
            let rules = parse_pss_to_attrs(&pss_content);
            apply_stylesheet_attrs(&mut root_node, &rules);
        }
    }

    // Read the JS script if present
    let mut script_payload = None;
    let pjs_path = source_dir.join("app.pjs");
    if pjs_path.exists() {
        script_payload = Some(fs::read(pjs_path)?);
    }

    Ok(Bundle {
        version: pwb_core::wire::CURRENT_VERSION,
        title: "DevTools".to_string(),
        author_pubkey: None,
        root_node,
        script_payload,
        components: std::collections::HashMap::new(),
        modules: std::collections::HashMap::new(),
    })
}

/// Parse a simple PML/HTML markup string into a PlainNode tree.
///
/// Handles attributes (`id`, `class`, ...), inline text on the same line as the
/// opening tag (`<div class="tab">Elements</div>`), comments, and void elements
/// (`<!DOCTYPE html>`, `<link ...>`, `<input ...>`).
fn parse_pml(content: &str) -> PlainNode {
    let mut id_counter = DEVTOOLS_ID_OFFSET;

    fn parse_tag(tag: &str) -> Tag {
        match tag.to_lowercase().as_str() {
            "plain" | "document" | "html" | "body" | "div" | "section" => Tag::View,
            "head" => Tag::Head,
            "title" => Tag::Title,
            "text" | "span" | "p" | "label" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Tag::Text,
            "image" | "img" => Tag::Image,
            "button" => Tag::Button,
            "input" => Tag::Input,
            "link" => Tag::Link,
            "code" => Tag::Code,
            "icon" => Tag::Icon,
            other if other.starts_with("icon-") => Tag::Icon,
            _ => Tag::View,
        }
    }

    fn extract_attrs(tag_content: &str) -> HashMap<String, String> {
        let mut attrs = HashMap::new();
        let mut rest = tag_content.trim();
        // Skip the tag name itself.
        if let Some(i) = rest.find(char::is_whitespace) {
            rest = &rest[i..];
        } else {
            return attrs;
        }

        let bytes = rest.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let key_start = i;
            while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let key = rest[key_start..i].trim().to_string();
            if key.is_empty() {
                break;
            }
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'=' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                // Quoted values may contain spaces; bare values run to whitespace.
                let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                    let quote = bytes[i];
                    i += 1;
                    let v_start = i;
                    while i < bytes.len() && bytes[i] != quote {
                        i += 1;
                    }
                    let v = rest[v_start..i].to_string();
                    if i < bytes.len() {
                        i += 1; // skip closing quote
                    }
                    v
                } else {
                    let v_start = i;
                    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    rest[v_start..i].to_string()
                };
                attrs.insert(key, value);
            } else {
                // Boolean attribute without a value.
                attrs.insert(key, String::new());
            }
        }
        attrs
    }

    // Tokenize the whole document into (Open(tag, attrs), Close, Text) events.
    let mut stack: Vec<PlainNode> = Vec::new();
    let mut root: Option<PlainNode> = None;
    let bytes = content.as_bytes();
    let mut cursor = 0;

    let take_node = |stack: &mut Vec<PlainNode>, root: &mut Option<PlainNode>| {
        if let Some(finished) = stack.pop() {
            if let Some(parent) = stack.last_mut() {
                parent.children.push(finished);
            } else if root.is_none() {
                *root = Some(finished);
            }
        }
    };

    while cursor < bytes.len() {
        // Comment: <!-- ... -->
        if bytes[cursor..].starts_with(b"<!--") {
            if let Some(end) = find_sub(bytes, cursor + 4, b"-->") {
                cursor = end + 3;
                continue;
            }
        }

        if bytes[cursor] == b'<' {
            // Closing tag: pop one level.
            if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'/' {
                if let Some(end) = find_char(bytes, cursor, b'>') {
                    cursor = end + 1;
                    take_node(&mut stack, &mut root);
                    continue;
                }
            }
            // Opening tag.
            if let Some(end) = find_char(bytes, cursor, b'>') {
                let tag_content = &content[cursor + 1..end];
                let tag_content = tag_content.trim();
                let tag_name = tag_content.split_whitespace().next().unwrap_or("view");

                // Skip doctype. Self-closing tags are real elements below (they
                // attach immediately like void elements, so `<icon-home/>` works).
                if tag_name.starts_with('!') {
                    cursor = end + 1;
                    continue;
                }
                let self_closing = tag_content.ends_with('/');
                let tag_content = if self_closing {
                    tag_content.trim_end_matches('/').trim_end()
                } else {
                    tag_content
                };
                // Re-derive the tag name from the trimmed content so the trailing
                // `/` never leaks into `tag_name` (e.g. `icon-home` not `icon-home/`).
                let tag_name = tag_content.split_whitespace().next().unwrap_or("view");

                // Metadata elements (`link`/`script`/`meta`) reference external files the
                // bundler loads separately. Skip through their matching closing tag so the
                // closing tag does not pop the wrong stack frame.
                let is_metadata =
                    matches!(tag_name.to_lowercase().as_str(), "link" | "script" | "meta");
                if is_metadata {
                    let needle = format!("</{}", tag_name.to_lowercase());
                    let rest = content[end + 1..].to_lowercase();
                    if let Some(rel) = rest.find(&needle) {
                        let after_open = end + 1 + rel + needle.len();
                        if let Some(close_end) = find_char(bytes, after_open, b'>') {
                            cursor = close_end + 1;
                            continue;
                        }
                    }
                    cursor = end + 1;
                    continue;
                }

                let attrs = extract_attrs(tag_content);
                let tag = parse_tag(tag_name);
                // `<icon-home>` / `<icon name="...">` → the icon name lives in the
                // `icon` attribute for layout/render.
                let mut attrs = attrs;
                if tag == Tag::Icon && !attrs.contains_key("icon") {
                    if let Some(suffix) = tag_name.to_lowercase().strip_prefix("icon-") {
                        attrs.insert("icon".to_string(), suffix.to_string());
                    } else if let Some(name) = attrs.get("name") {
                        attrs.insert("icon".to_string(), name.clone());
                    }
                }
                id_counter += 1;
                let node = PlainNode {
                    id: id_counter,
                    tag,
                    attributes: attrs,
                    styles: Vec::new(),
                    text_content: None,
                    children: Vec::new(),
                };

                // HTML void elements have no closing tag: attach immediately
                // instead of pushing an open frame that would swallow subsequent
                // siblings as children.
                let tag_lower = tag_name.to_lowercase();
                let void_element = matches!(
                    tag_lower.as_str(),
                    "area"
                        | "base"
                        | "br"
                        | "col"
                        | "embed"
                        | "hr"
                        | "img"
                        | "input"
                        | "param"
                        | "source"
                        | "track"
                        | "wbr"
                ) || tag == Tag::Icon;
                if void_element || self_closing {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    } else if root.is_none() {
                        root = Some(node);
                    }
                    cursor = end + 1;
                    continue;
                }

                stack.push(node);
                cursor = end + 1;
                continue;
            }
        }

        // Text content until the next '<'.
        if let Some(end) = find_char(bytes, cursor, b'<') {
            let text = content[cursor..end].trim();
            if !text.is_empty() {
                attach_text(&mut stack, &mut id_counter, &text);
            }
            cursor = end;
        } else {
            let text = content[cursor..].trim();
            if !text.is_empty() {
                attach_text(&mut stack, &mut id_counter, &text);
            }
            break;
        }
    }

    while stack.len() > 1 {
        take_node(&mut stack, &mut root);
    }

    root.or_else(|| stack.pop()).unwrap_or_else(|| PlainNode {
        id: DEVTOOLS_ID_OFFSET,
        tag: Tag::View,
        attributes: HashMap::new(),
        styles: Vec::new(),
        text_content: Some(content.to_string()),
        children: Vec::new(),
    })
}

/// Attach a text run to the current element. Views (e.g. `<div>Label</div>`)
/// get a `Text` child node so the label is actually rendered; leaf elements
/// (`title`, `button`, `text`, `input`) store it as `text_content`.
fn attach_text(stack: &mut Vec<PlainNode>, id_counter: &mut usize, text: &str) {
    let Some(parent) = stack.last_mut() else {
        return;
    };
    match parent.tag {
        Tag::View => {
            *id_counter += 1;
            parent.children.push(PlainNode {
                id: *id_counter,
                tag: Tag::Text,
                attributes: HashMap::new(),
                styles: Vec::new(),
                text_content: Some(text.to_string()),
                children: Vec::new(),
            });
        }
        _ => {
            parent.text_content = Some(text.to_string());
        }
    }
}

fn find_char(bytes: &[u8], start: usize, target: u8) -> Option<usize> {
    for i in start..bytes.len() {
        if bytes[i] == target {
            return Some(i);
        }
    }
    None
}

fn find_sub(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    if start >= bytes.len() {
        return None;
    }
    bytes[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + start)
}

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
                let required: Vec<&str> =
                    selector[1..].split('.').filter(|c| !c.is_empty()).collect();
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

fn apply_stylesheet_attrs(node: &mut PlainNode, rules: &[(String, Vec<(String, String)>)]) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_bundle_from_dir() {
        let test_dir = PathBuf::from("/tmp/test_devtools");
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(
            test_dir.join("index.pml"),
            "<view style='background: red;'><text>Hello</text></view>",
        )
        .unwrap();
        fs::write(test_dir.join("app.pjs"), "// empty").unwrap();

        let bundle = bundle_from_dir(&test_dir).unwrap();
        assert_eq!(bundle.title, "DevTools");
        assert!(bundle.script_payload.is_some());
    }

    #[test]
    fn test_text_inherits_parent_font_size() {
        // Inline text inside a font-sized container inherits the container's
        // `font-size` (the devtools tab labels) instead of the 3vh default.
        let test_dir = PathBuf::from(format!("/tmp/pwb_inherit_{}", std::process::id()));
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(
            test_dir.join("index.pml"),
            "<plain><view class=\"tab\" id=\"t\">Label</view></plain>",
        )
        .unwrap();
        fs::write(test_dir.join("style.pss"), ".tab { font-size: 2vh; }").unwrap();
        let bundle = bundle_from_dir(&test_dir).unwrap();
        fs::remove_dir_all(&test_dir).ok();
        fn find_text<'a>(n: &'a PlainNode, out: &mut Vec<&'a PlainNode>) {
            if n.tag == Tag::Text {
                out.push(n);
            }
            for c in &n.children {
                find_text(c, out);
            }
        }
        let mut texts = vec![];
        find_text(&bundle.root_node, &mut texts);
        assert_eq!(texts.len(), 1, "one text node from the inline label");
        assert_eq!(
            texts[0].attributes.get("font-size").map(|s| s.as_str()),
            Some("2vh"),
            "text inherits the parent's font-size"
        );
    }

    #[test]
    fn test_parse_pml_attributes_and_inline_text() {
        let root = parse_pml(
            r#"<!DOCTYPE html>
<html>
<head><title>DevTools</title></head>
<body>
    <div class="devtools-sidebar" id="sidebar">
        <div class="tab active" data-tab="elements">Elements</div>
        <button id="close-devtools">×</button>
    </div>
</body>
</html>"#,
        );

        // Root should be the <html> view.
        assert_eq!(root.tag, Tag::View);

        let sidebar = find_attr_node(&root, "id", "sidebar").expect("sidebar div parsed");
        assert_eq!(
            sidebar.attributes.get("class").map(|s| s.as_str()),
            Some("devtools-sidebar")
        );

        let tab = find_attr_node(&root, "data-tab", "elements").expect("tab div parsed");
        // Inline text in a View becomes a Text child node so it actually renders.
        let text_child = tab.children.first().expect("tab has a text child");
        assert_eq!(text_child.tag, Tag::Text);
        assert_eq!(text_child.text_content.as_deref(), Some("Elements"));

        let close = find_attr_node(&root, "id", "close-devtools").expect("close button parsed");
        assert_eq!(close.tag, Tag::Button);
        assert_eq!(close.text_content.as_deref(), Some("×"));
    }

    #[test]
    fn test_parse_pml_void_elements_do_not_swallow_siblings() {
        // HTML void elements (no closing tag) must attach to the parent
        // immediately; otherwise a subsequent element becomes their child.
        let root = parse_pml(
            "<view id='container'><input type='text'><view id='next'>after</view></view>",
        );
        let container = find_attr_node(&root, "id", "container").expect("container");
        assert_eq!(
            container.children.len(),
            2,
            "input and next view must be siblings"
        );
        assert_eq!(container.children[0].tag, Tag::Input);
        let next = find_attr_node(&root, "id", "next").expect("next view is a sibling");
        assert_eq!(next.children[0].text_content.as_deref(), Some("after"));

        // `<img>`/`<br>` are void too.
        let root2 = parse_pml("<view id='c'><img src='x.png'><br><view id='z'></view></view>");
        let c = find_attr_node(&root2, "id", "c").unwrap();
        assert_eq!(c.children.len(), 3);
        assert!(
            find_attr_node(&root2, "id", "z").is_some(),
            "element after <br> is a sibling"
        );
    }

    #[test]
    fn test_parse_pml_icon_elements() {
        // `<icon-home/>` parses as Tag::Icon with `icon="home"`, self-closing
        // (the next element is a sibling), and `<icon name="star">` works too.
        let root = parse_pml(
            "<view id='r'><icon-home/><icon name='star'/><view id='after'>x</view></view>",
        );
        let row = find_attr_node(&root, "id", "r").expect("row");
        assert_eq!(
            row.children.len(),
            3,
            "icons are void: next element stays a sibling"
        );
        assert_eq!(row.children[0].tag, Tag::Icon);
        assert_eq!(
            row.children[0].attributes.get("icon").map(|s| s.as_str()),
            Some("home")
        );
        assert_eq!(row.children[1].tag, Tag::Icon);
        assert_eq!(
            row.children[1].attributes.get("icon").map(|s| s.as_str()),
            Some("star")
        );
        assert_eq!(
            row.children[2].attributes.get("id").map(|s| s.as_str()),
            Some("after")
        );
    }

    #[test]
    fn test_parse_pml_code_element() {
        // `<code>` maps to Tag::Code and keeps its text.
        let root = parse_pml("<view id='r'><code>let s = 'hi';</code></view>");
        let row = find_attr_node(&root, "id", "r").expect("row");
        let code = row
            .children
            .iter()
            .find(|c| c.tag == Tag::Code)
            .expect("code node");
        assert_eq!(code.text_content.as_deref(), Some("let s = 'hi';"));
    }

    #[test]
    fn test_parse_pml_ids_start_high() {
        let root = parse_pml("<view id='a'><view id='b'></view></view>");
        assert!(root.id >= DEVTOOLS_ID_OFFSET);
        let child = root.children.first().unwrap();
        assert!(child.id > root.id);
    }

    #[test]
    fn test_pss_attrs_flattened() {
        let mut root = parse_pml(
            "<view class=\"devtools-sidebar\"><div class=\"tab active\">Elements</div></view>",
        );
        let rules = parse_pss_to_attrs(
            ".devtools-sidebar { position: fixed; right: 0; top: 0; width: 400px; height: 100%; background-color: #1e1e1e; }\n.tab.active { background-color: #0078d7; color: white; }",
        );
        apply_stylesheet_attrs(&mut root, &rules);

        let sidebar = find_attr_node(&root, "class", "devtools-sidebar").unwrap();
        assert_eq!(
            sidebar.attributes.get("position").map(|s| s.as_str()),
            Some("fixed")
        );
        assert_eq!(
            sidebar.attributes.get("width").map(|s| s.as_str()),
            Some("400px")
        );
        assert_eq!(
            sidebar
                .attributes
                .get("background-color")
                .map(|s| s.as_str()),
            Some("#1e1e1e")
        );

        let tab = find_attr_node(&root, "class", "tab active").unwrap();
        assert_eq!(
            tab.attributes.get("background-color").map(|s| s.as_str()),
            Some("#0078d7")
        );
    }

    fn find_attr_node<'a>(node: &'a PlainNode, key: &str, value: &str) -> Option<&'a PlainNode> {
        if node.attributes.get(key).map(|v| v.as_str()) == Some(value) {
            return Some(node);
        }
        for child in &node.children {
            if let Some(found) = find_attr_node(child, key, value) {
                return Some(found);
            }
        }
        None
    }
}
