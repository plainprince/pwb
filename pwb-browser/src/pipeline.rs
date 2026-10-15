use crate::bundler;
use std::path::Path;

/// Desktop GUI Window execution pipeline (`pwb-browser::pipeline`).
use crate::context_menu::{self, ContextMenuActions};
use crate::dom::Dom;
use crate::js_runtime::{
    execute_event_handler, execute_event_handler_typed, execute_script, DomHandle,
};
use crate::layout::{
    build_layout_tree, caret_index_at_x, find_box_by_id, hit_test_resize_handle,
    hit_test_text_char_scrolled, resolve_unit, ResizeEdge,
};
use crate::style::compute_style;

use crate::render::{
    draw_hover_highlight_scrolled, draw_selected_highlight_scrolled, draw_tooltip,
    find_scroll_container, hit_test_cursor_from_layout_scrolled, hit_test_layout_box_scrolled,
    render_dom_into_pixmap, SelectionPoint, TextSelection,
};
use pwb_core::spec::Tag;
use pwb_core::wire::Bundle;

use arboard::Clipboard;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

#[derive(Debug)]
pub enum PipelineError {
    Wire(pwb_core::wire::WireError),
    Io(std::io::Error),
    Windowing(String),
}

/// DevTools state
#[derive(Default)]
struct DevToolsState {
    is_open: bool,
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
fn refresh_devtools(handle: &DomHandle, size: (u32, u32)) {
    let script = format!("__pwb_refreshDevtools({}, {});", size.0, size.1);
    if let Err(e) = execute_script(handle, &script) {
        eprintln!("[DevTools] refresh failed: {:?}", e);
    }
}

/// An in-progress drag-resize of a `resize`-enabled element.
struct ResizeDrag {
    node_id: usize,
    edge: ResizeEdge,
    start_w: f32,
    start_h: f32,
    start_mx: f32,
    start_my: f32,
    /// Width clamp for the drag: the element's `min-width`/`max-width` (only
    /// meaningful for Left/Right edges; vertical drags use 40/MAX).
    min_w: f32,
    max_w: f32,
}

impl ResizeDrag {
    fn cursor(&self) -> CursorIcon {
        match self.edge {
            ResizeEdge::Left | ResizeEdge::Right => CursorIcon::EwResize,
            ResizeEdge::Top | ResizeEdge::Bottom => CursorIcon::NsResize,
        }
    }
}

/// Compute the new width/height (physical px) from a resize drag. `start_*` are the
/// physical dimensions/position at grab time; the result clamps to `[min, max]`
/// (the element's `min-width`/`max-width` for horizontal edges; vertical edges
/// only clamp at `min` since `max` is MAX for them).
#[allow(clippy::too_many_arguments)]
fn resize_new_dimensions(
    edge: ResizeEdge,
    start_w: f32,
    start_h: f32,
    start_mx: f32,
    start_my: f32,
    mx: f32,
    my: f32,
    min: f32,
    max: f32,
) -> (f32, f32) {
    let mut new_w = start_w;
    let mut new_h = start_h;
    match edge {
        ResizeEdge::Left => new_w = (start_w - (mx - start_mx)).clamp(min, max),
        ResizeEdge::Right => new_w = (start_w + (mx - start_mx)).clamp(min, max),
        ResizeEdge::Top => new_h = (start_h - (my - start_my)).max(min),
        ResizeEdge::Bottom => new_h = (start_h + (my - start_my)).max(min),
    }
    (new_w, new_h)
}

/// The `[min, max]` width clamp for a resize drag on `node_id`, from the
/// element's `min-width`/`max-width` style (horizontal edges) or the default
/// 40px floor (vertical edges).
fn resize_bounds(
    handle: &DomHandle,
    node_id: usize,
    edge: ResizeEdge,
    vw: f32,
    vh: f32,
) -> (f32, f32) {
    let style = handle.dom.borrow();
    let style = crate::dom::find_node_by_id_ref(&style.root, &node_id.to_string())
        .map(compute_style)
        .unwrap_or_default();
    match edge {
        ResizeEdge::Left | ResizeEdge::Right => (
            style
                .min_width
                .and_then(|uv| resolve_unit(uv, vw, vw, vh))
                .unwrap_or(40.0),
            style
                .max_width
                .and_then(|uv| resolve_unit(uv, vw, vw, vh))
                .unwrap_or(f32::MAX),
        ),
        ResizeEdge::Top | ResizeEdge::Bottom => (40.0, f32::MAX),
    }
}

/// Find a DOM node by its numeric node id (used by the F12 devtools toggle).
fn find_node_by_id<'a>(
    node: &'a mut crate::dom::DomNode,
    id: &str,
) -> Option<&'a mut crate::dom::DomNode> {
    if node.id.to_string() == id || node.attributes.get("id").map(|v| v == id).unwrap_or(false) {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_node_by_id(child, id) {
            return Some(found);
        }
    }
    None
}

/// Find the first node carrying an attribute (e.g. `data-devtools`).
fn find_node_with_attr<'a>(
    node: &'a crate::dom::DomNode,
    attr: &str,
) -> Option<&'a crate::dom::DomNode> {
    if node.attributes.contains_key(attr) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_with_attr(child, attr) {
            return Some(found);
        }
    }
    None
}

/// Whether `node_id` lies inside the subtree rooted at `subtree_root`.
fn node_in_subtree(subtree_root: &crate::dom::DomNode, node_id: usize) -> bool {
    fn walk(node: &crate::dom::DomNode, id: usize) -> bool {
        if node.id == id {
            return true;
        }
        node.children.iter().any(|c| walk(c, id))
    }
    walk(subtree_root, node_id)
}

/// The `id` attribute of a node by numeric id, if any ("" otherwise).
fn node_id_attr(root: &crate::dom::DomNode, id: usize) -> Option<String> {
    fn walk(node: &crate::dom::DomNode, id: usize) -> Option<&crate::dom::DomNode> {
        if node.id == id {
            return Some(node);
        }
        node.children.iter().find_map(|c| walk(c, id))
    }
    walk(root, id).and_then(|n| n.attributes.get("id").cloned())
}

/// Walk the subtree depth-first looking for a node with `node_id` that has a
/// `title` attribute. Returns the title string (or `None`).
fn find_title_for_node(node: &crate::dom::DomNode, node_id: usize) -> Option<String> {
    if node.id == node_id {
        return node.attributes.get("title").cloned();
    }
    node.children
        .iter()
        .find_map(|c| find_title_for_node(c, node_id))
}

/// The `title` attribute of the element under the cursor, for the hover
/// tooltip. Returns `None` when over devtools internals or when nothing
/// tooltip-worthy is hovered.
fn hovered_title(
    dom: &crate::dom::Dom,
    layout_root: &crate::layout::LayoutBox,
    mx: f32,
    my: f32,
    scrolls: &std::collections::HashMap<usize, f32>,
) -> Option<String> {
    let hit = hit_test_layout_box_scrolled(layout_root, mx, my, scrolls)?;
    let over_devtools = find_node_with_attr(&dom.root, "data-devtools")
        .map(|root| node_in_subtree(root, hit.node_id))
        .unwrap_or(false);
    if over_devtools {
        return None;
    }
    dom.root
        .children
        .iter()
        .find_map(|c| find_title_for_node(c, hit.node_id))
        .filter(|t| !t.is_empty())
}

/// If `node_id` (or any of its ancestors) is a devtools Elements tree row,
/// return the original DOM node it represents (from the row's `data-node`
/// attribute). Clicking any part of a row — the opening tag, the content, or
/// the closing `</tag>` line — selects that element.
pub(crate) fn tree_row_target_node(root: &crate::dom::DomNode, node_id: usize) -> Option<usize> {
    fn walk(node: &crate::dom::DomNode, id: usize) -> Option<&crate::dom::DomNode> {
        if node.id == id {
            return Some(node);
        }
        node.children.iter().find_map(|c| walk(c, id))
    }
    let mut ids = vec![node_id];
    ids.extend(ancestor_node_ids(root, node_id));
    for id in ids {
        if let Some(node) = walk(root, id) {
            if let Some(target) = node.attributes.get("data-node") {
                if let Ok(parsed) = target.parse::<usize>() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

/// Resolve a hit node id to the element it belongs to: walks up from a Text
/// run (or the document shell) to the nearest real element, so picking a text
/// run selects the container that holds it — exactly one element.
fn element_node_id(root: &crate::dom::DomNode, hit: usize) -> usize {
    let mut cur = hit;
    loop {
        let is_shell = crate::dom::find_node_by_id_ref(root, &cur.to_string())
            .map(|n| matches!(n.tag, Tag::Text | Tag::Document | Tag::Head | Tag::Title))
            .unwrap_or(false);
        if !is_shell {
            return cur;
        }
        let parent = ancestor_node_ids(root, cur).first().copied().unwrap_or(cur);
        if parent == cur {
            return cur;
        }
        cur = parent;
    }
}

/// Ancestor node ids of `node_id`, nearest first (excluding the node itself).
/// Used to bubble clicks from a text run up to the element containing it
/// (e.g. a `<div class="tab">Label</div>` where the label is a Text child).
fn ancestor_node_ids(root: &crate::dom::DomNode, node_id: usize) -> Vec<usize> {
    fn walk(node: &crate::dom::DomNode, id: usize, chain: &mut Vec<usize>) -> bool {
        if node.id == id {
            return true;
        }
        for child in &node.children {
            if walk(child, id, chain) {
                chain.push(node.id);
                return true;
            }
        }
        false
    }
    let mut chain = Vec::new();
    walk(root, node_id, &mut chain);
    chain
}

/// Collect `(node_id, text)` for every text-bearing node in document order.
fn collect_text_spans(node: &crate::dom::DomNode, out: &mut Vec<(usize, String)>) {
    if let Some(text) = &node.text {
        if !text.is_empty() {
            out.push((node.id, text.clone()));
        }
    }
    for child in &node.children {
        collect_text_spans(child, out);
    }
}

/// Extract the selected text between `(start_node, start_char)` and
/// `(end_node, end_char)`, walking the DOM in document order. Handles reversed
/// ranges and multi-node selections.
fn compute_selected_text(
    root: &crate::dom::DomNode,
    start_node: usize,
    start_char: usize,
    end_node: usize,
    end_char: usize,
) -> String {
    let mut spans = Vec::new();
    collect_text_spans(root, &mut spans);

    let start_idx = spans.iter().position(|s| s.0 == start_node);
    let end_idx = spans.iter().position(|s| s.0 == end_node);
    let (Some(si), Some(ei)) = (start_idx, end_idx) else {
        return String::new();
    };

    // Normalize to a forward range.
    let (s_idx, s_char, e_idx, e_char) = if (si, start_char) <= (ei, end_char) {
        (si, start_char, ei, end_char)
    } else {
        (ei, end_char, si, start_char)
    };

    let mut out = String::new();
    if s_idx == e_idx {
        let text = &spans[s_idx].1;
        out.push_str(
            &text
                .chars()
                .skip(s_char)
                .take(e_char.saturating_sub(s_char))
                .collect::<String>(),
        );
    } else {
        out.push_str(&spans[s_idx].1.chars().skip(s_char).collect::<String>());
        for span in &spans[s_idx + 1..e_idx] {
            out.push_str(&span.1);
        }
        out.push_str(&spans[e_idx].1.chars().take(e_char).collect::<String>());
    }
    out
}

pub fn run_browser_window(bundle_bytes: &[u8]) -> Result<(), PipelineError> {
    println!("Decompiling PlainWeb binary bundle...");
    let bundle = Bundle::deserialize(bundle_bytes).map_err(PipelineError::Wire)?;

    // Debug hooks: `PWB_DEVTOOLS_OPEN=1` starts with devtools open (default: closed);
    // `PWB_DUMP_PNG=/path` writes the rendered frame to a PNG on every redraw.
    let devtools_start_open = std::env::var("PWB_DEVTOOLS_OPEN")
        .map(|v| v == "1")
        .unwrap_or(false);
    let dump_png_path = std::env::var("PWB_DUMP_PNG").ok();

    let mut devtools_mounted = false;
    let mut devtools_root_id = None;

    let dom_handle = DomHandle::new(Dom::from_bundle(&bundle));

    // `localStorage` persists per bundle to `~/.pwb/<bundle-hash>.json`, keyed
    // by a short hash of the bundle bytes so different sites keep separate
    // storage. Override with `PWB_STORAGE_DIR` for tests.
    {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::Hasher;
        hasher.write(bundle_bytes);
        let hash = hasher.finish();
        let dir = std::env::var("PWB_STORAGE_DIR").unwrap_or_else(|_| {
            std::env::var("HOME").unwrap_or_else(|_| ".".to_string()) + "/.pwb"
        });
        *dom_handle.web.storage_path.borrow_mut() =
            Some(std::path::PathBuf::from(dir).join(format!("{:016x}.json", hash)));
    }

    // Load and mount devtools if enabled
    let devtools_dir = [
        Path::new("webpages").join("devtools"),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("webpages")
            .join("devtools"),
    ]
    .into_iter()
    .find(|p| p.exists());

    if let Some(dir) = devtools_dir {
        match bundler::bundle_from_dir(&dir) {
            Ok(devtools_bundle) => {
                let mut devtools_dom = Dom::from_bundle(&devtools_bundle);
                // Mark the root so F12 / the close button can find and hide it.
                devtools_root_id = Some(devtools_dom.root.id);
                devtools_dom
                    .root
                    .attributes
                    .insert("data-devtools".to_string(), "root".to_string());
                // Devtools start closed by default: hide the mounted root via
                // `display: none` (the DOM attribute is the source of truth for the
                // F12 toggle). `PWB_DEVTOOLS_OPEN=1` overrides for debugging.
                if !devtools_start_open {
                    devtools_dom
                        .root
                        .attributes
                        .insert("display".to_string(), "none".to_string());
                }
                if let Ok(mut dom) = dom_handle.dom.try_borrow_mut() {
                    dom.add_child(devtools_dom.root);
                }
                devtools_mounted = true;
                println!("[DevTools] Mounted devtools DOM as child of main document");

                // Run the devtools script so its buttons/tabs register handlers.
                if let Some(script_bytes) = &devtools_bundle.script_payload {
                    if let Ok(script_str) = std::str::from_utf8(script_bytes) {
                        if let Err(e) = execute_script(&dom_handle, script_str) {
                            eprintln!("[DevTools JS Error] Devtools script failed: {:?}", e);
                        }
                    }
                }
            }
            Err(e) => eprintln!("[DevTools] Failed to load devtools bundle: {:?}", e),
        }
    } else {
        eprintln!("[DevTools] Warning: devtools directory not found at webpages/devtools");
    }

    let devtools_state = Arc::new(Mutex::new(DevToolsState {
        is_open: devtools_start_open,
    }));

    // Mirror of the selected text, readable from the (static) context-menu Copy
    // action without holding the event-loop's `selection` state.
    let selected_text_shared: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    // Result channel from the (static) menu actions back to the event-loop
    // handler: 0 = nothing, 1 = Copy, 2 = Inspect Element. The actions only
    // record what was chosen (plus do the clipboard write for Copy); DOM state
    // changes happen in the handler where `DomHandle` is reachable.
    let menu_result: Arc<Mutex<u8>> = Arc::new(Mutex::new(0));

    let copy_text = selected_text_shared.clone();
    let copy_result = menu_result.clone();
    let inspect_result = menu_result.clone();
    context_menu::set_actions(ContextMenuActions {
        on_copy: Box::new(move || {
            *copy_result.lock().unwrap() = 1;
            let text = copy_text.lock().unwrap().clone();
            if text.is_empty() {
                return;
            }
            match arboard::Clipboard::new() {
                Ok(mut clipboard) => {
                    if clipboard.set_text(text).is_ok() {
                        println!("[Clipboard] Copied selection via context menu");
                    }
                }
                Err(e) => eprintln!("[Clipboard] Failed to access clipboard: {}", e),
            }
        }),
        on_inspect: Box::new(move || {
            *inspect_result.lock().unwrap() = 2;
            println!("[ContextMenu] Inspect Element chosen");
        }),
    });

    if let Some(script_bytes) = &bundle.script_payload {
        if let Ok(script_str) = std::str::from_utf8(script_bytes) {
            println!("Executing QuickJS-NG PlainJS script payload...");
            if let Err(e) = execute_script(&dom_handle, script_str) {
                eprintln!("[DevTools JS Error] Execution failed: {:?}", e);
            }
        }
    }
    let event_loop = EventLoop::new().map_err(|e| PipelineError::Windowing(e.to_string()))?;

    println!(
        "Opening interactive Skia desktop window (Press F12 or Cmd+Shift+I for devtools.plain)..."
    );
    let mut app = BrowserApp {
        window: None,
        context: None,
        surface: None,
        dom_handle_render: dom_handle.clone(),
        devtools_state,
        selected_text_shared,
        menu_result,
        devtools_root_id,
        devtools_mounted,
        dump_png_path,
        scroll_y: 0.0,
        element_scrolls: std::collections::HashMap::new(),
        selection: TextSelection::default(),
        current_mouse_pos: SelectionPoint { x: 0.0, y: 0.0 },
        last_tooltip: None,
        modifiers_state: winit::keyboard::ModifiersState::default(),
        resize_state: None,
        pixmap: tiny_skia::Pixmap::new(1, 1).unwrap(),
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| PipelineError::Windowing(e.to_string()))?;

    Ok(())
}

/// The winit application state. `window`/`context`/`surface` are created lazily in
/// `resumed` (winit 0.30's `ApplicationHandler` API, which replaces the deprecated
/// `EventLoop::run` + `EventLoop::create_window`), and the interactive session state
/// (scroll, selection, drag-resize, cursor tracking, modifier shortcuts) lives here
/// so it persists between events.
struct BrowserApp {
    window: Option<Rc<Window>>,
    context: Option<softbuffer::Context<Rc<Window>>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    dom_handle_render: DomHandle,
    devtools_state: Arc<Mutex<DevToolsState>>,
    selected_text_shared: Arc<Mutex<String>>,
    menu_result: Arc<Mutex<u8>>,
    devtools_root_id: Option<usize>,
    devtools_mounted: bool,
    dump_png_path: Option<String>,
    scroll_y: f32,
    /// Per-element vertical scroll offsets for `overflow: scroll/auto` boxes,
    /// keyed by node id.
    element_scrolls: std::collections::HashMap<usize, f32>,
    selection: TextSelection,
    current_mouse_pos: SelectionPoint,
    /// The tooltip currently shown (the hovered element's `title`), so cursor
    /// movement only repaints when the hovered tooltip changes instead of on
    /// every mouse move (tooltips used to only appear after a click-triggered
    /// redraw).
    last_tooltip: Option<String>,
    modifiers_state: winit::keyboard::ModifiersState,
    resize_state: Option<ResizeDrag>,
    pixmap: tiny_skia::Pixmap,
}

impl ApplicationHandler for BrowserApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window_title = format!(
            "PlainWeb Browser — {}",
            self.dom_handle_render.dom.borrow().title
        );
        let window_attrs = WindowAttributes::default()
            .with_title(window_title)
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 650.0));

        // NOTE: The softbuffer surface MUST be created once for the window's lifetime.
        // On macOS every `Surface::new` attaches a new `CALayer` sublayer to the window's
        // root layer, and softbuffer never removes it on drop — creating a surface per
        // event (or per frame) piles up layers that the WindowServer has to composite,
        // degrading the whole system the longer the app runs.
        let window = Rc::new(
            event_loop
                .create_window(window_attrs)
                .expect("failed to create window"),
        );
        let context =
            softbuffer::Context::new(window.clone()).expect("failed to create softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone())
            .expect("failed to create softbuffer surface");

        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);

        // The devtools tree was populated at mount time before any window existed
        // (the startup `__pwb_refreshDevtools()` call has no size, so leaf rows
        // used a tiny default truncation budget). Rebuild it now with the real
        // window dimensions so the tree fits its actual width.
        if self.devtools_mounted {
            if let Some(w) = self.window.as_ref() {
                let inner = w.inner_size();
                refresh_devtools(&self.dom_handle_render, (inner.width, inner.height));
            }
        }
    }

    /// Runs before the event loop sleeps: fire due JS timers and resolve
    /// completed async ops (fetch, WebSocket). If any ran, refresh the devtools
    /// tree (their handlers may have mutated the DOM) and redraw. Also wakes the
    /// loop at the next timer deadline instead of sleeping forever.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let mut fired = false;
        fired |= crate::js_runtime::pump_timers(&self.dom_handle_render);
        fired |= crate::js_runtime::poll_async_ops(&self.dom_handle_render);
        if fired {
            if let Some(w) = self.window.as_ref() {
                let inner = w.inner_size();
                refresh_devtools(&self.dom_handle_render, (inner.width, inner.height));
                w.request_redraw();
            }
        }
        if let Some(deadline) = crate::webapi::next_timer_deadline() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        event_loop.set_control_flow(ControlFlow::Wait);
        let Some(window) = &self.window else { return };
        if window_id != window.id() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                // Typing into a focused `<input>`: characters insert at the
                // caret, Backspace deletes before it, arrows move it, Enter
                // submits (dispatches an "enter" DOM event) and Escape blurs.
                // Cmd/Ctrl shortcuts are left alone (F12 / devtools combo etc.).
                if !self.modifiers_state.control_key() && !self.modifiers_state.super_key() {
                    let focused = *self.dom_handle_render.focused_input.borrow();
                    if let Some((node_id, mut caret)) = focused {
                        let mut submit = false;
                        let mut blur = false;
                        let mut changed = false;
                        let mut consumed = false;
                        let mut new_value: Option<String> = None;
                        let value_len = {
                            let dom = self.dom_handle_render.dom.borrow();
                            crate::dom::find_node_by_id_ref(&dom.root, &node_id.to_string())
                                .and_then(|n| n.attributes.get("value").map(|v| v.chars().count()))
                                .unwrap_or(0)
                        };
                        match &key_event.logical_key {
                            winit::keyboard::Key::Character(c) if !c.is_empty() => {
                                let chars: Vec<char> =
                                    c.chars().filter(|ch| !ch.is_control()).collect();
                                if !chars.is_empty() {
                                    let mut value: Vec<char> = {
                                        let dom = self.dom_handle_render.dom.borrow();
                                        crate::dom::find_node_by_id_ref(
                                            &dom.root,
                                            &node_id.to_string(),
                                        )
                                        .and_then(|n| n.attributes.get("value").cloned())
                                        .unwrap_or_default()
                                        .chars()
                                        .collect()
                                    };
                                    caret = caret.min(value.len());
                                    for ch in &chars {
                                        value.insert(caret, *ch);
                                        caret += 1;
                                    }
                                    new_value = Some(value.into_iter().collect());
                                    changed = true;
                                }
                            }
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace) => {
                                let mut value: Vec<char> = {
                                    let dom = self.dom_handle_render.dom.borrow();
                                    crate::dom::find_node_by_id_ref(&dom.root, &node_id.to_string())
                                        .and_then(|n| n.attributes.get("value").cloned())
                                        .unwrap_or_default()
                                        .chars()
                                        .collect()
                                };
                                caret = caret.min(value.len());
                                if caret > 0 && !value.is_empty() {
                                    value.remove(caret - 1);
                                    caret -= 1;
                                    new_value = Some(value.into_iter().collect());
                                    changed = true;
                                }
                            }
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowLeft) => {
                                caret = caret.saturating_sub(1);
                                consumed = true;
                            }
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowRight) => {
                                caret = (caret + 1).min(value_len);
                                consumed = true;
                            }
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter) => {
                                submit = true;
                                consumed = true;
                            }
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowUp) => {
                                let handlers = self
                                    .dom_handle_render
                                    .event_registry
                                    .get_handlers(node_id, "arrowup");
                                for handler in &handlers {
                                    if let Err(e) = execute_event_handler_typed(
                                        &self.dom_handle_render,
                                        node_id,
                                        handler,
                                        Some("arrowup"),
                                    ) {
                                        eprintln!("[EventHandler Error] {}", e);
                                    }
                                }
                                consumed = true;
                            }
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown) => {
                                let handlers = self
                                    .dom_handle_render
                                    .event_registry
                                    .get_handlers(node_id, "arrowdown");
                                for handler in &handlers {
                                    if let Err(e) = execute_event_handler_typed(
                                        &self.dom_handle_render,
                                        node_id,
                                        handler,
                                        Some("arrowdown"),
                                    ) {
                                        eprintln!("[EventHandler Error] {}", e);
                                    }
                                }
                                consumed = true;
                            }
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) => {
                                blur = true;
                                consumed = true;
                            }
                            _ => {}
                        }
                        if changed {
                            if let Some(v) = new_value {
                                let mut dom = self.dom_handle_render.dom.borrow_mut();
                                if let Some(node) =
                                    find_node_by_id(&mut dom.root, &node_id.to_string())
                                {
                                    if v.is_empty() {
                                        node.attributes.remove("value");
                                    } else {
                                        node.attributes.insert("value".to_string(), v);
                                    }
                                }
                            }
                            consumed = true;
                        }
                        if submit {
                            let handlers = self
                                .dom_handle_render
                                .event_registry
                                .get_handlers(node_id, "enter");
                            for handler in &handlers {
                                if let Err(e) = execute_event_handler_typed(
                                    &self.dom_handle_render,
                                    node_id,
                                    handler,
                                    Some("enter"),
                                ) {
                                    eprintln!("[EventHandler Error] {}", e);
                                }
                            }
                            *self.dom_handle_render.focused_input.borrow_mut() = None;
                        } else if blur {
                            *self.dom_handle_render.focused_input.borrow_mut() = None;
                        } else {
                            *self.dom_handle_render.focused_input.borrow_mut() =
                                Some((node_id, caret));
                        }
                        if consumed {
                            let size = window.inner_size();
                            refresh_devtools(&self.dom_handle_render, (size.width, size.height));
                            window.request_redraw();
                            return;
                        }
                    }
                }
                if let PhysicalKey::Code(code) = key_event.physical_key {
                    let is_f12 = code == KeyCode::F12;
                    let is_dev_combo = (code == KeyCode::KeyI || code == KeyCode::KeyJ)
                        && (self.modifiers_state.control_key() || self.modifiers_state.super_key())
                        && self.modifiers_state.shift_key();

                    if is_f12 || is_dev_combo {
                        let mut devtools = self.devtools_state.lock().unwrap();
                        let mut will_open = !devtools.is_open;

                        // When devtools are mounted as DOM, the DOM display attribute is the
                        // source of truth (the close button may have hidden them).
                        if let Some(root_id) = self.devtools_root_id {
                            if let Ok(mut dom) = self.dom_handle_render.dom.try_borrow_mut() {
                                if let Some(root) =
                                    find_node_by_id(&mut dom.root, &root_id.to_string())
                                {
                                    let hidden = root
                                        .attributes
                                        .get("display")
                                        .map(|d| d == "none")
                                        .unwrap_or(false);
                                    will_open = hidden;
                                    if hidden {
                                        root.attributes.remove("display");
                                    } else {
                                        root.attributes
                                            .insert("display".to_string(), "none".to_string());
                                    }
                                }
                                // Closing devtools also ends pick mode and deactivates
                                // the Pick button.
                                if !will_open {
                                    self.dom_handle_render
                                        .inspect_mode
                                        .store(false, Ordering::Relaxed);
                                    if let Some(btn) =
                                        find_node_by_id(&mut dom.root, "select-element")
                                    {
                                        btn.attributes.insert(
                                            "background-color".to_string(),
                                            "#2d2d2d".to_string(),
                                        );
                                        btn.attributes
                                            .insert("color".to_string(), "#cccccc".to_string());
                                    }
                                }
                            }
                        }
                        devtools.is_open = will_open;
                        println!(
                            "\n[DevTools] Keyboard shortcut triggered! DevTools {}",
                            if will_open { "opened" } else { "closed" }
                        );
                        // When opening, the tree was never built while the panel
                        // was hidden (`__pwb_refreshDevtools` no-ops on
                        // `display: none`), so rebuild it now that it is visible
                        // — otherwise the Elements tree stays empty until the
                        // next click/hover event. The size is irrelevant when
                        // closing (the binding no-ops), so pass it either way.
                        if will_open {
                            let size = window.inner_size();
                            refresh_devtools(&self.dom_handle_render, (size.width, size.height));
                        }
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers_state = mods.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let mx = position.x as f32;
                let my = position.y as f32;
                self.current_mouse_pos = SelectionPoint { x: mx, y: my };

                // Only the layout tree is needed for hit-testing; rasterizing the whole
                // frame on every mouse move is a large waste of CPU/GPU. One tree build
                // serves the resize drag, selection update, and cursor icon.
                let size = window.inner_size();
                let layout_root = {
                    let dom = self.dom_handle_render.dom.borrow();
                    build_layout_tree(&dom, size.width.max(1), size.height.max(1), self.scroll_y)
                };

                if let Some(drag) = &mut self.resize_state {
                    // Drag-resize: update the element's width/height attributes from the
                    // mouse delta; layout (and the redraw) follows. The attribute stores
                    // CSS px — an exact device pixel, so no scale compensation is needed.
                    let (new_w, new_h) = resize_new_dimensions(
                        drag.edge,
                        drag.start_w,
                        drag.start_h,
                        drag.start_mx,
                        drag.start_my,
                        mx,
                        my,
                        drag.min_w,
                        drag.max_w,
                    );
                    let mut dom_mut = self.dom_handle_render.dom.borrow_mut();
                    if let Some(node) =
                        find_node_by_id(&mut dom_mut.root, &drag.node_id.to_string())
                    {
                        // px is an exact device pixel, so the attribute stores the
                        // measured physical size directly.
                        if new_w != drag.start_w {
                            node.attributes
                                .insert("width".to_string(), format!("{}px", new_w.round()));
                        }
                        if new_h != drag.start_h {
                            node.attributes
                                .insert("height".to_string(), format!("{}px", new_h.round()));
                        }
                    }
                    drop(dom_mut);
                    // The drag changed width/height attributes: reflect them in
                    // the devtools tree + styles panel.
                    refresh_devtools(&self.dom_handle_render, (size.width, size.height));
                    window.request_redraw();
                    window.set_cursor(drag.cursor());
                } else {
                    if self.selection.is_selecting {
                        // Dragging moves the selection's end boundary to the text under the cursor.
                        if let Some((node_id, char_idx)) = hit_test_text_char_scrolled(
                            &layout_root,
                            mx,
                            my,
                            size.width as f32,
                            size.height as f32,
                            &self.element_scrolls,
                        ) {
                            self.selection.end_node_id = Some(node_id);
                            self.selection.end_char_pos = Some(char_idx);
                        }
                        window.request_redraw();
                    }

                    // Resize handles take cursor priority over the element's own cursor.
                    if let Some((_, edge)) = hit_test_resize_handle(&layout_root, mx, my) {
                        window.set_cursor(match edge {
                            ResizeEdge::Left | ResizeEdge::Right => CursorIcon::EwResize,
                            ResizeEdge::Top | ResizeEdge::Bottom => CursorIcon::NsResize,
                        });
                    } else {
                        let hover_cursor = hit_test_cursor_from_layout_scrolled(
                            &layout_root,
                            mx,
                            my,
                            &self.element_scrolls,
                        );
                        window.set_cursor(hover_cursor);
                    }

                    // Pick mode: repaint on every move so the outline + tint
                    // tracks the element under the cursor live (the highlight is
                    // drawn in RedrawRequested when inspect_mode is set).
                    if self.dom_handle_render.inspect_mode.load(Ordering::Relaxed) {
                        window.request_redraw();
                    }

                    // Tooltip: repaint only when the element under the cursor's
                    // `title` changes — otherwise the tooltip only appeared after
                    // a click-triggered redraw. The tooltip itself is drawn in
                    // RedrawRequested.
                    let dom_snapshot = self.dom_handle_render.dom.borrow();
                    let tt =
                        hovered_title(&dom_snapshot, &layout_root, mx, my, &self.element_scrolls);
                    drop(dom_snapshot);
                    if tt != self.last_tooltip {
                        self.last_tooltip = tt;
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let size = window.inner_size();
                let width = size.width.max(1);
                let height = size.height.max(1);

                match state {
                    ElementState::Pressed => {
                        let mx = self.current_mouse_pos.x;
                        let my = self.current_mouse_pos.y;
                        let layout_root = {
                            let dom = self.dom_handle_render.dom.borrow();
                            build_layout_tree(&dom, width, height, self.scroll_y)
                        };

                        // 1. Grab a resize handle first (highest priority).
                        if let Some((node_id, edge)) = hit_test_resize_handle(&layout_root, mx, my)
                        {
                            if let Some(b) = find_box_by_id(&layout_root, node_id) {
                                let (min_w, max_w) = resize_bounds(
                                    &self.dom_handle_render,
                                    node_id,
                                    edge,
                                    width as f32,
                                    height as f32,
                                );
                                self.resize_state = Some(ResizeDrag {
                                    node_id,
                                    edge,
                                    start_w: b.rect.width(),
                                    start_h: b.rect.height(),
                                    start_mx: mx,
                                    start_my: my,
                                    min_w,
                                    max_w,
                                });
                                println!("[Resize] Grabbed {:?} edge of node {} (min {:.0}px, max {:.0}px)", edge, node_id, min_w, max_w);
                            }
                        } else if let Some(input_box) = hit_test_layout_box_scrolled(
                            &layout_root,
                            mx,
                            my,
                            &self.element_scrolls,
                        )
                        .filter(|b| b.tag == Tag::Input)
                        {
                            // 2. Clicking an `<input>` focuses it and places the
                            //    caret at the click (no text selection).
                            let font_size = input_box
                                .style
                                .font_size
                                .and_then(|uv| {
                                    resolve_unit(
                                        uv,
                                        input_box.rect.width(),
                                        width as f32,
                                        height as f32,
                                    )
                                })
                                .unwrap_or(0.025 * height as f32);
                            let pad = input_box
                                .style
                                .padding
                                .and_then(|uv| {
                                    resolve_unit(
                                        uv,
                                        input_box.rect.width(),
                                        width as f32,
                                        height as f32,
                                    )
                                })
                                .unwrap_or(0.012 * height as f32);
                            let value = input_box.value.as_deref().unwrap_or("");
                            let caret = caret_index_at_x(
                                value,
                                font_size,
                                mx - (input_box.rect.left() + pad + 1.0),
                            );
                            *self.dom_handle_render.focused_input.borrow_mut() =
                                Some((input_box.node_id, caret));
                            self.selection = TextSelection::default();
                            println!(
                                "[Input] Focused node {} at caret {}",
                                input_box.node_id, caret
                            );
                        } else {
                            // 2b. Clicking anywhere else blurs the focused input and
                            //     anchors a text selection at the character under the
                            //     cursor (layout only — frame rasterized on redraw).
                            *self.dom_handle_render.focused_input.borrow_mut() = None;
                            let hit = hit_test_text_char_scrolled(
                                &layout_root,
                                mx,
                                my,
                                width as f32,
                                height as f32,
                                &self.element_scrolls,
                            );
                            match hit {
                                Some((node_id, char_idx)) => {
                                    if self.modifiers_state.shift_key() {
                                        // Shift+click extends: move only the end boundary.
                                        self.selection.end_node_id = Some(node_id);
                                        self.selection.end_char_pos = Some(char_idx);
                                    } else {
                                        self.selection.start_node_id = Some(node_id);
                                        self.selection.start_char_pos = Some(char_idx);
                                        self.selection.end_node_id = Some(node_id);
                                        self.selection.end_char_pos = Some(char_idx);
                                    }
                                    self.selection.is_selecting = true;
                                }
                                None => {
                                    // Clicking non-text (buttons, views) clears the selection.
                                    self.selection = TextSelection::default();
                                }
                            }
                        }
                        window.request_redraw();
                    }
                    ElementState::Released => {
                        // A drag-resize ends here without dispatching a click.
                        if self.resize_state.take().is_some() {
                            println!("[Resize] Released resize handle");
                            window.request_redraw();
                            return;
                        }

                        self.selection.is_selecting = false;

                        // Finalize the copied-text buffer from the finalized range.
                        if let (Some(sn), Some(sc)) =
                            (self.selection.start_node_id, self.selection.start_char_pos)
                        {
                            if let (Some(en), Some(ec)) =
                                (self.selection.end_node_id, self.selection.end_char_pos)
                            {
                                if sn != en || sc != ec {
                                    let dom = self.dom_handle_render.dom.borrow();
                                    self.selection.selected_text_buffer =
                                        compute_selected_text(&dom.root, sn, sc, en, ec);
                                }
                            }
                        }
                        // Mirror for the context-menu Copy action.
                        *self.selected_text_shared.lock().unwrap() =
                            self.selection.selected_text_buffer.clone();

                        // Layout only — the full frame is rasterized by RedrawRequested.
                        let size = window.inner_size();
                        let layout_root = {
                            let dom = self.dom_handle_render.dom.borrow();
                            build_layout_tree(
                                &dom,
                                size.width.max(1),
                                size.height.max(1),
                                self.scroll_y,
                            )
                        };

                        if let Some(target_box) = hit_test_layout_box_scrolled(
                            &layout_root,
                            self.current_mouse_pos.x,
                            self.current_mouse_pos.y,
                            &self.element_scrolls,
                        ) {
                            let el_id = target_box
                                .element_id
                                .clone()
                                .unwrap_or_else(|| format!("node_{}", target_box.node_id));
                            let node_id = target_box.node_id;

                            // Clicking a devtools Elements tree row selects the element it
                            // represents — same preview (outline + tint) as pick mode — and
                            // the click is consumed (devtools internals must not fire page
                            // handlers). The row is found by walking up from the hit text
                            // node to its `data-node`-tagged container, so clicking the
                            // opening tag, the content, or the closing `</tag>` all work.
                            {
                                let dom = self.dom_handle_render.dom.borrow();
                                let row_target = tree_row_target_node(&dom.root, node_id);
                                drop(dom);
                                if let Some(target) = row_target {
                                    let _ = execute_script(
                                        &self.dom_handle_render,
                                        &format!("__pwb_selectElement({});", target),
                                    );
                                    // Rebuild the devtools tree to reflect the new selection
                                    let size = window.inner_size();
                                    refresh_devtools(
                                        &self.dom_handle_render,
                                        (size.width, size.height),
                                    );
                                    window.request_redraw();
                                    return;
                                }
                            }

                            // In inspect (pick) mode, clicking selects exactly one element:
                            // it is reported, its tree row is highlighted, the styles
                            // panel fills in, and pick mode turns itself off. The click is
                            // consumed by the pick — the page's click handlers must NOT
                            // fire (that would mutate the element you're inspecting).
                            if self.dom_handle_render.inspect_mode.load(Ordering::Relaxed) {
                                // A click lands on whatever is under the cursor — often a
                                // Text run inside a container. Resolve it to the actual
                                // element so exactly one element is selected (the one you
                                // clicked), not the text node inside it.
                                let pick_id = {
                                    let dom = self.dom_handle_render.dom.borrow();
                                    element_node_id(&dom.root, node_id)
                                };
                                // Clicking devtools internals (outside a tree row) never
                                // picks — the page is what's being inspected.
                                let over_devtools = {
                                    let dom = self.dom_handle_render.dom.borrow();
                                    find_node_with_attr(&dom.root, "data-devtools")
                                        .map(|root| node_in_subtree(root, pick_id))
                                        .unwrap_or(false)
                                };
                                if !over_devtools {
                                    let (tag_name, el_label) = {
                                        let dom = self.dom_handle_render.dom.borrow();
                                        crate::dom::find_node_by_id_ref(
                                            &dom.root,
                                            &pick_id.to_string(),
                                        )
                                        .map(|n| {
                                            let id = n
                                                .attributes
                                                .get("id")
                                                .cloned()
                                                .unwrap_or_else(|| format!("node_{}", pick_id));
                                            (n.tag.name().to_string(), id)
                                        })
                                        .unwrap_or_else(
                                            || (target_box.tag.name().to_string(), el_id.clone()),
                                        )
                                    };
                                    crate::js_runtime::log_console(
                                        &self.dom_handle_render,
                                        &format!("Inspected <{}>#{}", tag_name, el_label),
                                    );
                                    self.dom_handle_render
                                        .inspect_mode
                                        .store(false, Ordering::Relaxed);
                                    let _ = execute_script(
                                        &self.dom_handle_render,
                                        &format!("__pwb_selectElement({});", pick_id),
                                    );
                                }
                            } else {
                                // Handlers are registered under the node id (new API) and the
                                // element id attribute / node id string (legacy API). A text
                                // run inside a container (e.g. a tab label in a View) hits
                                // the Text node, so clicks bubble up to the nearest
                                // ancestor that has a click handler — like a browser.
                                let chain: Vec<(usize, String)> = {
                                    let dom = self.dom_handle_render.dom.borrow();
                                    let mut ids = vec![node_id];
                                    ids.extend(ancestor_node_ids(&dom.root, node_id));
                                    ids.into_iter()
                                        .map(|id| {
                                            let el = node_id_attr(&dom.root, id)
                                                .unwrap_or_else(|| format!("node_{}", id));
                                            (id, el)
                                        })
                                        .collect()
                                };
                                for (target, target_el) in chain {
                                    let handlers = self
                                        .dom_handle_render
                                        .event_registry
                                        .get_handlers(target, "click");
                                    if !handlers.is_empty() {
                                        println!("[EventHandler] Dispatching click event for element '#{}'...", target_el);
                                        for handler_script in handlers {
                                            if let Err(e) = execute_event_handler(
                                                &self.dom_handle_render,
                                                target,
                                                &handler_script,
                                            ) {
                                                eprintln!("[EventHandler Error] {}", e);
                                            }
                                        }
                                        break;
                                    }
                                    let mut legacy_handlers = self
                                        .dom_handle_render
                                        .event_registry
                                        .get_click_handlers(&target_el);
                                    if legacy_handlers.is_empty() {
                                        legacy_handlers = self
                                            .dom_handle_render
                                            .event_registry
                                            .get_click_handlers(&target.to_string());
                                    }
                                    if !legacy_handlers.is_empty() {
                                        println!("[EventHandler] Dispatching click event for element '#{}' (legacy API)...", target_el);
                                        for handler_script in legacy_handlers {
                                            if let Err(e) = execute_event_handler(
                                                &self.dom_handle_render,
                                                target,
                                                &handler_script,
                                            ) {
                                                eprintln!("[EventHandler Error] {}", e);
                                            }
                                        }
                                        break;
                                    }
                                }
                            }
                        }

                        // The handlers may have mutated the DOM (text/styles):
                        // refresh the devtools Elements tree and styles panel.
                        refresh_devtools(&self.dom_handle_render, (width, height));

                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Right,
                ..
            } => {
                // Right-click: dispatch a `contextmenu` DOM event to the element
                // under the cursor. If any handler calls `preventDefault()` the
                // native menu is suppressed; otherwise the OS context menu shows.
                let size = window.inner_size();
                let layout_root = {
                    let dom = self.dom_handle_render.dom.borrow();
                    build_layout_tree(&dom, size.width.max(1), size.height.max(1), self.scroll_y)
                };
                // Reset once for the whole dispatch: any handler calling
                // `preventDefault()` suppresses the native menu.
                self.dom_handle_render
                    .suppress_context_menu
                    .store(false, Ordering::Relaxed);
                if let Some(target_box) = hit_test_layout_box_scrolled(
                    &layout_root,
                    self.current_mouse_pos.x,
                    self.current_mouse_pos.y,
                    &self.element_scrolls,
                ) {
                    let node_id = target_box.node_id;
                    let handlers = self
                        .dom_handle_render
                        .event_registry
                        .get_handlers(node_id, "contextmenu");
                    for handler_script in handlers {
                        if let Err(e) = execute_event_handler_typed(
                            &self.dom_handle_render,
                            node_id,
                            &handler_script,
                            Some("contextmenu"),
                        ) {
                            eprintln!("[EventHandler Error] {}", e);
                        }
                    }
                }
                let suppressed = self
                    .dom_handle_render
                    .suppress_context_menu
                    .load(Ordering::Relaxed);
                self.dom_handle_render
                    .suppress_context_menu
                    .store(false, Ordering::Relaxed);
                if !suppressed {
                    *self.menu_result.lock().unwrap() = 0;
                    context_menu::show_context_menu(
                        window,
                        self.current_mouse_pos.x,
                        self.current_mouse_pos.y,
                    );
                    // The menu runs a nested event loop; once it closes, apply the
                    // chosen action that needs access to DOM/loop state.
                    if *self.menu_result.lock().unwrap() == 2 {
                        let mut dev = self.devtools_state.lock().unwrap();
                        dev.is_open = true;
                        drop(dev);
                        if let Some(root_id) = self.devtools_root_id {
                            if let Ok(mut dom) = self.dom_handle_render.dom.try_borrow_mut() {
                                if let Some(node) =
                                    find_node_by_id(&mut dom.root, &root_id.to_string())
                                {
                                    node.attributes.remove("display");
                                }
                            }
                        }
                        self.dom_handle_render
                            .inspect_mode
                            .store(true, Ordering::Relaxed);
                        println!("[ContextMenu] DevTools opened, pick mode on");
                    }
                } else {
                    println!("[ContextMenu] Native menu suppressed by preventDefault()");
                }
                // The handler may have mutated the DOM: refresh devtools.
                refresh_devtools(&self.dom_handle_render, (size.width, size.height));
                window.request_redraw();
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(c),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.modifiers_state.control_key() && c == "c" => {
                if !self.selection.selected_text_buffer.is_empty() {
                    match Clipboard::new() {
                        Ok(mut clipboard) => {
                            if clipboard
                                .set_text(self.selection.selected_text_buffer.clone())
                                .is_ok()
                            {
                                println!("[Clipboard] Copied selection to system clipboard");
                            }
                        }
                        Err(e) => eprintln!("[Clipboard] Failed to access clipboard: {}", e),
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll_amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 25.0,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                let size = window.inner_size();
                let width = size.width.max(1);
                let height = size.height.max(1);
                let dom_snapshot = self.dom_handle_render.dom.borrow();
                let layout_root = build_layout_tree(&dom_snapshot, width, height, 0.0);

                // Over a scroll container (e.g. the devtools tree/console), scroll
                // that element instead of the page.
                if let Some(container) = find_scroll_container(
                    &layout_root,
                    self.current_mouse_pos.x,
                    self.current_mouse_pos.y,
                    &self.element_scrolls,
                ) {
                    let max_scroll = (container.content_height - container.rect.height()).max(0.0);
                    let entry = self.element_scrolls.entry(container.node_id).or_insert(0.0);
                    *entry = (*entry - scroll_amount).clamp(0.0, max_scroll);
                    drop(dom_snapshot);
                    window.request_redraw();
                    return;
                }

                // Otherwise the wheel is for the page — unless the cursor is over
                // the devtools (they sit "on top": wheel there never scrolls the
                // page underneath).
                let over_devtools = hit_test_layout_box_scrolled(
                    &layout_root,
                    self.current_mouse_pos.x,
                    self.current_mouse_pos.y,
                    &self.element_scrolls,
                )
                .and_then(|hit| {
                    find_node_with_attr(&dom_snapshot.root, "data-devtools")
                        .map(|root| node_in_subtree(root, hit.node_id))
                })
                .unwrap_or(false);
                if !over_devtools {
                    self.scroll_y = (self.scroll_y - scroll_amount).max(0.0);
                    let content_height = layout_root.rect.height();
                    let max_scroll = (content_height - height as f32).max(0.0);
                    self.scroll_y = self.scroll_y.min(max_scroll);
                }

                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let size = window.inner_size();
                let width = size.width.max(1);
                let height = size.height.max(1);

                if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
                    let _ = self.surface.as_mut().unwrap().resize(w, h);

                    let dom_snapshot = self.dom_handle_render.dom.borrow();
                    // Auto-scroll elements flagged for "scroll to bottom" (the
                    // devtools console jumps to its newest line). Compute each
                    // box's max scroll against the fresh layout, then render
                    // with the updated offsets.
                    let pending_scroll: Vec<usize> = self
                        .dom_handle_render
                        .scroll_to_bottom
                        .borrow_mut()
                        .drain(..)
                        .collect();
                    if !pending_scroll.is_empty() {
                        let layout_root =
                            build_layout_tree(&dom_snapshot, width, height, self.scroll_y);
                        apply_scroll_to_bottom(
                            &layout_root,
                            &mut self.element_scrolls,
                            &pending_scroll,
                        );
                    }
                    let input_focus = *self.dom_handle_render.focused_input.borrow();
                    let (layout_root, _scrollbar_info) = render_dom_into_pixmap(
                        &mut self.pixmap,
                        &dom_snapshot,
                        width,
                        height,
                        self.scroll_y,
                        &self.selection,
                        &self.element_scrolls,
                        input_focus,
                    );

                    // Re-hit-test the cursor against the freshly laid-out tree: a click
                    // handler may have resized/moved the element under the pointer, so
                    // the old icon (e.g. pointer over a now-shrunk button) must update.
                    let hover_cursor = hit_test_cursor_from_layout_scrolled(
                        &layout_root,
                        self.current_mouse_pos.x,
                        self.current_mouse_pos.y,
                        &self.element_scrolls,
                    );
                    window.set_cursor(hover_cursor);

                    // Mark the hovered element in the main section — devtools-only:
                    // a solid fill + outline while the Pick button is active, nothing
                    // otherwise (the element is reported on the Pick click itself).
                    // Never highlight devtools internals: they sit "on top" of the page.
                    let inspect = self.dom_handle_render.inspect_mode.load(Ordering::Relaxed);
                    if inspect {
                        let over_devtools = hit_test_layout_box_scrolled(
                            &layout_root,
                            self.current_mouse_pos.x,
                            self.current_mouse_pos.y,
                            &self.element_scrolls,
                        )
                        .and_then(|hit| {
                            find_node_with_attr(&dom_snapshot.root, "data-devtools")
                                .map(|root| node_in_subtree(root, hit.node_id))
                        })
                        .unwrap_or(false);
                        if !over_devtools {
                            draw_hover_highlight_scrolled(
                                &mut self.pixmap,
                                &layout_root,
                                self.current_mouse_pos.x,
                                self.current_mouse_pos.y,
                                true,
                                &self.element_scrolls,
                            );
                        }
                    }

                    // A devtools-selected element keeps its outline + tint in the
                    // main section (same preview as pick mode), so selecting it from
                    // the Elements tree makes it visible in the page. Skip devtools
                    // internals (the selection is a devtools node, not a page one).
                    // The check is on the selected node ONLY — not on what the cursor
                    // happens to be over: right after clicking a tree row the cursor
                    // is still inside the devtools, and gating on it hid the page
                    // highlight until the next click into page content.
                    if let Some(selected) = *self.dom_handle_render.selected_node_id.borrow() {
                        let selected_in_devtools =
                            find_node_with_attr(&dom_snapshot.root, "data-devtools")
                                .map(|root| node_in_subtree(root, selected))
                                .unwrap_or(false);
                        if !selected_in_devtools {
                            draw_selected_highlight_scrolled(
                                &mut self.pixmap,
                                &layout_root,
                                selected,
                                &self.element_scrolls,
                            );
                        }
                    }

                    // Tooltip: show the hovered element's `title` attribute as a
                    // small floating box near the cursor. `last_tooltip` was kept
                    // up to date on cursor movement, and the tooltip is drawn each
                    // frame so it follows the pointer while visible.
                    if let Some(title) = hovered_title(
                        &dom_snapshot,
                        &layout_root,
                        self.current_mouse_pos.x,
                        self.current_mouse_pos.y,
                        &self.element_scrolls,
                    ) {
                        draw_tooltip(
                            &mut self.pixmap,
                            &title,
                            self.current_mouse_pos.x,
                            self.current_mouse_pos.y,
                            width,
                            height,
                        );
                    }

                    // Debug: dump the rendered frame so the devtools can be inspected
                    // without macOS screen-capture permissions.
                    if let Some(path) = &self.dump_png_path {
                        if !path.is_empty() {
                            let _ = self.pixmap.save_png(path);
                        }
                    }

                    if let Ok(mut buffer) = self.surface.as_mut().unwrap().buffer_mut() {
                        let pixels = self.pixmap.data();
                        for (i, pixel) in buffer.iter_mut().enumerate() {
                            let idx = i * 4;
                            if idx + 3 < pixels.len() {
                                let r = pixels[idx] as u32;
                                let g = pixels[idx + 1] as u32;
                                let b = pixels[idx + 2] as u32;
                                let a = pixels[idx + 3] as u32;

                                *pixel = (a << 24) | (r << 16) | (g << 8) | b;
                            }
                        }
                        let _ = buffer.present();
                    }
                }
            }
            WindowEvent::Resized(_) => {
                window.request_redraw();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::DomNode;
    use pwb_core::spec::Tag;
    use std::collections::HashMap;

    fn tn(id: usize, text: &str) -> DomNode {
        DomNode {
            id,
            tag: Tag::Text,
            attributes: HashMap::new(),
            text: Some(text.to_string()),
            children: vec![],
        }
    }

    fn root_with(children: Vec<DomNode>) -> DomNode {
        DomNode {
            id: 1,
            tag: Tag::View,
            attributes: HashMap::new(),
            text: None,
            children,
        }
    }

    #[test]
    fn test_ancestor_chain_and_subtree() {
        // Text (3) inside a button (2) inside the root (1); a devtools subtree (4).
        let root = root_with(vec![
            DomNode {
                id: 2,
                tag: Tag::Button,
                attributes: HashMap::new(),
                text: None,
                children: vec![tn(3, "Pick")],
            },
            DomNode {
                id: 4,
                tag: Tag::View,
                attributes: HashMap::from([("data-devtools".to_string(), "root".to_string())]),
                text: None,
                children: vec![tn(5, "row")],
            },
        ]);
        // Clicking the label bubbles: nearest ancestor first, excluding itself.
        assert_eq!(ancestor_node_ids(&root, 3), vec![2, 1]);
        assert_eq!(ancestor_node_ids(&root, 5), vec![4, 1]);
        assert_eq!(ancestor_node_ids(&root, 1), Vec::<usize>::new());

        // Devtools-membership checks (used to keep devtools "on top").
        assert!(node_in_subtree(&root.children[1], 5));
        assert!(!node_in_subtree(&root.children[1], 3));
    }

    #[test]
    fn test_tree_row_target_node_finds_element() {
        // A devtools tree row (View with `data-node`) holding text children.
        // Clicking the row itself, a text child, or a close row (no id, but
        // `data-node`) all resolve to the represented element's node id.
        let row = DomNode {
            id: 10,
            tag: Tag::View,
            attributes: HashMap::from([("data-node".to_string(), "7".to_string())]),
            text: None,
            children: vec![
                tn(11, "  <button"),
                tn(12, "> Click me"),
                tn(13, "</button>"),
            ],
        };
        let close_row = DomNode {
            id: 20,
            tag: Tag::View,
            attributes: HashMap::from([("data-node".to_string(), "7".to_string())]),
            text: None,
            children: vec![tn(21, "  </view>")],
        };
        let root = root_with(vec![row, close_row]);
        assert_eq!(
            tree_row_target_node(&root, 10),
            Some(7),
            "row container itself"
        );
        assert_eq!(
            tree_row_target_node(&root, 12),
            Some(7),
            "text child of the row"
        );
        assert_eq!(tree_row_target_node(&root, 21), Some(7), "closing row");
        // Non-row nodes (the root) resolve to nothing.
        assert_eq!(tree_row_target_node(&root, 1), None);
    }

    #[test]
    fn test_resize_new_dimensions() {
        let (w, h) = resize_new_dimensions(
            ResizeEdge::Left,
            866.0,
            1300.0,
            934.0,
            100.0,
            834.0,
            100.0,
            40.0,
            f32::MAX,
        );
        assert_eq!(w, 966.0); // dragged left 100px → wider
        assert_eq!(h, 1300.0);
        let (w2, _) = resize_new_dimensions(
            ResizeEdge::Right,
            400.0,
            100.0,
            500.0,
            0.0,
            550.0,
            0.0,
            40.0,
            f32::MAX,
        );
        assert_eq!(w2, 450.0);
        // Clamped to the minimum (dragging the left edge right past the width).
        let (w3, _) = resize_new_dimensions(
            ResizeEdge::Left,
            100.0,
            100.0,
            100.0,
            0.0,
            300.0,
            0.0,
            40.0,
            f32::MAX,
        );
        assert_eq!(w3, 40.0);
        // Clamped to the maximum (the devtools sidebar's `max-width`).
        let (w4, _) = resize_new_dimensions(
            ResizeEdge::Right,
            300.0,
            100.0,
            500.0,
            0.0,
            900.0,
            0.0,
            40.0,
            450.0,
        );
        assert_eq!(w4, 450.0);
    }

    #[test]
    fn test_resize_bounds_reads_min_max_width() {
        // The drag clamp comes from the element's `min-width`/`max-width`.
        let root = DomNode {
            id: 1,
            tag: Tag::View,
            attributes: HashMap::from([
                ("min-width".to_string(), "300px".to_string()),
                ("max-width".to_string(), "450px".to_string()),
            ]),
            text: None,
            children: vec![],
        };
        let handle = DomHandle::new(crate::dom::Dom {
            title: "t".to_string(),
            root,
            components: HashMap::new(),
            modules: HashMap::new(),
        });
        let (min, max) = resize_bounds(&handle, 1, ResizeEdge::Left, 900.0, 600.0);
        assert_eq!(min, 300.0);
        assert_eq!(max, 450.0);
        // Vertical edges keep the default 40px floor / no cap.
        let (min, max) = resize_bounds(&handle, 1, ResizeEdge::Top, 900.0, 600.0);
        assert_eq!(min, 40.0);
        assert_eq!(max, f32::MAX);
    }

    #[test]
    fn test_element_node_id_resolves_text_to_container() {
        // Clicking a Text run inside a button resolves to the button (the
        // element), not the text node — pick mode selects exactly one element.
        let root = root_with(vec![DomNode {
            id: 2,
            tag: Tag::Button,
            attributes: HashMap::new(),
            text: None,
            children: vec![tn(3, "Click me")],
        }]);
        assert_eq!(
            element_node_id(&root, 3),
            2,
            "text run resolves to its container"
        );
        assert_eq!(element_node_id(&root, 2), 2, "element resolves to itself");
    }

    #[test]
    fn test_compute_selected_text_same_node() {
        let root = root_with(vec![tn(2, "hello world")]);
        assert_eq!(compute_selected_text(&root, 2, 6, 2, 11), "world");
    }

    #[test]
    fn test_compute_selected_text_reversed_range() {
        let root = root_with(vec![tn(2, "hello world")]);
        // Drag left: end before start still selects the same characters.
        assert_eq!(compute_selected_text(&root, 2, 11, 2, 6), "world");
    }

    #[test]
    fn test_compute_selected_text_across_nodes() {
        let root = root_with(vec![tn(2, "foo"), tn(3, "bar"), tn(4, "baz")]);
        assert_eq!(compute_selected_text(&root, 2, 1, 4, 2), "oobarba");
    }

    #[test]
    fn test_compute_selected_text_unknown_node() {
        let root = root_with(vec![tn(2, "hello")]);
        assert_eq!(compute_selected_text(&root, 2, 0, 99, 3), "");
    }

    #[test]
    fn test_apply_scroll_to_bottom() {
        use crate::dom::{Dom, DomNode};
        use pwb_core::spec::Tag;
        // A scroll container (overflow: scroll) with tall content.
        let scroll_box = DomNode {
            id: 5,
            tag: Tag::View,
            attributes: HashMap::from([
                ("overflow".to_string(), "scroll".to_string()),
                ("height".to_string(), "100px".to_string()),
            ]),
            text: None,
            children: vec![DomNode {
                id: 6,
                tag: Tag::View,
                attributes: HashMap::from([("height".to_string(), "400px".to_string())]),
                text: None,
                children: vec![],
            }],
        };
        let root = DomNode {
            id: 1,
            tag: Tag::View,
            attributes: HashMap::new(),
            text: None,
            children: vec![scroll_box],
        };
        let dom = Dom {
            title: "t".to_string(),
            root,
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        let layout = crate::layout::build_layout_tree(&dom, 900, 600, 0.0);
        let mut scrolls = std::collections::HashMap::new();
        apply_scroll_to_bottom(&layout, &mut scrolls, &[5]);
        // 400px content in a 100px viewport → scrolled to (near) the max
        // (block-flow spacing adds a little to the content height).
        let scrolled = *scrolls.get(&5).expect("scrolled");
        assert!(
            (300.0..=400.0).contains(&scrolled),
            "scrolled to bottom of the container (got {})",
            scrolled
        );
        // Unknown id: ignored.
        apply_scroll_to_bottom(&layout, &mut scrolls, &[999]);
        assert!(!scrolls.contains_key(&999));
    }

    #[test]
    fn test_find_title_for_node() {
        use crate::dom::DomNode;
        use std::collections::HashMap;

        let child = DomNode {
            id: 42,
            tag: pwb_core::spec::Tag::Button,
            attributes: {
                let mut m = HashMap::new();
                m.insert("title".to_string(), "Click me".to_string());
                m
            },
            text: Some("Go".to_string()),
            children: vec![],
        };
        let root = DomNode {
            id: 1,
            tag: pwb_core::spec::Tag::View,
            attributes: HashMap::new(),
            text: None,
            children: vec![child],
        };
        assert_eq!(find_title_for_node(&root, 42), Some("Click me".to_string()));
        assert_eq!(find_title_for_node(&root, 1), None);
        assert_eq!(find_title_for_node(&root, 99), None);
    }
}
