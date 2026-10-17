use super::*;
use crate::spec::{PlainNode, Tag};
use std::collections::HashMap;

#[test]
fn test_wire_roundtrip() {
    let mut root = PlainNode::new(1, Tag::Document);
    let mut text_node = PlainNode::new(2, Tag::Text);
    text_node.text_content = Some("Hello PlainWeb!".to_string());
    root.children.push(text_node);

    // An icon element survives the round trip with its `icon` attribute.
    let mut icon = PlainNode::new(0, Tag::Icon);
    icon.attributes
        .insert("icon".to_string(), "home".to_string());
    root.children.push(icon);

    // A `<code>` node survives with its highlighted text.
    let mut code = PlainNode::new(0, Tag::Code);
    code.text_content = Some("var x = 42;".to_string());
    root.children.push(code);

    // A component body with a {param} placeholder and a named script module
    // must survive the bit-packed wire round trip (v2 bundling fields).
    let mut comp = PlainNode::new(0, Tag::View);
    comp.attributes
        .insert("class".to_string(), "card".to_string());
    let mut comp_text = PlainNode::new(0, Tag::Text);
    comp_text.text_content = Some("Hello {name}".to_string());
    comp.children.push(comp_text);

    let bundle = Bundle {
        version: CURRENT_VERSION,
        title: "Test Site".to_string(),
        author_pubkey: None,
        root_node: root,
        script_payload: Some(vec![1, 2, 3, 4]),
        components: HashMap::from([("greeting".to_string(), comp)]),
        modules: HashMap::from([("lib.js".to_string(), b"var x = 1;".to_vec())]),
    };

    let binary = bundle.serialize();
    let decoded = Bundle::deserialize(&binary).unwrap();

    assert_eq!(bundle.title, decoded.title);
    assert_eq!(
        bundle.root_node.children.len(),
        decoded.root_node.children.len()
    );
    let decoded_icon = decoded
        .root_node
        .children
        .iter()
        .find(|n| n.tag == Tag::Icon)
        .expect("icon survives");
    assert_eq!(
        decoded_icon.attributes.get("icon").map(|s| s.as_str()),
        Some("home")
    );
    let decoded_code = decoded
        .root_node
        .children
        .iter()
        .find(|n| n.tag == Tag::Code)
        .expect("code survives");
    assert_eq!(decoded_code.text_content.as_deref(), Some("var x = 42;"));
    assert_eq!(bundle.script_payload, decoded.script_payload);
    assert_eq!(bundle.modules, decoded.modules);
    // Node ids are intentionally re-assigned on decode (never encoded), so
    // compare the component structure (tag/attrs/text) without ids.
    let greeting = decoded.components.get("greeting").unwrap();
    assert_eq!(greeting.tag, Tag::View);
    assert_eq!(
        greeting.attributes.get("class").map(|s| s.as_str()),
        Some("card")
    );
    assert_eq!(greeting.children.len(), 1);
    assert_eq!(greeting.children[0].tag, Tag::Text);
    assert_eq!(
        greeting.children[0].text_content.as_deref(),
        Some("Hello {name}")
    );
}
