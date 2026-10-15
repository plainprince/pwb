//! Web API support (`pwb-browser::webapi`).
//!
//! Backing machinery for the browser JS APIs:
//! - `fetch` — HTTP on a background thread (ureq); the result is queued and
//!   resolved into the JS promise on the main thread by
//!   [`crate::js_runtime::poll_async_ops`]. CORS enforcement is compiled in
//!   via the `strict-cors` cargo feature (see [`cors_validate`]).
//! - `WebSocket` — one background thread per connection (tungstenite); inbound
//!   events are queued like fetch, outbound messages are pushed through a
//!   shared per-connection queue the thread drains.
//! - `setTimeout`/`setInterval` — a main-thread timer registry pumped by
//!   [`crate::js_runtime::pump_timers`] (the callbacks are `Persistent`
//!   JS functions, so they survive across `execute_script` calls).
//! - `localStorage`/`sessionStorage` — JSON maps; `localStorage` persists to a
//!   per-bundle file.
//! - `alert`/`confirm`/`prompt` — native dialogs on macOS (NSAlert), console
//!   fallbacks elsewhere.
//! - `crypto`/`btoa`/`atob`/`performance.now` — random bytes, UUIDs, base64,
//!   and a monotonic millisecond clock.

use crate::dom::DomNode;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// JS callbacks (timer callbacks, promise resolvers) are NOT stored here as
// Rust values — the JS wrapper keeps them in global registries
// (`__pwb_timers`, `__pwb_pending`) that persist across `execute_script`
// calls in the shared context. Rust only stores plain data (deadlines, ids),
// so nothing tied to a JS lifetime ever crosses a closure or thread boundary.

thread_local! {
    static TIMERS: RefCell<HashMap<u64, TimerEntry>> = RefCell::new(HashMap::new());
}

struct TimerEntry {
    interval_ms: Option<f64>,
    fire_at: Instant,
}

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

// ---------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------

/// Register a timeout (`interval == false`) or interval. The JS callback lives
/// in the `__pwb_timers` global registry; here we only track the deadline.
pub fn register_timer(id: u64, ms: f64, interval: bool) {
    let ms = ms.max(0.0);
    let delay = Duration::from_millis(ms as u64);
    TIMERS.with(|t| {
        t.borrow_mut().insert(
            id,
            TimerEntry {
                interval_ms: if interval { Some(ms) } else { None },
                fire_at: Instant::now() + delay,
            },
        );
    });
}

pub fn clear_timer(id: u64) {
    TIMERS.with(|t| {
        t.borrow_mut().remove(&id);
    });
}

/// Pop every timer that is due; returns `(id, interval_ms)`.
pub fn pop_due_timers() -> Vec<(u64, Option<f64>)> {
    let now = Instant::now();
    TIMERS.with(|t| {
        let mut timers = t.borrow_mut();
        let mut due = Vec::new();
        timers.retain(|id, entry| {
            if entry.fire_at <= now {
                due.push((*id, entry.interval_ms));
                false
            } else {
                true
            }
        });
        due
    })
}

/// Re-arm an interval timer after it fired.
pub fn reschedule_timer(id: u64, ms: f64) {
    register_timer(id, ms, true);
}

/// Earliest pending timer deadline, if any (used to wake the event loop).
pub fn next_timer_deadline() -> Option<Instant> {
    TIMERS.with(|t| t.borrow().values().map(|e| e.fire_at).min())
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

/// Spawn the HTTP request on a background thread; the result is queued for the
/// main thread to resolve into the JS promise.
pub fn spawn_fetch(
    state: &WebApiState,
    id: u64,
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: String,
) {
    let queue = state.async_queue.clone();
    std::thread::spawn(move || {
        let result = perform_fetch(&url, &method, &headers, &body);
        queue
            .lock()
            .unwrap()
            .push_back(AsyncEvent::Fetch { id, result });
    });
}

fn perform_fetch(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<FetchResponse, String> {
    let mut req = ureq::request(method, url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let resp = match method {
        "GET" | "HEAD" => req.call(),
        _ => req.send_string(body),
    };
    let resp = resp.map_err(|e| e.to_string())?;
    let status = resp.status();
    let status_text = resp.status_text().to_string();
    let mut out_headers = Vec::new();
    for name in resp.headers_names() {
        if let Some(v) = resp.header(&name) {
            out_headers.push((name, v.to_string()));
        }
    }
    // CORS enforcement is cfg-gated: `cargo build -p pwb-browser --features strict-cors`.
    #[cfg(feature = "strict-cors")]
    cors_validate(url, &out_headers)?;
    let body = resp.into_string().map_err(|e| e.to_string())?.into_bytes();
    Ok(FetchResponse {
        status,
        status_text,
        headers: out_headers,
        body,
        url: url.to_string(),
    })
}

/// CORS check (only compiled with the `strict-cors` feature). The page is a
/// local bundle, so `file:`/relative URLs are always same-origin; any http(s)
/// request is treated as cross-origin and must be blessed by the response's
/// `Access-Control-Allow-Origin: *` header.
#[cfg(feature = "strict-cors")]
fn cors_validate(url: &str, headers: &[(String, String)]) -> Result<(), String> {
    if url.starts_with("file:") || !url.contains("://") {
        return Ok(());
    }
    let allowed = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("access-control-allow-origin"))
        .map(|(_, v)| v.trim() == "*")
        .unwrap_or(false);
    if allowed {
        Ok(())
    } else {
        Err("CORS: cross-origin fetch blocked (response lacks Access-Control-Allow-Origin: *). Build with the `strict-cors` cargo feature to enforce this check; it is off by default.".to_string())
    }
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

/// Open a WebSocket on a background thread. Events (open/message/error/close)
/// are queued into the session's async queue; the JS side receives them via
/// `__pwb_wsOnEvent`.
pub fn ws_open(state: &WebApiState, url: &str) -> u64 {
    let id = next_op_id();
    let url = url.to_string();
    let queue = state.async_queue.clone();
    let outbound = state.ws_outbound.clone();
    std::thread::spawn(move || {
        let mut ws = match tungstenite::connect(&url) {
            Ok((ws, _)) => ws,
            Err(e) => {
                queue.lock().unwrap().push_back(AsyncEvent::Ws {
                    id,
                    event: WsEvent::Error(e.to_string()),
                });
                queue.lock().unwrap().push_back(AsyncEvent::Ws {
                    id,
                    event: WsEvent::Close,
                });
                return;
            }
        };
        // Poll the outbound queue while `read()` blocks on a short timeout so
        // `send()`/`close()` from the main thread are noticed promptly.
        set_read_timeout(ws.get_mut(), Duration::from_millis(50));
        queue.lock().unwrap().push_back(AsyncEvent::Ws {
            id,
            event: WsEvent::Open,
        });
        loop {
            if drain_outbound(&outbound, &queue, &mut ws, id) {
                break;
            }
            match ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    queue.lock().unwrap().push_back(AsyncEvent::Ws {
                        id,
                        event: WsEvent::Message(t.to_string()),
                    });
                }
                Ok(tungstenite::Message::Binary(b)) => {
                    queue.lock().unwrap().push_back(AsyncEvent::Ws {
                        id,
                        event: WsEvent::Message(String::from_utf8_lossy(&b).to_string()),
                    });
                }
                Ok(tungstenite::Message::Close(_)) => {
                    queue.lock().unwrap().push_back(AsyncEvent::Ws {
                        id,
                        event: WsEvent::Close,
                    });
                    break;
                }
                Ok(tungstenite::Message::Ping(_)) | Ok(tungstenite::Message::Pong(_)) => {
                    // tungstenite answers pings automatically; nothing to do.
                }
                Ok(tungstenite::Message::Frame(_)) => {}
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // Read timeout — loop again to check the outbound queue.
                }
                Err(e) => {
                    queue.lock().unwrap().push_back(AsyncEvent::Ws {
                        id,
                        event: WsEvent::Error(e.to_string()),
                    });
                    queue.lock().unwrap().push_back(AsyncEvent::Ws {
                        id,
                        event: WsEvent::Close,
                    });
                    break;
                }
            }
        }
    });
    id
}

/// Drain the connection's outbound queue; returns `true` when a `close()` was
/// requested (the reader loop should stop).
fn drain_outbound(
    outbound: &Mutex<HashMap<u64, VecDeque<WsOutbound>>>,
    queue: &Mutex<VecDeque<AsyncEvent>>,
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    id: u64,
) -> bool {
    let msgs: Vec<WsOutbound> = {
        let mut out = outbound.lock().unwrap();
        if let Some(q) = out.get_mut(&id) {
            q.drain(..).collect()
        } else {
            Vec::new()
        }
    };
    let mut closing = false;
    for msg in msgs {
        match msg {
            WsOutbound::Send(data) => {
                if let Err(e) = ws.send(tungstenite::Message::Text(data)) {
                    queue.lock().unwrap().push_back(AsyncEvent::Ws {
                        id,
                        event: WsEvent::Error(e.to_string()),
                    });
                }
            }
            WsOutbound::Close => {
                let _ = ws.close(None);
                closing = true;
            }
        }
    }
    if closing {
        // The close frame may or may not be echoed back as a read result;
        // report `close` ourselves so the JS side always sees it.
        queue.lock().unwrap().push_back(AsyncEvent::Ws {
            id,
            event: WsEvent::Close,
        });
    }
    closing
}

fn set_read_timeout(
    stream: &mut tungstenite::stream::MaybeTlsStream<std::net::TcpStream>,
    dur: Duration,
) {
    use tungstenite::stream::MaybeTlsStream;
    match stream {
        MaybeTlsStream::Plain(s) => {
            let _ = s.set_read_timeout(Some(dur));
        }
        MaybeTlsStream::Rustls(s) => {
            let _ = s.sock.set_read_timeout(Some(dur));
        }
        // Other TLS backends (native-tls etc.) fall through; the read loop
        // still works, it just can't be woken for outbound sends as promptly.
        _ => {}
    }
}

/// Queue a `send()` for the connection's background thread.
pub fn ws_send(state: &WebApiState, id: u64, data: &str) {
    state
        .ws_outbound
        .lock()
        .unwrap()
        .entry(id)
        .or_default()
        .push_back(WsOutbound::Send(data.to_string()));
}

/// Ask the connection's background thread to close the socket.
pub fn ws_close(state: &WebApiState, id: u64) {
    state
        .ws_outbound
        .lock()
        .unwrap()
        .entry(id)
        .or_default()
        .push_back(WsOutbound::Close);
}

// ---------------------------------------------------------------------------
// localStorage / sessionStorage
// ---------------------------------------------------------------------------

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

    fn load_local(&self) {
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

    fn save_local(&self) {
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

/// `kind` is `local` or `session`; `op` is `get`/`set`/`remove`/`clear`/`key`/`length`.
pub fn storage_op(state: &WebApiState, kind: &str, op: &str, key: &str, value: &str) -> String {
    let local = kind == "local";
    if local {
        state.load_local();
    }
    if local {
        let mut map = state.local_storage.borrow_mut();
        match op {
            "get" => map.get(key).cloned().unwrap_or_default(),
            "set" => {
                map.insert(key.to_string(), value.to_string());
                drop(map);
                state.save_local();
                String::new()
            }
            "remove" => {
                map.remove(key);
                drop(map);
                state.save_local();
                String::new()
            }
            "clear" => {
                map.clear();
                drop(map);
                state.save_local();
                String::new()
            }
            "key" => {
                let keys: Vec<String> = map.keys().cloned().collect();
                keys.get(key.parse::<usize>().unwrap_or(0))
                    .cloned()
                    .unwrap_or_default()
            }
            "length" => map.len().to_string(),
            _ => String::new(),
        }
    } else {
        let mut map = state.session_storage.borrow_mut();
        match op {
            "get" => map.get(key).cloned().unwrap_or_default(),
            "set" => {
                map.insert(key.to_string(), value.to_string());
                String::new()
            }
            "remove" => {
                map.remove(key);
                String::new()
            }
            "clear" => {
                map.clear();
                String::new()
            }
            "key" => {
                let keys: Vec<String> = map.keys().cloned().collect();
                keys.get(key.parse::<usize>().unwrap_or(0))
                    .cloned()
                    .unwrap_or_default()
            }
            "length" => map.len().to_string(),
            _ => String::new(),
        }
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

// ---------------------------------------------------------------------------
// alert / confirm / prompt
// ---------------------------------------------------------------------------

pub fn show_alert(msg: &str) {
    #[cfg(target_os = "macos")]
    macos::alert(msg);
    #[cfg(not(target_os = "macos"))]
    println!("[JS Alert] {}", msg);
}

pub fn show_confirm(msg: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        return macos::confirm(msg);
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!("[JS Confirm] {}", msg);
        true
    }
}

pub fn show_prompt(msg: &str, default_val: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        return macos::prompt(msg, default_val);
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!("[JS Prompt] {} (default: {})", msg, default_val);
        Some(default_val.to_string())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSTextField};
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

    pub fn alert(msg: &str) {
        unsafe {
            let mtm = MainThreadMarker::new().expect("alert must run on the main thread");
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(msg));
            alert.runModal();
        }
    }

    pub fn confirm(msg: &str) -> bool {
        unsafe {
            let mtm = MainThreadMarker::new().expect("confirm must run on the main thread");
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(msg));
            alert.addButtonWithTitle(&NSString::from_str("OK"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            alert.runModal() == NSAlertFirstButtonReturn
        }
    }

    pub fn prompt(msg: &str, default_val: &str) -> Option<String> {
        unsafe {
            let mtm = MainThreadMarker::new().expect("prompt must run on the main thread");
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(msg));
            alert.addButtonWithTitle(&NSString::from_str("OK"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            let field = NSTextField::new(mtm);
            field.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(220.0, 24.0),
            ));
            field.setStringValue(&NSString::from_str(default_val));
            alert.setAccessoryView(Some(&*field));
            if alert.runModal() == NSAlertFirstButtonReturn {
                let value: Retained<NSString> = field.stringValue();
                Some(value.to_string())
            } else {
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// crypto / base64 / performance
// ---------------------------------------------------------------------------

/// `n` random bytes as a Latin-1 string (each char code 0-255), so the JS side
/// can fill a Uint8Array without base64 round-trips.
pub fn random_bytes(n: u32) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; n as usize];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.into_iter().map(|b| b as char).collect()
}

pub fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// `btoa`: binary string (chars U+0000..=U+00FF) to base64.
pub fn btoa(s: &str) -> Result<String, String> {
    use base64::Engine;
    let bytes: Result<Vec<u8>, ()> = s
        .chars()
        .map(|c| {
            let cp = c as u32;
            if cp > 0xFF {
                Err(())
            } else {
                Ok(cp as u8)
            }
        })
        .collect();
    let bytes = bytes.map_err(|_| "Invalid character in binary string (must be Latin-1)")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// `atob`: base64 to a binary string (each char code 0-255).
pub fn atob(s: &str) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|_| "Invalid base64 string")?;
    Ok(bytes.into_iter().map(|b| b as char).collect())
}

static PERF_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the first call (monotonic, never goes backwards).
pub fn performance_now() -> f64 {
    let origin = *PERF_ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_round_trip() {
        assert_eq!(
            btoa("Hello, PlainWeb!").unwrap(),
            "SGVsbG8sIFBsYWluV2ViIQ=="
        );
        assert_eq!(
            atob("SGVsbG8sIFBsYWluV2ViIQ==").unwrap(),
            "Hello, PlainWeb!"
        );
        // Latin-1 chars (≤ U+00FF) are allowed; anything above must fail.
        assert_eq!(btoa("héllo").unwrap(), "aOlsbG8=");
        assert!(btoa("€10").is_err(), "char above U+00FF must fail btoa");
        assert!(atob("!!not-base64!!").is_err());
    }

    #[test]
    fn test_random_bytes_are_latin1() {
        let bytes = random_bytes(64);
        assert_eq!(bytes.chars().count(), 64);
        assert!(bytes.chars().all(|c| (c as u32) <= 0xFF));
        // Extremely unlikely to be all the same byte.
        let distinct = bytes
            .chars()
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert!(distinct > 1);
    }

    #[test]
    fn test_random_uuid_format() {
        let uuid = random_uuid();
        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        assert!(uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }

    #[test]
    fn test_performance_now_monotonic() {
        let a = performance_now();
        std::thread::sleep(Duration::from_millis(5));
        let b = performance_now();
        assert!(b >= a);
        assert!(b - a >= 1.0);
    }

    #[test]
    fn test_storage_ops() {
        let state = WebApiState::new();
        storage_op(&state, "local", "set", "k", "v");
        assert_eq!(storage_op(&state, "local", "get", "k", ""), "v");
        assert_eq!(storage_op(&state, "local", "length", "", ""), "1");
        assert_eq!(storage_op(&state, "local", "key", "0", ""), "k");
        storage_op(&state, "local", "remove", "k", "");
        assert_eq!(storage_op(&state, "local", "get", "k", ""), "");
        storage_op(&state, "local", "set", "a", "1");
        storage_op(&state, "local", "clear", "", "");
        assert_eq!(storage_op(&state, "local", "length", "", ""), "0");
        // sessionStorage is independent.
        storage_op(&state, "session", "set", "s", "x");
        assert_eq!(storage_op(&state, "session", "get", "s", ""), "x");
        assert_eq!(storage_op(&state, "local", "get", "s", ""), "");
    }

    #[test]
    fn test_detached_nodes() {
        let state = WebApiState::new();
        let node = DomNode {
            id: 1,
            tag: pwb_core::spec::Tag::View,
            attributes: HashMap::new(),
            text: None,
            children: Vec::new(),
        };
        add_detached(&state, 1, node);
        assert_eq!(next_free_id(&state, 5), 6);
        assert!(take_detached(&state, 1).is_some());
        assert!(take_detached(&state, 1).is_none());
    }

    #[cfg(feature = "strict-cors")]
    #[test]
    fn test_cors_validate() {
        assert!(cors_validate("file:///x/y.pwb", &[]).is_ok());
        assert!(cors_validate("/relative/path", &[]).is_ok());
        assert!(cors_validate("https://api.example.com/x", &[]).is_err());
        assert!(cors_validate(
            "https://api.example.com/x",
            &[("access-control-allow-origin".to_string(), "*".to_string())]
        )
        .is_ok());
    }
}
