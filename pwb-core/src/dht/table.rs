//! The Kademlia routing table: buckets, health checks, and selection.

use std::time::{Duration, Instant};

use rand::rngs::OsRng;
use rand::Rng;

use super::constants::{ID_BITS, K_BUCKET_SIZE, MAX_FAILURES, STALE_AFTER};
use super::contact::Contact;
use super::node_id::NodeId;

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
    pub(super) buckets: Vec<Vec<Contact>>,
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
    pub(super) fn candidates_for(&self, target: &NodeId, count: usize) -> Vec<Contact> {
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
