//! DevTools panel state and refresh helpers.

use crate::dom::Dom;
use crate::js_runtime::{execute_script, DomHandle};
use crate::layout::find_box_by_id;
use pwb_core::wire::Bundle;

/// DevTools state
#[derive(Default)]
pub(super) struct DevToolsState {
    pub(super) is_open: bool,
}

/// Mount the devtools DOM as a child of the page and run its script in the
/// current JS context. Returns the devtools root node id. The caller decides
/// whether it starts open or hidden; either way the script runs so its
/// handlers are registered. Re-mounting after a navigation is what keeps
/// devtools working when the page context is recreated.
pub(super) fn mount_devtools(handle: &DomHandle, bundle: &Bundle, open: bool) -> Option<usize> {
    let mut devtools_dom = Dom::from_bundle(bundle);
    let root_id = devtools_dom.root.id;
    devtools_dom
        .root
        .attributes
        .insert("data-devtools".to_string(), "root".to_string());
    if !open {
        devtools_dom
            .root
            .attributes
            .insert("display".to_string(), "none".to_string());
    }
    {
        let mut dom = handle.dom.try_borrow_mut().ok()?;
        dom.add_child(devtools_dom.root);
    }
    if let Some(script_bytes) = &bundle.script_payload {
        if let Ok(script) = std::str::from_utf8(script_bytes) {
            if let Err(e) = execute_script(handle, script) {
                eprintln!("[DevTools JS Error] Devtools script failed: {:?}", e);
            }
        }
    }
    Some(root_id)
}

/// Scroll every listed box to its bottom (the devtools console jumps to the
/// newest line). Boxes that aren't scroll containers (or aren't laid out,
/// e.g. while their tab is hidden) are ignored.
pub(crate) fn apply_scroll_to_bottom(
    layout_root: &crate::layout::LayoutBox,
    element_scrolls: &mut std::collections::HashMap<usize, f32>,
    ids: &[usize],
) {
    for id in ids {
        if let Some(bx) = find_box_by_id(layout_root, *id) {
            let max_scroll = (bx.content_height - bx.rect.height()).max(0.0);
            element_scrolls.insert(*id, max_scroll);
        }
    }
}

/// Rebuild the devtools Elements tree + styles panel after a DOM mutation
/// (click/contextmenu handlers, resize drags). The binding itself no-ops while
/// devtools are closed, so calling this unconditionally is cheap.
pub(super) fn refresh_devtools(handle: &DomHandle, size: (u32, u32)) {
    let script = format!("__pwb_refreshDevtools({}, {});", size.0, size.1);
    if let Err(e) = execute_script(handle, &script) {
        eprintln!("[DevTools] refresh failed: {:?}", e);
    }
}
