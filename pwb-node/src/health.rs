//! HTTP health, readiness, and metrics endpoint for a public node.
//!
//! A node exposed to the public internet needs an out-of-band way for an
//! operator (or a supervisor) to ask "is this process healthy and what is it
//! doing?" without parsing log lines. This module serves a tiny read-only HTTP
//! API from a dedicated thread:
//!
//! * `GET /health` — `200` with `{"status":"ok", …}` while the engine is
//!   running, `503` with `{"status":"degraded"}` otherwise.
//! * `GET /status` — a JSON snapshot: listen/public addresses, peer count,
//!   index size, hosted artifacts, registry size, claim price, credits.
//! * `GET /metrics` — Prometheus text exposition (engine counters plus the
//!   node-level gauges).
//!
//! The listener is loopback by default and reads only a bounded request line;
//! it never exposes configuration, keys, or mutable operations.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pwb_core::net::NodeEngine;
use serde_json::json;

use crate::credit::CreditLedger;
use crate::domain::DomainRegistry;
use crate::host::ArtifactHost;
use crate::search::SearchEngine;

/// Maximum bytes read from one HTTP request (the parser only needs the request
/// line; anything longer is rejected rather than buffered).
const MAX_REQUEST_BYTES: usize = 8192;

/// Shared live state the endpoint reports on. All fields are handles to the
/// same objects the node runs with, so readings are live rather than copies.
pub struct HealthState {
    pub engine: NodeEngine,
    pub search: Arc<SearchEngine>,
    pub host: Arc<ArtifactHost>,
    pub registry: Arc<Mutex<DomainRegistry>>,
    pub ledger: Arc<CreditLedger>,
    pub started: Instant,
}

impl HealthState {
    /// Live JSON snapshot used by `/health` and `/status`.
    pub fn snapshot(&self) -> serde_json::Value {
        let peers = self.engine.peer_table().lock().unwrap().len();
        let (domains, claim_price) = {
            let registry = self.registry.lock().unwrap();
            (registry.count(), registry.claim_cost())
        };
        let started = self
            .engine
            .local_addr()
            .map(|result| result.is_ok())
            .unwrap_or(false);
        json!({
            "status": if started { "ok" } else { "degraded" },
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_secs": self.started.elapsed().as_secs(),
            "listen_addr": self.engine.local_addr().and_then(|r| r.ok()).map(|a| a.to_string()),
            "public_addr": self.engine.public_addr().map(|a| a.to_string()),
            "discovery_addr": self.engine.discovery_addr().map(|a| a.to_string()),
            "peers": peers,
            "index_documents": self.search.document_count(),
            "index_terms": self.search.term_count(),
            "artifacts_hosted": self.host.len(),
            "domains_registered": domains,
            "claim_price": claim_price,
            "credits_earned": self.ledger.earned(),
        })
    }

    /// Prometheus text: engine packet/message counters plus node gauges.
    pub fn prometheus(&self) -> String {
        let mut text = self.engine.metrics().prometheus();
        let snapshot = self.snapshot();
        let gauges = [
            (
                "pwb_peers_connected",
                snapshot["peers"].as_u64().unwrap_or(0),
            ),
            (
                "pwb_index_documents",
                snapshot["index_documents"].as_u64().unwrap_or(0),
            ),
            (
                "pwb_index_terms",
                snapshot["index_terms"].as_u64().unwrap_or(0),
            ),
            (
                "pwb_artifacts_hosted",
                snapshot["artifacts_hosted"].as_u64().unwrap_or(0),
            ),
            (
                "pwb_domains_registered",
                snapshot["domains_registered"].as_u64().unwrap_or(0),
            ),
            (
                "pwb_credits_earned",
                snapshot["credits_earned"].as_u64().unwrap_or(0),
            ),
        ];
        for (name, value) in gauges {
            text.push_str(&format!("# TYPE {name} gauge\n{name} {value}\n"));
        }
        text
    }
}

/// A running health endpoint. Dropping it (or calling [`HealthServer::stop`])
/// asks the accept loop to finish; the listener socket closes with the thread.
pub struct HealthServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
}

impl HealthServer {
    /// Bind and serve on a background thread. Returns once the socket is
    /// listening, so a bind failure is reported to the caller.
    pub fn spawn(addr: SocketAddr, state: Arc<HealthState>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_thread = Arc::clone(&shutdown);
        thread::Builder::new()
            .name("pwb-health".to_string())
            .spawn(move || {
                while !shutdown_for_thread.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = handle_connection(stream, &state);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(50)),
                    }
                }
            })?;
        Ok(Self {
            addr: local_addr,
            shutdown,
        })
    }

    /// The address the endpoint actually bound (useful with port `0` in tests).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Ask the accept loop to exit.
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for HealthServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle_connection(mut stream: TcpStream, state: &HealthState) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    // Read through the end of the headers instead of assuming the request
    // line arrives in one segment. Leaving unread bytes in the socket makes
    // the eventual close a RST on some platforms, which truncates the response
    // for a client that is still reading it.
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_REQUEST_BYTES.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..read.min(remaining)]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") || buf.len() >= MAX_REQUEST_BYTES {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf);
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("").split('?').next().unwrap_or("");

    if method != "GET" && method != "HEAD" {
        return respond(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            "method not allowed",
        );
    }

    match path {
        "/health" | "/healthz" => {
            let snapshot = state.snapshot();
            let healthy = state
                .engine
                .local_addr()
                .map(|result| result.is_ok())
                .unwrap_or(false);
            let body = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
            respond(
                &mut stream,
                if healthy { 200 } else { 503 },
                "application/json",
                &body,
            )
        }
        "/status" => {
            let body =
                serde_json::to_string(&state.snapshot()).unwrap_or_else(|_| "{}".to_string());
            respond(&mut stream, 200, "application/json", &body)
        }
        "/metrics" => respond(
            &mut stream,
            200,
            "text/plain; version=0.0.4; charset=utf-8",
            &state.prometheus(),
        ),
        "/" => respond(
            &mut stream,
            200,
            "application/json",
            "{\"endpoints\":[\"/health\",\"/status\",\"/metrics\"]}",
        ),
        _ => respond(&mut stream, 404, "text/plain; charset=utf-8", "not found"),
    }
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::crypto::Keypair;

    fn state() -> Arc<HealthState> {
        let keypair = Keypair::generate();
        let engine = NodeEngine::new(pwb_core::net::NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: keypair.clone(),
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 16,
            heartbeat_interval: Duration::from_secs(30),
        });
        Arc::new(HealthState {
            engine,
            search: Arc::new(SearchEngine::new()),
            host: Arc::new(ArtifactHost::new()),
            registry: Arc::new(Mutex::new(DomainRegistry::new())),
            ledger: Arc::new(CreditLedger::new(keypair, None, 0)),
            started: Instant::now(),
        })
    }

    fn get(addr: SocketAddr, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (status, body)
    }

    #[test]
    fn health_endpoint_reports_state_and_metrics() {
        let server = HealthServer::spawn("127.0.0.1:0".parse().unwrap(), state()).expect("bind");
        let addr = server.local_addr();

        let (status, body) = get(addr, "/health");
        assert_eq!(status, 503, "engine not started yet");
        assert!(body.contains("\"status\":\"degraded\""));

        let (status, body) = get(addr, "/status");
        assert_eq!(status, 200);
        assert!(body.contains("\"peers\":0"));
        assert!(body.contains("\"claim_price\""));

        let (status, body) = get(addr, "/metrics");
        assert_eq!(status, 200);
        assert!(body.contains("pwb_datagrams_received_total"));
        assert!(body.contains("pwb_peers_connected 0"));

        let (status, _) = get(addr, "/nope");
        assert_eq!(status, 404);
        server.stop();
    }
}
