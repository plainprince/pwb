//! Core types and the shared QuickJS context for `pwb-browser::js_runtime`.

use crate::dom::Dom;
use crate::event_handler::EventHandlerRegistry;
use crate::webapi::WebApiState;
use rquickjs::{Context, Runtime};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Errors raised while initializing or executing JavaScript.
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
    /// A URL the page wants to navigate to (set by `__pwb_navigate(url)` from
    /// JS event handlers). The pipeline checks this after event dispatch and
    /// restarts the browser with the new bundle.
    pub pending_navigate: Rc<RefCell<Option<String>>>,
    /// A connection link the page wants to dial (set by `__pwb_connect(link)`
    /// from the `nodes.plain` connect card). The pipeline performs the dial on
    /// its Tokio runtime and renders the outcome.
    pub pending_connect: Rc<RefCell<Option<String>>>,
    /// A chat message the page wants to send (set by
    /// `__pwb_chat_send(peer_hex, text)` from the `chat.plain` composer).
    /// The pipeline hands it to the chat service, which seals it with the
    /// networking layer's E2EE flag, then re-renders the page.
    pub pending_chat: Rc<RefCell<Option<(String, String)>>>,
    /// A public-room post the page wants to make (set by
    /// `__pwb_room_send(room, text)` from the `chat.plain?room=` composer).
    /// The pipeline hands it to the room service, which signs it as this
    /// identity and floods it, then re-renders the page.
    pub pending_room: Rc<RefCell<Option<(String, String)>>>,
    /// Web API state: `localStorage`/`sessionStorage` maps and detached DOM
    /// nodes (`document.createElement` before they are appended).
    pub web: WebApiState,
    /// Node ids to scroll to their bottom on the next redraw (the devtools
    /// console output auto-scrolls to the newest line).
    pub scroll_to_bottom: Rc<RefCell<Vec<usize>>>,
}

impl DomHandle {
    /// Drop every piece of page-scoped state before a top-level navigation:
    /// event handlers, focus/selection, pending navigation targets, timers and
    /// async queues, and detached DOM nodes. The JS globals are reset separately
    /// by [`reset_js_context`]; together they make a page's globals and handlers
    /// unable to affect the next page.
    pub fn reset_page_state(&mut self) {
        self.event_registry.clear();
        self.inspect_mode.store(false, Ordering::Relaxed);
        self.suppress_context_menu.store(false, Ordering::Relaxed);
        *self.selected_node_id.borrow_mut() = None;
        *self.focused_input.borrow_mut() = None;
        *self.pending_navigate.borrow_mut() = None;
        *self.pending_connect.borrow_mut() = None;
        *self.pending_chat.borrow_mut() = None;
        *self.pending_room.borrow_mut() = None;
        self.scroll_to_bottom.borrow_mut().clear();
        // A new WebApiState drops the old async queue, WebSocket outbound
        // queues, session storage, and detached nodes. Background threads that
        // still hold a clone deliver into the old queue, which the new page
        // never drains.
        self.web = WebApiState::new();
    }

    pub fn new(dom: Dom) -> Self {
        Self {
            dom: Rc::new(RefCell::new(dom)),
            event_registry: EventHandlerRegistry::new(),
            inspect_mode: Arc::new(AtomicBool::new(false)),
            suppress_context_menu: Arc::new(AtomicBool::new(false)),
            selected_node_id: Rc::new(RefCell::new(None)),
            focused_input: Rc::new(RefCell::new(None)),
            pending_navigate: Rc::new(RefCell::new(None)),
            pending_connect: Rc::new(RefCell::new(None)),
            pending_chat: Rc::new(RefCell::new(None)),
            pending_room: Rc::new(RefCell::new(None)),
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

/// Drop the shared page engine. The next `execute_script` creates a new
/// `Runtime`/`Context`, so globals, prototypes, and registered JS callbacks
/// from the previous page cannot leak into the next one.
///
/// This deliberately does not reset browser chrome state (devtools); callers
/// re-run the devtools script after mounting it on the new page.
pub(crate) fn reset_js_context() {
    JS_CONTEXT.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

pub(crate) fn get_or_create_context() -> Result<rquickjs::Context, JsError> {
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
