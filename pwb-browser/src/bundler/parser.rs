//! Minimal PML/HTML markup parser producing a `PlainNode` tree.

use pwb_core::spec::{PlainNode, Tag};
use std::collections::HashMap;

/// Node ids for the devtools subtree start here so they can never collide with ids
/// assigned by the wire decoder for the main bundle (which start at 1).
pub(super) const DEVTOOLS_ID_OFFSET: usize = 100_000;

/// Parse a simple PML/HTML markup string into a PlainNode tree.
///
/// Handles attributes (`id`, `class`, ...), inline text on the same line as the
/// opening tag (`<div class="tab">Elements</div>`), comments, and void elements
/// (`<!DOCTYPE html>`, `<link ...>`, `<input ...>`).
pub(super) fn parse_pml(content: &str) -> PlainNode {
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
                attach_text(&mut stack, &mut id_counter, text);
            }
            cursor = end;
        } else {
            let text = content[cursor..].trim();
            if !text.is_empty() {
                attach_text(&mut stack, &mut id_counter, text);
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
fn attach_text(stack: &mut [PlainNode], id_counter: &mut usize, text: &str) {
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
    (start..bytes.len()).find(|&i| bytes[i] == target)
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
