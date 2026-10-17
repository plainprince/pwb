//! Web API bindings: fetch, timers, storage, dialogs, crypto, DOM building, WebSocket.

use crate::dom::DomNode;
use crate::webapi;
use pwb_core::spec::Tag;
use rquickjs::prelude::List;
use rquickjs::{Ctx, Function, Object};
use std::collections::HashMap;

use super::super::core::DomHandle;
use super::super::dom_utils::{
    find_node_by_id, find_node_by_id_mut, max_node_id, remove_node_from_tree,
};

pub(super) fn install<'js>(dom_handle: &DomHandle, ctx: &Ctx<'js>, global: &Object<'js>) {
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
    let fetch_fn = Function::new(
        ctx.clone(),
        move |id: u64, url: String, method: String, headers_json: String, body: String| {
            let headers: Vec<(String, String)> =
                serde_json::from_str::<serde_json::Value>(&headers_json)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .map(|m| {
                        m.into_iter()
                            .filter_map(|(k, val)| val.as_str().map(|s| (k, s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
            webapi::spawn_fetch(&fetch_handle.web, id, url, method, headers, body);
        },
    )
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
    let storage_fn = Function::new(
        ctx.clone(),
        move |kind: String, op: String, key: String, value: String| -> String {
            webapi::storage_op(&stor.web, &kind, &op, &key, &value)
        },
    )
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
            Function::new(ctx.clone(), move |msg: String| -> bool {
                webapi::show_confirm(&msg)
            })
            .unwrap(),
        )
        .unwrap();
    global
        .set(
            "__pwb_prompt",
            Function::new(
                ctx.clone(),
                move |msg: String, default_val: String| -> Option<String> {
                    webapi::show_prompt(&msg, &default_val)
                },
            )
            .unwrap(),
        )
        .unwrap();

    // crypto + base64 + performance.now.
    global
        .set(
            "__pwb_randomBytes",
            Function::new(ctx.clone(), move |n: u32| -> String {
                webapi::random_bytes(n)
            })
            .unwrap(),
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
    let btoa_fn = Function::new(
        ctx.clone(),
        move |s: String| -> List<(bool, String, String)> {
            match webapi::btoa(&s) {
                Ok(v) => List((true, v, String::new())),
                Err(e) => List((false, String::new(), e)),
            }
        },
    )
    .unwrap();
    global.set("__pwb_btoa", btoa_fn).unwrap();
    let atob_fn = Function::new(
        ctx.clone(),
        move |s: String| -> List<(bool, String, String)> {
            match webapi::atob(&s) {
                Ok(v) => List((true, v, String::new())),
                Err(e) => List((false, String::new(), e)),
            }
        },
    )
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
            Function::new(ctx.clone(), move || -> u64 {
                dom_root.dom.borrow().root.id as u64
            })
            .unwrap(),
        )
        .unwrap();

    let dom_ac = dom_handle.clone();
    global
        .set(
            "__pwb_appendChild",
            Function::new(
                ctx.clone(),
                move |parent_id: String, child_id: String| -> bool {
                    let Some(node) =
                        webapi::take_detached(&dom_ac.web, child_id.parse().unwrap_or(0))
                    else {
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
                },
            )
            .unwrap(),
        )
        .unwrap();

    let dom_rc = dom_handle.clone();
    global
        .set(
            "__pwb_removeChild",
            Function::new(
                ctx.clone(),
                move |parent_id: String, child_id: String| -> bool {
                    let mut dom = dom_rc.dom.borrow_mut();
                    let Some(parent) = find_node_by_id_mut(&mut dom.root, &parent_id) else {
                        return false;
                    };
                    if let Some(pos) = parent
                        .children
                        .iter()
                        .position(|c| c.id == child_id.parse().unwrap_or(0))
                    {
                        let node = parent.children.remove(pos);
                        drop(dom);
                        webapi::put_detached(&dom_rc.web, node);
                        true
                    } else {
                        false
                    }
                },
            )
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
                let Some(node) = remove_node_from_tree(&mut dom.root, node_id.parse().unwrap_or(0))
                else {
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
            Function::new(
                ctx.clone(),
                move |node_id: String, name: String, value: String| -> bool {
                    {
                        let mut dom = dom_sa.dom.borrow_mut();
                        if let Some(node) = find_node_by_id_mut(&mut dom.root, &node_id) {
                            node.attributes.insert(name, value);
                            return true;
                        }
                    }
                    if let Some(node) = dom_sa
                        .web
                        .detached
                        .borrow_mut()
                        .get_mut(&node_id.parse().unwrap_or(0))
                    {
                        node.attributes.insert(name, value);
                        return true;
                    }
                    false
                },
            )
            .unwrap(),
        )
        .unwrap();

    let dom_ga = dom_handle.clone();
    global
        .set(
            "__pwb_getAttribute",
            Function::new(
                ctx.clone(),
                move |node_id: String, name: String| -> String {
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
                },
            )
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
}
