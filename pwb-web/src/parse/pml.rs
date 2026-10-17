use pwb_core::spec::{PlainNode, Tag};
use std::collections::HashMap;

use super::error::ParseError;

/// Parse custom `.pml` (Plain Markup Language) text.
pub fn parse_pml_markup(input: &str) -> Result<PlainNode, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::EmptySource);
    }

    let mut cursor = 0;
    let bytes = trimmed.as_bytes();
    let mut node_stack: Vec<PlainNode> = Vec::new();
    let mut root: Option<PlainNode> = None;
    let mut id_counter = 0;

    while cursor < bytes.len() {
        if bytes[cursor] == b'<' {
            if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'/' {
                let end = find_char(bytes, cursor, b'>')
                    .ok_or_else(|| ParseError::SyntaxError("Unclosed closing tag".to_string()))?;
                cursor = end + 1;

                if let Some(finished_node) = node_stack.pop() {
                    if let Some(parent) = node_stack.last_mut() {
                        parent.children.push(finished_node);
                    } else {
                        root = Some(finished_node);
                    }
                }
            } else {
                let end = find_char(bytes, cursor, b'>')
                    .ok_or_else(|| ParseError::SyntaxError("Unclosed opening tag".to_string()))?;
                let tag_content = std::str::from_utf8(&bytes[cursor + 1..end])
                    .map_err(|e| ParseError::SyntaxError(e.to_string()))?
                    .trim();

                let self_closing = tag_content.ends_with('/');
                let (tag_name, mut attrs) = parse_tag_content(tag_content.trim_end_matches('/'));
                id_counter += 1;

                let tag_enum = match tag_name.to_lowercase().as_str() {
                    "plain" | "document" => Tag::Document,
                    "head" => Tag::Head,
                    "title" => Tag::Title,
                    "view" => Tag::View,
                    "text" => Tag::Text,
                    "image" => Tag::Image,
                    "button" => Tag::Button,
                    "input" => Tag::Input,
                    "link" => Tag::Link,
                    "script" => Tag::Script,
                    "component" => Tag::Component,
                    "import" => Tag::Import,
                    "icon" => Tag::Icon,
                    other if other.starts_with("icon-") => Tag::Icon,
                    "code" => Tag::Code,
                    other => return Err(ParseError::InvalidTag(other.to_string())),
                };

                // `<icon-home>` encodes the icon name in the tag suffix; record
                // it as the `icon` attribute (falling back to an explicit
                // `name` attr on `<icon name="...">`).
                if tag_enum == Tag::Icon && !attrs.contains_key("icon") {
                    if let Some(suffix) = tag_name.to_lowercase().strip_prefix("icon-") {
                        attrs.insert("icon".to_string(), suffix.to_string());
                    } else if let Some(name) = attrs.get("name") {
                        attrs.insert("icon".to_string(), name.clone());
                    }
                }

                // `<script>` bodies are raw text (may contain `<`); consume
                // everything up to `</script>` verbatim.
                if tag_enum == Tag::Script && !self_closing {
                    let script_end = input[end + 1..]
                        .find("</script>")
                        .map(|i| end + 1 + i)
                        .ok_or_else(|| {
                            ParseError::SyntaxError("Unclosed <script> tag".to_string())
                        })?;
                    let raw = &input[end + 1..script_end];
                    let mut node = PlainNode::new(id_counter, tag_enum);
                    node.attributes = attrs;
                    node.text_content = Some(raw.to_string());
                    if let Some(parent) = node_stack.last_mut() {
                        parent.children.push(node);
                    } else {
                        root = Some(node);
                    }
                    cursor = script_end + "</script>".len();
                    continue;
                }

                // HTML void elements (`<input>`, `<img>`, `<br>`, ...) have no
                // closing tag: attach immediately instead of pushing an open frame
                // that would swallow subsequent siblings as children.
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
                ) || tag_enum == Tag::Icon;
                if void_element {
                    let mut node = PlainNode::new(id_counter, tag_enum);
                    node.attributes = attrs;
                    if let Some(parent) = node_stack.last_mut() {
                        parent.children.push(node);
                    } else if root.is_none() {
                        root = Some(node);
                    }
                    cursor = end + 1;
                    continue;
                }

                let mut node = PlainNode::new(id_counter, tag_enum);
                node.attributes = attrs;

                node_stack.push(node);
                cursor = end + 1;

                // Self-closing `<tag/>`: immediately attach to the parent.
                if self_closing {
                    if let Some(finished_node) = node_stack.pop() {
                        if let Some(parent) = node_stack.last_mut() {
                            parent.children.push(finished_node);
                        } else {
                            root = Some(finished_node);
                        }
                    }
                }
            }
        } else {
            let text_end = find_char(bytes, cursor, b'<').unwrap_or(bytes.len());
            let text = std::str::from_utf8(&bytes[cursor..text_end])
                .map_err(|e| ParseError::SyntaxError(e.to_string()))?;

            let is_raw_script = node_stack
                .last()
                .map(|n| n.tag == Tag::Script)
                .unwrap_or(false);
            let trimmed = text.trim();
            if !trimmed.is_empty() || is_raw_script {
                if let Some(current) = node_stack.last_mut() {
                    current.text_content = Some(if is_raw_script {
                        text.to_string()
                    } else {
                        trimmed.to_string()
                    });
                }
            }
            cursor = text_end;
        }
    }

    root.or_else(|| node_stack.pop())
        .ok_or_else(|| ParseError::SyntaxError("No valid root node found".to_string()))
}

/// Walk a parsed PML tree and pull out the bundling primitives:
fn find_char(bytes: &[u8], start: usize, target: u8) -> Option<usize> {
    (start..bytes.len()).find(|&i| bytes[i] == target)
}

pub(crate) fn parse_tag_content(content: &str) -> (String, HashMap<String, String>) {
    let content = content.trim();
    if content.is_empty() {
        return ("view".to_string(), HashMap::new());
    }

    let tag_name = content
        .split_whitespace()
        .next()
        .unwrap_or("view")
        .to_string();

    let rest = content[tag_name.len()..].trim();
    let mut attrs = HashMap::new();

    let bytes = rest.as_bytes();
    let mut i = 0;
    let len = bytes.len();

    while i < len {
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }

        let key_start = i;
        while i < len && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = rest[key_start..i].trim();
        if key.is_empty() {
            break;
        }

        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        if i < len && bytes[i] == b'=' {
            i += 1;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }

            if i < len {
                let quote = bytes[i];
                if quote == b'"' || quote == b'\'' {
                    i += 1;
                    let val_start = i;
                    while i < len && bytes[i] != quote {
                        i += 1;
                    }
                    let val = &rest[val_start..i];
                    if i < len && bytes[i] == quote {
                        i += 1;
                    }
                    attrs.insert(key.to_string(), val.to_string());
                } else {
                    let val_start = i;
                    while i < len && !bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    let val = &rest[val_start..i];
                    attrs.insert(key.to_string(), val.to_string());
                }
            } else {
                attrs.insert(key.to_string(), String::new());
            }
        } else {
            attrs.insert(key.to_string(), "true".to_string());
        }
    }

    (tag_name, attrs)
}
