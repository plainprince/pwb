//! Async operation polling, timer pumping, and JS event-handler dispatch.

use crate::webapi;
use rquickjs::{Function, Object};

use super::core::{get_or_create_context, DomHandle, JsError};
use super::devtools::append_console_line;
use super::script::execute_script;

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
