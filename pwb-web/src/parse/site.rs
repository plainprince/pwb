use pwb_core::spec::{PlainNode, Tag};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::components::{extract_bundlables, resolve_imports};
use super::error::ParseError;
use super::pml::parse_pml_markup;
use super::style::{apply_stylesheet, parse_pss_stylesheet};

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
        .copied();
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
