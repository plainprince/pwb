//! Networked `register.plain` service (`pwb-node::domain::service`).
//!
//! Turns the domain registry into a real, decentralized namespace:
//!
//! 1. **Signed claims.** [`DomainService::claim`] signs a canonical
//!    `domain -> content-id` statement with the owner key and broadcasts a
//!    [`NetworkMessage::ClaimDomain`]. Every receiver re-derives the statement,
//!    verifies the signature, enforces the reserved/duplicate rules and the
//!    minimum price, applies it with the registry's last-writer-wins merge, and
//!    re-gossips it to its own peers (flood with per-claim dedup, so the
//!    propagation terminates).
//! 2. **Registry sync.** On startup a node broadcasts
//!    [`NetworkMessage::RegistrySyncRequest`]; peers answer with their full
//!    record set ([`NetworkMessage::RegistrySyncResponse`]) which merges in, so
//!    a late joiner converges quickly without replaying every historical claim.
//! 3. **Remote resolution.** [`DomainService::resolve`] answers a `.pwb` name it
//!    does not hold locally by asking the swarm
//!    ([`NetworkMessage::DomainQuery`] / [`NetworkMessage::DomainResponse`]).
//! 4. **Persistence.** Records are mirrored to disk and reloaded on start.
//!
//! ## Trust model
//!
//! A claim is trusted when it is signed by the owner and declares at least the
//! current rolling price. Plain claims enforce a local spend gate; attested
//! claims and funding citations carried by registry sync are checked against a
//! `k`-of-`N` quorum certificate (see `crate::ledger`). The quorum is the
//! deterministic live-peer neighborhood by default, or an operator-pinned
//! anchor set when one is configured (`new_with_trust`), so a bootstrap peer
//! cannot unilaterally invent funding. Receivers reject underpaid claims
//! (`credits_spent < claim_cost`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_core::discovery::unix_time;
use pwb_core::net::{
    domain_claim_funding_sign_message, domain_claim_sign_message, BlindClaimWire, DomainRecordWire,
    NetworkMessage, NodeEngine, QuorumCertificateWire, SyncRecordWire,
};
use pwb_core::system::{is_reserved_system_domain, is_unofficial_domain};
use rand::seq::SliceRandom;
use rand::RngCore;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::pricing::PricingEpochStats;
use super::registry::{ClaimFunding, DomainRecord, DomainRegistry};
use crate::blind::{claim_from_wire, claim_to_wire, BlindClaimError};
use crate::credit::{CreditLedger, LedgerError};
use crate::host::ArtifactHost;
use crate::ledger::{
    select_attestors, LedgerError as AttestationError, QuorumCertificate, DEFAULT_ATTESTOR_N,
    DEFAULT_QUORUM_K,
};
use crate::privacy::{self, PrivacyError};

/// How long a remote `.pwb` name resolution waits for a peer to answer.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_millis(600);

/// How long a private (onion-routed) name resolution waits for one exit. A
/// three-hop circuit adds two network legs plus cell encryption, so the direct
/// 600 ms budget is too tight.
pub const PRIVATE_RESOLVE_TIMEOUT: Duration = Duration::from_secs(4);

/// Claims may not be stamped further than this into the future, so a bad
/// clock (or a malicious claimant) cannot win every future merge forever.
pub const MAX_CLAIM_EPOCH_SKEW: u64 = 300;

/// Upper bound on remembered claim digests. Flood dedup only needs recent
/// history (gossip terminates in seconds); the cap stops a long-lived node
/// from growing memory for every claim it ever saw.
pub const MAX_SEEN_CLAIMS: usize = 10_000;

/// Upper bound on cached funding certificates. Only certificates that
/// verified against the local attestor policy are cached, but a long-lived
/// node can still see many funded owners; serving a sync without a cached
/// certificate is safe (the receiver refuses unfunded citations), so dropping
/// the excess is preferable to unbounded memory.
pub const MAX_FUNDING_CERTIFICATES: usize = 4096;

/// Bounded FIFO dedup set for claim digests.
///
/// `insert` returns `false` when the digest was already present (the caller
/// must drop the duplicate gossip), `true` when it is newly recorded. Once
/// the cap is reached the oldest digest is evicted, so memory stays bounded
/// while recent floods still terminate.
#[derive(Debug)]
pub(crate) struct ClaimDedup {
    set: HashSet<ContentId>,
    order: VecDeque<ContentId>,
    cap: usize,
}

impl ClaimDedup {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    pub(crate) fn insert(&mut self, digest: ContentId) -> bool {
        if !self.set.insert(digest) {
            return false;
        }
        self.order.push_back(digest);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.set.len()
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, digest: &ContentId) -> bool {
        self.set.contains(digest)
    }
}

/// Fields of an inbound signed claim, grouped so the verification path has a
/// single, readable argument.
struct ClaimFields {
    domain: String,
    target_content_id: ContentId,
    owner: PublicKey,
    credits_spent: u64,
    registered_at_epoch: u64,
    signature: pwb_core::crypto::Signature,
}

/// Why a local claim was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// The name is empty.
    EmptyDomain,
    /// The name falls under the reserved official `.plain` namespace.
    ReservedDomain,
    /// The name carries no TLD from the unofficial namespace.
    UnknownTld,
    /// The name is already registered (locally or via gossip).
    AlreadyRegistered,
    /// The owner's local credit balance is below the current price.
    InsufficientCredits { balance: u64, cost: u64 },
    /// The signed credit ledger could not be written.
    Persist(String),
    /// A balance certificate did not verify or could not fund the claim.
    Attestation(String),
    /// A blind voucher claim was refused.
    Blind(String),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::EmptyDomain => write!(f, "enter a domain name"),
            ClaimError::ReservedDomain => {
                write!(f, "'.plain' is reserved for official built-in pages")
            }
            ClaimError::UnknownTld => write!(f, "{}", pwb_core::system::unknown_tld_message()),
            ClaimError::AlreadyRegistered => write!(f, "that domain is already registered"),
            ClaimError::InsufficientCredits { balance, cost } => {
                write!(f, "need {cost} credits, have {balance}")
            }
            ClaimError::Persist(e) => write!(f, "cannot persist credits: {e}"),
            ClaimError::Attestation(e) => write!(f, "balance attestation failed: {e}"),
            ClaimError::Blind(e) => write!(f, "blind voucher claim failed: {e}"),
        }
    }
}

/// A node's `register.plain` service: signed claim gossip + registry sync +
/// remote resolution, driven over a [`NodeEngine`].
pub struct DomainService {
    engine: NodeEngine,
    registry: Arc<Mutex<DomainRegistry>>,
    keypair: Keypair,
    /// Signed credit history: domain claims debit it, verified services credit
    /// it. The balance is derived, never read from an editable field.
    ledger: Arc<CreditLedger>,
    persist_path: Option<PathBuf>,
    /// Canonical-claim digests already applied, so gossip flooding terminates.
    /// Bounded LRU so a long-lived node cannot grow memory without bound.
    seen_claims: Mutex<ClaimDedup>,
    /// In-flight remote resolutions, keyed by query id.
    pending_queries: Mutex<HashMap<u64, mpsc::UnboundedSender<Option<DomainRecord>>>>,
    next_query_id: AtomicU64,
    /// Quorum certificates that back attested records, keyed by
    /// `(owner, funding head)`. Registry sync attaches them so a late joiner
    /// can verify a funding citation instead of trusting the sender.
    funding_certificates: Mutex<HashMap<(PublicKey, [u8; 32]), QuorumCertificateWire>>,
    /// Operator-pinned attestors. When non-empty, funding certificates must
    /// be signed by `min(DEFAULT_QUORUM_K, len)` of these keys instead of the
    /// receiver's live peer table, so a fresh node whose only peer is
    /// malicious cannot be lied to during bootstrap.
    trust_anchors: Vec<PublicKey>,
}

impl DomainService {
    /// Create a service over `registry`, signing claims with `keypair`.
    ///
    /// `seed` is only used when no signed credit ledger exists yet (it becomes
    /// a signed `seed` entry, so tests can start funded); a reloaded ledger
    /// always wins. When `persist_path` is set, the ledger lives beside the
    /// domain records as `credits.json`, and records are mirrored to disk.
    pub fn new(
        engine: NodeEngine,
        registry: Arc<Mutex<DomainRegistry>>,
        keypair: Keypair,
        seed: u64,
        persist_path: Option<PathBuf>,
    ) -> Self {
        Self::new_with_trust(engine, registry, keypair, seed, persist_path, Vec::new())
    }

    /// [`Self::new`] with an operator-pinned attestor set.
    ///
    /// A node started with anchors does not derive its funding quorum from
    /// the live peer table. It requires `min(DEFAULT_QUORUM_K, anchors.len())`
    /// distinct anchor signatures for every attested claim or synced funding
    /// citation. Pinning fewer than five keys is allowed (small deployments),
    /// but the threshold is then all of them. Pinning the attestor identities
    /// is a deployment decision: those keys should be operated independently
    /// of the seed node and published out of band with its connection link.
    pub fn new_with_trust(
        engine: NodeEngine,
        registry: Arc<Mutex<DomainRegistry>>,
        keypair: Keypair,
        seed: u64,
        persist_path: Option<PathBuf>,
        mut trust_anchors: Vec<PublicKey>,
    ) -> Self {
        trust_anchors.sort_unstable_by_key(|key| key.0);
        trust_anchors.dedup();
        let ledger_path = persist_path
            .as_ref()
            .map(|path| path.with_file_name("credits.json"));
        let service = Self {
            engine,
            registry,
            ledger: Arc::new(CreditLedger::new(keypair.clone(), ledger_path, seed)),
            keypair,
            persist_path,
            seen_claims: Mutex::new(ClaimDedup::new(MAX_SEEN_CLAIMS)),
            pending_queries: Mutex::new(HashMap::new()),
            next_query_id: AtomicU64::new(1),
            funding_certificates: Mutex::new(HashMap::new()),
            trust_anchors,
        };
        service.load();
        service
    }

    /// The pinned attestor identities, sorted and deduplicated.
    pub fn trust_anchors(&self) -> &[PublicKey] {
        &self.trust_anchors
    }

    /// The active pinned quorum as `(required, total)`, or `None` when the
    /// node verifies funding against its live peer table instead.
    pub fn trust_quorum(&self) -> Option<(usize, usize)> {
        if self.trust_anchors.is_empty() {
            return None;
        }
        Some((self.required_anchor_quorum(), self.trust_anchors.len()))
    }

    fn required_anchor_quorum(&self) -> usize {
        DEFAULT_QUORUM_K.min(self.trust_anchors.len()).max(1)
    }

    /// The signed credit ledger shared with the federated search service, so
    /// query/index earnings are spendable on domain claims.
    pub fn ledger(&self) -> &Arc<CreditLedger> {
        &self.ledger
    }

    /// This node's public key (the claim owner / identity).
    pub fn public_key(&self) -> PublicKey {
        self.keypair.public
    }

    /// Local spendable credit balance, derived from the signed ledger.
    pub fn available_credits(&self) -> u64 {
        self.ledger.balance()
    }

    /// Append credits earned from a verified service (`query`, `index`,
    /// `relay`) to the signed ledger.
    pub fn record_earned(&self, kind: &str, amount: u64, detail: &str) {
        self.ledger.record_earned(kind, amount, detail);
    }

    /// Current rolling claim price.
    pub fn claim_cost(&self) -> u64 {
        self.registry.lock().unwrap().claim_cost()
    }

    /// A snapshot of every known registered domain.
    pub fn records(&self) -> Vec<DomainRecord> {
        self.registry.lock().unwrap().records_snapshot()
    }

    /// The UDP address this node's engine is listening on, for
    /// `network.plain`.
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.engine.local_addr().and_then(Result::ok)
    }

    /// Where LAN discovery is bound, when discovery is enabled.
    pub fn discovery_addr(&self) -> Option<SocketAddr> {
        self.engine.discovery_addr()
    }

    /// Public address learned via STUN, when NAT traversal is configured.
    pub fn public_addr(&self) -> Option<SocketAddr> {
        self.engine.public_addr()
    }

    /// Live peers as `(public key, dialable address)` for `nodes.plain`.
    pub fn peers(&self) -> Vec<(PublicKey, SocketAddr)> {
        self.engine
            .peer_table()
            .lock()
            .unwrap()
            .values()
            .map(|peer| (peer.public_key, peer.address))
            .collect()
    }

    /// Dial a `plain://nodes.plain?connect=...` link from the `nodes.plain`
    /// connect card. Returns the verified peer after the Ping/Pong probe.
    pub async fn dial_link(&self, link: &str) -> Result<pwb_core::net::PeerInfo, String> {
        let Some((address, public_key)) = NodeEngine::parse_connection_link(link) else {
            return Err("could not parse the connection link".to_string());
        };
        self.engine
            .dial_resilient(address, public_key, Duration::from_secs(2))
            .await
            .map_err(|e| e.to_string())
    }

    /// Resolve a name from the local registry only.
    pub fn resolve_local(&self, domain: &str) -> Option<DomainRecord> {
        self.registry.lock().unwrap().resolve(domain).cloned()
    }

    /// Claim `domain -> target` for this identity: validate, debit the local
    /// balance, apply locally, then broadcast and gossip the signed claim.
    pub fn claim(&self, domain: &str, target: ContentId) -> Result<DomainRecord, ClaimError> {
        let domain = domain.trim().to_lowercase();
        if domain.is_empty() {
            return Err(ClaimError::EmptyDomain);
        }
        if is_reserved_system_domain(&domain) {
            return Err(ClaimError::ReservedDomain);
        }
        if !is_unofficial_domain(&domain) {
            return Err(ClaimError::UnknownTld);
        }
        // A name is first-come-first-served, but its owner may re-point it at
        // a new content id (for example after publishing an updated bundle).
        // Re-pointing does not buy the name again, so it is free; other
        // identities still cannot overwrite an existing claim.
        let existing = self.registry.lock().unwrap().resolve(&domain).cloned();
        let is_update = match &existing {
            Some(record) if record.owner == self.keypair.public => true,
            Some(_) => return Err(ClaimError::AlreadyRegistered),
            None => false,
        };
        let cost = if is_update { 0 } else { self.claim_cost() };
        if !is_update {
            self.ledger
                .spend("claim", cost, &domain)
                .map_err(|e| match e {
                    LedgerError::InsufficientCredits { balance, .. } => {
                        ClaimError::InsufficientCredits { balance, cost }
                    }
                    LedgerError::Persist(message) => ClaimError::Persist(message),
                })?;
        }

        let owner = self.keypair.public;
        // Claims are ordered by unix epoch, so a re-point issued in the same
        // second as the original claim must still sort after it or LWW would
        // keep serving the old target.
        let epoch = match &existing {
            Some(record) => unix_time().max(record.registered_at_epoch.saturating_add(1)),
            None => unix_time(),
        };
        let signature = self.keypair.sign(&domain_claim_sign_message(
            &domain, target, owner, cost, epoch,
        ));
        let record = DomainRecord {
            domain: domain.clone(),
            target_content_id: target,
            owner,
            credits_spent: cost,
            registered_at_epoch: epoch,
            signature: Some(signature),
            // Plain claims draw on the local signed ledger, not on an
            // attested funding head.
            funding: None,
        };
        self.registry.lock().unwrap().apply_record(record.clone());

        let message = NetworkMessage::ClaimDomain {
            domain,
            target_content_id: target,
            owner,
            credits_spent: cost,
            registered_at_epoch: epoch,
            signature,
        };
        self.seen_claims
            .lock()
            .unwrap()
            .insert(claim_digest(&message));
        let _ = self.engine.broadcast(message);
        self.persist();
        self.broadcast_pricing_epoch();
        Ok(record)
    }

    /// Claim a name against a `k`-of-`N` attested balance certificate and
    /// gossip the attested claim. The certificate is verified locally before
    /// the registry records it; `neighborhood` is the deterministic attestor
    /// set the certificate must belong to. The record cites the certificate's
    /// `(head, balance)` as its funding and the owner signature commits to
    /// that citation, so the same attested state cannot fund two surviving
    /// claims — not even across partitions.
    pub fn claim_attested(
        &self,
        domain: &str,
        target: ContentId,
        certificate: &QuorumCertificate,
        _neighborhood: &[PublicKey],
    ) -> Result<DomainRecord, ClaimError> {
        let domain = domain.trim().to_lowercase();
        if domain.is_empty() {
            return Err(ClaimError::EmptyDomain);
        }
        if is_reserved_system_domain(&domain) {
            return Err(ClaimError::ReservedDomain);
        }
        if !is_unofficial_domain(&domain) {
            return Err(ClaimError::UnknownTld);
        }
        if self.registry.lock().unwrap().contains(&domain) {
            return Err(ClaimError::AlreadyRegistered);
        }
        // The attestor set comes from the pinned anchors when configured, or
        // from the deterministic live-peer neighborhood otherwise; a claimant
        // cannot choose a small or self-controlled quorum.
        let (neighborhood, _) = self.attestor_quorum(&certificate.subject, certificate.epoch);
        self.verify_funding_certificate(certificate)?;
        if certificate.subject != self.keypair.public {
            return Err(ClaimError::Attestation(
                "certificate is for another subject".to_string(),
            ));
        }
        let cost = self.claim_cost();
        // Spendable balance is derived from the live funded records, so a
        // restarted node enforces the same budget as before the restart.
        let already = self
            .registry
            .lock()
            .unwrap()
            .claimed_by(&certificate.subject);
        if already.saturating_add(cost) > certificate.balance {
            return Err(ClaimError::Attestation(
                AttestationError::InsufficientAttestedBalance {
                    attested: certificate.balance,
                    already_claimed: already,
                    needed: cost,
                }
                .to_string(),
            ));
        }

        let funding = ClaimFunding {
            head: certificate.head,
            balance: certificate.balance,
        };
        let registered_at_epoch = unix_time().max(certificate.epoch);
        let owner = self.keypair.public;
        let signature = self.keypair.sign(&domain_claim_funding_sign_message(
            &domain,
            target,
            owner,
            cost,
            registered_at_epoch,
            &funding.head,
            funding.balance,
        ));
        let record = DomainRecord {
            domain: domain.clone(),
            target_content_id: target,
            owner,
            credits_spent: cost,
            registered_at_epoch,
            signature: Some(signature),
            funding: Some(funding),
        };
        self.registry.lock().unwrap().apply_record(record.clone());

        let message = NetworkMessage::ClaimDomainAttested {
            domain,
            target_content_id: target,
            owner,
            credits_spent: cost,
            registered_at_epoch,
            certificate: QuorumCertificateWire::from(certificate),
            neighborhood,
            funding_head: funding.head,
            funding_balance: funding.balance,
            signature,
        };
        self.seen_claims
            .lock()
            .unwrap()
            .insert(claim_digest(&message));
        let _ = self.engine.broadcast(message);
        self.persist();
        self.broadcast_pricing_epoch();
        Ok(record)
    }

    /// Claim a name privately with a blind bearer voucher. The voucher is
    /// checked against the attestor-signed issuer binding and the current
    /// rolling price; the record's owner is this node's pseudonym key.
    pub fn claim_blind(
        &self,
        domain: &str,
        target: ContentId,
        claim: &BlindClaimWire,
        _neighborhood: &[PublicKey],
    ) -> Result<DomainRecord, ClaimError> {
        let domain = domain.trim().to_lowercase();
        if domain.is_empty() {
            return Err(ClaimError::EmptyDomain);
        }
        if is_reserved_system_domain(&domain) {
            return Err(ClaimError::ReservedDomain);
        }
        if !is_unofficial_domain(&domain) {
            return Err(ClaimError::UnknownTld);
        }
        if self.registry.lock().unwrap().contains(&domain) {
            return Err(ClaimError::AlreadyRegistered);
        }
        let claim = claim_from_wire(claim)
            .ok_or_else(|| ClaimError::Blind(BlindClaimError::BadVoucher.to_string()))?;
        let (neighborhood, _) = self.attestor_quorum(&self.keypair.public, claim.binding.epoch);
        self.registry
            .lock()
            .unwrap()
            .register_blind(&domain, target, self.keypair.public, &claim, &neighborhood)
            .map_err(|error| ClaimError::Blind(error.to_string()))?;

        let cost = self.claim_cost();
        let registered_at_epoch = unix_time().max(claim.binding.epoch);
        let owner = self.keypair.public;
        let signature = self.keypair.sign(&domain_claim_sign_message(
            &domain,
            target,
            owner,
            cost,
            registered_at_epoch,
        ));
        let record = DomainRecord {
            domain: domain.clone(),
            target_content_id: target,
            owner,
            credits_spent: cost,
            registered_at_epoch,
            signature: Some(signature),
            // Blind spends are single-use by voucher token, not by
            // attested funding head.
            funding: None,
        };
        self.registry.lock().unwrap().apply_record(record.clone());

        let message = NetworkMessage::ClaimDomainBlind {
            domain,
            target_content_id: target,
            owner,
            credits_spent: cost,
            registered_at_epoch,
            claim: claim_to_wire(&claim),
            neighborhood,
            signature,
        };
        self.seen_claims
            .lock()
            .unwrap()
            .insert(claim_digest(&message));
        let _ = self.engine.broadcast(message);
        self.persist();
        self.broadcast_pricing_epoch();
        Ok(record)
    }

    /// The deterministic attestor set for `subject` at `epoch`, over the live
    /// peer identities this node knows. Used for both producing and checking
    /// certificates so a claimant cannot pick its own quorum.
    fn deterministic_neighborhood(&self, subject: &PublicKey, epoch: u64) -> Vec<PublicKey> {
        let mut peers: Vec<PublicKey> = self.peers().into_iter().map(|(key, _)| key).collect();
        peers.push(self.keypair.public);
        peers.sort_unstable_by_key(|key| key.0);
        peers.dedup();
        select_attestors(subject, epoch, &peers, DEFAULT_ATTESTOR_N)
    }

    /// The attestor set and threshold used to verify a funding certificate.
    ///
    /// Pinned anchors replace the live peer view entirely; without anchors the
    /// historical deterministic neighborhood is used. Both paths return the
    /// threshold that matches their own set, so a caller can never combine an
    /// anchor list with a peer-derived `k`.
    fn attestor_quorum(&self, subject: &PublicKey, epoch: u64) -> (Vec<PublicKey>, usize) {
        if !self.trust_anchors.is_empty() {
            return (self.trust_anchors.clone(), self.required_anchor_quorum());
        }
        let neighborhood = self.deterministic_neighborhood(subject, epoch);
        let k = DEFAULT_QUORUM_K.min(neighborhood.len()).max(1);
        (neighborhood, k)
    }

    /// Verify a balance certificate against this node's attestor policy.
    ///
    /// With pinned anchors this is the only authority; without them it is the
    /// deterministic neighborhood of live peers. Every inbound claim and
    /// registry-sync citation goes through this function.
    pub fn verify_funding_certificate(
        &self,
        certificate: &QuorumCertificate,
    ) -> Result<(), ClaimError> {
        let (neighborhood, k) = self.attestor_quorum(&certificate.subject, certificate.epoch);
        certificate
            .verify(k, &neighborhood)
            .map_err(|error| ClaimError::Attestation(error.to_string()))
    }

    /// Number of certificates cached for registry-sync replies. Test-only:
    /// production callers should not depend on cache occupancy.
    #[cfg(test)]
    pub(crate) fn cached_funding_certificate_count(&self) -> usize {
        self.funding_certificates.lock().unwrap().len()
    }

    /// Cache a *verified* funding certificate for later registry-sync replies.
    /// The cache is bounded; when full, new certificates are dropped rather
    /// than evicting a previously verified one.
    fn cache_funding_certificate(
        &self,
        owner: PublicKey,
        head: [u8; 32],
        certificate: QuorumCertificateWire,
    ) {
        let mut cache = self.funding_certificates.lock().unwrap();
        if cache.contains_key(&(owner, head)) || cache.len() < MAX_FUNDING_CERTIFICATES {
            cache.insert((owner, head), certificate);
        }
    }

    /// Broadcast this node's current pricing observation. The design calls
    /// for gossip-delivered epoch stats; sending one after each claim gives
    /// the rolling-price window real demand/work/breadth input instead of
    /// leaving every node at the initial price forever.
    fn broadcast_pricing_epoch(&self) {
        let epoch = unix_time() / 3600;
        let window_start = epoch.saturating_mul(3600);
        let registrations = self
            .records()
            .iter()
            .filter(|record| record.registered_at_epoch >= window_start)
            .count() as u64;
        let stats = PricingEpochStats {
            epoch,
            registrations,
            network_work: self.ledger.earned(),
            node_count: self.peers().len() as u64 + 1,
        };
        self.apply_pricing_stats(stats);
        let _ = self.engine.broadcast(NetworkMessage::PricingEpoch {
            sender: self.keypair.public,
            epoch: stats.epoch,
            registrations: stats.registrations,
            network_work: stats.network_work,
            node_count: stats.node_count,
        });
    }

    fn apply_pricing_stats(&self, stats: PricingEpochStats) {
        self.registry.lock().unwrap().record_pricing_epoch(stats);
        self.persist();
    }

    /// Resolve a `.pwb` name from the swarm: locally first, otherwise a
    /// broadcast [`NetworkMessage::DomainQuery`] whose first valid answer wins.
    /// Positive results are cached locally and persisted.
    pub async fn resolve(&self, domain: &str) -> Option<DomainRecord> {
        let domain = domain.trim().to_lowercase();
        if let Some(record) = self.resolve_local(&domain) {
            return Some(record);
        }
        if self.engine.peer_table().lock().unwrap().is_empty() {
            return None;
        }

        let query_id = self.next_query_id.fetch_add(1, Ordering::Relaxed);
        let mut challenge = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut challenge);
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.pending_queries.lock().unwrap().insert(query_id, tx);

        let _ = self.engine.broadcast(NetworkMessage::DomainQuery {
            domain: domain.clone(),
            query_id,
            sender: self.keypair.public,
            challenge,
        });

        let result = match tokio::time::timeout(RESOLVE_TIMEOUT, rx.recv()).await {
            Ok(Some(record)) => record,
            _ => None,
        };
        self.pending_queries.lock().unwrap().remove(&query_id);

        if let Some(record) = &result {
            if self.registry.lock().unwrap().apply_record(record.clone()) {
                self.persist();
            }
        }
        result
    }

    /// Resolve a `.pwb` name through onion circuits. Each candidate exit is
    /// asked in its own three-hop circuit, so no peer learns who is looking up
    /// which name. The first valid signed record wins; `Ok(None)` means at
    /// least one peer answered "I do not know it", while an error means no
    /// circuit could be built or no peer answered at all.
    pub async fn resolve_private(
        &self,
        domain: &str,
    ) -> Result<Option<DomainRecord>, PrivacyError> {
        let domain = domain.trim().to_lowercase();
        if let Some(record) = self.resolve_local(&domain) {
            return Ok(Some(record));
        }
        let mut peers = privacy::direct_peers(&self.engine);
        if peers.len() < 3 {
            return Err(PrivacyError::NotEnoughPeers {
                needed: 3,
                available: peers.len(),
            });
        }
        peers.shuffle(&mut rand::thread_rng());
        peers.truncate(4);

        let mut tasks = JoinSet::new();
        for exit in peers {
            let path = privacy::select_path(&self.engine, Some(exit), 3)?;
            let query_id = self.next_query_id.fetch_add(1, Ordering::Relaxed);
            let mut challenge = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut challenge);
            // The exit sees this throwaway circuit identity, not the browser's
            // long-term key. It still answers through the signed return path.
            let circuit_key = Keypair::generate();
            let message = NetworkMessage::DomainQuery {
                domain: domain.clone(),
                query_id,
                sender: circuit_key.public,
                challenge,
            };
            let engine = self.engine.clone();
            let expected_domain = domain.clone();
            tasks.spawn(async move {
                engine
                    .send_onion_request_as(&circuit_key, &path, &message, PRIVATE_RESOLVE_TIMEOUT)
                    .await
                    .ok()
                    .filter(|reply| {
                        matches!(
                            reply,
                            NetworkMessage::DomainResponse {
                                query_id: response_id,
                                domain: response_domain,
                                ..
                            } if *response_id == query_id
                                && response_domain.to_lowercase() == expected_domain
                        )
                    })
            });
        }

        let mut answered = false;
        while let Some(joined) = tasks.join_next().await {
            let Ok(Some(NetworkMessage::DomainResponse { record, .. })) = joined else {
                continue;
            };
            answered = true;
            let Some(record) = record.filter(|wire| wire.verify()) else {
                continue;
            };
            let record = DomainRecord::from(record);
            if self.registry.lock().unwrap().apply_record(record.clone()) {
                self.persist();
            }
            return Ok(Some(record));
        }
        if answered {
            Ok(None)
        } else {
            Err(PrivacyError::RequestFailed(format!(
                "no peer answered the private lookup for '{domain}'"
            )))
        }
    }

    /// Answer an onion-delivered `DomainQuery`. The originator key comes from
    /// the onion layer; a message claiming a different sender is refused.
    pub fn answer_private_domain_query(
        &self,
        message: &NetworkMessage,
        originator: PublicKey,
    ) -> Option<NetworkMessage> {
        let NetworkMessage::DomainQuery {
            domain,
            query_id,
            sender,
            ..
        } = message
        else {
            return None;
        };
        if *sender != originator {
            return None;
        }
        let record = self
            .resolve_local(domain)
            .map(DomainRecordWire::from)
            .filter(DomainRecordWire::verify);
        Some(NetworkMessage::DomainResponse {
            query_id: *query_id,
            domain: domain.clone(),
            record,
        })
    }

    /// Render the live `register.plain` claim console as serialized bundle
    /// bytes: the identity/balance summary, the claim form (optionally
    /// prefilled with `prefilled`, plus a `message`), the known domains, and
    /// `host`'s hosted artifacts as one-click content-id fillers.
    pub fn render_view(
        &self,
        host: &ArtifactHost,
        prefilled: Option<ContentId>,
        message: Option<&str>,
    ) -> Vec<u8> {
        crate::system::render_register_page_bundle(
            self.public_key(),
            self.available_credits(),
            self.claim_cost(),
            &self.records(),
            &host.artifacts(),
            prefilled,
            message,
        )
        .serialize()
    }

    /// Handle a `register.plain?claim=<domain>&cid=<hex>` submission: parse the
    /// content id, attempt the claim, and render the console with the outcome
    /// message.
    pub fn claim_and_render(&self, host: &ArtifactHost, domain: &str, cid_hex: &str) -> Vec<u8> {
        match parse_content_id(cid_hex) {
            None => self.render_view(host, None, Some("Enter a valid hex content ID.")),
            Some(cid) => match self.claim(domain, cid) {
                Ok(record) => self.render_view(
                    host,
                    Some(cid),
                    Some(&format!(
                        "Claimed {} → {}. It will propagate to peers by gossip.",
                        record.domain,
                        record.target_content_id.to_hex()
                    )),
                ),
                Err(e) => self.render_view(host, Some(cid), Some(&format!("Claim failed: {e}"))),
            },
        }
    }

    /// Run the inbound-message loop until the engine stops. On start, ask the
    /// swarm for its registry so this node converges with the current state.
    pub async fn run(self: Arc<Self>) {
        let mut rx = self.engine.subscribe();
        let _ = self.engine.broadcast(NetworkMessage::RegistrySyncRequest {
            sender: self.keypair.public,
        });
        while let Some((from, message)) = rx.recv().await {
            self.handle_message(from, message);
        }
    }

    fn handle_message(&self, from: SocketAddr, message: NetworkMessage) {
        match message {
            NetworkMessage::ClaimDomain {
                domain,
                target_content_id,
                owner,
                credits_spent,
                registered_at_epoch,
                signature,
            } => self.on_claim(
                from,
                ClaimFields {
                    domain,
                    target_content_id,
                    owner,
                    credits_spent,
                    registered_at_epoch,
                    signature,
                },
            ),

            NetworkMessage::ClaimDomainAttested {
                domain,
                target_content_id,
                owner,
                credits_spent,
                registered_at_epoch,
                certificate,
                neighborhood,
                funding_head,
                funding_balance,
                signature,
            } => self.on_attested_claim(
                from,
                domain,
                target_content_id,
                owner,
                credits_spent,
                registered_at_epoch,
                certificate,
                neighborhood,
                funding_head,
                funding_balance,
                signature,
            ),

            NetworkMessage::ClaimDomainBlind {
                domain,
                target_content_id,
                owner,
                credits_spent,
                registered_at_epoch,
                claim,
                neighborhood,
                signature,
            } => self.on_blind_claim(
                from,
                domain,
                target_content_id,
                owner,
                credits_spent,
                registered_at_epoch,
                claim,
                neighborhood,
                signature,
            ),

            NetworkMessage::RegistrySyncRequest { .. } => {
                let records = self.records_wire();
                let _ = self
                    .engine
                    .send_to_addr(from, NetworkMessage::RegistrySyncResponse { records });
            }

            NetworkMessage::RegistrySyncResponse { records } => {
                self.merge_records(records);
            }

            NetworkMessage::PricingEpoch {
                epoch,
                registrations,
                network_work,
                node_count,
                ..
            } => self.apply_pricing_stats(PricingEpochStats {
                epoch,
                registrations,
                network_work,
                node_count,
            }),

            NetworkMessage::DomainQuery {
                domain,
                query_id,
                sender,
                challenge: _,
            } => {
                // Never answer our own broadcast (the local lookup already missed).
                if sender == self.keypair.public {
                    return;
                }
                // Only serve records we can prove the owner signed; a
                // locally seeded record is not authoritative for the swarm.
                let record = self
                    .resolve_local(&domain)
                    .map(DomainRecordWire::from)
                    .filter(DomainRecordWire::verify);
                let _ = self.engine.send_to_addr(
                    from,
                    NetworkMessage::DomainResponse {
                        query_id,
                        domain,
                        record,
                    },
                );
            }

            NetworkMessage::DomainResponse {
                query_id, record, ..
            } => {
                // An unsigned or wrongly signed record is no answer at all.
                let record = record.filter(|wire| wire.verify());
                if let Some(tx) = self.pending_queries.lock().unwrap().remove(&query_id) {
                    let _ = tx.send(record.map(DomainRecord::from));
                }
            }

            _ => {}
        }
    }

    /// Verify, apply, and re-gossip an inbound claim.
    fn on_claim(&self, from: SocketAddr, claim: ClaimFields) {
        let ClaimFields {
            domain,
            target_content_id: target,
            owner,
            credits_spent,
            registered_at_epoch,
            signature,
        } = claim;
        let domain = domain.trim().to_lowercase();
        let canonical =
            domain_claim_sign_message(&domain, target, owner, credits_spent, registered_at_epoch);
        if !signature.verify(&owner, &canonical) {
            return;
        }
        if is_reserved_system_domain(&domain) || !is_unofficial_domain(&domain) {
            return;
        }
        // The owner re-pointing an existing name pays nothing; every other
        // claim must cover the current rolling price.
        let current_owner = self
            .registry
            .lock()
            .unwrap()
            .resolve(&domain)
            .map(|record| record.owner);
        let is_owner_update = current_owner == Some(owner) && credits_spent == 0;
        if !is_owner_update && credits_spent < self.claim_cost() {
            return;
        }
        if registered_at_epoch == 0
            || registered_at_epoch > unix_time().saturating_add(MAX_CLAIM_EPOCH_SKEW)
        {
            return;
        }
        if !self
            .seen_claims
            .lock()
            .unwrap()
            .insert(ContentId::compute(&canonical))
        {
            return;
        }

        let record = DomainRecord {
            domain: domain.clone(),
            target_content_id: target,
            owner,
            credits_spent,
            registered_at_epoch,
            signature: Some(signature),
            // Plain claims draw on the local signed ledger, not on an
            // attested funding head.
            funding: None,
        };
        if self.registry.lock().unwrap().apply_record(record) {
            self.persist();
        }

        // Flood to every peer except the one we heard it from.
        let message = NetworkMessage::ClaimDomain {
            domain,
            target_content_id: target,
            owner,
            credits_spent,
            registered_at_epoch,
            signature,
        };
        self.forward_claim(from, message);
    }

    /// Verify, apply, and re-gossip a balance-certificate claim.
    ///
    /// The cited funding must equal the certificate's `(head, balance)` and
    /// the owner signature must commit to that citation; a claim that spends
    /// more than the owner's live funded records leave unspent is refused,
    /// so one certificate cannot fund two claims on this node — and merges
    /// repair the same overspend across partitions.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_attested_claim(
        &self,
        from: SocketAddr,
        domain: String,
        target: ContentId,
        owner: PublicKey,
        credits_spent: u64,
        registered_at_epoch: u64,
        certificate: QuorumCertificateWire,
        _neighborhood: Vec<PublicKey>,
        funding_head: [u8; 32],
        funding_balance: u64,
        signature: pwb_core::crypto::Signature,
    ) {
        let domain = domain.trim().to_lowercase();
        if is_reserved_system_domain(&domain)
            || !is_unofficial_domain(&domain)
            || credited_claim_epoch_invalid(registered_at_epoch)
            || credits_spent < self.claim_cost()
        {
            return;
        }
        let certificate_wire = certificate;
        let certificate = QuorumCertificate::from(&certificate_wire);
        // Recompute the attestor set from the pinned anchors or live peers; the
        // hint in the message is advisory only and a claimant cannot choose
        // its quorum.
        let (neighborhood, _) = self.attestor_quorum(&owner, certificate.epoch);
        if certificate.subject != owner
            || self.verify_funding_certificate(&certificate).is_err()
            || (funding_head, funding_balance) != (certificate.head, certificate.balance)
            || credits_spent > certificate.balance
        {
            return;
        }
        // Cache only after verification so unauthenticated gossip cannot
        // poison the certificates served by registry sync.
        self.cache_funding_certificate(owner, funding_head, certificate_wire);
        let funding = ClaimFunding {
            head: funding_head,
            balance: funding_balance,
        };
        let wire = DomainRecordWire {
            domain: domain.clone(),
            target_content_id: target,
            owner,
            credits_spent,
            registered_at_epoch,
            signature: Some(signature),
            funding_head: Some(funding.head),
            funding_balance: Some(funding.balance),
        };
        // The funding-bound signature check also covers the plain claim
        // fields, so this is the only signature verification needed.
        if !wire.verify() {
            return;
        }
        let canonical =
            domain_claim_sign_message(&domain, target, owner, credits_spent, registered_at_epoch);
        if !self
            .seen_claims
            .lock()
            .unwrap()
            .insert(ContentId::compute(&canonical))
        {
            return;
        }
        let record = DomainRecord::from(wire);
        let changed = {
            let mut registry = self.registry.lock().unwrap();
            let already = registry.claimed_by(&owner);
            if already.saturating_add(credits_spent) > certificate.balance {
                return;
            }
            registry.apply_record(record)
        };
        if changed {
            self.persist();
        }
        self.forward_claim(
            from,
            NetworkMessage::ClaimDomainAttested {
                domain,
                target_content_id: target,
                owner,
                credits_spent,
                registered_at_epoch,
                certificate: (&certificate).into(),
                neighborhood,
                funding_head: funding.head,
                funding_balance: funding.balance,
                signature,
            },
        );
    }

    /// Verify, apply, and re-gossip a blind-voucher claim.
    #[allow(clippy::too_many_arguments)]
    fn on_blind_claim(
        &self,
        from: SocketAddr,
        domain: String,
        target: ContentId,
        owner: PublicKey,
        credits_spent: u64,
        registered_at_epoch: u64,
        claim: BlindClaimWire,
        _neighborhood: Vec<PublicKey>,
        signature: pwb_core::crypto::Signature,
    ) {
        let domain = domain.trim().to_lowercase();
        if is_reserved_system_domain(&domain)
            || !is_unofficial_domain(&domain)
            || credited_claim_epoch_invalid(registered_at_epoch)
            || credits_spent < self.claim_cost()
        {
            return;
        }
        let canonical =
            domain_claim_sign_message(&domain, target, owner, credits_spent, registered_at_epoch);
        if !signature.verify(&owner, &canonical) {
            return;
        }
        let Some(claim) = claim_from_wire(&claim) else {
            return;
        };
        // Recompute the attestor set for the pseudonymous owner (pinned anchors
        // when configured); a claimant cannot supply its own neighborhood and
        // self-issue vouchers through an unknown key.
        let (neighborhood, _) = self.attestor_quorum(&owner, claim.binding.epoch);
        if !neighborhood.contains(&claim.binding.attestor) {
            return;
        }
        let mut registry = self.registry.lock().unwrap();
        if registry
            .register_blind(&domain, target, owner, &claim, &neighborhood)
            .is_err()
        {
            return;
        }
        let record = DomainRecord {
            domain: domain.clone(),
            target_content_id: target,
            owner,
            credits_spent,
            registered_at_epoch,
            signature: Some(signature),
            funding: None,
        };
        registry.apply_record(record);
        drop(registry);
        // Record the digest only after the voucher validated and applied, so
        // malformed gossip cannot burn the claim's dedup slot.
        self.seen_claims
            .lock()
            .unwrap()
            .insert(ContentId::compute(&canonical));
        self.persist();
        self.forward_claim(
            from,
            NetworkMessage::ClaimDomainBlind {
                domain,
                target_content_id: target,
                owner,
                credits_spent,
                registered_at_epoch,
                claim: claim_to_wire(&claim),
                neighborhood,
                signature,
            },
        );
    }

    /// Merge a batch of records from a registry-sync response.
    ///
    /// Each entry is verified independently: the owner signature always, and —
    /// when the record cites attested funding — a quorum certificate over the
    /// exact `(head, balance)` checked against this node's deterministic
    /// attestor neighborhood. A sync peer can therefore no longer invent a
    /// funding balance and register unlimited names for free; a record whose
    /// certificate is missing or does not verify is ignored.
    fn merge_records(&self, records: Vec<SyncRecordWire>) {
        let mut changed = false;
        let mut verified_certificates = Vec::new();
        {
            let mut registry = self.registry.lock().unwrap();
            for entry in records {
                let wire = entry.record;
                // Registry sync is unauthenticated peer input; only accept
                // records the owner actually signed (funding citations
                // included — a stripped or swapped citation fails here).
                if !wire.verify() {
                    continue;
                }
                if let (Some(head), Some(balance)) = (wire.funding_head, wire.funding_balance) {
                    let Some(certificate) = entry.certificate else {
                        continue;
                    };
                    let parsed = QuorumCertificate::from(&certificate);
                    if parsed.subject != wire.owner
                        || (parsed.head, parsed.balance) != (head, balance)
                        || wire.credits_spent > parsed.balance
                        || self.verify_funding_certificate(&parsed).is_err()
                    {
                        continue;
                    }
                    verified_certificates.push(((wire.owner, head), certificate));
                }
                if registry.apply_record(DomainRecord::from(wire)) {
                    changed = true;
                }
            }
            // A sync batch can reunite partitions that each spent the same
            // certificate; repair evicts the overspending records.
            changed = registry.repair_funding() > 0 || changed;
        }
        for ((owner, head), certificate) in verified_certificates {
            self.cache_funding_certificate(owner, head, certificate);
        }
        if changed {
            self.persist();
        }
    }

    fn records_wire(&self) -> Vec<SyncRecordWire> {
        let records = self.registry.lock().unwrap().records_snapshot();
        let certificates = self.funding_certificates.lock().unwrap();
        records
            .iter()
            .filter_map(|record| {
                let wire = DomainRecordWire::from(record);
                if !wire.verify() {
                    return None;
                }
                let certificate = wire
                    .funding_head
                    .and_then(|head| certificates.get(&(wire.owner, head)).cloned());
                Some(SyncRecordWire {
                    record: wire,
                    certificate,
                })
            })
            .collect()
    }

    /// Send `message` to every known peer except `except`.
    fn forward_claim(&self, except: SocketAddr, message: NetworkMessage) {
        let addresses: Vec<SocketAddr> = self
            .engine
            .peer_table()
            .lock()
            .unwrap()
            .values()
            .map(|info| info.address)
            .filter(|addr| *addr != except)
            .collect();
        for address in addresses {
            let _ = self.engine.send_to_addr(address, message.clone());
        }
    }

    fn persist(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let records: Vec<DomainRecordWire> = self
            .records_wire()
            .into_iter()
            .map(|entry| entry.record)
            .collect();
        let certificates: Vec<QuorumCertificateWire> = self
            .funding_certificates
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let value = serde_json::json!({
            "version": 3,
            "records": records,
            "certificates": certificates,
        });
        if let Ok(json) = serde_json::to_vec_pretty(&value) {
            // Crash-safe: temp-file + rename so a crash never leaves a
            // truncated registry behind (see `crate::persist`).
            let _ = crate::persist::atomic_write(path, &json);
        }
    }

    fn load(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        // Legacy files carried a plain `balance`; it is deliberately ignored
        // now that the signed ledger is authoritative.
        let records = match &value {
            serde_json::Value::Array(arr) => serde_json::from_value::<Vec<DomainRecordWire>>(
                serde_json::Value::Array(arr.clone()),
            )
            .ok(),
            _ => value
                .get("records")
                .and_then(|r| serde_json::from_value::<Vec<DomainRecordWire>>(r.clone()).ok()),
        };
        let Some(records) = records else { return };
        if let Some(certificates) = value.get("certificates").and_then(|value| {
            serde_json::from_value::<Vec<QuorumCertificateWire>>(value.clone()).ok()
        }) {
            let mut cache = self.funding_certificates.lock().unwrap();
            for certificate in certificates {
                if cache.len() >= MAX_FUNDING_CERTIFICATES {
                    break;
                }
                if !self.trust_anchors.is_empty()
                    && self
                        .verify_funding_certificate(&QuorumCertificate::from(&certificate))
                        .is_err()
                {
                    continue;
                }
                cache.insert((certificate.subject, certificate.head), certificate);
            }
        }
        let certificates = self.funding_certificates.lock().unwrap();
        let mut registry = self.registry.lock().unwrap();
        for wire in records {
            // Disk is local but not trusted blindly: a hand-edited file
            // must not inject forged records. Unsigned or mis-signed
            // entries are skipped; funding repair still runs below.
            if !wire.verify() {
                continue;
            }
            // With pinned anchors, a record funded before the pin was added
            // must still prove its citation against the anchors. Without
            // anchors the startup peer table is usually empty, so applying
            // the same test would discard every legitimate funded record on
            // restart; leave that policy to live gossip and sync.
            if !self.trust_anchors.is_empty() {
                if let (Some(head), Some(balance)) = (wire.funding_head, wire.funding_balance) {
                    let Some(certificate) = certificates.get(&(wire.owner, head)) else {
                        continue;
                    };
                    let parsed = QuorumCertificate::from(certificate);
                    if parsed.subject != wire.owner
                        || (parsed.head, parsed.balance) != (head, balance)
                        || wire.credits_spent > parsed.balance
                        || self.verify_funding_certificate(&parsed).is_err()
                    {
                        continue;
                    }
                }
            }
            registry.apply_record(DomainRecord::from(wire));
        }
        drop(certificates);
        registry.repair_funding();
    }
}

/// Claim epochs must be non-zero and within the future-skew bound.
fn credited_claim_epoch_invalid(epoch: u64) -> bool {
    epoch == 0 || epoch > unix_time().saturating_add(MAX_CLAIM_EPOCH_SKEW)
}

/// Parse a 32-byte content id from hex (the `cid=` claim-form parameter).
fn parse_content_id(hex: &str) -> Option<ContentId> {
    let bytes = hex::decode(hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut cid = [0u8; 32];
    cid.copy_from_slice(&bytes);
    Some(ContentId(cid))
}

/// Stable digest identifying a claim regardless of message framing.
fn claim_digest(message: &NetworkMessage) -> ContentId {
    match message {
        NetworkMessage::ClaimDomainAttested {
            domain,
            target_content_id,
            owner,
            credits_spent,
            registered_at_epoch,
            ..
        }
        | NetworkMessage::ClaimDomainBlind {
            domain,
            target_content_id,
            owner,
            credits_spent,
            registered_at_epoch,
            ..
        }
        | NetworkMessage::ClaimDomain {
            domain,
            target_content_id,
            owner,
            credits_spent,
            registered_at_epoch,
            ..
        } => ContentId::compute(&domain_claim_sign_message(
            domain,
            *target_content_id,
            *owner,
            *credits_spent,
            *registered_at_epoch,
        )),
        _ => ContentId([0; 32]),
    }
}
