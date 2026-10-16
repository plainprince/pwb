//! Proof-of-time & useful-work credit minting (`pwb-node::credit`).
//!
//! Credits are minted **only** against verified network services — never for
//! raw computation (TODO §4.1):
//!
//! - **Serving `search.plain` queries** — a [`QueryReceipt`] must close a
//!   challenge/response round-trip with a real requester: the server signs the
//!   query, its result set, and a nonce the *requester* issued (with TTL, one
//!   claim per nonce), so receipts cannot be fabricated without a live
//!   counterparty.
//! - **Indexing PML bundles** — an [`IndexReceipt`] is content-addressed: the
//!   bundle's BLAKE3 CID and a canonical hash of its term set must both
//!   re-verify against the actual bytes, so the indexer demonstrably parsed
//!   real data.
//! - **Relaying onion cells** — [`CreditMinter::relay_cell`] folds every cell
//!   into a hash chain; [`CreditMinter::seal_relay_window`] seals a window's
//!   cell count/bytes/chain head into a signed [`RelayReceipt`] that
//!   `mint_relay` re-checks against the sealed state (window-continuous,
//!   claim-once).
//!
//! Minting is **rate-limited by real network traffic flow**, not compute:
//! every credit consumes `MintPolicy::traffic_per_credit` bytes of *verified*
//! exchange traffic recorded on the [`TrafficMeter`] — an ASIC farm that
//! fabricates service proofs faster than it moves bytes across the network
//! hits the floor. Per-service cooldowns spread work over time (proof of
//! time), and a per-epoch cap bounds absolute issuance.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use pwb_core::crypto::{ContentId, Keypair, PublicKey, Signature};
use pwb_core::wire::Bundle;

use crate::search;

/// Domain-separation prefix for every signed credit message.
const CREDIT_SIGN_PREFIX: &[u8] = b"PWBCREDIT1";

/// KiB divisor for relay work accounting.
const KIB: u64 = 1024;

/// The three verified services that mint credits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    /// Serving a `search.plain` query to a requester (challenge round-trip).
    ServeSearchQuery,
    /// Indexing a content-addressed PML bundle.
    IndexBundle,
    /// Relaying onion cells through this node.
    RelayOnionCell,
}

/// Amounts, floors, and windows for [`CreditMinter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintPolicy {
    /// Credits per verified query receipt.
    pub query_credits: u64,
    /// Credits per verified bundle index.
    pub index_credits: u64,
    /// Credits per whole KiB (floor) of sealed relayed cells.
    pub relay_credits_per_kib: u64,
    /// Verified traffic bytes required per credit minted (the anti-ASIC
    /// floor: issuance rate is bounded by network flow, not compute).
    pub traffic_per_credit: u64,
    /// Minimum spacing between mints of the *same* service kind (proof of
    /// time: services must be spread over real time, not burst-computed).
    pub per_service_cooldown: Duration,
    /// Length of one issuance epoch.
    pub epoch: Duration,
    /// Maximum credits mints within one epoch.
    pub epoch_cap: u64,
    /// Lifetime of a requester challenge before it expires unclaimed.
    pub challenge_ttl: Duration,
}

impl Default for MintPolicy {
    fn default() -> Self {
        MintPolicy {
            query_credits: 5,
            index_credits: 10,
            relay_credits_per_kib: 1,
            traffic_per_credit: 1024,
            per_service_cooldown: Duration::from_secs(1),
            epoch: Duration::from_secs(3600),
            epoch_cap: 10_000,
            challenge_ttl: Duration::from_secs(300),
        }
    }
}

/// Why a mint was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintError {
    /// The receipt signature does not verify.
    BadSignature,
    /// The challenge was never issued or was already claimed (replay).
    UnknownChallenge,
    /// The challenge outlived `MintPolicy::challenge_ttl`.
    ChallengeExpired,
    /// The bundle bytes do not hash to the receipt's content id.
    ContentMismatch,
    /// The bundle bytes do not deserialize as a PML bundle.
    BundleUnreadable,
    /// The receipt's term hash does not match the bundle's actual terms.
    TermMismatch,
    /// The receipt's signer is not this minter's owner identity.
    WrongSigner,
    /// The relay receipt does not match the sealed cell chain.
    RelayChainBroken,
    /// `mint_relay` called with no sealed window awaiting claim.
    NoSealedRelayWork,
    /// The per-service cooldown has not elapsed.
    Cooldown { retry_after: Duration },
    /// Verified traffic covers fewer bytes than the mint requires.
    RateLimited {
        needed_bytes: u64,
        available_bytes: u64,
    },
    /// The epoch's issuance cap is exhausted.
    EpochCapReached,
}

impl fmt::Display for MintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MintError::BadSignature => write!(f, "service receipt signature is invalid"),
            MintError::UnknownChallenge => {
                write!(f, "requester challenge is unknown or already claimed")
            }
            MintError::ChallengeExpired => write!(f, "requester challenge expired"),
            MintError::ContentMismatch => {
                write!(f, "bundle bytes do not hash to the receipt content id")
            }
            MintError::BundleUnreadable => write!(f, "bundle bytes do not deserialize"),
            MintError::TermMismatch => {
                write!(f, "receipt term hash does not match the bundle's terms")
            }
            MintError::WrongSigner => {
                write!(f, "receipt was not signed by this node's identity")
            }
            MintError::RelayChainBroken => {
                write!(f, "relay receipt does not match the sealed cell chain")
            }
            MintError::NoSealedRelayWork => {
                write!(f, "no sealed relay window is awaiting mint")
            }
            MintError::Cooldown { retry_after } => {
                write!(f, "service cooldown active, retry in {retry_after:?}")
            }
            MintError::RateLimited {
                needed_bytes,
                available_bytes,
            } => write!(
                f,
                "verified traffic covers {available_bytes} of {needed_bytes} required bytes"
            ),
            MintError::EpochCapReached => write!(f, "epoch credit cap reached"),
        }
    }
}

impl std::error::Error for MintError {}

/// Signed proof that a `search.plain` query was served to a live requester.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryReceipt {
    /// The serving node (the account that will mint).
    pub server: PublicKey,
    /// The query that was answered.
    pub query: String,
    /// Content ids returned to the requester.
    pub results: Vec<ContentId>,
    /// Requester-issued challenge nonce binding this receipt to a real
    /// round-trip (one claim per nonce).
    pub challenge: [u8; 32],
    /// Server signature over the canonical encoding of the fields above.
    pub signature: Signature,
}

impl QueryReceipt {
    /// Sign a receipt for a served query.
    pub fn sign(
        server: &Keypair,
        query: &str,
        results: Vec<ContentId>,
        challenge: [u8; 32],
    ) -> Self {
        let message = query_sign_message(server.public, query, &results, &challenge);
        QueryReceipt {
            server: server.public,
            query: query.to_string(),
            results,
            challenge,
            signature: server.sign(&message),
        }
    }

    /// Verify the server signature over the canonical encoding.
    pub fn verify(&self) -> bool {
        let message = query_sign_message(self.server, &self.query, &self.results, &self.challenge);
        self.signature.verify(&self.server, &message)
    }

    /// Evidence digest binding this receipt into an attestor ledger chain.
    pub fn evidence_digest(&self) -> [u8; 32] {
        let message = query_sign_message(self.server, &self.query, &self.results, &self.challenge);
        ContentId::compute(&message).0
    }
}

/// Signed proof that a node indexed the exact bundle bytes it claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexReceipt {
    /// The indexing node (the account that will mint).
    pub indexer: PublicKey,
    /// BLAKE3 content id of the indexed bundle bytes.
    pub content_id: ContentId,
    /// Canonical hash of the bundle's term set (see [`canonical_term_hash`]).
    pub term_hash: ContentId,
    /// Indexer signature over the canonical encoding of the fields above.
    pub signature: Signature,
}

impl IndexReceipt {
    /// Sign a receipt over the exact serialized bytes of a bundle. Returns
    /// `None` when the bytes do not deserialize (no terms to hash).
    pub fn sign(indexer: &Keypair, bundle_bytes: &[u8]) -> Option<Self> {
        let bundle = Bundle::deserialize(bundle_bytes).ok()?;
        let content_id = ContentId::compute(bundle_bytes);
        let term_hash = canonical_term_hash(&bundle);
        let message = index_sign_message(indexer.public, content_id, term_hash);
        Some(IndexReceipt {
            indexer: indexer.public,
            content_id,
            term_hash,
            signature: indexer.sign(&message),
        })
    }
}

/// Signed seal over one window of relayed onion cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayReceipt {
    /// The relaying node (the account that will mint).
    pub relayer: PublicKey,
    /// Window index (monotone, starts at 0).
    pub window: u64,
    /// Cells relayed in the window.
    pub cells: u64,
    /// Cell bytes relayed in the window.
    pub bytes: u64,
    /// Rolling hash-chain head over the window's cells.
    pub chain_head: [u8; 32],
    /// Relayer signature over the canonical encoding of the fields above.
    pub signature: Signature,
}

impl RelayReceipt {
    /// Sign a window seal.
    pub fn sign(
        relayer: &Keypair,
        window: u64,
        cells: u64,
        bytes: u64,
        chain_head: [u8; 32],
    ) -> Self {
        let message = relay_sign_message(relayer.public, window, cells, bytes, chain_head);
        RelayReceipt {
            relayer: relayer.public,
            window,
            cells,
            bytes,
            chain_head,
            signature: relayer.sign(&message),
        }
    }

    /// Verify the relayer signature over the canonical encoding.
    pub fn verify(&self) -> bool {
        let message = relay_sign_message(
            self.relayer,
            self.window,
            self.cells,
            self.bytes,
            self.chain_head,
        );
        self.signature.verify(&self.relayer, &message)
    }

    /// Evidence digest binding this receipt into an attestor ledger chain.
    pub fn evidence_digest(&self) -> [u8; 32] {
        let message = relay_sign_message(
            self.relayer,
            self.window,
            self.cells,
            self.bytes,
            self.chain_head,
        );
        ContentId::compute(&message).0
    }
}

/// Canonical hash of a bundle's term set: every distinct extracted term
/// (lowercased, alphanumerics only — the same extractor the search index
/// uses), sorted and joined. Re-derivable by any verifier from the bytes.
pub fn canonical_term_hash(bundle: &Bundle) -> ContentId {
    let mut counts = HashMap::new();
    search::extract_terms(&bundle.root_node, &mut counts);
    let mut terms: Vec<&str> = counts.keys().map(String::as_str).collect();
    terms.sort_unstable();
    ContentId::compute(terms.join("\n").as_bytes())
}

fn query_sign_message(
    server: PublicKey,
    query: &str,
    results: &[ContentId],
    challenge: &[u8; 32],
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(CREDIT_SIGN_PREFIX);
    msg.extend_from_slice(b"query");
    msg.extend_from_slice(&server.0);
    msg.extend_from_slice(&(query.len() as u64).to_le_bytes());
    msg.extend_from_slice(query.as_bytes());
    msg.extend_from_slice(&(results.len() as u64).to_le_bytes());
    for cid in results {
        msg.extend_from_slice(&cid.0);
    }
    msg.extend_from_slice(challenge);
    msg
}

pub(crate) fn index_sign_message(
    indexer: PublicKey,
    content_id: ContentId,
    term_hash: ContentId,
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(CREDIT_SIGN_PREFIX);
    msg.extend_from_slice(b"index");
    msg.extend_from_slice(&indexer.0);
    msg.extend_from_slice(&content_id.0);
    msg.extend_from_slice(&term_hash.0);
    msg
}

fn relay_sign_message(
    relayer: PublicKey,
    window: u64,
    cells: u64,
    bytes: u64,
    chain_head: [u8; 32],
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(CREDIT_SIGN_PREFIX);
    msg.extend_from_slice(b"relay");
    msg.extend_from_slice(&relayer.0);
    msg.extend_from_slice(&window.to_le_bytes());
    msg.extend_from_slice(&cells.to_le_bytes());
    msg.extend_from_slice(&bytes.to_le_bytes());
    msg.extend_from_slice(&chain_head);
    msg
}

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
struct ChallengeLedger {
    outstanding: HashMap<[u8; 32], Instant>,
    ttl: Duration,
}

impl ChallengeLedger {
    fn check(&self, nonce: &[u8; 32], now: Instant) -> Result<(), MintError> {
        let issued = self
            .outstanding
            .get(nonce)
            .ok_or(MintError::UnknownChallenge)?;
        if now.saturating_duration_since(*issued) >= self.ttl {
            return Err(MintError::ChallengeExpired);
        }
        Ok(())
    }

    fn claim(&mut self, nonce: &[u8; 32]) {
        self.outstanding.remove(nonce);
    }

    fn issue(&mut self, nonce: [u8; 32], now: Instant) {
        let ttl = self.ttl;
        self.outstanding
            .retain(|_, issued| now.saturating_duration_since(*issued) < ttl);
        self.outstanding.insert(nonce, now);
    }
}

/// Live relay accounting plus the last sealed (unclaimed) window.
#[derive(Debug, Clone, Default)]
struct RelayLedger {
    /// Rolling chain over every cell ever relayed (continues across windows).
    live_chain: [u8; 32],
    live_cells: u64,
    live_bytes: u64,
    next_window: u64,
    /// Last sealed window awaiting `mint_relay` (claim-once).
    sealed: Option<SealedRelay>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SealedRelay {
    window: u64,
    cells: u64,
    bytes: u64,
    chain_head: [u8; 32],
}

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

    /// Issue a requester challenge nonce (TTL-bound, one claim). The caller
    /// supplies the random nonce — the requester side owns its entropy.
    pub fn issue_challenge(&mut self, nonce: [u8; 32], now: Instant) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::spec::{PlainNode, Tag};
    use std::collections::HashMap;

    fn test_bundle(text: &str) -> Bundle {
        let mut root = PlainNode::new(0, Tag::View);
        let mut title = PlainNode::new(0, Tag::Title);
        title.text_content = Some("PlainWeb Demo".to_string());
        let mut body = PlainNode::new(0, Tag::Text);
        body.text_content = Some(text.to_string());
        root.children.push(title);
        root.children.push(body);
        Bundle {
            version: pwb_core::wire::CURRENT_VERSION,
            title: "PlainWeb Demo".to_string(),
            author_pubkey: None,
            root_node: root,
            script_payload: None,
            components: HashMap::new(),
            modules: HashMap::new(),
        }
    }

    fn minter_with(owner: &Keypair, policy: MintPolicy) -> CreditMinter {
        CreditMinter::new(owner.public, policy)
    }

    fn no_cooldown_policy() -> MintPolicy {
        MintPolicy {
            per_service_cooldown: Duration::ZERO,
            ..MintPolicy::default()
        }
    }

    #[test]
    fn test_query_mint_happy_path_and_replay_rejection() {
        let server = Keypair::generate();
        let mut minter = minter_with(&server, no_cooldown_policy());
        let t0 = Instant::now();

        let challenge = [7u8; 32];
        minter.issue_challenge(challenge, t0);
        minter.record_traffic(8 * 1024);

        let results = vec![
            ContentId::compute(b"bundle-a"),
            ContentId::compute(b"bundle-b"),
        ];
        let receipt = QueryReceipt::sign(&server, "plainweb", results, challenge);
        assert_eq!(
            minter.mint_query(&receipt, t0),
            Ok(MintPolicy::default().query_credits)
        );
        assert_eq!(minter.total_earned(), 5);
        // Traffic was consumed at the floor rate.
        assert_eq!(minter.available_traffic(), 8 * 1024 - 5 * 1024);

        // The challenge was claimed: the same receipt cannot mint twice.
        assert_eq!(
            minter.mint_query(&receipt, t0),
            Err(MintError::UnknownChallenge)
        );
    }

    #[test]
    fn test_query_receipt_rejects_bad_signature() {
        let server = Keypair::generate();
        let mut minter = minter_with(&server, no_cooldown_policy());
        let t0 = Instant::now();
        let challenge = [9u8; 32];
        minter.issue_challenge(challenge, t0);
        minter.record_traffic(10 * 1024);

        let mut receipt = QueryReceipt::sign(&server, "plainweb", vec![], challenge);
        receipt.query = "plainweb2".to_string(); // tampered after signing
        assert_eq!(
            minter.mint_query(&receipt, t0),
            Err(MintError::BadSignature)
        );

        // A receipt signed by a different key cannot mint this minter's
        // credits — the owner identity is bound at construction.
        let impostor = Keypair::generate();
        let forged = QueryReceipt::sign(&impostor, "plainweb", vec![], challenge);
        assert_eq!(minter.mint_query(&forged, t0), Err(MintError::WrongSigner));
    }

    #[test]
    fn test_challenge_unknown_and_expired() {
        let server = Keypair::generate();
        let mut minter = minter_with(
            &server,
            MintPolicy {
                challenge_ttl: Duration::from_secs(60),
                ..no_cooldown_policy()
            },
        );
        let t0 = Instant::now();
        minter.record_traffic(10 * 1024);

        // Never issued.
        let unknown = QueryReceipt::sign(&server, "q", vec![], [1u8; 32]);
        assert_eq!(
            minter.mint_query(&unknown, t0),
            Err(MintError::UnknownChallenge)
        );

        // Issued but expired.
        let challenge = [2u8; 32];
        minter.issue_challenge(challenge, t0);
        let expired = QueryReceipt::sign(&server, "q", vec![], challenge);
        assert_eq!(
            minter.mint_query(&expired, t0 + Duration::from_secs(61)),
            Err(MintError::ChallengeExpired)
        );

        // Re-issuing refreshes the same nonce.
        minter.issue_challenge(challenge, t0 + Duration::from_secs(100));
        assert_eq!(
            minter.mint_query(&expired, t0 + Duration::from_secs(101)),
            Ok(5)
        );
    }

    #[test]
    fn test_index_mint_verifies_content_and_terms() {
        let indexer = Keypair::generate();
        let mut minter = minter_with(&indexer, no_cooldown_policy());
        let t0 = Instant::now();

        let bytes = test_bundle("serving search indexing relaying").serialize();
        let receipt = IndexReceipt::sign(&indexer, &bytes).expect("receipt");
        assert_eq!(receipt.content_id, ContentId::compute(&bytes));

        // Honest path: record the verified download exchange, then mint.
        minter.record_traffic(bytes.len() as u64 + 16 * 1024);
        assert_eq!(
            minter.mint_index(&receipt, &bytes, t0),
            Ok(MintPolicy::default().index_credits)
        );
        assert_eq!(minter.total_earned(), 10);
    }

    #[test]
    fn test_index_rejects_content_mismatch() {
        let indexer = Keypair::generate();
        let mut minter = minter_with(&indexer, no_cooldown_policy());
        let t0 = Instant::now();
        minter.record_traffic(16 * 1024);

        let a = test_bundle("alpha").serialize();
        let b = test_bundle("beta").serialize();
        let receipt = IndexReceipt::sign(&indexer, &a).expect("receipt");
        assert_eq!(
            minter.mint_index(&receipt, &b, t0),
            Err(MintError::ContentMismatch)
        );
    }

    #[test]
    fn test_index_rejects_term_mismatch() {
        let indexer = Keypair::generate();
        let mut minter = minter_with(&indexer, no_cooldown_policy());
        let t0 = Instant::now();
        minter.record_traffic(16 * 1024);

        let a = test_bundle("alpha terms here").serialize();
        let other = test_bundle("completely different words");
        // A well-formed signature over a content id of A but the term hash
        // of a different bundle: signature and CID pass, terms must not.
        let message = index_sign_message(
            indexer.public,
            ContentId::compute(&a),
            canonical_term_hash(&other),
        );
        let receipt = IndexReceipt {
            indexer: indexer.public,
            content_id: ContentId::compute(&a),
            term_hash: canonical_term_hash(&other),
            signature: indexer.sign(&message),
        };
        assert_eq!(
            minter.mint_index(&receipt, &a, t0),
            Err(MintError::TermMismatch)
        );
    }

    #[test]
    fn test_index_rejects_unreadable_bundle() {
        let indexer = Keypair::generate();
        let mut minter = minter_with(&indexer, no_cooldown_policy());
        let t0 = Instant::now();
        minter.record_traffic(16 * 1024);

        let garbage = b"not a bundle at all";
        let message = index_sign_message(
            indexer.public,
            ContentId::compute(garbage),
            ContentId::compute(b"terms"),
        );
        let receipt = IndexReceipt {
            indexer: indexer.public,
            content_id: ContentId::compute(garbage),
            term_hash: ContentId::compute(b"terms"),
            signature: indexer.sign(&message),
        };
        assert_eq!(
            minter.mint_index(&receipt, garbage, t0),
            Err(MintError::BundleUnreadable)
        );
    }

    #[test]
    fn test_relay_mint_happy_path_and_replay_rejection() {
        let relayer = Keypair::generate();
        let mut minter = minter_with(&relayer, no_cooldown_policy());
        let t0 = Instant::now();

        // Three 1000-byte cells: 3000 bytes → floor 2 KiB × 1 credit = 2.
        for i in 0..3u8 {
            minter.relay_cell(&[i; 1000]);
        }
        assert_eq!(minter.traffic().total(), 3000);

        let receipt = minter.seal_relay_window(&relayer).expect("sealed");
        assert_eq!(receipt.window, 0);
        assert_eq!(receipt.cells, 3);
        assert_eq!(receipt.bytes, 3000);

        assert_eq!(minter.mint_relay(&receipt, t0), Ok(2));
        assert_eq!(minter.total_earned(), 2);
        // Claim-once: the same receipt cannot mint again.
        assert_eq!(
            minter.mint_relay(&receipt, t0),
            Err(MintError::NoSealedRelayWork)
        );
    }

    #[test]
    fn test_relay_chain_continuity_across_windows() {
        let relayer = Keypair::generate();
        let mut minter = minter_with(&relayer, no_cooldown_policy());
        let t0 = Instant::now();

        minter.relay_cell(&[1u8; 2048]);
        let first = minter.seal_relay_window(&relayer).expect("first");
        assert_eq!(minter.mint_relay(&first, t0), Ok(2));

        // The live chain continues into the next window.
        minter.relay_cell(&[2u8; 2048]);
        let second = minter.seal_relay_window(&relayer).expect("second");
        assert_eq!(
            minter.mint_relay(&second, t0 + Duration::from_secs(1)),
            Ok(2)
        );

        assert_eq!(first.window, 0);
        assert_eq!(second.window, 1);
        assert_ne!(first.chain_head, second.chain_head, "chain advances");
    }

    #[test]
    fn test_relay_rejects_tampered_receipt_then_accepts_original() {
        let relayer = Keypair::generate();
        let mut minter = minter_with(&relayer, no_cooldown_policy());
        let t0 = Instant::now();

        minter.relay_cell(&[9u8; 1024]);
        let receipt = minter.seal_relay_window(&relayer).expect("sealed");

        // Post-signature tampering breaks the signature itself.
        let mut tampered = receipt.clone();
        tampered.cells += 1;
        assert_eq!(
            minter.mint_relay(&tampered, t0),
            Err(MintError::BadSignature)
        );

        // A well-signed receipt for a window the minter never sealed is a
        // chain break — the sealed window survives to pay the real claim.
        let wrong_window = RelayReceipt::sign(
            &relayer,
            99,
            receipt.cells,
            receipt.bytes,
            receipt.chain_head,
        );
        assert_eq!(
            minter.mint_relay(&wrong_window, t0),
            Err(MintError::RelayChainBroken)
        );
        assert_eq!(minter.mint_relay(&receipt, t0), Ok(1));
    }

    #[test]
    fn test_relay_seal_refuses_empty_and_double_seal() {
        let relayer = Keypair::generate();
        let mut minter = minter_with(&relayer, no_cooldown_policy());

        // No cells → nothing to seal.
        assert!(minter.seal_relay_window(&relayer).is_none());

        minter.relay_cell(&[1u8; 100]);
        assert!(minter.seal_relay_window(&relayer).is_some());
        // Previous seal is still unclaimed → refuse to overwrite it.
        minter.relay_cell(&[2u8; 100]);
        assert!(minter.seal_relay_window(&relayer).is_none());
    }

    #[test]
    fn test_cooldown_spreads_mints_over_time() {
        let server = Keypair::generate();
        let mut minter = minter_with(
            &server,
            MintPolicy {
                per_service_cooldown: Duration::from_secs(5),
                ..MintPolicy::default()
            },
        );
        let t0 = Instant::now();
        minter.record_traffic(100 * 1024);

        let c1 = [1u8; 32];
        minter.issue_challenge(c1, t0);
        let r1 = QueryReceipt::sign(&server, "q1", vec![], c1);
        assert_eq!(minter.mint_query(&r1, t0), Ok(5));

        // Same kind inside the cooldown → refused with the remaining wait.
        let c2 = [2u8; 32];
        minter.issue_challenge(c2, t0);
        let r2 = QueryReceipt::sign(&server, "q2", vec![], c2);
        assert_eq!(
            minter.mint_query(&r2, t0 + Duration::from_secs(1)),
            Err(MintError::Cooldown {
                retry_after: Duration::from_secs(4)
            })
        );

        // Once the cooldown elapses the second query pays.
        assert_eq!(minter.mint_query(&r2, t0 + Duration::from_secs(5)), Ok(5));

        // A different service kind is not gated by the query cooldown.
        let bytes = test_bundle("cooldown isolation").serialize();
        let receipt = IndexReceipt::sign(&server, &bytes).expect("receipt");
        assert_eq!(
            minter.mint_index(&receipt, &bytes, t0 + Duration::from_secs(1)),
            Ok(10)
        );
    }

    #[test]
    fn test_traffic_floor_rate_limits_mints() {
        let server = Keypair::generate();
        let mut minter = minter_with(
            &server,
            MintPolicy {
                traffic_per_credit: 100,
                ..no_cooldown_policy()
            },
        );
        let t0 = Instant::now();

        // 300 bytes < 5 credits × 100 bytes needed.
        minter.record_traffic(300);
        let c1 = [1u8; 32];
        minter.issue_challenge(c1, t0);
        let r1 = QueryReceipt::sign(&server, "q1", vec![], c1);
        assert_eq!(
            minter.mint_query(&r1, t0),
            Err(MintError::RateLimited {
                needed_bytes: 500,
                available_bytes: 300
            })
        );

        // With enough traffic the same receipt mints; the floor is consumed.
        minter.record_traffic(300); // total 600
        assert_eq!(minter.mint_query(&r1, t0), Ok(5));
        assert_eq!(minter.available_traffic(), 100);

        // The next mint needs fresh traffic: issuance tracks network flow.
        let c2 = [2u8; 32];
        minter.issue_challenge(c2, t0);
        let r2 = QueryReceipt::sign(&server, "q2", vec![], c2);
        assert_eq!(
            minter.mint_query(&r2, t0),
            Err(MintError::RateLimited {
                needed_bytes: 500,
                available_bytes: 100
            })
        );
    }

    #[test]
    fn test_epoch_cap_and_rollover() {
        let server = Keypair::generate();
        let mut minter = minter_with(
            &server,
            MintPolicy {
                epoch_cap: 8,
                epoch: Duration::from_secs(60),
                ..no_cooldown_policy()
            },
        );
        let t0 = Instant::now();
        minter.record_traffic(1024 * 1024);

        let c1 = [1u8; 32];
        minter.issue_challenge(c1, t0);
        let r1 = QueryReceipt::sign(&server, "q1", vec![], c1);
        assert_eq!(minter.mint_query(&r1, t0), Ok(5)); // 5/8 used

        let c2 = [2u8; 32];
        minter.issue_challenge(c2, t0);
        let r2 = QueryReceipt::sign(&server, "q2", vec![], c2);
        assert_eq!(
            minter.mint_query(&r2, t0),
            Err(MintError::EpochCapReached) // 5+5 > 8
        );

        // Rollover opens a fresh epoch window.
        let t1 = t0 + Duration::from_secs(61);
        minter.issue_challenge(c2, t1);
        assert_eq!(minter.mint_query(&r2, t1), Ok(5));
    }

    #[test]
    fn test_anti_asic_traffic_invariant_across_services() {
        // After any sequence of mints, credits × traffic floor never exceeds
        // the verified traffic actually recorded — the core §4.1 property.
        let node = Keypair::generate();
        let mut minter = minter_with(
            &node,
            MintPolicy {
                traffic_per_credit: 64,
                ..no_cooldown_policy()
            },
        );
        let t0 = Instant::now();

        minter.record_traffic(4096);
        let c1 = [1u8; 32];
        minter.issue_challenge(c1, t0);
        let query = QueryReceipt::sign(&node, "q", vec![], c1);
        minter.mint_query(&query, t0).expect("query pays");

        let bytes = test_bundle("invariant check").serialize();
        let index = IndexReceipt::sign(&node, &bytes).expect("receipt");
        minter.mint_index(&index, &bytes, t0).expect("index pays");

        minter.relay_cell(&[3u8; 512]);
        let relay = minter.seal_relay_window(&node).expect("sealed");
        minter.mint_relay(&relay, t0).expect("relay pays");

        let floor = minter.policy().traffic_per_credit;
        assert!(
            minter.total_earned() * floor <= minter.traffic().total(),
            "earned {} credits against {} bytes (floor {floor}/credit)",
            minter.total_earned(),
            minter.traffic().total()
        );
    }

    #[test]
    fn test_receipt_verify_and_evidence_digest() {
        let server = Keypair::generate();
        let query = QueryReceipt::sign(&server, "plainweb", vec![], [3u8; 32]);
        assert!(query.verify());
        assert_ne!(query.evidence_digest(), [0u8; 32]);
        let mut tampered = query.clone();
        tampered.query = "plainweb2".into();
        assert!(!tampered.verify());
        assert_ne!(tampered.evidence_digest(), query.evidence_digest());

        let relayer = Keypair::generate();
        let relay = RelayReceipt::sign(&relayer, 0, 3, 3000, [9u8; 32]);
        assert!(relay.verify());
        let mut tampered = relay;
        tampered.bytes += 1;
        assert!(!tampered.verify());
    }

    #[test]
    fn test_mint_error_display_and_error_trait() {
        let all: [MintError; 10] = [
            MintError::BadSignature,
            MintError::UnknownChallenge,
            MintError::ChallengeExpired,
            MintError::ContentMismatch,
            MintError::BundleUnreadable,
            MintError::TermMismatch,
            MintError::WrongSigner,
            MintError::RelayChainBroken,
            MintError::NoSealedRelayWork,
            MintError::Cooldown {
                retry_after: Duration::from_secs(2),
            },
        ];
        for err in all {
            let text = err.to_string();
            assert!(!text.is_empty());
            let _dyn: &dyn std::error::Error = &err;
        }
        let limited = MintError::RateLimited {
            needed_bytes: 512,
            available_bytes: 256,
        };
        assert!(limited.to_string().contains("512"));
        assert!(MintError::EpochCapReached.to_string().contains("cap"));
    }
}
