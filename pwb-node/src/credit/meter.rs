//! Verified traffic meter and internal mint ledgers.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::policy::MintError;

/// Verified network traffic counter backing the mint rate floor.
#[derive(Debug, Clone, Default)]
pub struct TrafficMeter {
    total_bytes: u64,
}

impl TrafficMeter {
    /// Count `bytes` of verified exchange traffic.
    pub fn record(&mut self, bytes: u64) {
        self.total_bytes = self.total_bytes.saturating_add(bytes);
    }

    /// Total verified bytes recorded.
    pub fn total(&self) -> u64 {
        self.total_bytes
    }
}

/// Outstanding and already-claimed requester challenges. Keeping claimed
/// nonces for their TTL is what makes a re-sent `ShardQuery` safe: a retry can
/// receive a second signed response without letting the responder mint twice
/// from the same challenge.
#[derive(Debug, Clone)]
pub(super) struct ChallengeLedger {
    pub(super) states: HashMap<[u8; 32], ChallengeState>,
    pub(super) ttl: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ChallengeState {
    Open(Instant),
    Claimed(Instant),
}

impl ChallengeState {
    fn at(&self) -> Instant {
        match self {
            ChallengeState::Open(at) | ChallengeState::Claimed(at) => *at,
        }
    }
}

impl ChallengeLedger {
    pub(super) fn check(&self, nonce: &[u8; 32], now: Instant) -> Result<(), MintError> {
        match self.states.get(nonce) {
            Some(ChallengeState::Open(issued)) => {
                if now.saturating_duration_since(*issued) >= self.ttl {
                    return Err(MintError::ChallengeExpired);
                }
                Ok(())
            }
            // A claimed nonce is deliberately indistinguishable from an
            // unknown one: replaying the receipt must never mint again.
            Some(ChallengeState::Claimed(_)) | None => Err(MintError::UnknownChallenge),
        }
    }

    pub(super) fn claim(&mut self, nonce: &[u8; 32], now: Instant) {
        self.states.insert(*nonce, ChallengeState::Claimed(now));
    }

    pub(super) fn issue(&mut self, nonce: [u8; 32], now: Instant) {
        let ttl = self.ttl;
        self.states
            .retain(|_, state| now.saturating_duration_since(state.at()) < ttl);
        // Do not reopen a claimed challenge or extend an open one's TTL. A
        // retry is allowed to ask again, but not to buy a fresh mint window.
        self.states
            .entry(nonce)
            .or_insert(ChallengeState::Open(now));
    }
}

/// Live relay accounting plus the last sealed (unclaimed) window.
#[derive(Debug, Clone, Default)]
pub(super) struct RelayLedger {
    /// Rolling chain over every cell ever relayed (continues across windows).
    pub(super) live_chain: [u8; 32],
    pub(super) live_cells: u64,
    pub(super) live_bytes: u64,
    pub(super) next_window: u64,
    /// Last sealed window awaiting `mint_relay` (claim-once).
    pub(super) sealed: Option<SealedRelay>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SealedRelay {
    pub(super) window: u64,
    pub(super) cells: u64,
    pub(super) bytes: u64,
    pub(super) chain_head: [u8; 32],
}

/// Live rendezvous (seed) accounting plus the last sealed (unclaimed) window.
#[derive(Debug, Clone, Default)]
pub(super) struct RendezvousLedger {
    /// Rolling chain over every rendezvous event ever served (continues across
    /// windows).
    pub(super) live_chain: [u8; 32],
    pub(super) live_joins: u64,
    pub(super) live_bytes: u64,
    pub(super) next_window: u64,
    /// Last sealed window awaiting `mint_rendezvous` (claim-once).
    pub(super) sealed: Option<SealedRendezvous>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SealedRendezvous {
    pub(super) window: u64,
    pub(super) joins: u64,
    pub(super) bytes: u64,
    pub(super) chain_head: [u8; 32],
}
