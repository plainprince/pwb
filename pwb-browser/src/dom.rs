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
