use super::dom_utils::{find_node_by_id, find_node_by_selector};
use super::*;
use crate::dom::{Dom, DomNode};
use pwb_core::spec::Tag;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

fn first_node_with_tag(node: &DomNode, tag: Tag) -> Option<usize> {
    if node.tag == tag {
        return Some(node.id);
    }
    node.children
        .iter()
        .find_map(|c| first_node_with_tag(c, tag))
}

#[test]
fn test_pwb_connect_records_pending_link() {
    let handle = test_handle();
    let link = "plain://nodes.plain?connect=127.0.0.1:9000/";
    execute_script(
        &handle,
        &format!("__pwb_connect('{link}0000000000000000000000000000000000000000000000000000000000000000');"),
    )
    .expect("script runs");
    assert_eq!(
        handle.pending_connect.borrow().as_deref(),
        Some("plain://nodes.plain?connect=127.0.0.1:9000/0000000000000000000000000000000000000000000000000000000000000000")
    );
}

#[test]
fn test_pwb_chat_send_records_pending_message() {
    let handle = test_handle();
    let peer = "0000000000000000000000000000000000000000000000000000000000000001";
    execute_script(&handle, &format!("__pwb_chat_send('{peer}', 'hi there');"))
        .expect("script runs");
    assert_eq!(
        handle.pending_chat.borrow().clone(),
        Some((peer.to_string(), "hi there".to_string())),
        "the composer action reaches the pipeline as (peer, text)"
    );
}

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
    let devtools_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
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
    // Mount real devtools + the real demo.plain DOM and populate the tree.
    let devtools_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
    let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
    let mut devtools_dom = Dom::from_bundle(&bundle);
    devtools_dom
        .root
        .attributes
        .insert("data-devtools".to_string(), "root".to_string());
    let demo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/demo.plain");
    let demo_bundle = crate::bundler::bundle_from_dir(&demo_dir).expect("demo bundle");
    let mut main = Dom::from_bundle(&demo_bundle);
    let title_id = first_node_with_tag(&main.root, Tag::Title).expect("demo title");
    let head_id = first_node_with_tag(&main.root, Tag::Head).expect("demo head");
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
    assert_eq!(
        title_open.1.and_then(|v| v.parse::<usize>().ok()),
        Some(title_id),
        "leaf rows carry data-node"
    );
    let head_close = rows
        .iter()
        .find(|(_, _, p)| p.contains(&"  </head>"))
        .expect("head close row");
    assert_eq!(
        head_close.1.and_then(|v| v.parse::<usize>().ok()),
        Some(head_id),
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
        title_row.2.iter().any(|t| t.starts_with(" Plain")),
        "leaf row keeps content with leading space: {:?}",
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
    const LABEL: &str = "This is an extremely long button label that would overflow the tree row";
    let handle = test_handle();
    // Node 2 is the button with text "Click me"; make its text very long.
    execute_script(&handle, &format!("__pwb_setText('2', '{LABEL}');")).unwrap();
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
    // Content keeps its leading space and is truncated with an ellipsis. How
    // many characters fit is decided by the host's font metrics (a machine
    // with no usable system font measures with the fallback advance), so
    // assert the shape of a valid truncation instead of a fixed prefix length.
    let content = parts.iter().find(|p| p.ends_with('…')).unwrap();
    assert!(
        content.starts_with(' '),
        "content preserved with leading space: {:?}",
        content
    );
    // How much survives is up to the font, but whatever survives must be a
    // prefix of the source text — the row never invents characters.
    let kept = content
        .strip_prefix(' ')
        .and_then(|c| c.strip_suffix('…'))
        .expect("content is a space, some text, then an ellipsis");
    assert!(
        LABEL.starts_with(kept),
        "truncated content is a prefix of the source text: {:?}",
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
    let devtools_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
    let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
    let mut devtools_dom = Dom::from_bundle(&bundle);
    devtools_dom
        .root
        .attributes
        .insert("data-devtools".to_string(), "root".to_string());
    let mut main = Dom {
        title: "Demo".to_string(),
        root: DomNode {
            id: 1,
            tag: Tag::View,
            attributes: HashMap::new(),
            text: None,
            children: vec![],
        },
        components: HashMap::new(),
        modules: HashMap::new(),
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
    let devtools_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
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
    let devtools_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
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
            let (r, g, b) = (p.red() as i32, p.green() as i32, p.blue() as i32);
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
                        attributes: HashMap::from([("id".to_string(), "styles-panel".to_string())]),
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
        // The browser enforces CORS on cross-origin fetches by default, so
        // the fixture blesses its own response.
        let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
    // A listener that accepts and hangs up without answering is a transport
    // error everywhere. Connecting to a closed port is not: Unix refuses it
    // instantly, while Windows can leave the SYN unanswered for seconds,
    // which outlasts this test's pump loop and was red on the Windows job.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            drop(stream);
        }
    });

    let handle = test_handle();
    let url = format!("http://127.0.0.1:{}/nothing", port);
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

#[test]
fn scratch_hit_elements_tree_rows() {
    let devtools_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
    let bundle = crate::bundler::bundle_from_dir(&devtools_dir).expect("devtools bundle");
    let mut devtools_dom = Dom::from_bundle(&bundle);
    devtools_dom
        .root
        .attributes
        .insert("data-devtools".to_string(), "root".to_string());
    let demo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/demo.plain");
    let demo_bundle = crate::bundler::bundle_from_dir(&demo_dir).expect("demo bundle");
    let mut main = Dom::from_bundle(&demo_bundle);
    main.add_child(devtools_dom.root);
    let handle = DomHandle::new(main);
    execute_script(&handle, "__pwb_refreshDevtools(1800, 1300);").unwrap();

    let dom = handle.dom.borrow();
    let tree_id = crate::dom::find_node_by_id_ref(&dom.root, "elements-tree")
        .expect("elements tree node")
        .id;
    let layout_root = crate::layout::build_layout_tree(&dom, 1800, 1300, 0.0);
    let tree = crate::layout::find_box_by_id(&layout_root, tree_id).expect("elements tree box");
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
        let demo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/demo.plain");
        let demo_bundle = crate::bundler::bundle_from_dir(&demo_dir).expect("demo bundle");
        let mut main = Dom::from_bundle(&demo_bundle);
        main.add_child(devtools_dom.root);
        let handle = DomHandle::new(main);
        execute_script(&handle, &format!("__pwb_refreshDevtools({}, {});", vw, vh)).unwrap();

        let dom = handle.dom.borrow();
        let tree_id = crate::dom::find_node_by_id_ref(&dom.root, "elements-tree")
            .expect("elements tree node")
            .id;
        let layout_root = crate::layout::build_layout_tree(&dom, vw, vh, 0.0);
        let scrolls = std::collections::HashMap::new();
        let tree = crate::layout::find_box_by_id(&layout_root, tree_id).expect("elements tree box");
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
