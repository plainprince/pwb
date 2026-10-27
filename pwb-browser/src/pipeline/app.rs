//! Browser application state and the winit event handler.

use super::devtools::{refresh_devtools, DevToolsState};
use super::dom::{
    ancestor_node_ids, compute_selected_text, element_node_id, find_node_by_id,
    find_node_with_attr, hovered_title, node_id_attr, node_in_subtree, TOOLTIP_DELAY,
};
use super::resize::{resize_bounds, resize_new_dimensions, ResizeDrag};
use crate::context_menu;
use crate::dom::Dom;
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
use crate::resolver::{
    content_id_of, is_chat_target, is_register_target, is_system_target, pwb_name_of,
    register_claim_of, resolve_url_or_file_with_context, room_of, search_query_of,
};
use crate::shell::{self, ChromeAction, Shell};
use arboard::Clipboard;
use pwb_core::spec::Tag;
use pwb_node::chat::{ChatRoute, ChatService};
use pwb_node::domain::{DomainRegistry, DomainService};
use pwb_node::federation::DistributedSearch;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::privacy::PrivacyMode;
use pwb_node::room::RoomService;
use pwb_node::search::SearchEngine;
use pwb_node::system::{
    render_message_bundle, render_search_pending_bundle, render_search_results_bundle,
    NetworkSnapshot,
};
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
    pub(super) menu_result: Arc<Mutex<u8>>,
    /// Long-lived system clipboard handle. Keeping it alive (instead of
    /// creating a `Clipboard` per operation and dropping it immediately) is
    /// what lets X11/Wayland clipboard managers read the contents on Linux.
    pub(super) clipboard: Option<Clipboard>,
    /// Open in-window context menu position (non-macOS): the menu is painted
    /// above the page and the next left click either selects an item or closes
    /// it.
    pub(super) inline_menu: Option<SelectionPoint>,
    /// True when the press that just closed the in-window menu must not turn
    /// into a page click on release.
    pub(super) swallow_click: bool,
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
    pub(super) registry: Arc<Mutex<DomainRegistry>>,
    pub(super) identity: Arc<IdentityManager>,
    /// Federated, DHT-sharded search service (shard routing + service receipts).
    pub(super) federation: Arc<DistributedSearch>,
    /// Domain-claim service backing `register.plain` and remote `.pwb` lookups.
    pub(super) domain: Arc<DomainService>,
    /// Direct E2EE chat service backing `chat.plain`: outbound sends are
    /// sealed for the peer (directly, or across an onion circuit when the
    /// browser runs in onion privacy mode), inbound messages land in
    /// per-peer transcripts.
    pub(super) chat: Arc<ChatService>,
    /// The chat version seen at the last render; a bump means a send echo, a
    /// recorded failure, or an inbound message — re-render `chat.plain`.
    pub(super) last_chat_version: u64,
    /// Public-room service backing `chat.plain?room=`: posts are signed as
    /// this identity and flooded to the network, membership is announced to
    /// peers, and history is fetched when a room is opened.
    pub(super) room: Arc<RoomService>,
    /// The room version seen at the last render; a bump means a post echo, a
    /// membership change, or an inbound post — re-render the room page.
    pub(super) last_room_version: u64,
    /// Tokio runtime handle used to spawn background shard-query tasks.
    pub(super) runtime: tokio::runtime::Handle,
    /// In-flight async page load (federated search results or remote `.pwb`
    /// resolution): delivers the rendered page bytes once the background task
    /// completes (polled from `about_to_wait`).
    pub(super) pending_page: Option<std::sync::mpsc::Receiver<(String, Vec<u8>)>>,
    /// Tabs, per-tab history, and bookmarks. The fixed chrome at the top of
    /// every page is rebuilt from this state.
    pub(super) shell: Shell,
    /// Whether remote lookups use onion circuits or the direct transport.
    pub(super) privacy: PrivacyMode,
}

/// How many top hits a federated search prefetches so "Visit Site" resolves
/// instantly from the local artifact store.
const SEARCH_PREFETCH: usize = 10;

/// Insert `text` at a character caret into a single-line value, returning the
/// new value and caret. Control characters are ignored.
pub(super) fn insert_text_at_caret(value: &str, caret: usize, text: &str) -> (String, usize) {
    let mut chars: Vec<char> = value.chars().collect();
    let mut caret = caret.min(chars.len());
    for ch in text.chars().filter(|ch| !ch.is_control()) {
        chars.insert(caret, ch);
        caret += 1;
    }
    (chars.into_iter().collect(), caret)
}

impl BrowserApp {
    /// The focused input as `(node_id, value, caret)`, if one is focused.
    fn focused_input_state(&self) -> Option<(usize, String, usize)> {
        let (node_id, caret) = (*self.dom_handle_render.focused_input.borrow())?;
        let dom = self.dom_handle_render.dom.borrow();
        let node = crate::dom::find_node_by_id_ref(&dom.root, &node_id.to_string())?;
        let value = node.attributes.get("value").cloned().unwrap_or_default();
        let caret = caret.min(value.chars().count());
        Some((node_id, value, caret))
    }

    fn set_input_value(&mut self, node_id: usize, value: String, caret: usize) {
        let mut dom = self.dom_handle_render.dom.borrow_mut();
        if let Some(node) = find_node_by_id(&mut dom.root, &node_id.to_string()) {
            if value.is_empty() {
                node.attributes.remove("value");
            } else {
                node.attributes.insert("value".to_string(), value);
            }
        }
        drop(dom);
        *self.dom_handle_render.focused_input.borrow_mut() = Some((node_id, caret));
    }

    /// Lazily open the system clipboard and keep it for the process lifetime.
    fn clipboard(&mut self) -> Option<&mut Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }

    fn copy_text_to_clipboard(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let Some(clipboard) = self.clipboard() else {
            eprintln!("[Clipboard] Failed to access clipboard");
            return false;
        };
        match clipboard.set_text(text.to_string()) {
            Ok(()) => {
                println!(
                    "[Clipboard] Copied {} chars to system clipboard",
                    text.chars().count()
                );
                true
            }
            Err(e) => {
                eprintln!("[Clipboard] Failed to write clipboard: {}", e);
                false
            }
        }
    }

    fn clipboard_text(&mut self) -> Option<String> {
        let clipboard = self.clipboard()?;
        match clipboard.get_text() {
            Ok(text) => Some(text),
            Err(arboard::Error::ContentNotAvailable) => None,
            Err(e) => {
                eprintln!("[Clipboard] Failed to read clipboard: {}", e);
                None
            }
        }
    }

    /// Copy the focused input's value, or the current text selection.
    fn copy_current_text(&mut self) -> bool {
        let text = if let Some((_, value, _)) = self.focused_input_state() {
            value
        } else {
            self.selection.selected_text_buffer.clone()
        };
        self.copy_text_to_clipboard(&text)
    }

    /// Cut the focused input's value, or delete the selected page text.
    fn cut_current_text(&mut self) -> bool {
        if let Some((node_id, value, _)) = self.focused_input_state() {
            if value.is_empty() || !self.copy_text_to_clipboard(&value) {
                return false;
            }
            self.set_input_value(node_id, String::new(), 0);
            return true;
        }

        let (Some(sn), Some(sc), Some(en), Some(ec)) = (
            self.selection.start_node_id,
            self.selection.start_char_pos,
            self.selection.end_node_id,
            self.selection.end_char_pos,
        ) else {
            return false;
        };
        let text = {
            let dom = self.dom_handle_render.dom.borrow();
            compute_selected_text(&dom.root, sn, sc, en, ec)
        };
        if text.is_empty() || !self.copy_text_to_clipboard(&text) {
            return false;
        }
        {
            let mut dom = self.dom_handle_render.dom.borrow_mut();
            super::dom::delete_selected_text(&mut dom.root, sn, sc, en, ec);
        }
        self.selection = TextSelection::default();
        true
    }

    /// Paste clipboard text into the focused `<input>` at the caret.
    fn paste_into_focused_input(&mut self) -> bool {
        if self.focused_input_state().is_none() {
            return false;
        }
        let Some(text) = self.clipboard_text() else {
            return false;
        };
        // `<input>` is single-line: flatten pasted newlines to spaces.
        let text = text.replace(['\r', '\n'], " ");
        let Some((node_id, value, caret)) = self.focused_input_state() else {
            return false;
        };
        let (new_value, new_caret) = insert_text_at_caret(&value, caret, &text);
        self.set_input_value(node_id, new_value, new_caret);
        true
    }

    /// Apply the action recorded by the context menu: `1` = Copy, `2` =
    /// Inspect Element (opens devtools and turns on pick mode). Called after
    /// the native menu closes and after an in-window menu item is clicked.
    fn apply_context_menu_choice(&mut self) {
        let choice = *self.menu_result.lock().unwrap();
        *self.menu_result.lock().unwrap() = 0;
        match choice {
            1 => {
                self.copy_current_text();
            }
            2 => {
                let mut dev = self.devtools_state.lock().unwrap();
                dev.is_open = true;
                drop(dev);
                if let Some(root_id) = self.devtools_root_id {
                    if let Ok(mut dom) = self.dom_handle_render.dom.try_borrow_mut() {
                        if let Some(node) = find_node_by_id(&mut dom.root, &root_id.to_string()) {
                            node.attributes.remove("display");
                        }
                    }
                }
                self.dom_handle_render
                    .inspect_mode
                    .store(true, Ordering::Relaxed);
                println!("[ContextMenu] DevTools opened, pick mode on");
            }
            _ => {}
        }
    }

    /// Hot-swap the DOM bundle for `url` within the same event loop: resolve
    /// the URL to a `.pwb` bundle, replace the DOM handle, re-run the page
    /// script, and re-mount devtools. Returns `true` on success.
    ///
    /// `register.plain` is handled by the domain service directly (render the
    /// live claim console, or apply a `?claim=…&cid=…` submission). A `.pwb`
    /// name that is not known locally returns `false` so the caller can fall
    /// back to [`BrowserApp::start_domain_resolve`].
    pub(super) fn navigate_to(&mut self, url: &str) -> bool {
        if is_register_target(url) {
            let bytes = match register_claim_of(url) {
                Some((domain, cid)) => self.domain.claim_and_render(&self.host, &domain, &cid),
                None => self.domain.render_view(&self.host, None, None),
            };
            if !self.install_bundle(&bytes) {
                return false;
            }
            println!("[Browser] Navigated to {}", url);
            return true;
        }

        // An open room is a membership: join before resolving so the page
        // renders with its roster and transcript in one go. The join is
        // idempotent and announces at most once per session, so re-rendering
        // this page does not spam the network.
        if let Some(room) = room_of(url) {
            if let Err(error) = self.room.join(&room) {
                eprintln!("[Room] Cannot join {room}: {error}");
            }
        }

        let bytes: Vec<u8> = {
            let registry = self.registry.lock().unwrap();
            let peers = self.domain.peers();
            let network = NetworkSnapshot {
                listen_addr: self.domain.listen_addr(),
                discovery_addr: self.domain.discovery_addr(),
                public_addr: self.domain.public_addr(),
                peers: peers.clone(),
            };
            match resolve_url_or_file_with_context(
                url,
                &self.search,
                &self.host,
                &registry,
                &self.identity,
                self.domain.available_credits(),
                &peers,
                network,
                self.chat.snapshot(),
                self.room.snapshot(),
            ) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[Browser] Navigation failed to resolve '{}': {:?}", url, e);
                    return false;
                }
            }
        };

        // Publishing a visited page broadcasts the bundle and the browser's
        // identity. That is useful for direct mode but defeats anonymous
        // browsing, so onion mode only caches the page locally (the fetch path
        // already does that).
        if self.privacy == PrivacyMode::Direct && !is_system_target(url) {
            self.federation.index_and_publish(&bytes);
        }

        if !self.install_bundle(&bytes) {
            return false;
        }
        println!("[Browser] Navigated to {}", url);
        true
    }

    /// Adopt the command-line target as the initial tab URL and draw the
    /// shell around the already-resolved page. Called once by `main` after the
    /// app is built (the initial bundle is resolved before the window exists).
    pub fn set_initial_url(&mut self, url: &str) {
        if self.shell.tabs.is_empty() {
            self.shell.new_tab(url);
        } else {
            self.shell.navigate(url);
        }
        self.refresh_chrome();
    }

    /// Rebuild the fixed chrome from the current shell state. Returns `false` when the DOM is
    /// borrowed by a running script, so callers can retry on the next tick.
    fn refresh_chrome(&mut self) -> bool {
        if let Ok(mut dom) = self.dom_handle_render.dom.try_borrow_mut() {
            shell::install_chrome(&mut dom, &self.shell);
            true
        } else {
            false
        }
    }

    /// Navigate to `url` and commit it to the active tab's history.
    pub(super) fn go(&mut self, url: &str) {
        let url = url.trim();
        if url.is_empty() {
            return;
        }
        // A bare word (or words) is a search, like a browser omnibox; a
        // dotted name, URI scheme, or path is treated as an address.
        let looks_like_address = url.contains('.') || url.contains(':') || url.contains('/');
        let canonical = if looks_like_address {
            url.to_string()
        } else {
            format!("search.plain?q={}", url)
        };
        self.shell.navigate(&canonical);
        self.refresh_chrome();
        self.load_url(&canonical);
    }

    /// Load a URL without touching history (back/forward/tab switches).
    pub(super) fn load_url(&mut self, url: &str) {
        match url {
            "bookmarks.plain" | "history.plain" => {
                let kind = if url == "history.plain" {
                    "history"
                } else {
                    "bookmarks"
                };
                let bytes = shell::list_page(&self.shell, kind);
                self.install_bundle(&bytes);
                return;
            }
            _ => {}
        }
        if let Some(query) = search_query_of(url) {
            self.start_federated_search(&query);
            return;
        }
        if self.navigate_to(url) {
            return;
        }
        if let Some(cid) = content_id_of(url) {
            self.start_content_fetch(cid);
            return;
        }
        if let Some(name) = pwb_name_of(url) {
            self.start_domain_resolve(&name);
            return;
        }
        let bytes = render_message_bundle(
            "cannot open page",
            &format!("PlainWeb could not resolve '{url}'."),
        )
        .serialize();
        self.install_bundle(&bytes);
    }

    /// Execute a chrome button click. Returns `false` for actions this shell
    /// does not implement.
    pub(super) fn handle_chrome_action(&mut self, action: ChromeAction) -> bool {
        match action {
            ChromeAction::Back => {
                if let Some(url) = self.shell.back() {
                    self.refresh_chrome();
                    self.load_url(&url);
                }
            }
            ChromeAction::Forward => {
                if let Some(url) = self.shell.forward() {
                    self.refresh_chrome();
                    self.load_url(&url);
                }
            }
            ChromeAction::Reload => {
                let url = self.shell.current_url().to_string();
                self.load_url(&url);
            }
            ChromeAction::NewTab => {
                self.shell.new_tab("welcome.plain");
                self.refresh_chrome();
                self.load_url("welcome.plain");
            }
            ChromeAction::SelectTab(index) => {
                if let Some(url) = self.shell.select(index) {
                    self.refresh_chrome();
                    self.load_url(&url);
                }
            }
            ChromeAction::ToggleBookmark => {
                let bookmarked = self.shell.toggle_bookmark();
                refresh_devtools(&self.dom_handle_render, self.window_size());
                println!(
                    "[Shell] {} {}",
                    if bookmarked {
                        "Bookmarked"
                    } else {
                        "Removed bookmark for"
                    },
                    self.shell.current_url()
                );
                self.refresh_chrome();
            }
            ChromeAction::OpenBookmarks => self.go("bookmarks.plain"),
            ChromeAction::OpenHistory => self.go("history.plain"),
            ChromeAction::OpenNetwork => self.go("network.plain"),
        }
        true
    }

    pub(super) fn window_size(&self) -> (u32, u32) {
        self.window
            .as_ref()
            .map(|window| {
                let size = window.inner_size();
                (size.width, size.height)
            })
            .unwrap_or((800, 600))
    }

    /// Focus the address input (Ctrl/Cmd+L).
    fn focus_address(&mut self) -> bool {
        let node_id = {
            let dom = self.dom_handle_render.dom.borrow();
            shell::address_node_id(&dom.root)
        };
        let Some(node_id) = node_id else {
            return false;
        };
        let len = {
            let dom = self.dom_handle_render.dom.borrow();
            crate::dom::find_node_by_id_ref(&dom.root, &node_id.to_string())
                .and_then(|node| node.attributes.get("value").cloned())
                .unwrap_or_default()
                .chars()
                .count()
        };
        *self.dom_handle_render.focused_input.borrow_mut() = Some((node_id, len));
        true
    }

    /// Read the value of an `<input>` node.
    fn input_value(&self, node_id: usize) -> String {
        let dom = self.dom_handle_render.dom.borrow();
        crate::dom::find_node_by_id_ref(&dom.root, &node_id.to_string())
            .and_then(|node| node.attributes.get("value").cloned())
            .unwrap_or_default()
    }

    /// Fetch a `content.pwb/<cid>` target from the swarm in the background.
    /// Search results render every hit, but only the first few are prefetched;
    /// clicking any other result must still load it on demand.
    pub(super) fn start_content_fetch(&mut self, cid: pwb_core::crypto::ContentId) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_page = Some(rx);

        let federation = Arc::clone(&self.federation);
        let privacy = self.privacy;
        let cid_hex = cid.to_hex();
        println!(
            "[Content] Fetching {cid_hex} from peers ({})…",
            if privacy == PrivacyMode::Onion {
                "onion"
            } else {
                "direct"
            }
        );
        self.runtime.spawn(async move {
            let bytes = match privacy {
                PrivacyMode::Onion => match federation.fetch_private(&cid).await {
                    Ok(bytes) => bytes,
                    Err(error) => render_message_bundle(
                        "content unavailable privately",
                        &format!(
                            "No onion circuit could fetch content {cid_hex}: {error}. Use --direct only if you accept revealing your address."
                        ),
                    )
                    .serialize(),
                },
                PrivacyMode::Direct => match federation.fetch(&cid).await {
                    Some(bytes) => bytes,
                    None => render_message_bundle(
                        "content unavailable",
                        &format!("No peer served content {cid_hex}."),
                    )
                    .serialize(),
                },
            };
            let _ = tx.send((format!("content.pwb/{cid_hex}"), bytes));
        });
    }

    /// Resolve a `.pwb` name from the swarm in the background: ask the domain
    /// service for the name, fetch its content bundle from peers, and deliver
    /// the rendered page bytes over the pending channel. On failure a native
    /// status page explains why.
    pub(super) fn start_domain_resolve(&mut self, name: &str) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_page = Some(rx);

        let service = Arc::clone(&self.domain);
        let federation = Arc::clone(&self.federation);
        let privacy = self.privacy;
        let name = name.to_string();
        println!(
            "[Domain] Asking the swarm for '{name}' ({})…",
            if privacy == PrivacyMode::Onion {
                "onion"
            } else {
                "direct"
            }
        );
        self.runtime.spawn(async move {
            let (record, failure) = match privacy {
                PrivacyMode::Onion => match service.resolve_private(&name).await {
                    Ok(Some(record)) => (Some(record), None),
                    Ok(None) => (None, None),
                    Err(error) => (None, Some(error.to_string())),
                },
                PrivacyMode::Direct => (service.resolve(&name).await, None),
            };
            let bytes = match record {
                Some(record) => {
                    let fetched = match privacy {
                        PrivacyMode::Onion => federation
                            .fetch_private(&record.target_content_id)
                            .await
                            .ok(),
                        PrivacyMode::Direct => federation.fetch(&record.target_content_id).await,
                    };
                    match fetched {
                        Some(bytes) => bytes,
                        None => render_message_bundle(
                            "content unavailable",
                            &format!(
                                "'{name}' resolved to {}, but no peer served its content{}.",
                                record.target_content_id.to_hex(),
                                if privacy == PrivacyMode::Onion {
                                    " through an onion circuit"
                                } else {
                                    ""
                                }
                            ),
                        )
                        .serialize(),
                    }
                }
                None => match failure {
                    Some(error) => render_message_bundle(
                        "private lookup failed",
                        &format!("No onion circuit could resolve '{name}': {error}."),
                    )
                    .serialize(),
                    None => render_message_bundle(
                        "domain not found",
                        &format!("No node on the swarm knows the domain '{name}'."),
                    )
                    .serialize(),
                },
            };
            let _ = tx.send((name, bytes));
        });
    }

    /// Install already-resolved `.pwb` bundle bytes as the live DOM: replace the
    /// DOM handle, set the storage path, re-run the page script, and update the
    /// window title. Returns `true` on success.
    pub(super) fn install_bundle(&mut self, bytes: &[u8]) -> bool {
        self.inline_menu = None;
        let bundle = match pwb_core::wire::Bundle::deserialize(bytes) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[Browser] Failed to deserialize bundle: {:?}", e);
                return false;
            }
        };

        // Replace the DOM in place (the JS context shares this DomHandle, so
        // it will see the new tree on the next script call), then mount the
        // shell chrome as a fixed overlay above the page.
        let url = self.shell.current_url().to_string();
        self.shell.set_title(&url, &bundle.title);
        let mut new_dom = Dom::from_bundle(&bundle);
        shell::install_chrome(&mut new_dom, &self.shell);
        *self.dom_handle_render.dom.borrow_mut() = new_dom;

        // Set localStorage path for the new bundle.
        {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            use std::hash::Hasher;
            hasher.write(bytes);
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
                "PlainWeb Browser — {} — {}",
                self.dom_handle_render.dom.borrow().title,
                self.shell.connection_label()
            ));
        }
        true
    }

    /// Start a federated, swarm-wide search: show a transient "searching" page
    /// immediately, then spawn a background task that routes the query across
    /// the sharded index, prefetches the top hits into the local store, and
    /// sends the rendered results page back over a channel for
    /// [`BrowserApp::about_to_wait`] to install.
    pub(super) fn start_federated_search(&mut self, query: &str) {
        if self.window.is_some() {
            let pending = render_search_pending_bundle(query).serialize();
            self.install_bundle(&pending);
        }

        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_page = Some(rx);

        let federation = Arc::clone(&self.federation);
        let privacy = self.privacy;
        let query = query.to_string();
        println!(
            "[Search] Federated query across the swarm ({}): '{query}'",
            if privacy == PrivacyMode::Onion {
                "onion"
            } else {
                "direct"
            }
        );
        self.runtime.spawn(async move {
            let bytes = match privacy {
                PrivacyMode::Onion => match federation.query_private(&query).await {
                    Ok(results) => {
                        println!(
                            "[Search] Merged {} result(s) through onion circuits.",
                            results.len()
                        );
                        render_search_results_bundle(&query, &results).serialize()
                    }
                    Err(error) => render_message_bundle(
                        "private search unavailable",
                        &format!(
                            "Could not search anonymously: {error}. The browser will not fall back to direct traffic; join more peers or pass --direct to accept revealing your address."
                        ),
                    )
                    .serialize(),
                },
                PrivacyMode::Direct => {
                    let results = federation.query(&query).await;
                    for result in results.iter().take(SEARCH_PREFETCH) {
                        let _ = federation.fetch(&result.content_id).await;
                    }
                    println!(
                        "[Search] Merged {} result(s) from the sharded index.",
                        results.len()
                    );
                    render_search_results_bundle(&query, &results).serialize()
                }
            };
            let _ = tx.send((format!("search.plain?q={query}"), bytes));
        });
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
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 650.0))
            // The chrome bar is a fixed-height toolbar; below this width the
            // address field and its controls stop fitting on one row.
            .with_min_inner_size(winit::dpi::LogicalSize::new(760.0, 480.0));

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
        // Connection badge: the chrome is a static tree, so mirror the live
        // peer count into it (and the window title) when it changes. Waking
        // once a second keeps an idle window honest; later branches below can
        // shorten the wait for pending work.
        let peers = self.domain.peers().len();
        if peers != self.shell.peers {
            let previous = self.shell.peers;
            self.shell.peers = peers;
            if !self.refresh_chrome() {
                // A script holds the DOM right now; try again on the next tick.
                self.shell.peers = previous;
            } else if let Some(w) = self.window.as_ref() {
                w.set_title(&format!(
                    "PlainWeb Browser — {} — {}",
                    self.dom_handle_render.dom.borrow().title,
                    self.shell.connection_label()
                ));
                w.request_redraw();
            }
        }
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
        // Wake at least once a second so the connection badge follows the live
        // peer count even in an idle window; a sooner timer still wins.
        let badge_wake = std::time::Instant::now() + std::time::Duration::from_secs(1);
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            crate::webapi::next_timer_deadline()
                .map_or(badge_wake, |deadline| deadline.min(badge_wake)),
        ));

        // In-place navigation swap: when a JS handler set pending_navigate on
        // the DomHandle, resolve the URL and swap the DOM bundle without
        // recreating the winit EventLoop (which cannot be recreated in the
        // same process).
        let pending_url = self.dom_handle_render.pending_navigate.borrow_mut().take();
        if let Some(url) = pending_url {
            // `go` records the navigation in the active tab, refreshes the
            // chrome, and routes system pages / searches / remote content.
            self.go(&url);
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }

        // `nodes.plain` connect card action: dial the peer and render the
        // outcome. The dial probes with Ping/Pong before reporting success.
        let pending_connect = self.dom_handle_render.pending_connect.borrow_mut().take();
        if let Some(link) = pending_connect {
            let (tx, rx) = std::sync::mpsc::channel();
            self.pending_page = Some(rx);
            let domain = Arc::clone(&self.domain);
            self.runtime.spawn(async move {
                let bytes = match domain.dial_link(&link).await {
                    Ok(peer) => render_message_bundle(
                        "peer connected",
                        &format!(
                            "Connected to {} at {} after a Ping/Pong probe.",
                            peer.public_key, peer.address
                        ),
                    )
                    .serialize(),
                    Err(e) => render_message_bundle("connect failed", &e).serialize(),
                };
                let _ = tx.send(("nodes.plain".to_string(), bytes));
            });
        }

        // `chat.plain` composer action: hand the message to the chat
        // service, which seals it with the networking layer's E2EE flag and
        // records the outbound echo (or the failure). Then re-render the page
        // when the chat state changed — a send, a failure, or an inbound
        // message delivered while the page is on screen.
        let pending_chat = self.dom_handle_render.pending_chat.borrow_mut().take();
        let mut chat_refresh = pending_chat.is_some();
        if let Some((peer_hex, text)) = pending_chat {
            let parsed = hex::decode(peer_hex.trim()).ok().and_then(|bytes| {
                (bytes.len() == 32).then(|| {
                    let mut key = [0u8; 32];
                    key.copy_from_slice(&bytes);
                    pwb_core::crypto::PublicKey(key)
                })
            });
            match parsed {
                Some(peer) => {
                    // Chat follows the browser's privacy mode: onion mode
                    // routes through a circuit when one can be built (and
                    // records a visible note when it falls back), --direct
                    // stays on the direct transport.
                    let route = if self.privacy == PrivacyMode::Onion {
                        ChatRoute::Auto
                    } else {
                        ChatRoute::Direct
                    };
                    if let Err(error) = self.chat.send_via(&peer, &text, route) {
                        println!("[Chat] send failed: {error}");
                    }
                }
                None => {
                    println!("[Chat] ignoring send with a malformed peer key");
                }
            }
        }
        let chat_version = self.chat.version();
        if chat_version != self.last_chat_version {
            self.last_chat_version = chat_version;
            chat_refresh = true;
        }

        // `chat.plain?room=` composer action: sign the line as this identity
        // and flood it to every peer, which relays it on.
        let pending_room = self.dom_handle_render.pending_room.borrow_mut().take();
        let mut room_refresh = pending_room.is_some();
        if let Some((room, text)) = pending_room {
            if let Err(error) = self.room.post(&room, &text) {
                println!("[Room] post failed: {error}");
            }
        }
        let room_version = self.room.version();
        if room_version != self.last_room_version {
            self.last_room_version = room_version;
            room_refresh = true;
        }

        if (chat_refresh || room_refresh) && is_chat_target(self.shell.current_url()) {
            let url = self.shell.current_url().to_string();
            self.navigate_to(&url);
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }

        // Poll an in-flight async page load (federated search results or a
        // remote `.pwb` resolution); install the page as soon as it completes.
        if let Some(receiver) = self.pending_page.as_ref() {
            match receiver.try_recv() {
                Ok((url, bytes)) => {
                    self.pending_page = None;
                    // The async load knows its final URL (search results,
                    // content id, or resolved `.pwb` name); commit it now so
                    // history and the address bar reflect what is on screen.
                    self.shell.navigate(&url);
                    if self.window.is_some() {
                        self.install_bundle(&bytes);
                    } else {
                        self.refresh_chrome();
                    }
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(
                        std::time::Instant::now() + std::time::Duration::from_millis(30),
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_page = None;
                }
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
        // Owned handle so actions that need `&mut self` can run while the
        // window is in use (Rc clone is cheap).
        let Some(window) = self.window.clone() else {
            return;
        };
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
                // The in-window menu is modal: Escape closes it before any
                // page/input handling. (This must live in the first keyboard
                // arm; later arms for pressed keys are unreachable.)
                if self.inline_menu.is_some()
                    && matches!(
                        &key_event.logical_key,
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                    )
                {
                    self.inline_menu = None;
                    window.request_redraw();
                    return;
                }

                // Browser shell shortcuts: tabs, address bar, history, and
                // bookmarks. These run before page input/JS handling so a
                // focused page input cannot swallow Ctrl+L or Ctrl+T.
                if self.modifiers_state.alt_key()
                    && !(self.modifiers_state.control_key() || self.modifiers_state.super_key())
                {
                    if let PhysicalKey::Code(code) = key_event.physical_key {
                        match code {
                            KeyCode::ArrowLeft => {
                                if let Some(url) = self.shell.back() {
                                    self.refresh_chrome();
                                    self.load_url(&url);
                                }
                                window.request_redraw();
                                return;
                            }
                            KeyCode::ArrowRight => {
                                if let Some(url) = self.shell.forward() {
                                    self.refresh_chrome();
                                    self.load_url(&url);
                                }
                                window.request_redraw();
                                return;
                            }
                            _ => {}
                        }
                    }
                }
                if (self.modifiers_state.control_key() || self.modifiers_state.super_key())
                    && !self.modifiers_state.alt_key()
                {
                    if let PhysicalKey::Code(code) = key_event.physical_key {
                        match code {
                            KeyCode::KeyL => {
                                if self.focus_address() {
                                    window.request_redraw();
                                }
                                return;
                            }
                            KeyCode::KeyT => {
                                self.shell.new_tab("welcome.plain");
                                self.refresh_chrome();
                                self.load_url("welcome.plain");
                                window.request_redraw();
                                return;
                            }
                            KeyCode::KeyW => {
                                if let Some(url) = self.shell.close_active() {
                                    self.refresh_chrome();
                                    self.load_url(&url);
                                }
                                window.request_redraw();
                                return;
                            }
                            KeyCode::KeyD => {
                                let bookmarked = self.shell.toggle_bookmark();
                                println!(
                                    "[Shell] {} {}",
                                    if bookmarked {
                                        "Bookmarked"
                                    } else {
                                        "Removed bookmark for"
                                    },
                                    self.shell.current_url()
                                );
                                self.refresh_chrome();
                                window.request_redraw();
                                return;
                            }
                            KeyCode::KeyR => {
                                let url = self.shell.current_url().to_string();
                                self.load_url(&url);
                                window.request_redraw();
                                return;
                            }
                            _ => {}
                        }
                    }
                }

                // Clipboard shortcuts: Ctrl on Linux/Windows, Cmd on macOS.
                // Handled before the typing block (which intentionally skips
                // modifier combos) and before the F12/devtools combos, which
                // are matched below on their own key codes.
                if (self.modifiers_state.control_key() || self.modifiers_state.super_key())
                    && !self.modifiers_state.alt_key()
                {
                    // Match the physical key as well as the logical character:
                    // some backends report Ctrl+C/V/X as control characters.
                    let matches_key = |code: KeyCode, letter: &str| {
                        matches!(key_event.physical_key, PhysicalKey::Code(c) if c == code)
                            || matches!(
                                &key_event.logical_key,
                                winit::keyboard::Key::Character(c)
                                    if c.eq_ignore_ascii_case(letter)
                            )
                    };
                    let handled = if matches_key(KeyCode::KeyC, "c") {
                        self.copy_current_text()
                    } else if matches_key(KeyCode::KeyX, "x") {
                        self.cut_current_text()
                    } else if matches_key(KeyCode::KeyV, "v") {
                        self.paste_into_focused_input()
                    } else {
                        false
                    };
                    if handled {
                        let size = window.inner_size();
                        refresh_devtools(&self.dom_handle_render, (size.width, size.height));
                        window.request_redraw();
                        return;
                    }
                }

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
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Space) => {
                                let mut value: Vec<char> = {
                                    let dom = self.dom_handle_render.dom.borrow();
                                    crate::dom::find_node_by_id_ref(&dom.root, &node_id.to_string())
                                        .and_then(|n| n.attributes.get("value").cloned())
                                        .unwrap_or_default()
                                        .chars()
                                        .collect()
                                };
                                caret = caret.min(value.len());
                                value.insert(caret, ' ');
                                caret += 1;
                                new_value = Some(value.into_iter().collect());
                                changed = true;
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
                            // The address bar is shell chrome, not a page
                            // form: Enter navigates instead of dispatching a
                            // page "enter" handler.
                            let is_address = {
                                let dom = self.dom_handle_render.dom.borrow();
                                crate::dom::find_node_by_id_ref(&dom.root, &node_id.to_string())
                                    .and_then(|node| node.attributes.get("id").cloned())
                                    .map(|id| id == "__pwb_address")
                                    .unwrap_or(false)
                            };
                            if is_address {
                                let value = self.input_value(node_id);
                                *self.dom_handle_render.focused_input.borrow_mut() = None;
                                if !value.trim().is_empty() {
                                    self.go(&value);
                                }
                                window.request_redraw();
                                return;
                            }
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

                // The in-window context menu paints a hover highlight under the
                // pointer, so every move over it needs a repaint. Page hover
                // state (tooltips, cursors, inspect tint) stays frozen while
                // the menu owns the pointer.
                if self.inline_menu.is_some() {
                    window.set_cursor(CursorIcon::Default);
                    window.request_redraw();
                    return;
                }

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

                        // In-window context menu: a click selects an item or
                        // dismisses the menu, and never reaches the page.
                        if let Some(pos) = self.inline_menu.take() {
                            self.swallow_click = true;
                            let rect = context_menu::inline::menu_rect(
                                pos.x,
                                pos.y,
                                width as f32,
                                height as f32,
                            );
                            if let Some(item) = context_menu::inline::hit(rect, mx, my) {
                                match item {
                                    0 => context_menu::perform_copy(),
                                    1 => context_menu::perform_inspect(),
                                    _ => {}
                                }
                                self.apply_context_menu_choice();
                            }
                            window.request_redraw();
                            return;
                        }

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
                        // The press that closed the context menu must not also
                        // activate the page element underneath it.
                        if std::mem::take(&mut self.swallow_click) {
                            return;
                        }
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

                            // Shell chrome is above the page: a click on a tab,
                            // nav button, or bookmark toggle is handled here and
                            // never reaches the page's link/JS handlers.
                            let chrome = {
                                let dom = self.dom_handle_render.dom.borrow();
                                shell::chrome_action(&dom.root, node_id)
                            };
                            if let Some(action) = chrome {
                                self.handle_chrome_action(action);
                                window.request_redraw();
                                return;
                            }

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
                                // Native hyperlink navigation: a click on an `<a>`/Link
                                // (or on text inside it) follows its `href`, unless a JS
                                // click handler already requested a navigation.
                                if self.dom_handle_render.pending_navigate.borrow().is_none() {
                                    let href = {
                                        let dom = self.dom_handle_render.dom.borrow();
                                        super::dom::link_href(&dom.root, node_id)
                                    };
                                    if let Some(href) = href {
                                        println!("[Navigate] Link click → {}", href);
                                        *self.dom_handle_render.pending_navigate.borrow_mut() =
                                            Some(href);
                                    }
                                }

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
                    #[cfg(target_os = "macos")]
                    {
                        *self.menu_result.lock().unwrap() = 0;
                        context_menu::show_context_menu(
                            &window,
                            self.current_mouse_pos.x,
                            self.current_mouse_pos.y,
                        );
                        // The native menu runs a nested event loop; once it
                        // closes, apply the chosen action.
                        self.apply_context_menu_choice();
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        // winit has no portable native popup: paint the menu in
                        // the window and resolve the next click ourselves.
                        self.inline_menu = Some(self.current_mouse_pos);
                        println!("[ContextMenu] In-window menu opened");
                    }
                } else {
                    println!("[ContextMenu] Menu suppressed by preventDefault()");
                }
                // The handler may have mutated the DOM: refresh devtools.
                refresh_devtools(&self.dom_handle_render, (size.width, size.height));
                window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.inline_menu = None;
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

                    // In-window context menu paints last, above page content.
                    if let Some(pos) = self.inline_menu {
                        let rect = context_menu::inline::menu_rect(
                            pos.x,
                            pos.y,
                            width as f32,
                            height as f32,
                        );
                        context_menu::inline::draw(
                            &mut self.pixmap,
                            rect,
                            self.current_mouse_pos.x,
                            self.current_mouse_pos.y,
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
