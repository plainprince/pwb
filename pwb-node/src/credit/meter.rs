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

/// Outstanding requester challenges (nonce → issue time).
#[derive(Debug, Clone)]
pub(super) struct ChallengeLedger {
    pub(super) outstanding: HashMap<[u8; 32], Instant>,
    pub(super) ttl: Duration,
}

impl ChallengeLedger {
    pub(super) fn check(&self, nonce: &[u8; 32], now: Instant) -> Result<(), MintError> {
        let issued = self
            .outstanding
            .get(nonce)
            .ok_or(MintError::UnknownChallenge)?;
        if now.saturating_duration_since(*issued) >= self.ttl {
            return Err(MintError::ChallengeExpired);
        }
        Ok(())
    }

    pub(super) fn claim(&mut self, nonce: &[u8; 32]) {
        self.outstanding.remove(nonce);
    }

    pub(super) fn issue(&mut self, nonce: [u8; 32], now: Instant) {
        let ttl = self.ttl;
        self.outstanding
            .retain(|_, issued| now.saturating_duration_since(*issued) < ttl);
        self.outstanding.insert(nonce, now);
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
