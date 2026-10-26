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
                    // Imported components may themselves use `<import>`: resolve
                    // the body's own children (which expands direct `<import>`
                    // children exactly like the top-level pass does — recursing
                    // *into* a child import would descend past it and leave it
                    // unexpanded).
                    resolve_imports(&mut instance, components, depth + 1);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: usize, tag: Tag) -> PlainNode {
        PlainNode {
            id,
            tag,
            attributes: HashMap::new(),
            styles: Vec::new(),
            text_content: None,
            children: Vec::new(),
        }
    }

    fn with_attr(mut n: PlainNode, key: &str, value: &str) -> PlainNode {
        n.attributes.insert(key.to_string(), value.to_string());
        n
    }

    fn tmp_site(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pwb-components-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn find_text(node: &PlainNode) -> Option<&str> {
        node.text_content
            .as_deref()
            .or_else(|| node.children.iter().find_map(find_text))
    }

    #[test]
    fn extracts_inline_and_file_backed_components_and_scripts() {
        let dir = tmp_site("extract");
        fs::write(
            dir.join("card.pml"),
            r#"<plain><component name="FromFile"><view><text>file body</text></view></component></plain>"#,
        )
        .unwrap();
        fs::write(
            dir.join("bare.pml"),
            r#"<component name="Bare"><view><text>bare body</text></view></component>"#,
        )
        .unwrap();
        fs::write(dir.join("app.js"), "console.log('hi');").unwrap();

        let mut root = node(0, Tag::Document);
        // Inline component with exactly one child: the child becomes the body.
        let mut inline = with_attr(node(1, Tag::Component), "name", "Inline");
        inline
            .children
            .push(with_attr(node(2, Tag::Text), "data-x", "1"));
        root.children.push(inline);
        // Multi-child component: wrapped in a View container.
        let mut multi = with_attr(node(3, Tag::Component), "name", "Multi");
        multi.children.push(node(4, Tag::View));
        multi.children.push(node(5, Tag::View));
        root.children.push(multi);
        // Component loading its body from a sibling file.
        let from_file = with_attr(node(6, Tag::Component), "name", "FromFile");
        root.children.push(with_attr(from_file, "src", "card.pml"));
        // A file whose root IS the component (no wrapper) takes the other branch.
        let bare = with_attr(node(14, Tag::Component), "name", "Bare");
        root.children.push(with_attr(bare, "src", "bare.pml"));
        // Missing src file: reported, the claim still consumes the element.
        let missing = with_attr(node(7, Tag::Component), "name", "Missing");
        root.children.push(with_attr(missing, "src", "nope.pml"));
        // A nameless component is dropped without registering.
        root.children.push(node(8, Tag::Component));
        // Scripts: external file, missing file, and inline code.
        root.children
            .push(with_attr(node(9, Tag::Script), "src", "app.js"));
        root.children
            .push(with_attr(node(10, Tag::Script), "src", "missing.js"));
        let mut inline_js = node(11, Tag::Script);
        inline_js.text_content = Some("let x = 1;".to_string());
        root.children.push(inline_js);
        // Regular content is kept and recursed into (an import below survives).
        let mut view = node(12, Tag::View);
        view.children
            .push(with_attr(node(13, Tag::Import), "component", "Inline"));
        root.children.push(view);

        let mut components = HashMap::new();
        let mut js_entries = Vec::new();
        extract_bundlables(&mut root, &dir, &mut components, &mut js_entries)
            .expect("extraction never fails");

        assert!(components.contains_key("Inline"), "single-child body");
        assert!(components.contains_key("Multi"), "multi-child body");
        assert!(components.contains_key("FromFile"), "file-backed body");
        assert!(components.contains_key("Bare"), "bare file-backed body");
        assert!(
            components.contains_key("Missing"),
            "failed src still registers"
        );
        assert!(!components.contains_key(""), "nameless component dropped");
        assert_eq!(
            find_text(components.get("FromFile").unwrap()),
            Some("file body")
        );
        assert_eq!(
            find_text(components.get("Bare").unwrap()),
            Some("bare body")
        );

        assert!(
            js_entries.iter().any(|(name, _)| name == "app.js"),
            "external script bundled: {js_entries:?}"
        );
        assert!(
            js_entries.iter().any(|(name, _)| name == "inline-1.js"),
            "inline script bundled: {js_entries:?}"
        );
        assert!(
            !js_entries.iter().any(|(name, _)| name == "missing.js"),
            "missing script is reported, not bundled"
        );

        // Component and script elements are removed from the page itself.
        assert!(root
            .children
            .iter()
            .all(|c| c.tag != Tag::Component && c.tag != Tag::Script));
        assert_eq!(root.children.len(), 1, "only the view remains");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_imports_substitutes_params_and_drops_unknown_ones() {
        let mut components = HashMap::new();
        // Body with placeholders in attributes and text, plus a nested import.
        let mut body = with_attr(node(1, Tag::View), "class", "card {tone}");
        let mut label = node(2, Tag::Text);
        label.text_content = Some("Hello {name}!".to_string());
        body.children.push(label);
        // The nested import re-declares the params it needs; the outer
        // substitution rewrites `{name}` in its attributes first.
        let mut nested = with_attr(node(3, Tag::Import), "component", "Leaf");
        nested
            .attributes
            .insert("name".to_string(), "{name}".to_string());
        body.children.push(nested);
        components.insert("Card".to_string(), body);

        let mut leaf = with_attr(node(4, Tag::View), "id", "leaf");
        let mut leaf_text = node(5, Tag::Text);
        leaf_text.text_content = Some("{name}".to_string());
        leaf.children.push(leaf_text);
        components.insert("Leaf".to_string(), leaf);

        // A component whose body is itself an <import> takes the direct branch.
        let mut import_body = with_attr(node(15, Tag::Import), "component", "Leaf");
        import_body
            .children
            .push(with_attr(node(16, Tag::Import), "component", "Leaf"));
        components.insert("Wrapper".to_string(), import_body);

        let mut root = node(0, Tag::Document);
        let mut imp = with_attr(node(7, Tag::Import), "component", "Card");
        imp.attributes.insert("name".to_string(), "Ada".to_string());
        imp.attributes
            .insert("tone".to_string(), "warm".to_string());
        root.children.push(imp);
        // Unknown component: element dropped, siblings kept.
        root.children
            .push(with_attr(node(8, Tag::Import), "component", "Nope"));
        // Import-tag body path.
        let mut imp2 = with_attr(node(9, Tag::Import), "component", "Wrapper");
        imp2.attributes.insert("name".to_string(), "Bo".to_string());
        root.children.push(imp2);
        // Regular node: recursed into and kept.
        let mut plain = node(10, Tag::View);
        plain
            .children
            .push(with_attr(node(11, Tag::Import), "component", "Leaf"));
        root.children.push(plain);

        resolve_imports(&mut root, &components, 0);

        // Card, Wrapper, and the plain view are all still there — imports are
        // replaced 1:1 by their bodies, the unknown one is dropped.
        assert_eq!(root.children.len(), 3, "unknown import dropped, rest kept");
        let card = &root.children[0];
        assert_eq!(
            card.attributes.get("class").map(String::as_str),
            Some("card warm")
        );
        let label = card
            .children
            .iter()
            .find(|c| c.tag == Tag::Text)
            .expect("text child substituted");
        assert_eq!(label.text_content.as_deref(), Some("Hello Ada!"));
        // The nested <import> inside the body was resolved to the Leaf body.
        let leaf = card
            .children
            .iter()
            .find(|c| c.tag == Tag::View)
            .expect("nested import resolved");
        assert_eq!(leaf.attributes.get("id").map(String::as_str), Some("leaf"));
        assert_eq!(find_text(leaf), Some("Ada"));
        // The import-tag body keeps its own tag but its nested import expanded.
        let wrapper = &root.children[1];
        assert_eq!(wrapper.tag, Tag::Import);
        assert_eq!(wrapper.children.len(), 1, "nested import resolved");
        assert_eq!(
            wrapper.children[0].attributes.get("id").map(String::as_str),
            Some("leaf")
        );
        // Recursion reached imports inside plain nodes.
        let plain = &root.children[2];
        assert_eq!(
            plain.children[0].attributes.get("id").map(String::as_str),
            Some("leaf")
        );
    }

    #[test]
    fn deep_import_nesting_aborts_instead_of_overflowing() {
        // C0 -> C1 -> ... : each body only holds the next import.
        let mut components = HashMap::new();
        for i in 0..10 {
            let mut body = node(100 + i, Tag::View);
            if i < 9 {
                body.children.push(with_attr(
                    node(200 + i, Tag::Import),
                    "component",
                    &format!("C{}", i + 1),
                ));
            }
            components.insert(format!("C{i}"), body);
        }
        let mut root = node(0, Tag::Document);
        root.children
            .push(with_attr(node(1, Tag::Import), "component", "C0"));

        resolve_imports(&mut root, &components, 0);
        // The depth guard fired, so the chain is only partially expanded
        // (nothing panics and the recursion terminates).
        assert!(!root.children.is_empty());
    }

    #[test]
    fn substitute_in_string_replaces_known_keys_only() {
        let params = HashMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "two".to_string()),
        ]);
        assert_eq!(substitute_in_string("{a}-{b}", &params), "1-two");
        assert_eq!(
            substitute_in_string("{a} {missing}", &params),
            "1 {missing}"
        );
        assert_eq!(
            substitute_in_string("no placeholders", &params),
            "no placeholders"
        );
    }
}
