use super::*;
use pwb_core::spec::{PlainNode, Tag};
use std::fs;
use std::path::PathBuf;

#[test]
fn test_bundle_from_dir() {
    let test_dir = PathBuf::from(format!("/tmp/pwb_bundle_{}", std::process::id()));
    let _ = fs::remove_dir_all(&test_dir);
    fs::create_dir_all(&test_dir).unwrap();
    fs::write(
        test_dir.join("index.pml"),
        "<plain><head><title>Test</title></head><view style='background: red;'><text>Hello</text></view></plain>",
    )
    .unwrap();
    fs::write(test_dir.join("app.pjs"), "// empty").unwrap();

    let bundle = bundle_from_dir(&test_dir).unwrap();
    fs::remove_dir_all(&test_dir).ok();
    assert_eq!(bundle.title, "Test");
    assert!(bundle.script_payload.is_some());
}

#[test]
fn test_text_inherits_parent_font_size() {
    // Inline text inside a font-sized container inherits the container's
    // `font-size` (the devtools tab labels) instead of the 3vh default.
    let test_dir = PathBuf::from(format!("/tmp/pwb_inherit_{}", std::process::id()));
    let _ = fs::remove_dir_all(&test_dir);
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
fn test_devtools_bundle_uses_shared_compiler() {
    // The real devtools sources go through `pwb_web::parse::parse_site_directory`,
    // so HTML container tags parse, metadata (`<link rel=stylesheet>`) does not
    // become a DOM node, and button labels keep their text.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
    let bundle = bundle_from_dir(&dir).expect("devtools bundle");

    // Mounted as a page child, so devtools ids must not collide with the
    // main bundle's ids (which start at 1).
    assert!(bundle.root_node.id >= 100_000);

    let sidebar = find_attr_node(&bundle.root_node, "class", "devtools-sidebar")
        .expect("sidebar div becomes a View");
    assert_eq!(sidebar.tag, Tag::View);

    // Runtime-only PSS properties survive compilation: the resize clamps and
    // the separator borders the sidebar/header rely on.
    assert_eq!(
        sidebar.attributes.get("min-width").map(String::as_str),
        Some("15vw")
    );
    assert_eq!(
        sidebar.attributes.get("max-width").map(String::as_str),
        Some("50vw")
    );
    let header = find_attr_node(&bundle.root_node, "class", "devtools-header")
        .expect("header div becomes a View");
    assert_eq!(
        header.attributes.get("border-bottom").map(String::as_str),
        Some("0.1vh solid #3e3e42")
    );

    let pick = find_attr_node(&bundle.root_node, "id", "select-element").expect("pick button");
    assert_eq!(pick.tag, Tag::Button);
    assert_eq!(pick.text_content.as_deref(), Some("Pick"));

    // `<link rel="stylesheet">` is metadata, not an anchor.
    fn has_link(node: &PlainNode) -> bool {
        node.tag == Tag::Link || node.children.iter().any(has_link)
    }
    assert!(!has_link(&bundle.root_node), "no metadata link node");

    // Every `<icon-*>` is a void child of the tab row (not a parent of it).
    let tab = find_attr_node(&bundle.root_node, "id", "tab-elements").expect("tab");
    assert_eq!(tab.children.len(), 1);
    assert_eq!(tab.children[0].tag, Tag::Icon);
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
