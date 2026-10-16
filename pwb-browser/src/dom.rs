//! DOM tree representation and conversion (`pwb-browser::dom`).

use pwb_core::spec::{PlainNode, Tag};
use pwb_core::wire::Bundle;

/// Runtime DOM element representation.
#[derive(Debug, Clone)]
pub struct DomNode {
    pub id: usize,
    pub tag: Tag,
    pub attributes: std::collections::HashMap<String, String>,
    pub text: Option<String>,
    pub children: Vec<DomNode>,
}

#[derive(Debug, Clone)]
pub struct Dom {
    pub title: String,
    pub root: DomNode,
    /// Component registry: name → component body (from `<component>` defs).
    pub components: std::collections::HashMap<String, DomNode>,
    /// Named script modules for `pwb.import(name)` / `pwb.hotload(name)`.
    pub modules: std::collections::HashMap<String, Vec<u8>>,
}

impl Dom {
    pub fn from_bundle(bundle: &Bundle) -> Self {
        let root = convert_node(&bundle.root_node);
        Dom {
            title: bundle.title.clone(),
            root,
            components: bundle
                .components
                .iter()
                .map(|(name, body)| (name.clone(), convert_node(body)))
                .collect(),
            modules: bundle.modules.clone(),
        }
    }
    pub fn add_child(&mut self, child: DomNode) {
        self.root.children.push(child);
    }
}

/// Immutable depth-first lookup of a node by its numeric id (or id attribute).
pub fn find_node_by_id_ref<'a>(node: &'a DomNode, id: &str) -> Option<&'a DomNode> {
    if node.id.to_string() == id || node.attributes.get("id").map(|s| s == id).unwrap_or(false) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_by_id_ref(child, id) {
            return Some(found);
        }
    }
    None
}
fn convert_node(node: &PlainNode) -> DomNode {
    DomNode {
        id: node.id,
        tag: node.tag,
        attributes: node.attributes.clone(),
        text: node.text_content.clone(),
        children: node.children.iter().map(convert_node).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn node(id: usize, tag: Tag, text: Option<&str>, children: Vec<DomNode>) -> DomNode {
        DomNode {
            id,
            tag,
            attributes: HashMap::new(),
            text: text.map(|s| s.to_string()),
            children,
        }
    }

    #[test]
    fn test_find_node_by_id_ref_matches_numeric_and_attribute() {
        let deep = node(3, Tag::Text, Some("deep"), vec![]);
        let mid = node(2, Tag::View, None, vec![deep]);
        let root = node(
            1,
            Tag::Document,
            None,
            vec![DomNode {
                id: 4,
                tag: Tag::View,
                attributes: HashMap::from([("id".to_string(), "hero".to_string())]),
                text: None,
                children: vec![],
            }],
        );
        let root = node(0, Tag::Document, None, vec![root, mid]);

        // Numeric id, attribute id, and a nested child.
        assert_eq!(find_node_by_id_ref(&root, "0").map(|n| n.id), Some(0));
        assert_eq!(find_node_by_id_ref(&root, "hero").map(|n| n.id), Some(4));
        assert_eq!(find_node_by_id_ref(&root, "3").map(|n| n.id), Some(3));
        assert!(find_node_by_id_ref(&root, "99").is_none());
    }

    #[test]
    fn test_dom_from_bundle_and_add_child() {
        let mut title = PlainNode::new(1, Tag::Title);
        title.text_content = Some("Doc".into());
        let mut root = PlainNode::new(0, Tag::Document);
        root.children.push(title);
        let bundle = pwb_core::wire::Bundle {
            version: pwb_core::wire::CURRENT_VERSION,
            title: "Doc".into(),
            author_pubkey: None,
            root_node: root,
            script_payload: None,
            components: HashMap::new(),
            modules: HashMap::from([("m.js".to_string(), vec![1, 2, 3])]),
        };
        let mut dom = Dom::from_bundle(&bundle);
        assert_eq!(dom.title, "Doc");
        assert_eq!(dom.root.children.len(), 1);
        assert_eq!(dom.root.children[0].tag, Tag::Title);
        assert!(dom.modules.contains_key("m.js"));

        dom.add_child(node(9, Tag::View, None, vec![]));
        assert_eq!(dom.root.children.len(), 2);
    }
}
