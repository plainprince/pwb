//! Public chat rooms for `chat.plain?room=<name>` (`pwb-node::room`).
//!
//! Where [`crate::chat`] is a sealed 1:1 conversation, a room is public and
//! **flooded**: [`RoomService::post`] signs a [`RoomPostWire`] as its author
//! and broadcasts it, and every holder re-broadcasts it to its own peers
//! until the hop budget runs out — so a message reaches the whole network
//! through intermediaries. Because a forwarder re-signs the transport
//! envelope, authorship comes from the post's own signature (checked by the
//! engine on every copy), never from who handed it to us.
//!
//! Subscriptions are the membership layer:
//!
//! - [`RoomService::join`] marks a room as one this node keeps — it is
//!   stored, rendered, and served as history — and announces the membership
//!   to the swarm (`RoomSubscribe`), then asks for what it missed
//!   (`RoomSync`). The announcement is sent once per session, so a page that
//!   re-renders (or a restart that reloads a persisted subscription) cannot
//!   spam the network.
//! - `RoomSubscribe` is one hop and envelope-authenticated, so it builds an
//!   honest roster of the peers known to be in a room (and each member
//!   answers a newcomer's first announcement with its own, which terminates
//!   because the roster only replies on first sight).
//! - `RoomSync` is answered directly with up to [`ROOM_HISTORY_MAX`] signed
//!   posts, so a late joiner converges instead of waiting for new traffic.
//!
//! Non-members still forward: a public room's whole point is to arrive
//! through people who are not in it. They simply do not store what passes
//! through. History, though, is served by members — only they keep it.
//!
//! Transcripts are bounded per room and written to `<data>/rooms.json`
//! after every change, merging with what is already on the file by
//! `(author, id)` so a `PWB_DATA` shared with another process survives.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pwb_core::crypto::{ContentId, Keypair, PublicKey, Signature};
use pwb_core::discovery::unix_time;
use pwb_core::net::{
    is_valid_room_name, room_post_sign_message, NetworkMessage, NodeEngine, RoomPostWire,
    MAX_CHAT_TEXT, MAX_ROOM_NAME, ROOM_HISTORY_MAX, ROOM_POST_HOPS,
};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// The room every binary joins by default — the public square.
pub const DEFAULT_ROOM: &str = "lobby";

/// Posts kept per room. Oldest are evicted, so a long-lived node cannot grow
/// without bound and a history reply stays a bounded datagram.
pub const ROOM_HISTORY_LIMIT: usize = 200;

/// File the per-room transcripts live in, under the node's data directory.
const ROOM_STORE_FILE: &str = "rooms.json";

/// Distinct post digests remembered for flood control (bounded, oldest first).
/// Evicting one only matters if the same post is still circulating much
/// later; the hop budget is what actually bounds a post's lifetime.
const MAX_SEEN_POSTS: usize = 4096;

/// Posts one author may contribute within [`AUTHOR_WINDOW_SECS`] before they
/// are throttled — applied to this node's own `try_post` and to every
/// inbound `on_post` alike. The dedup set and the hop budget bound a *single*
/// post's lifetime; this bounds how many posts one key can make a node store
/// and re-flood at all.
const AUTHOR_POST_LIMIT: u32 = 10;

/// Length of the per-author posting window, in seconds.
const AUTHOR_WINDOW_SECS: u64 = 30;

/// Authors tracked at once. Expired windows are reclaimed first; if every
/// slot is live the oldest window is dropped so the table stays bounded.
const MAX_TRACKED_AUTHORS: usize = 512;

/// Per-author sliding window counter for room posts.
///
/// The map is bounded because authors are attacker-chosen keys: a fixed-size
/// table with oldest-window eviction keeps memory constant, while a single
/// key — the case a per-author limit exists for — cannot reset its own
/// counter without a new key. A key-cycling attacker is still bounded by the
/// engine's per-peer token bucket and by the dedup set.
#[derive(Debug, Default)]
pub(crate) struct AuthorFloodControl {
    windows: HashMap<PublicKey, AuthorWindow>,
}

#[derive(Debug, Clone, Copy)]
struct AuthorWindow {
    started: u64,
    count: u32,
}

impl AuthorFloodControl {
    /// Record one post by `author` at `now`; `false` means over the limit
    /// (and the post must be dropped rather than stored or relayed).
    pub(crate) fn allow(&mut self, author: &PublicKey, now: u64) -> bool {
        if !self.windows.contains_key(author) && self.windows.len() >= MAX_TRACKED_AUTHORS {
            // Reclaim finished windows first, then the one running longest.
            self.windows
                .retain(|_, window| now.saturating_sub(window.started) < AUTHOR_WINDOW_SECS);
            if self.windows.len() >= MAX_TRACKED_AUTHORS {
                let oldest = self
                    .windows
                    .iter()
                    .min_by_key(|(_, window)| window.started)
                    .map(|(key, _)| *key);
                if let Some(key) = oldest {
                    self.windows.remove(&key);
                }
            }
        }
        let window = self.windows.entry(*author).or_insert(AuthorWindow {
            started: now,
            count: 0,
        });
        if now.saturating_sub(window.started) >= AUTHOR_WINDOW_SECS {
            window.started = now;
            window.count = 0;
        }
        if window.count >= AUTHOR_POST_LIMIT {
            return false;
        }
        window.count += 1;
        true
    }
}

/// Live state of one room: its transcript, the peers known to be here, and
/// whether this node is a member (and therefore keeps and serves it).
#[derive(Debug, Clone, Default)]
pub struct RoomState {
    /// Newest last, capped at [`ROOM_HISTORY_LIMIT`].
    pub posts: Vec<RoomPostWire>,
    /// Direct peers that announced a subscription (one hop, best effort).
    pub subscribers: Vec<PublicKey>,
    /// Whether this node has joined.
    pub subscribed: bool,
}

/// An immutable view of room state, built by [`RoomService::snapshot`] and
/// rendered by the `chat.plain?room=` page.
#[derive(Debug, Clone)]
pub struct RoomSnapshot {
    /// This node's identity (the author of its own posts).
    pub local: PublicKey,
    /// The last failure to join or post, shown on the page.
    pub error: Option<String>,
    /// State per room this node has joined or heard about.
    pub rooms: HashMap<String, RoomState>,
}

impl Default for RoomSnapshot {
    fn default() -> Self {
        Self {
            local: PublicKey([0u8; 32]),
            error: None,
            rooms: HashMap::new(),
        }
    }
}

impl RoomSnapshot {
    /// The state of `room`, when this node has it at all.
    pub fn state(&self, room: &str) -> Option<&RoomState> {
        self.rooms.get(room)
    }

    /// The members of `room` this node knows about, including itself when it
    /// has joined.
    pub fn subscriber_count(&self, room: &str) -> usize {
        self.state(room)
            .map(|state| state.subscribers.len() + usize::from(state.subscribed))
            .unwrap_or(0)
    }
}

/// On-disk form of the room file. Rooms are keyed by name; each carries the
/// membership flag and its signed posts.
#[derive(Serialize, Deserialize)]
struct RoomStoreFile {
    version: u32,
    rooms: HashMap<String, RoomStoreRoom>,
}

#[derive(Serialize, Deserialize)]
struct RoomStoreRoom {
    #[serde(default)]
    subscribed: bool,
    #[serde(default)]
    posts: Vec<RoomPostStore>,
}

/// On-disk form of one post. The wire carries keys as byte arrays like every
/// other field of `NetworkMessage`, but this file spells them hex, matching
/// `chat.json` and `peers.json` so an operator can read who wrote what.
#[derive(Serialize, Deserialize)]
struct RoomPostStore {
    sender: String,
    room: String,
    id: String,
    sent_at: u64,
    text: String,
    hops: u8,
    signature: String,
}

impl From<&RoomPostWire> for RoomPostStore {
    fn from(post: &RoomPostWire) -> Self {
        Self {
            sender: post.sender.to_hex(),
            room: post.room.clone(),
            id: hex::encode(post.id),
            sent_at: post.sent_at,
            text: post.text.clone(),
            hops: post.hops,
            signature: post.signature.to_hex(),
        }
    }
}

impl RoomPostStore {
    /// Rebuild the wire form; a field that is not valid hex drops the post
    /// (the signature check that follows drops anything malformed anyway).
    fn into_wire(self) -> Option<RoomPostWire> {
        fn key(hex: &str) -> Option<PublicKey> {
            let bytes: [u8; 32] = hex::decode(hex).ok()?.try_into().ok()?;
            Some(PublicKey(bytes))
        }
        fn id(hex: &str) -> Option<[u8; 16]> {
            hex::decode(hex).ok()?.try_into().ok()
        }
        fn signature(hex: &str) -> Option<Signature> {
            let bytes: [u8; 64] = hex::decode(hex).ok()?.try_into().ok()?;
            Some(Signature(bytes))
        }
        Some(RoomPostWire {
            sender: key(&self.sender)?,
            room: self.room,
            id: id(&self.id)?,
            sent_at: self.sent_at,
            text: self.text,
            hops: self.hops,
            signature: signature(&self.signature)?,
        })
    }
}

/// Normalize a room name the way both binaries do before it reaches the wire:
/// trimmed, lowercased, then validated.
fn normalize_room(room: &str) -> Result<String, String> {
    let room = room.trim().to_lowercase();
    if !is_valid_room_name(&room) {
        return Err(format!(
            "invalid room name — use up to {MAX_ROOM_NAME} letters, digits, - _ ."
        ));
    }
    Ok(room)
}

/// Identity of one post for flood dedup: who wrote it and which message it
/// is. The room and hops are deliberately excluded (forwarders spend hops;
/// the same id in another room is a different message only by room, which
/// the transcript dedup covers).
fn post_digest(post: &RoomPostWire) -> ContentId {
    let mut bytes = Vec::with_capacity(48);
    bytes.extend_from_slice(&post.sender.0);
    bytes.extend_from_slice(&post.id);
    ContentId::compute(&bytes)
}

/// Bounded set of post digests already handled, so flooding terminates.
///
/// Shared with [`crate::blog`]: both flooded feeds need the same "remember
/// what I already forwarded" set to stop a message circulating forever.
#[derive(Debug)]
pub(crate) struct PostDedup {
    set: HashSet<ContentId>,
    order: VecDeque<ContentId>,
    cap: usize,
}

impl PostDedup {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Record `digest` and report whether it was new.
    pub(crate) fn insert(&mut self, digest: ContentId) -> bool {
        if !self.set.insert(digest) {
            return false;
        }
        self.order.push_back(digest);
        while self.order.len() > self.cap {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.set.remove(&oldest);
        }
        true
    }
}

/// Read persisted rooms from `path`.
///
/// A missing or corrupt file yields nothing (rooms are an enhancement and
/// must never stop a node from starting); a post whose signature no longer
/// verifies is skipped rather than shown as history.
fn load_store(path: &Path) -> HashMap<String, RoomState> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    let Ok(file) = serde_json::from_slice::<RoomStoreFile>(&bytes) else {
        return HashMap::new();
    };
    file.rooms
        .into_iter()
        .filter(|(room, _)| is_valid_room_name(room))
        .map(|(room, stored)| {
            (
                room,
                RoomState {
                    posts: stored
                        .posts
                        .into_iter()
                        .filter_map(RoomPostStore::into_wire)
                        .filter(RoomPostWire::verify)
                        .take(ROOM_HISTORY_LIMIT)
                        .collect(),
                    subscribers: Vec::new(),
                    subscribed: stored.subscribed,
                },
            )
        })
        .collect()
}

/// Persist every room to `path` (atomically), merging with what is already
/// on disk: posts union by `(author, id)`, subscriptions OR together. Two
/// processes can share one `PWB_DATA` deliberately, and each only holds what
/// it has seen. Failures are reported but never propagated — a full disk
/// must not break a live conversation.
fn save_store(path: &Path, rooms: &HashMap<String, RoomState>) {
    let mut merged = load_store(path);
    for (room, state) in rooms {
        let slot = merged.entry(room.clone()).or_default();
        slot.subscribed |= state.subscribed;
        for post in &state.posts {
            if !slot
                .posts
                .iter()
                .any(|seen| seen.sender == post.sender && seen.id == post.id)
            {
                slot.posts.push(post.clone());
            }
        }
        if slot.posts.len() > ROOM_HISTORY_LIMIT {
            let overflow = slot.posts.len() - ROOM_HISTORY_LIMIT;
            slot.posts.drain(..overflow);
        }
    }
    let file = RoomStoreFile {
        version: 1,
        rooms: merged
            .into_iter()
            .map(|(room, state)| {
                (
                    room,
                    RoomStoreRoom {
                        subscribed: state.subscribed,
                        posts: state.posts.iter().map(RoomPostStore::from).collect(),
                    },
                )
            })
            .collect(),
    };
    let json = match serde_json::to_vec_pretty(&file) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("[Room] Could not serialize rooms: {error}");
            return;
        }
    };
    if let Err(error) = crate::persist::atomic_write(path, &json) {
        eprintln!(
            "[Room] Could not persist rooms to {}: {error}",
            path.display()
        );
    }
}

/// Public rooms over the node's engine: signed posts that flood with a hop
/// budget, membership announced one hop at a time, and history served on
/// request so late joiners catch up.
pub struct RoomService {
    engine: NodeEngine,
    keypair: Keypair,
    local: PublicKey,
    rooms: Arc<Mutex<HashMap<String, RoomState>>>,
    error: Arc<Mutex<Option<String>>>,
    version: Arc<AtomicU64>,
    /// Post digests already relayed, so flooding terminates.
    seen: Mutex<PostDedup>,
    /// Per-author post budget: shared by this node's own posts and every
    /// peer's, so one key cannot make a node flood or store without bound.
    flood: Mutex<AuthorFloodControl>,
    /// Rooms announced during this session. A restart re-announces (the set
    /// starts empty even when the subscription was persisted); a page that
    /// re-renders does not.
    announced: Mutex<HashSet<String>>,
    /// Where rooms are written back to; `None` keeps them in memory only.
    store: Option<PathBuf>,
}

impl RoomService {
    /// Wrap `engine` with in-memory rooms. Nothing runs until [`Self::run`]
    /// is spawned on the tokio runtime.
    pub fn new(engine: NodeEngine, keypair: Keypair) -> Arc<Self> {
        Self::build(engine, keypair, None)
    }

    /// Like [`Self::new`], but rooms are loaded from `data_dir/rooms.json`
    /// and written back after every change, so subscriptions and transcripts
    /// survive restarts.
    pub fn with_data_dir(engine: NodeEngine, keypair: Keypair, data_dir: &Path) -> Arc<Self> {
        Self::build(engine, keypair, Some(data_dir.join(ROOM_STORE_FILE)))
    }

    fn build(engine: NodeEngine, keypair: Keypair, store: Option<PathBuf>) -> Arc<Self> {
        let local = keypair.public;
        let rooms = match &store {
            Some(path) => load_store(path),
            None => HashMap::new(),
        };
        Arc::new(Self {
            engine,
            keypair,
            local,
            rooms: Arc::new(Mutex::new(rooms)),
            error: Arc::new(Mutex::new(None)),
            version: Arc::new(AtomicU64::new(0)),
            seen: Mutex::new(PostDedup::new(MAX_SEEN_POSTS)),
            flood: Mutex::new(AuthorFloodControl::default()),
            announced: Mutex::new(HashSet::new()),
            store,
        })
    }

    /// Join `room`: keep it, announce the membership to every peer, and ask
    /// for the history this node has not seen.
    ///
    /// Idempotent in both senses that matter — a room already joined (or
    /// rejoined from a persisted subscription) changes nothing, and the
    /// announcement goes out at most once per session.
    pub fn join(&self, room: &str) -> Result<(), String> {
        let room = normalize_room(room)?;
        let mut changed = false;
        {
            let mut rooms = self.rooms.lock().unwrap();
            let state = rooms.entry(room.clone()).or_default();
            if !state.subscribed {
                state.subscribed = true;
                changed = true;
                if let Some(path) = &self.store {
                    save_store(path, &rooms);
                }
            }
        }
        if changed {
            self.version.fetch_add(1, Ordering::Relaxed);
        }

        if self.announced.lock().unwrap().insert(room.clone()) {
            let _ = self.engine.broadcast(NetworkMessage::RoomSubscribe {
                sender: self.local,
                room: room.clone(),
                subscribed: true,
            });
            let _ = self.engine.broadcast(NetworkMessage::RoomSync {
                sender: self.local,
                room,
            });
        }
        Ok(())
    }

    /// Post `text` to `room` as this identity: sign it, keep it, and flood it.
    ///
    /// Every failure is recorded for the page (and bumps the version), so a
    /// rejected message is visible instead of silently lost.
    pub fn post(&self, room: &str, text: &str) -> Result<(), String> {
        match self.try_post(room, text) {
            Ok(()) => {
                *self.error.lock().unwrap() = None;
                Ok(())
            }
            Err(error) => {
                *self.error.lock().unwrap() = Some(error.clone());
                self.version.fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    fn try_post(&self, room: &str, text: &str) -> Result<(), String> {
        let room = normalize_room(room)?;
        let text = text.trim();
        if text.is_empty() {
            return Err("message is empty".to_string());
        }
        if text.len() > MAX_CHAT_TEXT {
            return Err(format!("message is too long (max {MAX_CHAT_TEXT} bytes)"));
        }
        {
            let rooms = self.rooms.lock().unwrap();
            if !rooms.get(&room).is_some_and(|state| state.subscribed) {
                return Err(format!("not joined to {room} — open the room first"));
            }
        }
        if !self.flood.lock().unwrap().allow(&self.local, unix_time()) {
            return Err(format!(
                "you are posting too fast — at most {AUTHOR_POST_LIMIT} messages per {AUTHOR_WINDOW_SECS}s"
            ));
        }

        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        let sent_at = unix_time();
        let post = RoomPostWire {
            sender: self.local,
            room: room.clone(),
            id,
            sent_at,
            text: text.to_string(),
            hops: ROOM_POST_HOPS,
            signature: self.keypair.sign(&room_post_sign_message(
                &self.local,
                &room,
                &id,
                sent_at,
                text,
            )),
        };
        // Own copy counts as handled: a peer that echoes it back must not
        // bounce it around again.
        self.seen.lock().unwrap().insert(post_digest(&post));

        let message = NetworkMessage::RoomPost(post.clone());
        self.engine
            .broadcast(message)
            .map_err(|error| error.to_string())?;

        if self.record(&room, post) {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Run the inbound loop until the engine stops: apply what peers post,
    /// track who joins, answer history requests, and relay what should travel
    /// further. Prints inbound posts so a headless operator sees the room
    /// without a page.
    pub async fn run(self: Arc<Self>) {
        let mut inbound = self.engine.subscribe();
        while let Some((from, message)) = inbound.recv().await {
            self.handle(from, message);
        }
    }

    /// An immutable view for rendering `chat.plain?room=`.
    pub fn snapshot(&self) -> RoomSnapshot {
        RoomSnapshot {
            local: self.local,
            error: self.error.lock().unwrap().clone(),
            rooms: self.rooms.lock().unwrap().clone(),
        }
    }

    /// Monotonic change counter: bumped on every membership, roster, or
    /// transcript update, so the page can re-render when it differs.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Whether this node keeps `room` (and should therefore store and serve
    /// it). Used by the pipeline to decide whether a page needs a join.
    pub fn is_subscribed(&self, room: &str) -> bool {
        self.rooms
            .lock()
            .unwrap()
            .get(room)
            .is_some_and(|state| state.subscribed)
    }

    /// Dispatch one inbound message. Signature, shape, and the one-hop
    /// identity rule for announcements are already enforced by the engine;
    /// everything here re-checks what it relies on, because a transcript is
    /// shown to a user.
    fn handle(&self, from: SocketAddr, message: NetworkMessage) {
        match message {
            NetworkMessage::RoomPost(post) => self.on_post(from, post),
            NetworkMessage::RoomSubscribe {
                sender,
                room,
                subscribed,
            } => self.on_subscribe(from, sender, room, subscribed),
            NetworkMessage::RoomSync { sender, room } => self.on_sync(from, sender, room),
            NetworkMessage::RoomHistory {
                sender,
                room,
                posts,
            } => self.on_history(sender, room, posts),
            _ => {}
        }
    }

    /// A flooded post: keep it if this node is a member, then relay it on
    /// (members or not — a public room is supposed to arrive through
    /// bystanders). Dedup by `(author, id)` is what makes the flood stop.
    fn on_post(&self, from: SocketAddr, post: RoomPostWire) {
        if post.sender == self.local || !post.verify() || post.hops == 0 {
            return;
        }
        if !self.seen.lock().unwrap().insert(post_digest(&post)) {
            return;
        }
        // One key, one budget: an author past their limit is dropped before
        // anything is stored or re-flooded, so a flood cannot buy fan-out.
        if !self.flood.lock().unwrap().allow(&post.sender, unix_time()) {
            eprintln!(
                "[Room {}] dropped a post from {} (per-author rate limit)",
                post.room,
                short_key(&post.sender)
            );
            return;
        }

        let room = post.room.clone();
        let subscribed = {
            let rooms = self.rooms.lock().unwrap();
            rooms.get(&room).is_some_and(|state| state.subscribed)
        };
        if subscribed {
            println!(
                "[Room {room}] {} says: {}",
                short_key(&post.sender),
                post.text
            );
            if self.record(&room, post.clone()) {
                self.version.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Spend one hop and hand it to every peer but the one it came from.
        if post.hops <= 1 {
            return;
        }
        let mut next = post;
        next.hops -= 1;
        let addresses: Vec<SocketAddr> = self
            .engine
            .peer_table()
            .lock()
            .unwrap()
            .values()
            .map(|info| info.address)
            .filter(|address| *address != from)
            .collect();
        for address in addresses {
            let _ = self
                .engine
                .send_to_addr(address, NetworkMessage::RoomPost(next.clone()));
        }
    }

    /// A membership announcement. First sight of a member in a room this node
    /// is also in is answered with its own announcement, so both rosters
    /// learn about each other; repeating it cannot loop, because the roster
    /// only answers once per peer.
    fn on_subscribe(&self, from: SocketAddr, sender: PublicKey, room: String, subscribed: bool) {
        if sender == self.local || !is_valid_room_name(&room) {
            return;
        }
        let mut first_sight = false;
        let mut changed = false;
        let subscribed_here = {
            let mut rooms = self.rooms.lock().unwrap();
            // Only rooms this node is in keep a roster.
            let Some(state) = rooms.get_mut(&room) else {
                return;
            };
            if subscribed {
                if !state.subscribers.contains(&sender) {
                    state.subscribers.push(sender);
                    first_sight = true;
                    changed = true;
                }
            } else if let Some(position) = state.subscribers.iter().position(|key| *key == sender) {
                state.subscribers.remove(position);
                changed = true;
            }
            state.subscribed
        };
        if changed {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        if first_sight && subscribed_here {
            let _ = self.engine.send_to_addr(
                from,
                NetworkMessage::RoomSubscribe {
                    sender: self.local,
                    room,
                    subscribed: true,
                },
            );
        }
    }

    /// A joiner asking what it missed: answer directly with the newest
    /// posts, signed by their authors, so the joiner verifies each one.
    fn on_sync(&self, from: SocketAddr, sender: PublicKey, room: String) {
        if sender == self.local || !is_valid_room_name(&room) {
            return;
        }
        let rooms = self.rooms.lock().unwrap();
        let Some(state) = rooms.get(&room) else {
            return;
        };
        if !state.subscribed && state.posts.is_empty() {
            return;
        }
        let start = state.posts.len().saturating_sub(ROOM_HISTORY_MAX);
        let posts = state.posts[start..].to_vec();
        drop(rooms);
        let _ = self.engine.send_to_addr(
            from,
            NetworkMessage::RoomHistory {
                sender: self.local,
                room,
                posts,
            },
        );
    }

    /// History from a peer: keep what is new and still verifiable. History
    /// is catch-up, not traffic — it is never re-flooded, and the live copy
    /// of the same post still relays normally afterwards.
    fn on_history(&self, sender: PublicKey, room: String, posts: Vec<RoomPostWire>) {
        if sender == self.local || !is_valid_room_name(&room) {
            return;
        }
        let subscribed = {
            let rooms = self.rooms.lock().unwrap();
            rooms.get(&room).is_some_and(|state| state.subscribed)
        };
        if !subscribed {
            return;
        }
        let mut changed = false;
        for post in posts {
            // A peer cannot file a post under a room its author did not name.
            if post.room != room || !post.verify() {
                continue;
            }
            if self.record(&room, post) {
                changed = true;
            }
        }
        if changed {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Append one post to `room`'s transcript (ignoring a duplicate),
    /// evicting the oldest beyond the cap, and persist. Returns whether the
    /// transcript changed.
    fn record(&self, room: &str, post: RoomPostWire) -> bool {
        let mut rooms = self.rooms.lock().unwrap();
        let state = rooms.entry(room.to_string()).or_default();
        if state
            .posts
            .iter()
            .any(|seen| seen.sender == post.sender && seen.id == post.id)
        {
            return false;
        }
        state.posts.push(post);
        if state.posts.len() > ROOM_HISTORY_LIMIT {
            let overflow = state.posts.len() - ROOM_HISTORY_LIMIT;
            state.posts.drain(..overflow);
        }
        if let Some(path) = &self.store {
            save_store(path, &rooms);
        }
        true
    }
}

/// First bytes of a key as hex, for a log line a human can skim.
fn short_key(key: &PublicKey) -> String {
    key.0
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::net::{NodeConfig, NodeEngine};

    fn engine(key_seed: u8) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: Keypair::from_seed(&[key_seed; 32]),
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 16,
            heartbeat_interval: std::time::Duration::from_secs(30),
        })
    }

    fn service(key_seed: u8) -> Arc<RoomService> {
        RoomService::new(engine(key_seed), Keypair::from_seed(&[key_seed; 32]))
    }

    /// A post signed by `seed`'s identity, as a peer would send it.
    fn signed_post(seed: u8, room: &str, text: &str, hops: u8) -> RoomPostWire {
        let keypair = Keypair::from_seed(&[seed; 32]);
        let mut id = [0u8; 16];
        id[0] = seed;
        id[1] = text.len() as u8;
        RoomPostWire {
            sender: keypair.public,
            room: room.to_string(),
            id,
            sent_at: 1_700_000_000,
            text: text.to_string(),
            hops,
            signature: keypair.sign(&room_post_sign_message(
                &keypair.public,
                room,
                &id,
                1_700_000_000,
                text,
            )),
        }
    }

    fn temp_store_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pwb-rooms-{name}-{}-{}",
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
    fn joining_keeps_the_room_and_announces_only_once_per_session() {
        let room = service(1);
        let before = room.version();

        room.join(DEFAULT_ROOM).expect("a valid name joins");
        assert!(room.is_subscribed(DEFAULT_ROOM));
        assert!(
            room.version() > before,
            "joining changes what the page shows"
        );

        let after_first = room.version();
        room.join(DEFAULT_ROOM).expect("rejoining is not an error");
        assert_eq!(
            room.version(),
            after_first,
            "a page that re-renders must not look like a new join every tick"
        );

        assert!(
            room.join("bad name!").is_err(),
            "names the wire would reject are refused here too"
        );
    }

    #[test]
    fn posting_requires_a_membership_and_sane_text() {
        let room = service(2);
        let error = room
            .post(DEFAULT_ROOM, "nobody is here")
            .expect_err("an unjoined room refuses posts");
        assert!(error.contains("not joined"), "got: {error}");
        assert_eq!(room.snapshot().error.as_deref(), Some(error.as_str()));

        room.join(DEFAULT_ROOM).expect("join");
        assert!(room.post(DEFAULT_ROOM, "   ").is_err());
        assert!(room
            .post(DEFAULT_ROOM, &"x".repeat(MAX_CHAT_TEXT + 1))
            .is_err());
        assert!(room.snapshot().rooms[DEFAULT_ROOM].posts.is_empty());
        assert!(
            room.snapshot().error.is_some(),
            "the rejection stays visible on the page"
        );
    }

    #[test]
    fn a_post_is_signed_by_its_author_and_recorded_once() {
        let room = service(3);
        room.join(DEFAULT_ROOM).expect("join");

        let text = "hello lobby";
        room.post(DEFAULT_ROOM, text).expect("posts");

        let snapshot = room.snapshot();
        let posts = &snapshot.rooms[DEFAULT_ROOM].posts;
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, text);
        assert_eq!(posts[0].sender, room.local);
        assert_eq!(
            posts[0].hops, ROOM_POST_HOPS,
            "it starts with a full budget"
        );
        assert!(posts[0].verify(), "the author signature covers every field");
    }

    #[test]
    fn a_peer_post_is_kept_once_and_never_attributed_to_someone_else() {
        let room = service(4);
        room.join(DEFAULT_ROOM).expect("join");
        let post = signed_post(9, DEFAULT_ROOM, "from a peer", ROOM_POST_HOPS);

        room.handle(
            "127.0.0.1:9001".parse().unwrap(),
            NetworkMessage::RoomPost(post.clone()),
        );
        room.handle(
            "127.0.0.1:9001".parse().unwrap(),
            NetworkMessage::RoomPost(post.clone()),
        );

        let snapshot = room.snapshot();
        let posts = &snapshot.rooms[DEFAULT_ROOM].posts;
        assert_eq!(posts.len(), 1, "flooding is deduplicated by (author, id)");
        assert_eq!(posts[0].sender.0, Keypair::from_seed(&[9u8; 32]).public.0);

        // A post whose text was edited after signing is dropped: authorship
        // is proven by the signature, not by the envelope.
        let mut forged = signed_post(9, DEFAULT_ROOM, "harmless", ROOM_POST_HOPS);
        forged.text = "rewritten in transit".to_string();
        room.handle(
            "127.0.0.1:9001".parse().unwrap(),
            NetworkMessage::RoomPost(forged),
        );
        assert_eq!(
            room.snapshot().rooms[DEFAULT_ROOM].posts.len(),
            1,
            "a tampered post never reaches the transcript"
        );
    }

    #[test]
    fn rooms_i_have_not_joined_are_not_stored() {
        let room = service(5);
        let post = signed_post(8, "other-room", "not my business", ROOM_POST_HOPS);

        room.handle(
            "127.0.0.1:9001".parse().unwrap(),
            NetworkMessage::RoomPost(post),
        );

        assert!(
            room.snapshot().rooms.is_empty(),
            "non-members relay but keep nothing"
        );
    }

    #[test]
    fn history_from_a_peer_merges_without_duplicates_or_foreign_rooms() {
        let room = service(6);
        room.join(DEFAULT_ROOM).expect("join");
        let wanted = signed_post(7, DEFAULT_ROOM, "you missed this", ROOM_POST_HOPS);
        let duplicate = signed_post(7, DEFAULT_ROOM, "you missed this", 3);
        // Same id as `wanted` but filed under another room by a lying peer.
        let mut foreign = wanted.clone();
        foreign.room = "sneaky".to_string();
        foreign.signature = Keypair::from_seed(&[7u8; 32]).sign(&room_post_sign_message(
            &Keypair::from_seed(&[7u8; 32]).public,
            "sneaky",
            &wanted.id,
            wanted.sent_at,
            &wanted.text,
        ));

        room.on_history(
            Keypair::from_seed(&[4u8; 32]).public,
            DEFAULT_ROOM.to_string(),
            vec![wanted, duplicate, foreign],
        );

        let snapshot = room.snapshot();
        let posts = &snapshot.rooms[DEFAULT_ROOM].posts;
        assert_eq!(
            posts.len(),
            1,
            "dedup by (author, id) and no room smuggling"
        );
        assert_eq!(posts[0].text, "you missed this");
        assert!(snapshot.state("sneaky").is_none());
    }

    #[test]
    fn history_for_a_room_i_have_not_joined_is_ignored() {
        let room = service(7);
        let post = signed_post(7, "nope", "catch-up for someone else", ROOM_POST_HOPS);
        room.on_history(
            Keypair::from_seed(&[4u8; 32]).public,
            "nope".to_string(),
            vec![post],
        );
        assert!(room.snapshot().rooms.is_empty());
    }

    #[test]
    fn the_roster_tracks_announcements_and_a_vacated_seat_is_released() {
        let room = service(8);
        room.join(DEFAULT_ROOM).expect("join");
        let peer = Keypair::from_seed(&[9u8; 32]).public;
        let from: SocketAddr = "127.0.0.1:9002".parse().unwrap();

        room.on_subscribe(from, peer, DEFAULT_ROOM.to_string(), true);
        room.on_subscribe(from, peer, DEFAULT_ROOM.to_string(), true);
        assert_eq!(room.snapshot().rooms[DEFAULT_ROOM].subscribers, vec![peer]);

        room.on_subscribe(from, peer, DEFAULT_ROOM.to_string(), false);
        assert!(room.snapshot().rooms[DEFAULT_ROOM].subscribers.is_empty());
    }

    #[test]
    fn rooms_persist_across_a_restart_and_merge_with_a_shared_directory() {
        let dir = temp_store_dir("persist");
        let peer_post = signed_post(7, DEFAULT_ROOM, "from the daemon", ROOM_POST_HOPS);

        {
            let node = RoomService::with_data_dir(engine(1), Keypair::from_seed(&[1u8; 32]), &dir);
            node.join(DEFAULT_ROOM).expect("join");
            node.post(DEFAULT_ROOM, "from the node").expect("post");
            node.handle(
                "127.0.0.1:9001".parse().unwrap(),
                NetworkMessage::RoomPost(peer_post),
            );
        }

        // A restart keeps the membership and the transcript.
        let reloaded = RoomService::with_data_dir(engine(1), Keypair::from_seed(&[1u8; 32]), &dir);
        assert!(reloaded.is_subscribed(DEFAULT_ROOM));
        let reloaded_state = reloaded.snapshot();
        let texts: Vec<&str> = reloaded_state.rooms[DEFAULT_ROOM]
            .posts
            .iter()
            .map(|post| post.text.as_str())
            .collect();
        assert_eq!(texts, ["from the node", "from the daemon"]);
        // The announcement still has to go out: this session has not sent it.
        assert!(!reloaded.announced.lock().unwrap().contains(DEFAULT_ROOM));

        // A second process sharing the directory inherits the membership
        // and cannot clobber what is already on the file.
        let other = RoomService::with_data_dir(engine(2), Keypair::from_seed(&[2u8; 32]), &dir);
        assert!(
            other.is_subscribed(DEFAULT_ROOM),
            "the shared store carries the subscription too"
        );
        other.post(DEFAULT_ROOM, "from the browser").expect("post");
        drop(other);

        let check = RoomService::with_data_dir(engine(1), Keypair::from_seed(&[1u8; 32]), &dir);
        let check_state = check.snapshot();
        let texts: Vec<&str> = check_state.rooms[DEFAULT_ROOM]
            .posts
            .iter()
            .map(|post| post.text.as_str())
            .collect();
        assert_eq!(
            texts,
            ["from the node", "from the daemon", "from the browser"],
            "neither process's posts are lost to the other's write"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_room_file_starts_empty_instead_of_failing() {
        let dir = temp_store_dir("corrupt");
        std::fs::write(dir.join(ROOM_STORE_FILE), b"{not json").expect("write corrupt store");
        let room = RoomService::with_data_dir(engine(3), Keypair::from_seed(&[3u8; 32]), &dir);
        assert!(room.snapshot().rooms.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One post by `seed`, with an id distinct per `nonce` so the dedup set
    /// does not collapse them into one message.
    fn signed_post_nonce(seed: u8, room: &str, nonce: u8) -> RoomPostWire {
        let keypair = Keypair::from_seed(&[seed; 32]);
        let mut id = [0u8; 16];
        id[0] = seed;
        id[1] = nonce;
        let text = format!("spam {nonce}");
        RoomPostWire {
            sender: keypair.public,
            room: room.to_string(),
            id,
            sent_at: 1_700_000_000,
            text,
            hops: ROOM_POST_HOPS,
            signature: keypair.sign(&room_post_sign_message(
                &keypair.public,
                room,
                &id,
                1_700_000_000,
                &format!("spam {nonce}"),
            )),
        }
    }

    #[test]
    fn one_author_cannot_flood_a_room_beyond_the_per_author_limit() {
        let room = service(10);
        room.join(DEFAULT_ROOM).expect("join");
        let from: SocketAddr = "127.0.0.1:9003".parse().unwrap();

        for nonce in 0..(AUTHOR_POST_LIMIT + 5) {
            room.handle(
                from,
                NetworkMessage::RoomPost(signed_post_nonce(9, DEFAULT_ROOM, nonce as u8)),
            );
        }

        assert_eq!(
            room.snapshot().rooms[DEFAULT_ROOM].posts.len(),
            AUTHOR_POST_LIMIT as usize,
            "posts past the per-author window are dropped, not stored or relayed"
        );

        // A different author is unaffected by the first one's budget.
        room.handle(
            from,
            NetworkMessage::RoomPost(signed_post_nonce(6, DEFAULT_ROOM, 1)),
        );
        assert_eq!(
            room.snapshot().rooms[DEFAULT_ROOM].posts.len(),
            AUTHOR_POST_LIMIT as usize + 1
        );
    }

    #[test]
    fn my_own_posts_are_rate_limited_too() {
        let room = service(11);
        room.join(DEFAULT_ROOM).expect("join");

        for _ in 0..AUTHOR_POST_LIMIT {
            room.post(DEFAULT_ROOM, "one more").expect("within limit");
        }
        let error = room
            .post(DEFAULT_ROOM, "too fast")
            .expect_err("the local author is bounded like any other");
        assert!(error.contains("too fast"), "got: {error}");
        assert_eq!(
            room.snapshot().rooms[DEFAULT_ROOM].posts.len(),
            AUTHOR_POST_LIMIT as usize
        );
    }
}
