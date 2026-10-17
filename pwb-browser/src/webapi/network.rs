//! Background network operations: `fetch` (ureq) and `WebSocket` (tungstenite).

use crate::webapi::state::{
    next_op_id, AsyncEvent, FetchResponse, WebApiState, WsEvent, WsOutbound,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

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
pub(super) fn cors_validate(url: &str, headers: &[(String, String)]) -> Result<(), String> {
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
