//! Peer memory across restarts.
//!
//! A node that only knows bootstrap peers cannot rejoin a swarm whose
//! rendezvous point is offline, and a node behind a NAT cannot be re-found by
//! LAN discovery. The node therefore remembers the peers it has authenticated
//! and re-dials the most recent ones on startup, keeping only those that answer
//! a liveness probe.

use std::path::Path;
use std::time::Duration;

use pwb_core::crypto::PublicKey;
use pwb_core::net::NodeEngine;

use crate::persist::{self, StoredPeer};

/// How many remembered peers a restart dials in parallel.
pub const DIAL_LIMIT: usize = 32;

/// How long each remembered-peer liveness probe waits.
pub const DIAL_TIMEOUT: Duration = Duration::from_millis(1500);

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
    for peer in stored.into_iter().take(DIAL_LIMIT) {
        let engine = engine.clone();
        dials.spawn(async move {
            let ok = engine
                .dial_resilient(peer.address, peer.public_key, DIAL_TIMEOUT)
                .await
                .is_ok();
            if !ok {
                engine.remove_peer(&peer.public_key);
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
