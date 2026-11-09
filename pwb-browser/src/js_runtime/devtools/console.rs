//! Devtools console-line helper.

use crate::dom::DomNode;
use pwb_core::spec::Tag;
use std::collections::HashMap;

use super::super::core::DomHandle;
use super::super::dom_utils::{find_node_by_selector_mut, max_node_id};

/// Append one message line at `level`.
pub(crate) fn append_console_line(dom_handle: &DomHandle, level: &str, msg: &str) {
    append_console_lines(dom_handle, &[(msg.to_string(), level)]);
}

/// Append a block of console lines (a message plus its stack frames, or a
/// multi-line value) in one DOM borrow, then auto-scroll the panel once.
///
/// Levels set the color so errors, warnings, and stack traces are visually
/// distinct: `error` red, `warn` amber, `trace` dim grey, `log` default.
/// Lines are `<code>` nodes, so the renderer syntax-highlights them like a
/// browser console (strings orange, keywords blue, numbers green — with the
/// level color as the base for identifiers).
pub(crate) fn append_console_lines(dom_handle: &DomHandle, lines: &[(String, &str)]) {
    if lines.is_empty() {
        return;
    }
    let mut dom = match dom_handle.dom.try_borrow_mut() {
        Ok(dom) => dom,
        Err(_) => return,
    };
    let mut next_id = max_node_id(&dom.root) + 1;
    let mut output_id = None;
    if let Some(output) = find_node_by_selector_mut(&mut dom.root, "#console-output") {
        output_id = Some(output.id);
        for (msg, level) in lines {
            let (color, font_size) = match *level {
                "error" => ("#f44747", "1.7vh"),
                "warn" => ("#cca700", "1.7vh"),
                // Stack frames: smaller and dimmer so the error itself stands
                // out and long frames fit the panel width.
                "trace" => ("#9aa0a6", "1.5vh"),
                _ => ("#cccccc", "1.7vh"),
            };
            let mut attrs = HashMap::new();
            attrs.insert("color".to_string(), color.to_string());
            attrs.insert("font-size".to_string(), font_size.to_string());
            output.children.push(DomNode {
                id: next_id,
                tag: Tag::Code,
                attributes: attrs,
                text: Some(msg.clone()),
                children: Vec::new(),
            });
            next_id += 1;
        }
    }
    drop(dom);
    if let Some(id) = output_id {
        dom_handle.scroll_to_bottom.borrow_mut().push(id);
    }
}
