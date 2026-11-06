//! Loss-aware peer reliability tracking.
//!
//! TCP (and therefore Tor) stalls on the slowest hop: in-order delivery turns
//! one lossy relay into head-of-line blocking for the whole circuit. This
//! module keeps the fix local and additive:
//!
//! * every direct message carries an envelope sequence number already, so the
//!   receiver can acknowledge it cheaply with [`NetworkMessage::Ack`];
//! * the sender keeps unacknowledged datagrams for a few seconds and
//!   retransmits them with backoff (selective repeat per message, not
//!   go-back-N, so one lossy peer never stalls the rest);
//! * per-peer RTT + loss estimates rank relays, so onion path selection can
//!   route around hosts that are slow or lossy instead of waiting on them.
//!
//! [`NetworkMessage::Ack`]: super::messages::NetworkMessage::Ack

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::crypto::PublicKey;

/// Base retransmit timeout for an unacknowledged direct message.
pub const RTO_BASE: Duration = Duration::from_millis(250);
/// Upper bound for the exponential backoff.
pub const RTO_MAX: Duration = Duration::from_secs(2);
/// How many times one message is retransmitted before it is dropped.
pub const MAX_RETRANSMITS: u8 = 4;
/// How long an unacknowledged message is kept before it expires anyway.
pub const PENDING_TTL: Duration = Duration::from_secs(10);
/// Maximum unacknowledged messages tracked per engine (bounded memory).
pub const MAX_PENDING: usize = 1024;
/// Cooldown after consecutive failures before a relay is avoided.
pub const AVOID_COOLDOWN: Duration = Duration::from_secs(30);
/// Consecutive failures that mark a relay as avoided.
pub const AVOID_THRESHOLD: u32 = 3;

/// Per-peer loss / latency estimate.
#[derive(Debug, Clone)]
pub struct PeerStats {
    /// EWMA of successful round-trip samples.
    pub rtt: Duration,
    /// Successful acknowledgements / replies.
    pub successes: u64,
    /// Timeouts / retransmit exhaustions.
    pub failures: u64,
    /// Consecutive failures (reset on success).
    pub consecutive_failures: u32,
    /// Last failure time (for cooldown).
    pub last_failure: Option<Instant>,
    /// Number of datagrams retransmitted to this peer.
    pub retransmits: u64,
}

impl Default for PeerStats {
    fn default() -> Self {
        Self {
            rtt: Duration::from_millis(250),
            successes: 0,
            failures: 0,
            consecutive_failures: 0,
            last_failure: None,
            retransmits: 0,
        }
    }
}

impl PeerStats {
    /// Lower is worse; higher means prefer this peer for relays / paths.
    /// Combines latency (fast peers win) with failure history.
    pub fn score(&self) -> f64 {
        let rtt_ms = self.rtt.as_secs_f64() * 1000.0;
        let latency = 1.0 / (1.0 + rtt_ms / 100.0);
        let total = self.successes + self.failures;
        let reliability = if total == 0 {
            0.8 // unknown peers are usable but rank below proven ones
        } else {
            (self.successes as f64 + 1.0) / (total as f64 + 2.0)
        };
        // Consecutive failures heavily discount a peer even before cooldown.
        let penalty = 1.0 / (1.0 + self.consecutive_failures as f64);
        latency * reliability * penalty
    }

    pub fn record_success(&mut self, sample: Option<Duration>) {
        self.successes = self.successes.saturating_add(1);
        self.consecutive_failures = 0;
        if let Some(s) = sample {
            // EWMA α = 1/8, matching the DHT RTT smoothing.
            let old_ms = self.rtt.as_millis() as u64;
            let new_ms = s.as_millis().min(5000) as u64;
            self.rtt = Duration::from_millis(old_ms - old_ms / 8 + new_ms / 8);
        }
    }

    pub fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure = Some(Instant::now());
    }
}

/// Shared handle for the loss-aware tracker.
pub type SharedReliability = std::sync::Arc<std::sync::Mutex<ReliabilityTracker>>;
/// Shared handle for unacknowledged direct messages (seq -> pending).
pub type SharedPending = std::sync::Arc<std::sync::Mutex<HashMap<u64, PendingSend>>>;

/// Loss-aware tracker shared by the engine loops.
#[derive(Debug, Default)]
pub struct ReliabilityTracker {
    peers: HashMap<PublicKey, PeerStats>,
}

impl ReliabilityTracker {
    pub fn stats(&self, peer: &PublicKey) -> Option<&PeerStats> {
        self.peers.get(peer)
    }

    pub fn score(&self, peer: &PublicKey) -> f64 {
        self.peers.get(peer).map(|s| s.score()).unwrap_or(0.8)
    }

    pub fn record_success(&mut self, peer: &PublicKey, rtt: Option<Duration>) {
        self.peers
            .entry(*peer)
            .or_default()
            .record_success(rtt);
    }

    pub fn record_failure(&mut self, peer: &PublicKey) {
        // Bound memory: evict the worst peer when full.
        if !self.peers.contains_key(peer) && self.peers.len() >= 4096 {
            let worst = self
                .peers
                .iter()
                .min_by(|a, b| {
                    a.1.score()
                        .partial_cmp(&b.1.score())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(k, _)| *k);
            if let Some(worst) = worst {
                self.peers.remove(&worst);
            }
        }
        self.peers.entry(*peer).or_default().record_failure();
    }

    pub fn record_retransmit(&mut self, peer: &PublicKey) {
        self.peers.entry(*peer).or_default().retransmits += 1;
    }

    /// Whether this peer should currently be avoided for new paths.
    pub fn should_avoid(&self, peer: &PublicKey) -> bool {
        let Some(stats) = self.peers.get(peer) else {
            return false;
        };
        if stats.consecutive_failures < AVOID_THRESHOLD {
            return false;
        }
        match stats.last_failure {
            Some(at) => at.elapsed() < AVOID_COOLDOWN,
            None => false,
        }
    }

    /// Rank candidates best-first, filtering peers in cooldown unless
    /// everything is in cooldown (then return the least-bad).
    pub fn ranked(&self, candidates: &[PublicKey]) -> Vec<PublicKey> {
        let mut scored: Vec<(PublicKey, f64)> = candidates
            .iter()
            .map(|k| (*k, self.score(k)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let usable: Vec<PublicKey> = scored
            .iter()
            .filter(|(k, _)| !self.should_avoid(k))
            .map(|(k, _)| *k)
            .collect();
        if usable.is_empty() {
            scored.into_iter().map(|(k, _)| k).collect()
        } else {
            usable
        }
    }
}

/// One unacknowledged direct message awaiting an [`Ack`](super::messages::NetworkMessage::Ack).
#[derive(Debug, Clone)]
pub struct PendingSend {
    pub peer: PublicKey,
    pub addr: std::net::SocketAddr,
    pub datagrams: Vec<Vec<u8>>,
    pub attempts: u8,
    pub last_send: Instant,
    pub created: Instant,
    /// RTT base for backoff (per-peer EWMA at send time is folded in by caller).
    pub rto: Duration,
}

impl PendingSend {
    pub fn new(
        peer: PublicKey,
        addr: std::net::SocketAddr,
        datagrams: Vec<Vec<u8>>,
        rto: Duration,
    ) -> Self {
        let now = Instant::now();
        Self {
            peer,
            addr,
            datagrams,
            attempts: 0,
            last_send: now,
            created: now,
            rto,
        }
    }

    /// Current retransmit deadline with exponential backoff.
    pub fn deadline(&self) -> Instant {
        let shift = self.attempts.min(4) as u32;
        let backoff = self.rto.checked_mul(1 << shift).unwrap_or(RTO_MAX).min(RTO_MAX);
        self.last_send + backoff
    }

    pub fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.created) >= PENDING_TTL || self.attempts > MAX_RETRANSMITS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reliable_peers_outrank_lossy_slow_ones() {
        let mut tracker = ReliabilityTracker::default();
        let good = PublicKey([1; 32]);
        let bad = PublicKey([2; 32]);
        tracker.record_success(&good, Some(Duration::from_millis(20)));
        tracker.record_success(&good, Some(Duration::from_millis(25)));
        for _ in 0..5 {
            tracker.record_failure(&bad);
        }
        assert!(tracker.score(&good) > tracker.score(&bad));
        assert!(tracker.should_avoid(&bad));
        assert!(!tracker.should_avoid(&good));
        let ranked = tracker.ranked(&[bad, good]);
        assert_eq!(ranked[0], good);
    }

    #[test]
    fn pending_backoff_grows_and_expires() {
        let peer = PublicKey([9; 32]);
        let addr: std::net::SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let mut pending = PendingSend::new(peer, addr, vec![vec![0u8; 10]], RTO_BASE);
        let first = pending.deadline();
        pending.attempts = 2;
        let later = pending.deadline();
        assert!(later > first);
        assert!(!pending.expired(Instant::now()));
        pending.attempts = MAX_RETRANSMITS + 1;
        assert!(pending.expired(Instant::now()));
    }
}
