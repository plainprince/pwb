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
//! - **IPv6** — the service binds a second, v6-only socket beside the IPv4
//!   one and fans out to `[ff02::1]:7922` (link-local all-nodes, the IPv6
//!   broadcast equivalent) and the site-scoped group `[ff05::255:1]:7922`.
//!   An IPv4-only or IPv6-only host keeps working: a family whose socket
//!   could not bind is simply skipped, and a beacon is only required to
//!   leave on *some* socket. Beacons received on either socket merge into
//!   the same peer table.
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
    BROADCAST_ADDR_V6, DISCOVERY_PORT, MULTICAST_GROUP, MULTICAST_GROUP_V6,
};
pub use error::{BeaconError, DiscoveryError};
pub use service::{DiscoveryConfig, DiscoveryService};
