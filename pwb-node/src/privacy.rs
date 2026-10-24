//! Onion path selection for private browser/node operations.
//!
//! Paths are built only from *directly dialable* peers in the engine's peer
//! table, because every hop must be addressed individually. Selection is
//! randomized with a cryptographic RNG and rejects duplicate hops. A caller
//! that must reach a specific exit (for example the shard node responsible for
//! a search term) can pin the last hop; the entry and middle hops are still
//! random.
//!
//! This module deliberately does not implement guard nodes or a persisted guard
//! set yet. Random entry selection is weaker against a global passive adversary,
//! but it removes the old deterministic "first three peers by node id" choice,
//! which let any peer predict every circuit.

use std::collections::HashSet;

use pwb_core::crypto::{DhPublicKey, PublicKey};
use pwb_core::dht::NodeId;
use pwb_core::net::NodeEngine;
use rand::seq::SliceRandom;
use rand::Rng;

/// How a browser or node performs a remote lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyMode {
    /// Route requests through three-hop onion circuits. The default for the
    /// browser; never silently falls back to direct traffic.
    Onion,
    /// Use the ordinary authenticated transport. Faster, but peers see the
    /// requester's address.
    Direct,
}

/// Minimum hops accepted by the onion layer.
pub const MIN_ONION_HOPS: usize = 3;
/// Maximum hops accepted by the onion layer.
pub const MAX_ONION_HOPS: usize = 6;

/// Why a private path could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivacyError {
    /// The requested hop count is outside the onion protocol's range.
    BadHopCount(usize),
    /// Fewer direct peers are available than the path needs.
    NotEnoughPeers { needed: usize, available: usize },
    /// A pinned exit is not a direct peer.
    ExitNotConnected(String),
    /// A peer key could not be converted into an onion key.
    InvalidRelayKey(String),
    /// Every circuit was built but no exit produced a usable answer.
    RequestFailed(String),
}

impl std::fmt::Display for PrivacyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivacyError::BadHopCount(hops) => write!(
                f,
                "onion paths need {MIN_ONION_HOPS}..={MAX_ONION_HOPS} hops, got {hops}"
            ),
            PrivacyError::NotEnoughPeers { needed, available } => write!(
                f,
                "private routing needs {needed} direct peers, have {available}"
            ),
            PrivacyError::ExitNotConnected(key) => {
                write!(f, "requested onion exit {key} is not a direct peer")
            }
            PrivacyError::InvalidRelayKey(key) => {
                write!(f, "peer {key} does not have a valid onion key")
            }
            PrivacyError::RequestFailed(detail) => {
                write!(f, "private request failed: {detail}")
            }
        }
    }
}

impl std::error::Error for PrivacyError {}

/// Every direct peer in the live peer table, excluding this node.
pub fn direct_peers(engine: &NodeEngine) -> Vec<PublicKey> {
    let local = engine.public_key();
    engine
        .peer_table()
        .lock()
        .unwrap()
        .keys()
        .copied()
        .filter(|key| *key != local)
        .collect()
}

/// Build a random circuit of `hops` distinct direct peers.
///
/// When `exit` is set it must be a direct peer and becomes the last hop; the
/// remaining hops are chosen randomly from the other peers. When it is `None`,
/// every hop (including the exit) is random.
pub fn select_path(
    engine: &NodeEngine,
    exit: Option<PublicKey>,
    hops: usize,
) -> Result<Vec<DhPublicKey>, PrivacyError> {
    if !(MIN_ONION_HOPS..=MAX_ONION_HOPS).contains(&hops) {
        return Err(PrivacyError::BadHopCount(hops));
    }

    let peers = direct_peers(engine);
    let mut rng = rand::thread_rng();
    select_path_with(engine, &peers, exit, hops, &mut rng)
}

/// Testable core of [`select_path`]. `peers` must already exclude this node.
fn select_path_with<R: Rng + ?Sized>(
    engine: &NodeEngine,
    peers: &[PublicKey],
    exit: Option<PublicKey>,
    hops: usize,
    rng: &mut R,
) -> Result<Vec<DhPublicKey>, PrivacyError> {
    if !(MIN_ONION_HOPS..=MAX_ONION_HOPS).contains(&hops) {
        return Err(PrivacyError::BadHopCount(hops));
    }
    let mut candidates: Vec<PublicKey> = peers.to_vec();
    candidates.sort_unstable_by_key(NodeId::from_public_key);
    candidates.dedup();
    let local = engine.public_key();
    candidates.retain(|key| *key != local);
    if candidates.len() < hops {
        return Err(PrivacyError::NotEnoughPeers {
            needed: hops,
            available: candidates.len(),
        });
    }

    let mut selected: Vec<PublicKey> = Vec::with_capacity(hops);
    if let Some(exit) = exit {
        if !candidates.contains(&exit) {
            return Err(PrivacyError::ExitNotConnected(exit.to_hex()));
        }
        candidates.retain(|key| *key != exit);
        candidates.shuffle(rng);
        selected.extend(candidates.into_iter().take(hops - 1));
        selected.push(exit);
    } else {
        candidates.shuffle(rng);
        selected.extend(candidates.into_iter().take(hops));
    }

    let mut seen = HashSet::with_capacity(selected.len());
    let mut path = Vec::with_capacity(selected.len());
    for key in selected {
        if !seen.insert(key) {
            continue;
        }
        let dh = DhPublicKey::from_ed25519(&key)
            .ok_or_else(|| PrivacyError::InvalidRelayKey(key.to_hex()))?;
        path.push(dh);
    }
    if path.len() != hops {
        return Err(PrivacyError::NotEnoughPeers {
            needed: hops,
            available: path.len(),
        });
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::crypto::Keypair;
    use pwb_core::net::NodeConfig;
    use std::time::Duration;

    fn engine() -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: Keypair::generate(),
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    #[test]
    fn selects_distinct_exit_pinned_hops_from_direct_peers() {
        let observer = engine();
        let peers: Vec<PublicKey> = (0..8).map(|_| Keypair::generate().public).collect();
        let exit = peers[4];
        let mut rng = rand::thread_rng();
        let path = select_path_with(&observer, &peers, Some(exit), 3, &mut rng)
            .expect("enough peers for a circuit");
        assert_eq!(path.len(), 3);
        assert_eq!(
            path.last(),
            Some(&DhPublicKey::from_ed25519(&exit).unwrap()),
            "the pinned exit is last"
        );
        let unique: HashSet<_> = path.iter().collect();
        assert_eq!(unique.len(), 3, "no relay is repeated");
    }

    #[test]
    fn rejects_too_few_peers_and_disconnected_exits() {
        let observer = engine();
        let peers: Vec<PublicKey> = (0..2).map(|_| Keypair::generate().public).collect();
        let mut rng = rand::thread_rng();
        assert_eq!(
            select_path_with(&observer, &peers, None, 3, &mut rng),
            Err(PrivacyError::NotEnoughPeers {
                needed: 3,
                available: 2
            })
        );

        let peers: Vec<PublicKey> = (0..4).map(|_| Keypair::generate().public).collect();
        let absent = Keypair::generate().public;
        assert!(matches!(
            select_path_with(&observer, &peers, Some(absent), 3, &mut rng),
            Err(PrivacyError::ExitNotConnected(_))
        ));
    }

    #[test]
    fn hop_count_is_bounded() {
        let observer = engine();
        let peers: Vec<PublicKey> = (0..8).map(|_| Keypair::generate().public).collect();
        let mut rng = rand::thread_rng();
        assert_eq!(
            select_path_with(&observer, &peers, None, 2, &mut rng),
            Err(PrivacyError::BadHopCount(2))
        );
        assert_eq!(
            select_path_with(&observer, &peers, None, 7, &mut rng),
            Err(PrivacyError::BadHopCount(7))
        );
    }
}
