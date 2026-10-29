//! Browser window entry point: bundle loading, devtools mount and event loop startup.
//!
//! The winit `EventLoop` is created exactly once per process (it cannot be
//! recreated). Navigation therefore hot-swaps the DOM bundle inside the same
//! event loop: `about_to_wait` checks `pending_navigate`, resolves the URL,
//! replaces the DOM handle, re-runs the page script, and keeps rendering.

use crate::bundler;
use std::path::Path;

use crate::context_menu::{self, ContextMenuActions};
use crate::dom::Dom;
use crate::js_runtime::{execute_script, DomHandle};
use crate::render::{SelectionPoint, TextSelection};
use pwb_core::wire::Bundle;
use pwb_node::chat::ChatService;
use pwb_node::domain::{DomainRegistry, DomainService};
use pwb_node::federation::DistributedSearch;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::privacy::PrivacyMode;
use pwb_node::room::RoomService;
use pwb_node::search::SearchEngine;

use std::sync::{Arc, Mutex};
use winit::event_loop::EventLoop;

use super::app::BrowserApp;
use super::devtools::DevToolsState;

#[derive(Debug)]
pub enum PipelineError {
    Wire(pwb_core::wire::WireError),
    Io(std::io::Error),
    Windowing(String),
}

/// Build a `BrowserApp` for `bundle_bytes` without running the event loop.
/// Callers (main.rs) own the `EventLoop` and drive `run_app` once; navigation
/// swaps the DOM in place via `BrowserApp::navigate_to`.
#[allow(clippy::too_many_arguments)]
pub fn build_browser_app(
    bundle_bytes: &[u8],
    search: Arc<SearchEngine>,
    host: Arc<ArtifactHost>,
    registry: Arc<Mutex<DomainRegistry>>,
    identity: Arc<IdentityManager>,
    federation: Arc<DistributedSearch>,
    domain: Arc<DomainService>,
    chat: Arc<ChatService>,
    room: Arc<RoomService>,
    privacy: PrivacyMode,
    runtime: tokio::runtime::Handle,
) -> Result<BrowserApp, PipelineError> {
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

    let mut initial_dom = Dom::from_bundle(&bundle);
    let mut shell = crate::shell::Shell::new("welcome.plain");
    // Start the badge from the live peer table so the first frame is truthful.
    shell.peers = domain.peers().len();
    crate::shell::install_chrome(&mut initial_dom, &shell);
    let dom_handle = DomHandle::new(initial_dom);

    // `localStorage` persists per bundle to `~/.pwb/<bundle-hash>.json`, keyed
    // by a short hash of the bundle bytes so different sites keep separate
    // storage. Override with `PWB_STORAGE_DIR` for tests.
    {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::Hasher;
        hasher.write(bundle_bytes);
        let hash = hasher.finish();
        let dir = std::env::var_os("PWB_STORAGE_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| pwb_core::paths::home_dir().map(|home| home.join(".pwb")))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        *dom_handle.web.storage_path.borrow_mut() = Some(dir.join(format!("{:016x}.json", hash)));
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

    // Result channel from the (static) menu actions back to the event-loop
    // handler: 0 = nothing, 1 = Copy, 2 = Inspect Element. The actions only
    // record what was chosen; the actual clipboard write happens on the event
    // loop, where the long-lived `Clipboard` handle lives.
    let menu_result: Arc<Mutex<u8>> = Arc::new(Mutex::new(0));

    let copy_result = menu_result.clone();
    let inspect_result = menu_result.clone();
    context_menu::set_actions(ContextMenuActions {
        on_copy: Box::new(move || {
            *copy_result.lock().unwrap() = 1;
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

    Ok(BrowserApp {
        window: None,
        context: None,
        surface: None,
        dom_handle_render: dom_handle,
        devtools_state,
        menu_result,
        clipboard: arboard::Clipboard::new().ok(),
        inline_menu: None,
        swallow_click: false,
        devtools_root_id,
        devtools_mounted,
        dump_png_path,
        scroll_y: 0.0,
        element_scrolls: std::collections::HashMap::new(),
        selection: TextSelection::default(),
        current_mouse_pos: SelectionPoint { x: 0.0, y: 0.0 },
        last_tooltip: None,
        hovered_title_candidate: None,
        tooltip_hover_since: None,
        modifiers_state: winit::keyboard::ModifiersState::default(),
        resize_state: None,
        pixmap: tiny_skia::Pixmap::new(1, 1).unwrap(),
        search,
        host,
        registry,
        identity,
        federation,
        domain,
        chat,
        // No sends yet: any version bump (including one that raced the
        // initial page render) triggers the first `chat.plain` refresh.
        last_chat_version: 0,
        room,
        last_room_version: 0,
        privacy,
        runtime,
        pending_page: None,
        shell,
    })
}

/// Run the browser event loop for `app`, handling in-place navigation swaps.
///
/// `about_to_wait` (in app.rs) checks `pending_navigate` and hot-swaps the DOM
/// bundle within the same event loop. This function returns only when the
/// window is closed.
pub fn run_event_loop(mut app: BrowserApp) -> Result<(), PipelineError> {
    let event_loop = EventLoop::new().map_err(|e| PipelineError::Windowing(e.to_string()))?;

    println!(
        "Opening interactive Skia desktop window (Press F12 or Cmd+Shift+I for devtools.plain)..."
    );
    event_loop
        .run_app(&mut app)
        .map_err(|e| PipelineError::Windowing(e.to_string()))?;

    Ok(())
}
