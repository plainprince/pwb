//! LAN auto-discovery via UDP broadcast beacons and IP multicast (TODO §3.1).
//!
//! Every node periodically announces itself with a small, Ed25519-signed
//! **beacon** on the local network, and listens for the same:
//!
//! - **Broadcast fanout** — beacons go to `255.255.255.255:7922`, reaching
//!   every host on the LAN subnet.
//! - **Multicast group** — beacons are also sent to (and received from)
//!   `239.255.0.1:7922`; only sockets that joined the group exchange this
//!   traffic, so the group works independently of subnet broadcast rules.
//! - **Authenticated** — each beacon carries the announcer's Ed25519 public
//!   key and a signature over magic/version/port/timestamp/key, so a peer
//!   identity cannot be forged or altered in flight.
//! - **Fresh** — timestamps outside [`DiscoveryConfig::max_age`] are
//!   rejected in both directions (stale replays and future-dated floods).
//!
//! Valid beacons merge into a peer table — the same [`crate::net::PeerInfo`]
//! table [`crate::net::NodeEngine`] uses. Bind with
//! [`DiscoveryService::bind_with_table`] passing `engine.peer_table()` and
//! LAN peers appear to the engine immediately; [`DiscoveryService::spawn`]
//! then runs announce + receive as one background task.
//!
//! Address rebinding is rate-limited: a beacon claiming a *different*
//! address for a recently seen key refreshes `last_seen` but keeps the old
//! address until [`ADDR_REBIND_GRACE`] elapses, so a replayed beacon cannot
//! redirect peer traffic to an attacker's socket.

mod beacon;
mod constants;
mod error;
mod service;

#[cfg(test)]
mod tests;

pub use beacon::{unix_time, Beacon};
pub use constants::{
    ADDR_REBIND_GRACE, BEACON_INTERVAL, BEACON_MAX_AGE, BEACON_SIZE, BROADCAST_ADDR,
    DISCOVERY_PORT, MULTICAST_GROUP,
};
pub use error::{BeaconError, DiscoveryError};
pub use service::{DiscoveryConfig, DiscoveryService};
