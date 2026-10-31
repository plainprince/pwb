//! NodeEngine background loops: inbound receiver, outbound sender, message
//! handler, peer maintenance, and per-message processing.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::crypto::{DhKeypair, DhPublicKey, PublicKey};
use crate::dht::{Contact, NodeId, RoutingTable, UpdateOutcome};
use crate::discovery::unix_time;
use crate::onion::{CoverConfig, CoverDispatch, OnionAction, OnionCell, CELL_SIZE, MIN_HOPS};
use crate::telemetry::Metrics;

use super::addr::{map_dest_for, unmap_source};
use super::e2ee::{self, E2eeState};
use super::engine::{NodeConfig, PeerInfo, PEER_TIMEOUT};
use super::envelope::{self, Reassembler, ReplayGuard};
use super::messages::{
    decode_punch_probe, encode_punch_probe, BlogEntryWire, NetworkMessage, OutgoingDatagram,
    RoomPostWire, MAX_CHAT_TEXT, PUNCH_MAGIC,
};
use super::onion_transport::{self, OnionAssembly, OnionKind, OnionRequest, RelayEvent};

/// Application-level listener for inbound, already-authenticated messages.
pub(super) type MessageSink = mpsc::Sender<(SocketAddr, NetworkMessage)>;

/// The set of registered application listeners.
pub(super) type Subscribers = Arc<Mutex<Vec<MessageSink>>>;

/// One onion-delivery listener: authenticated originator plus decoded message.
pub(super) type OnionSink = mpsc::Sender<(PublicKey, NetworkMessage)>;

/// The set of registered onion-delivery listeners.
pub(super) type OnionSubscribers = Arc<Mutex<Vec<OnionSink>>>;

/// One onion-request listener: originator, return path, and decoded message.
pub(super) type OnionRequestSink = mpsc::Sender<OnionRequest>;

/// The set of registered onion-request listeners.
pub(super) type OnionRequestSubscribers = Arc<Mutex<Vec<OnionRequestSink>>>;

/// In-flight `send_onion_request` callers, keyed by request id. A reply
/// delivered by an onion exit resolves here instead of being treated as a new
/// inbound request.
pub(super) type PendingOnionReplies = Arc<Mutex<HashMap<[u8; 16], mpsc::Sender<NetworkMessage>>>>;

/// One relay-event listener (relay-credit accounting).
pub(super) type RelaySink = mpsc::Sender<RelayEvent>;

/// The set of registered relay-event listeners.
pub(super) type RelaySubscribers = Arc<Mutex<Vec<RelaySink>>>;

/// Sustained authenticated messages accepted per peer per second.
const PEER_MESSAGE_RATE: f64 = 128.0;
/// Burst capacity for a peer that has been quiet (about two seconds' worth).
const PEER_MESSAGE_BURST: f64 = 256.0;
/// Upper bound on tracked keys; beyond this the least-recently-active bucket
/// is evicted, so a flood cannot grow the limiter without bound.
const RATE_TRACKED_KEYS: usize = 4096;

/// Sustained onion cells accepted from one source IP per second. A 48 KiB
/// onion message is ~417 cells, so 512/s sustains roughly 1.2 maximum-size
/// messages per second from one peer while the global 4096/s budget remains
/// the hard ceiling for the node.
const CELL_RATE_PER_SOURCE: f64 = 512.0;
/// Burst capacity for one source IP. Chunked onion fetches send several
/// maximum-size replies through different circuits that can share an entry IP;
/// 2048 cells covers four complete 48 KiB messages arriving back-to-back.
const CELL_BURST_PER_SOURCE: f64 = 2048.0;
/// Global peel budget, shared by every source. A spoofed-source flood still
/// costs one X25519 + ChaCha20-Poly1305 peel per cell, so the node needs a
/// ceiling that does not depend on per-source accounting.
const CELL_GLOBAL_RATE: f64 = 4096.0;
const CELL_GLOBAL_BURST: f64 = 8192.0;
/// How long an exact onion cell is remembered for replay rejection.
const ONION_REPLAY_TTL: Duration = Duration::from_secs(300);
/// Maximum number of cell digests kept for replay rejection. At 512 bytes per
/// cell this covers roughly 32 MiB of recent traffic while storing only 32-byte
/// digests plus bookkeeping.
const ONION_REPLAY_CAPACITY: usize = 65_536;

struct RateBucket {
    tokens: f64,
    last: Instant,
}

/// Token buckets keyed by an arbitrary identity.
///
/// A key may burst up to `burst` units and then sustain `rate` per second.
/// Excess units are dropped without reaching the caller, which bounds the work
/// any one key can force the node to do. Freshly seen keys start full.
struct TokenBuckets<K> {
    buckets: HashMap<K, RateBucket>,
    rate: f64,
    burst: f64,
    max_keys: usize,
}

impl<K: Eq + Hash + Copy> TokenBuckets<K> {
    fn new(rate: f64, burst: f64, max_keys: usize) -> Self {
        Self {
            buckets: HashMap::new(),
            rate,
            burst,
            max_keys,
        }
    }

    /// Consume one token for `key` at `now`. Returns `false` when the key is
    /// over its rate and the unit must be dropped.
    fn accept(&mut self, key: &K, now: Instant) -> bool {
        let (rate, burst) = (self.rate, self.burst);
        if !self.buckets.contains_key(key) && self.buckets.len() >= self.max_keys {
            // Evict the least-recently-active bucket instead of growing.
            if let Some(oldest) = self
                .buckets
                .iter()
                .min_by_key(|(_, bucket)| bucket.last)
                .map(|(key, _)| *key)
            {
                self.buckets.remove(&oldest);
            }
        }
        let bucket = self.buckets.entry(*key).or_insert(RateBucket {
            tokens: burst,
            last: now,
        });
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(burst);
        bucket.last = now;
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }
}

/// Per-peer token bucket applied to authenticated inbound messages.
type PeerRateLimiter = TokenBuckets<PublicKey>;

impl Default for PeerRateLimiter {
    fn default() -> Self {
        Self::new(PEER_MESSAGE_RATE, PEER_MESSAGE_BURST, RATE_TRACKED_KEYS)
    }
}

/// Quota for unauthenticated onion cells.
///
/// Cells cannot be attributed to an identity before they are peeled, so they
/// are limited per source IP, with a global ceiling that keeps a spoofed-source
/// flood from multiplying the cost of peeling. This closes the gap where the
/// authenticated per-peer bucket could be bypassed entirely by sending 512-byte
/// well-formed cell headers.
struct OnionRateLimiter {
    per_source: TokenBuckets<IpAddr>,
    global: TokenBuckets<()>,
}

impl OnionRateLimiter {
    fn new(
        source_rate: f64,
        source_burst: f64,
        global_rate: f64,
        global_burst: f64,
        max_sources: usize,
    ) -> Self {
        Self {
            per_source: TokenBuckets::new(source_rate, source_burst, max_sources),
            global: TokenBuckets::new(global_rate, global_burst, 1),
        }
    }

    /// Accept or drop one inbound cell. The global budget is checked first so
    /// a global flood does not pay for per-source bookkeeping.
    fn accept(&mut self, source: IpAddr, now: Instant) -> bool {
        if !self.global.accept(&(), now) {
            return false;
        }
        self.per_source.accept(&source, now)
    }
}

impl Default for OnionRateLimiter {
    fn default() -> Self {
        Self::new(
            CELL_RATE_PER_SOURCE,
            CELL_BURST_PER_SOURCE,
            CELL_GLOBAL_RATE,
            CELL_GLOBAL_BURST,
            RATE_TRACKED_KEYS,
        )
    }
}

/// Bounded cache of exact onion cells already accepted.
///
/// A cell has no circuit id or counter, so a relay cannot distinguish a replay
/// from a fresh cell by its header. Hashing the authenticated ciphertext still
/// catches re-injection of the exact bytes: an attacker cannot produce a
/// different valid cell for the same layer without the relay's key, and a
/// legitimate sender never retransmits the same encrypted cell (retries build
/// fresh cells). Digests are kept for a fixed TTL, and the oldest entries are
/// evicted once the capacity is reached, so the cache cannot grow without
/// bound.
#[derive(Debug)]
pub(super) struct OnionReplayCache {
    seen: HashMap<[u8; 32], Instant>,
    order: VecDeque<([u8; 32], Instant)>,
    ttl: Duration,
    capacity: usize,
}

impl OnionReplayCache {
    fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            seen: HashMap::new(),
            order: VecDeque::new(),
            ttl,
            capacity: capacity.max(1),
        }
    }

    /// Accept a cell once, rejecting exact duplicates still inside the TTL.
    fn accept(&mut self, cell: &[u8], now: Instant) -> bool {
        let digest = *blake3::hash(cell).as_bytes();
        if let Some(seen_at) = self.seen.get(&digest) {
            if now.saturating_duration_since(*seen_at) <= self.ttl {
                return false;
            }
        }

        self.seen.insert(digest, now);
        self.order.push_back((digest, now));
        while self.order.len() > self.capacity || self.seen.len() > self.capacity {
            let Some((oldest, inserted_at)) = self.order.pop_front() else {
                break;
            };
            // Only remove the current entry when the queue item still owns it;
            // a digest reinserted after its TTL has a newer timestamp.
            if self.seen.get(&oldest) == Some(&inserted_at) {
                self.seen.remove(&oldest);
            }
        }
        true
    }
}

impl Default for OnionReplayCache {
    fn default() -> Self {
        Self::new(ONION_REPLAY_TTL, ONION_REPLAY_CAPACITY)
    }
}

/// Peer-table and anti-replay state shared with the receiver loop.
pub(super) struct InboundState {
    pub peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    pub max_peers: usize,
    pub replay: Arc<Mutex<ReplayGuard>>,
    pub reassembly: Arc<Mutex<Reassembler>>,
    pub onion_replay: Mutex<OnionReplayCache>,
    pub onion_assembly: Arc<Mutex<OnionAssembly>>,
    pub onion_subscribers: OnionSubscribers,
    pub onion_request_subscribers: OnionRequestSubscribers,
    pub pending_onion_replies: PendingOnionReplies,
    pub relay_subscribers: RelaySubscribers,
    pub outgoing_tx: mpsc::Sender<OutgoingDatagram>,
    pub routing: Arc<Mutex<RoutingTable>>,
    pub local_key: PublicKey,
    pub local_keypair: crate::crypto::Keypair,
    /// Anti-replay state for inbound [`NetworkMessage::Sealed`] payloads.
    pub e2ee: Arc<E2eeState>,
    pub local_dh: DhKeypair,
    pub metrics: Arc<Metrics>,
    pub relay_routes: Arc<Mutex<HashMap<PublicKey, PublicKey>>>,
    pub punch_waiters: Arc<Mutex<HashMap<PublicKey, mpsc::Sender<SocketAddr>>>>,
}

/// Everything the inbound handler needs to answer protocol messages.
pub(super) struct HandlerState {
    pub peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    pub config: NodeConfig,
    pub running: Arc<Mutex<bool>>,
    pub subscribers: Subscribers,
    pub outgoing_tx: mpsc::Sender<OutgoingDatagram>,
    pub seq: Arc<AtomicU64>,
    pub routing: Arc<Mutex<RoutingTable>>,
    pub relay_routes: Arc<Mutex<HashMap<PublicKey, PublicKey>>>,
    /// This node's dialable address, used as the origin address when wrapping
    /// a reply for a relayed peer.
    pub advertised_addr: SocketAddr,
}

/// Borrowed view of [`HandlerState`] for one message.
struct ProcessContext<'a> {
    peer_table: &'a Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    config: &'a NodeConfig,
    outgoing_tx: &'a mpsc::Sender<OutgoingDatagram>,
    seq: &'a Arc<AtomicU64>,
    routing: &'a Arc<Mutex<RoutingTable>>,
    relay_routes: &'a Arc<Mutex<HashMap<PublicKey, PublicKey>>>,
    advertised_addr: SocketAddr,
}

/// Internal message handler loop.
pub(super) async fn message_handler_loop(
    mut receiver: mpsc::Receiver<(SocketAddr, PublicKey, NetworkMessage)>,
    state: HandlerState,
) {
    let HandlerState {
        peer_table,
        config,
        running,
        subscribers,
        outgoing_tx,
        seq,
        routing,
        relay_routes,
        advertised_addr,
    } = state;
    let context = ProcessContext {
        peer_table: &peer_table,
        config: &config,
        outgoing_tx: &outgoing_tx,
        seq: &seq,
        routing: &routing,
        relay_routes: &relay_routes,
        advertised_addr,
    };
    while *running.lock().unwrap() {
        tokio::select! {
            Some((sender_addr, sender_key, message)) = receiver.recv() => {
                // Forward to application subscribers first (they may need the
                // message before internal bookkeeping), then run the engine's
                // own peer-table maintenance on it.
                {
                    let mut subs = subscribers.lock().unwrap();
                    try_deliver(&mut subs, (sender_addr, message.clone()));
                }
                process_message(message, sender_addr, &sender_key, &context).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                // Small sleep to prevent busy loop
            }
        }
    }
}

/// Internal peer maintenance loop: prune peers we have not heard from and
/// probe the remaining ones so `last_seen` tracks reachability instead of
/// merely time-of-insertion. Any authenticated reply refreshes the peer.
pub(super) async fn peer_maintenance_loop(
    peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    config: NodeConfig,
    running: Arc<Mutex<bool>>,
    outgoing_tx: mpsc::Sender<OutgoingDatagram>,
    seq: Arc<AtomicU64>,
) {
    let mut interval = interval(config.heartbeat_interval.max(Duration::from_secs(1)));

    loop {
        interval.tick().await;
        if !*running.lock().unwrap() {
            break;
        }

        let now = Instant::now();
        let live: Vec<(PublicKey, SocketAddr)> = {
            let mut peer_table = peer_table.lock().unwrap();
            peer_table.retain(|_, peer_info| {
                now.saturating_duration_since(peer_info.last_seen) < PEER_TIMEOUT
            });
            peer_table
                .values()
                .map(|peer| (peer.public_key, peer.address))
                .collect()
        };

        // Liveness probe: a Pong refreshes the peer, silence eventually
        // expires it above. Each recipient needs its own ciphertext.
        if live.is_empty() {
            continue;
        }
        let ping = NetworkMessage::Ping {
            nonce: seq.fetch_add(1, Ordering::Relaxed),
        };
        let Ok(bytes) = serde_json::to_vec(&ping) else {
            continue;
        };
        for (public_key, addr) in live {
            let Some(recipient) = DhPublicKey::from_ed25519(&public_key) else {
                continue;
            };
            let Ok(datagrams) = envelope::encode_for(
                &config.keypair,
                &recipient,
                seq.fetch_add(1, Ordering::Relaxed),
                unix_time(),
                &bytes,
            ) else {
                continue;
            };
            for datagram in datagrams {
                let _ = outgoing_tx.try_send((addr, datagram));
            }
        }
    }
}

/// Opt-in cover-traffic loop: periodically builds a decoy onion cell over a
/// randomized path of live peers and sends it to the entry hop exactly like a
/// real request. Timing is exponential (see [`CoverConfig`]), and no cell is
/// emitted until at least [`MIN_HOPS`] distinct peers are known.
pub(super) async fn cover_traffic_loop(
    outgoing_tx: mpsc::Sender<(SocketAddr, Vec<u8>)>,
    peers: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    running: Arc<Mutex<bool>>,
    metrics: Arc<Metrics>,
    config: CoverConfig,
) {
    let mut dispatch = CoverDispatch::new(config);
    // The scheduler's minimum interval is milliseconds in tests and seconds
    // in production; poll often enough for both without a busy loop.
    let mut ticker = interval(Duration::from_millis(250));
    let mut rng = rand::rngs::OsRng;
    loop {
        ticker.tick().await;
        if !*running.lock().unwrap() {
            break;
        }
        let candidates: Vec<(DhPublicKey, SocketAddr)> = {
            let table = peers.lock().unwrap();
            table
                .values()
                .filter_map(|peer| {
                    DhPublicKey::from_ed25519(&peer.public_key).map(|key| (key, peer.address))
                })
                .collect()
        };
        if candidates.len() < MIN_HOPS {
            continue;
        }
        let pool: Vec<DhPublicKey> = candidates.iter().map(|(key, _)| *key).collect();
        let Some((cell, path)) = dispatch.poll(Instant::now(), &pool, &mut rng) else {
            continue;
        };
        let Some((_, addr)) = candidates.iter().find(|(key, _)| *key == path.relays[0]) else {
            continue;
        };
        match outgoing_tx.try_send((*addr, cell.to_bytes().to_vec())) {
            Ok(()) => metrics.record_cover_cell_sent(),
            Err(_) => metrics.record_send_queue_drop(),
        }
    }
}

/// Outgoing sender loop: dequeues `(SocketAddr, Vec<u8>)` datagrams and
/// transmits them over the bound UDP socket. Runs until `running` is false.
pub(super) async fn sender_loop(
    socket: Arc<UdpSocket>,
    mut receiver: mpsc::Receiver<OutgoingDatagram>,
    running: Arc<Mutex<bool>>,
    metrics: Arc<Metrics>,
) {
    // A complete 48 KiB onion message is ~417 cells. Blasting them into a
    // default UDP receive buffer (and the receiver's per-cell accounting)
    // overflows and silently loses fragments, so pace the socket every
    // `SEND_BURST` datagrams. The pause is tiny compared with a circuit's
    // round-trip time and keeps bulk replies intact on loopback and LAN links.
    const SEND_BURST: usize = 32;
    const SEND_PAUSE: Duration = Duration::from_millis(1);
    // The bound socket's family decides how destinations are written: an
    // IPv6 (including dual-stack `[::]`) socket must address IPv4 peers
    // v4-mapped. Resolve it once instead of per datagram.
    let local = socket.local_addr().ok();
    let mut sent_since_pause = 0usize;
    while *running.lock().unwrap() {
        tokio::select! {
            Some((addr, bytes)) = receiver.recv() => {
                let dest = match local {
                    Some(local) => map_dest_for(addr, local),
                    None => addr,
                };
                if socket.send_to(&bytes, dest).await.is_ok() {
                    metrics.record_datagram_sent(bytes.len());
                }
                sent_since_pause += 1;
                if sent_since_pause >= SEND_BURST {
                    sent_since_pause = 0;
                    tokio::time::sleep(SEND_PAUSE).await;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                // Small sleep to prevent busy loop when channel is empty
            }
        }
    }
}

/// Warn once per remote wire version, so a mixed-version swarm is diagnosable
/// instead of looking like a network that silently drops everything.
fn warn_version_mismatch(peer_version: u16) {
    static LAST_WARNED: AtomicU16 = AtomicU16::new(u16::MAX);
    if LAST_WARNED.swap(peer_version, Ordering::Relaxed) != peer_version {
        eprintln!(
            "[Network] Ignoring traffic from a peer speaking PlainWeb wire v{peer_version}, but this build speaks v{}. Update one of the two.",
            envelope::WIRE_VERSION
        );
    }
}

/// A clock that is minutes fast makes every handshake fail with no visible
/// reason. Say it once, instead of leaving a silent, total failure.
fn warn_clock_skew() {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "[Network] Dropping datagrams stamped more than 10 minutes in the future. Check this machine's clock if it is not set automatically."
        );
    }
}

/// Inbound receiver loop: reads UDP datagrams, verifies their envelope
/// signature, attributes them to the signing key, reassembles fragments,
/// rejects replays, and enqueues the decoded message for the handler loop.
///
/// Nothing reaches the handler — or application subscribers — unless it was
/// signed by the key it claims to come from, so a LAN or on-path attacker
/// cannot inject messages attributed to someone else.
pub(super) async fn receiver_loop(
    socket: Arc<UdpSocket>,
    sender: mpsc::Sender<(SocketAddr, PublicKey, NetworkMessage)>,
    running: Arc<Mutex<bool>>,
    state: InboundState,
) {
    let mut buf = [0u8; 65535];
    let mut limiter = PeerRateLimiter::default();
    let mut cell_limiter = OnionRateLimiter::default();
    // Hole-punch probes are unauthenticated until decoded, so they get a
    // separate per-source budget; the reply is always a same-size probe to the
    // source address, so there is no amplification.
    let mut probe_limiter = TokenBuckets::<IpAddr>::new(32.0, 128.0, RATE_TRACKED_KEYS);
    while *running.lock().unwrap() {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                let (len, from) = match result {
                    Ok((len, from)) => (len, unmap_source(from)),
                    // Transient socket error; keep listening.
                    Err(_) => continue,
                };
                state.metrics.record_datagram_received(len);

                if len == 113 && buf.starts_with(PUNCH_MAGIC) {
                    if !probe_limiter.accept(&from.ip(), Instant::now()) {
                        continue;
                    }
                    if let Some((sender, nonce)) = decode_punch_probe(&buf[..len]) {
                        if sender != state.local_key {
                            let reply =
                                encode_punch_probe(&state.local_keypair, nonce);
                            let _ = state.outgoing_tx.try_send((from, reply));
                            if let Some(waiter) = state
                                .punch_waiters
                                .lock()
                                .unwrap()
                                .get(&sender)
                                .cloned()
                            {
                                let _ = waiter.try_send(from);
                            }
                        }
                    }
                    continue;
                }

                // Onion cells share the socket but not the envelope format:
                // they are recognised by their fixed size/version byte and
                // peeled before envelope decoding. A relay never needs to
                // authenticate the previous hop; layer AEAD is the check.
                if len == CELL_SIZE && buf[0] == crate::onion::ONION_VERSION {
                    // Cells are unauthenticated until peeled, so the
                    // authenticated peer bucket below cannot see them. Drop
                    // over-quota cells before spending a peel on them.
                    if !cell_limiter.accept(from.ip(), Instant::now()) {
                        state.metrics.record_onion_rate_limited();
                        continue;
                    }
                    let now = Instant::now();
                    if !state.onion_replay.lock().unwrap().accept(&buf[..len], now) {
                        state.metrics.record_onion_replay();
                        continue;
                    }
                    state.metrics.record_onion_cell();
                    handle_onion_datagram(&buf[..len], from, &state);
                    continue;
                }

                // Invalid framing, bad signatures, and future-dated envelopes
                // are dropped without reaching the rest of the node.
                let envelope = match envelope::decode_for(&buf[..len], &state.local_dh, unix_time())
                {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        state.metrics.record_invalid();
                        match error {
                            envelope::EnvelopeError::UnsupportedVersion(peer) => {
                                warn_version_mismatch(peer)
                            }
                            envelope::EnvelopeError::ClockSkew => warn_clock_skew(),
                            _ => {}
                        }
                        continue;
                    }
                };
                if envelope.sender == state.local_key {
                    continue; // never process our own traffic
                }

                let Some(message) = state
                    .reassembly
                    .lock()
                    .unwrap()
                    .insert(&envelope, from, Instant::now())
                else {
                    continue; // fragment buffered / duplicate
                };

                if !state.replay.lock().unwrap().accept(&message.sender, message.seq) {
                    state.metrics.record_replay();
                    continue; // replayed message
                }

                let Ok(mut decoded) =
                    serde_json::from_slice::<NetworkMessage>(&message.bytes)
                else {
                    state.metrics.record_invalid();
                    continue;
                };

                // The identity inside the payload must be the key that signed
                // the envelope; otherwise one peer could speak for another.
                if !claimed_identity_matches(&decoded, &message.sender) {
                    state.metrics.record_invalid();
                    continue;
                }
                // Variants with their own signature (gossip announcements,
                // index postings) must carry a valid one too, so a relay
                // cannot rewrite content attributed to the publisher.
                if !payload_signature_valid(&decoded) {
                    state.metrics.record_invalid();
                    continue;
                }

                // E2EE: open a sealed payload here, where the local identity
                // and the anti-replay cache live. Everything downstream —
                // subscribers, the handler, relay bookkeeping — sees only the
                // inner message; a payload that will not open is dropped.
                if matches!(decoded, NetworkMessage::Sealed { .. }) {
                    match open_sealed(decoded, &message.sender, &state) {
                        Some(inner) => {
                            decoded = inner;
                            state.metrics.record_e2ee_opened();
                        }
                        None => {
                            state.metrics.record_e2ee_dropped();
                            continue;
                        }
                    }
                }
                if !payload_well_formed(&decoded) {
                    state.metrics.record_invalid();
                    continue;
                }

                // Per-peer quota: a peer that floods valid messages is dropped
                // here, before peer bookkeeping or application delivery.
                if !limiter.accept(&message.sender, Instant::now()) {
                    state.metrics.record_message_rate_limited();
                    continue;
                }

                // A relay-delivered envelope for us is opened here, where the
                // replay guard and fragment reassembler live. The inner
                // datagrams are the originator's own signed envelopes, so the
                // relay cannot read or alter them; it only supplied routing.
                if let NetworkMessage::RelayDeliver {
                    origin,
                    origin_addr,
                    target,
                    datagrams,
                } = &decoded
                {
                    if *target == state.local_key {
                        state.metrics.record_relay_delivered();
                        let relay = message.sender;
                        for datagram in datagrams.iter().take(64) {
                            let Ok(inner_envelope) =
                                envelope::decode_for(datagram, &state.local_dh, unix_time())
                            else {
                                continue;
                            };
                            if inner_envelope.sender != *origin
                                || inner_envelope.sender == state.local_key
                            {
                                continue;
                            }
                            let Some(inner_message) = state
                                .reassembly
                                .lock()
                                .unwrap()
                                .insert(&inner_envelope, *origin_addr, Instant::now())
                            else {
                                continue;
                            };
                            if !state
                                .replay
                                .lock()
                                .unwrap()
                                .accept(&inner_message.sender, inner_message.seq)
                            {
                                continue;
                            }
                            let Ok(mut inner) =
                                serde_json::from_slice::<NetworkMessage>(&inner_message.bytes)
                            else {
                                continue;
                            };
                            // The originator may seal payloads end-to-end:
                            // open them here before the shared checks below.
                            if matches!(inner, NetworkMessage::Sealed { .. }) {
                                let Some(opened) =
                                    open_sealed(inner, &inner_message.sender, &state)
                                else {
                                    continue;
                                };
                                inner = opened;
                            }
                            if matches!(inner, NetworkMessage::RelayDeliver { .. })
                                || !claimed_identity_matches(&inner, &inner_message.sender)
                                || !payload_signature_valid(&inner)
                                || !payload_well_formed(&inner)
                            {
                                continue;
                            }
                            if !limiter.accept(&inner_message.sender, Instant::now()) {
                                state.metrics.record_message_rate_limited();
                                continue;
                            }
                            state
                                .relay_routes
                                .lock()
                                .unwrap()
                                .insert(inner_message.sender, relay);
                            register_peer_relayed(
                                &state,
                                &inner_message.sender,
                                *origin_addr,
                            );
                            state.metrics.record_message_received();
                            let _ = sender.try_send((
                                *origin_addr,
                                inner_message.sender,
                                inner,
                            ));
                        }
                        continue;
                    }
                    // Addressed elsewhere: let the handler forward it.
                }

                state.metrics.record_message_received();
                register_peer(&state, &message.sender, message.from);
                // A slow handler drops traffic instead of growing the queue;
                // UDP already treats loss as normal, and the per-peer bucket
                // keeps one sender from monopolising the queue.
                let _ = sender.try_send((message.from, message.sender, decoded));
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                // Small sleep to prevent busy loop
            }
        }
    }
}

/// Insert or refresh an authenticated peer, always using the datagram's real
/// source address. Unknown senders are only added while the table has room.
fn register_peer(state: &InboundState, sender: &PublicKey, from: SocketAddr) {
    // Direct traffic proves the peer is dialable at this address, so any
    // previous relay route is no longer needed.
    state.relay_routes.lock().unwrap().remove(sender);
    register_peer_at(state, sender, from);
}

/// Register a peer reached through a relay at the address the origin
/// advertised. The relay route is installed by the caller; replies are wrapped
/// and sent to the relay instead of this (possibly private) address.
fn register_peer_relayed(state: &InboundState, sender: &PublicKey, from: SocketAddr) {
    register_peer_at(state, sender, from);
}

fn register_peer_at(state: &InboundState, sender: &PublicKey, from: SocketAddr) {
    let now = Instant::now();
    let mut table = state.peer_table.lock().unwrap();
    if let Some(peer) = table.get_mut(sender) {
        peer.last_seen = now;
        peer.address = from;
    } else if table.len() < state.max_peers {
        table.insert(
            *sender,
            PeerInfo {
                public_key: *sender,
                last_seen: now,
                address: from,
                reputation: 0,
            },
        );
    }
    drop(table);

    // Feed the Kademlia table as well. A full bucket surfaces its LRU head for
    // a ping; the existing peer-liveness loop already pings everyone, so the
    // head is refreshed or evicted by normal traffic.
    if let Ok(mut routing) = state.routing.lock() {
        let contact = Contact::new(*sender, from);
        if let UpdateOutcome::PingLeastRecent { least_recent } = routing.update(contact, now) {
            let _ = routing.mark_alive(&least_recent.node_id, None, now);
        }
    }
}

/// Peel, forward, or deliver one inbound onion cell.
///
/// Authorisation is the cell's per-hop AEAD: only the addressed relay can
/// open its front slot. A relay re-originates the peeled cell from this node's
/// own socket, so the next hop observes only this relay's address. Exit hops
/// reassemble the multi-cell message, verify the originator's signature, and
/// hand it to onion subscribers.
fn handle_onion_datagram(datagram: &[u8], from: SocketAddr, state: &InboundState) {
    let Ok(cell) = OnionCell::from_bytes(datagram) else {
        return;
    };
    let Ok(Some(peeled)) = cell.peel(&state.local_dh) else {
        return;
    };

    match peeled.action {
        OnionAction::Forward(next) => {
            // Never route a cell back to this node: a cell addressed to our
            // own onion key would otherwise loop locally and mint credit.
            if next == state.local_dh.public {
                return;
            }
            let Some(addr) = resolve_onion_address(state, &next) else {
                return;
            };
            let Some(forward) = peeled.forward else {
                return;
            };
            // Count the cell only once it is actually queued for the next
            // hop. Peeling a cell that names an unknown relay, or that the
            // outgoing queue rejects, is not relay service and must not mint.
            if state
                .outgoing_tx
                .try_send((addr, forward.to_bytes().to_vec()))
                .is_err()
            {
                state.metrics.record_send_queue_drop();
                return;
            }
            state.metrics.record_onion_relayed();
            report_relay(state, from, datagram);
        }
        OnionAction::Deliver => {
            let Some(payload) = peeled.payload else {
                return;
            };
            let Some(inner) = state
                .onion_assembly
                .lock()
                .unwrap()
                .insert(&payload, Instant::now())
            else {
                return;
            };
            let Some(decoded) = onion_transport::decode_message(&inner) else {
                return;
            };
            let Ok(mut message) = serde_json::from_slice::<NetworkMessage>(&decoded.message) else {
                return;
            };
            if !claimed_identity_matches(&message, &decoded.sender)
                || !payload_signature_valid(&message)
            {
                return;
            }
            // The originator may seal the payload end-to-end: the exit only
            // routes it, so open it here before anything downstream sees it.
            if matches!(message, NetworkMessage::Sealed { .. }) {
                let Some(opened) = open_sealed(message, &decoded.sender, state) else {
                    state.metrics.record_e2ee_dropped();
                    return;
                };
                message = opened;
                state.metrics.record_e2ee_opened();
            }
            if !payload_well_formed(&message) {
                return;
            }

            // Delivery is the exit role, not relaying: it earns no relay
            // credit (only forwarded cells do), which also stops a sender
            // from minting by sealing valid messages to this node.
            match decoded.kind {
                // A reply resolves the matching in-flight request wait; it is
                // never surfaced as a new inbound request.
                OnionKind::Reply => {
                    let waiter = state
                        .pending_onion_replies
                        .lock()
                        .unwrap()
                        .remove(&decoded.request_id);
                    if let Some(waiter) = waiter {
                        let _ = waiter.try_send(message);
                    }
                }
                OnionKind::Request => {
                    state.metrics.record_onion_request();
                    let request = OnionRequest {
                        sender: decoded.sender,
                        request_id: decoded.request_id,
                        return_path: decoded.return_path,
                        message: message.clone(),
                    };
                    try_deliver(
                        &mut state.onion_request_subscribers.lock().unwrap(),
                        request,
                    );
                    // Legacy listener: originator and message only.
                    try_deliver(
                        &mut state.onion_subscribers.lock().unwrap(),
                        (decoded.sender, message.clone()),
                    );
                }
            }
        }
    }
}

/// Deliver one message to every live subscriber.
///
/// A full subscriber queue drops the message (the subscriber is simply too
/// slow) while a closed one is removed. Neither case grows the queue or blocks
/// the engine, so a stalled application cannot exhaust memory.
fn try_deliver<T: Clone>(subscribers: &mut Vec<mpsc::Sender<T>>, message: T) {
    subscribers.retain(|tx| {
        !matches!(
            tx.try_send(message.clone()),
            Err(mpsc::error::TrySendError::Closed(_))
        )
    });
}

/// Report one genuinely forwarded cell to the relay-credit listeners.
fn report_relay(state: &InboundState, from: SocketAddr, datagram: &[u8]) {
    let event = RelayEvent {
        from,
        bytes: CELL_SIZE as u64,
        cell: datagram.to_vec(),
    };
    try_deliver(&mut state.relay_subscribers.lock().unwrap(), event);
}

/// Map a peeled next-hop onion key to the authenticated peer address it was
/// last seen at. Unregistered relays are dropped rather than guessed.
fn resolve_onion_address(state: &InboundState, next: &DhPublicKey) -> Option<SocketAddr> {
    let table = state.peer_table.lock().unwrap();
    table
        .iter()
        .find(|(key, _)| DhPublicKey::from_ed25519(key) == Some(*next))
        .map(|(_, peer)| peer.address)
}

/// Message variants that carry an identity field without a signature over it.
/// The engine requires that field to equal the envelope signer.
fn claimed_identity_matches(message: &NetworkMessage, signer: &PublicKey) -> bool {
    match message {
        NetworkMessage::SearchQuery { sender, .. }
        | NetworkMessage::ShardQuery { sender, .. }
        | NetworkMessage::RegistrySyncRequest { sender }
        | NetworkMessage::DomainQuery { sender, .. }
        | NetworkMessage::PricingEpoch { sender, .. }
        | NetworkMessage::AttestBalance { sender, .. }
        | NetworkMessage::WithdrawCommit { sender, .. }
        | NetworkMessage::WithdrawSign { sender, .. }
        | NetworkMessage::FindNode { sender, .. }
        | NetworkMessage::Nodes { sender, .. }
        | NetworkMessage::Chat { sender, .. }
        | NetworkMessage::ChatAck { sender, .. }
        | NetworkMessage::RoomSubscribe { sender, .. }
        | NetworkMessage::RoomSync { sender, .. }
        | NetworkMessage::RoomHistory { sender, .. }
        | NetworkMessage::BlogSubscribe { sender, .. }
        | NetworkMessage::BlogSync { sender }
        | NetworkMessage::BlogHistory { sender, .. } => sender == signer,
        NetworkMessage::CheckIndexProof { proof, .. } => proof.miner == *signer,
        NetworkMessage::ProofVerdict { verifier, .. } => verifier == signer,
        NetworkMessage::ClaimDomainAttested { owner, .. }
        | NetworkMessage::ClaimDomainBlind { owner, .. } => owner == signer,
        NetworkMessage::PeerDiscovery { sender_pubkey, .. }
        | NetworkMessage::PeerExchange { sender_pubkey, .. } => sender_pubkey == signer,
        NetworkMessage::StorePosting { publisher, .. }
        | NetworkMessage::StorePostings { publisher, .. } => publisher == signer,
        NetworkMessage::GossipAnnounce { publisher, .. } => publisher == signer,
        NetworkMessage::ShardResponse { responder, .. } => responder == signer,
        _ => true,
    }
}

/// Verify signatures that live inside the payload, for variants that carry
/// data a peer could otherwise rewrite while keeping a truthful envelope
/// identity.
fn payload_signature_valid(message: &NetworkMessage) -> bool {
    match message {
        NetworkMessage::GossipAnnounce {
            content_id,
            title,
            publisher,
            signature,
        } => signature.verify(
            publisher,
            &crate::net::gossip_announce_sign_message(content_id, title, publisher),
        ),
        NetworkMessage::StorePosting {
            posting,
            publisher,
            signature,
        } => signature.verify(
            publisher,
            &crate::net::posting_sign_message(posting, publisher),
        ),
        NetworkMessage::StorePostings {
            postings,
            publisher,
            signature,
        } => signature.verify(
            publisher,
            &crate::net::postings_sign_message(postings, publisher),
        ),
        NetworkMessage::CheckIndexProof { proof, .. } => proof.signature.verify(
            &proof.miner,
            &crate::net::index_proof_sign_message(&proof.content_id, &proof.term_hash),
        ),
        // An attested claim's owner signature is bound to the funding it
        // cites; the funding citation must match the presented certificate
        // (checked by the domain service), so a stripped or swapped citation
        // fails here before reaching any application handler.
        NetworkMessage::ClaimDomainAttested {
            domain,
            target_content_id,
            owner,
            credits_spent,
            registered_at_epoch,
            funding_head,
            funding_balance,
            signature,
            ..
        } => crate::net::verify_claim_signature(
            domain,
            *target_content_id,
            owner,
            *credits_spent,
            *registered_at_epoch,
            Some((*funding_head, *funding_balance)),
            signature,
        ),
        NetworkMessage::ClaimDomainBlind {
            domain,
            target_content_id,
            owner,
            credits_spent,
            registered_at_epoch,
            signature,
            ..
        } => signature.verify(
            owner,
            &crate::net::domain_claim_sign_message(
                domain,
                *target_content_id,
                *owner,
                *credits_spent,
                *registered_at_epoch,
            ),
        ),
        // A room post is re-signed by every forwarder, so authorship lives
        // in the post's own signature; a history reply inherits that rule
        // entry by entry.
        NetworkMessage::RoomPost(post) => post.verify(),
        NetworkMessage::RoomHistory { posts, .. } => posts.iter().all(RoomPostWire::verify),
        // A blog entry carries its author's signature exactly like a room
        // post does: the forwarder re-signs the envelope, so the author is
        // proved by the entry itself.
        NetworkMessage::BlogEntry(entry) => entry.verify(),
        NetworkMessage::BlogHistory { entries, .. } => entries.iter().all(BlogEntryWire::verify),
        _ => true,
    }
}

/// Shape checks that are not signatures: a chat payload must respect the
/// length cap, and a room or blog message its own caps, so one peer cannot
/// push megabytes of text into an application handler through an otherwise
/// valid envelope.
fn payload_well_formed(message: &NetworkMessage) -> bool {
    match message {
        NetworkMessage::Chat { text, .. } => text.len() <= MAX_CHAT_TEXT,
        NetworkMessage::RoomPost(post) => {
            post.text.len() <= MAX_CHAT_TEXT
                && crate::net::is_valid_room_name(&post.room)
                && post.hops > 0
        }
        NetworkMessage::RoomSubscribe { room, .. } | NetworkMessage::RoomSync { room, .. } => {
            crate::net::is_valid_room_name(room)
        }
        NetworkMessage::RoomHistory { room, posts, .. } => {
            crate::net::is_valid_room_name(room)
                && posts.len() <= crate::net::ROOM_HISTORY_MAX
                && posts.iter().all(|post| {
                    post.text.len() <= MAX_CHAT_TEXT && crate::net::is_valid_room_name(&post.room)
                })
        }
        NetworkMessage::BlogEntry(entry) => {
            entry.title.len() <= crate::net::MAX_BLOG_TITLE
                && entry.text.len() <= crate::net::MAX_BLOG_TEXT
                && entry.hops > 0
        }
        NetworkMessage::BlogHistory { entries, .. } => {
            entries.len() <= crate::net::BLOG_HISTORY_MAX
                && entries.iter().all(|entry| {
                    entry.title.len() <= crate::net::MAX_BLOG_TITLE
                        && entry.text.len() <= crate::net::MAX_BLOG_TEXT
                })
        }
        _ => true,
    }
}

/// Open an E2EE-sealed message with the local identity and validate the inner
/// payload against the envelope signer. Non-sealed messages pass through
/// unchanged. Returns `None` when the sealed payload fails to open (wrong
/// recipient, tamper, replay, nested seal) or when the inner message does not
/// satisfy the same identity/signature/shape checks as an unsealed one.
fn open_sealed(
    message: NetworkMessage,
    signer: &PublicKey,
    state: &InboundState,
) -> Option<NetworkMessage> {
    if !matches!(message, NetworkMessage::Sealed { .. }) {
        return Some(message);
    }
    let inner = e2ee::open(
        &state.local_key,
        &state.local_dh,
        signer,
        &message,
        &state.e2ee,
    )?;
    if !claimed_identity_matches(&inner, signer)
        || !payload_signature_valid(&inner)
        || !payload_well_formed(&inner)
    {
        return None;
    }
    Some(inner)
}

/// Process an incoming network message.
///
/// `sender_addr` is the real UDP source address of the datagram and
/// `sender_key` is the key that signed its envelope; both are used instead of
/// trusting identity fields inside the payload.
async fn process_message(
    message: NetworkMessage,
    sender_addr: SocketAddr,
    sender_key: &PublicKey,
    context: &ProcessContext<'_>,
) {
    let ProcessContext {
        peer_table,
        config,
        outgoing_tx,
        seq,
        routing,
        relay_routes,
        advertised_addr,
    } = context;
    match message {
        // Peer discovery: the sender is already registered by the receiver.
        // `known_peers` carries keys without addresses, and inventing the
        // sender's address for them would poison the table, so they are not
        // registered until they talk to us (or arrive via PeerExchange, which
        // does carry addresses).
        NetworkMessage::PeerDiscovery { .. } => {}

        // Liveness probe: answer at the source address the datagram actually
        // came from, signed with this node's key.
        NetworkMessage::Ping { nonce } => send_signed_route(
            config,
            peer_table,
            relay_routes,
            outgoing_tx,
            seq,
            *advertised_addr,
            sender_key,
            sender_addr,
            NetworkMessage::Pong { nonce },
        ),

        // PeerExchange advertises dialable addresses. Merge only entries with
        // usable addresses and never overwrite existing knowledge with them;
        // the advertised peer still has to authenticate before it is trusted.
        NetworkMessage::PeerExchange { peers, .. } => {
            merge_peers(peers, peer_table, config, routing);
        }

        // Nodes is the reply to FindNode: merge the discovered contacts, then
        // the caller (or the next lookup round) sees them in the DHT table.
        NetworkMessage::Nodes { peers, .. } => {
            merge_peers(peers, peer_table, config, routing);
        }

        // Kademlia FIND_NODE: return the closest contacts we know for target.
        NetworkMessage::FindNode { target, .. } => {
            let target = NodeId::from_bytes(target);
            let contacts: Vec<(SocketAddr, PublicKey)> = routing
                .lock()
                .unwrap()
                .closest(&target, 8)
                .into_iter()
                .map(|contact| (contact.address, contact.public_key))
                .collect();
            if contacts.is_empty() {
                return;
            }
            let response = NetworkMessage::Nodes {
                target: *target.as_bytes(),
                peers: contacts,
                sender: config.keypair.public,
            };
            send_signed_route(
                config,
                peer_table,
                relay_routes,
                outgoing_tx,
                seq,
                *advertised_addr,
                sender_key,
                sender_addr,
                response,
            );
        }

        // NAT rendezvous: signal both sides with each other's observed source
        // address. Only known peers can be targets, so a peer cannot turn the
        // node into an arbitrary UDP probe reflector.
        NetworkMessage::PunchRequest { target } => {
            let local = config.keypair.public;
            if target == local || target == *sender_key {
                return;
            }
            let target_addr = {
                let peers = peer_table.lock().unwrap();
                peers.get(&target).map(|peer| peer.address)
            };
            let Some(target_addr) = target_addr else {
                return;
            };
            // Tell the target to probe the requester's real source address.
            send_signed_route(
                config,
                peer_table,
                relay_routes,
                outgoing_tx,
                seq,
                *advertised_addr,
                &target,
                target_addr,
                NetworkMessage::PunchSignal {
                    from: *sender_key,
                    from_addr: sender_addr,
                },
            );
            // Tell the requester where the target's datagrams really came
            // from (its NAT-mapped address, not necessarily its advertised one).
            send_signed_route(
                config,
                peer_table,
                relay_routes,
                outgoing_tx,
                seq,
                *advertised_addr,
                sender_key,
                sender_addr,
                NetworkMessage::PunchSignal {
                    from: target,
                    from_addr: target_addr,
                },
            );
        }

        // A rendezvous says: this peer is trying to reach you; open your NAT
        // mapping with a few signed probes. Only already-known peers are
        // probed, so the signal is not an arbitrary UDP scanning primitive.
        NetworkMessage::PunchSignal { from, from_addr } => {
            if from == config.keypair.public {
                return;
            }
            if !peer_table.lock().unwrap().contains_key(&from) {
                return;
            }
            let probe = encode_punch_probe(&config.keypair, rand::random());
            for _ in 0..3 {
                let _ = outgoing_tx.try_send((from_addr, probe.clone()));
            }
        }

        // Relay role: forward an end-to-end message to its target unchanged.
        // The outer envelope is re-signed by this relay, but the inner
        // datagrams keep the originator's signature and encryption, so the
        // relay cannot read or alter them. A relayed relay target is refused
        // to keep the fallback to exactly one hop.
        NetworkMessage::RelayDeliver {
            origin,
            origin_addr,
            target,
            datagrams,
        } => {
            if target == config.keypair.public {
                return; // local delivery is handled by the receiver loop
            }
            let (target_addr, target_is_relayed) = {
                let peers = peer_table.lock().unwrap();
                match peers.get(&target) {
                    Some(peer) => (
                        peer.address,
                        relay_routes.lock().unwrap().contains_key(&target),
                    ),
                    None => return,
                }
            };
            if target_is_relayed {
                return;
            }
            let Some(recipient) = DhPublicKey::from_ed25519(&target) else {
                return;
            };
            let forwarded = NetworkMessage::RelayDeliver {
                origin,
                origin_addr,
                target,
                datagrams,
            };
            let Ok(bytes) = serde_json::to_vec(&forwarded) else {
                return;
            };
            let Ok(encoded) = envelope::encode_for(
                &config.keypair,
                &recipient,
                seq.fetch_add(1, Ordering::Relaxed),
                unix_time(),
                &bytes,
            ) else {
                return;
            };
            for datagram in encoded {
                let _ = outgoing_tx.try_send((target_addr, datagram));
            }
        }

        // Everything else is handled by the application layer; the sender has
        // already been registered/refreshed by the receiver loop.
        _ => {
            let _ = sender_key;
        }
    }
}

/// Send one engine control message to `recipient`, respecting a relay route.
///
/// Ping/Pong, punch signals, and DHT replies are generated inside the handler
/// loop, which has no `NodeEngine`; without this helper they would bypass the
/// relay route and try to reach a NAT'd origin directly. The inner envelope
/// stays end-to-end encrypted for the recipient; only the routing wrapper is
/// readable by the relay.
#[allow(clippy::too_many_arguments)]
fn send_signed_route(
    config: &NodeConfig,
    peer_table: &Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    relay_routes: &Arc<Mutex<HashMap<PublicKey, PublicKey>>>,
    outgoing_tx: &mpsc::Sender<OutgoingDatagram>,
    seq: &Arc<AtomicU64>,
    advertised_addr: SocketAddr,
    recipient: &PublicKey,
    direct_addr: SocketAddr,
    message: NetworkMessage,
) {
    let Some(recipient_dh) = DhPublicKey::from_ed25519(recipient) else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(&message) else {
        return;
    };
    let Ok(inner) = envelope::encode_for(
        &config.keypair,
        &recipient_dh,
        seq.fetch_add(1, Ordering::Relaxed),
        unix_time(),
        &bytes,
    ) else {
        return;
    };
    let via = relay_routes.lock().unwrap().get(recipient).copied();
    let Some(relay) = via else {
        for datagram in inner {
            let _ = outgoing_tx.try_send((direct_addr, datagram));
        }
        return;
    };
    let (relay_addr, relay_key) = {
        let peers = peer_table.lock().unwrap();
        match peers.get(&relay) {
            Some(peer) => (peer.address, peer.public_key),
            None => return,
        }
    };
    let Some(relay_dh) = DhPublicKey::from_ed25519(&relay_key) else {
        return;
    };
    let wrapper = NetworkMessage::RelayDeliver {
        origin: config.keypair.public,
        origin_addr: advertised_addr,
        target: *recipient,
        datagrams: inner,
    };
    let Ok(bytes) = serde_json::to_vec(&wrapper) else {
        return;
    };
    let Ok(outer) = envelope::encode_for(
        &config.keypair,
        &relay_dh,
        seq.fetch_add(1, Ordering::Relaxed),
        unix_time(),
        &bytes,
    ) else {
        return;
    };
    for datagram in outer {
        let _ = outgoing_tx.try_send((relay_addr, datagram));
    }
}

/// Merge an advertised peer list into both the flat peer table and the DHT.
/// Invalid/self addresses are ignored; advertised entries still have to
/// authenticate before they become trusted for encryption.
fn merge_peers(
    peers: Vec<(SocketAddr, PublicKey)>,
    peer_table: &Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    config: &NodeConfig,
    routing: &Arc<Mutex<RoutingTable>>,
) {
    let local_key = config.keypair.public;
    let now = Instant::now();
    let mut pt = peer_table.lock().unwrap();
    let mut rt = routing.lock().unwrap();
    for (addr, pubkey) in peers {
        if pubkey == local_key || addr.port() == 0 || addr.ip().is_unspecified() {
            continue;
        }
        pt.entry(pubkey).or_insert_with(|| PeerInfo {
            public_key: pubkey,
            last_seen: now,
            address: addr,
            reputation: 0,
        });
        let contact = Contact::new(pubkey, addr);
        if let UpdateOutcome::PingLeastRecent { least_recent } = rt.update(contact, now) {
            let _ = rt.mark_alive(&least_recent.node_id, None, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_burst_then_refills_over_time() {
        let mut limiter = PeerRateLimiter::new(2.0, 4.0, 8);
        let peer = PublicKey([1u8; 32]);
        let start = Instant::now();

        for _ in 0..4 {
            assert!(limiter.accept(&peer, start), "burst tokens must be spent");
        }
        assert!(!limiter.accept(&peer, start), "burst is exhausted");

        // Two tokens per second: half a second restores exactly one.
        let later = start + Duration::from_millis(500);
        assert!(limiter.accept(&peer, later));
        assert!(!limiter.accept(&peer, later), "refilled token is consumed");

        // A longer silence is capped at the burst, not accumulated forever.
        let much_later = later + Duration::from_secs(60);
        for _ in 0..4 {
            assert!(limiter.accept(&peer, much_later));
        }
        assert!(!limiter.accept(&peer, much_later));
    }

    #[test]
    fn rate_limiter_tracks_peers_independently() {
        let mut limiter = PeerRateLimiter::new(0.0, 1.0, 8);
        let now = Instant::now();
        let first = PublicKey([2u8; 32]);
        let second = PublicKey([3u8; 32]);

        assert!(limiter.accept(&first, now));
        assert!(!limiter.accept(&first, now));
        // The second peer has its own full bucket.
        assert!(limiter.accept(&second, now));
    }

    #[test]
    fn rate_limiter_bounds_tracked_peers() {
        let mut limiter = PeerRateLimiter::new(0.0, 1.0, 2);
        let start = Instant::now();
        let a = PublicKey([4u8; 32]);
        let b = PublicKey([5u8; 32]);
        let c = PublicKey([6u8; 32]);

        assert!(limiter.accept(&a, start));
        assert!(limiter.accept(&b, start + Duration::from_millis(1)));
        assert!(limiter.accept(&c, start + Duration::from_millis(2)));
        assert_eq!(limiter.buckets.len(), 2, "capacity must bound the map");
        assert!(!limiter.buckets.contains_key(&a), "oldest bucket evicted");

        // The evicted peer starts fresh rather than inheriting a penalty.
        assert!(limiter.accept(&a, start + Duration::from_millis(3)));
    }

    #[test]
    fn onion_limiter_drops_one_source_flood_without_penalising_others() {
        // Two cells burst per source, no refill; a generous global budget.
        let mut limiter = OnionRateLimiter::new(0.0, 2.0, 100.0, 100.0, 8);
        let now = Instant::now();
        let noisy = "192.0.2.10".parse::<IpAddr>().unwrap();
        let quiet = "198.51.100.20".parse::<IpAddr>().unwrap();

        assert!(limiter.accept(noisy, now));
        assert!(limiter.accept(noisy, now));
        assert!(!limiter.accept(noisy, now), "per-source burst is exhausted");
        assert!(
            limiter.accept(quiet, now),
            "other sources keep their bucket"
        );
    }

    #[test]
    fn onion_limiter_enforces_a_global_budget_across_spoofed_sources() {
        // One cell total per key, one global token: spoofing a new source for
        // every datagram cannot multiply the number of peels.
        let mut limiter = OnionRateLimiter::new(100.0, 100.0, 0.0, 1.0, 8);
        let now = Instant::now();
        assert!(limiter.accept("192.0.2.1".parse().unwrap(), now));
        assert!(!limiter.accept("192.0.2.2".parse().unwrap(), now));
        assert!(!limiter.accept("192.0.2.1".parse().unwrap(), now));
    }

    #[test]
    fn a_full_subscriber_drops_messages_without_growing_or_blocking() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut subs = vec![tx];

        try_deliver(&mut subs, 1u8);
        // The queue is full: this message is dropped, not buffered.
        try_deliver(&mut subs, 2u8);

        assert_eq!(subs.len(), 1, "a slow subscriber stays subscribed");
        assert_eq!(rx.try_recv().unwrap(), 1);
        assert!(rx.try_recv().is_err(), "nothing was queued behind it");
    }

    #[test]
    fn closed_subscribers_are_dropped() {
        let (tx, rx) = mpsc::channel::<u8>(1);
        drop(rx);
        let mut subs = vec![tx];
        try_deliver(&mut subs, 1u8);
        assert!(subs.is_empty());
    }

    #[test]
    fn default_onion_limiter_admits_a_full_size_circuit() {
        // One 48 KiB request frames into ~470 cells that all arrive at once
        // from the origin's address; the default burst must not drop any.
        let mut limiter = OnionRateLimiter::default();
        let now = Instant::now();
        let source = "198.51.100.9".parse::<IpAddr>().unwrap();
        for cell in 0..480 {
            assert!(
                limiter.accept(source, now),
                "cell {cell} of a legitimate circuit was dropped"
            );
        }
    }

    #[test]
    fn onion_limiter_refills_over_time() {
        let mut limiter = OnionRateLimiter::new(2.0, 1.0, 100.0, 100.0, 8);
        let start = Instant::now();
        let source = "203.0.113.7".parse::<IpAddr>().unwrap();

        assert!(limiter.accept(source, start));
        assert!(!limiter.accept(source, start));
        assert!(limiter.accept(source, start + Duration::from_millis(500)));
    }

    #[test]
    fn onion_replay_cache_rejects_duplicates_until_expiry() {
        let mut cache = OnionReplayCache::new(Duration::from_secs(60), 8);
        let start = Instant::now();
        let cell = [7u8; CELL_SIZE];

        assert!(cache.accept(&cell, start));
        assert!(!cache.accept(&cell, start + Duration::from_secs(30)));
        assert!(cache.accept(&cell, start + Duration::from_secs(61)));
    }

    #[test]
    fn onion_replay_cache_evicts_oldest_at_capacity() {
        let mut cache = OnionReplayCache::new(Duration::from_secs(60), 2);
        let now = Instant::now();
        assert!(cache.accept(&[1u8; CELL_SIZE], now));
        assert!(cache.accept(&[2u8; CELL_SIZE], now));
        assert!(cache.accept(&[3u8; CELL_SIZE], now));
        assert_eq!(cache.seen.len(), 2);
    }
}
