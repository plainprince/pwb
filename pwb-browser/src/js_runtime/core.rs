//! Core types and the shared QuickJS context for `pwb-browser::js_runtime`.

use crate::dom::Dom;
use crate::event_handler::EventHandlerRegistry;
use crate::webapi::WebApiState;
use rquickjs::{Context, Runtime};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Upper bound on the QuickJS heap for one page. A hostile script that
/// allocates without limit fails with an out-of-memory exception instead of
/// exhausting the host process's memory.
const JS_MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// Upper bound on the JS value stack (QuickJS's default is 256 KiB).
const JS_STACK_LIMIT: usize = 1 << 20;
/// Wall-clock budget for one script or event handler. QuickJS polls the
/// interrupt handler while executing, so `while (true) {}` cannot freeze the
/// browser forever.
#[cfg(not(test))]
const JS_EXECUTION_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(test)]
const JS_EXECUTION_TIMEOUT: Duration = Duration::from_secs(1);

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
    /// True when [`pending_navigate`](Self::pending_navigate) was set by page
    /// JavaScript (`__pwb_navigate`) rather than by a user click. Scripted
    /// navigation must not spend credits, so the pipeline asks for explicit
    /// confirmation instead of submitting a `register.plain` claim.
    pub scripted_navigate: Rc<Cell<bool>>,
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
    /// A blog entry the page wants to post (set by
    /// `__pwb_blog_post(title, text)` from the `blog.plain` composer). The
    /// pipeline hands it to the blog service, which signs it as this identity
    /// and floods it, then re-renders the page.
    pub pending_blog: Rc<RefCell<Option<(String, String)>>>,
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
        self.scripted_navigate.set(false);
        *self.pending_connect.borrow_mut() = None;
        *self.pending_chat.borrow_mut() = None;
        *self.pending_room.borrow_mut() = None;
        *self.pending_blog.borrow_mut() = None;
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
            scripted_navigate: Rc::new(Cell::new(false)),
            pending_connect: Rc::new(RefCell::new(None)),
            pending_chat: Rc::new(RefCell::new(None)),
            pending_room: Rc::new(RefCell::new(None)),
            pending_blog: Rc::new(RefCell::new(None)),
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
    /// Deadline for the script currently executing on this thread. `None`
    /// means no script is running, so the interrupt handler never fires.
    static JS_DEADLINE: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
}

/// Arms the execution deadline for as long as it is alive, and clears it on
/// drop (including when the script panics or returns an error).
pub(crate) struct ExecutionDeadline;

impl ExecutionDeadline {
    pub(crate) fn arm() -> Self {
        JS_DEADLINE.with(|deadline| deadline.set(Some(Instant::now() + JS_EXECUTION_TIMEOUT)));
        Self
    }
}

impl Drop for ExecutionDeadline {
    fn drop(&mut self) {
        JS_DEADLINE.with(|deadline| deadline.set(None));
    }
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
    // Rejections from the page being torn down must not surface on the next
    // page's console.
    super::errors::clear_rejections();
}

pub(crate) fn get_or_create_context() -> Result<rquickjs::Context, JsError> {
    JS_CONTEXT.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            let runtime = Runtime::new().map_err(|e| JsError::InitFailed(e.to_string()))?;
            runtime.set_memory_limit(JS_MEMORY_LIMIT);
            runtime.set_max_stack_size(JS_STACK_LIMIT);
            // Unhandled promise rejections would otherwise be silent.
            super::errors::install_rejection_tracker(&runtime);
            let deadline = JS_DEADLINE.with(Rc::clone);
            runtime.set_interrupt_handler(Some(Box::new(move || {
                deadline
                    .get()
                    .is_some_and(|deadline| Instant::now() >= deadline)
            })));
            let context =
                Context::full(&runtime).map_err(|e| JsError::InitFailed(e.to_string()))?;
            *borrow = Some(context);
        }
        Ok(borrow.as_ref().unwrap().clone())
    })
}
