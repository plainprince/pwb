//! QuickJS-NG JavaScript Runtime Integration & Custom `document` Bindings (`pwb-browser::js_runtime`).

use crate::dom::{Dom, DomNode};
use crate::event_handler::EventHandlerRegistry;
use crate::webapi;
use crate::webapi::WebApiState;
use pwb_core::spec::Tag;
use rquickjs::function::Opt;
use rquickjs::prelude::List;
use rquickjs::{CatchResultExt, Context, Function, Object, Runtime};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug)]
pub enum JsError {
    InitFailed(String),
    ExecutionFailed(String),
}

impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsError::InitFailed(msg) => write!(f, "JS runtime init failed: {}", msg),
            JsError::ExecutionFailed(msg) => write!(f, "JS execution failed: {}", msg),
        }
    }
}

/// Dynamic DOM Handle shared between QuickJS runtime and Rust Browser.
#[derive(Clone)]
pub struct DomHandle {
    pub dom: Rc<RefCell<Dom>>,
    pub event_registry: EventHandlerRegistry,
    /// True while devtools element-inspection is active (select-element button).
    pub inspect_mode: Arc<AtomicBool>,
    /// Set by a handler calling `event.preventDefault()` on a `contextmenu`
    /// event; the pipeline reads it to decide whether to show the native menu.
    pub suppress_context_menu: Arc<AtomicBool>,
    /// The devtools-selected element (numeric node id), if any. The devtools
    /// styles panel shows this element's attributes; `__pwb_refreshDevtools`
    /// re-highlights its tree row and re-renders the panel.
    pub selected_node_id: Rc<RefCell<Option<usize>>>,
    /// The focused `<input>` as `(node_id, caret_index)`, if any. Shared with
    /// the pipeline (click-to-focus, typing) and JS (`element.focus()`); the
    /// renderer draws the caret for this element.
    pub focused_input: Rc<RefCell<Option<(usize, usize)>>>,
    /// Web API state: `localStorage`/`sessionStorage` maps and detached DOM
    /// nodes (`document.createElement` before they are appended).
    pub web: WebApiState,
    /// Node ids to scroll to their bottom on the next redraw (the devtools
    /// console output auto-scrolls to the newest line).
    pub scroll_to_bottom: Rc<RefCell<Vec<usize>>>,
}

impl DomHandle {
    pub fn new(dom: Dom) -> Self {
        Self {
            dom: Rc::new(RefCell::new(dom)),
            event_registry: EventHandlerRegistry::new(),
            inspect_mode: Arc::new(AtomicBool::new(false)),
            suppress_context_menu: Arc::new(AtomicBool::new(false)),
            selected_node_id: Rc::new(RefCell::new(None)),
            focused_input: Rc::new(RefCell::new(None)),
            web: WebApiState::new(),
            scroll_to_bottom: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

// A single QuickJS engine shared by all scripts in the page.
//
// Creating a fresh `Runtime` + full `Context` per `execute_script` call (as before)
// allocates a full JS engine on every invocation — twice at startup and again on every
// click — leaving many megabytes of cached allocation behind. Browsers run one engine
// per page, and rquickjs contexts are designed to be reused; each call re-installs the
// `document`/`console` bindings for the current DOM handle.
thread_local! {
    static JS_CONTEXT: RefCell<Option<rquickjs::Context>> = const { RefCell::new(None) };
}

fn get_or_create_context() -> Result<rquickjs::Context, JsError> {
    JS_CONTEXT.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            let runtime = Runtime::new().map_err(|e| JsError::InitFailed(e.to_string()))?;
            let context =
                Context::full(&runtime).map_err(|e| JsError::InitFailed(e.to_string()))?;
            *borrow = Some(context);
        }
        Ok(borrow.as_ref().unwrap().clone())
    })
}

/// Execute a JS script payload against a DOM handle with custom `document` bindings.
pub fn execute_script(dom_handle: &DomHandle, script: &str) -> Result<(), JsError> {
    let context = get_or_create_context()?;

    context.with(|ctx| {
        let global = ctx.globals();

        // 1. console.log/info/warn/error bindings: printed to stdout and mirrored into
        //    the devtools `#console-output` panel when it is mounted.
        let console = Object::new(ctx.clone()).unwrap();
        for (name, level) in [("log", "log"), ("info", "log"), ("warn", "warn"), ("error", "error")] {
            let dom_c = dom_handle.clone();
            let level = level.to_string();
            let log_fn = Function::new(ctx.clone(), move |msg: String| {
                println!("[QuickJS Console] {}", msg);
                append_console_line(&dom_c, &level, &msg);
            })
            .unwrap();
            console.set(name, log_fn).unwrap();
        }
        global.set("console", console).unwrap();

        // 2. Native Rust primitive helpers for DOM querying, text/style mutations,
        //    and event handlers.
        let dom_c1 = dom_handle.clone();
        let query_fn = Function::new(ctx.clone(), move |selector: String| -> String {
            let dom = dom_c1.dom.borrow();
            if let Some(node) = find_node_by_selector(&dom.root, &selector) {
                node.id.to_string()
            } else {
                String::new()
            }
        })
        .unwrap();
        global.set("__pwb_querySelector", query_fn).unwrap();

        let dom_c2 = dom_handle.clone();
        let get_el_id_fn = Function::new(ctx.clone(), move |node_id: String| -> String {
            let dom = dom_c2.dom.borrow();
            if let Some(node) = find_node_by_id(&dom.root, &node_id) {
                node.attributes.get("id").cloned().unwrap_or(node_id)
            } else {
                node_id
            }
        })
        .unwrap();
        global.set("__pwb_getElementId", get_el_id_fn).unwrap();

        let dom_c3 = dom_handle.clone();
        let get_text_fn = Function::new(ctx.clone(), move |id_or_tag: String| -> String {
            {
                let dom = dom_c3.dom.borrow();
                if let Some(node) = find_node_by_selector(&dom.root, &id_or_tag) {
                    return node.text.clone().unwrap_or_default();
                }
            }
            // Detached `createElement` nodes live outside the tree.
            if let Ok(id) = id_or_tag.parse::<usize>() {
                return dom_c3
                    .web
                    .detached
                    .borrow()
                    .get(&id)
                    .and_then(|n| n.text.clone())
                    .unwrap_or_default();
            }
            String::new()
        })
        .unwrap();
        global.set("__pwb_getText", get_text_fn).unwrap();

        let dom_c4 = dom_handle.clone();
        let set_text_fn = Function::new(ctx.clone(), move |id_or_tag: String, text: String| -> bool {
            {
                let mut dom = dom_c4.dom.borrow_mut();
                if let Some(node) = find_node_by_selector_mut(&mut dom.root, &id_or_tag) {
                    node.text = Some(text);
                    return true;
                }
            }
            if let Ok(id) = id_or_tag.parse::<usize>() {
                if let Some(node) = dom_c4.web.detached.borrow_mut().get_mut(&id) {
                    node.text = Some(text);
                    return true;
                }
            }
            false
        })
        .unwrap();
        global.set("__pwb_setText", set_text_fn).unwrap();

        // `<input>` value: getter/setter over the `value` attribute (the live
        // value the renderer draws and typing updates).
        let dom_val = dom_handle.clone();
        let get_value_fn = Function::new(ctx.clone(), move |id_or_tag: String| -> String {
            let dom = dom_val.dom.borrow();
            find_node_by_selector(&dom.root, &id_or_tag)
                .and_then(|n| n.attributes.get("value").cloned())
                .unwrap_or_default()
        })
        .unwrap();
        global.set("__pwb_getValue", get_value_fn).unwrap();

        let dom_val2 = dom_handle.clone();
        let set_value_fn = Function::new(ctx.clone(), move |id_or_tag: String, value: String| -> bool {
            let mut dom = dom_val2.dom.borrow_mut();
            if let Some(node) = find_node_by_selector_mut(&mut dom.root, &id_or_tag) {
                if value.is_empty() {
                    node.attributes.remove("value");
                } else {
                    node.attributes.insert("value".to_string(), value);
                }
                true
            } else {
                false
            }
        })
        .unwrap();
        global.set("__pwb_setValue", set_value_fn).unwrap();

        // Focus/blur an `<input>`: `element.focus()`/`element.blur()` and the
        // pipeline share the same `focused_input` state (caret at value end).
        let foc = dom_handle.clone();
        global
            .set(
                "__pwb_focusInput",
                Function::new(ctx.clone(), move |id_or_tag: String| -> bool {
                    let dom = foc.dom.borrow();
                    let Some(node) = find_node_by_selector(&dom.root, &id_or_tag) else {
                        return false;
                    };
                    let node_id = node.id;
                    let caret = node.attributes.get("value").map(|v| v.chars().count()).unwrap_or(0);
                    drop(dom);
                    *foc.focused_input.borrow_mut() = Some((node_id, caret));
                    true
                })
                .unwrap(),
            )
            .unwrap();

        let blr = dom_handle.clone();
        global
            .set(
                "__pwb_blurInput",
                Function::new(ctx.clone(), move || -> bool {
                    *blr.focused_input.borrow_mut() = None;
                    true
                })
                .unwrap(),
            )
            .unwrap();

        let dom_c5 = dom_handle.clone();
        let get_style_fn = Function::new(ctx.clone(), move |id_or_tag: String, prop: String| -> String {
            let dom = dom_c5.dom.borrow();
            if let Some(node) = find_node_by_selector(&dom.root, &id_or_tag) {
                node.attributes.get(&prop).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        })
        .unwrap();
        global.set("__pwb_getStyle", get_style_fn).unwrap();

        let dom_c6 = dom_handle.clone();
        let set_style_fn = Function::new(ctx.clone(), move |id_or_tag: String, prop: String, val: String| -> bool {
            let mut dom = dom_c6.dom.borrow_mut();
            if let Some(node) = find_node_by_selector_mut(&mut dom.root, &id_or_tag) {
                node.attributes.insert(prop, val);
                true
            } else {
                false
            }
        })
        .unwrap();
        global.set("__pwb_setStyle", set_style_fn).unwrap();

        // Event handler registration: handlers are keyed by node id (both the new
        // usize-keyed registry and the legacy string registry) and by the element's
        // `id` attribute so dispatch can find them either way.
        let dom_c7 = dom_handle.clone();
        let reg_event_fn = Function::new(ctx.clone(), move |id: String, event_type: String, js_code: String| {
            let node_id: Option<usize> = id.parse().ok();
            if let Some(nid) = node_id {
                dom_c7.event_registry.add_handler(nid, &event_type, js_code.clone());
            }
            dom_c7.event_registry.register_click_handler(&id, js_code.clone());
            if let Ok(dom) = dom_c7.dom.try_borrow() {
                if let Some(node) = find_node_by_id(&dom.root, &id) {
                    if let Some(el_id) = node.attributes.get("id") {
                        dom_c7.event_registry.register_click_handler(el_id, js_code);
                    }
                }
            }
            println!("[QuickJS Console] Registered {} handler for node {}", event_type, id);
        })
        .unwrap();
        global.set("__pwb_registerEventHandler", reg_event_fn).unwrap();

        // `event.preventDefault()` on a `contextmenu` event flips this flag so
        // the pipeline suppresses the native OS menu.
        let sup = dom_handle.clone();
        global
            .set(
                "__pwb_suppressContextMenu",
                Function::new(ctx.clone(), move |suppress: bool| {
                    sup.suppress_context_menu.store(suppress, Ordering::Relaxed);
                })
                .unwrap(),
            )
            .unwrap();

        // DevTools management APIs
        let dom_c8 = dom_handle.clone();
        global
            .set(
                "__pwb_closeDevtools",
                Function::new(ctx.clone(), move || -> String {
                    let mut dom = dom_c8.dom.borrow_mut();
                    if let Some(node) = find_node_by_selector_mut(&mut dom.root, "[data-devtools]") {
                        node.attributes.insert("display".to_string(), "none".to_string());
                    }
                    "DevTools closed".to_string()
                })
                .unwrap(),
            )
            .unwrap();

        let insp_on = dom_handle.clone();
        global
            .set(
                "__pwb_startElementSelection",
                Function::new(ctx.clone(), move || -> String {
                    insp_on.inspect_mode.store(true, Ordering::Relaxed);
                    "Element selection started".to_string()
                })
                .unwrap(),
            )
            .unwrap();

        let insp_off = dom_handle.clone();
        global
            .set(
                "__pwb_stopElementSelection",
                Function::new(ctx.clone(), move || -> String {
                    insp_off.inspect_mode.store(false, Ordering::Relaxed);
                    "Element selection stopped".to_string()
                })
                .unwrap(),
            )
            .unwrap();

        // DOM tree listing for the devtools Elements panel (excludes the devtools
        // subtree itself). One pipe-separated row per node:
        //   `node_id|depth|tag|id|text`
        let dom_tree = dom_handle.clone();
        let get_tree_fn = Function::new(ctx.clone(), move || -> String {
            let dom = dom_tree.dom.borrow();
            let mut out = String::new();
            collect_dom_tree_lines(&dom.root, 0, &mut out);
            out.trim_end().to_string()
        })
        .unwrap();
        global.set("__pwb_getDomTree", get_tree_fn).unwrap();

        // Build one syntax-highlighted tree row: a flex-row View with colored parts
        // (`<tag`, `#id`, `> text`, `</tag>`) so the Elements panel looks like a
        // real devtools. The row gets `id = row_id` so picking an element can
        // highlight its row.
        let dom_tr = dom_handle.clone();
        let append_tree_row_fn = Function::new(
            ctx.clone(),
            move |parent_selector: String,
                  row_id: String,
                  indent: String,
                  tag: String,
                  id: String,
                  text: String| -> bool {
                let next_base = {
                    let dom = dom_tr.dom.borrow();
                    max_node_id(&dom.root) + 1
                };
                let mut dom = dom_tr.dom.borrow_mut();
                if let Some(parent) = find_node_by_selector_mut(&mut dom.root, &parent_selector) {
                    let mut next_id = next_base;
                    let row = make_tree_row(&row_id, &mut next_id, &indent, &tag, &id, &text);
                    parent.children.push(row);
                    true
                } else {
                    false
                }
            },
        )
        .unwrap();
        global.set("__pwb_appendTreeRow", append_tree_row_fn).unwrap();

        // Select an element in the devtools: record the selection, highlight its
        // tree row, deactivate the Pick button, and fill the styles panel at the
        // bottom. Called by the pipeline when an element is picked.
        let sel = dom_handle.clone();
        global
            .set(
                "__pwb_selectElement",
                Function::new(ctx.clone(), move |node_id: i64| -> String {
                    let parsed = node_id.max(0) as usize;
                    *sel.selected_node_id.borrow_mut() = Some(parsed);
                    let mut dom = sel.dom.borrow_mut();
                    // Highlight the selected element's row and every row of its
                    // contents (its descendant elements), so selecting a container
                    // marks the whole subtree like a browser's inspector.
                    highlight_selected_node(&mut dom, parsed);
                    if let Some(btn) = find_node_by_id_mut(&mut dom.root, "select-element") {
                        btn.attributes.insert("background-color".to_string(), "#2d2d2d".to_string());
                        btn.attributes.insert("color".to_string(), "#cccccc".to_string());
                    }
                    render_styles_panel(&mut dom, Some(parsed));
                    format!("selected {}", node_id)
                })
                .unwrap(),
            )
            .unwrap();

        // Rebuild the devtools Elements tree from the live DOM, re-apply the
        // selected row's highlight, and re-render the styles panel. Called by the
        // pipeline after any event-driven DOM mutation (click handlers, resize
        // drags, contextmenu handlers). No-ops while devtools are closed.
        // `vw`/`vh` are the real window dimensions (physical px) used to budget
        // leaf-row content so closing tags never overflow the tree. The startup
        // call from app.pjs passes nothing, in which case a default budget is
        // used and the next event-driven refresh corrects it.
        let refr = dom_handle.clone();
        global
            .set(
                "__pwb_refreshDevtools",
                Function::new(ctx.clone(), move |vw: Opt<f64>, vh: Opt<f64>| -> bool {
                    let mut dom = refr.dom.borrow_mut();
                    if let Some(root) = find_node_by_selector(&dom.root, "[data-devtools]") {
                        if root.attributes.get("display").map(|d| d == "none").unwrap_or(false) {
                            return false;
                        }
                    }
                    let selected = *refr.selected_node_id.borrow();
                    let vw = vw.0.unwrap_or(0.0).max(1.0) as f32;
                    let vh = vh.0.unwrap_or(0.0).max(1.0) as f32;
                    rebuild_tree_rows(&mut dom, selected, vw, vh);
                    render_styles_panel(&mut dom, selected);
                    true
                })
                .unwrap(),
            )
            .unwrap();

        global
            .set(
                "__pwb_inspectElement",
                Function::new(ctx.clone(), move |id: String| -> String { format!("Element inspected: {}", id) })
                    .unwrap(),
            )
            .unwrap();

        global
            .set(
                "__pwb_onDevtoolsOpen",
                Function::new(ctx.clone(), move |callback: rquickjs::Value| -> rquickjs::Result<()> {
                    if let Some(fn_val) = callback.as_function() {
                        let _: rquickjs::Result<()> = fn_val.call(());
                    }
                    Ok(())
                })
                .unwrap(),
            )
            .unwrap();

        // Component registry: list names (the `pwb.components()` binding).
        let dom_cp = dom_handle.clone();
        global
            .set(
                "__pwb_componentNames",
                Function::new(ctx.clone(), move || -> String {
                    let dom = dom_cp.dom.borrow();
                    let mut names: Vec<&String> = dom.components.keys().collect();
                    names.sort();
                    names.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(",")
                })
                .unwrap(),
            )
            .unwrap();

        // Instantiate a component and append it to a parent (default: the page
        // root). Props are a JSON object whose values fill `{param}`
        // placeholders. Returns the new instance's node id ("" on failure).
        let dom_cc = dom_handle.clone();
        global
            .set(
                "__pwb_createComponent",
                Function::new(ctx.clone(), move |name: String, props_json: String, parent_selector: String| -> String {
                    let params = parse_props_json(&props_json);
                    let mut dom = dom_cc.dom.borrow_mut();
                    let mut instance = match dom.components.get(&name) {
                        Some(body) => body.clone(),
                        None => return String::new(),
                    };
                    substitute_params_dom(&mut instance, &params);
                    let base = max_node_id(&dom.root) + 1;
                    reassign_ids(&mut instance, base);
                    let parent_id = if parent_selector.is_empty() {
                        dom.root.id
                    } else {
                        find_node_by_selector(&dom.root, &parent_selector)
                            .map(|n| n.id)
                            .unwrap_or(dom.root.id)
                    };
                    if let Some(parent) = find_node_by_id_mut(&mut dom.root, &parent_id.to_string()) {
                        parent.children.push(instance);
                        base.to_string()
                    } else {
                        String::new()
                    }
                })
                .unwrap(),
            )
            .unwrap();

        // Apply PSS text to the live DOM (the `pwb.loadStyle(css)` binding) —
        // no `<style>` element or head injection needed.
        let dom_ls = dom_handle.clone();
        global
            .set(
                "__pwb_loadStyle",
                Function::new(ctx.clone(), move |css_text: String| -> bool {
                    let mut dom = dom_ls.dom.borrow_mut();
                    crate::bundler::apply_pss_to_dom(&mut dom.root, &css_text)
                })
                .unwrap(),
            )
            .unwrap();

        // Fetch a bundled module's source by name (the `pwb.import`/`pwb.hotload`
        // bindings eval it; "" when unknown).
        let dom_im = dom_handle.clone();
        let module_fn = move |name: String| -> String {
            let dom = dom_im.dom.borrow();
            dom.modules
                .get(&name)
                .map(|code| String::from_utf8_lossy(code).to_string())
                .unwrap_or_default()
        };
        global
            .set(
                "__pwb_importModule",
                Function::new(ctx.clone(), module_fn.clone()).unwrap(),
            )
            .unwrap();
        global
            .set(
                "__pwb_hotloadModule",
                Function::new(ctx.clone(), module_fn).unwrap(),
            )
            .unwrap();

        // 5. Web API bindings (fetch, timers, storage, dialogs, crypto, base64,
        //    performance.now, DOM building, WebSocket). The JS-facing wrappers
        //    live in the doc_wrapper below.

        // Fresh op id for fetch/timer/WebSocket operations. The JS wrapper
        // allocates the id (via this binding) and keeps its callbacks in global
        // registries, so Rust never holds JS values.
        global
            .set(
                "__pwb_nextOpId",
                Function::new(ctx.clone(), move || -> u64 { webapi::next_op_id() }).unwrap(),
            )
            .unwrap();

        // fetch: starts an HTTP request on a background thread; the result is
        // resolved into the promise by `poll_async_ops`, which calls the
        // resolve/reject functions the JS wrapper registered in
        // `__pwb_pending[id]`.
        let fetch_handle = dom_handle.clone();
        let fetch_fn = Function::new(ctx.clone(), move |id: u64, url: String, method: String, headers_json: String, body: String| {
            let headers: Vec<(String, String)> = serde_json::from_str::<serde_json::Value>(&headers_json)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .map(|m| {
                    m.into_iter()
                        .filter_map(|(k, val)| val.as_str().map(|s| (k, s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            webapi::spawn_fetch(&fetch_handle.web, id, url, method, headers, body);
        })
        .unwrap();
        global.set("__pwb_fetchStart", fetch_fn).unwrap();

        // setTimeout/setInterval: the JS wrapper keeps the callback in the
        // `__pwb_timers` global registry; here we only track the deadline, and
        // `pump_timers` (event loop) fires the callback by registry lookup.
        let timer_fn = Function::new(ctx.clone(), move |id: u64, ms: f64, interval: bool| {
            webapi::register_timer(id, ms, interval);
        })
        .unwrap();
        global.set("__pwb_setTimer", timer_fn).unwrap();
        let clear_fn = Function::new(ctx.clone(), move |id: u64| {
            webapi::clear_timer(id);
        })
        .unwrap();
        global.set("__pwb_clearTimer", clear_fn).unwrap();

        // localStorage / sessionStorage: `kind` is "local" or "session"; `op` is
        // get/set/remove/clear/key/length.
        let stor = dom_handle.clone();
        let storage_fn = Function::new(ctx.clone(), move |kind: String, op: String, key: String, value: String| -> String {
            webapi::storage_op(&stor.web, &kind, &op, &key, &value)
        })
        .unwrap();
        global.set("__pwb_storage", storage_fn).unwrap();

        // alert / confirm / prompt (native dialogs on macOS, console fallbacks
        // elsewhere).
        global
            .set(
                "__pwb_alert",
                Function::new(ctx.clone(), move |msg: String| {
                    webapi::show_alert(&msg);
                })
                .unwrap(),
            )
            .unwrap();
        global
            .set(
                "__pwb_confirm",
                Function::new(ctx.clone(), move |msg: String| -> bool { webapi::show_confirm(&msg) }).unwrap(),
            )
            .unwrap();
        global
            .set(
                "__pwb_prompt",
                Function::new(ctx.clone(), move |msg: String, default_val: String| -> Option<String> {
                    webapi::show_prompt(&msg, &default_val)
                })
                .unwrap(),
            )
            .unwrap();

        // crypto + base64 + performance.now.
        global
            .set(
                "__pwb_randomBytes",
                Function::new(ctx.clone(), move |n: u32| -> String { webapi::random_bytes(n) }).unwrap(),
            )
            .unwrap();
        global
            .set(
                "__pwb_randomUUID",
                Function::new(ctx.clone(), move || -> String { webapi::random_uuid() }).unwrap(),
            )
            .unwrap();
        // btoa/atob return `[ok, value, error]` (a JS array); the wrapper throws
        // on !ok. (Raw tuples aren't `IntoJs`; `List` converts them to arrays.)
        let btoa_fn = Function::new(ctx.clone(), move |s: String| -> List<(bool, String, String)> {
            match webapi::btoa(&s) {
                Ok(v) => List((true, v, String::new())),
                Err(e) => List((false, String::new(), e)),
            }
        })
        .unwrap();
        global.set("__pwb_btoa", btoa_fn).unwrap();
        let atob_fn = Function::new(ctx.clone(), move |s: String| -> List<(bool, String, String)> {
            match webapi::atob(&s) {
                Ok(v) => List((true, v, String::new())),
                Err(e) => List((false, String::new(), e)),
            }
        })
        .unwrap();
        global.set("__pwb_atob", atob_fn).unwrap();
        global
            .set(
                "__pwb_performanceNow",
                Function::new(ctx.clone(), move || -> f64 { webapi::performance_now() }).unwrap(),
            )
            .unwrap();

        // DOM building: createElement returns a detached node id; appendChild /
        // removeChild move nodes between the tree and the detached registry;
        // set/get/removeAttribute work on tree or detached nodes.
        let dom_ce = dom_handle.clone();
        global
            .set(
                "__pwb_createElement",
                Function::new(ctx.clone(), move |tag: String| -> u64 {
                    let tag: Tag = tag.trim().parse().unwrap_or(Tag::View);
                    let tree_max = {
                        let dom = dom_ce.dom.borrow();
                        max_node_id(&dom.root)
                    };
                    let id = webapi::next_free_id(&dom_ce.web, tree_max);
                    webapi::add_detached(
                        &dom_ce.web,
                        id,
                        DomNode {
                            id,
                            tag,
                            attributes: HashMap::new(),
                            text: None,
                            children: Vec::new(),
                        },
                    );
                    id as u64
                })
                .unwrap(),
            )
            .unwrap();

        let dom_root = dom_handle.clone();
        global
            .set(
                "__pwb_documentRoot",
                Function::new(ctx.clone(), move || -> u64 { dom_root.dom.borrow().root.id as u64 }).unwrap(),
            )
            .unwrap();

        let dom_ac = dom_handle.clone();
        global
            .set(
                "__pwb_appendChild",
                Function::new(ctx.clone(), move |parent_id: String, child_id: String| -> bool {
                    let Some(node) = webapi::take_detached(&dom_ac.web, child_id.parse().unwrap_or(0)) else {
                        return false;
                    };
                    let mut dom = dom_ac.dom.borrow_mut();
                    if let Some(parent) = find_node_by_id_mut(&mut dom.root, &parent_id) {
                        parent.children.push(node);
                        true
                    } else {
                        webapi::put_detached(&dom_ac.web, node);
                        false
                    }
                })
                .unwrap(),
            )
            .unwrap();

        let dom_rc = dom_handle.clone();
        global
            .set(
                "__pwb_removeChild",
                Function::new(ctx.clone(), move |parent_id: String, child_id: String| -> bool {
                    let mut dom = dom_rc.dom.borrow_mut();
                    let Some(parent) = find_node_by_id_mut(&mut dom.root, &parent_id) else {
                        return false;
                    };
                    if let Some(pos) = parent.children.iter().position(|c| c.id == child_id.parse().unwrap_or(0)) {
                        let node = parent.children.remove(pos);
                        drop(dom);
                        webapi::put_detached(&dom_rc.web, node);
                        true
                    } else {
                        false
                    }
                })
                .unwrap(),
            )
            .unwrap();

        // `el.remove()`: find the element's parent anywhere in the tree.
        let dom_rp = dom_handle.clone();
        global
            .set(
                "__pwb_removeFromParent",
                Function::new(ctx.clone(), move |node_id: String| -> bool {
                    let mut dom = dom_rp.dom.borrow_mut();
                    let Some(node) = remove_node_from_tree(&mut dom.root, node_id.parse().unwrap_or(0)) else {
                        return false;
                    };
                    drop(dom);
                    webapi::put_detached(&dom_rp.web, node);
                    true
                })
                .unwrap(),
            )
            .unwrap();

        let dom_sa = dom_handle.clone();
        global
            .set(
                "__pwb_setAttribute",
                Function::new(ctx.clone(), move |node_id: String, name: String, value: String| -> bool {
                    {
                        let mut dom = dom_sa.dom.borrow_mut();
                        if let Some(node) = find_node_by_id_mut(&mut dom.root, &node_id) {
                            node.attributes.insert(name, value);
                            return true;
                        }
                    }
                    if let Some(node) = dom_sa.web.detached.borrow_mut().get_mut(&node_id.parse().unwrap_or(0)) {
                        node.attributes.insert(name, value);
                        return true;
                    }
                    false
                })
                .unwrap(),
            )
            .unwrap();

        let dom_ga = dom_handle.clone();
        global
            .set(
                "__pwb_getAttribute",
                Function::new(ctx.clone(), move |node_id: String, name: String| -> String {
                    {
                        let dom = dom_ga.dom.borrow();
                        if let Some(node) = find_node_by_id(&dom.root, &node_id) {
                            if let Some(v) = node.attributes.get(&name) {
                                return v.clone();
                            }
                        }
                    }
                    dom_ga
                        .web
                        .detached
                        .borrow()
                        .get(&node_id.parse().unwrap_or(0))
                        .and_then(|n| n.attributes.get(&name).cloned())
                        .unwrap_or_default()
                })
                .unwrap(),
            )
            .unwrap();

        let dom_ra = dom_handle.clone();
        global
            .set(
                "__pwb_removeAttribute",
                Function::new(ctx.clone(), move |node_id: String, name: String| -> bool {
                    {
                        let mut dom = dom_ra.dom.borrow_mut();
                        if let Some(node) = find_node_by_id_mut(&mut dom.root, &node_id) {
                            return node.attributes.remove(&name).is_some();
                        }
                    }
                    dom_ra
                        .web
                        .detached
                        .borrow_mut()
                        .get_mut(&node_id.parse().unwrap_or(0))
                        .map(|n| n.attributes.remove(&name).is_some())
                        .unwrap_or(false)
                })
                .unwrap(),
            )
            .unwrap();

        // WebSocket: the connection lives on a background thread; events are
        // dispatched to the JS `WebSocket` objects via `__pwb_wsOnEvent` by
        // `poll_async_ops`.
        let ws_handle = dom_handle.clone();
        global
            .set(
                "__pwb_wsOpen",
                Function::new(ctx.clone(), move |url: String| -> u64 {
                    webapi::ws_open(&ws_handle.web, &url)
                })
                .unwrap(),
            )
            .unwrap();
        let ws_send_handle = dom_handle.clone();
        global
            .set(
                "__pwb_wsSend",
                Function::new(ctx.clone(), move |id: u64, data: String| {
                    webapi::ws_send(&ws_send_handle.web, id, &data);
                })
                .unwrap(),
            )
            .unwrap();
        let ws_close_handle = dom_handle.clone();
        global
            .set(
                "__pwb_wsClose",
                Function::new(ctx.clone(), move |id: u64| {
                    webapi::ws_close(&ws_close_handle.web, id);
                })
                .unwrap(),
            )
            .unwrap();

        // Set document object
        global.set("document", Object::new(ctx.clone()).unwrap()).unwrap();

        let initial_title = dom_handle.dom.borrow().title.clone();

        // 3. Inject JS `document` object wrapper
        let doc_wrapper = format!(
            r##"
            function makeEl(nodeId) {{
                return {{
                    id: __pwb_getElementId(nodeId),
                    nodeId: nodeId,
                    text: __pwb_getText(nodeId),
                    getText: function() {{ return __pwb_getText(nodeId); }},
                    setText: function(t) {{ __pwb_setText(nodeId, t); this.text = t; }},
                    html: __pwb_getText(nodeId),
                    getHtml: function() {{ return __pwb_getText(nodeId); }},
                    setHtml: function(h) {{ __pwb_setText(nodeId, h); this.html = h; }},
                    value: __pwb_getValue(nodeId),
                    getValue: function() {{ return __pwb_getValue(nodeId); }},
                    setValue: function(v) {{ __pwb_setValue(nodeId, String(v)); this.value = String(v); }},
                    focus: function() {{ __pwb_focusInput(nodeId); }},
                    blur: function() {{ __pwb_blurInput(); }},
                    getStyle: function(p) {{ return __pwb_getStyle(nodeId, p); }},
                    setStyle: function(p, v) {{ __pwb_setStyle(nodeId, p, v); }},
                    onClick: function(handler) {{ __pwb_registerEventHandler(nodeId, "click", String(handler)); }},
                    addEventHandler: function(evt, handler) {{ __pwb_registerEventHandler(nodeId, evt, String(handler)); }},
                    setAttribute: function(k, v) {{ __pwb_setAttribute(nodeId, String(k), String(v)); }},
                    getAttribute: function(k) {{ return __pwb_getAttribute(nodeId, String(k)); }},
                    removeAttribute: function(k) {{ __pwb_removeAttribute(nodeId, String(k)); }},
                    appendChild: function(child) {{ if (child && child.nodeId) __pwb_appendChild(nodeId, child.nodeId); return child; }},
                    removeChild: function(child) {{ if (child && child.nodeId) __pwb_removeChild(nodeId, child.nodeId); return child; }},
                    remove: function() {{ __pwb_removeFromParent(nodeId); }}
                }};
            }}

            globalThis.document = {{
                title: "{title}",
                selector: function(sel) {{
                    var targetId = __pwb_querySelector(sel);
                    if (targetId && targetId.length > 0) {{ return makeEl(targetId); }}
                    return null;
                }},
                getElementById: function(id) {{
                    return this.selector("#" + id);
                }},
                createElement: function(tag) {{
                    var id = __pwb_createElement(String(tag));
                    return makeEl(String(id));
                }},
                body: makeEl(String(__pwb_documentRoot()))
            }};

            // PlainWeb module system: components, runtime styles, and dynamic
            // script loading via the bundled named-module map.
            globalThis.pwb = {{
                components: function() {{
                    var s = __pwb_componentNames();
                    return s.length ? s.split(",") : [];
                }},
                createComponent: function(name, props, parentSelector) {{
                    var id = __pwb_createComponent(name, JSON.stringify(props || {{}}), parentSelector || "");
                    return id && id.length > 0 ? makeEl(id) : null;
                }},
                loadStyle: function(css) {{ return __pwb_loadStyle(css); }},
                import: function(name) {{
                    var src = __pwb_importModule(name);
                    if (src) {{ (0, eval)(src); return true; }}
                    return false;
                }},
                hotload: function(name) {{
                    var src = __pwb_hotloadModule(name);
                    if (src) {{ (0, eval)(src); return true; }}
                    return false;
                }}
            }};

            // ---- Web APIs ----
            // Pending fetch resolvers and timer callbacks live in these global
            // registries (they survive every `execute_script` call in the shared
            // context), so Rust never stores JS function handles.
            if (!globalThis.__pwb_pending) globalThis.__pwb_pending = {{}};
            if (!globalThis.__pwb_timers) globalThis.__pwb_timers = {{}};

            globalThis.fetch = function(url, options) {{
                options = options || {{}};
                return new Promise(function(resolve, reject) {{
                    var id = __pwb_nextOpId();
                    globalThis.__pwb_pending[id] = {{ resolve: resolve, reject: reject }};
                    var method = String(options.method || "GET").toUpperCase();
                    var headers = options.headers || {{}};
                    var headerObj = {{}};
                    for (var k in headers) {{
                        if (Object.prototype.hasOwnProperty.call(headers, k)) headerObj[k] = String(headers[k]);
                    }}
                    var body = options.body != null ? String(options.body) : "";
                    __pwb_fetchStart(id, String(url), method, JSON.stringify(headerObj), body);
                }}).then(function(raw) {{
                    return {{
                        ok: raw.ok,
                        status: raw.status,
                        statusText: raw.statusText,
                        url: raw.url,
                        headers: raw._headers || {{}},
                        text: function() {{ return Promise.resolve(raw._body); }},
                        json: function() {{ return Promise.resolve(JSON.parse(raw._body)); }},
                        arrayBuffer: function() {{
                            var bytes = new Uint8Array(raw._body.length);
                            for (var i = 0; i < raw._body.length; i++) bytes[i] = raw._body.charCodeAt(i);
                            return Promise.resolve(bytes.buffer);
                        }}
                    }};
                }});
            }};

            globalThis.setTimeout = function(fn, ms) {{
                var id = __pwb_nextOpId();
                globalThis.__pwb_timers[id] = fn;
                __pwb_setTimer(id, ms || 0, false);
                return id;
            }};
            globalThis.setInterval = function(fn, ms) {{
                var id = __pwb_nextOpId();
                globalThis.__pwb_timers[id] = fn;
                __pwb_setTimer(id, ms || 0, true);
                return id;
            }};
            globalThis.clearTimeout = function(id) {{ delete globalThis.__pwb_timers[id]; __pwb_clearTimer(id); }};
            globalThis.clearInterval = function(id) {{ delete globalThis.__pwb_timers[id]; __pwb_clearTimer(id); }};

            function makeStorage(kind) {{
                return {{
                    getItem: function(k) {{ return __pwb_storage(kind, "get", String(k), ""); }},
                    setItem: function(k, v) {{ __pwb_storage(kind, "set", String(k), String(v)); }},
                    removeItem: function(k) {{ __pwb_storage(kind, "remove", String(k), ""); }},
                    clear: function() {{ __pwb_storage(kind, "clear", "", ""); }},
                    key: function(i) {{ return __pwb_storage(kind, "key", String(i), ""); }},
                    get length() {{ return parseInt(__pwb_storage(kind, "length", "", ""), 10) || 0; }}
                }};
            }}
            globalThis.localStorage = makeStorage("local");
            globalThis.sessionStorage = makeStorage("session");

            globalThis.alert = function(msg) {{ __pwb_alert(msg == null ? "" : String(msg)); }};
            globalThis.confirm = function(msg) {{ return __pwb_confirm(msg == null ? "" : String(msg)); }};
            globalThis.prompt = function(msg, def) {{ return __pwb_prompt(msg == null ? "" : String(msg), def == null ? "" : String(def)); }};

            globalThis.crypto = {{
                getRandomValues: function(arr) {{
                    if (!arr || typeof arr.length !== "number" || arr.length === 0) return arr;
                    var s = __pwb_randomBytes(arr.length);
                    for (var i = 0; i < arr.length; i++) arr[i] = s.charCodeAt(i);
                    return arr;
                }},
                randomUUID: function() {{ return __pwb_randomUUID(); }}
            }};
            globalThis.btoa = function(s) {{
                var r = __pwb_btoa(String(s));
                if (!r[0]) throw new Error(r[2]);
                return r[1];
            }};
            globalThis.atob = function(s) {{
                var r = __pwb_atob(String(s));
                if (!r[0]) throw new Error(r[2]);
                return r[1];
            }};
            globalThis.performance = {{ now: function() {{ return __pwb_performanceNow(); }} }};

            // ---- WebSocket ----
            if (!globalThis.__pwb_wsObjects) globalThis.__pwb_wsObjects = {{}};
            globalThis.WebSocket = function(url) {{
                this.url = String(url);
                this.readyState = 0;
                this.bufferedAmount = 0;
                this._id = __pwb_wsOpen(this.url);
                globalThis.__pwb_wsObjects[this._id] = this;
            }};
            WebSocket.CONNECTING = 0;
            WebSocket.OPEN = 1;
            WebSocket.CLOSING = 2;
            WebSocket.CLOSED = 3;
            WebSocket.prototype.send = function(data) {{ __pwb_wsSend(this._id, String(data)); this.bufferedAmount = 0; }};
            WebSocket.prototype.close = function() {{
                if (this.readyState !== 3) {{ this.readyState = 2; __pwb_wsClose(this._id); }}
            }};
            globalThis.__pwb_wsOnEvent = function(id, type, data) {{
                var ws = globalThis.__pwb_wsObjects[id];
                if (!ws) return;
                if (type === "open") {{
                    ws.readyState = 1;
                    if (ws.onopen) ws.onopen({{ type: "open" }});
                }} else if (type === "message") {{
                    if (ws.onmessage) ws.onmessage({{ data: data, type: "message" }});
                }} else if (type === "error") {{
                    if (ws.onerror) ws.onerror({{ type: "error", message: data }});
                }} else if (type === "close") {{
                    ws.readyState = 3;
                    if (ws.onclose) ws.onclose({{ type: "close" }});
                    delete globalThis.__pwb_wsObjects[id];
                }}
            }};
            "##,
            title = js_escape(&initial_title),
        );

        let _: rquickjs::Value = ctx
            .eval(doc_wrapper)
            .map_err(|e| JsError::ExecutionFailed(format!("document wrapper failed: {}", e)))?;

        // 4. Execute bundled user script. Wrap it so uncaught exceptions are routed to
        //    console.error (message + stack) and re-thrown with full detail.
        let wrapped = format!(
            "(function() {{\n  try {{\n{script}\n  }} catch (e) {{\n    console.error('Uncaught: ' + (e && e.stack ? e.stack : String(e)));\n    throw e;\n  }}\n}})();"
        );
        match ctx.eval::<rquickjs::Value, _>(wrapped).catch(&ctx) {
            Ok(_) => Ok(()),
            Err(caught) => {
                let detail = caught.to_string();
                append_console_line(dom_handle, "error", &detail);
                Err(JsError::ExecutionFailed(detail))
            }
        }
    })
}

/// Fire due `setTimeout`/`setInterval` callbacks (called from the event loop's
/// `about_to_wait`). Returns whether any callback ran — the caller requests a
/// redraw, since the callback may have mutated the DOM.
pub fn pump_timers(dom_handle: &DomHandle) -> bool {
    let due = webapi::pop_due_timers();
    if due.is_empty() {
        return false;
    }
    let context = match get_or_create_context() {
        Ok(c) => c,
        Err(_) => return false,
    };
    context.with(|ctx| {
        // The callbacks live in the JS-side `__pwb_timers` registry (a global
        // object that persists across `execute_script` calls).
        let registry: Option<Object> = ctx.globals().get("__pwb_timers").ok();
        for (id, interval_ms) in due {
            if let Some(ms) = interval_ms {
                // Re-arm intervals BEFORE firing, so a `clearInterval()` inside
                // the callback removes the freshly re-armed entry.
                webapi::reschedule_timer(id, ms);
            }
            if let Some(reg) = &registry {
                let callback: Option<Function> = reg.get(id.to_string()).ok();
                if let Some(callback) = callback {
                    let _: rquickjs::Result<()> = callback.call(());
                }
                if interval_ms.is_none() {
                    // One-shot: drop the callback from the registry.
                    let _ = reg.remove(id.to_string());
                }
            }
        }
        // Run any promise reactions the callbacks queued.
        while ctx.execute_pending_job() {}
    });
    let _ = dom_handle;
    true
}

/// Resolve completed async operations (fetch responses, WebSocket events) into
/// their JS promises / callbacks (called from the event loop's `about_to_wait`).
/// Returns whether anything was dispatched — the caller redraws.
pub fn poll_async_ops(dom_handle: &DomHandle) -> bool {
    let events = webapi::drain_async(&dom_handle.web);
    if events.is_empty() {
        return false;
    }
    let context = match get_or_create_context() {
        Ok(c) => c,
        Err(_) => return false,
    };
    context.with(|ctx| {
        let globals = ctx.globals();
        let ws_dispatch: Option<Function> = globals.get("__pwb_wsOnEvent").ok();
        // The promise resolvers live in the JS-side `__pwb_pending` registry.
        let pending: Option<Object> = globals.get("__pwb_pending").ok();
        for event in events {
            match event {
                webapi::AsyncEvent::Fetch { id, result } => {
                    let op: Option<Object> = pending
                        .as_ref()
                        .and_then(|p| p.get::<_, Object>(id.to_string()).ok());
                    if let Some(op) = op {
                        let resolve: Option<Function> = op.get("resolve").ok();
                        let reject: Option<Function> = op.get("reject").ok();
                        match result {
                            Ok(resp) => {
                                let obj = Object::new(ctx.clone()).unwrap();
                                let _ = obj.set("status", resp.status);
                                let _ = obj.set("statusText", &resp.status_text);
                                let _ = obj.set("ok", resp.status >= 200 && resp.status < 300);
                                let _ = obj.set("url", &resp.url);
                                let headers = Object::new(ctx.clone()).unwrap();
                                for (k, v) in &resp.headers {
                                    let _ = headers.set(k.clone(), v.clone());
                                }
                                let _ = obj.set("_headers", headers);
                                let _ = obj
                                    .set("_body", String::from_utf8_lossy(&resp.body).to_string());
                                if let Some(resolve) = resolve {
                                    let _: rquickjs::Result<()> = resolve.call((obj,));
                                }
                            }
                            Err(msg) => {
                                if let Some(reject) = reject {
                                    let _: rquickjs::Result<()> = reject.call((msg,));
                                }
                            }
                        }
                        if let Some(p) = &pending {
                            let _ = p.remove(id.to_string());
                        }
                    }
                }
                webapi::AsyncEvent::Ws { id, event } => {
                    if let Some(f) = &ws_dispatch {
                        let (kind, data): (String, String) = match &event {
                            webapi::WsEvent::Open => ("open".to_string(), String::new()),
                            webapi::WsEvent::Message(d) => ("message".to_string(), d.clone()),
                            webapi::WsEvent::Error(e) => ("error".to_string(), e.clone()),
                            webapi::WsEvent::Close => ("close".to_string(), String::new()),
                        };
                        let _: rquickjs::Result<()> = f.call((id, kind, data));
                    }
                }
            }
        }
        while ctx.execute_pending_job() {}
    });
    let _ = dom_handle;
    true
}

/// Execute a registered event handler with `this` bound to the target element.
///
/// Handlers may be stored either as a function source (`function() { ... }`) or as a
/// plain statement list; both are wrapped so `this` resolves to the element proxy
/// returned by `document.selector`.
pub fn execute_event_handler(
    dom_handle: &DomHandle,
    node_id: usize,
    source: &str,
) -> Result<(), JsError> {
    execute_event_handler_typed(dom_handle, node_id, source, None)
}

/// Like [`execute_event_handler`], but passes a DOM `event` object as the first
/// argument (used for `contextmenu` etc.). The event has a working
/// `preventDefault()` that flips `suppress_context_menu`, which the pipeline
/// reads to suppress the native OS menu.
pub fn execute_event_handler_typed(
    dom_handle: &DomHandle,
    node_id: usize,
    source: &str,
    event_type: Option<&str>,
) -> Result<(), JsError> {
    // NOTE: the caller resets `suppress_context_menu` once before dispatching a
    // batch of handlers, so any single `preventDefault()` call wins even when
    // several handlers run for the same event.
    let trimmed = source.trim();
    let script = match event_type {
        Some(evt) => {
            let prelude = format!(
                "var el = document.selector('#{node_id}');\n\
                 var __evt = {{ type: '{evt}', defaultPrevented: false, preventDefault: function() {{ this.defaultPrevented = true; __pwb_suppressContextMenu(true); }} }};\n\
                 var event = __evt;"
            );
            if trimmed.starts_with("function") || trimmed.starts_with("async function") {
                format!("{prelude}\n({trimmed}).call(el, __evt);")
            } else {
                format!("{prelude}\n(function(evt) {{ {trimmed} }}).call(el, __evt);")
            }
        }
        None => {
            if trimmed.starts_with("function") || trimmed.starts_with("async function") {
                format!("var el = document.selector('#{node_id}');\n({trimmed}).call(el);")
            } else {
                format!("var el = document.selector('#{node_id}');\n(function() {{ {trimmed} }}).call(el);")
            }
        }
    };
    execute_script(dom_handle, &script)
}

/// Log a message to the devtools console panel (and stdout). Used by the pipeline
/// for non-JS events such as element inspection.
pub fn log_console(dom_handle: &DomHandle, msg: &str) {
    println!("[QuickJS Console] {}", msg);
    append_console_line(dom_handle, "log", msg);
}

/// Append a message line to the devtools `#console-output` panel if it is
/// mounted. Lines are `<code>` nodes, so the renderer syntax-highlights them
/// like a browser console (strings orange, keywords blue, numbers green — with
/// the level color as the base for identifiers). The output panel is flagged to
/// auto-scroll to the newest line.
fn append_console_line(dom_handle: &DomHandle, level: &str, msg: &str) {
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

/// A colored `Text` node used for syntax-highlighted devtools tree rows.
fn text_child(id: usize, text: String, color: &str) -> DomNode {
    text_child_sized(id, text, color, "1.8vh")
}

/// Like [`text_child`] but with an explicit font size (styles panel uses 2vh).
fn text_child_sized(id: usize, text: String, color: &str, font_size: &str) -> DomNode {
    let mut attrs = HashMap::new();
    attrs.insert("color".to_string(), color.to_string());
    attrs.insert("font-size".to_string(), font_size.to_string());
    DomNode {
        id,
        tag: Tag::Text,
        attributes: attrs,
        text: Some(text),
        children: Vec::new(),
    }
}

/// One syntax-highlighted Elements row: a flex-row View whose `id` is
/// `tree-row-{node_id}` (so picking an element can highlight its row), with
/// colored `<tag`, `#id`, `> text`, `</tag>` parts. Used by the console's
/// `__pwb_appendTreeRow` binding (a leaf-style row without width budgeting).
fn make_tree_row(
    row_id: &str,
    next_id: &mut usize,
    indent: &str,
    tag: &str,
    id: &str,
    text: &str,
) -> DomNode {
    let mut attrs = HashMap::new();
    attrs.insert("id".to_string(), row_id.to_string());
    attrs.insert("flex-direction".to_string(), "row".to_string());
    attrs.insert("gap".to_string(), "0".to_string());
    attrs.insert("padding".to_string(), "0.33vh".to_string());
    let mut row_children = Vec::new();
    // Opening tag with closing `>` baked in: `<tag>` or `<tag #id>`.
    let open = if id.is_empty() {
        format!("{}<{}>", indent, tag)
    } else {
        format!("{}<{} #{}>", indent, tag, id)
    };
    row_children.push(text_child(*next_id, open, "#569cd6"));
    *next_id += 1;
    if !text.is_empty() {
        // One space before content, matching Chrome DevTools.
        row_children.push(text_child(*next_id, format!(" {}", text), "#d4d4d4"));
        *next_id += 1;
    }
    row_children.push(text_child(*next_id, format!("</{}>", tag), "#569cd6"));
    *next_id += 1;
    // Same unique-id rule as `tree_row_container`: take the current slot for
    // the container and advance, so it never collides with another node.
    let container_id = *next_id;
    *next_id += 1;
    DomNode {
        id: container_id,
        tag: Tag::View,
        attributes: attrs,
        text: None,
        children: row_children,
    }
}

/// Shared row scaffolding: a flex-row View with the given text children.
/// `row_id` becomes the row's `id` (empty for closing-tag rows).
fn tree_row_container(
    row_id: &str,
    data_node: Option<&str>,
    next_id: &mut usize,
    children: Vec<DomNode>,
) -> DomNode {
    let mut attrs = HashMap::new();
    if !row_id.is_empty() {
        attrs.insert("id".to_string(), row_id.to_string());
    }
    // Every row — opening, closing, and leaf alike — carries the original DOM
    // node it represents, so the pipeline can select the element when any part
    // of the row (including the `</tag>` line) is clicked.
    if let Some(node_id) = data_node {
        attrs.insert("data-node".to_string(), node_id.to_string());
    }
    attrs.insert("flex-direction".to_string(), "row".to_string());
    attrs.insert("gap".to_string(), "0".to_string());
    attrs.insert("padding".to_string(), "0.33vh".to_string());
    // Give the container a node id from the SAME monotonic counter as its text
    // children and advance past it, so sibling rows never share ids. (The old
    // `id: *next_id + 1` without advancing left the container's id equal to the
    // next row's first text child; the pipeline resolves a clicked row's
    // `data-node` by walking the DOM for the hit node's id, so a duplicate id
    // made clicking a row's text select the wrong element — e.g. clicking
    // `<input #demo-input>` selected the `<icon-github>` row above it.)
    let container_id = *next_id;
    *next_id += 1;
    DomNode {
        id: container_id,
        tag: Tag::View,
        attributes: attrs,
        text: None,
        children,
    }
}

/// Opening-tag row for a container element: `<tag` + optional `#id`. The
/// closing tag is its own row, emitted after the children (see
/// [`collect_dom_tree_lines`]), so containers read like a browser's tree:
/// `<head>` on one row, children below, `</head>` after them.
fn make_open_row(
    row_id: &str,
    data_node: &str,
    next_id: &mut usize,
    indent: &str,
    tag: &str,
    id: &str,
) -> DomNode {
    let mut children = Vec::new();
    // Opening tag with closing `>` baked in: `<tag>` or `<tag #id>`.
    let open = if id.is_empty() {
        format!("{}<{}>", indent, tag)
    } else {
        format!("{}<{} #{}>", indent, tag, id)
    };
    children.push(text_child(*next_id, open, "#569cd6"));
    *next_id += 1;
    tree_row_container(row_id, Some(data_node), next_id, children)
}

/// Closing-tag row: `</tag>` alone on its own line, indented like its opening
/// row, placed after the element's children. Carries `data-node` (but no
/// `tree-row-*` id) so clicking it selects the same element as its opening row.
fn make_close_row(data_node: &str, next_id: &mut usize, indent: &str, tag: &str) -> DomNode {
    let mut children = Vec::new();
    children.push(text_child(
        *next_id,
        format!("{}</{}>", indent, tag),
        "#569cd6",
    ));
    *next_id += 1;
    tree_row_container("", Some(data_node), next_id, children)
}

/// Leaf row: `<tag` `#id` `> text` `</tag>` on one line. The content text is
/// width-truncated with a `…` ellipsis so the closing tag always stays visible
/// within `budget_px` (the tree's content width minus the scrollbar).
#[allow(clippy::too_many_arguments)]
fn make_leaf_row(
    row_id: &str,
    next_id: &mut usize,
    indent: &str,
    tag: &str,
    id: &str,
    text: &str,
    budget_px: f32,
    vh: f32,
) -> DomNode {
    let font_px = 0.018 * vh; // 1.8vh tree font
                              // Opening tag with `>` baked in: `<tag>` or `<tag #id>`.
    let open_txt = if id.is_empty() {
        format!("{}<{}>", indent, tag)
    } else {
        format!("{}<{} #{}>", indent, tag, id)
    };
    let close_txt = format!("</{}>", tag);
    // Fixed parts: `<tag>` and `</tag>` — no gap padding.
    let fixed_w = crate::layout::measure_text(&open_txt, font_px)
        + crate::layout::measure_text(&close_txt, font_px);
    let content_avail = (budget_px - fixed_w).max(12.0);
    let content = fit_text_width(text, content_avail, font_px);

    let mut children = Vec::new();
    children.push(text_child(*next_id, open_txt, "#569cd6"));
    *next_id += 1;
    if !content.is_empty() {
        // One space before content, matching Chrome DevTools.
        children.push(text_child(*next_id, format!(" {}", content), "#d4d4d4"));
        *next_id += 1;
    }
    children.push(text_child(*next_id, close_txt, "#569cd6"));
    *next_id += 1;
    let data_node = row_id.strip_prefix("tree-row-").unwrap_or("");
    tree_row_container(row_id, Some(data_node), next_id, children)
}

/// Shorten `text` (with a trailing `…`) so it fits within `avail_px` at
/// `font_px`, matching how the row is drawn. Returns the text unchanged when
/// it already fits. (The opening tag includes `>`, so only the content text
/// is budgeted here.)
fn fit_text_width(text: &str, avail_px: f32, font_px: f32) -> String {
    if text.is_empty() {
        return String::new();
    }
    if crate::layout::measure_text(text, font_px) <= avail_px {
        return text.to_string();
    }
    let mut out: String = text.chars().collect();
    while out.chars().count() > 1 {
        out.pop();
        if crate::layout::measure_text(&format!("{}…", out), font_px) <= avail_px {
            return format!("{}…", out);
        }
    }
    "…".to_string()
}

/// Rebuild the Elements tree from the live DOM (the devtools subtree is
/// excluded), re-applying the selected row's highlight. Fresh rows replace the
/// previous ones entirely, so node ids grow monotonically but the
/// `tree-row-{node_id}` keys stay stable. `vw`/`vh` are the real window
/// dimensions (physical px), used to budget leaf-row content so closing tags
/// never overflow the tree.
fn rebuild_tree_rows(dom: &mut Dom, selected: Option<usize>, vw: f32, vh: f32) {
    let Some(tree_id) = find_node_by_selector(&dom.root, "#elements-tree").map(|n| n.id) else {
        return;
    };
    let mut lines = String::new();
    collect_dom_tree_lines(&dom.root, 0, &mut lines);

    // Truncation budget: the sidebar's content width minus the tab-content,
    // tree and row paddings and the scrollbar track — i.e. the visible width
    // available for one row's text children.
    let sidebar_w = find_node_by_selector(&dom.root, ".devtools-sidebar")
        .and_then(|n| n.attributes.get("width"))
        .and_then(|w| resolve_style_width(w, vw))
        .unwrap_or(0.25 * vw);
    let pad = |v: f32| v * vh;
    let budget = (sidebar_w - pad(0.013) * 2.0 - pad(0.0033) * 4.0 - 12.0).max(60.0);

    // Only the selected element is highlighted — its opening AND closing rows
    // (and the single text row when it's a leaf) — but never its child
    // elements, matching a browser inspector's single-element selection.
    let selected_ids: std::collections::HashSet<usize> = selected.into_iter().collect();

    let mut next_id = max_node_id(&dom.root) + 1;
    let mut rows = Vec::new();
    for line in lines.lines() {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 6 || parts[0].is_empty() {
            continue;
        }
        let node_id = parts[0];
        let node_id_num: usize = node_id.parse().unwrap_or(0);
        let depth: usize = parts[1].parse().unwrap_or(0);
        let indent = "  ".repeat(depth);
        let tag = parts[2];
        let id = parts.get(3).copied().unwrap_or("");
        let text = parts.get(4).copied().unwrap_or("");
        let kind = parts[5];
        let (mut row, _is_node_row) = match kind {
            "c" => (make_close_row(node_id, &mut next_id, &indent, tag), false),
            "l" => (
                make_leaf_row(
                    &format!("tree-row-{}", node_id),
                    &mut next_id,
                    &indent,
                    tag,
                    id,
                    text,
                    budget,
                    vh,
                ),
                true,
            ),
            _ => (
                make_open_row(
                    &format!("tree-row-{}", node_id),
                    node_id,
                    &mut next_id,
                    &indent,
                    tag,
                    id,
                ),
                true,
            ),
        };
        if selected_ids.contains(&node_id_num) {
            row.attributes
                .insert("background-color".to_string(), "#094771".to_string());
        }
        rows.push(row);
    }
    if let Some(tree) = find_node_by_id_mut(&mut dom.root, &tree_id.to_string()) {
        tree.children = rows;
    }
}

/// Resolve a style `width` attribute (`25vw` or `450px`) into physical pixels
/// for the given viewport width. `vw` values are percentages of the viewport
/// (the style engine stores `25vw` as the fraction 25), matching the layout's
/// `UnitValue::Vw` resolution.
fn resolve_style_width(width: &str, vw: f32) -> Option<f32> {
    let w = width.trim();
    if let Some(rest) = w.strip_suffix("vw") {
        rest.trim().parse::<f32>().ok().map(|f| (f / 100.0) * vw)
    } else if let Some(rest) = w.strip_suffix("px") {
        rest.trim().parse::<f32>().ok()
    } else {
        w.parse::<f32>().ok()
    }
}

/// Render the bottom styles panel: the selected element's attributes (minus
/// structural `id`/`class`/`data-*`) as `prop: value` rows, or a hint when
/// nothing is selected.
fn render_styles_panel(dom: &mut Dom, selected: Option<usize>) {
    let Some(panel_id) = find_node_by_selector(&dom.root, "#styles-panel").map(|n| n.id) else {
        return;
    };
    let attrs: Vec<(String, String)> =
        match selected.and_then(|id| find_node_by_id(&dom.root, &id.to_string())) {
            Some(node) => {
                let mut a: Vec<(String, String)> = node
                    .attributes
                    .iter()
                    .filter(|(k, _)| {
                        k.as_str() != "id" && k.as_str() != "class" && !k.starts_with("data-")
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                a.sort();
                a
            }
            None => Vec::new(),
        };

    let mut next_id = max_node_id(&dom.root) + 1;
    let mut children = Vec::new();
    children.push(text_child_sized(
        next_id,
        "Styles".to_string(),
        "#cccccc",
        "2vh",
    ));
    next_id += 1;
    if attrs.is_empty() {
        let hint = if selected.is_some() {
            "No styles".to_string()
        } else {
            "Select an element to inspect its styles.".to_string()
        };
        children.push(text_child_sized(next_id, hint, "#6a9955", "2vh"));
    } else {
        for (k, v) in attrs {
            let mut row_attrs = HashMap::new();
            row_attrs.insert("flex-direction".to_string(), "row".to_string());
            row_attrs.insert("gap".to_string(), "0.5vh".to_string());
            row_attrs.insert("padding".to_string(), "0.2vh".to_string());
            children.push(DomNode {
                id: next_id,
                tag: Tag::View,
                attributes: row_attrs,
                text: None,
                children: vec![
                    text_child_sized(next_id + 1, k, "#9cdcfe", "2vh"),
                    text_child_sized(next_id + 2, format!(": {}", v), "#d4d4d4", "2vh"),
                ],
            });
            next_id += 3;
        }
    }
    if let Some(panel) = find_node_by_id_mut(&mut dom.root, &panel_id.to_string()) {
        panel.children = children;
    }
}

/// Walk a DOM subtree (skipping the mounted devtools root) and emit one
/// pipe-separated line per tree row: `node_id|depth|tag|id|text|kind`, where
/// `kind` is `o` (opening-tag row), `c` (closing-tag row, emitted after the
/// children) or `l` (a single leaf row carrying the element's text).
fn collect_dom_tree_lines(node: &DomNode, depth: usize, out: &mut String) {
    if node.attributes.contains_key("data-devtools") {
        return;
    }
    let id_attr = node.attributes.get("id").cloned().unwrap_or_default();
    // Icons show as `<icon-home>` so the tree reflects the actual element.
    let tag_label = if node.tag == Tag::Icon {
        node.attributes
            .get("icon")
            .map(|n| format!("icon-{}", n))
            .unwrap_or_else(|| "icon".to_string())
    } else {
        node.tag.name().to_string()
    };
    if node.children.is_empty() {
        // `<input>` leaves show their live `value` (or their text) so the tree
        // reflects what is typed into them.
        let text_attr = node
            .text
            .as_ref()
            .map(|t| t.trim().chars().take(40).collect::<String>())
            .or_else(|| {
                node.attributes
                    .get("value")
                    .map(|v| v.chars().take(40).collect())
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "{}|{}|{}|{}|{}|l\n",
            node.id, depth, tag_label, id_attr, text_attr
        ));
    } else {
        out.push_str(&format!(
            "{}|{}|{}|{}||o\n",
            node.id, depth, tag_label, id_attr
        ));
        for child in &node.children {
            collect_dom_tree_lines(child, depth + 1, out);
        }
        out.push_str(&format!("{}|{}|{}|||c\n", node.id, depth, tag_label));
    }
}

/// Parse a tiny JSON object (`{"k": "v", ...}`) into string params. Values
/// are unquoted; a bare `{}`/empty string yields an empty map.
fn parse_props_json(props_json: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let trimmed = props_json.trim();
    let body = trimmed
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(trimmed);
    for part in body.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once(':') {
            let key = k.trim().trim_matches('"').trim_matches('\'').to_string();
            let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !key.is_empty() {
                out.insert(key, val);
            }
        }
    }
    out
}

/// Replace `{param}` placeholders in a DOM subtree's attributes and text.
fn substitute_params_dom(node: &mut DomNode, params: &HashMap<String, String>) {
    for v in node.attributes.values_mut() {
        for (key, value) in params {
            *v = v.replace(&format!("{{{}}}", key), value);
        }
    }
    if let Some(text) = &mut node.text {
        for (key, value) in params {
            *text = text.replace(&format!("{{{}}}", key), value);
        }
    }
    for child in &mut node.children {
        substitute_params_dom(child, params);
    }
}

/// Reassign node ids in a subtree starting at `base` (document order).
fn reassign_ids(node: &mut DomNode, base: usize) -> usize {
    let mut next = base;
    fn walk(node: &mut DomNode, next: &mut usize) {
        node.id = *next;
        *next += 1;
        for child in &mut node.children {
            walk(child, next);
        }
    }
    walk(node, &mut next);
    next - 1
}

fn max_node_id(node: &DomNode) -> usize {
    let mut max = node.id;
    for child in &node.children {
        max = max.max(max_node_id(child));
    }
    max
}

/// Apply the selection highlight to the row for `id` and every row of its
/// descendant elements (the element's contents). Missing rows (e.g. text nodes,
/// which have no rows of their own) are skipped.
fn highlight_selected_node(dom: &mut Dom, id: usize) {
    // Highlight exactly one element in the tree: its opening row, its closing
    // row (multi-line elements), and its single text row (leaf). Every other
    // row is un-highlighted so picking a new element deselects the previous
    // selection. Rows are matched by `data-node` — the attribute every row
    // (opening, closing, and leaf) carries for the element it represents.
    if let Some(sel) = find_node_by_selector_mut(&mut dom.root, "#elements-tree") {
        for row in &mut sel.children {
            let is_selected = row
                .attributes
                .get("data-node")
                .and_then(|v| v.parse::<usize>().ok())
                == Some(id);
            if is_selected {
                row.attributes
                    .insert("background-color".to_string(), "#094771".to_string());
            } else {
                row.attributes.remove("background-color");
            }
        }
    }
}

/// Escape a string for safe interpolation into a JS string literal.
fn js_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "")
}

fn find_node_by_id<'a>(node: &'a DomNode, id: &str) -> Option<&'a DomNode> {
    if node.id.to_string() == id || node.attributes.get("id").map(|v| v == id).unwrap_or(false) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_by_id(child, id) {
            return Some(found);
        }
    }
    None
}

fn find_node_by_selector<'a>(node: &'a DomNode, selector: &str) -> Option<&'a DomNode> {
    // Attribute selector: [attr] or [attr=value]
    if selector.starts_with('[') && selector.ends_with(']') {
        let inner = &selector[1..selector.len() - 1];
        let (attr, expected) = inner
            .split_once('=')
            .map(|(a, v)| {
                (
                    a.trim(),
                    Some(v.trim().trim_matches('"').trim_matches('\'')),
                )
            })
            .unwrap_or((inner, None));
        let matches = match node.attributes.get(attr) {
            Some(v) => expected.map(|e| v == e).unwrap_or(true),
            None => false,
        };
        if matches {
            return Some(node);
        }
    }
    if selector.starts_with('#') {
        // ID selector: matches the `id` attribute or the numeric node id.
        return find_node_by_id(node, selector.strip_prefix('#').unwrap_or(selector));
    }
    if selector.starts_with('.') {
        // Class selector - check attributes
        if let Some(class_attr) = node.attributes.get("class") {
            if class_attr.split_whitespace().any(|c| c == &selector[1..]) {
                return Some(node);
            }
        }
    }
    // Tag selector or plain numeric node id.
    if node.tag.name() == selector || node.id.to_string() == selector {
        return Some(node);
    }
    // Check children
    for child in &node.children {
        if let Some(found) = find_node_by_selector(child, selector) {
            return Some(found);
        }
    }
    None
}

fn find_node_by_selector_mut<'a>(node: &'a mut DomNode, selector: &str) -> Option<&'a mut DomNode> {
    if selector.starts_with('[') && selector.ends_with(']') {
        let inner = &selector[1..selector.len() - 1];
        let (attr, expected) = inner
            .split_once('=')
            .map(|(a, v)| {
                (
                    a.trim(),
                    Some(v.trim().trim_matches('"').trim_matches('\'')),
                )
            })
            .unwrap_or((inner, None));
        let matches = match node.attributes.get(attr) {
            Some(v) => expected.map(|e| v == e).unwrap_or(true),
            None => false,
        };
        if matches {
            return Some(node);
        }
    }

    let id = selector.strip_prefix('#').unwrap_or(selector);

    if selector.starts_with('#') {
        return find_node_by_id_mut(node, id);
    }

    if selector.starts_with('.') {
        if let Some(class_attr) = node.attributes.get("class") {
            if class_attr.split_whitespace().any(|c| c == &selector[1..]) {
                return Some(node);
            }
        }
    }

    if node.tag.name() == selector || node.id.to_string() == selector {
        return Some(node);
    }

    for child in node.children.iter_mut() {
        if let Some(found) = find_node_by_selector_mut(child, selector) {
            return Some(found);
        }
    }
    None
}

fn find_node_by_id_mut<'a>(node: &'a mut DomNode, id: &str) -> Option<&'a mut DomNode> {
    if node.id.to_string() == id || node.attributes.get("id").map(|v| v == id).unwrap_or(false) {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_node_by_id_mut(child, id) {
            return Some(found);
        }
    }
    None
}

/// Remove the node with `target_id` from the tree (searching every level) and
/// return it, so it can be kept alive as a detached node (`el.remove()`).
fn remove_node_from_tree(node: &mut DomNode, target_id: usize) -> Option<DomNode> {
    if let Some(pos) = node.children.iter().position(|c| c.id == target_id) {
        return Some(node.children.remove(pos));
    }
    for child in &mut node.children {
        if let Some(found) = remove_node_from_tree(child, target_id) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::DomNode;
    use pwb_core::spec::Tag;
    use std::collections::HashMap;

    fn test_handle() -> DomHandle {
        let dom = Dom {
            title: "Probe".to_string(),
            components: HashMap::new(),
            modules: HashMap::new(),
            root: DomNode {
                id: 1,
                tag: Tag::View,
                attributes: HashMap::new(),
                text: None,
                children: vec![
                    DomNode {
                        id: 2,
                        tag: Tag::Button,
                        attributes: HashMap::from([("id".to_string(), "main-btn".to_string())]),
                        text: Some("Click me".to_string()),
                        children: vec![],
                    },
                    DomNode {
                        id: 3,
                        tag: Tag::View,
                        attributes: HashMap::from([
                            ("class".to_string(), "devtools-sidebar".to_string()),
                            ("data-devtools".to_string(), "root".to_string()),
                        ]),
                        text: None,
                        children: vec![
                            DomNode {
                                id: 4,
                                tag: Tag::View,
                                attributes: HashMap::from([(
                                    "id".to_string(),
                                    "elements-tree".to_string(),
                                )]),
                                text: None,
                                children: vec![],
                            },
                            DomNode {
                                id: 5,
                                tag: Tag::View,
                                attributes: HashMap::from([(
                                    "id".to_string(),
                                    "styles-panel".to_string(),
                                )]),
                                text: None,
                                children: vec![],
                            },
                            DomNode {
                                id: 6,
                                tag: Tag::View,
                                attributes: HashMap::from([(
                                    "id".to_string(),
                                    "console-output".to_string(),
                                )]),
                                text: None,
                                children: vec![],
                            },
                        ],
                    },
                ],
            },
        };
        DomHandle::new(dom)
    }

    #[test]
    fn test_find_node_by_id_attr() {
        let handle = test_handle();
        let dom = handle.dom.borrow();
        assert!(find_node_by_id(&dom.root, "main-btn").is_some());
        assert!(find_node_by_id(&dom.root, "2").is_some());
        assert!(find_node_by_id(&dom.root, "99").is_none());
    }

    #[test]
    fn test_find_node_by_selector() {
        let handle = test_handle();
        let dom = handle.dom.borrow();
        assert!(find_node_by_selector(&dom.root, "#main-btn").is_some());
        assert!(find_node_by_selector(&dom.root, ".devtools-sidebar").is_some());
        assert!(find_node_by_selector(&dom.root, "[data-devtools]").is_some());
        assert!(find_node_by_selector(&dom.root, "button").is_some());
        assert!(find_node_by_selector(&dom.root, "2").is_some());
    }

    #[test]
    fn test_register_and_dispatch_string_handler() {
        let handle = test_handle();
        let script = r##"
            var btn = document.getElementById("main-btn");
            if (!btn) throw new Error("button not found");
            btn.onClick("this.setText('Clicked!');");
        "##;
        execute_script(&handle, script).expect("script should execute");

        let handlers = handle.event_registry.get_handlers(2, "click");
        assert_eq!(handlers.len(), 1);

        // Dispatch the handler: text should change via `this`-bound setText.
        execute_event_handler(&handle, 2, &handlers[0]).expect("handler should execute");
        let dom = handle.dom.borrow();
        let btn = find_node_by_id(&dom.root, "main-btn").unwrap();
        assert_eq!(btn.text.as_deref(), Some("Clicked!"));
    }

    #[test]
    fn test_register_function_handler_with_this() {
        let handle = test_handle();
        let script = r##"
            var btn = document.getElementById("main-btn");
            btn.onClick(function() {
                var name = this.id;
                this.setText("handled:" + name);
            });
        "##;
        // Function-valued handlers used to crash script execution; now they register fine.
        execute_script(&handle, script).expect("script should execute");

        let handlers = handle.event_registry.get_handlers(2, "click");
        assert_eq!(handlers.len(), 1);

        execute_event_handler(&handle, 2, &handlers[0]).expect("handler should execute");
        let dom = handle.dom.borrow();
        let btn = find_node_by_id(&dom.root, "main-btn").unwrap();
        assert_eq!(btn.text.as_deref(), Some("handled:main-btn"));
    }

    #[test]
    fn test_component_style_module_bindings() {
        // A handle with a component registry, a style rule, and a named module.
        let handle = test_handle();
        {
            let mut dom = handle.dom.borrow_mut();
            dom.components.insert(
                "badge".to_string(),
                DomNode {
                    id: 900,
                    tag: Tag::View,
                    attributes: HashMap::from([("class".to_string(), "badge".to_string())]),
                    text: None,
                    children: vec![DomNode {
                        id: 901,
                        tag: Tag::Text,
                        attributes: HashMap::new(),
                        text: Some("{label}".to_string()),
                        children: vec![],
                    }],
                },
            );
            dom.modules
                .insert("lib.js".to_string(), b"var dyn = 7;".to_vec());
        }

        let script = r##"
            var el = pwb.createComponent("badge", {label: "Hello"});
            if (!el) throw new Error("createComponent returned null");
            pwb.loadStyle(".badge { background-color: #112233; }");
            if (!pwb.import("lib.js")) throw new Error("import failed");
            if (typeof dyn !== "number") throw new Error("imported module did not run");
        "##;
        execute_script(&handle, script).expect("bindings should work");

        let dom = handle.dom.borrow();
        // The component was appended to the page root with the {label} param
        // substituted into its text child.
        let root = &dom.root;
        let instance = root
            .children
            .iter()
            .find(|n| n.attributes.get("class").map(|c| c.as_str()) == Some("badge"))
            .expect("component instance appended");
        assert_eq!(
            instance.children[0].text.as_deref(),
            Some("Hello"),
            "the label param must be substituted"
        );
        // loadStyle applied the rule's attribute to the instance.
        assert_eq!(
            instance
                .attributes
                .get("background-color")
                .map(|s| s.as_str()),
            Some("#112233")
        );
    }

    #[test]
    fn test_contextmenu_event_prevent_default() {
        let handle = test_handle();
        let script = r##"
            var btn = document.getElementById("main-btn");
            btn.addEventHandler("contextmenu", function(evt) {
                evt.preventDefault();
                this.setText("menu suppressed");
            });
        "##;
        execute_script(&handle, script).expect("script should execute");

        let handlers = handle.event_registry.get_handlers(2, "contextmenu");
        assert_eq!(handlers.len(), 1);

        assert!(!handle.suppress_context_menu.load(Ordering::Relaxed));
        execute_event_handler_typed(&handle, 2, &handlers[0], Some("contextmenu"))
            .expect("handler should execute");

        // `preventDefault()` flipped the flag the pipeline reads to skip the
        // native menu, and `this` was bound to the element.
        assert!(handle.suppress_context_menu.load(Ordering::Relaxed));
        let dom = handle.dom.borrow();
        let btn = find_node_by_id(&dom.root, "main-btn").unwrap();
        assert_eq!(btn.text.as_deref(), Some("menu suppressed"));
    }

    #[test]
    fn test_contextmenu_any_handler_prevent_default_suppresses() {
        // Two handlers for the same event; the second calls preventDefault().
        // The caller resets the flag once before dispatching, so the suppression
        // must stick across the batch (previously each handler reset it).
        let handle = test_handle();
        let script = r##"
            var btn = document.getElementById("main-btn");
            btn.addEventHandler("contextmenu", function(evt) { this.setText("first"); });
            btn.addEventHandler("contextmenu", function(evt) { evt.preventDefault(); this.setText("second"); });
        "##;
        execute_script(&handle, script).expect("script should execute");
        let handlers = handle.event_registry.get_handlers(2, "contextmenu");
        assert_eq!(handlers.len(), 2);

        // Reset once (as the pipeline does), then run all handlers.
        handle.suppress_context_menu.store(false, Ordering::Relaxed);
        for h in &handlers {
            execute_event_handler_typed(&handle, 2, h, Some("contextmenu")).expect("handler runs");
        }
        assert!(
            handle.suppress_context_menu.load(Ordering::Relaxed),
            "preventDefault from any handler must win"
        );
    }

    #[test]
    fn test_contextmenu_without_prevent_default() {
        let handle = test_handle();
        let script = r##"
            var btn = document.getElementById("main-btn");
            btn.addEventHandler("contextmenu", function(evt) {
                this.setText("no preventDefault");
            });
        "##;
        execute_script(&handle, script).expect("script should execute");
        let handlers = handle.event_registry.get_handlers(2, "contextmenu");
        execute_event_handler_typed(&handle, 2, &handlers[0], Some("contextmenu"))
            .expect("handler should execute");
        // No preventDefault call → the flag stays false → the OS menu shows.
        assert!(!handle.suppress_context_menu.load(Ordering::Relaxed));
    }

    #[test]
    fn test_js_error_reports_message() {
        let handle = test_handle();
        let err = execute_script(&handle, "throw new Error('boom');").unwrap_err();
        assert!(
            err.to_string().contains("boom"),
            "error should contain message: {:?}",
            err
        );
    }

    #[test]
    fn test_devtools_tab_switching_end_to_end() {
        // Integration: mount the real devtools directory like the pipeline, run its
        // script, then dispatch a tab click and verify the display attributes change.
        let devtools_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
        let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
        let mut devtools_dom = Dom::from_bundle(&bundle);
        devtools_dom
            .root
            .attributes
            .insert("data-devtools".to_string(), "root".to_string());

        let main_root = DomNode {
            id: 1,
            tag: Tag::View,
            attributes: HashMap::new(),
            text: None,
            children: vec![],
        };
        let mut main = Dom {
            title: "Demo".to_string(),
            components: HashMap::new(),
            modules: HashMap::new(),
            root: main_root,
        };
        main.add_child(devtools_dom.root);
        let handle = DomHandle::new(main);

        // Run the devtools app.pjs (previously crashed on the first function handler).
        let script = std::str::from_utf8(bundle.script_payload.as_deref().unwrap()).unwrap();
        execute_script(&handle, script).expect("devtools script should run without crashing");

        // Find the console tab button and its content panel.
        let console_tab_id = {
            let dom = handle.dom.borrow();
            find_node_by_id(&dom.root, "tab-console")
                .expect("console tab button exists")
                .id
        };
        let get_display = |handle: &DomHandle, id: &str| -> Option<String> {
            let dom = handle.dom.borrow();
            find_node_by_id(&dom.root, id)
                .unwrap()
                .attributes
                .get("display")
                .cloned()
        };

        // The elements tab starts active: `.tab-content.active { display: block }`.
        assert_eq!(
            get_display(&handle, "elements-tab").as_deref(),
            Some("block")
        );
        assert_eq!(get_display(&handle, "console-tab").as_deref(), Some("none"));

        // Dispatch a click on the console tab: this exercises the registered function
        // handler with `this` bound to the tab element.
        let handlers = handle.event_registry.get_handlers(console_tab_id, "click");
        assert_eq!(
            handlers.len(),
            1,
            "console tab should have a registered click handler"
        );
        execute_event_handler(&handle, console_tab_id, &handlers[0]).expect("tab click handler");

        assert_eq!(
            get_display(&handle, "elements-tab").as_deref(),
            Some("none"),
            "elements content hidden after switching"
        );
        assert_eq!(
            get_display(&handle, "console-tab").as_deref(),
            Some("block"),
            "console content shown after switching"
        );
        // The styles panel closes while inspecting the console.
        assert_eq!(
            get_display(&handle, "styles-panel").as_deref(),
            Some("none"),
            "styles panel hidden on console tab"
        );
        // The clicked tab button gets the active color.
        let (console_btn_color, elements_tab_id) = {
            let dom = handle.dom.borrow();
            let console_btn = find_node_by_id(&dom.root, "tab-console").unwrap();
            let color = console_btn
                .attributes
                .get("background-color")
                .map(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let elements_tab_id = find_node_by_id(&dom.root, "tab-elements").unwrap().id;
            (color, elements_tab_id)
        };
        assert_eq!(console_btn_color, "#0078d7");

        // Switching back to Elements reopens the styles panel.
        let handlers = handle.event_registry.get_handlers(elements_tab_id, "click");
        execute_event_handler(&handle, elements_tab_id, &handlers[0]).expect("elements tab click");
        assert_eq!(
            get_display(&handle, "styles-panel").as_deref(),
            Some("block"),
            "styles panel back on elements tab"
        );
    }

    #[test]
    fn test_select_element_fills_styles_panel_and_survives_refresh() {
        let handle = test_handle();
        // Give the main button a style attribute, then populate the tree.
        execute_script(
            &handle,
            "__pwb_setStyle('2', 'background-color', '#ff0000');",
        )
        .unwrap();
        execute_script(&handle, "__pwb_refreshDevtools();").unwrap();

        // Select the button (node id 2): its tree row highlights and the styles
        // panel fills with its attributes.
        execute_script(&handle, "__pwb_selectElement(2);").unwrap();
        assert_eq!(*handle.selected_node_id.borrow(), Some(2));
        {
            let dom = handle.dom.borrow();
            let row = find_node_by_id(&dom.root, "tree-row-2").expect("row exists");
            assert_eq!(
                row.attributes.get("background-color").map(|s| s.as_str()),
                Some("#094771")
            );
            let panel = find_node_by_id(&dom.root, "styles-panel").expect("styles panel");
            assert_eq!(panel.children.len(), 2, "title + one style row");
            assert_eq!(panel.children[0].text.as_deref(), Some("Styles"));
            let style_row = &panel.children[1];
            assert_eq!(style_row.tag, Tag::View);
            assert_eq!(
                style_row.children[0].text.as_deref(),
                Some("background-color")
            );
            assert_eq!(style_row.children[1].text.as_deref(), Some(": #ff0000"));
        }

        // A refresh (e.g. after a click handler) rebuilds the tree, keeps the
        // highlight, and reflects the new style value.
        execute_script(
            &handle,
            "__pwb_setStyle('2', 'background-color', '#00ff00');",
        )
        .unwrap();
        execute_script(&handle, "__pwb_refreshDevtools();").unwrap();
        {
            let dom = handle.dom.borrow();
            let row = find_node_by_id(&dom.root, "tree-row-2").expect("row still exists");
            assert_eq!(
                row.attributes.get("background-color").map(|s| s.as_str()),
                Some("#094771"),
                "highlight survives refresh"
            );
            let panel = find_node_by_id(&dom.root, "styles-panel").unwrap();
            let style_row = &panel.children[1];
            assert_eq!(
                style_row.children[1].text.as_deref(),
                Some(": #00ff00"),
                "style value auto-updated"
            );
        }
    }

    #[test]
    fn test_devtools_tree_open_close_structure() {
        // Mount real devtools + the real demo.pwb DOM and populate the tree.
        let devtools_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
        let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
        let mut devtools_dom = Dom::from_bundle(&bundle);
        devtools_dom
            .root
            .attributes
            .insert("data-devtools".to_string(), "root".to_string());
        let demo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/demo.pwb");
        let demo_bundle = crate::bundler::bundle_from_dir(&demo_dir).expect("demo bundle");
        let mut main = Dom::from_bundle(&demo_bundle);
        main.add_child(devtools_dom.root);
        let handle = DomHandle::new(main);
        // Real window size so the truncation budget matches runtime.
        execute_script(&handle, "__pwb_refreshDevtools(1800, 1300);").unwrap();

        // Collect the tree rows in document order: `(row_id, text_parts)`.
        let dom = handle.dom.borrow();
        let tree = find_node_by_id(&dom.root, "elements-tree").expect("elements tree");
        let rows: Vec<(Option<&str>, Option<&str>, Vec<&str>)> = tree
            .children
            .iter()
            .map(|r| {
                let id = r.attributes.get("id").map(|s| s.as_str());
                let data_node = r.attributes.get("data-node").map(|s| s.as_str());
                let parts: Vec<&str> = r
                    .children
                    .iter()
                    .map(|c| c.text.as_deref().unwrap_or(""))
                    .collect();
                (id, data_node, parts)
            })
            .collect();

        // Every row — opening, closing, and leaf — carries `data-node` pointing
        // at the original DOM node, so clicking any part of a row (including the
        // `</tag>` line) can select the element it represents.
        let title_open = rows
            .iter()
            .find(|(_, _, p)| p.contains(&"    <title>"))
            .expect("title open row");
        assert_eq!(title_open.1, Some("100003"), "leaf rows carry data-node");
        let head_close = rows
            .iter()
            .find(|(_, _, p)| p.contains(&"  </head>"))
            .expect("head close row");
        assert_eq!(
            head_close.1,
            Some("100002"),
            "closing rows carry data-node too (head node)"
        );

        // The demo's `head` is a container: its opening row comes first, then its
        // `<title>` child, then the `</head>` closing row — not inline.
        let title_row = rows
            .iter()
            .find(|(_, _, p)| p.contains(&"</title>"))
            .expect("title row");
        let title_idx = rows
            .iter()
            .position(|r| std::ptr::eq(r, title_row))
            .unwrap();
        assert_eq!(title_row.2[0], "    <title>", "opening tag with > baked in");
        assert!(
            title_row
                .2
                .iter()
                .any(|t| t.starts_with(" Untrackable") && t.ends_with('…')),
            "leaf row keeps (truncated) content with leading space: {:?}",
            title_row.2
        );
        assert!(title_row
            .2
            .last()
            .map(|s| *s == "</title>")
            .unwrap_or(false));

        // The head's closing row sits directly after the title leaf row.
        let head_close_row = &rows[title_idx + 1];
        assert_eq!(head_close_row.0, None, "closing rows carry no tree-row id");
        assert_eq!(
            head_close_row.2,
            vec!["  </head>"],
            "closing tag on its own row after children: {:?}",
            head_close_row.2
        );

        // Every node row keeps its `tree-row-{id}` id so selection can highlight it.
        let node_rows: Vec<&str> = rows.iter().filter_map(|(id, _, _)| *id).collect();
        assert!(!node_rows.is_empty(), "rows carry tree-row ids");
        assert_eq!(
            node_rows.len(),
            15,
            "one id per real node (input + echo + icon row + 5 icons added): {:?}",
            node_rows
        );

        // Icons show as `<icon-home>` (name from the tag suffix), not a generic
        // `<icon>`, so the tree reflects the actual element.
        let icon_row = rows
            .iter()
            .find(|(_, _, p)| p.iter().any(|t| t.contains("icon-home")))
            .expect("icon-home row");
        assert!(
            icon_row.2.iter().any(|t| t.contains("</icon-home>")),
            "icon leaf keeps its closing tag: {:?}",
            icon_row.2
        );
    }

    #[test]
    fn test_devtools_leaf_content_truncated_to_fit() {
        // Long leaf text must be ellipsized so the closing tag stays visible
        // (it used to be pushed past the tree's right edge and clipped).
        let handle = test_handle();
        // Node 2 is the button with text "Click me"; make its text very long.
        execute_script(
            &handle,
            "__pwb_setText('2', 'This is an extremely long button label that would overflow the tree row');",
        )
        .unwrap();
        execute_script(&handle, "__pwb_refreshDevtools(1800, 1300);").unwrap();
        let dom = handle.dom.borrow();
        let row = find_node_by_id(&dom.root, "tree-row-2").expect("row exists");
        let parts: Vec<&str> = row
            .children
            .iter()
            .map(|c| c.text.as_deref().unwrap_or(""))
            .collect();
        // First part is the opening tag, last is the closing tag.
        assert_eq!(
            parts[0], "  <button #main-btn>",
            "opening tag with id and > baked in"
        );
        assert_eq!(
            parts.last().copied(),
            Some("</button>"),
            "closing tag always present"
        );
        // Content has a leading space and is truncated with an ellipsis.
        let content = parts.iter().find(|p| p.ends_with('…')).unwrap();
        assert!(
            content.starts_with(" This is"),
            "content preserved with leading space: {:?}",
            content
        );
    }

    #[test]
    fn test_input_value_and_focus_bindings() {
        // A small page with an `<input id="field" value="hello">`.
        let root = DomNode {
            id: 1,
            tag: Tag::View,
            attributes: HashMap::new(),
            text: None,
            children: vec![DomNode {
                id: 2,
                tag: Tag::Input,
                attributes: HashMap::from([
                    ("id".to_string(), "field".to_string()),
                    ("value".to_string(), "hello".to_string()),
                ]),
                text: None,
                children: vec![],
            }],
        };
        let handle = DomHandle::new(Dom {
            title: "t".to_string(),
            root,
            components: HashMap::new(),
            modules: HashMap::new(),
        });
        execute_script(
            &handle,
            r##"
            var f = document.getElementById("field");
            if (f.getValue() !== "hello") throw new Error("getValue: " + f.getValue());
            f.setValue("world");
            if (f.getValue() !== "world") throw new Error("after setValue: " + f.getValue());
            "##,
        )
        .unwrap();
        // setValue wrote the `value` attribute.
        {
            let dom = handle.dom.borrow();
            let node = find_node_by_id(&dom.root, "2").unwrap();
            assert_eq!(
                node.attributes.get("value").map(|s| s.as_str()),
                Some("world")
            );
        }
        // focus()/blur() flip the shared focus state (caret at value end).
        assert_eq!(*handle.focused_input.borrow(), None);
        execute_script(&handle, "document.getElementById('field').focus();").unwrap();
        assert_eq!(*handle.focused_input.borrow(), Some((2, 5)));
        execute_script(&handle, "document.getElementById('field').blur();").unwrap();
        assert_eq!(*handle.focused_input.borrow(), None);
    }

    #[test]
    fn test_console_input_enter_evaluates_js() {
        // The devtools console's command input evaluates typed JS on Enter and
        // echoes the expression into the panel above it.
        let devtools_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
        let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
        let mut devtools_dom = Dom::from_bundle(&bundle);
        devtools_dom
            .root
            .attributes
            .insert("data-devtools".to_string(), "root".to_string());
        let mut main = Dom {
            title: "Demo".to_string(),
            components: HashMap::new(),
            modules: HashMap::new(),
            root: DomNode {
                id: 1,
                tag: Tag::View,
                attributes: HashMap::new(),
                text: None,
                children: vec![],
            },
        };
        main.add_child(devtools_dom.root);
        let handle = DomHandle::new(main);
        let script = std::str::from_utf8(bundle.script_payload.as_deref().unwrap()).unwrap();
        execute_script(&handle, script).expect("devtools script runs");

        let input_id = {
            let dom = handle.dom.borrow();
            find_node_by_id(&dom.root, "console-input")
                .expect("console input exists")
                .id
        };
        // Type `1 + 2` into the input, then press Enter (dispatch the handler).
        execute_script(&handle, &format!("__pwb_setValue('{input_id}', '1 + 2');")).unwrap();
        let handlers = handle.event_registry.get_handlers(input_id, "enter");
        assert_eq!(
            handlers.len(),
            1,
            "console input registers an enter handler"
        );
        execute_event_handler_typed(&handle, input_id, &handlers[0], Some("enter"))
            .expect("enter dispatch");

        let dom = handle.dom.borrow();
        let output = find_node_by_id(&dom.root, "console-output").unwrap();
        let texts: Vec<&str> = output
            .children
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("> 1 + 2")),
            "echoes the expression: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("3")),
            "logs the result: {:?}",
            texts
        );
        // The input cleared itself after submit.
        let input = find_node_by_id(&dom.root, "console-input").unwrap();
        assert_eq!(
            input.attributes.get("value"),
            None,
            "input clears after Enter"
        );
    }

    #[test]
    fn test_console_arrowup_arrowdown_events() {
        let _handle = test_handle();
        // Mount devtools and run its script.
        let devtools_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
        let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
        let mut devtools_dom = Dom::from_bundle(&bundle);
        devtools_dom
            .root
            .attributes
            .insert("data-devtools".to_string(), "root".to_string());
        let mut main = crate::dom::Dom {
            root: crate::dom::DomNode {
                id: 1,
                tag: Tag::View,
                attributes: std::collections::HashMap::new(),
                text: None,
                children: vec![],
            },
            title: "Demo".to_string(),
            components: std::collections::HashMap::new(),
            modules: std::collections::HashMap::new(),
        };
        main.add_child(devtools_dom.root);
        let handle = DomHandle::new(main);
        let script = std::str::from_utf8(bundle.script_payload.as_deref().unwrap()).unwrap();
        execute_script(&handle, script).expect("devtools script runs");

        let input_id = {
            let dom = handle.dom.borrow();
            find_node_by_id(&dom.root, "console-input")
                .expect("console input exists")
                .id
        };
        // Devtools script registers arrowup + arrowdown handlers on the
        // console input (for command history navigation).
        let handlers_up = handle.event_registry.get_handlers(input_id, "arrowup");
        assert_eq!(handlers_up.len(), 1, "arrowup handler registered");
        // Dispatching must not error.
        execute_event_handler_typed(&handle, input_id, &handlers_up[0], Some("arrowup"))
            .expect("arrowup dispatch");

        let handlers_down = handle.event_registry.get_handlers(input_id, "arrowdown");
        assert_eq!(handlers_down.len(), 1, "arrowdown handler registered");
        execute_event_handler_typed(&handle, input_id, &handlers_down[0], Some("arrowdown"))
            .expect("arrowdown dispatch");
    }

    #[test]
    fn test_inspect_mode_bindings_toggle_flag() {
        let handle = test_handle();
        assert!(!handle.inspect_mode.load(Ordering::Relaxed));
        execute_script(&handle, "__pwb_startElementSelection();").unwrap();
        assert!(handle.inspect_mode.load(Ordering::Relaxed));
        execute_script(&handle, "__pwb_stopElementSelection();").unwrap();
        assert!(!handle.inspect_mode.load(Ordering::Relaxed));
    }

    #[test]
    fn test_dom_tree_and_append_tree_row() {
        let handle = test_handle();
        execute_script(
            &handle,
            r##"
            var tree = __pwb_getDomTree();
            if (tree.indexOf("2|1|button|main-btn|Click me") === -1) throw new Error("tree missing button: " + tree);
            // The devtools subtree must be excluded.
            if (tree.indexOf("devtools-sidebar") !== -1) throw new Error("tree leaked devtools");
            __pwb_appendTreeRow("#console-output", "tree-row-2", "  ", "button", "main-btn", "Click me");
            "##,
        )
        .expect("script should run");
        let dom = handle.dom.borrow();
        let output = find_node_by_id(&dom.root, "console-output").unwrap();
        assert_eq!(output.children.len(), 1);
        let row = &output.children[0];
        assert_eq!(row.tag, Tag::View);
        assert_eq!(
            row.attributes.get("id").map(|s| s.as_str()),
            Some("tree-row-2")
        );
        // Syntax-highlighted parts: <button #main-btn>, Click me, </button> —
        // the `>` is baked into the opening tag, one space before content.
        let parts: Vec<&str> = row
            .children
            .iter()
            .map(|c| c.text.as_deref().unwrap())
            .collect();
        assert_eq!(
            parts,
            vec!["  <button #main-btn>", " Click me", "</button>"]
        );
        assert_eq!(
            row.children[0].attributes.get("color").map(|s| s.as_str()),
            Some("#569cd6")
        );
    }

    #[test]
    fn test_console_error_logs_to_panel() {
        let handle = test_handle();
        execute_script(&handle, "console.error('something failed');").unwrap();
        let dom = handle.dom.borrow();
        let output = find_node_by_id(&dom.root, "console-output").unwrap();
        assert_eq!(output.children.len(), 1);
        assert_eq!(output.children[0].text.as_deref(), Some("something failed"));
        assert_eq!(
            output.children[0]
                .attributes
                .get("color")
                .map(|s| s.as_str()),
            Some("#f44747")
        );
        // Console lines are `<code>` nodes so the renderer highlights them,
        // and the output panel is flagged to auto-scroll to the new line.
        assert_eq!(
            output.children[0].tag,
            Tag::Code,
            "console lines are code elements"
        );
        let scrolls = handle.scroll_to_bottom.borrow();
        assert!(
            scrolls.contains(&output.id),
            "console output flagged to auto-scroll"
        );
    }

    #[test]
    fn test_devtools_console_renders_with_syntax_highlighting() {
        // Mount the real devtools bundle, switch to the Console tab, log a few
        // lines, and render: keyword/string tokens must paint in the Chrome
        // palette and error lines keep their red base. Skips when the devtools
        // directory isn't present (e.g. a bare checkout).
        use crate::bundler::bundle_from_dir;
        let devtools_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
        if !devtools_dir.exists() {
            return;
        }
        let bundle = bundle_from_dir(&devtools_dir).expect("devtools bundle");
        let handle = DomHandle::new(Dom::from_bundle(&bundle));
        // Activate the Console tab (what app.pjs does on click).
        execute_script(
            &handle,
            "document.getElementById('tab-console').setStyle('background-color', '#0078d7'); document.getElementById('elements-tab').setStyle('display', 'none'); document.getElementById('console-tab').setStyle('display', 'block');",
        )
        .unwrap();
        execute_script(
            &handle,
            "console.log('> var x = 42;'); console.log(\"'a string'\"); console.error('oops');",
        )
        .unwrap();

        let mut pixmap = tiny_skia::Pixmap::new(1200, 800).unwrap();
        let dom = handle.dom.borrow();
        crate::render::render_dom_into_pixmap(
            &mut pixmap,
            &dom,
            1200,
            800,
            0.0,
            &crate::render::TextSelection::default(),
            &HashMap::new(),
            None,
        );
        drop(dom);
        let mut keyword_px = 0;
        let mut string_px = 0;
        let mut error_base_px = 0;
        for y in 0..800 {
            for x in 0..1200 {
                let p = pixmap.pixel(x, y).unwrap();
                let (r, g, b) = (p.red(), p.green(), p.blue());
                // Antialiased keyword pixels on dark background (#1e1e1e) have b > g > r
                if b > g && g > r + 15 && b > 80 {
                    keyword_px += 1;
                }
                // Antialiased string pixels have r > g > b
                if r > g && g > b + 15 && r > 80 {
                    string_px += 1;
                }
                // Error base pixels have high red, low green/blue
                if r > g + 30 && r > b + 30 && r > 60 {
                    error_base_px += 1;
                }
            }
        }
        assert!(
            keyword_px > 10,
            "'var' keyword painted blue (got {})",
            keyword_px
        );
        assert!(
            string_px > 10,
            "'a string' painted orange (got {})",
            string_px
        );
        assert!(
            error_base_px > 10,
            "error line base stays red (got {})",
            error_base_px
        );
        assert!(
            string_px > 50,
            "'a string' painted orange (got {})",
            string_px
        );
        assert!(
            error_base_px > 50,
            "error line base stays red (got {})",
            error_base_px
        );
    }

    #[test]
    fn test_select_container_highlights_whole_subtree() {
        // A page root (View) holding a container (id 2) with a button (3) and a
        // text (4) child, plus the mounted devtools.
        let root = DomNode {
            id: 1,
            tag: Tag::View,
            attributes: HashMap::new(),
            text: None,
            children: vec![
                DomNode {
                    id: 2,
                    tag: Tag::View,
                    attributes: HashMap::from([("id".to_string(), "card".to_string())]),
                    text: None,
                    children: vec![
                        DomNode {
                            id: 3,
                            tag: Tag::Button,
                            attributes: HashMap::new(),
                            text: Some("Go".to_string()),
                            children: vec![],
                        },
                        DomNode {
                            id: 4,
                            tag: Tag::Text,
                            attributes: HashMap::new(),
                            text: Some("caption".to_string()),
                            children: vec![],
                        },
                    ],
                },
                DomNode {
                    id: 5,
                    tag: Tag::View,
                    attributes: HashMap::from([
                        ("class".to_string(), "devtools-sidebar".to_string()),
                        ("data-devtools".to_string(), "root".to_string()),
                    ]),
                    text: None,
                    children: vec![
                        DomNode {
                            id: 6,
                            tag: Tag::View,
                            attributes: HashMap::from([(
                                "id".to_string(),
                                "elements-tree".to_string(),
                            )]),
                            text: None,
                            children: vec![],
                        },
                        DomNode {
                            id: 7,
                            tag: Tag::View,
                            attributes: HashMap::from([(
                                "id".to_string(),
                                "styles-panel".to_string(),
                            )]),
                            text: None,
                            children: vec![],
                        },
                    ],
                },
            ],
        };
        let handle = DomHandle::new(Dom {
            title: "t".to_string(),
            root,
            components: HashMap::new(),
            modules: HashMap::new(),
        });
        execute_script(&handle, "__pwb_refreshDevtools(1800, 1300);").unwrap();

        // Selecting a container highlights only that container's opening AND
        // closing rows — not its children's rows.
        execute_script(&handle, "__pwb_selectElement(2);").unwrap();
        assert_eq!(*handle.selected_node_id.borrow(), Some(2));
        {
            let dom = handle.dom.borrow();
            let highlighted: Vec<&str> = {
                let tree = find_node_by_id(&dom.root, "elements-tree").expect("tree");
                tree.children
                    .iter()
                    .filter(|r| {
                        r.attributes
                            .get("background-color")
                            .map(|s| s == "#094771")
                            .unwrap_or(false)
                    })
                    .filter_map(|r| r.attributes.get("data-node").map(|s| s.as_str()))
                    .collect()
            };
            // Both the opening row and the closing row of element 2 are
            // highlighted (data-node="2" appears twice), and nothing else.
            assert_eq!(
                highlighted.len(),
                2,
                "exactly the open+close rows of 2: {:?}",
                highlighted
            );
            assert!(highlighted.iter().all(|d| *d == "2"));
            // Children 3 and 4 are one level deeper in the tree: their rows
            // carry data-node "3"/"4" and must NOT be highlighted.
            let child_highlighted = highlighted.iter().any(|d| *d != "2");
            assert!(
                !child_highlighted,
                "children are not selected: {:?}",
                highlighted
            );
        }

        // A refresh (rebuild) keeps the single-element highlight: still exactly
        // the open + close rows of 2, with children un-highlighted.
        execute_script(&handle, "__pwb_refreshDevtools(1800, 1300);").unwrap();
        {
            let dom = handle.dom.borrow();
            let tree = find_node_by_id(&dom.root, "elements-tree").expect("tree");
            for row in &tree.children {
                let on = row
                    .attributes
                    .get("background-color")
                    .map(|b| b.as_str() == "#094771")
                    .unwrap_or(false);
                let data_node = row.attributes.get("data-node").map(|s| s.as_str());
                assert_eq!(
                    on,
                    data_node == Some("2"),
                    "row for {} highlight mismatch (selected=element 2)",
                    data_node.unwrap_or("(closing row w/o id?)")
                );
            }
        }
    }

    /// Read the test button's live text (side-effect sink for scripts).
    fn btn_text(handle: &DomHandle) -> String {
        let dom = handle.dom.borrow();
        find_node_by_id(&dom.root, "main-btn")
            .and_then(|n| n.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn test_settimeout_fires_via_pump() {
        let handle = test_handle();
        execute_script(
            &handle,
            "var t = setTimeout(function() { __pwb_setText('#main-btn', 'fired'); }, 5);",
        )
        .unwrap();
        assert_eq!(btn_text(&handle), "Click me", "not fired yet");
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(pump_timers(&handle), "a timer was due");
        assert_eq!(btn_text(&handle), "fired");
        // One-shot: pumping again does nothing.
        assert!(!pump_timers(&handle));
    }

    #[test]
    fn test_setinterval_fires_repeatedly_and_clears() {
        let handle = test_handle();
        execute_script(
            &handle,
            "var n = 0; var iv = setInterval(function() { n++; __pwb_setText('#main-btn', 'n=' + n); if (n >= 2) clearInterval(iv); }, 5);",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(pump_timers(&handle));
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(pump_timers(&handle));
        assert_eq!(btn_text(&handle), "n=2");
        // Interval cleared itself: nothing more fires.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!pump_timers(&handle));
    }

    #[test]
    fn test_local_storage_js_api() {
        let handle = test_handle();
        execute_script(
            &handle,
            "localStorage.setItem('k', 'v1'); sessionStorage.setItem('s', 'sv'); __pwb_setText('#main-btn', localStorage.getItem('k') + '|' + localStorage.length + '|' + sessionStorage.getItem('s'));",
        )
        .unwrap();
        assert_eq!(btn_text(&handle), "v1|1|sv");
        // Persists across separate execute_script calls (same handle/context).
        execute_script(&handle, "__pwb_setText('#main-btn', localStorage.getItem('k') + '|' + sessionStorage.getItem('s'));")
            .unwrap();
        assert_eq!(btn_text(&handle), "v1|sv");
        execute_script(
            &handle,
            "localStorage.removeItem('k'); localStorage.clear(); __pwb_setText('#main-btn', String(localStorage.length));",
        )
        .unwrap();
        assert_eq!(btn_text(&handle), "0");
    }

    #[test]
    fn test_base64_crypto_performance_js() {
        let handle = test_handle();
        execute_script(
            &handle,
            "__pwb_setText('#main-btn', btoa('ab') + '|' + atob('YWI=') + '|' + String(performance.now() >= 0) + '|' + crypto.randomUUID().length + '|' + crypto.getRandomValues(new Uint8Array(8)).length);",
        )
        .unwrap();
        let text = btn_text(&handle);
        let parts: Vec<&str> = text.split('|').collect();
        assert_eq!(parts[0], "YWI=");
        assert_eq!(parts[1], "ab");
        assert_eq!(parts[2], "true");
        assert_eq!(parts[3], "36");
        assert_eq!(parts[4], "8");
        // btoa throws on chars above Latin-1; the error reaches JS.
        execute_script(&handle, "try { btoa('€'); __pwb_setText('#main-btn', 'no-throw'); } catch (e) { __pwb_setText('#main-btn', 'threw'); }")
            .unwrap();
        assert_eq!(btn_text(&handle), "threw");
    }

    #[test]
    fn test_dom_building_js_api() {
        let handle = test_handle();
        execute_script(
            &handle,
            "var el = document.createElement('view'); el.setAttribute('id', 'dyn'); el.setText('Dyn!'); document.getElementById('main-btn').appendChild(el);",
        )
        .unwrap();
        {
            let dom = handle.dom.borrow();
            let dyn_node = find_node_by_selector(&dom.root, "#dyn").expect("appended node");
            assert_eq!(dyn_node.text.as_deref(), Some("Dyn!"));
            // The button (id 2) has it as a direct child.
            let btn = find_node_by_id(&dom.root, "main-btn").unwrap();
            assert!(btn.children.iter().any(|c| c.id == dyn_node.id));
        }
        // setAttribute/getAttribute round-trip through the proxy.
        execute_script(
            &handle,
            "__pwb_setText('#main-btn', document.getElementById('dyn').getAttribute('id'));",
        )
        .unwrap();
        assert_eq!(btn_text(&handle), "dyn");
        // removeChild detaches it from the tree.
        execute_script(
            &handle,
            "var el = document.getElementById('dyn'); document.getElementById('main-btn').removeChild(el);",
        )
        .unwrap();
        {
            let dom = handle.dom.borrow();
            assert!(
                find_node_by_selector(&dom.root, "#dyn").is_none(),
                "removed from tree"
            );
        }
    }

    #[test]
    fn test_fetch_resolves_promise() {
        // A tiny HTTP server on localhost that answers one request.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let body = "hello-from-server";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = std::io::Write::write_all(&mut stream, resp.as_bytes());
        });

        let handle = test_handle();
        let url = format!("http://127.0.0.1:{}/hello", port);
        let script = format!(
            "fetch('{}').then(function(r) {{ return r.text(); }}).then(function(t) {{ __pwb_setText('#main-btn', t); }});",
            url
        );
        execute_script(&handle, &script).unwrap();

        // Pump until the response arrives (the background thread is fast).
        let mut got = false;
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            poll_async_ops(&handle);
            if btn_text(&handle) == "hello-from-server" {
                got = true;
                break;
            }
        }
        assert!(
            got,
            "fetch promise resolved and .then ran (text was {:?})",
            btn_text(&handle)
        );
    }

    #[test]
    fn test_fetch_rejects_on_connection_error() {
        // A port with nothing listening → the background thread errors → reject.
        let handle = test_handle();
        let url = "http://127.0.0.1:1/nothing";
        let script = format!(
            "fetch('{}').then(function(r) {{ __pwb_setText('#main-btn', 'resolved'); }}).catch(function(e) {{ __pwb_setText('#main-btn', 'rejected'); }});",
            url
        );
        execute_script(&handle, &script).unwrap();
        let mut got = false;
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            poll_async_ops(&handle);
            if btn_text(&handle) == "rejected" {
                got = true;
                break;
            }
        }
        assert!(got, "fetch rejected (text was {:?})", btn_text(&handle));
    }
}

#[test]
fn scratch_hit_elements_tree_rows() {
    let devtools_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
    let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
    let mut devtools_dom = Dom::from_bundle(&bundle);
    devtools_dom
        .root
        .attributes
        .insert("data-devtools".to_string(), "root".to_string());
    let demo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/demo.pwb");
    let demo_bundle = crate::bundler::bundle_from_dir(&demo_dir).expect("demo bundle");
    let mut main = Dom::from_bundle(&demo_bundle);
    main.add_child(devtools_dom.root);
    let handle = DomHandle::new(main);
    execute_script(&handle, "__pwb_refreshDevtools(1800, 1300);").unwrap();

    let dom = handle.dom.borrow();
    let layout_root = crate::layout::build_layout_tree(&dom, 1800, 1300, 0.0);
    let tree = crate::layout::find_box_by_id(&layout_root, /*elements-tree id*/ 100016)
        .expect("elements tree box");
    println!(
        "=== elements-tree box: x={:.1} y={:.1} w={:.1} h={:.1}",
        tree.rect.left(),
        tree.rect.top(),
        tree.rect.width(),
        tree.rect.height()
    );
    for row in &tree.children {
        let r = row.rect;
        println!(
            "ROW box={} y={:.1} h={:.1}",
            row.node_id,
            r.top(),
            r.height()
        );
        for c in &row.children {
            if let Some(t) = c.text.as_deref() {
                let cr = c.rect;
                let px = cr.left() + cr.width() / 2.0;
                let py = cr.top() + cr.height() / 2.0;
                let hit = crate::render::hit_test_layout_box_scrolled(
                    &layout_root,
                    px,
                    py,
                    &std::collections::HashMap::new(),
                );
                let mismatch = hit.map(|b| b.node_id) != Some(c.node_id);
                println!(
                    "    {} box={} text={:?} x={:.1} y={:.1} w={:.1} -> hit={} {}",
                    t,
                    c.node_id,
                    t,
                    cr.left(),
                    cr.top(),
                    cr.width(),
                    hit.map(|b| b.node_id).unwrap_or(0),
                    if mismatch {
                        "<== MISMATCH (element above)"
                    } else {
                        ""
                    }
                );
            }
        }
    }
    drop(dom);
}
#[test]
/// Rows in the Elements tree are rebuilt with fresh node ids; they must ALL
/// be unique. A shared id makes the pipeline's id-walk
/// `tree_row_target_node` resolve a click to the wrong row's `data-node`
/// (reported: clicking the `<input #demo-input>` row selected the
/// `<icon-github>` element two rows above). Every visible clickable point
/// inside a row must resolve back to that row's `data-node`.
fn test_tree_rows_hit_only_their_own_row() {
    for (vw, vh) in [
        (1800, 1300),
        (1500, 1000),
        (1200, 900),
        (1000, 700),
        (900, 600),
        (800, 500),
    ] {
        let devtools_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
        let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
        let mut devtools_dom = Dom::from_bundle(&bundle);
        devtools_dom
            .root
            .attributes
            .insert("data-devtools".to_string(), "root".to_string());
        let demo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/demo.pwb");
        let demo_bundle = crate::bundler::bundle_from_dir(&demo_dir).expect("demo bundle");
        let mut main = Dom::from_bundle(&demo_bundle);
        main.add_child(devtools_dom.root);
        let handle = DomHandle::new(main);
        execute_script(&handle, &format!("__pwb_refreshDevtools({}, {});", vw, vh)).unwrap();

        let dom = handle.dom.borrow();
        let layout_root = crate::layout::build_layout_tree(&dom, vw, vh, 0.0);
        let scrolls = std::collections::HashMap::new();
        let tree = crate::layout::find_box_by_id(&layout_root, 100016).expect("elements tree box");
        // The tree is `overflow: scroll`: only points inside its visible
        // viewport are clickable, so clamp the samples to `tree.rect`.
        let vp_top = tree.rect.top();
        let vp_bottom = tree.rect.bottom();

        for row in &tree.children {
            let expected = find_node_by_id(&dom.root, &row.node_id.to_string())
                .and_then(|n| n.attributes.get("data-node"))
                .and_then(|v| v.parse::<usize>().ok());
            let Some(expected) = expected else { continue };

            let r = row.rect;
            let y0 = r.top().max(vp_top).max(0.0);
            let y1 = r.bottom().min(vp_bottom);
            let mut bad = Vec::new();
            let mut y = y0;
            while y < y1 {
                // Sample the row's own padding/"void" (before its text) and
                // deeper into the text region on the next pass.
                for x in [r.left() + 6.0, r.left() + 38.0] {
                    if let Some(hit) =
                        crate::render::hit_test_layout_box_scrolled(&layout_root, x, y, &scrolls)
                    {
                        let got = crate::pipeline::tree_row_target_node(&dom.root, hit.node_id);
                        if got != Some(expected) {
                            bad.push((y, x, hit.node_id, got));
                        }
                    }
                }
                y += 0.5;
            }
            assert!(bad.is_empty(),
                    "[{}x{}] row box={} (data-node={}) y=[{:.0},{:.0}]: {} visible clicks resolved to a different element: {:?}",
                    vw, vh, row.node_id, expected, r.top(), r.height(), bad.len(), &bad[..bad.len().min(4)]);
        }
        drop(dom);
    }
}
