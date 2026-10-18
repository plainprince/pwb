//! Browser application state and the winit event handler.

use super::devtools::{refresh_devtools, DevToolsState};
use super::dom::{
    ancestor_node_ids, compute_selected_text, element_node_id, find_node_by_id,
    find_node_with_attr, hovered_title, node_id_attr, node_in_subtree, TOOLTIP_DELAY,
};
use crate::dom::Dom;
use super::resize::{resize_bounds, resize_new_dimensions, ResizeDrag};
use crate::context_menu;
use crate::js_runtime::{
    execute_event_handler, execute_event_handler_typed, execute_script, log_console, DomHandle,
};
use crate::layout::{
    build_layout_tree, caret_index_at_x, find_box_by_id, hit_test_resize_handle,
    hit_test_text_char_scrolled, resolve_unit, ResizeEdge,
};
use crate::pipeline::{apply_scroll_to_bottom, tree_row_target_node};
use crate::render::{
    draw_hover_highlight_scrolled, draw_selected_highlight_scrolled, draw_tooltip,
    find_scroll_container, hit_test_cursor_from_layout_scrolled, hit_test_layout_box_scrolled,
    render_dom_into_pixmap, SelectionPoint, TextSelection,
};
use arboard::Clipboard;
use pwb_core::spec::Tag;
use pwb_node::domain::DomainRegistry;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::search::SearchEngine;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

/// The winit application state. `window`/`context`/`surface` are created lazily in
/// `resumed` (winit 0.30's `ApplicationHandler` API, which replaces the deprecated
/// `EventLoop::run` + `EventLoop::create_window`), and the interactive session state
/// (scroll, selection, drag-resize, cursor tracking, modifier shortcuts) lives here
/// so it persists between events.
pub struct BrowserApp {
    pub(super) window: Option<Rc<Window>>,
    pub(super) context: Option<softbuffer::Context<Rc<Window>>>,
    pub(super) surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    pub(super) dom_handle_render: DomHandle,
    pub(super) devtools_state: Arc<Mutex<DevToolsState>>,
    pub(super) selected_text_shared: Arc<Mutex<String>>,
    pub(super) menu_result: Arc<Mutex<u8>>,
    pub(super) devtools_root_id: Option<usize>,
    pub(super) devtools_mounted: bool,
    pub(super) dump_png_path: Option<String>,
    pub(super) scroll_y: f32,
    /// Per-element vertical scroll offsets for `overflow: scroll/auto` boxes,
    /// keyed by node id.
    pub(super) element_scrolls: std::collections::HashMap<usize, f32>,
    pub(super) selection: TextSelection,
    pub(super) current_mouse_pos: SelectionPoint,
    /// The tooltip currently drawn on screen (the dwell-approved `title`), so
    /// `about_to_wait` can tell whether a fully-dwelled candidate still needs
    /// its first repaint.
    pub(super) last_tooltip: Option<String>,
    /// The `title` currently under the cursor (dwell candidate), independent of
    /// whether the tooltip has appeared yet.
    pub(super) hovered_title_candidate: Option<String>,
    /// When `hovered_title_candidate` first came under the cursor; the tooltip
    /// appears only once [`TOOLTIP_DELAY`] has elapsed.
    pub(super) tooltip_hover_since: Option<std::time::Instant>,
    pub(super) modifiers_state: winit::keyboard::ModifiersState,
    pub(super) resize_state: Option<ResizeDrag>,
    pub(super) pixmap: tiny_skia::Pixmap,
    /// Resolver dependencies for in-place navigation swaps.
    pub(super) search: Arc<SearchEngine>,
    pub(super) host: Arc<ArtifactHost>,
    pub(super) registry: Arc<DomainRegistry>,
    pub(super) identity: Arc<IdentityManager>,
}

impl BrowserApp {
    /// Hot-swap the DOM bundle for `url` within the same event loop: resolve
    /// the URL to a `.pwb` bundle, replace the DOM handle, re-run the page
    /// script, and re-mount devtools. Returns `true` on success.
    pub(super) fn navigate_to(&mut self, url: &str) -> bool {
        use crate::resolver::resolve_url_or_file;

        let bytes: Vec<u8> = match resolve_url_or_file(
            url,
            &self.search,
            &self.host,
            &self.registry,
            &self.identity,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[Browser] Navigation failed to resolve '{}': {:?}", url, e);
                return false;
            }
        };

        let bundle = match pwb_core::wire::Bundle::deserialize(&bytes) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[Browser] Navigation failed to deserialize bundle: {:?}", e);
                return false;
            }
        };

        // Replace the DOM in place (the JS context shares this DomHandle, so
        // it will see the new tree on the next script call).
        *self.dom_handle_render.dom.borrow_mut() = Dom::from_bundle(&bundle);

        // Set localStorage path for the new bundle.
        {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            use std::hash::Hasher;
            hasher.write(&bytes);
            let hash = hasher.finish();
            let dir = std::env::var("PWB_STORAGE_DIR").unwrap_or_else(|_| {
                std::env::var("HOME").unwrap_or_else(|_| ".".to_string()) + "/.pwb"
            });
            *self.dom_handle_render.web.storage_path.borrow_mut() =
                Some(std::path::PathBuf::from(dir).join(format!("{:016x}.json", hash)));
        }

        // Re-run the page script so handlers are registered for the new DOM.
        if let Some(script_bytes) = &bundle.script_payload {
            if let Ok(script_str) = std::str::from_utf8(script_bytes) {
                if let Err(e) = execute_script(&self.dom_handle_render, script_str) {
                    eprintln!("[Browser] Navigation script failed: {:?}", e);
                }
            }
        }

        // Update window title.
        if let Some(w) = self.window.as_ref() {
            w.set_title(&format!(
                "PlainWeb Browser — {}",
                self.dom_handle_render.dom.borrow().title
            ));
        }

        println!("[Browser] Navigated to {}", url);
        true
    }
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

        // In-place navigation swap: when a JS handler set pending_navigate on
        // the DomHandle, resolve the URL and swap the DOM bundle without
        // recreating the winit EventLoop (which cannot be recreated in the
        // same process).
        let pending_url = self.dom_handle_render.pending_navigate.borrow_mut().take();
        if let Some(url) = pending_url {
            if self.window.is_some() {
                self.navigate_to(&url);
            }
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }

        // Tooltip dwell: a still pointer produces no events, so wake the loop
        // at the dwell deadline (or immediately, once it has passed and the
        // tooltip still hasn't been drawn).
        if self.hovered_title_candidate.is_some()
            && self.last_tooltip != self.hovered_title_candidate
        {
            if let Some(since) = self.tooltip_hover_since {
                let deadline = since + TOOLTIP_DELAY;
                let now = std::time::Instant::now();
                if now < deadline {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                } else if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
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

                        // If an "enter" handler requested navigation (via
                        // `__pwb_navigate`), request redraw so `about_to_wait`
                        // hot-swaps the DOM without recreating the EventLoop.
                        if self.dom_handle_render.pending_navigate.borrow().is_some() {
                            window.request_redraw();
                            return;
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

                    // Tooltip dwell tracking: when the hovered `title` changes,
                    // restart the dwell timer and repaint immediately so the old
                    // tooltip disappears; a redraw to *show* the new one is
                    // scheduled by `about_to_wait` once `TOOLTIP_DELAY` passes.
                    let dom_snapshot = self.dom_handle_render.dom.borrow();
                    let tt =
                        hovered_title(&dom_snapshot, &layout_root, mx, my, &self.element_scrolls);
                    drop(dom_snapshot);
                    if tt != self.hovered_title_candidate {
                        self.hovered_title_candidate = tt;
                        self.tooltip_hover_since = self
                            .hovered_title_candidate
                            .as_ref()
                            .map(|_| std::time::Instant::now());
                        window.request_redraw();
                    } else if self.hovered_title_candidate.is_some()
                        && self.last_tooltip != self.hovered_title_candidate
                    {
                        // Same candidate, tooltip not yet shown: wake the loop at
                        // the dwell deadline (a still pointer produces no events).
                        if let Some(since) = self.tooltip_hover_since {
                            event_loop
                                .set_control_flow(ControlFlow::WaitUntil(since + TOOLTIP_DELAY));
                        }
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
                                    log_console(
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

                        // If a click handler requested navigation (via
                        // `__pwb_navigate`), set pending so `about_to_wait`
                        // hot-swaps the DOM without recreating the EventLoop.
                        if self.dom_handle_render.pending_navigate.borrow().is_some() {
                            window.request_redraw();
                            return;
                        }

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

                    // Tooltip: draw the hovered element's `title` once the
                    // pointer has dwelled on it for `TOOLTIP_DELAY` (native
                    // tooltips never pop instantly). `last_tooltip` records what
                    // was drawn so `about_to_wait` knows when the dwell-eligible
                    // candidate still needs its first repaint.
                    let tooltip_title = self.hovered_title_candidate.clone().filter(|_| {
                        self.tooltip_hover_since
                            .map(|s| s.elapsed() >= TOOLTIP_DELAY)
                            .unwrap_or(false)
                    });
                    if let Some(title) = &tooltip_title {
                        draw_tooltip(
                            &mut self.pixmap,
                            title,
                            self.current_mouse_pos.x,
                            self.current_mouse_pos.y,
                            width,
                            height,
                        );
                    }
                    self.last_tooltip = tooltip_title;

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
