//! Native `document`/`pwb` bindings: console, DOM primitives, devtools, components.

use crate::bundler;
use rquickjs::function::Opt;
use rquickjs::{Ctx, Function, Object, Value};
use std::sync::atomic::Ordering;

use super::super::core::DomHandle;
use super::super::devtools::{
    append_console_line, collect_dom_tree_lines, highlight_selected_node, make_tree_row,
    rebuild_tree_rows, render_styles_panel,
};
use super::super::dom_utils::{
    find_node_by_id, find_node_by_id_mut, find_node_by_selector, find_node_by_selector_mut,
    max_node_id, parse_props_json, reassign_ids, substitute_params_dom,
};

pub(super) fn install<'js>(dom_handle: &DomHandle, ctx: &Ctx<'js>, global: &Object<'js>) {
    // 1. console.log/info/warn/error bindings: printed to stdout and mirrored into
    //    the devtools `#console-output` panel when it is mounted.
    let console = Object::new(ctx.clone()).unwrap();
    for (name, level) in [
        ("log", "log"),
        ("info", "log"),
        ("warn", "warn"),
        ("error", "error"),
    ] {
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
    let set_text_fn = Function::new(
        ctx.clone(),
        move |id_or_tag: String, text: String| -> bool {
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
        },
    )
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
    let set_value_fn = Function::new(
        ctx.clone(),
        move |id_or_tag: String, value: String| -> bool {
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
        },
    )
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
                let caret = node
                    .attributes
                    .get("value")
                    .map(|v| v.chars().count())
                    .unwrap_or(0);
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
    let get_style_fn = Function::new(
        ctx.clone(),
        move |id_or_tag: String, prop: String| -> String {
            let dom = dom_c5.dom.borrow();
            if let Some(node) = find_node_by_selector(&dom.root, &id_or_tag) {
                node.attributes.get(&prop).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        },
    )
    .unwrap();
    global.set("__pwb_getStyle", get_style_fn).unwrap();

    let dom_c6 = dom_handle.clone();
    let set_style_fn = Function::new(
        ctx.clone(),
        move |id_or_tag: String, prop: String, val: String| -> bool {
            let mut dom = dom_c6.dom.borrow_mut();
            if let Some(node) = find_node_by_selector_mut(&mut dom.root, &id_or_tag) {
                node.attributes.insert(prop, val);
                true
            } else {
                false
            }
        },
    )
    .unwrap();
    global.set("__pwb_setStyle", set_style_fn).unwrap();

    // Event handler registration: handlers are keyed by node id (both the new
    // usize-keyed registry and the legacy string registry) and by the element's
    // `id` attribute so dispatch can find them either way.
    let dom_c7 = dom_handle.clone();
    let reg_event_fn = Function::new(
        ctx.clone(),
        move |id: String, event_type: String, js_code: String| {
            let node_id: Option<usize> = id.parse().ok();
            if let Some(nid) = node_id {
                dom_c7
                    .event_registry
                    .add_handler(nid, &event_type, js_code.clone());
            }
            dom_c7
                .event_registry
                .register_click_handler(&id, js_code.clone());
            if let Ok(dom) = dom_c7.dom.try_borrow() {
                if let Some(node) = find_node_by_id(&dom.root, &id) {
                    if let Some(el_id) = node.attributes.get("id") {
                        dom_c7.event_registry.register_click_handler(el_id, js_code);
                    }
                }
            }
            println!(
                "[QuickJS Console] Registered {} handler for node {}",
                event_type, id
            );
        },
    )
    .unwrap();
    global
        .set("__pwb_registerEventHandler", reg_event_fn)
        .unwrap();

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
                    node.attributes
                        .insert("display".to_string(), "none".to_string());
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
              text: String|
              -> bool {
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
    global
        .set("__pwb_appendTreeRow", append_tree_row_fn)
        .unwrap();

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
                    btn.attributes
                        .insert("background-color".to_string(), "#2d2d2d".to_string());
                    btn.attributes
                        .insert("color".to_string(), "#cccccc".to_string());
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
                    if root
                        .attributes
                        .get("display")
                        .map(|d| d == "none")
                        .unwrap_or(false)
                    {
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
            Function::new(ctx.clone(), move |id: String| -> String {
                format!("Element inspected: {}", id)
            })
            .unwrap(),
        )
        .unwrap();

    global
        .set(
            "__pwb_onDevtoolsOpen",
            Function::new(
                ctx.clone(),
                move |callback: Value| -> rquickjs::Result<()> {
                    if let Some(fn_val) = callback.as_function() {
                        let _: rquickjs::Result<()> = fn_val.call(());
                    }
                    Ok(())
                },
            )
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
                names
                    .iter()
                    .map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
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
            Function::new(
                ctx.clone(),
                move |name: String, props_json: String, parent_selector: String| -> String {
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
                    if let Some(parent) = find_node_by_id_mut(&mut dom.root, &parent_id.to_string())
                    {
                        parent.children.push(instance);
                        base.to_string()
                    } else {
                        String::new()
                    }
                },
            )
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
                bundler::apply_pss_to_dom(&mut dom.root, &css_text)
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
}
