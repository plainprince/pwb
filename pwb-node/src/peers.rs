//! Peer memory across restarts.
//!
//! A node that only knows bootstrap peers cannot rejoin a swarm whose
//! rendezvous point is offline, and a node behind a NAT cannot be re-found by
//! LAN discovery. The node therefore remembers the peers it has authenticated
//! and re-dials the most recent ones on startup, keeping only those that answer
//! a liveness probe.
//!
//! What restarts inherit matters for eclipse attacks: `peers.json` is ordered
//! by recency, and the peers that talked to this node just before it stopped
//! are exactly the ones an active attacker controls. [`select_for_restore`]
//! therefore spends most of the dial budget on recent peers but caps how many
//! slots any one address prefix may take, and reserves part of the budget for
//! the *oldest* remembered peers, so a table flooded moments before shutdown
//! cannot occupy every slot on the next start.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use pwb_core::crypto::PublicKey;
use pwb_core::net::NodeEngine;

use crate::persist::{self, StoredPeer};

/// How many remembered peers a restart dials in parallel.
pub const DIAL_LIMIT: usize = 32;

/// Dial slots reserved for the least recently seen remembered peers. Recency
/// is attacker-controlled (send a lot just before the target restarts), so a
/// minority of the budget goes to peers that have been in the table the
/// longest instead.
pub const OLD_PEER_RESERVE: usize = 8;

/// Most remembered peers dialed from a single address prefix — IPv4 /24 or
/// IPv6 /64 (v4-mapped addresses count as their IPv4 prefix). One hostile
/// subnet must not fill the restart dial list.
pub const MAX_PER_PREFIX: usize = 6;

/// How long each remembered-peer liveness probe waits.
pub const DIAL_TIMEOUT: Duration = Duration::from_millis(1500);

/// Address prefix used for the diversity cap: an IPv4 /24, or an IPv6 /64,
/// with v4-mapped IPv6 treated as IPv4.
fn prefix_key(addr: &SocketAddr) -> Vec<u8> {
    let ip = addr.ip();
    let v4 = match ip {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(v6) => v6.to_ipv4_mapped(),
    };
    if let Some(v4) = v4 {
        let octets = v4.octets();
        return vec![4, octets[0], octets[1], octets[2]];
    }
    let IpAddr::V6(v6) = ip else {
        return Vec::new();
    };
    let mut key = vec![6u8];
    key.extend_from_slice(&v6.octets()[..8]);
    key
}

/// Pick the remembered peers a restart should re-dial.
///
/// `stored` is newest-seen first (as [`persist::load_peers`] returns it).
/// Most of the budget follows recency, every address prefix is capped at
/// [`MAX_PER_PREFIX`], and [`OLD_PEER_RESERVE`] slots go to the oldest
/// entries so a table recency-flooded just before shutdown still restores
/// peers the attacker did not just add.
pub fn select_for_restore(stored: &[StoredPeer]) -> Vec<&StoredPeer> {
    let recent_budget = DIAL_LIMIT.saturating_sub(OLD_PEER_RESERVE);
    let mut chosen: Vec<&StoredPeer> = Vec::new();
    let mut seen_keys: HashSet<PublicKey> = HashSet::new();
    let mut per_prefix: std::collections::HashMap<Vec<u8>, usize> =
        std::collections::HashMap::new();

    // Pass 1: most recent first, up to the recent budget.
    for peer in stored.iter() {
        if chosen.len() >= recent_budget {
            break;
        }
        if seen_keys.contains(&peer.public_key) {
            continue;
        }
        let key = prefix_key(&peer.address);
        let used = per_prefix.entry(key).or_insert(0);
        if *used >= MAX_PER_PREFIX {
            continue;
        }
        *used += 1;
        seen_keys.insert(peer.public_key);
        chosen.push(peer);
    }

    // Pass 2: the oldest remembered peers fill the reserved slots, skipping
    // anything already chosen above.
    for peer in stored.iter().rev() {
        if chosen.len() >= DIAL_LIMIT {
            break;
        }
        if seen_keys.contains(&peer.public_key) {
            continue;
        }
        let key = prefix_key(&peer.address);
        let used = per_prefix.entry(key).or_insert(0);
        if *used >= MAX_PER_PREFIX {
            continue;
        }
        *used += 1;
        seen_keys.insert(peer.public_key);
        chosen.push(peer);
    }

    chosen
}

/// Snapshot the live peer table in the on-disk form, with wall-clock times.
pub fn snapshot(engine: &NodeEngine, local: &PublicKey) -> Vec<StoredPeer> {
    let now = std::time::Instant::now();
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    engine
        .peer_table()
        .lock()
        .unwrap()
        .values()
        .filter(|peer| peer.public_key != *local)
        .map(|peer| StoredPeer {
            public_key: peer.public_key,
            address: peer.address,
            last_seen_unix: now_unix
                .saturating_sub(now.saturating_duration_since(peer.last_seen).as_secs()),
        })
        .collect()
}

/// Live peers first, then previously remembered peers that are not live right
/// now: an unreachable address today may be back tomorrow, so it is not erased
/// just because the node restarted while it was offline.
pub fn merged(engine: &NodeEngine, local: &PublicKey, path: &Path) -> Vec<StoredPeer> {
    let mut peers = snapshot(engine, local);
    for remembered in persist::load_peers(path) {
        if !peers
            .iter()
            .any(|peer| peer.public_key == remembered.public_key)
        {
            peers.push(remembered);
        }
    }
    peers
}

/// Re-dial the most recently seen remembered peers in parallel, keeping only
/// those that answer a liveness probe. Failed probes are removed from the peer
/// table so they do not linger as supposedly-live entries. Returns how many
/// peers reconnected.
pub async fn restore(engine: &NodeEngine, path: &Path) -> usize {
    let stored = persist::load_peers(path);
    let mut dials = tokio::task::JoinSet::new();
    for peer in select_for_restore(&stored) {
        let engine = engine.clone();
        let address = peer.address;
        let public_key = peer.public_key;
        dials.spawn(async move {
            let ok = engine
                .dial_resilient(address, public_key, DIAL_TIMEOUT)
                .await
                .is_ok();
            if !ok {
                engine.remove_peer(&public_key);
            }
            ok
        });
    }
    let mut connected = 0;
    while let Some(result) = dials.join_next().await {
        if result.unwrap_or(false) {
            connected += 1;
        }
    }
    connected
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::crypto::Keypair;

    fn peer(seed: u8, address: &str, last_seen_unix: u64) -> StoredPeer {
        StoredPeer {
            public_key: Keypair::from_seed(&[seed; 32]).public,
            address: address.parse().unwrap(),
            last_seen_unix,
        }
    }

    /// Newest first, as [`persist::load_peers`] orders the store.
    fn store(peers: Vec<StoredPeer>) -> Vec<StoredPeer> {
        let mut peers = peers;
        peers.sort_unstable_by_key(|peer| std::cmp::Reverse(peer.last_seen_unix));
        peers
    }

    #[test]
    fn one_subnet_cannot_fill_the_restart_dial_list() {
        let mut stored = Vec::new();
        // 40 distinct keys behind one /24, newest first…
        for seed in 0..40u8 {
            stored.push(peer(seed, "203.0.113.9:7921", 10_000 + u64::from(seed)));
        }
        // …plus older peers from other prefixes.
        for (seed, ip) in [
            (200u8, "198.51.100.1"),
            (201, "192.0.2.7"),
            (202, "[2001:db8::1]"),
        ] {
            stored.push(peer(seed, &format!("{ip}:7921"), 1));
        }
        let stored = store(stored);
        let selected = select_for_restore(&stored);

        let hostile = selected
            .iter()
            .filter(|peer| peer.address.ip().to_string() == "203.0.113.9")
            .count();
        assert_eq!(
            hostile, MAX_PER_PREFIX,
            "one prefix is capped at MAX_PER_PREFIX slots"
        );
        assert!(
            selected.len() >= MAX_PER_PREFIX + 3,
            "other prefixes still fill the list (got {})",
            selected.len()
        );
        // No key is dialed twice.
        let unique: HashSet<PublicKey> = selected.iter().map(|peer| peer.public_key).collect();
        assert_eq!(unique.len(), selected.len());
    }

    #[test]
    fn the_oldest_remembered_peers_keep_a_share_of_the_budget() {
        let mut stored = Vec::new();
        // A table flooded with fresh attacker entries…
        for seed in 0..30u8 {
            stored.push(peer(seed, &format!("203.0.113.{}:7921", seed), 50_000 + u64::from(seed)));
        }
        // …on top of a few long-standing legitimate peers.
        for seed in 100..104u8 {
            stored.push(peer(seed, "198.51.100.4:7921", 100));
        }
        let stored = store(stored);
        let selected = select_for_restore(&stored);

        let old_kept = selected.iter().filter(|peer| peer.last_seen_unix < 1_000).count();
        assert!(
            old_kept >= 4,
            "the reserved slots keep the long-standing peers (got {old_kept})"
        );
        assert!(selected.len() <= DIAL_LIMIT);
    }
}

/// Periodically write the merged peer store so a restart can rejoin without a
/// bootstrap. Existing remembered peers survive a temporarily empty table.
pub async fn persist_loop(
    engine: NodeEngine,
    path: std::path::PathBuf,
    interval: Duration,
    local: PublicKey,
) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let peers = merged(&engine, &local, &path);
        if let Err(error) = persist::save_peers(&path, &peers) {
            eprintln!("[Network] Could not persist peer store: {error}");
        }
    }
}
