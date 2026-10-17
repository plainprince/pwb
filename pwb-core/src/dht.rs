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

mod constants;
mod contact;
mod node_id;
mod table;

#[cfg(test)]
mod tests;

pub use constants::{ID_BITS, K_BUCKET_SIZE, MAX_FAILURES, STALE_AFTER, UNKNOWN_RTT};
pub use contact::Contact;
pub use node_id::NodeId;
pub use table::{KConfig, RoutingTable, UpdateOutcome};
