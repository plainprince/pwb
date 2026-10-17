//! DevTools panel state and refresh helpers.

use crate::js_runtime::{execute_script, DomHandle};
use crate::layout::find_box_by_id;

/// DevTools state
#[derive(Default)]
pub(super) struct DevToolsState {
    pub(super) is_open: bool,
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
