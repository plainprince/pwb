//! Routing-table contact entries with liveness/latency state.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::crypto::PublicKey;

use super::constants::{RTT_DIVISOR, UNKNOWN_RTT};
use super::node_id::NodeId;

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
    pub(super) fn record_rtt(&mut self, sample: Duration) {
        let old = self.rtt.as_millis() as u64;
        let s = sample.as_millis() as u64;
        self.rtt = Duration::from_millis(old - old / RTT_DIVISOR + s / RTT_DIVISOR);
    }
}
