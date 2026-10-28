//! Discovery addressing, framing, and timing constants.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

/// Discovery port shared by every PlainWeb node (beacons + multicast).
pub const DISCOVERY_PORT: u16 = 7922;

/// IPv4 multicast group for discovery beacons (TODO §3.1).
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 0, 1);

/// IPv4 limited broadcast address (whole subnet).
pub const BROADCAST_ADDR: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);

/// IPv6 site-scoped multicast group for discovery beacons — the counterpart
/// of [`MULTICAST_GROUP`]. IPv4's administratively scoped space (239/8) has
/// no exact analogue, so the group sits at site scope (`ff05::`): it stays
/// inside the local site by default and never crosses onto the public
/// internet.
pub const MULTICAST_GROUP_V6: Ipv6Addr = Ipv6Addr::new(0xff05, 0, 0, 0, 0, 0, 0x0255, 0x0001);

/// IPv6 link-local all-nodes address — the counterpart of [`BROADCAST_ADDR`].
/// Every IPv6 interface already belongs to this group, so a beacon sent here
/// reaches the whole link without any per-host configuration.
pub const BROADCAST_ADDR_V6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);

/// Fixed beacon wire size: magic(4) + version(1) + kind(1) + port(2) +
/// timestamp(8) + public key(32) + signature(64).
pub const BEACON_SIZE: usize = 112;

/// Bytes covered by the beacon signature (`magic` ..= `public key`).
pub(super) const BEACON_SIGNED_LEN: usize = 48;

/// Default announce interval.
pub const BEACON_INTERVAL: Duration = Duration::from_secs(30);

/// Default freshness window for beacon timestamps (both directions).
pub const BEACON_MAX_AGE: Duration = Duration::from_secs(120);

/// How long a known peer's address is sticky: within this window after the
/// last sighting, a beacon claiming a *new* address for the same key
/// refreshes `last_seen` only. Prevents replayed beacons from redirecting
/// traffic to an attacker-controlled socket. Mirrors `NodeEngine`'s
/// 5-minute stale-peer retention.
pub const ADDR_REBIND_GRACE: Duration = Duration::from_secs(300);

pub(super) const BEACON_MAGIC: [u8; 4] = *b"PWBD";
pub(super) const BEACON_VERSION: u8 = 1;
pub(super) const BEACON_KIND_ANNOUNCE: u8 = 0;
