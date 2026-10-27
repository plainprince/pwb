//! Shared chat state for `chat.plain` (`pwb-node::chat`).
//!
//! [`ChatService`] is the application half of the networking layer's E2EE
//! flag: every outgoing chat message is sealed for its recipient with
//! [`SendFlags::E2EE`] on the direct path, or sealed explicitly via
//! [`NodeEngine::seal_message`] before it is carried across an onion circuit
//! ([`ChatRoute::Onion`]), so what travels on the wire is a payload only the
//! recipient identity can open. The receiver's engine dispatches the inner
//! [`NetworkMessage::Chat`] transparently to this service's subscription —
//! both the ordinary one and the onion-exit one.
//!
//! The service keeps one capped transcript per counterparty plus the last
//! send failure (rendered on the page), and exposes an atomic
//! [`ChatService::version`] the UI polls to know when to re-render. When it
//! is built with [`ChatService::with_data_dir`], transcripts are also written
//! to `<data>/chat.json` after every change, so conversations survive a
//! restart; both binaries use that constructor, and [`ChatService::new`] is
//! the in-memory form for tests. Saving merges with what is already on disk,
//! so a browser and a node pointed at one `PWB_DATA` directory cannot
//! overwrite each other's entries.
//!
//! Delivery is confirmed, not assumed: every inbound chat is answered with a
//! [`NetworkMessage::ChatAck`] receipt on the path it arrived on, and every
//! outbound entry carries [`ChatEntry::ack`] — `None` while the receipt is
//! outstanding, `Some(true)` once the peer confirms, `Some(false)` when the
//! wait expires (which also surfaces as the page's last send note).
//!
//! Both binaries use it: `pwb-browser` renders the interactive `chat.plain`
//! page from [`ChatService::snapshot`]; `pwb-node` runs the same receive loop
//! (and offers a `--chat <peer>` stdin REPL) so a headless daemon stores,
//! prints, and can answer chats addressed to its identity.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pwb_core::crypto::{DhPublicKey, PublicKey};
use pwb_core::discovery::unix_time;
use pwb_core::net::{
    NetworkError, NetworkMessage, NodeEngine, OnionRequest, SendFlags, MAX_CHAT_TEXT,
};

use crate::privacy::{select_path, MIN_ONION_HOPS};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Per-peer transcript cap. The oldest entries are evicted so a long-lived
/// session cannot grow without bound.
pub const TRANSCRIPT_LIMIT: usize = 500;

/// File the persisted transcripts live in, under the node's `--data`
/// directory.
const CHAT_STORE_FILE: &str = "chat.json";

/// How long a send waits for the recipient's [`NetworkMessage::ChatAck`]
/// before its transcript entry is shown as unconfirmed. Direct sends usually
/// confirm in milliseconds; circuit sends retransmit inside the same window.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(8);

/// How a chat message reaches its peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRoute {
    /// Sealed with the E2EE flag and sent straight to the peer's registered
    /// address (through its relay when the peer is relay-routed).
    Direct,
    /// Sealed explicitly, then carried across a three-hop onion circuit, so
    /// no relay — and no observer — learns who is talking to whom. Requires
    /// two direct peers to relay in between the two endpoints.
    Onion,
    /// Prefer a circuit; when one cannot be built (too few peers), fall back
    /// to the direct transport and record a visible note instead of silently
    /// revealing the addresses.
    Auto,
}

/// One chat message in a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEntry {
    /// Who sent it — the local identity for outbound echoes.
    pub from: PublicKey,
    /// The random message id carried on the wire.
    pub id: [u8; 16],
    /// Sender wall-clock unix timestamp (display only, never trusted for
    /// ordering).
    pub sent_at: u64,
    /// The message body (always within [`MAX_CHAT_TEXT`]).
    pub text: String,
    /// Whether this node sent it.
    pub outbound: bool,
    /// Whether it travelled over an onion circuit: set when this node sent
    /// it that way, and when it arrived through the onion exit.
    pub via_onion: bool,
    /// Delivery confirmation for an outbound message: `None` while the
    /// recipient's receipt is outstanding (also the state of inbound
    /// entries and of history written before receipts existed), `Some(true)`
    /// once the peer confirmed, `Some(false)` when the wait expired. Never
    /// `Some(_)` on an inbound entry.
    pub ack: Option<bool>,
}

/// An immutable view of chat state, built by [`ChatService::snapshot`] and
/// rendered by the `chat.plain` page.
#[derive(Debug, Clone)]
pub struct ChatSnapshot {
    /// This node's identity (the transcript partner of every outbound echo).
    pub local: PublicKey,
    /// The last send failure or routing note, shown on the page; replaced by
    /// the next send's outcome.
    pub error: Option<String>,
    /// Transcript per counterparty identity.
    pub transcripts: HashMap<PublicKey, Vec<ChatEntry>>,
}

impl Default for ChatSnapshot {
    fn default() -> Self {
        Self {
            local: PublicKey([0u8; 32]),
            error: None,
            transcripts: HashMap::new(),
        }
    }
}

impl ChatSnapshot {
    /// The transcript with `peer`; empty when they have never talked.
    pub fn transcript(&self, peer: &PublicKey) -> &[ChatEntry] {
        self.transcripts.get(peer).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// On-disk form of the transcript file. Keys are hex, matching every other
/// key representation in the protocol, so an operator can read the file.
#[derive(Serialize, Deserialize)]
struct ChatStoreFile {
    version: u32,
    transcripts: HashMap<String, Vec<ChatEntryWire>>,
}

/// On-disk form of one [`ChatEntry`].
#[derive(Serialize, Deserialize)]
struct ChatEntryWire {
    from: String,
    id: String,
    sent_at: u64,
    text: String,
    outbound: bool,
    #[serde(default)]
    via_onion: bool,
    /// Absent in stores written before receipts existed; see
    /// [`ChatEntry::ack`].
    #[serde(default)]
    ack: Option<bool>,
}

/// Parse a 32-byte hex public key; `None` for anything else.
fn parse_key(hex: &str) -> Option<PublicKey> {
    let bytes = hex::decode(hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(PublicKey(key))
}

/// Parse a 16-byte hex message id.
fn parse_id(hex: &str) -> Option<[u8; 16]> {
    let bytes = hex::decode(hex.trim()).ok()?;
    if bytes.len() != 16 {
        return None;
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes);
    Some(id)
}

/// Load persisted transcripts from `path`.
///
/// A missing or corrupt file yields an empty map: chat history is an
/// enhancement and must never stop a node from starting (the same rule the
/// peer store follows). Individual unparseable entries are skipped.
fn load_store(path: &Path) -> HashMap<PublicKey, Vec<ChatEntry>> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    let Ok(file) = serde_json::from_slice::<ChatStoreFile>(&bytes) else {
        return HashMap::new();
    };
    file.transcripts
        .into_iter()
        .filter_map(|(peer_hex, entries)| {
            let peer = parse_key(&peer_hex)?;
            let entries = entries
                .into_iter()
                .filter_map(|entry| {
                    Some(ChatEntry {
                        from: parse_key(&entry.from)?,
                        id: parse_id(&entry.id)?,
                        sent_at: entry.sent_at,
                        text: entry.text,
                        outbound: entry.outbound,
                        via_onion: entry.via_onion,
                        ack: entry.ack,
                    })
                })
                .collect();
            Some((peer, entries))
        })
        .collect()
}

/// Persist every transcript to `path` (atomically). Failures are reported
/// but never propagated: a full disk must not break a live conversation.
///
/// The write is a merge, not an overwrite. Two processes can deliberately
/// share one `PWB_DATA` directory (the browser and the node pointed at the
/// same profile), and each holds only the entries it has seen; entries are
/// unique by message id, so what is already on disk is kept and this
/// session's new entries are appended before the per-peer cap is applied.
fn save_store(path: &Path, transcripts: &HashMap<PublicKey, Vec<ChatEntry>>) {
    let mut merged = load_store(path);
    for (peer, entries) in transcripts {
        let slot = merged.entry(*peer).or_default();
        for entry in entries {
            if !slot.iter().any(|seen| seen.id == entry.id) {
                slot.push(entry.clone());
            }
        }
        if slot.len() > TRANSCRIPT_LIMIT {
            let overflow = slot.len() - TRANSCRIPT_LIMIT;
            slot.drain(..overflow);
        }
    }
    let file = ChatStoreFile {
        version: 1,
        transcripts: merged
            .iter()
            .map(|(peer, entries)| {
                (
                    peer.to_hex(),
                    entries
                        .iter()
                        .map(|entry| ChatEntryWire {
                            from: entry.from.to_hex(),
                            id: hex::encode(entry.id),
                            sent_at: entry.sent_at,
                            text: entry.text.clone(),
                            outbound: entry.outbound,
                            via_onion: entry.via_onion,
                            ack: entry.ack,
                        })
                        .collect(),
                )
            })
            .collect(),
    };
    let json = match serde_json::to_vec_pretty(&file) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("[Chat] Could not serialize transcripts: {error}");
            return;
        }
    };
    if let Err(error) = crate::persist::atomic_write(path, &json) {
        eprintln!(
            "[Chat] Could not persist transcripts to {}: {error}",
            path.display()
        );
    }
}

/// E2EE chat over the node's engine: direct sends use
/// [`SendFlags::E2EE`], onion sends seal first and ride a circuit; inbound
/// [`NetworkMessage::Chat`] messages land in per-peer transcripts.
pub struct ChatService {
    engine: NodeEngine,
    local: PublicKey,
    transcripts: Arc<Mutex<HashMap<PublicKey, Vec<ChatEntry>>>>,
    error: Arc<Mutex<Option<String>>>,
    version: Arc<AtomicU64>,
    /// Where transcripts are written back to; `None` keeps them in memory
    /// for the session only.
    store: Option<PathBuf>,
    /// How long a send waits for a receipt.
    ack_timeout: Duration,
}

/// The shared half of a [`ChatService`] that a receipt wait needs after the
/// send has returned: cloned into the wait so it can settle the transcript
/// and the page's last note without owning the service.
#[derive(Clone)]
struct Receipts {
    transcripts: Arc<Mutex<HashMap<PublicKey, Vec<ChatEntry>>>>,
    error: Arc<Mutex<Option<String>>>,
    version: Arc<AtomicU64>,
    store: Option<PathBuf>,
}

impl ChatService {
    /// Wrap `engine` with session-scoped (in-memory) transcripts. Nothing
    /// runs until [`Self::run`] is spawned on the tokio runtime.
    pub fn new(engine: NodeEngine) -> Arc<Self> {
        Self::build(engine, None)
    }

    /// Like [`Self::new`], but transcripts are loaded from
    /// `data_dir/chat.json` on startup and written back after every change,
    /// so conversations survive restarts.
    pub fn with_data_dir(engine: NodeEngine, data_dir: &Path) -> Arc<Self> {
        Self::build_at(engine, Some(data_dir.join(CHAT_STORE_FILE)), ACK_TIMEOUT)
    }

    fn build(engine: NodeEngine, store: Option<PathBuf>) -> Arc<Self> {
        Self::build_at(engine, store, ACK_TIMEOUT)
    }

    fn build_at(engine: NodeEngine, store: Option<PathBuf>, ack_timeout: Duration) -> Arc<Self> {
        let local = engine.public_key();
        let transcripts = match &store {
            Some(path) => load_store(path),
            None => HashMap::new(),
        };
        Arc::new(Self {
            engine,
            local,
            transcripts: Arc::new(Mutex::new(transcripts)),
            error: Arc::new(Mutex::new(None)),
            version: Arc::new(AtomicU64::new(0)),
            store,
            ack_timeout,
        })
    }

    /// Run the inbound loops until the engine stops: record every chat
    /// addressed to this identity (and answer it with a receipt), and print
    /// it, so a headless `pwb-node` operator sees conversations even without
    /// the page. Both delivery paths feed the same transcripts —
    /// direct/relayed envelopes and messages opened by an onion exit. The
    /// onion subscription is the request-shaped one, because its return path
    /// is how the receipt gets back without revealing an address.
    pub async fn run(self: Arc<Self>) {
        let mut inbound = self.engine.subscribe();
        let mut onion = self.engine.subscribe_onion_requests();
        loop {
            tokio::select! {
                received = inbound.recv() => {
                    let Some((_from, message)) = received else {
                        break;
                    };
                    self.receive(message, None);
                }
                received = onion.recv() => {
                    let Some(request) = received else {
                        break;
                    };
                    let message = request.message.clone();
                    self.receive(message, Some(request));
                }
            }
        }
    }

    /// Record one inbound chat — and receipt — addressed to this identity.
    ///
    /// `via` is the onion request that delivered it: its signed originator
    /// must agree with the payload's `sender` (a mismatch is forged and
    /// dropped), and it carries the return path a circuit receipt travels
    /// back on. A chat is acknowledged only once it has been stored, so a
    /// receipt means "recorded by the peer", not merely "received".
    fn receive(&self, message: NetworkMessage, via: Option<OnionRequest>) {
        let origin = via.as_ref().map(|request| request.sender);
        match message {
            NetworkMessage::Chat {
                sender,
                id,
                sent_at,
                text,
            } => {
                // The receiver already enforces the cap; re-check here so the
                // invariant holds even if a future path bypasses it.
                if text.len() > MAX_CHAT_TEXT || sender == self.local {
                    return;
                }
                let via_onion = match origin {
                    Some(origin) => {
                        if origin != sender {
                            return;
                        }
                        true
                    }
                    None => false,
                };
                if via_onion {
                    println!("[Chat] {} says (via onion): {}", sender, text);
                } else {
                    println!("[Chat] {} says: {}", sender, text);
                }
                self.record(sender, id, sent_at, text, false, via_onion);
                self.acknowledge(sender, id, via);
            }
            NetworkMessage::ChatAck { sender, id } => {
                if sender == self.local {
                    return;
                }
                if origin.is_some_and(|origin| origin != sender) {
                    return;
                }
                // Only settles an entry this node actually sent: unknown ids
                // and inbound entries are ignored, so a peer cannot confirm
                // a conversation it was never part of.
                self.settle(&sender, &id, true, None);
            }
            _ => {}
        }
    }

    /// Confirm `id` back to `peer` on the path the chat arrived on: a
    /// sealed receipt straight down the direct transport, or the reply to
    /// the onion request (which carries its own return path). A receipt that
    /// cannot leave is a network condition, not a chat failure — the
    /// sender's wait simply ends as unconfirmed.
    fn acknowledge(&self, peer: PublicKey, id: [u8; 16], via: Option<OnionRequest>) {
        let ack = NetworkMessage::ChatAck {
            sender: self.local,
            id,
        };
        let sent = match via {
            Some(request) => self
                .engine
                .seal_message(&peer, ack)
                .and_then(|sealed| self.engine.send_onion_reply(&request, &sealed)),
            None => self.engine.send_to_with(&peer, ack, SendFlags::E2EE),
        };
        if let Err(error) = sent {
            eprintln!("[Chat] could not confirm delivery to {peer}: {error}");
        }
    }

    /// Send `text` to `peer` over the direct transport, sealed with
    /// [`SendFlags::E2EE`], and append the outbound echo to the transcript.
    ///
    /// Every failure is recorded for the page (and bumps the version), so a
    /// dropped message is visible instead of silently lost.
    pub fn send(&self, peer: &PublicKey, text: &str) -> Result<(), String> {
        self.send_via(peer, text, ChatRoute::Direct)
    }

    /// [`Self::send`] with an explicit [`ChatRoute`]. See the route for what
    /// each path guarantees; `Auto` additionally records a visible note when
    /// it had to fall back to the direct transport.
    pub fn send_via(&self, peer: &PublicKey, text: &str, route: ChatRoute) -> Result<(), String> {
        match self.try_send(peer, text, route) {
            Ok(note) => {
                // A successful send replaces the previous failure; an `Auto`
                // fallback note takes its place so the page can show that the
                // addresses were revealed.
                *self.error.lock().unwrap() = note;
                Ok(())
            }
            Err(error) => {
                *self.error.lock().unwrap() = Some(error.clone());
                self.version.fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    /// Validate, route, deliver, and record one outbound message. Returns
    /// the routing note to show (`Some` only for an `Auto` fallback).
    fn try_send(
        &self,
        peer: &PublicKey,
        text: &str,
        route: ChatRoute,
    ) -> Result<Option<String>, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("message is empty".to_string());
        }
        if text.len() > MAX_CHAT_TEXT {
            return Err(format!("message is too long (max {MAX_CHAT_TEXT} bytes)"));
        }
        if *peer == self.local {
            return Err("cannot chat with yourself".to_string());
        }

        // Resolve `Auto` first: build the circuit if one exists, otherwise
        // remember a note and use the direct transport.
        let mut note = None;
        let mut path = None;
        let route = match route {
            ChatRoute::Direct => ChatRoute::Direct,
            ChatRoute::Onion | ChatRoute::Auto => {
                match select_path(&self.engine, Some(*peer), MIN_ONION_HOPS) {
                    Ok(built) => {
                        path = Some(built);
                        ChatRoute::Onion
                    }
                    Err(error) if route == ChatRoute::Onion => {
                        return Err(format!("no onion circuit: {error}"));
                    }
                    Err(error) => {
                        note = Some(format!(
                            "No onion circuit to this peer ({error}) — sent directly, revealing both addresses."
                        ));
                        ChatRoute::Direct
                    }
                }
            }
        };

        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        let sent_at = unix_time();
        let message = NetworkMessage::Chat {
            sender: self.local,
            id,
            sent_at,
            text: text.to_string(),
        };

        match route {
            ChatRoute::Direct => {
                self.engine
                    .send_to_with(peer, message, SendFlags::E2EE)
                    .map_err(map_network_error)?;
                self.record(*peer, id, sent_at, text.to_string(), true, false);
                // The peer's receipt decides whether this entry ends up
                // confirmed or visibly unconfirmed.
                self.watch_ack(*peer, id);
            }
            _ => {
                let path = path.expect("onion route always carries a built path");
                let sealed = self
                    .engine
                    .seal_message(peer, message)
                    .map_err(|error| error.to_string())?;
                // Recorded before the send leaves: the circuit send and its
                // receipt wait run on the runtime after this returns.
                self.record(*peer, id, sent_at, text.to_string(), true, true);
                self.deliver_onion(*peer, id, path, sealed);
            }
        }

        Ok(note)
    }

    /// Append one entry to the counterparty's transcript (evicting the oldest
    /// beyond the cap), persist it when a store is configured, and bump the
    /// version so the UI re-renders.
    fn record(
        &self,
        peer: PublicKey,
        id: [u8; 16],
        sent_at: u64,
        text: String,
        outbound: bool,
        via_onion: bool,
    ) {
        let entry = ChatEntry {
            from: if outbound { self.local } else { peer },
            id,
            sent_at,
            text,
            outbound,
            via_onion,
            // A fresh entry waits for the peer's receipt (inbound entries and
            // history from before receipts existed are simply `None`).
            ack: None,
        };
        {
            let mut transcripts = self.transcripts.lock().unwrap();
            let transcript = transcripts.entry(peer).or_default();
            transcript.push(entry);
            if transcript.len() > TRANSCRIPT_LIMIT {
                let overflow = transcript.len() - TRANSCRIPT_LIMIT;
                transcript.drain(..overflow);
            }
            if let Some(path) = &self.store {
                save_store(path, &transcripts);
            }
        }
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// The shared state a receipt wait settles once the send has returned.
    fn receipts(&self) -> Receipts {
        Receipts {
            transcripts: Arc::clone(&self.transcripts),
            error: Arc::clone(&self.error),
            version: Arc::clone(&self.version),
            store: self.store.clone(),
        }
    }

    /// Record a delivery outcome for one of this node's own messages.
    /// See [`Receipts::settle`].
    fn settle(
        &self,
        peer: &PublicKey,
        id: &[u8; 16],
        confirmed: bool,
        note: Option<String>,
    ) -> bool {
        self.receipts().settle(peer, id, confirmed, note)
    }

    /// Start the wait for a direct send's receipt. When none arrives inside
    /// [`ACK_TIMEOUT`] the entry becomes visibly unconfirmed instead of
    /// sitting in the transcript looking delivered forever. The wait runs on
    /// its own thread: it must not block the caller (a page send or a REPL
    /// line), and it must work whether or not the caller is on a runtime.
    fn watch_ack(&self, peer: PublicKey, id: [u8; 16]) {
        let receipts = self.receipts();
        let timeout = self.ack_timeout;
        let note = no_receipt_note(&peer, timeout);
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            receipts.settle(&peer, &id, false, Some(note));
        });
    }

    /// Hand an onion send to its own thread: it rides the circuit as a
    /// request carrying a return path, so the recipient's receipt comes back
    /// on that same circuit instead of needing the sender's address. The
    /// send and its wait therefore happen together, after this call has
    /// returned, on a small runtime of their own — a caller outside any
    /// runtime gets the same behaviour as one inside it.
    fn deliver_onion(
        &self,
        peer: PublicKey,
        id: [u8; 16],
        path: Vec<DhPublicKey>,
        sealed: NetworkMessage,
    ) {
        let engine = self.engine.clone();
        let receipts = self.receipts();
        let timeout = self.ack_timeout;
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let outcome = match runtime {
                Ok(runtime) => runtime.block_on(engine.send_onion_request(&path, &sealed, timeout)),
                Err(_) => {
                    // No way to wait for a receipt; send the one-way way so
                    // the message still leaves, then let the wait expire.
                    let _ = engine.send_onion(&path, &sealed);
                    Err(NetworkError::Timeout)
                }
            };
            match outcome {
                // The receipt names the message it confirms, so a reply for
                // anything else leaves the entry waiting.
                Ok(NetworkMessage::ChatAck { sender, id: acked })
                    if sender == peer && acked == id =>
                {
                    receipts.settle(&peer, &id, true, None);
                }
                Ok(_) | Err(NetworkError::Timeout) => {
                    let note = no_receipt_note(&peer, timeout);
                    receipts.settle(&peer, &id, false, Some(note));
                }
                Err(error) => {
                    receipts.settle(
                        &peer,
                        &id,
                        false,
                        Some(format!("onion send failed: {error}")),
                    );
                }
            }
        });
    }

    /// An immutable view for rendering `chat.plain`.
    pub fn snapshot(&self) -> ChatSnapshot {
        ChatSnapshot {
            local: self.local,
            error: self.error.lock().unwrap().clone(),
            transcripts: self.transcripts.lock().unwrap().clone(),
        }
    }

    /// Monotonic change counter: bumped on every transcript or error update.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }
}

impl Receipts {
    /// Record a delivery outcome for one of this node's own messages:
    /// `confirmed` when the peer's receipt arrives, `false` when the wait
    /// expires. The first answer wins, except that a late receipt still
    /// upgrades a timed-out entry. `note` (only when the outcome changed)
    /// becomes the page's last send note, and a change bumps the version so
    /// the page re-renders. Returns whether anything changed.
    ///
    /// Matching is by `(peer, message id)` against outbound entries only,
    /// so a peer cannot confirm a message it was never sent.
    fn settle(
        &self,
        peer: &PublicKey,
        id: &[u8; 16],
        confirmed: bool,
        note: Option<String>,
    ) -> bool {
        let mut transcripts = self.transcripts.lock().unwrap();
        let Some(entry) = transcripts.get_mut(peer).and_then(|transcript| {
            transcript
                .iter_mut()
                .find(|entry| entry.outbound && entry.id == *id)
        }) else {
            return false;
        };
        if entry.ack == Some(true) || entry.ack == Some(confirmed) {
            return false;
        }
        entry.ack = Some(confirmed);
        if let Some(path) = &self.store {
            save_store(path, &transcripts);
        }
        drop(transcripts);
        // Both outcomes matter without a page: the daemon REPL and the
        // browser console see them the same way they see inbound chats.
        if let Some(note) = note {
            eprintln!("[Chat] {note}");
            *self.error.lock().unwrap() = Some(note);
        }
        if confirmed {
            println!("[Chat] {peer} confirmed delivery");
        }
        self.version.fetch_add(1, Ordering::Relaxed);
        true
    }
}

/// The page's note for a send whose receipt never arrived.
fn no_receipt_note(peer: &PublicKey, timeout: Duration) -> String {
    let window = if timeout.as_secs() == 0 {
        format!("{}ms", timeout.as_millis())
    } else {
        format!("{}s", timeout.as_secs())
    };
    format!(
        "No delivery confirmation from {peer} within {window} — the message may not have arrived."
    )
}

/// Map a transport failure to the message shown in the composer.
fn map_network_error(error: NetworkError) -> String {
    match error {
        NetworkError::PeerNotFound(_) => {
            "peer is not connected — pair with it on nodes.plain first".to_string()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::net::{NodeConfig, NodeEngine};

    fn engine(key_seed: u8) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: pwb_core::crypto::Keypair::from_seed(&[key_seed; 32]),
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 16,
            heartbeat_interval: std::time::Duration::from_secs(30),
        })
    }

    fn temp_store_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pwb-chat-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp store dir");
        dir
    }

    #[test]
    fn sending_to_an_unknown_peer_records_the_error_and_bumps_the_version() {
        let service = ChatService::new(engine(1));
        let stranger = PublicKey([2u8; 32]);

        let before = service.version();
        let error = service
            .send(&stranger, "hello")
            .expect_err("an unregistered peer cannot be dialed");
        assert!(error.contains("not connected"), "got: {error}");
        assert!(
            service.version() > before,
            "a failure must re-render the page"
        );
        assert_eq!(
            service.snapshot().error.as_deref(),
            Some(error.as_str()),
            "the page can show why the send failed"
        );
        assert!(
            service.snapshot().transcript(&stranger).is_empty(),
            "a failed send must not echo into the transcript"
        );
    }

    #[test]
    fn empty_and_oversized_messages_are_rejected_before_the_network() {
        let service = ChatService::new(engine(1));
        let peer = PublicKey([2u8; 32]);
        assert!(service.send(&peer, "   ").is_err());
        assert!(service.send(&peer, &"x".repeat(MAX_CHAT_TEXT + 1)).is_err());
        assert!(service.snapshot().transcript(&peer).is_empty());
    }

    #[test]
    fn transcripts_are_capped_at_the_limit() {
        let service = ChatService::new(engine(1));
        let peer = PublicKey([3u8; 32]);
        {
            let mut transcripts = service.transcripts.lock().unwrap();
            let transcript = transcripts.entry(peer).or_default();
            for i in 0..(TRANSCRIPT_LIMIT + 25) {
                transcript.push(ChatEntry {
                    from: peer,
                    id: [0u8; 16],
                    sent_at: i as u64,
                    text: format!("m{i}"),
                    outbound: false,
                    via_onion: false,
                    ack: None,
                });
            }
        }
        // Drive the same eviction `record` performs.
        service.record(peer, [0u8; 16], 0, "tail".to_string(), false, false);

        let snapshot = service.snapshot();
        let transcript = snapshot.transcript(&peer);
        assert_eq!(transcript.len(), TRANSCRIPT_LIMIT);
        assert_eq!(transcript.last().unwrap().text, "tail");
    }

    #[test]
    fn snapshot_defaults_to_a_zero_identity_without_transcripts() {
        let snapshot = ChatSnapshot::default();
        assert_eq!(snapshot.local, PublicKey([0u8; 32]));
        assert_eq!(snapshot.error, None);
        assert!(snapshot.transcript(&PublicKey([1u8; 32])).is_empty());
    }

    #[test]
    fn transcripts_persist_across_a_restart() {
        let dir = temp_store_dir("persist");
        let peer = PublicKey([7u8; 32]);

        {
            let service = ChatService::with_data_dir(engine(4), &dir);
            assert!(
                service.snapshot().transcript(&peer).is_empty(),
                "a fresh store starts empty"
            );
            service.record(
                peer,
                [9u8; 16],
                1_700_000_000,
                "hello".to_string(),
                false,
                true,
            );
            service.record(
                peer,
                [8u8; 16],
                1_700_000_001,
                "hi back".to_string(),
                true,
                false,
            );
        }

        // A second service on the same data dir (a restart) sees both sides
        // of the conversation, including the onion marker.
        let service = ChatService::with_data_dir(engine(4), &dir);
        let transcript = service.snapshot().transcript(&peer).to_vec();
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].text, "hello");
        assert!(!transcript[0].outbound);
        assert!(transcript[0].via_onion, "the onion marker survives");
        assert_eq!(transcript[1].text, "hi back");
        assert!(transcript[1].outbound);
        assert_eq!(transcript[0].id, [9u8; 16]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_store_file_starts_empty_instead_of_failing() {
        let dir = temp_store_dir("corrupt");
        std::fs::write(dir.join(CHAT_STORE_FILE), b"{not json").expect("write corrupt store");

        let service = ChatService::with_data_dir(engine(5), &dir);
        assert!(service.snapshot().transcripts.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_route_falls_back_to_direct_with_a_visible_note() {
        // One registered peer is not enough for a three-hop circuit, so
        // `Auto` must fall back — loudly — and the send itself must succeed.
        let service = ChatService::new(engine(1));
        // A real keypair, so the key is a valid curve point for the envelope
        // encoder; a random 32-byte value need not be.
        let peer = pwb_core::crypto::Keypair::from_seed(&[2u8; 32]).public;
        service.engine.add_peer(pwb_core::net::PeerInfo {
            public_key: peer,
            last_seen: std::time::Instant::now(),
            address: "127.0.0.1:9999".parse().unwrap(),
            reputation: 0,
        });

        service
            .send_via(&peer, "hi", ChatRoute::Auto)
            .expect("the direct fallback still delivers");
        let snapshot = service.snapshot();
        let note = snapshot.error.as_deref().expect("a fallback note is shown");
        assert!(
            note.contains("No onion circuit") && note.contains("revealing"),
            "got: {note}"
        );
        let transcript = snapshot.transcript(&peer);
        assert_eq!(transcript.len(), 1);
        assert!(transcript[0].outbound);
        assert!(!transcript[0].via_onion, "the fallback went direct");
    }

    #[test]
    fn an_impossible_onion_route_fails_loudly_instead_of_leaking_directly() {
        let service = ChatService::new(engine(1));
        let stranger = PublicKey([2u8; 32]);

        let error = service
            .send_via(&stranger, "hi", ChatRoute::Onion)
            .expect_err("no circuit exists to an unknown peer");
        assert!(error.contains("onion circuit"), "got: {error}");
        assert!(service.snapshot().transcript(&stranger).is_empty());
    }

    #[test]
    fn a_receipt_settles_only_the_outbound_entry_it_names() {
        let service = ChatService::new(engine(1));
        let peer = PublicKey([2u8; 32]);
        service.record(peer, [4u8; 16], 1, "hi".to_string(), true, false);
        service.record(peer, [5u8; 16], 2, "you said".to_string(), false, false);

        assert_eq!(
            service.snapshot().transcript(&peer)[0].ack,
            None,
            "a fresh send waits for the peer's receipt"
        );
        assert!(
            service.settle(&peer, &[4u8; 16], true, None),
            "the receipt changes the entry"
        );
        assert_eq!(service.snapshot().transcript(&peer)[0].ack, Some(true));

        assert!(
            !service.settle(&peer, &[4u8; 16], false, None),
            "a stale expiry cannot take back a confirmation"
        );
        assert_eq!(service.snapshot().transcript(&peer)[0].ack, Some(true));

        assert!(
            !service.settle(&peer, &[5u8; 16], true, None),
            "an inbound entry is never confirmed by the peer"
        );
        assert_eq!(service.snapshot().transcript(&peer)[1].ack, None);
        assert!(
            !service.settle(&PublicKey([9u8; 32]), &[4u8; 16], true, None),
            "a receipt from another counterparty confirms nothing"
        );
    }

    #[test]
    fn a_send_nobody_confirms_becomes_visibly_unconfirmed() {
        // A registered-but-silent peer: the datagram is composed and the
        // transcript records the echo, then nothing ever answers.
        let service = ChatService::build_at(engine(1), None, Duration::from_millis(60));
        let peer = pwb_core::crypto::Keypair::from_seed(&[2u8; 32]).public;
        service.engine.add_peer(pwb_core::net::PeerInfo {
            public_key: peer,
            last_seen: std::time::Instant::now(),
            address: "127.0.0.1:9999".parse().unwrap(),
            reputation: 0,
        });

        service
            .send(&peer, "anyone there?")
            .expect("the send leaves");
        assert_eq!(
            service.snapshot().transcript(&peer)[0].ack,
            None,
            "immediately after sending the receipt is still outstanding"
        );

        std::thread::sleep(Duration::from_millis(300));
        let snapshot = service.snapshot();
        assert_eq!(
            snapshot.transcript(&peer)[0].ack,
            Some(false),
            "the wait expired instead of leaving the entry looking delivered"
        );
        let note = snapshot.error.as_deref().expect("the expiry is shown");
        assert!(
            note.contains("No delivery confirmation") && note.contains(&peer.to_hex()),
            "got: {note}"
        );
    }

    #[test]
    fn two_processes_sharing_one_store_keep_each_others_entries() {
        // `PWB_DATA` deliberately points the browser and the node at one
        // directory. Both load the file before either writes, so the save
        // has to merge rather than overwrite.
        let dir = temp_store_dir("merge");
        let peer = PublicKey([3u8; 32]);
        let browser = ChatService::with_data_dir(engine(6), &dir);
        let node = ChatService::with_data_dir(engine(7), &dir);

        node.record(
            peer,
            [1u8; 16],
            10,
            "from the daemon".to_string(),
            false,
            false,
        );
        browser.record(
            peer,
            [2u8; 16],
            11,
            "from the browser".to_string(),
            true,
            false,
        );

        let service = ChatService::with_data_dir(engine(8), &dir);
        let transcript = service.snapshot().transcript(&peer).to_vec();
        let texts: Vec<&str> = transcript.iter().map(|entry| entry.text.as_str()).collect();
        assert_eq!(
            texts,
            ["from the daemon", "from the browser"],
            "neither process's entry is lost to the other's write"
        );
        assert!(transcript[1].outbound);
        assert!(!transcript[0].outbound);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
