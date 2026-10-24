//! The credit minter: verified services under policy rate limits.

use std::collections::HashMap;
use std::time::Instant;

use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_core::wire::Bundle;

use super::meter::{ChallengeLedger, RelayLedger, SealedRelay, TrafficMeter};
use super::policy::{MintError, MintPolicy, ServiceKind};
use super::receipts::{canonical_term_hash, IndexReceipt, QueryReceipt, RelayReceipt};
use super::signing::{index_sign_message, query_sign_message, relay_sign_message};

/// KiB divisor for relay work accounting.
const KIB: u64 = 1024;

/// Mints credits for verified services under [`MintPolicy`] rate limits.
#[derive(Debug, Clone)]
pub struct CreditMinter {
    policy: MintPolicy,
    /// The one identity allowed to mint through this minter.
    owner: PublicKey,
    traffic: TrafficMeter,
    /// Verified traffic bytes already converted into credits.
    credited_bytes: u64,
    total_earned: u64,
    epoch_start: Instant,
    epoch_minted: u64,
    last_mint: HashMap<ServiceKind, Instant>,
    challenges: ChallengeLedger,
    relay: RelayLedger,
}

impl CreditMinter {
    /// Minter owned by one node identity (only that identity's receipts
    /// mint) with an explicit policy.
    pub fn new(owner: PublicKey, policy: MintPolicy) -> Self {
        CreditMinter {
            owner,
            challenges: ChallengeLedger {
                outstanding: HashMap::new(),
                ttl: policy.challenge_ttl,
            },
            policy,
            traffic: TrafficMeter::default(),
            credited_bytes: 0,
            total_earned: 0,
            epoch_start: Instant::now(),
            epoch_minted: 0,
            last_mint: HashMap::new(),
            relay: RelayLedger::default(),
        }
    }

    /// Minter with [`MintPolicy::default`].
    pub fn with_default_policy(owner: PublicKey) -> Self {
        Self::new(owner, MintPolicy::default())
    }

    /// The governing policy.
    pub fn policy(&self) -> &MintPolicy {
        &self.policy
    }

    /// Total verified bytes recorded on the traffic meter.
    pub fn traffic(&self) -> &TrafficMeter {
        &self.traffic
    }

    /// Verified traffic bytes not yet converted into credits (the current
    /// anti-ASIC headroom).
    pub fn available_traffic(&self) -> u64 {
        self.traffic.total().saturating_sub(self.credited_bytes)
    }

    /// Total credits minted over the minter's lifetime.
    pub fn total_earned(&self) -> u64 {
        self.total_earned
    }

    /// Count `bytes` of verified exchange traffic (downloads, served
    /// responses, relayed cells) backing future mints.
    pub fn record_traffic(&mut self, bytes: u64) {
        self.traffic.record(bytes);
    }

    /// Record a challenge nonce that arrived in a signed requester message
    /// (`ShardQuery`). The *requester* owns the nonce and its entropy; the
    /// serving node only remembers it, TTL-bound and claim-once, so it can
    /// verify the receipt it signs against a real round-trip. A node cannot
    /// mint by inventing nonces for itself: `mint_query` only pays for a
    /// challenge that a peer sent.
    pub fn record_challenge(&mut self, nonce: [u8; 32], now: Instant) {
        self.challenges.issue(nonce, now);
    }

    /// Mint for a served `search.plain` query: the receipt must carry an
    /// outstanding requester challenge and a valid server signature.
    pub fn mint_query(&mut self, receipt: &QueryReceipt, now: Instant) -> Result<u64, MintError> {
        if receipt.server != self.owner {
            return Err(MintError::WrongSigner);
        }
        let message = query_sign_message(
            receipt.server,
            &receipt.query,
            &receipt.results,
            &receipt.challenge,
        );
        if !receipt.signature.verify(&receipt.server, &message) {
            return Err(MintError::BadSignature);
        }
        self.challenges.check(&receipt.challenge, now)?;
        let amount = self.authorize(
            ServiceKind::ServeSearchQuery,
            self.policy.query_credits,
            now,
        )?;
        self.challenges.claim(&receipt.challenge);
        Ok(amount)
    }

    /// Mint for indexing a bundle: the actual bytes must hash to the receipt
    /// content id and re-derive its term hash.
    pub fn mint_index(
        &mut self,
        receipt: &IndexReceipt,
        bundle_bytes: &[u8],
        now: Instant,
    ) -> Result<u64, MintError> {
        if receipt.indexer != self.owner {
            return Err(MintError::WrongSigner);
        }
        let message = index_sign_message(receipt.indexer, receipt.content_id, receipt.term_hash);
        if !receipt.signature.verify(&receipt.indexer, &message) {
            return Err(MintError::BadSignature);
        }
        if ContentId::compute(bundle_bytes) != receipt.content_id {
            return Err(MintError::ContentMismatch);
        }
        let bundle = Bundle::deserialize(bundle_bytes).map_err(|_| MintError::BundleUnreadable)?;
        if canonical_term_hash(&bundle) != receipt.term_hash {
            return Err(MintError::TermMismatch);
        }
        self.authorize(ServiceKind::IndexBundle, self.policy.index_credits, now)
    }

    /// Relay one onion cell: folds it into the live hash chain and records
    /// its bytes as verified traffic.
    pub fn relay_cell(&mut self, cell: &[u8]) {
        let digest = ContentId::compute(cell);
        let mut fold = [0u8; 64];
        fold[..32].copy_from_slice(&self.relay.live_chain);
        fold[32..].copy_from_slice(&digest.0);
        self.relay.live_chain = ContentId::compute(&fold).0;
        self.relay.live_cells = self.relay.live_cells.saturating_add(1);
        self.relay.live_bytes = self.relay.live_bytes.saturating_add(cell.len() as u64);
        self.traffic.record(cell.len() as u64);
    }

    /// Bytes relayed in the current unsealed window (relay credits are based
    /// on whole KiB, so callers wait for at least 1024 bytes before sealing).
    pub fn pending_relay_bytes(&self) -> u64 {
        self.relay.live_bytes
    }

    /// Cells relayed in the current unsealed window.
    pub fn pending_relay_cells(&self) -> u64 {
        self.relay.live_cells
    }

    /// Seal the current relay window into a signed receipt. Returns `None`
    /// when no cells are pending or the previous seal is still unclaimed.
    pub fn seal_relay_window(&mut self, relayer: &Keypair) -> Option<RelayReceipt> {
        if self.relay.sealed.is_some() || self.relay.live_cells == 0 {
            return None;
        }
        let receipt = RelayReceipt::sign(
            relayer,
            self.relay.next_window,
            self.relay.live_cells,
            self.relay.live_bytes,
            self.relay.live_chain,
        );
        self.relay.sealed = Some(SealedRelay {
            window: self.relay.next_window,
            cells: self.relay.live_cells,
            bytes: self.relay.live_bytes,
            chain_head: self.relay.live_chain,
        });
        self.relay.next_window = self.relay.next_window.saturating_add(1);
        self.relay.live_cells = 0;
        self.relay.live_bytes = 0;
        Some(receipt)
    }

    /// Mint for a sealed relay window: the receipt must signature-verify and
    /// exactly match the sealed window (window, cells, bytes, chain head).
    pub fn mint_relay(&mut self, receipt: &RelayReceipt, now: Instant) -> Result<u64, MintError> {
        if receipt.relayer != self.owner {
            return Err(MintError::WrongSigner);
        }
        let message = relay_sign_message(
            receipt.relayer,
            receipt.window,
            receipt.cells,
            receipt.bytes,
            receipt.chain_head,
        );
        if !receipt.signature.verify(&receipt.relayer, &message) {
            return Err(MintError::BadSignature);
        }
        let sealed = self
            .relay
            .sealed
            .as_ref()
            .ok_or(MintError::NoSealedRelayWork)?;
        let matches = receipt.window == sealed.window
            && receipt.cells == sealed.cells
            && receipt.bytes == sealed.bytes
            && receipt.chain_head == sealed.chain_head;
        if !matches {
            return Err(MintError::RelayChainBroken);
        }
        // Whole-KiB floor: relayed bytes back relay credits 1:1 under the
        // default policy (never more credits than bytes moved).
        let amount = receipt
            .bytes
            .saturating_mul(self.policy.relay_credits_per_kib)
            / KIB;
        let amount = self.authorize(ServiceKind::RelayOnionCell, amount, now)?;
        self.relay.sealed = None;
        Ok(amount)
    }

    /// Shared issuance gate: epoch rollover, per-service cooldown, traffic
    /// floor, and epoch cap — then account for the mint.
    fn authorize(
        &mut self,
        kind: ServiceKind,
        amount: u64,
        now: Instant,
    ) -> Result<u64, MintError> {
        self.roll_epoch(now);
        if let Some(last) = self.last_mint.get(&kind) {
            let waited = now.saturating_duration_since(*last);
            if waited < self.policy.per_service_cooldown {
                return Err(MintError::Cooldown {
                    retry_after: self.policy.per_service_cooldown - waited,
                });
            }
        }
        let needed = amount.saturating_mul(self.policy.traffic_per_credit);
        let available = self.available_traffic();
        if needed > available {
            return Err(MintError::RateLimited {
                needed_bytes: needed,
                available_bytes: available,
            });
        }
        if self.epoch_minted.saturating_add(amount) > self.policy.epoch_cap {
            return Err(MintError::EpochCapReached);
        }
        self.credited_bytes = self.credited_bytes.saturating_add(needed);
        self.epoch_minted = self.epoch_minted.saturating_add(amount);
        self.total_earned = self.total_earned.saturating_add(amount);
        self.last_mint.insert(kind, now);
        Ok(amount)
    }

    fn roll_epoch(&mut self, now: Instant) {
        if now.saturating_duration_since(self.epoch_start) >= self.policy.epoch {
            self.epoch_start = now;
            self.epoch_minted = 0;
        }
    }
}
