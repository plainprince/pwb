//! Devtools console-line helper.

use crate::dom::DomNode;
use pwb_core::spec::Tag;
use std::collections::HashMap;

use super::super::core::DomHandle;
use super::super::dom_utils::{find_node_by_selector_mut, max_node_id};

/// Append a message line to the devtools `#console-output` panel if it is
/// mounted. Lines are `<code>` nodes, so the renderer syntax-highlights them
/// like a browser console (strings orange, keywords blue, numbers green — with
/// the level color as the base for identifiers). The output panel is flagged to
/// auto-scroll to the newest line.
pub(crate) fn append_console_line(dom_handle: &DomHandle, level: &str, msg: &str) {
    let mut dom = match dom_handle.dom.try_borrow_mut() {
        Ok(dom) => dom,
        Err(_) => return,
    };
    let next_id = max_node_id(&dom.root) + 1;
    let mut output_id = None;
    if let Some(output) = find_node_by_selector_mut(&mut dom.root, "#console-output") {
        output_id = Some(output.id);
        let color = match level {
            "error" => "#f44747",
            "warn" => "#cca700",
            _ => "#cccccc",
        };
        let mut attrs = HashMap::new();
        attrs.insert("color".to_string(), color.to_string());
        attrs.insert("font-size".to_string(), "1.7vh".to_string());
        output.children.push(DomNode {
            id: next_id,
            tag: Tag::Code,
            attributes: attrs,
            text: Some(msg.to_string()),
            children: Vec::new(),
        });
    }
    drop(dom);
    if let Some(id) = output_id {
        dom_handle.scroll_to_bottom.borrow_mut().push(id);
    }
}
