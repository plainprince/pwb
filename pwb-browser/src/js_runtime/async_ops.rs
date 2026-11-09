//! Async operation polling, timer pumping, and JS event-handler dispatch.

use crate::webapi;
use rquickjs::{CatchResultExt, Function, Object};

use super::core::{get_or_create_context, DomHandle, JsError};
use super::devtools::append_console_line;
use super::errors::{drain_microtasks, report_error};
use super::script::execute_script_named;

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
                    // An exception in a timer callback must not vanish: report
                    // it to the devtools console like any other uncaught error.
                    if let Err(caught) = callback.call::<_, ()>(()).catch(&ctx) {
                        report_error(dom_handle, &caught);
                    }
                }
                if interval_ms.is_none() {
                    // One-shot: drop the callback from the registry.
                    let _ = reg.remove(id.to_string());
                }
            }
        }
        // Run any promise reactions the callbacks queued.
        drain_microtasks(&ctx, dom_handle);
    });
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
                                    if let Err(caught) = resolve.call::<_, ()>((obj,)).catch(&ctx) {
                                        report_error(dom_handle, &caught);
                                    }
                                }
                            }
                            Err(msg) => {
                                if let Some(reject) = reject {
                                    if let Err(caught) = reject.call::<_, ()>((msg,)).catch(&ctx) {
                                        report_error(dom_handle, &caught);
                                    }
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
                        if let Err(caught) = f.call::<_, ()>((id, kind, data)).catch(&ctx) {
                            report_error(dom_handle, &caught);
                        }
                    }
                }
            }
        }
        drain_microtasks(&ctx, dom_handle);
    });
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
    // The prelude stays on the handler's first line so an exception's reported
    // line numbers still match the handler source, while the stack names the
    // event and element that failed (`event-click-12.pjs`).
    let prelude = match event_type {
        Some(evt) => format!(
            "var el = document.selector('#{node_id}');\
             var __evt = {{ type: '{evt}', defaultPrevented: false, preventDefault: function() {{ this.defaultPrevented = true; __pwb_suppressContextMenu(true); }} }};\
             var event = __evt;"
        ),
        None => format!("var el = document.selector('#{node_id}');"),
    };
    let script = if trimmed.starts_with("function") || trimmed.starts_with("async function") {
        // The newline keeps a trailing `// comment` from swallowing the
        // closing call (and leaves reported line numbers unchanged).
        match event_type {
            Some(_) => format!("{prelude}({trimmed}\n).call(el, event);"),
            None => format!("{prelude}({trimmed}\n).call(el);"),
        }
    } else {
        match event_type {
            Some(_) => format!("{prelude}(function(evt){{{trimmed}\n}}).call(el, event);"),
            None => format!("{prelude}(function(){{{trimmed}\n}}).call(el);"),
        }
    };
    let label = match event_type {
        Some(evt) => format!("event-{evt}-{node_id}.pjs"),
        None => format!("event-click-{node_id}.pjs"),
    };
    execute_script_named(dom_handle, &script, &label)
}

/// Log a message to the devtools console panel (and stdout). Used by the pipeline
/// for non-JS events such as element inspection.
pub fn log_console(dom_handle: &DomHandle, msg: &str) {
    println!("[QuickJS Console] {}", msg);
    append_console_line(dom_handle, "log", msg);
}
