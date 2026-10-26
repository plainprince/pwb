//! Per-session Web API state, async event queue, and detached DOM bookkeeping.

use crate::dom::DomNode;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// JS callbacks (timer callbacks, promise resolvers) are NOT stored here as
// Rust values — the JS wrapper keeps them in global registries
// (`__pwb_timers`, `__pwb_pending`) that persist across `execute_script`
// calls in the shared context. Rust only stores plain data (deadlines, ids),
// so nothing tied to a JS lifetime ever crosses a closure or thread boundary.

static NEXT_OP_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a fresh op id (fetch, WebSocket, timers).
pub fn next_op_id() -> u64 {
    NEXT_OP_ID.fetch_add(1, Ordering::Relaxed)
}

/// Plain-data results produced by background threads, drained on the main
/// thread by `js_runtime::poll_async_ops`. Async op ids are unique
/// process-wide, but the queues themselves are per-session (inside
/// [`WebApiState`]) so concurrent browser sessions or parallel tests never
/// steal each other's events.
pub enum AsyncEvent {
    Fetch {
        id: u64,
        result: Result<FetchResponse, String>,
    },
    Ws {
        id: u64,
        event: WsEvent,
    },
}

pub struct FetchResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub url: String,
}

#[derive(Debug)]
pub enum WsEvent {
    Open,
    Message(String),
    Error(String),
    Close,
}

pub(crate) enum WsOutbound {
    Send(String),
    Close,
}

/// Drain completed async operations for this session (main thread only).
pub fn drain_async(state: &WebApiState) -> Vec<AsyncEvent> {
    let mut q = state.async_queue.lock().unwrap();
    q.drain(..).collect()
}

/// Per-session browser state shared with the JS bindings.
#[derive(Clone)]
pub struct WebApiState {
    pub local_storage: Rc<RefCell<HashMap<String, String>>>,
    pub session_storage: Rc<RefCell<HashMap<String, String>>>,
    /// Where `localStorage` persists (set by the pipeline at startup). `None`
    /// keeps localStorage in-memory only.
    pub storage_path: Rc<RefCell<Option<PathBuf>>>,
    /// Detached DOM nodes (`document.createElement` before they are appended).
    pub detached: Rc<RefCell<HashMap<usize, DomNode>>>,
    /// Completed async operations (fetch/WebSocket) from background threads,
    /// drained by `js_runtime::poll_async_ops`. Per-session so parallel tests
    /// or multiple windows never steal each other's events.
    pub async_queue: Arc<Mutex<VecDeque<AsyncEvent>>>,
    /// Outbound WebSocket messages waiting for each connection's thread.
    pub(crate) ws_outbound: Arc<Mutex<HashMap<u64, VecDeque<WsOutbound>>>>,
    local_loaded: Rc<Cell<bool>>,
}

impl WebApiState {
    pub fn new() -> Self {
        Self {
            local_storage: Rc::new(RefCell::new(HashMap::new())),
            session_storage: Rc::new(RefCell::new(HashMap::new())),
            storage_path: Rc::new(RefCell::new(None)),
            detached: Rc::new(RefCell::new(HashMap::new())),
            async_queue: Arc::new(Mutex::new(VecDeque::new())),
            ws_outbound: Arc::new(Mutex::new(HashMap::new())),
            local_loaded: Rc::new(Cell::new(false)),
        }
    }

    pub(super) fn load_local(&self) {
        if self.local_loaded.get() {
            return;
        }
        self.local_loaded.set(true);
        let path = self.storage_path.borrow().clone();
        if let Some(path) = path {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) {
                    *self.local_storage.borrow_mut() = map;
                }
            }
        }
    }

    pub(super) fn save_local(&self) {
        let path = self.storage_path.borrow().clone();
        if let Some(path) = path {
            let map = self.local_storage.borrow();
            if let Ok(text) = serde_json::to_string_pretty(&*map) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, text);
            }
        }
    }
}

impl Default for WebApiState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Detached DOM nodes (document.createElement / appendChild / removeChild)
// ---------------------------------------------------------------------------

/// Next free node id above the current tree maximum (also avoiding detached ids).
pub fn next_free_id(state: &WebApiState, tree_max: usize) -> usize {
    let mut id = tree_max + 1;
    loop {
        if !state.detached.borrow().contains_key(&id) {
            return id;
        }
        id += 1;
    }
}

pub fn add_detached(state: &WebApiState, id: usize, node: DomNode) {
    state.detached.borrow_mut().insert(id, node);
}

pub fn take_detached(state: &WebApiState, id: usize) -> Option<DomNode> {
    state.detached.borrow_mut().remove(&id)
}

pub fn put_detached(state: &WebApiState, node: DomNode) {
    state.detached.borrow_mut().insert(node.id, node);
}
