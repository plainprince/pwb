//! The global blog feed at `blog.plain` (`pwb-node::blog`).
//!
//! One feed for the whole network: [`BlogService::post`] signs a
//! [`BlogEntryWire`] as its author and broadcasts it, and every holder
//! re-broadcasts it to its own peers until the hop budget runs out. Because a
//! forwarder re-signs the transport envelope, authorship comes from the
//! entry's own signature (checked by the engine on every copy), never from who
//! handed it to us. Unlike [`crate::room`], there is no room name — the feed is
//! global.
//!
//! Subscriptions are the membership layer:
//!
//! - [`BlogService::subscribe`] marks the feed as one this node keeps — it is
//!   stored, rendered, and served as history — and announces the membership
//!   to the swarm (`BlogSubscribe`), then asks for what it missed (`BlogSync`).
//!   The announcement is sent once per session, so a page that re-renders (or
//!   a restart that reloads a persisted subscription) cannot spam the network.
//! - `BlogSubscribe` is one hop and envelope-authenticated, so it builds an
//!   honest roster of the peers known to be reading (and each reader answers a
//!   newcomer's first announcement with its own, which terminates because the
//!   roster only replies on first sight).
//! - `BlogSync` is answered directly with up to [`BLOG_HISTORY_MAX`] signed
//!   entries, so a late reader converges instead of waiting for new traffic.
//!
//! Non-subscribers still forward: the feed is meant to arrive through people
//! who are not reading it. They simply do not store what passes through.
//! History, though, is served by subscribers — only they keep it.
//!
//! The feed is bounded at [`BLOG_HISTORY_LIMIT`] entries and written to
//! `<data>/blog.json` after every change, merging with what is already on the
//! file by `(author, id)` so a `PWB_DATA` shared with another process survives.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pwb_core::crypto::{ContentId, Keypair, PublicKey, Signature};
use pwb_core::discovery::unix_time;
use pwb_core::net::{
    blog_entry_sign_message, BlogEntryWire, NetworkMessage, NodeEngine, BLOG_ENTRY_HOPS,
    BLOG_HISTORY_MAX, MAX_BLOG_TEXT, MAX_BLOG_TITLE,
};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Entries kept in the feed. Oldest are evicted, so a long-lived node cannot
/// grow without bound and a history reply stays a bounded datagram.
pub const BLOG_HISTORY_LIMIT: usize = 200;

/// File the feed lives in, under the node's data directory.
const BLOG_STORE_FILE: &str = "blog.json";

/// Distinct entry digests remembered for flood control (bounded, oldest
/// first). Evicting one only matters if the same entry is still circulating
/// much later; the hop budget is what actually bounds an entry's lifetime.
const MAX_SEEN_ENTRIES: usize = 4096;

/// Entries one author may contribute within [`AUTHOR_WINDOW_SECS`] before
/// they are throttled — applied to this node's own `post` and to every
/// inbound `on_entry` alike. An entry can carry [`MAX_BLOG_TEXT`] bytes, four
/// times a room post, so the budget is tighter than a room's.
const AUTHOR_ENTRY_LIMIT: u32 = 6;

/// Length of the per-author posting window, in seconds.
const AUTHOR_WINDOW_SECS: u64 = 60;

use crate::room::{AuthorFloodControl, PostDedup};

/// Live state of the feed: its entries, the readers this node knows about,
/// and whether this node keeps it (and therefore stores and serves it).
#[derive(Debug, Clone, Default)]
struct BlogState {
    /// Oldest first, capped at [`BLOG_HISTORY_LIMIT`].
    entries: Vec<BlogEntryWire>,
    /// Direct peers that announced a subscription (one hop, best effort).
    subscribers: Vec<PublicKey>,
    /// Whether this node has opened the feed.
    subscribed: bool,
}

/// An immutable view of the feed, built by [`BlogService::snapshot`] and
/// rendered by the `blog.plain` page.
#[derive(Debug, Clone)]
pub struct BlogSnapshot {
    /// This node's identity (the author of its own entries).
    pub local: PublicKey,
    /// The last failure to read or post, shown on the page.
    pub error: Option<String>,
    /// Whether this node keeps the feed.
    pub subscribed: bool,
    /// Direct peers known to be reading.
    pub subscribers: Vec<PublicKey>,
    /// The feed, oldest first.
    pub entries: Vec<BlogEntryWire>,
}

impl Default for BlogSnapshot {
    fn default() -> Self {
        Self {
            local: PublicKey([0u8; 32]),
            error: None,
            subscribed: false,
            subscribers: Vec::new(),
            entries: Vec::new(),
        }
    }
}

impl BlogSnapshot {
    /// Everyone reading the feed that this node knows about, including itself
    /// when it has subscribed.
    pub fn reader_count(&self) -> usize {
        self.subscribers.len() + usize::from(self.subscribed)
    }

    /// How many distinct identities have written in the feed.
    pub fn author_count(&self) -> usize {
        let mut authors = HashSet::new();
        for entry in &self.entries {
            authors.insert(entry.sender);
        }
        authors.len()
    }
}

/// On-disk form of the feed. Keys are hex like `rooms.json`, `chat.json`, and
/// `peers.json`, so an operator can read who wrote what.
#[derive(Serialize, Deserialize)]
struct BlogStoreFile {
    version: u32,
    #[serde(default)]
    subscribed: bool,
    #[serde(default)]
    entries: Vec<BlogEntryStore>,
}

#[derive(Serialize, Deserialize)]
struct BlogEntryStore {
    sender: String,
    id: String,
    sent_at: u64,
    title: String,
    text: String,
    hops: u8,
    signature: String,
}

impl From<&BlogEntryWire> for BlogEntryStore {
    fn from(entry: &BlogEntryWire) -> Self {
        Self {
            sender: entry.sender.to_hex(),
            id: hex::encode(entry.id),
            sent_at: entry.sent_at,
            title: entry.title.clone(),
            text: entry.text.clone(),
            hops: entry.hops,
            signature: entry.signature.to_hex(),
        }
    }
}

impl BlogEntryStore {
    /// Rebuild the wire form; a field that is not valid hex drops the entry
    /// (the signature check that follows drops anything malformed anyway).
    fn into_wire(self) -> Option<BlogEntryWire> {
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
        Some(BlogEntryWire {
            sender: key(&self.sender)?,
            id: id(&self.id)?,
            sent_at: self.sent_at,
            title: self.title,
            text: self.text,
            hops: self.hops,
            signature: signature(&self.signature)?,
        })
    }
}

/// Identity of one entry for flood dedup: who wrote it and which message it
/// is. `hops` is deliberately excluded (forwarders spend it).
fn entry_digest(entry: &BlogEntryWire) -> ContentId {
    let mut bytes = Vec::with_capacity(48);
    bytes.extend_from_slice(&entry.sender.0);
    bytes.extend_from_slice(&entry.id);
    ContentId::compute(&bytes)
}

/// Read the persisted feed from `path`.
///
/// A missing or corrupt file yields an empty feed (the blog is an enhancement
/// and must never stop a node from starting); an entry whose signature no
/// longer verifies is skipped rather than shown as history.
fn load_store(path: &Path) -> BlogState {
    let Ok(bytes) = std::fs::read(path) else {
        return BlogState::default();
    };
    let Ok(file) = serde_json::from_slice::<BlogStoreFile>(&bytes) else {
        return BlogState::default();
    };
    BlogState {
        entries: file
            .entries
            .into_iter()
            .filter_map(BlogEntryStore::into_wire)
            .filter(BlogEntryWire::verify)
            .take(BLOG_HISTORY_LIMIT)
            .collect(),
        subscribers: Vec::new(),
        subscribed: file.subscribed,
    }
}

/// Persist the feed to `path` (atomically), merging with what is already on
/// disk: entries union by `(author, id)`, the subscription ORs together. Two
/// processes can share one `PWB_DATA` deliberately, and each only holds what
/// it has seen. Failures are reported but never propagated — a full disk must
/// not break a live feed.
fn save_store(path: &Path, state: &BlogState) {
    let mut merged = load_store(path);
    merged.subscribed |= state.subscribed;
    for entry in &state.entries {
        if !merged
            .entries
            .iter()
            .any(|seen| seen.sender == entry.sender && seen.id == entry.id)
        {
            merged.entries.push(entry.clone());
        }
    }
    if merged.entries.len() > BLOG_HISTORY_LIMIT {
        let overflow = merged.entries.len() - BLOG_HISTORY_LIMIT;
        merged.entries.drain(..overflow);
    }
    let file = BlogStoreFile {
        version: 1,
        subscribed: merged.subscribed,
        entries: merged.entries.iter().map(BlogEntryStore::from).collect(),
    };
    let json = match serde_json::to_vec_pretty(&file) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("[Blog] Could not serialize the feed: {error}");
            return;
        }
    };
    if let Err(error) = crate::persist::atomic_write(path, &json) {
        eprintln!(
            "[Blog] Could not persist the feed to {}: {error}",
            path.display()
        );
    }
}

/// The global feed over the node's engine: signed entries that flood with a
/// hop budget, reader membership announced one hop at a time, and history
/// served on request so late readers catch up.
pub struct BlogService {
    engine: NodeEngine,
    keypair: Keypair,
    local: PublicKey,
    state: Mutex<BlogState>,
    error: Mutex<Option<String>>,
    version: AtomicU64,
    /// Entry digests already relayed, so flooding terminates.
    seen: Mutex<PostDedup>,
    /// Per-author entry budget: shared by this node's own posts and every
    /// peer's, so one key cannot make a node store or flood without bound.
    flood: Mutex<AuthorFloodControl>,
    /// Whether the subscription was announced during this session. A restart
    /// re-announces (the flag starts false even when the subscription was
    /// persisted); a page that re-renders does not.
    announced: Mutex<bool>,
    /// Where the feed is written back to; `None` keeps it in memory only.
    store: Option<PathBuf>,
}

impl BlogService {
    /// Wrap `engine` with an in-memory feed. Nothing runs until [`Self::run`]
    /// is spawned on the tokio runtime.
    pub fn new(engine: NodeEngine, keypair: Keypair) -> Arc<Self> {
        Self::build(engine, keypair, None)
    }

    /// Like [`Self::new`], but the feed is loaded from `data_dir/blog.json`
    /// and written back after every change, so the subscription and entries
    /// survive restarts.
    pub fn with_data_dir(engine: NodeEngine, keypair: Keypair, data_dir: &Path) -> Arc<Self> {
        Self::build(engine, keypair, Some(data_dir.join(BLOG_STORE_FILE)))
    }

    fn build(engine: NodeEngine, keypair: Keypair, store: Option<PathBuf>) -> Arc<Self> {
        let local = keypair.public;
        let state = match &store {
            Some(path) => load_store(path),
            None => BlogState::default(),
        };
        Arc::new(Self {
            engine,
            keypair,
            local,
            state: Mutex::new(state),
            error: Mutex::new(None),
            version: AtomicU64::new(0),
            seen: Mutex::new(PostDedup::new(MAX_SEEN_ENTRIES)),
            flood: Mutex::new(AuthorFloodControl::with_limit(
                AUTHOR_ENTRY_LIMIT,
                AUTHOR_WINDOW_SECS,
            )),
            announced: Mutex::new(false),
            store,
        })
    }

    /// Subscribe to the feed: keep it, announce the membership to every peer,
    /// and ask for the history this node has not seen.
    ///
    /// Idempotent in both senses that matter — subscribing again (or
    /// re-subscribing from a persisted subscription) changes nothing, and the
    /// announcement goes out at most once per session.
    pub fn subscribe(&self) -> Result<(), String> {
        let mut changed = false;
        {
            let mut state = self.state.lock().unwrap();
            if !state.subscribed {
                state.subscribed = true;
                changed = true;
                if let Some(path) = &self.store {
                    save_store(path, &state);
                }
            }
        }
        if changed {
            self.version.fetch_add(1, Ordering::Relaxed);
        }

        let mut announced = self.announced.lock().unwrap();
        if !*announced {
            *announced = true;
            drop(announced);
            let _ = self.engine.broadcast(NetworkMessage::BlogSubscribe {
                sender: self.local,
                subscribed: true,
            });
            let _ = self
                .engine
                .broadcast(NetworkMessage::BlogSync { sender: self.local });
        }
        Ok(())
    }

    /// Post `title`/`text` as this identity: sign it, keep it, and flood it.
    ///
    /// Every failure is recorded for the page (and bumps the version), so a
    /// rejected entry is visible instead of silently lost.
    pub fn post(&self, title: &str, text: &str) -> Result<(), String> {
        match self.try_post(title, text) {
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

    fn try_post(&self, title: &str, text: &str) -> Result<(), String> {
        let title = title.trim();
        let text = text.trim();
        if title.is_empty() {
            return Err("title is empty".to_string());
        }
        if title.len() > MAX_BLOG_TITLE {
            return Err(format!("title is too long (max {MAX_BLOG_TITLE} bytes)"));
        }
        if text.len() > MAX_BLOG_TEXT {
            return Err(format!("entry is too long (max {MAX_BLOG_TEXT} bytes)"));
        }
        {
            let state = self.state.lock().unwrap();
            if !state.subscribed {
                return Err("not subscribed to the feed — open blog.plain first".to_string());
            }
        }
        if !self.flood.lock().unwrap().allow(&self.local, unix_time()) {
            return Err(format!(
                "you are posting too fast — at most {AUTHOR_ENTRY_LIMIT} entries per {AUTHOR_WINDOW_SECS}s"
            ));
        }

        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        let sent_at = unix_time();
        let entry = BlogEntryWire {
            sender: self.local,
            id,
            sent_at,
            title: title.to_string(),
            text: text.to_string(),
            hops: BLOG_ENTRY_HOPS,
            signature: self.keypair.sign(&blog_entry_sign_message(
                &self.local,
                &id,
                sent_at,
                title,
                text,
            )),
        };
        // Own copy counts as handled: a peer that echoes it back must not
        // bounce it around again.
        self.seen.lock().unwrap().insert(entry_digest(&entry));

        let message = NetworkMessage::BlogEntry(entry.clone());
        self.engine
            .broadcast(message)
            .map_err(|error| error.to_string())?;

        if self.record(entry) {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Run the inbound loop until the engine stops: apply what peers post,
    /// track who is reading, answer history requests, and relay what should
    /// travel further. Prints inbound entries so a headless operator sees the
    /// feed without a page.
    pub async fn run(self: Arc<Self>) {
        let mut inbound = self.engine.subscribe();
        while let Some((from, message)) = inbound.recv().await {
            self.handle(from, message);
        }
    }

    /// An immutable view for rendering `blog.plain`.
    pub fn snapshot(&self) -> BlogSnapshot {
        let state = self.state.lock().unwrap();
        BlogSnapshot {
            local: self.local,
            error: self.error.lock().unwrap().clone(),
            subscribed: state.subscribed,
            subscribers: state.subscribers.clone(),
            entries: state.entries.clone(),
        }
    }

    /// Monotonic change counter: bumped on every subscription, roster, or
    /// feed update, so the page can re-render when it differs.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Whether this node keeps the feed (and should therefore store and serve
    /// it).
    pub fn is_subscribed(&self) -> bool {
        self.state.lock().unwrap().subscribed
    }

    /// Dispatch one inbound message. Signature, shape, and the one-hop
    /// identity rule for announcements are already enforced by the engine;
    /// everything here re-checks what it relies on, because the feed is shown
    /// to a user.
    fn handle(&self, from: SocketAddr, message: NetworkMessage) {
        match message {
            NetworkMessage::BlogEntry(entry) => self.on_entry(from, entry),
            NetworkMessage::BlogSubscribe { sender, subscribed } => {
                self.on_subscribe(from, sender, subscribed)
            }
            NetworkMessage::BlogSync { sender } => self.on_sync(from, sender),
            NetworkMessage::BlogHistory { sender, entries } => self.on_history(sender, entries),
            _ => {}
        }
    }

    /// A flooded entry: keep it if this node is subscribed, then relay it on
    /// (subscribed or not — the feed is supposed to arrive through
    /// bystanders). Dedup by `(author, id)` is what makes the flood stop.
    fn on_entry(&self, from: SocketAddr, entry: BlogEntryWire) {
        if entry.sender == self.local || !entry.verify() || entry.hops == 0 {
            return;
        }
        if !self.seen.lock().unwrap().insert(entry_digest(&entry)) {
            return;
        }
        // One key, one budget: an author past their limit is dropped before
        // anything is stored or re-flooded, so a flood cannot buy fan-out.
        if !self.flood.lock().unwrap().allow(&entry.sender, unix_time()) {
            eprintln!(
                "[Blog] dropped an entry from {} (per-author rate limit)",
                short_key(&entry.sender)
            );
            return;
        }

        let subscribed = self.state.lock().unwrap().subscribed;
        if subscribed {
            println!("[Blog] {} wrote: {}", short_key(&entry.sender), entry.title);
            if self.record(entry.clone()) {
                self.version.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Spend one hop and hand it to every peer but the one it came from.
        if entry.hops <= 1 {
            return;
        }
        let mut next = entry;
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
                .send_to_addr(address, NetworkMessage::BlogEntry(next.clone()));
        }
    }

    /// A membership announcement. First sight of a reader while this node is
    /// also reading is answered with its own announcement, so both rosters
    /// learn about each other; repeating it cannot loop, because the roster
    /// only answers once per peer.
    fn on_subscribe(&self, from: SocketAddr, sender: PublicKey, subscribed: bool) {
        if sender == self.local {
            return;
        }
        let mut first_sight = false;
        let mut changed = false;
        {
            let mut state = self.state.lock().unwrap();
            // Only a subscribed node keeps a roster.
            if !state.subscribed {
                return;
            }
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
        }
        if changed {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        if first_sight {
            let _ = self.engine.send_to_addr(
                from,
                NetworkMessage::BlogSubscribe {
                    sender: self.local,
                    subscribed: true,
                },
            );
        }
    }

    /// A late reader asking what it missed: answer directly with the newest
    /// entries, signed by their authors, so the reader verifies each one.
    fn on_sync(&self, from: SocketAddr, sender: PublicKey) {
        if sender == self.local {
            return;
        }
        let state = self.state.lock().unwrap();
        if !state.subscribed {
            return;
        }
        let start = state.entries.len().saturating_sub(BLOG_HISTORY_MAX);
        let entries = state.entries[start..].to_vec();
        drop(state);
        let _ = self.engine.send_to_addr(
            from,
            NetworkMessage::BlogHistory {
                sender: self.local,
                entries,
            },
        );
    }

    /// History from a peer: keep what is new and still verifiable. History is
    /// catch-up, not traffic — it is never re-flooded, and the live copy of
    /// the same entry still relays normally afterwards.
    fn on_history(&self, sender: PublicKey, entries: Vec<BlogEntryWire>) {
        if sender == self.local {
            return;
        }
        if !self.state.lock().unwrap().subscribed {
            return;
        }
        let mut changed = false;
        for entry in entries {
            if !entry.verify() {
                continue;
            }
            if self.record(entry) {
                changed = true;
            }
        }
        if changed {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Append one entry to the feed (ignoring a duplicate), evicting the
    /// oldest beyond the cap, and persist. Returns whether the feed changed.
    fn record(&self, entry: BlogEntryWire) -> bool {
        let mut state = self.state.lock().unwrap();
        if state
            .entries
            .iter()
            .any(|seen| seen.sender == entry.sender && seen.id == entry.id)
        {
            return false;
        }
        state.entries.push(entry);
        if state.entries.len() > BLOG_HISTORY_LIMIT {
            let overflow = state.entries.len() - BLOG_HISTORY_LIMIT;
            state.entries.drain(..overflow);
        }
        if let Some(path) = &self.store {
            save_store(path, &state);
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

    fn service(key_seed: u8) -> Arc<BlogService> {
        BlogService::new(engine(key_seed), Keypair::from_seed(&[key_seed; 32]))
    }

    /// An entry signed by `seed`'s identity, as a peer would send it.
    fn signed_entry(seed: u8, title: &str, text: &str, hops: u8) -> BlogEntryWire {
        let keypair = Keypair::from_seed(&[seed; 32]);
        let mut id = [0u8; 16];
        id[0] = seed;
        id[1] = title.len() as u8;
        BlogEntryWire {
            sender: keypair.public,
            id,
            sent_at: 1_700_000_000,
            title: title.to_string(),
            text: text.to_string(),
            hops,
            signature: keypair.sign(&blog_entry_sign_message(
                &keypair.public,
                &id,
                1_700_000_000,
                title,
                text,
            )),
        }
    }

    fn temp_store_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pwb-blog-{name}-{}-{}",
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
    fn subscribing_keeps_the_feed_and_announces_only_once_per_session() {
        let blog = service(1);
        let before = blog.version();

        blog.subscribe().expect("subscribing works");
        assert!(blog.is_subscribed());
        assert!(
            blog.version() > before,
            "subscribing changes what the page shows"
        );

        let after_first = blog.version();
        blog.subscribe().expect("re-subscribing is not an error");
        assert_eq!(
            blog.version(),
            after_first,
            "a page that re-renders must not look like a new subscription every tick"
        );
    }

    #[test]
    fn posting_requires_a_subscription_and_sane_fields() {
        let blog = service(2);
        let error = blog
            .post("Hello", "nobody is reading")
            .expect_err("an unsubscribed feed refuses posts");
        assert!(error.contains("not subscribed"), "got: {error}");
        assert_eq!(blog.snapshot().error.as_deref(), Some(error.as_str()));

        blog.subscribe().expect("subscribe");
        assert!(blog.post("   ", "body").is_err(), "a title is required");
        assert!(blog.post(&"x".repeat(MAX_BLOG_TITLE + 1), "body").is_err());
        assert!(blog.post("Title", &"x".repeat(MAX_BLOG_TEXT + 1)).is_err());
        assert!(blog.snapshot().entries.is_empty());
        assert!(
            blog.snapshot().error.is_some(),
            "the rejection stays visible on the page"
        );
    }

    #[test]
    fn an_entry_is_signed_by_its_author_and_recorded_once() {
        let blog = service(3);
        blog.subscribe().expect("subscribe");

        blog.post("First post", "hello feed").expect("posts");

        let snapshot = blog.snapshot();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].title, "First post");
        assert_eq!(snapshot.entries[0].text, "hello feed");
        assert_eq!(snapshot.entries[0].sender, snapshot.local);
        assert_eq!(
            snapshot.entries[0].hops, BLOG_ENTRY_HOPS,
            "it starts with a full budget"
        );
        assert!(
            snapshot.entries[0].verify(),
            "the author signature covers every field"
        );
    }

    #[test]
    fn a_peer_entry_is_kept_once_and_never_attributed_to_someone_else() {
        let blog = service(4);
        blog.subscribe().expect("subscribe");
        let entry = signed_entry(9, "From a peer", "body", BLOG_ENTRY_HOPS);

        blog.handle(
            "127.0.0.1:9001".parse().unwrap(),
            NetworkMessage::BlogEntry(entry.clone()),
        );
        blog.handle(
            "127.0.0.1:9001".parse().unwrap(),
            NetworkMessage::BlogEntry(entry.clone()),
        );

        let snapshot = blog.snapshot();
        assert_eq!(snapshot.entries.len(), 1, "flooding is deduplicated");
        assert_eq!(
            snapshot.entries[0].sender.0,
            Keypair::from_seed(&[9u8; 32]).public.0
        );

        // An entry whose text was edited after signing is dropped: authorship
        // is proven by the signature, not by the envelope.
        let mut forged = signed_entry(9, "Harmless", "body", BLOG_ENTRY_HOPS);
        forged.text = "rewritten in transit".to_string();
        blog.handle(
            "127.0.0.1:9001".parse().unwrap(),
            NetworkMessage::BlogEntry(forged),
        );
        assert_eq!(
            blog.snapshot().entries.len(),
            1,
            "a tampered entry never reaches the feed"
        );
    }

    #[test]
    fn entries_while_unsubscribed_are_not_stored() {
        let blog = service(5);
        let entry = signed_entry(8, "Not my business", "body", BLOG_ENTRY_HOPS);

        blog.handle(
            "127.0.0.1:9001".parse().unwrap(),
            NetworkMessage::BlogEntry(entry),
        );

        assert!(
            blog.snapshot().entries.is_empty(),
            "non-readers relay but keep nothing"
        );
    }

    #[test]
    fn history_from_a_peer_merges_without_duplicates_or_forgeries() {
        let blog = service(6);
        blog.subscribe().expect("subscribe");
        let wanted = signed_entry(7, "You missed this", "body", BLOG_ENTRY_HOPS);
        let duplicate = signed_entry(7, "You missed this", "body", 3);
        let mut forged = signed_entry(7, "Another", "body", BLOG_ENTRY_HOPS);
        forged.title = "rewritten".to_string();

        blog.on_history(
            Keypair::from_seed(&[4u8; 32]).public,
            vec![wanted, duplicate, forged],
        );

        let snapshot = blog.snapshot();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].title, "You missed this");
    }

    #[test]
    fn history_for_an_unsubscribed_feed_is_ignored() {
        let blog = service(7);
        let entry = signed_entry(7, "Catch-up for someone else", "body", BLOG_ENTRY_HOPS);
        blog.on_history(Keypair::from_seed(&[4u8; 32]).public, vec![entry]);
        assert!(blog.snapshot().entries.is_empty());
    }

    #[test]
    fn the_reader_roster_tracks_announcements_and_a_vacated_seat_is_released() {
        let blog = service(8);
        blog.subscribe().expect("subscribe");
        let peer = Keypair::from_seed(&[9u8; 32]).public;
        let from: SocketAddr = "127.0.0.1:9002".parse().unwrap();

        blog.on_subscribe(from, peer, true);
        blog.on_subscribe(from, peer, true);
        assert_eq!(blog.snapshot().subscribers, vec![peer]);

        blog.on_subscribe(from, peer, false);
        assert!(blog.snapshot().subscribers.is_empty());
    }

    #[test]
    fn the_feed_persists_across_a_restart_and_merges_with_a_shared_directory() {
        let dir = temp_store_dir("persist");
        let peer_entry = signed_entry(7, "From the daemon", "body", BLOG_ENTRY_HOPS);

        {
            let node = BlogService::with_data_dir(engine(1), Keypair::from_seed(&[1u8; 32]), &dir);
            node.subscribe().expect("subscribe");
            node.post("From the node", "hello").expect("post");
            node.handle(
                "127.0.0.1:9001".parse().unwrap(),
                NetworkMessage::BlogEntry(peer_entry),
            );
        }

        // A restart keeps the subscription and the feed.
        let reloaded = BlogService::with_data_dir(engine(1), Keypair::from_seed(&[1u8; 32]), &dir);
        assert!(reloaded.is_subscribed());
        let reloaded_state = reloaded.snapshot();
        let titles: Vec<&str> = reloaded_state
            .entries
            .iter()
            .map(|entry| entry.title.as_str())
            .collect();
        assert_eq!(titles, ["From the node", "From the daemon"]);
        // The announcement still has to go out: this session has not sent it.
        assert!(!*reloaded.announced.lock().unwrap());

        // A second process sharing the directory inherits the subscription
        // and cannot clobber what is already on the file.
        let other = BlogService::with_data_dir(engine(2), Keypair::from_seed(&[2u8; 32]), &dir);
        assert!(
            other.is_subscribed(),
            "the shared store carries the subscription too"
        );
        other.post("From the browser", "body").expect("post");
        drop(other);

        let check = BlogService::with_data_dir(engine(1), Keypair::from_seed(&[1u8; 32]), &dir);
        let check_state = check.snapshot();
        let titles: Vec<&str> = check_state
            .entries
            .iter()
            .map(|entry| entry.title.as_str())
            .collect();
        assert_eq!(
            titles,
            ["From the node", "From the daemon", "From the browser"],
            "neither process's entries are lost to the other's write"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_blog_file_starts_empty_instead_of_failing() {
        let dir = temp_store_dir("corrupt");
        std::fs::write(dir.join(BLOG_STORE_FILE), b"{not json").expect("write corrupt store");
        let blog = BlogService::with_data_dir(engine(3), Keypair::from_seed(&[3u8; 32]), &dir);
        assert!(blog.snapshot().entries.is_empty());
        assert!(!blog.is_subscribed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One entry by `seed`, with an id distinct per `nonce` so the dedup set
    /// does not collapse them into one message.
    fn signed_entry_nonce(seed: u8, nonce: u8) -> BlogEntryWire {
        let keypair = Keypair::from_seed(&[seed; 32]);
        let mut id = [0u8; 16];
        id[0] = seed;
        id[1] = nonce;
        let title = format!("spam {nonce}");
        BlogEntryWire {
            sender: keypair.public,
            id,
            sent_at: 1_700_000_000,
            title: title.clone(),
            text: "body".to_string(),
            hops: BLOG_ENTRY_HOPS,
            signature: keypair.sign(&blog_entry_sign_message(
                &keypair.public,
                &id,
                1_700_000_000,
                &title,
                "body",
            )),
        }
    }

    #[test]
    fn one_author_cannot_flood_the_feed_beyond_the_per_author_limit() {
        let blog = service(10);
        blog.subscribe().expect("subscribe");
        let from: SocketAddr = "127.0.0.1:9003".parse().unwrap();

        for nonce in 0..(AUTHOR_ENTRY_LIMIT + 5) {
            blog.handle(
                from,
                NetworkMessage::BlogEntry(signed_entry_nonce(9, nonce as u8)),
            );
        }

        assert_eq!(
            blog.snapshot().entries.len(),
            AUTHOR_ENTRY_LIMIT as usize,
            "entries past the per-author window are dropped, not stored or relayed"
        );

        // A different author is unaffected by the first one's budget.
        blog.handle(from, NetworkMessage::BlogEntry(signed_entry_nonce(6, 1)));
        assert_eq!(
            blog.snapshot().entries.len(),
            AUTHOR_ENTRY_LIMIT as usize + 1
        );
    }

    #[test]
    fn my_own_entries_are_rate_limited_too() {
        let blog = service(11);
        blog.subscribe().expect("subscribe");

        for nonce in 0..AUTHOR_ENTRY_LIMIT {
            blog.post("Title", "body")
                .unwrap_or_else(|error| panic!("entry {nonce} is within limit: {error}"));
        }
        let error = blog
            .post("Too fast", "body")
            .expect_err("the local author is bounded like any other");
        assert!(error.contains("too fast"), "got: {error}");
        assert_eq!(blog.snapshot().entries.len(), AUTHOR_ENTRY_LIMIT as usize);
    }

    #[test]
    fn the_feed_evicts_the_oldest_entries_beyond_the_cap() {
        let blog = service(12);
        for nonce in 0..(BLOG_HISTORY_LIMIT + 3) {
            assert!(
                blog.record(signed_entry_nonce(9, nonce as u8)),
                "entry {nonce} is new"
            );
        }

        let snapshot = blog.snapshot();
        assert_eq!(snapshot.entries.len(), BLOG_HISTORY_LIMIT);
        assert_eq!(snapshot.entries.first().unwrap().title, "spam 3");
        assert_eq!(
            snapshot.entries.last().unwrap().title,
            format!("spam {}", BLOG_HISTORY_LIMIT + 2)
        );
    }

    #[test]
    fn a_persisted_entry_tampered_with_after_signing_is_dropped() {
        let dir = temp_store_dir("tampered");
        {
            let node = BlogService::with_data_dir(engine(1), Keypair::from_seed(&[1u8; 32]), &dir);
            node.subscribe().expect("subscribe");
            node.post("Original", "body").expect("post");
        }

        let path = dir.join(BLOG_STORE_FILE);
        let mut store: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read persisted feed"))
                .expect("store is json");
        store["entries"][0]["title"] = serde_json::Value::String("Rewritten".to_string());
        std::fs::write(&path, serde_json::to_vec_pretty(&store).expect("serialize"))
            .expect("write tampered store");

        let reloaded = BlogService::with_data_dir(engine(1), Keypair::from_seed(&[1u8; 32]), &dir);
        assert!(
            reloaded.snapshot().entries.is_empty(),
            "a store edited after signing is not shown as history"
        );
        assert!(
            reloaded.is_subscribed(),
            "the persisted subscription is still honored"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
