//! Kademlia DHT peer routing table (§3 Decentralized Node Discovery).
//!
//! Maintains the classic XOR-metric k-bucket table over 256-bit node IDs
//! derived (BLAKE3) from Ed25519 public keys, with the operational pieces the
//! TODO bullet calls for:
//!
//! - **Ping/pong health checks** — [`RoutingTable::update`] never silently
//!   displaces a live node: when the target bucket is full it surfaces the
//!   least-recently-seen head as [`UpdateOutcome::PingLeastRecent`]; the
//!   caller pings it and answers with [`RoutingTable::mark_alive`] (pong:
//!   refresh, fold the measured RTT, reset failures) or
//!   [`RoutingTable::mark_failed`] (timeout: after
//!   [`KConfig::max_failures`] consecutive failures the entry is evicted and
//!   the pending contact can be re-inserted). [`RoutingTable::stale_for_check`]
//!   returns quiet entries due for a proactive ping sweep.
//! - **Latency-weighted bucket selection** — [`RoutingTable::select_contacts`]
//!   walks buckets outward from the target's bucket (Kademlia's lookup-start
//!   order) and samples without replacement with probability
//!   ∝ 1/(1+RTT-ms), so fast responders dominate lookup fan-out.
//!
//! All time is caller-supplied (`Instant` parameters) — no sleeps, no sockets;
//! the ping/pong *transport* stays with the caller (see `discovery` for UDP).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use rand::rngs::OsRng;
use rand::Rng;

use crate::crypto::PublicKey;

/// Kademlia bucket capacity (contacts per bucket).
pub const K_BUCKET_SIZE: usize = 8;

/// Node-ID width in bits.
pub const ID_BITS: usize = 256;

/// Consecutive-failure threshold after which an entry is evicted.
pub const MAX_FAILURES: u32 = 3;

/// Quiet-time threshold after which an entry is due for a ping sweep.
pub const STALE_AFTER: Duration = Duration::from_secs(900);

/// Neutral RTT used before a peer has been measured (scoring + insert).
pub const UNKNOWN_RTT: Duration = Duration::from_millis(250);

/// EWMA smoothing divisor for RTT samples (α = 1/8).
const RTT_DIVISOR: u64 = 8;

/// A 256-bit Kademlia node ID: BLAKE3 of the node's Ed25519 public key.
///
/// Distances are plain XOR of the big-endian bit strings; lexicographic byte
/// order equals numeric order, so `Ord` on the raw bytes *is* distance order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId([u8; 32]);

impl NodeId {
    /// Derive the routing-table ID for a node identity.
    pub fn from_public_key(key: &PublicKey) -> Self {
        NodeId(*blake3::hash(&key.0).as_bytes())
    }

    /// Wrap raw ID bytes (tests, wire format).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        NodeId(bytes)
    }

    /// Raw big-endian bytes of the ID.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// XOR distance between two IDs (the Kademlia metric).
    pub fn xor(&self, other: &NodeId) -> [u8; 32] {
        let mut d = [0u8; 32];
        for (i, (a, b)) in self.0.iter().zip(other.0.iter()).enumerate() {
            d[i] = a ^ b;
        }
        d
    }

    /// Length of the shared bit prefix (0..=256); 256 only for equal IDs.
    pub fn common_prefix_len(&self, other: &NodeId) -> usize {
        let mut i = 0;
        while i < ID_BITS {
            let byte = i / 8;
            let bit = i % 8;
            if self.0[byte] & (0x80 >> bit) != other.0[byte] & (0x80 >> bit) {
                break;
            }
            i += 1;
        }
        i
    }
}

/// A routing-table entry: identity, dial address, liveness/latency state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    /// Node identity (Ed25519 public key).
    pub public_key: PublicKey,
    /// Derived routing ID (`NodeId::from_public_key`).
    pub node_id: NodeId,
    /// Last advertised dial address.
    pub address: SocketAddr,
    /// Time of the last sighting (beacon, pong, or successful exchange).
    pub last_seen: Instant,
    /// Smoothed round-trip time (EWMA α=1/8 of pong samples).
    pub rtt: Duration,
    /// Consecutive failed health checks since the last success.
    pub failures: u32,
}

impl Contact {
    /// Contact with the neutral [`UNKNOWN_RTT`] placeholder; set `rtt`
    /// directly when a measurement is already known.
    pub fn new(public_key: PublicKey, address: SocketAddr) -> Self {
        Contact {
            public_key,
            node_id: NodeId::from_public_key(&public_key),
            address,
            last_seen: Instant::now(),
            rtt: UNKNOWN_RTT,
            failures: 0,
        }
    }

    /// Selection weight: monotone decreasing in RTT (unknown → 250ms).
    pub fn latency_score(&self) -> f64 {
        1.0 / (1.0 + self.rtt.as_secs_f64() * 1000.0)
    }

    /// Fold one pong RTT sample into the smoothed estimate.
    fn record_rtt(&mut self, sample: Duration) {
        let old = self.rtt.as_millis() as u64;
        let s = sample.as_millis() as u64;
        self.rtt = Duration::from_millis(old - old / RTT_DIVISOR + s / RTT_DIVISOR);
    }
}

/// Result of offering a sighting to [`RoutingTable::update`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The contact was appended to its bucket.
    Inserted,
    /// The contact was already known: refreshed, moved to the bucket tail.
    Refreshed,
    /// The bucket is full. Kademlia requires pinging the least-recently-seen
    /// entry (`head`) first — on pong call [`RoutingTable::mark_alive`], on
    /// timeout call [`RoutingTable::mark_failed`] until it is evicted, then
    /// re-submit the pending contact with `update`.
    PingLeastRecent { least_recent: Contact },
    /// The local node's own ID is never stored in its own table.
    RejectedSelf,
}

/// Tunables for a [`RoutingTable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KConfig {
    /// Contacts per bucket.
    pub bucket_size: usize,
    /// Consecutive failures before eviction.
    pub max_failures: u32,
    /// Quiet time before an entry is swept by [`stale_for_check`].
    pub stale_after: Duration,
}

impl Default for KConfig {
    fn default() -> Self {
        KConfig {
            bucket_size: K_BUCKET_SIZE,
            max_failures: MAX_FAILURES,
            stale_after: STALE_AFTER,
        }
    }
}

/// Kademlia routing table: 256 XOR-metric k-buckets around `local_id`.
#[derive(Debug, Clone)]
pub struct RoutingTable {
    local_id: NodeId,
    config: KConfig,
    /// `buckets[i]` holds contacts whose ID shares exactly `i` leading bits
    /// with `local_id` (head = least-recently seen, tail = most recent).
    buckets: Vec<Vec<Contact>>,
}

impl RoutingTable {
    /// Table with [`KConfig::default`].
    pub fn new(local_id: NodeId) -> Self {
        Self::with_config(local_id, KConfig::default())
    }

    /// Table with an explicit configuration.
    pub fn with_config(local_id: NodeId, config: KConfig) -> Self {
        RoutingTable {
            local_id,
            config,
            buckets: vec![Vec::new(); ID_BITS],
        }
    }

    /// The local node's routing ID.
    pub fn local_id(&self) -> NodeId {
        self.local_id
    }

    /// Total contacts across all buckets.
    pub fn len(&self) -> usize {
        self.buckets.iter().map(Vec::len).sum()
    }

    /// True when the table holds no contacts.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True when `id` is a known contact.
    pub fn contains(&self, id: &NodeId) -> bool {
        self.bucket_of(id).iter().any(|c| &c.node_id == id)
    }

    /// Bucket index for an ID: its common-prefix length with the local ID,
    /// clamped to 255 (the local ID itself would be 256).
    pub fn bucket_index(&self, id: &NodeId) -> usize {
        id.common_prefix_len(&self.local_id).min(ID_BITS - 1)
    }

    /// Remove and return a contact.
    pub fn remove(&mut self, id: &NodeId) -> Option<Contact> {
        let idx = self.bucket_index(id);
        let bucket = &mut self.buckets[idx];
        let pos = bucket.iter().position(|c| &c.node_id == id)?;
        Some(bucket.remove(pos))
    }

    /// Offer a sighting (beacon, handshake, or pong). New contacts are
    /// appended; known contacts are refreshed in place and moved to the
    /// bucket tail; a full bucket surfaces its head for a ping check.
    /// RTT smoothing happens through [`mark_alive`](Self::mark_alive) — the
    /// contact's current `rtt` field is stored verbatim on first insert.
    pub fn update(&mut self, contact: Contact, now: Instant) -> UpdateOutcome {
        if contact.node_id == self.local_id {
            return UpdateOutcome::RejectedSelf;
        }
        let idx = self.bucket_index(&contact.node_id);
        let bucket_size = self.config.bucket_size;
        let bucket = &mut self.buckets[idx];
        if let Some(pos) = bucket.iter().position(|c| c.node_id == contact.node_id) {
            {
                let entry = &mut bucket[pos];
                entry.address = contact.address;
                entry.last_seen = now;
                entry.failures = 0;
            }
            let entry = bucket.remove(pos);
            bucket.push(entry);
            return UpdateOutcome::Refreshed;
        }
        if bucket.len() >= bucket_size {
            return UpdateOutcome::PingLeastRecent {
                least_recent: bucket[0].clone(),
            };
        }
        let mut stored = contact;
        stored.last_seen = now;
        bucket.push(stored);
        UpdateOutcome::Inserted
    }

    /// Pong / successful exchange: refresh `id`, fold `sample` (when any)
    /// into the smoothed RTT, and move the entry to the bucket tail.
    /// Returns false when the ID is unknown.
    pub fn mark_alive(&mut self, id: &NodeId, sample: Option<Duration>, now: Instant) -> bool {
        let idx = self.bucket_index(id);
        let bucket = &mut self.buckets[idx];
        let Some(pos) = bucket.iter().position(|c| &c.node_id == id) else {
            return false;
        };
        let mut entry = bucket.remove(pos);
        entry.last_seen = now;
        entry.failures = 0;
        if let Some(sample) = sample {
            entry.record_rtt(sample);
        }
        bucket.push(entry);
        true
    }

    /// Failed ping: count one consecutive failure; evict once
    /// [`KConfig::max_failures`] is reached (returning the corpse). Eviction
    /// frees the slot so the pending replacement can be re-`update`d.
    pub fn mark_failed(&mut self, id: &NodeId) -> Option<Contact> {
        let idx = self.bucket_index(id);
        let bucket = &mut self.buckets[idx];
        let pos = bucket.iter().position(|c| &c.node_id == id)?;
        bucket[pos].failures += 1;
        if bucket[pos].failures >= self.config.max_failures {
            Some(bucket.remove(pos))
        } else {
            None
        }
    }

    /// Entries quiet longer than [`KConfig::stale_after`] as of `now` — the
    /// proactive ping/pong sweep set.
    pub fn stale_for_check(&self, now: Instant) -> Vec<Contact> {
        let stale_after = self.config.stale_after;
        self.iter()
            .filter(|c| now.saturating_duration_since(c.last_seen) >= stale_after)
            .cloned()
            .collect()
    }

    /// Up to `count` contacts closest to `target` under XOR distance
    /// (full-table scan; tables at most 256×k make bucket-walking a wash).
    pub fn closest(&self, target: &NodeId, count: usize) -> Vec<Contact> {
        let mut scored: Vec<([u8; 32], Contact)> = self
            .iter()
            .map(|c| (c.node_id.xor(target), c.clone()))
            .collect();
        scored.sort_by_key(|(dist, _)| *dist);
        scored.into_iter().take(count).map(|(_, c)| c).collect()
    }

    /// Latency-weighted contact selection for a lookup toward `target`:
    /// candidates are gathered bucket-by-bucket outward from the target's
    /// bucket (Kademlia's nearest-first order), then `count` of them are
    /// sampled without replacement with weight ∝ [`Contact::latency_score`].
    pub fn select_contacts(&self, target: &NodeId, count: usize) -> Vec<Contact> {
        let mut rng = OsRng;
        self.select_contacts_with(target, count, &mut rng)
    }

    /// [`select_contacts`](Self::select_contacts) with a caller-supplied RNG
    /// (deterministic tests, simulations).
    pub fn select_contacts_with<R: Rng + ?Sized>(
        &self,
        target: &NodeId,
        count: usize,
        rng: &mut R,
    ) -> Vec<Contact> {
        let candidates = self.candidates_for(target, count);
        weighted_sample(&candidates, count, rng)
    }

    /// Iterate every contact (bucket order, heads first).
    pub fn iter(&self) -> impl Iterator<Item = &Contact> {
        self.buckets.iter().flat_map(|bucket| bucket.iter())
    }

    /// Contacts gathered nearest-bucket-first around `target` until at least
    /// `count` candidates exist (or every bucket has been walked).
    fn candidates_for(&self, target: &NodeId, count: usize) -> Vec<Contact> {
        let center = self.bucket_index(target);
        let mut out = Vec::new();
        for distance in 0..ID_BITS {
            let hi = center + distance;
            if hi < ID_BITS {
                out.extend(self.buckets[hi].iter().cloned());
                if out.len() >= count {
                    return out;
                }
            }
            if distance > 0 {
                if let Some(lo) = center.checked_sub(distance) {
                    out.extend(self.buckets[lo].iter().cloned());
                    if out.len() >= count {
                        return out;
                    }
                }
            }
        }
        out
    }

    fn bucket_of(&self, id: &NodeId) -> &[Contact] {
        &self.buckets[self.bucket_index(id)]
    }
}

/// Weighted sample of `count` contacts without replacement
/// (probability ∝ `latency_score`).
fn weighted_sample<R: Rng + ?Sized>(items: &[Contact], count: usize, rng: &mut R) -> Vec<Contact> {
    let mut remaining = items.to_vec();
    let mut out = Vec::with_capacity(count.min(remaining.len()));
    while out.len() < count && !remaining.is_empty() {
        let total: f64 = remaining.iter().map(Contact::latency_score).sum();
        let mut pick = rng.gen_range(0.0..total);
        let mut idx = remaining.len() - 1;
        for (i, contact) in remaining.iter().enumerate() {
            pick -= contact.latency_score();
            if pick < 0.0 {
                idx = i;
                break;
            }
        }
        out.push(remaining.swap_remove(idx));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::net::Ipv4Addr;

    fn key(byte: u8) -> PublicKey {
        PublicKey([byte; 32])
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    fn contact(byte: u8, port: u16) -> Contact {
        Contact::new(key(byte), addr(port))
    }

    /// Contact with a crafted ID whose first bit is 1 — relative to the local
    /// ID `[0;32]` that places it deterministically in bucket 0 (BLAKE3
    /// hashes would scatter randomly across buckets).
    fn bucket0_contact(byte: u8, port: u16) -> Contact {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x80 | byte;
        Contact {
            node_id: NodeId::from_bytes(bytes),
            ..Contact::new(key(byte), addr(port))
        }
    }

    /// Contact with a fully chosen ID (first two bytes set from `first`).
    fn id_contact(first: u8, port: u16) -> Contact {
        let mut bytes = [0u8; 32];
        bytes[0] = first;
        bytes[1] = first;
        Contact {
            node_id: NodeId::from_bytes(bytes),
            ..Contact::new(key(first), addr(port))
        }
    }

    fn table() -> RoutingTable {
        RoutingTable::new(NodeId::from_bytes([0u8; 32]))
    }

    #[test]
    fn test_node_id_metrics() {
        let a = NodeId::from_bytes([0u8; 32]);
        let mut bytes = [0u8; 32];
        bytes[0] = 0b1000_0000; // differs in the very first bit
        let b = NodeId::from_bytes(bytes);
        assert_eq!(a.common_prefix_len(&a), 256);
        assert_eq!(a.common_prefix_len(&b), 0);
        assert_eq!(b.common_prefix_len(&a), 0);
        assert_eq!(a.xor(&a), [0u8; 32]);
        assert_eq!(a.xor(&b)[0], 0b1000_0000);
        // XOR is symmetric and the metric is commutative.
        assert_eq!(a.xor(&b), b.xor(&a));

        bytes[0] = 0b0100_0000; // differs in the second bit only
        let c = NodeId::from_bytes(bytes);
        assert_eq!(a.common_prefix_len(&c), 1);

        // Distance ordering matches lexicographic ID order for equal prefixes.
        let far = NodeId::from_bytes([0xFF; 32]);
        assert!(a.xor(&c) < a.xor(&far));
    }

    #[test]
    fn test_bucket_index_follows_common_prefix() {
        let table = table();
        let mut bytes = [0u8; 32];
        bytes[0] = 0b0100_0000; // shares 1 bit with local [0;32]
        assert_eq!(table.bucket_index(&NodeId::from_bytes(bytes)), 1);
        bytes[0] = 0b1000_0000; // shares 0 bits
        assert_eq!(table.bucket_index(&NodeId::from_bytes(bytes)), 0);
        bytes[0] = 0b0000_0001; // differs in the 8th bit → shares 7
        assert_eq!(table.bucket_index(&NodeId::from_bytes(bytes)), 7);
        bytes[0] = 0;
        bytes[31] = 0b0000_0001; // differs only in the very last bit
        assert_eq!(table.bucket_index(&NodeId::from_bytes(bytes)), 255);
        // The local ID clamps to the last bucket instead of 256.
        assert_eq!(table.bucket_index(&table.local_id()), 255);
    }

    #[test]
    fn test_update_inserts_and_closest_orders_by_xor() {
        let mut table = table();
        for i in 1..=5u8 {
            let outcome = table.update(id_contact(i, 9000 + u16::from(i)), Instant::now());
            assert_eq!(outcome, UpdateOutcome::Inserted);
        }
        assert_eq!(table.len(), 5);

        // IDs [i;i,0…0] all sit in bucket 8; nearest to [0;32] is the
        // smallest first byte under XOR.
        let target = NodeId::from_bytes([0u8; 32]);
        let closest = table.closest(&target, 3);
        assert_eq!(closest.len(), 3);
        let first_bytes: Vec<u8> = closest.iter().map(|c| c.node_id.as_bytes()[0]).collect();
        assert_eq!(first_bytes, vec![1, 2, 3]);
        // Distances are non-decreasing.
        let dists: Vec<[u8; 32]> = closest.iter().map(|c| c.node_id.xor(&target)).collect();
        let mut sorted = dists.clone();
        sorted.sort();
        assert_eq!(dists, sorted);
    }

    #[test]
    fn test_update_refresh_moves_to_tail_and_resets_failures() {
        let mut table = table();
        let now = Instant::now();
        let mut ids = Vec::new();
        for i in 1..=3u8 {
            let c = bucket0_contact(i, 9000);
            ids.push(c.node_id);
            table.update(c, now);
        }
        // Age the first entry so the tail move is observable.
        table.mark_failed(&ids[0]);
        assert_eq!(
            table.iter().map(|c| c.failures).collect::<Vec<_>>(),
            vec![1, 0, 0]
        );

        assert_eq!(
            table.update(bucket0_contact(1, 9111), now + Duration::from_secs(1)),
            UpdateOutcome::Refreshed
        );
        let order: Vec<NodeId> = table.iter().map(|c| c.node_id).collect();
        assert_eq!(
            order,
            vec![ids[1], ids[2], ids[0]],
            "refreshed contact moves to tail"
        );
        assert_eq!(table.iter().last().unwrap().failures, 0);
        assert_eq!(table.iter().last().unwrap().address, addr(9111));
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn test_update_rejects_self() {
        let mut table = table();
        let mut self_contact = contact(0, 1);
        self_contact.node_id = table.local_id();
        assert_eq!(
            table.update(self_contact, Instant::now()),
            UpdateOutcome::RejectedSelf
        );
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn test_full_bucket_surfaces_lru_for_ping() {
        let mut table = table();
        let now = Instant::now();
        for i in 1..=K_BUCKET_SIZE as u8 {
            table.update(bucket0_contact(i, 9000), now);
        }
        // Every crafted contact lands in bucket 0 (first bit set).
        assert_eq!(table.buckets[0].len(), K_BUCKET_SIZE);

        let outcome = table.update(bucket0_contact(99, 9999), now + Duration::from_secs(1));
        let UpdateOutcome::PingLeastRecent { least_recent } = outcome else {
            panic!("full bucket must surface the head, got {outcome:?}");
        };
        // Head = least-recently seen = the first inserted contact.
        assert_eq!(least_recent.public_key, key(1));
        assert_eq!(table.len(), K_BUCKET_SIZE, "pending contact not stored yet");
    }

    #[test]
    fn test_ping_pong_replacement_cycle() {
        let mut table = table();
        let t0 = Instant::now();
        for i in 1..=K_BUCKET_SIZE as u8 {
            table.update(bucket0_contact(i, 9000), t0);
        }
        let pending = bucket0_contact(99, 9999);
        let pending_id = pending.node_id;
        let UpdateOutcome::PingLeastRecent { least_recent } = table.update(pending, t0) else {
            panic!("bucket must be full");
        };

        // Pong path: the head answers, folds its RTT, and stays.
        let lru_id = least_recent.node_id;
        assert!(table.mark_alive(&lru_id, Some(Duration::from_millis(7)), t0));
        assert_eq!(table.iter().last().unwrap().node_id, lru_id);
        assert_eq!(
            table.iter().last().unwrap().rtt,
            Duration::from_millis(219), // 250 − 250/8 + 7/8
        );

        // Timeout path: head never answers → evicted after max_failures,
        // then the pending contact fits.
        assert!(table.mark_failed(&lru_id).is_none());
        assert!(table.mark_failed(&lru_id).is_none());
        let evicted = table.mark_failed(&lru_id);
        assert!(evicted.is_some(), "head evicted on final failure");
        assert!(!table.contains(&lru_id));
        assert_eq!(table.len(), K_BUCKET_SIZE - 1);
        assert_eq!(
            table.update(bucket0_contact(99, 9999), t0 + Duration::from_secs(2)),
            UpdateOutcome::Inserted
        );
        assert_eq!(table.len(), K_BUCKET_SIZE);
        assert!(table.contains(&pending_id));
    }

    #[test]
    fn test_stale_for_check_threshold() {
        let mut table = table();
        let t0 = Instant::now();
        let first = bucket0_contact(1, 9000);
        let first_id = first.node_id;
        table.update(first, t0);
        // The second entry is refreshed right at the threshold boundary.
        table.update(bucket0_contact(2, 9000), t0 + STALE_AFTER);

        let soon = t0 + STALE_AFTER - Duration::from_secs(1);
        assert!(table.stale_for_check(soon).is_empty());

        // Only the first entry has crossed the quiet threshold.
        let later = t0 + STALE_AFTER + Duration::from_secs(1);
        let mut stale = table.stale_for_check(later);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale.pop().unwrap().public_key, key(1));

        // A pong brings it back out of the sweep set.
        assert!(table.mark_alive(&first_id, None, later));
        assert!(table.stale_for_check(later).is_empty());
    }

    #[test]
    fn test_mark_alive_and_remove_unknown_ids() {
        let mut table = table();
        let ghost = NodeId::from_public_key(&key(42));
        assert!(!table.mark_alive(&ghost, Some(Duration::from_millis(1)), Instant::now()));
        assert!(table.mark_failed(&ghost).is_none());
        assert!(table.remove(&ghost).is_none());
        assert!(table.is_empty());

        table.update(contact(42, 9000), Instant::now());
        assert!(table.contains(&ghost));
        assert!(table.remove(&ghost).is_some());
        assert!(!table.contains(&ghost));
    }

    #[test]
    fn test_bucket_never_exceeds_capacity() {
        let mut table = table();
        let now = Instant::now();
        for i in 0..64u8 {
            table.update(bucket0_contact(i, 9000), now);
            assert!(table.buckets[0].len() <= K_BUCKET_SIZE);
        }
        assert_eq!(table.len(), K_BUCKET_SIZE);
    }

    #[test]
    fn test_candidates_walk_buckets_outward() {
        let mut table = table();
        let t0 = Instant::now();
        // X shares 1 bit with local [0;32] → bucket 1; Y shares 0 → bucket 0.
        let mut x_bytes = [0u8; 32];
        x_bytes[0] = 0b0100_0000;
        let mut y_bytes = [0u8; 32];
        y_bytes[0] = 0b1000_0000;
        let x = Contact::new(key(7), addr(1));
        let x = Contact {
            node_id: NodeId::from_bytes(x_bytes),
            ..x
        };
        let y = Contact::new(key(8), addr(2));
        let y = Contact {
            node_id: NodeId::from_bytes(y_bytes),
            ..y
        };
        table.update(y, t0);
        table.update(x, t0);

        // Target in bucket 1: X (same bucket) must be gathered before Y.
        let target = NodeId::from_bytes(x_bytes);
        let candidates = table.candidates_for(&target, 2);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].node_id, NodeId::from_bytes(x_bytes));
        assert_eq!(candidates[1].node_id, NodeId::from_bytes(y_bytes));
    }

    #[test]
    fn test_latency_weighted_selection_prefers_fast_nodes() {
        let mut table = table();
        let t0 = Instant::now();
        // One fast responder and three slow ones, all in bucket 0 so the
        // candidate walk gathers the whole set.
        let mut fast = bucket0_contact(1, 9001);
        fast.rtt = Duration::from_millis(1);
        table.update(fast, t0);
        for i in 2..=4u8 {
            let mut slow = bucket0_contact(i, 9000 + u16::from(i));
            slow.rtt = Duration::from_millis(900);
            table.update(slow, t0);
        }

        let target = table.local_id();
        let mut rng = StdRng::seed_from_u64(7);
        let mut fast_wins = 0;
        for _ in 0..500 {
            let picked = table.select_contacts_with(&target, 1, &mut rng);
            assert_eq!(picked.len(), 1);
            if picked[0].rtt < Duration::from_millis(50) {
                fast_wins += 1;
            }
        }
        // Unweighted would win ~125/500; weighted is >99% at 1ms vs 900ms.
        assert!(fast_wins > 400, "fast node won {fast_wins}/500 draws");
    }

    #[test]
    fn test_select_respects_count_and_uniqueness() {
        let mut table = table();
        let t0 = Instant::now();
        for i in 1..=10u8 {
            table.update(contact(i, 9000), t0);
        }
        let target = table.local_id();
        let mut rng = StdRng::seed_from_u64(3);
        let picked = table.select_contacts_with(&target, 4, &mut rng);
        assert_eq!(picked.len(), 4);
        let unique: std::collections::HashSet<_> = picked.iter().map(|c| c.node_id).collect();
        assert_eq!(unique.len(), 4, "no duplicates without replacement");
        // Selecting more than exist returns everyone exactly once.
        let all = table.select_contacts_with(&target, 64, &mut rng);
        assert_eq!(all.len(), table.len());
    }

    #[tokio::test]
    async fn test_udp_ping_pong_feeds_latency_health_check() {
        use tokio::net::UdpSocket;

        let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let b_addr = b.local_addr().unwrap();

        // Ping.
        let start = Instant::now();
        a.send_to(b"ping", b_addr).await.unwrap();
        let mut buf = [0u8; 8];
        let (n, from) = b.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");

        // Pong.
        b.send_to(b"pong", from).await.unwrap();
        let (n, _) = a.recv_from(&mut buf).await.unwrap();
        let rtt = start.elapsed();
        assert_eq!(&buf[..n], b"pong");

        // The measured RTT seeds the contact; the table stores it verbatim
        // on insert and the peer is immediately selectable.
        let mut table = table();
        let peer_key = key(9);
        let mut peer = Contact::new(peer_key, b_addr);
        peer.rtt = rtt;
        assert_eq!(table.update(peer, Instant::now()), UpdateOutcome::Inserted);

        let peer_id = NodeId::from_public_key(&peer_key);
        let stored = table.closest(&peer_id, 1);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].rtt, rtt);
        assert!(
            rtt < UNKNOWN_RTT,
            "loopback pong is faster than the placeholder"
        );

        // Pong refresh path folds a second sample through the EWMA.
        let sample = Duration::from_millis(3);
        assert!(table.mark_alive(&peer_id, Some(sample), Instant::now()));
        let folded = table.closest(&peer_id, 1)[0].rtt;
        assert!(folded < rtt + sample, "EWMA moves toward the new sample");

        // And the latency-weighted selector returns the only peer.
        let picked = table.select_contacts(&NodeId::from_bytes([0xFF; 32]), 4);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].node_id, peer_id);
    }
}
