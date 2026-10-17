use super::*;
use crate::bundler::parser::{parse_pml, DEVTOOLS_ID_OFFSET};
use crate::bundler::pss::{apply_stylesheet_attrs, parse_pss_to_attrs};
use pwb_core::spec::{PlainNode, Tag};
use std::fs;
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
    let root =
        parse_pml("<view id='container'><input type='text'><view id='next'>after</view></view>");
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
    let root =
        parse_pml("<view id='r'><icon-home/><icon name='star'/><view id='after'>x</view></view>");
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
