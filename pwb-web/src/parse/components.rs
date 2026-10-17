use pwb_core::spec::{PlainNode, Tag};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::error::ParseError;
use super::pml::parse_pml_markup;

/// Walk a parsed PML tree and pull out the bundling primitives:
///
/// - `<component name="X">…</component>` (inline or `src="file.pml"`) → the
///   component registry (the element itself is removed from the page).
/// - `<script src="x.pjs">` / inline `<script>…</script>` → bundled JS
///   entries (the element is removed; `<import>` elements are left in place
///   for [`resolve_imports`]).
pub(crate) fn extract_bundlables(
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
                    let name = src.split('/').next_back().unwrap_or(&src).to_string();
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
pub(crate) fn resolve_imports(
    node: &mut PlainNode,
    components: &HashMap<String, PlainNode>,
    depth: usize,
) {
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
    for v in node.attributes.values_mut() {
        *v = substitute_in_string(v, params);
    }
    if let Some(text) = &mut node.text_content {
        *text = substitute_in_string(text, params);
    }
    for child in &mut node.children {
        substitute_params(child, params);
    }
}

pub(crate) fn substitute_in_string(input: &str, params: &HashMap<String, String>) -> String {
    let mut out = input.to_string();
    for (key, value) in params {
        out = out.replace(&format!("{{{}}}", key), value);
    }
    out
}
