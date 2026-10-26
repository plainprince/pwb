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
    // CORS is enforced unless the build explicitly opts out for local
    // development (`--features permissive-cors`).
    #[cfg(not(feature = "permissive-cors"))]
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

/// CORS check. The page is a local bundle, so `file:`/relative URLs are
/// always same-origin; any http(s) request is treated as cross-origin and must
/// be blessed by the response's `Access-Control-Allow-Origin: *` header.
/// Build with `--features permissive-cors` to disable the check during local
/// development.
#[cfg(not(feature = "permissive-cors"))]
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
        Err(
            "CORS: cross-origin fetch blocked (response lacks Access-Control-Allow-Origin: *)."
                .to_string(),
        )
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

#[cfg(all(test, not(feature = "permissive-cors")))]
mod tests {
    use super::perform_fetch;
    use super::{drain_outbound, spawn_fetch, ws_close, ws_open, ws_send};
    use crate::webapi::state::{drain_async, AsyncEvent, WebApiState, WsEvent, WsOutbound};
    use std::collections::{HashMap, VecDeque};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Serve one HTTP response and return the bound address.
    fn serve_once(extra_headers: &'static str) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let body = "ok";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                body.len(),
                extra_headers,
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        addr
    }

    /// Drain the session queue until `pred` sees the events it wants (or fail
    /// after a bounded wait), returning everything drained so far.
    fn wait_for_events(
        state: &WebApiState,
        mut pred: impl FnMut(&[AsyncEvent]) -> bool,
    ) -> Vec<AsyncEvent> {
        let mut log = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            log.extend(drain_async(state));
            if pred(&log) {
                return log;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for async events"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn ws_events(events: &[AsyncEvent], id: u64) -> Vec<&WsEvent> {
        events
            .iter()
            .filter_map(|e| match e {
                AsyncEvent::Ws { id: i, event } if *i == id => Some(event),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn default_build_blocks_cross_origin_response_without_cors() {
        let addr = serve_once("");
        let error = match perform_fetch(&format!("http://{addr}/data"), "GET", &[], "") {
            Ok(_) => panic!("a missing Access-Control-Allow-Origin must be rejected"),
            Err(error) => error,
        };
        assert!(error.starts_with("CORS:"), "got {error}");
    }

    #[test]
    fn default_build_allows_a_blessed_cross_origin_response() {
        let addr = serve_once("Access-Control-Allow-Origin: *\r\n");
        let response = perform_fetch(&format!("http://{addr}/data"), "GET", &[], "")
            .expect("ACAO:* allows the response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"ok");
    }

    #[test]
    fn spawn_fetch_delivers_its_result_to_the_session_queue() {
        let addr = serve_once("Access-Control-Allow-Origin: *\r\n");
        let state = WebApiState::new();
        spawn_fetch(
            &state,
            42,
            format!("http://{addr}/data"),
            "GET".to_string(),
            Vec::new(),
            String::new(),
        );
        let events = wait_for_events(&state, |ev| {
            ev.iter()
                .any(|e| matches!(e, AsyncEvent::Fetch { id: 42, .. }))
        });
        match &events[0] {
            AsyncEvent::Fetch { id, result } => {
                assert_eq!(*id, 42);
                let response = result.as_ref().expect("blessed response succeeds");
                assert_eq!(response.status, 200);
                assert_eq!(response.body, b"ok");
                assert_eq!(response.url, format!("http://{addr}/data"));
            }
            other => panic!(
                "expected a fetch event, got something else: {:?}",
                other as *const _
            ),
        }
    }

    #[test]
    fn ws_send_and_ws_close_queue_outbound_messages() {
        let state = WebApiState::new();
        ws_send(&state, 5, "hello");
        ws_close(&state, 5);
        let mut guard = state.ws_outbound.lock().unwrap();
        let queued = guard
            .get_mut(&5)
            .expect("outbound queue for the connection");
        assert_eq!(queued.len(), 2);
        assert!(matches!(&queued[0], WsOutbound::Send(data) if data == "hello"));
        assert!(matches!(queued[1], WsOutbound::Close));
    }

    /// A connected socket pair wrapped as raw websockets (no handshake): the
    /// outbound queue must flush to the wire and a close must stop the loop.
    #[test]
    fn drain_outbound_flushes_sends_and_reports_close() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        let mut ws = tungstenite::WebSocket::from_raw_socket(
            tungstenite::stream::MaybeTlsStream::Plain(client),
            tungstenite::protocol::Role::Client,
            None,
        );
        let mut peer_ws = tungstenite::WebSocket::from_raw_socket(
            peer,
            tungstenite::protocol::Role::Server,
            None,
        );

        let outbound = Mutex::new(HashMap::from([(
            7u64,
            VecDeque::from([WsOutbound::Send("hi".to_string())]),
        )]));
        let queue = Mutex::new(VecDeque::new());
        assert!(
            !drain_outbound(&outbound, &queue, &mut ws, 7),
            "a queued send does not stop the reader"
        );
        match peer_ws.read() {
            Ok(tungstenite::Message::Text(text)) => assert_eq!(text, "hi"),
            other => panic!("peer receives the queued frame, got {other:?}"),
        }

        // A close drains the remaining sends, reports `Close`, and stops the loop.
        outbound.lock().unwrap().insert(
            7,
            VecDeque::from([WsOutbound::Send("bye".to_string()), WsOutbound::Close]),
        );
        assert!(drain_outbound(&outbound, &queue, &mut ws, 7));
        match peer_ws.read() {
            Ok(tungstenite::Message::Text(text)) => assert_eq!(text, "bye"),
            other => panic!("remaining send flushes before close, got {other:?}"),
        }
        let events: Vec<AsyncEvent> = queue.lock().unwrap().drain(..).collect();
        assert!(
            events.iter().any(|e| matches!(
                e,
                AsyncEvent::Ws {
                    id: 7,
                    event: WsEvent::Close
                }
            )),
            "close is reported to the JS side"
        );
    }

    /// Connect failure: the JS side sees an error and then a close, never a
    /// hung promise.
    #[test]
    fn ws_open_reports_a_failed_connect_as_error_then_close() {
        // Bind and immediately release a port so nothing listens on it.
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let state = WebApiState::new();
        let id = ws_open(&state, &format!("ws://{addr}/"));
        let events = wait_for_events(&state, |ev| {
            ev.iter()
                .any(|e| matches!(e, AsyncEvent::Ws { id: i, event: WsEvent::Close } if *i == id))
        });
        let seen = ws_events(&events, id);
        assert!(
            matches!(seen.first(), Some(WsEvent::Error(_))),
            "error comes first: {seen:?}"
        );
        assert!(
            matches!(seen.last(), Some(WsEvent::Close)),
            "close follows the error: {seen:?}"
        );
    }

    /// Full happy path: handshake, open event, send → server echo → message
    /// event, then close is delivered to both sides.
    #[test]
    fn ws_open_roundtrips_messages_and_close() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut server = match tungstenite::accept(stream) {
                Ok(server) => server,
                Err(error) => {
                    let _ = tx.send(format!("handshake failed: {error}"));
                    return;
                }
            };
            loop {
                match server.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        let _ = tx.send(format!("recv:{text}"));
                        let _ = server.send(tungstenite::Message::Text(format!("echo:{text}")));
                    }
                    Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
            let _ = tx.send("done".to_string());
        });

        let state = WebApiState::new();
        let id = ws_open(&state, &format!("ws://{addr}/"));
        let open_events = wait_for_events(&state, |ev| {
            ev.iter()
                .any(|e| matches!(e, AsyncEvent::Ws { id: i, event: WsEvent::Open } if *i == id))
        });
        assert!(
            open_events
                .iter()
                .any(|e| matches!(e, AsyncEvent::Ws { id: i, event: WsEvent::Open } if *i == id)),
            "open was reported"
        );

        ws_send(&state, id, "ping");
        wait_for_events(&state, |ev| {
            ev.iter().any(|e| {
                matches!(e, AsyncEvent::Ws { id: i, event: WsEvent::Message(m) } if *i == id && m == "echo:ping")
            })
        });

        ws_close(&state, id);
        let close_events = wait_for_events(&state, |ev| {
            ev.iter()
                .any(|e| matches!(e, AsyncEvent::Ws { id: i, event: WsEvent::Close } if *i == id))
        });
        assert!(
            close_events
                .iter()
                .any(|e| matches!(e, AsyncEvent::Ws { id: i, event: WsEvent::Close } if *i == id)),
            "close was reported"
        );

        // The server saw the client's frame and its close frame.
        let mut seen = Vec::new();
        while let Ok(msg) = rx.recv_timeout(Duration::from_secs(2)) {
            let done = msg == "done";
            seen.push(msg);
            if done {
                break;
            }
        }
        assert!(
            seen.iter().any(|m| m == "recv:ping"),
            "server received the send: {seen:?}"
        );
    }
}
