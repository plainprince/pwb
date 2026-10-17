//! Core types and the shared QuickJS context for `pwb-browser::js_runtime`.

use crate::dom::Dom;
use crate::event_handler::EventHandlerRegistry;
use crate::webapi::WebApiState;
use rquickjs::{Context, Runtime};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
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
