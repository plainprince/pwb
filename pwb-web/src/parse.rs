//! Parser for PlainWeb site directories and `.pml`, `.pss`, `.js` files (`pwb-web::parse`).

use pwb_core::spec::{Color, PlainNode, StyleProperty, Tag, UnitValue};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum ParseError {
    EmptySource,
    InvalidTag(String),
    SyntaxError(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for ParseError {
    fn from(err: std::io::Error) -> Self {
        ParseError::Io(err)
    }
}

/// Parsed site payload: the page tree, the bundled script, the component
/// registry, and the named script-module map.
pub struct ParsedSite {
    pub root_node: PlainNode,
    pub title: String,
    pub script_payload: Option<Vec<u8>>,
    /// Component name → component body (from `<component name="...">`).
    pub components: HashMap<String, PlainNode>,
    /// Module name → script bytes, for the runtime `pwb.import`/`pwb.hotload`.
    pub modules: HashMap<String, Vec<u8>>,
}

/// Parse an entire site directory containing `.pml`, `.pss`, and optional `.js` files.
pub fn parse_site_directory<P: AsRef<Path>>(dir_path: P) -> Result<ParsedSite, ParseError> {
    let path = dir_path.as_ref();
    if !path.exists() {
        return Err(ParseError::SyntaxError(format!(
            "Directory or file does not exist: {}",
            path.display()
        )));
    }

    if path.is_file() {
        let content = fs::read_to_string(path)?;
        let mut root = parse_pml_markup(&content)?;
        let mut components = HashMap::new();
        let mut js_entries = Vec::new();
        let dir = path.parent().unwrap_or(Path::new("."));
        extract_bundlables(&mut root, dir, &mut components, &mut js_entries)?;
        resolve_imports(&mut root, &components, 0);
        return Ok(ParsedSite {
            title: extract_title(&root),
            root_node: root,
            script_payload: None,
            components,
            modules: HashMap::new(),
        });
    }

    // Deterministic file collection: PML pages + components, PSS (native),
    // CSS (lightningcss-bundled), and PJS/JS (bundled into one non-minified
    // script plus a named module map).
    let mut pml_files: Vec<String> = Vec::new();
    let mut pss_files: Vec<String> = Vec::new();
    let mut css_files: Vec<String> = Vec::new();
    let mut js_files: Vec<String> = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_file() {
            if let Some(name) = entry_path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
                match entry_path.extension().and_then(|s| s.to_str()) {
                    Some("pml") => pml_files.push(name.to_string()),
                    Some("pss") => pss_files.push(name.to_string()),
                    Some("css") => css_files.push(name.to_string()),
                    Some("pjs") | Some("js") => js_files.push(name.to_string()),
                    _ => {}
                }
            }
        }
    }
    pml_files.sort();
    pss_files.sort();
    css_files.sort();
    js_files.sort();

    // HTML bundling: parse every `.pml` file, pull `<component>`/`<script>`
    // definitions out into the registry/bundle, and merge the page content.
    let mut components: HashMap<String, PlainNode> = HashMap::new();
    let mut js_entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut merged_root: Option<PlainNode> = None;
    for pml in &pml_files {
        let text = fs::read_to_string(path.join(pml))?;
        let mut tree = parse_pml_markup(&text)?;
        extract_bundlables(&mut tree, path, &mut components, &mut js_entries)?;
        if let Some(root) = &mut merged_root {
            root.children.append(&mut tree.children);
        } else {
            merged_root = Some(tree);
        }
    }
    let mut root_node = merged_root.ok_or(ParseError::EmptySource)?;

    // Resolve `<import component="...">` usages by inlining the component body
    // with `{param}` substitution.
    resolve_imports(&mut root_node, &components, 0);

    // Styles: native `.pss` concatenated, `.css` bundled via lightningcss
    // (non-minified, `@import` resolved) — then applied as one stylesheet.
    let mut stylesheet = String::new();
    for pss in &pss_files {
        stylesheet.push_str(&fs::read_to_string(path.join(pss))?);
        stylesheet.push('\n');
    }
    let css_bundle = crate::bundler::bundle_css_files(path, &css_files);
    if !css_bundle.is_empty() {
        stylesheet.push_str(&css_bundle);
        stylesheet.push('\n');
    }
    if !stylesheet.trim().is_empty() {
        let rules = parse_pss_stylesheet(&stylesheet);
        apply_stylesheet(&mut root_node, &rules);
    }

    // Scripts: directory files plus anything referenced by `<script src>` or
    // defined inline, deduplicated by name and bundled non-minified. The entry
    // (`app.pjs`/`main.pjs`/`index.pjs` when present) runs last so its
    // dependencies are defined first.
    for js in &js_files {
        js_entries.push((js.clone(), fs::read(path.join(js))?));
    }
    js_entries.sort_by(|a, b| a.0.cmp(&b.0));
    js_entries.dedup_by(|a, b| a.0 == b.0);

    let entry_name = ["app.pjs", "main.pjs", "index.pjs"]
        .iter()
        .find(|n| js_entries.iter().any(|(name, _)| name == *n))
        .map(|s| *s);
    let (script_payload, modules) = crate::bundler::bundle_js_files(&js_entries, entry_name);
    let title = extract_title(&root_node);

    Ok(ParsedSite {
        root_node,
        title,
        script_payload: if script_payload.is_empty() {
            None
        } else {
            Some(script_payload)
        },
        components,
        modules,
    })
}

fn extract_title(node: &PlainNode) -> String {
    if node.tag == Tag::Title {
        if let Some(txt) = &node.text_content {
            return txt.clone();
        }
    }
    if let Some(title_attr) = node.attributes.get("title") {
        return title_attr.clone();
    }
    for child in &node.children {
        let child_title = extract_title(child);
        if child_title != "PlainWeb Site" {
            return child_title;
        }
    }
    "PlainWeb Site".to_string()
}

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
///
/// - `<component name="X">…</component>` (inline or `src="file.pml"`) → the
///   component registry (the element itself is removed from the page).
/// - `<script src="x.pjs">` / inline `<script>…</script>` → bundled JS
///   entries (the element is removed; `<import>` elements are left in place
///   for [`resolve_imports`]).
fn extract_bundlables(
    node: &mut PlainNode,
    dir: &Path,
    components: &mut HashMap<String, PlainNode>,
    js_entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), ParseError> {
    let mut kept = Vec::with_capacity(node.children.len());
    let mut inline_idx = 0usize;
    for mut child in std::mem::take(&mut node.children) {
        match child.tag {
            Tag::Component => {
                let name = child.attributes.get("name").cloned();
                if let Some(name) = name {
                    // `<component name="X" src="path.pml">` loads the body
                    // from a file; inline definitions use their children.
                    if let Some(src) = child.attributes.get("src").cloned() {
                        let src_path = dir.join(&src);
                        if let Ok(text) = fs::read_to_string(&src_path) {
                            if let Ok(mut src_tree) = parse_pml_markup(&text) {
                                if let Some(comp) = src_tree
                                    .children
                                    .iter_mut()
                                    .find(|c| c.tag == Tag::Component)
                                {
                                    child.children = std::mem::take(&mut comp.children);
                                } else {
                                    child.children = std::mem::take(&mut src_tree.children);
                                }
                            }
                        } else {
                            eprintln!("[pwb-web] Component '{}' src '{}' not found", name, src);
                        }
                    }
                    // The component body is a single node: the lone child when
                    // there is one, otherwise a View container for the rest
                    // (matching how a multi-root component reads in markup).
                    let mut children = std::mem::take(&mut child.children);
                    let body = if children.len() == 1 {
                        children.pop().unwrap()
                    } else {
                        PlainNode {
                            id: 0,
                            tag: Tag::View,
                            attributes: HashMap::new(),
                            styles: Vec::new(),
                            text_content: None,
                            children,
                        }
                    };
                    components.insert(name, body);
                }
            }
            Tag::Script => {
                if let Some(src) = child.attributes.get("src").cloned() {
                    let name = src.split('/').last().unwrap_or(&src).to_string();
                    if let Ok(bytes) = fs::read(dir.join(&src)) {
                        js_entries.push((name, bytes));
                    } else {
                        eprintln!("[pwb-web] <script src=\"{}\"> not found", src);
                    }
                } else if let Some(code) = child.text_content.clone() {
                    inline_idx += 1;
                    js_entries.push((format!("inline-{}.js", inline_idx), code.into_bytes()));
                }
            }
            _ => {
                extract_bundlables(&mut child, dir, components, js_entries)?;
                kept.push(child);
            }
        }
    }
    node.children = kept;
    Ok(())
}

/// Replace `<import component="X" a="b">` elements with the component's body,
/// substituting `{a}` placeholders in attributes and text with the import's
/// attribute values. Recurses into substituted bodies (depth-limited).
fn resolve_imports(node: &mut PlainNode, components: &HashMap<String, PlainNode>, depth: usize) {
    if depth > 8 {
        eprintln!("[pwb-web] Component import nesting too deep, aborting");
        return;
    }
    let mut kept = Vec::with_capacity(node.children.len());
    for mut child in std::mem::take(&mut node.children) {
        if child.tag == Tag::Import {
            if let Some(name) = child.attributes.get("component").cloned() {
                if let Some(body) = components.get(&name) {
                    let params: HashMap<String, String> = child
                        .attributes
                        .iter()
                        .filter(|(k, _)| k.as_str() != "component")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    let mut instance = body.clone();
                    substitute_params(&mut instance, &params);
                    // Imported components may themselves use `<import>`.
                    if instance.tag == Tag::Import {
                        resolve_imports(&mut instance, components, depth + 1);
                    } else {
                        for c in &mut instance.children {
                            resolve_imports(c, components, depth + 1);
                        }
                    }
                    kept.push(instance);
                    continue;
                }
            }
            eprintln!(
                "[pwb-web] Unknown component import: {:?}",
                child.attributes.get("component")
            );
            continue;
        }
        resolve_imports(&mut child, components, depth + 1);
        kept.push(child);
    }
    node.children = kept;
}

/// Replace `{key}` placeholders in a node's attributes and text with the given
/// parameter values (unknown placeholders are left untouched).
fn substitute_params(node: &mut PlainNode, params: &HashMap<String, String>) {
    for (_, v) in node.attributes.iter_mut() {
        *v = substitute_in_string(v, params);
    }
    if let Some(text) = &mut node.text_content {
        *text = substitute_in_string(text, params);
    }
    for child in &mut node.children {
        substitute_params(child, params);
    }
}

fn substitute_in_string(input: &str, params: &HashMap<String, String>) -> String {
    let mut out = input.to_string();
    for (key, value) in params {
        out = out.replace(&format!("{{{}}}", key), value);
    }
    out
}

fn find_char(bytes: &[u8], start: usize, target: u8) -> Option<usize> {
    for i in start..bytes.len() {
        if bytes[i] == target {
            return Some(i);
        }
    }
    None
}

fn parse_tag_content(content: &str) -> (String, HashMap<String, String>) {
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

/// Parse `.pss` Plain Stylesheet rules.
pub fn parse_pss_stylesheet(content: &str) -> HashMap<String, Vec<StyleProperty>> {
    let mut rules: HashMap<String, Vec<StyleProperty>> = HashMap::new();

    for block in content.split('}') {
        if let Some((selector_part, body)) = block.split_once('{') {
            let selector = selector_part.trim().to_lowercase();
            let mut properties = Vec::new();

            for line in body.split(';') {
                if let Some((prop_k, prop_v)) = line.split_once(':') {
                    let k = prop_k.trim().to_lowercase();
                    let v = prop_v.trim().to_lowercase();

                    match k.as_str() {
                        "color" => {
                            if let Some(color) = parse_hex_color(&v) {
                                properties.push(StyleProperty::Color(color));
                            }
                        }
                        "background-color" | "background" => {
                            if let Some(color) = parse_hex_color(&v) {
                                properties.push(StyleProperty::BackgroundColor(color));
                            }
                        }
                        "padding" => {
                            if let Some(uv) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Padding(uv));
                            }
                        }
                        "margin" => {
                            if let Some(uv) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Margin(uv));
                            }
                        }
                        "gap" => {
                            if let Some(uv) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Gap(uv));
                            }
                        }
                        "font-size" => {
                            if let Some(uv) = parse_unit_value(&v) {
                                properties.push(StyleProperty::FontSize(uv));
                            }
                        }
                        "border-radius" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::BorderRadius(val));
                            }
                        }
                        "position" => match v.as_str() {
                            "absolute" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Absolute,
                            )),
                            "relative" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Relative,
                            )),
                            "static" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Static,
                            )),
                            "fixed" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Fixed,
                            )),
                            "sticky" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Sticky,
                            )),
                            _ => {}
                        },
                        "width" => {
                            if let Some(unit) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Width(unit));
                            }
                        }
                        "height" => {
                            if let Some(unit) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Height(unit));
                            }
                        }
                        "top" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::Top(val));
                            }
                        }
                        "left" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::Left(val));
                            }
                        }
                        "right" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::Right(val));
                            }
                        }
                        "bottom" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::Bottom(val));
                            }
                        }
                        "cursor" => match v.as_str() {
                            "pointer" => properties
                                .push(StyleProperty::Cursor(pwb_core::spec::CursorStyle::Pointer)),
                            "text" => properties
                                .push(StyleProperty::Cursor(pwb_core::spec::CursorStyle::Text)),
                            "grab" => properties
                                .push(StyleProperty::Cursor(pwb_core::spec::CursorStyle::Grab)),
                            "crosshair" => properties.push(StyleProperty::Cursor(
                                pwb_core::spec::CursorStyle::Crosshair,
                            )),
                            "default" => properties
                                .push(StyleProperty::Cursor(pwb_core::spec::CursorStyle::Default)),
                            _ => {}
                        },
                        "display" => match v.as_str() {
                            "flex" => properties
                                .push(StyleProperty::Display(pwb_core::spec::DisplayStyle::Flex)),
                            "block" => properties
                                .push(StyleProperty::Display(pwb_core::spec::DisplayStyle::Block)),
                            "none" => properties
                                .push(StyleProperty::Display(pwb_core::spec::DisplayStyle::None)),
                            _ => {}
                        },
                        "flex-direction" => match v.as_str() {
                            "row" => properties
                                .push(StyleProperty::Direction(pwb_core::spec::FlexDirection::Row)),
                            "column" => properties.push(StyleProperty::Direction(
                                pwb_core::spec::FlexDirection::Column,
                            )),
                            _ => {}
                        },
                        "justify-content" => {
                            if let Some(alignment) = parse_alignment(&v) {
                                properties.push(StyleProperty::JustifyContent(alignment));
                            }
                        }
                        "align-items" => {
                            if let Some(alignment) = parse_alignment(&v) {
                                properties.push(StyleProperty::AlignItems(alignment));
                            }
                        }
                        "resize" => {
                            if let Some(resize) = parse_resize(&v) {
                                properties.push(StyleProperty::Resize(resize));
                            }
                        }
                        "overflow" => match v.as_str() {
                            "visible" => properties.push(StyleProperty::Overflow(
                                pwb_core::spec::OverflowStyle::Visible,
                            )),
                            "hidden" => properties.push(StyleProperty::Overflow(
                                pwb_core::spec::OverflowStyle::Hidden,
                            )),
                            "scroll" => properties.push(StyleProperty::Overflow(
                                pwb_core::spec::OverflowStyle::Scroll,
                            )),
                            "auto" => properties
                                .push(StyleProperty::Overflow(pwb_core::spec::OverflowStyle::Auto)),
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }

            if !selector.is_empty() && !properties.is_empty() {
                rules.entry(selector).or_default().extend(properties);
            }
        }
    }

    rules
}

/// Apply parsed stylesheet rules to a node tree (tag, class, and id selectors).
pub fn apply_stylesheet(node: &mut PlainNode, rules: &HashMap<String, Vec<StyleProperty>>) {
    let tag_name = node.tag.name();
    if let Some(props) = rules.get(tag_name) {
        apply_style_properties(node, props);
    }

    if let Some(class_attr) = node.attributes.get("class") {
        // Support compound selectors like `.tab.active` (all classes must be present).
        let class_attr = class_attr.clone();
        let classes: Vec<&str> = class_attr.split_whitespace().collect();
        for (selector, props) in rules {
            if let Some(sel_classes) = selector.strip_prefix('.') {
                let required: Vec<&str> =
                    sel_classes.split('.').filter(|c| !c.is_empty()).collect();
                if !required.is_empty() && required.iter().all(|c| classes.contains(c)) {
                    apply_style_properties(node, props);
                }
            }
        }
    }

    if let Some(id_attr) = node.attributes.get("id") {
        let id_selector = format!("#{}", id_attr.trim());
        if let Some(props) = rules.get(&id_selector) {
            apply_style_properties(node, props);
        }
    }

    for child in &mut node.children {
        apply_stylesheet(child, rules);
    }
}

/// Apply style properties to a node, recording them both in `styles` and flattened
/// into attributes so they survive the `.pwb` wire format (which only carries attributes).
fn apply_style_properties(node: &mut PlainNode, props: &[StyleProperty]) {
    node.styles.extend(props.iter().cloned());
    for prop in props {
        if let Some((k, v)) = style_property_to_attr(prop) {
            node.attributes.insert(k, v);
        }
    }
}

fn style_property_to_attr(prop: &StyleProperty) -> Option<(String, String)> {
    use pwb_core::spec::PositionStyle;
    let (k, v): (&str, String) = match prop {
        StyleProperty::BackgroundColor(c) => (
            "background-color",
            format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
        ),
        StyleProperty::Color(c) => ("color", format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)),
        StyleProperty::Padding(p) => ("padding", format_unit(p)),
        StyleProperty::Margin(m) => ("margin", format_unit(m)),
        StyleProperty::Gap(g) => ("gap", format_unit(g)),
        StyleProperty::FontSize(s) => ("font-size", format_unit(s)),
        StyleProperty::BorderRadius(r) => ("border-radius", format!("{}px", r)),
        StyleProperty::Position(p) => (
            "position",
            match p {
                PositionStyle::Static => "static".to_string(),
                PositionStyle::Relative => "relative".to_string(),
                PositionStyle::Absolute => "absolute".to_string(),
                PositionStyle::Fixed => "fixed".to_string(),
                PositionStyle::Sticky => "sticky".to_string(),
            },
        ),
        StyleProperty::Top(v) => ("top", format!("{}px", v)),
        StyleProperty::Left(v) => ("left", format!("{}px", v)),
        StyleProperty::Right(v) => ("right", format!("{}px", v)),
        StyleProperty::Bottom(v) => ("bottom", format!("{}px", v)),
        StyleProperty::Width(v) => ("width", format_unit(v)),
        StyleProperty::Height(v) => ("height", format_unit(v)),
        StyleProperty::Cursor(c) => ("cursor", format!("{:?}", c).to_lowercase()),
        StyleProperty::Display(d) => (
            "display",
            match d {
                pwb_core::spec::DisplayStyle::Flex => "flex".to_string(),
                pwb_core::spec::DisplayStyle::Block => "block".to_string(),
                pwb_core::spec::DisplayStyle::None => "none".to_string(),
            },
        ),
        StyleProperty::Direction(d) => (
            "flex-direction",
            match d {
                pwb_core::spec::FlexDirection::Row => "row".to_string(),
                pwb_core::spec::FlexDirection::Column => "column".to_string(),
            },
        ),
        StyleProperty::JustifyContent(a) => ("justify-content", format_alignment(a)),
        StyleProperty::AlignItems(a) => ("align-items", format_alignment(a)),
        StyleProperty::Resize(r) => (
            "resize",
            match r {
                pwb_core::spec::ResizeStyle::None => "none".to_string(),
                pwb_core::spec::ResizeStyle::Both => "both".to_string(),
                pwb_core::spec::ResizeStyle::Vertical => "vertical".to_string(),
                pwb_core::spec::ResizeStyle::Horizontal => "horizontal".to_string(),
            },
        ),
        StyleProperty::Overflow(o) => (
            "overflow",
            match o {
                pwb_core::spec::OverflowStyle::Visible => "visible".to_string(),
                pwb_core::spec::OverflowStyle::Hidden => "hidden".to_string(),
                pwb_core::spec::OverflowStyle::Scroll => "scroll".to_string(),
                pwb_core::spec::OverflowStyle::Auto => "auto".to_string(),
            },
        ),
        _ => return None,
    };
    Some((k.to_string(), v))
}

fn parse_alignment(v: &str) -> Option<pwb_core::spec::Alignment> {
    match v {
        "center" => Some(pwb_core::spec::Alignment::Center),
        "flex-start" | "start" => Some(pwb_core::spec::Alignment::Start),
        "flex-end" | "end" => Some(pwb_core::spec::Alignment::End),
        "space-between" => Some(pwb_core::spec::Alignment::SpaceBetween),
        _ => None,
    }
}

fn format_alignment(a: &pwb_core::spec::Alignment) -> String {
    match a {
        pwb_core::spec::Alignment::Start => "flex-start".to_string(),
        pwb_core::spec::Alignment::Center => "center".to_string(),
        pwb_core::spec::Alignment::End => "flex-end".to_string(),
        pwb_core::spec::Alignment::SpaceBetween => "space-between".to_string(),
    }
}

fn parse_resize(v: &str) -> Option<pwb_core::spec::ResizeStyle> {
    match v {
        "both" => Some(pwb_core::spec::ResizeStyle::Both),
        "vertical" => Some(pwb_core::spec::ResizeStyle::Vertical),
        "horizontal" => Some(pwb_core::spec::ResizeStyle::Horizontal),
        "none" => Some(pwb_core::spec::ResizeStyle::None),
        _ => None,
    }
}

fn format_unit(v: &UnitValue) -> String {
    match v {
        UnitValue::Px(px) => format!("{}px", px),
        UnitValue::Percent(p) => format!("{}%", p * 100.0),
        UnitValue::Vw(p) => format!("{}vw", p * 100.0),
        UnitValue::Vh(p) => format!("{}vh", p * 100.0),
        UnitValue::Auto => "auto".to_string(),
    }
}

fn parse_unit_value(v: &str) -> Option<pwb_core::spec::UnitValue> {
    let v = v.trim();
    if v == "auto" {
        return Some(pwb_core::spec::UnitValue::Auto);
    }
    if let Some(percent) = v.strip_suffix('%') {
        if let Ok(val) = percent.trim().parse::<f32>() {
            return Some(pwb_core::spec::UnitValue::Percent(val / 100.0));
        }
    }
    if let Some(rest) = v.strip_suffix("vw") {
        if let Ok(val) = rest.trim().parse::<f32>() {
            return Some(pwb_core::spec::UnitValue::Vw(val / 100.0));
        }
    }
    if let Some(rest) = v.strip_suffix("vh") {
        if let Ok(val) = rest.trim().parse::<f32>() {
            return Some(pwb_core::spec::UnitValue::Vh(val / 100.0));
        }
    }
    if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
        return Some(pwb_core::spec::UnitValue::Px(val));
    }
    None
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let clean = hex.trim().trim_start_matches('#');
    if clean == "white" {
        return Some(Color::WHITE);
    }
    if clean == "black" {
        return Some(Color::BLACK);
    }
    if clean.len() == 6 {
        let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
        let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
        let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
        Some(Color { r, g, b, a: 255 })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn test_pml_void_elements_do_not_swallow_siblings() {
        let root = parse_pml_markup(
            "<view id='container'><input type='text'><view id='next'>after</view></view>",
        )
        .expect("parses");
        assert_eq!(
            root.attributes.get("id").map(|s| s.as_str()),
            Some("container")
        );
        assert_eq!(
            root.children.len(),
            2,
            "input and next view must be siblings"
        );
        assert_eq!(root.children[0].tag, Tag::Input);
        let next = &root.children[1];
        assert_eq!(next.attributes.get("id").map(|s| s.as_str()), Some("next"));
        assert_eq!(next.text_content.as_deref(), Some("after"));
    }

    #[test]
    fn test_icon_elements_parse() {
        // `<icon-home/>` and `<icon name="star">` become Tag::Icon with the
        // icon name recorded in the `icon` attribute, and they are void (the
        // following element stays a sibling, not a child).
        let root = parse_pml_markup(
            "<view id='r'><icon-home/><icon name='star'/><view id='after'>x</view></view>",
        )
        .expect("parses");
        assert_eq!(root.attributes.get("id").map(|s| s.as_str()), Some("r"));
        assert_eq!(
            root.children.len(),
            3,
            "icons are void: following view stays a sibling"
        );
        let home = &root.children[0];
        assert_eq!(home.tag, Tag::Icon);
        assert_eq!(
            home.attributes.get("icon").map(|s| s.as_str()),
            Some("home")
        );
        let star = &root.children[1];
        assert_eq!(star.tag, Tag::Icon);
        assert_eq!(
            star.attributes.get("icon").map(|s| s.as_str()),
            Some("star")
        );
        assert_eq!(
            root.children[2].attributes.get("id").map(|s| s.as_str()),
            Some("after")
        );
    }

    #[test]
    fn test_code_elements_parse() {
        // `<code>` maps to Tag::Code and keeps its text (it's a normal
        // non-void element, unlike icons).
        let root = parse_pml_markup(
            "<view id='r'><code>var x = 1;</code><view id='after'>x</view></view>",
        )
        .expect("parses");
        let row = root
            .children
            .iter()
            .find(|c| c.tag == Tag::Code)
            .expect("code node");
        assert_eq!(row.text_content.as_deref(), Some("var x = 1;"));
        assert!(root
            .children
            .iter()
            .any(|c| c.attributes.get("id").map(|s| s.as_str()) == Some("after")));
    }

    #[test]
    fn test_component_import_with_spaces_in_param() {
        // `<import component="chip" label="...">` must substitute `{label}` in
        // the registered component body, including values containing spaces.
        // Component registration and import resolution happen in
        // `extract_bundlables` + `resolve_imports`, not in the raw parser.
        let pml = r#"
            <plain>
                <component name="chip" params="label">
                    <view class="chip"><text>{label}</text></view>
                </component>
                <view class="chips">
                    <import component="chip" label="built from components"></import>
                </view>
            </plain>
        "#;
        let mut root = parse_pml_markup(pml).expect("parses");
        let mut components = HashMap::new();
        let mut js_entries = Vec::new();
        extract_bundlables(&mut root, Path::new("."), &mut components, &mut js_entries)
            .expect("extracts components");
        assert!(components.contains_key("chip"));
        resolve_imports(&mut root, &components, 0);

        fn find_text(n: &PlainNode) -> Option<String> {
            if n.tag == Tag::Text {
                return n.text_content.clone();
            }
            for c in &n.children {
                if let Some(t) = find_text(c) {
                    return Some(t);
                }
            }
            None
        }
        let label = find_text(&root).expect("found text");
        assert_eq!(label, "built from components");
    }

    #[test]
    fn test_overflow_survives_stylesheet_compile() {
        // `overflow` is a real StyleProperty: parsed from PSS and flattened back
        // to an attribute so it survives the `.pwb` wire format.
        let dir = std::env::temp_dir().join(format!("pwb-overflow-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "index.pml",
            r#"
            <plain>
                <head><title>Overflow</title></head>
                <view class="scrollbox"><text>rows</text></view>
            </plain>
        "#,
        );
        write(
            &dir,
            "style.pss",
            ".scrollbox { overflow: scroll; height: 58vh; }\n",
        );

        let site = parse_site_directory(&dir).expect("site parses");
        let box_node = &site.root_node.children[1];
        assert_eq!(
            box_node.attributes.get("overflow").map(|s| s.as_str()),
            Some("scroll")
        );
        assert!(box_node.styles.iter().any(|p| matches!(
            p,
            StyleProperty::Overflow(pwb_core::spec::OverflowStyle::Scroll)
        )));
    }

    #[test]
    fn test_components_imports_scripts_and_bundling() {
        // A multi-file site exercising the HTML bundler: a `<component>`
        // definition, a build-time `<import>` with {param} substitution, a
        // `<script src>` reference, CSS (lightningcss) + PSS, and multiple JS.
        let dir = std::env::temp_dir().join(format!("pwb-parse-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "index.pml",
            r#"
            <plain>
                <head><title>Comp Test</title></head>
                <component name="badge">
                    <view class="badge"><text>{label}</text></view>
                </component>
                <view class="page">
                    <import component="badge" label="Inline!" />
                    <script src="app.pjs"></script>
                    <script>console.log("inline script");</script>
                </view>
            </plain>
        "#,
        );
        write(&dir, "lib.js", "var greet = function(n) { return n; };\n");
        write(
            &dir,
            "app.pjs",
            "import \"./lib.js\";\nconsole.log(greet(\"hi\"));\n",
        );
        write(&dir, "style.css", ".page { background-color: #101014; }\n");
        write(&dir, "extra.pss", ".badge { border-radius: 6px; }\n");

        let site = parse_site_directory(&dir).expect("site parses");

        // Component registry holds the badge definition.
        assert!(site.components.contains_key("badge"));
        let badge = &site.components["badge"];
        assert_eq!(badge.tag, Tag::View);
        assert_eq!(badge.children[0].text_content.as_deref(), Some("{label}"));

        // The <import> was inlined at build time with substitution.
        fn find_text(node: &PlainNode, needle: &str) -> bool {
            if node.text_content.as_deref() == Some(needle) {
                return true;
            }
            node.children.iter().any(|c| find_text(c, needle))
        }
        assert!(
            find_text(&site.root_node, "Inline!"),
            "import must inline the component with the label param substituted"
        );
        assert!(
            !find_text(&site.root_node, "{label}"),
            "placeholder must not leak into the page"
        );

        // Scripts: entry (app.pjs) runs last, inline script included, and every
        // file is a named module.
        let payload = String::from_utf8(site.script_payload.clone().unwrap()).unwrap();
        assert!(payload.contains("── file: lib.js ──"));
        assert!(payload.contains("── file: app.pjs ──"));
        assert!(payload.contains("console.log(\"inline script\")"));
        assert!(!payload.contains("import "));
        assert!(site.modules.contains_key("lib.js"));
        assert!(site.modules.contains_key("inline-1.js"));

        // Styles: .css went through lightningcss, .pss stayed native; both
        // applied (page bg + badge radius as attributes on the nodes).
        fn find_attr(node: &PlainNode, class: &str, key: &str) -> bool {
            if node
                .attributes
                .get("class")
                .map(|c| c.contains(class))
                .unwrap_or(false)
                && node.attributes.contains_key(key)
            {
                return true;
            }
            node.children.iter().any(|c| find_attr(c, class, key))
        }
        assert!(find_attr(&site.root_node, "page", "background-color"));
        assert!(find_attr(&site.root_node, "badge", "border-radius"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
