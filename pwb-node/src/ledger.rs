//! Sharded peer-attested credit ledger, $k$-of-$N$ neighborhoods (§4.2).
//!
//! Authoritative credit history does **not** live with the earning node. Each
//! node's balance is attested and hash-chained by $N$ deterministic peer
//! *attestors*, chosen every epoch by ranking candidate peers under the shard
//! key $\text{BLAKE3}(\text{NodeID} \parallel \text{Epoch})$ (XOR metric, so
//! membership rotates with the epoch and cannot be parked by a Sybil).
//! The earning node only holds a local hint (`identity.credits`); spendable
//! authority is what the neighborhood attests ([`QuorumCertificate`]).
//!
//! # Attestor view
//!
//! An [`AttestorLedger`] stores, per subject, a tamper-evident chain of
//! [`CreditEvent`]s (`Earned`/`Spent` with evidence digests). Recording an
//! event for your own identity is refused ([`LedgerError::SelfAttestation`])
//! — matching the spec that nodes never author their own balance. Index
//! earnings are re-verified end-to-end against the actual bundle bytes before
//! they extend the chain.
//!
//! # Claiming with $k$-of-$N$
//!
//! [`BalanceAttestation`] is one attestor's signed statement of a subject's
//! balance and chain head. A [`QuorumCertificate`] is $k$ of the neighborhood's
//! attestations over the *same* statement; verification enforces quorum size,
//! signer membership in the deterministic neighborhood, signer distinctness,
//! and signature validity. (Without new dependencies this is a verifiable
//! quorum of per-attestor Ed25519 signatures rather than a single
//! cryptographically-thresholded signature — any $k$ members must still
//! independently vouch, which is what the $k$-of-$N$ property needs.)
//! `register.plain` claims go through [`crate::domain::DomainRegistry::register_attested`],
//! which also prevents replaying one certificate across claims via per-subject
//! claimed-spend accounting.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use pwb_core::crypto::{ContentId, Keypair, PublicKey, Signature};
use pwb_core::dht::NodeId;
use pwb_core::wire::Bundle;

use crate::credit::{canonical_term_hash, index_sign_message, IndexReceipt, MintPolicy};

/// Domain-separation prefix for every ledger-signed message and chain fold.
pub const LEDGER_SIGN_PREFIX: &[u8] = b"PWBLEDGER1";

/// Default attestor neighborhood size $N$.
pub const DEFAULT_ATTESTOR_N: usize = 8;

/// Default quorum threshold $k$ (must satisfy $1 \le k \le N$).
pub const DEFAULT_QUORUM_K: usize = 5;

/// Whether a [`CreditEvent`] moves credits in or out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Credits earned for a verified network service.
    Earned,
    /// Credits spent (e.g. a domain claim on `register.plain`).
    Spent,
}

impl EventKind {
    fn tag(self) -> u8 {
        match self {
            EventKind::Earned => 1,
            EventKind::Spent => 2,
        }
    }
}

/// One subject's credit-history entry as attestors store it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditEvent {
    /// Whose balance this event moves.
    pub subject: PublicKey,
    /// Epoch the event was recorded in (monotone per subject).
    pub epoch: u64,
    /// Direction of the movement.
    pub kind: EventKind,
    /// Credits moved.
    pub amount: u64,
    /// Evidence digest binding the event to its supporting receipt/claim.
    pub evidence: [u8; 32],
}

/// A [`CreditEvent`] chained onto its subject's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainedEvent {
    /// The event itself.
    pub event: CreditEvent,
    /// Chain head before this event.
    pub prev_head: [u8; 32],
    /// Chain head after folding this event.
    pub head: [u8; 32],
}

/// One attestor's signed statement of a subject's balance at an epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceAttestation {
    /// Subject the statement is about.
    pub subject: PublicKey,
    /// Epoch the statement covers.
    pub epoch: u64,
    /// Attested cumulative balance.
    pub balance: u64,
    /// Attested subject chain head.
    pub head: [u8; 32],
    /// Attestor identity.
    pub attestor: PublicKey,
    /// Attestor signature over the canonical statement encoding.
    pub signature: Signature,
}

impl BalanceAttestation {
    /// Sign a balance statement with an attestor keypair.
    pub fn sign(
        attestor: &Keypair,
        subject: PublicKey,
        epoch: u64,
        balance: u64,
        head: [u8; 32],
    ) -> Self {
        let message = attest_sign_message(subject, epoch, balance, head);
        BalanceAttestation {
            subject,
            epoch,
            balance,
            head,
            attestor: attestor.public,
            signature: attestor.sign(&message),
        }
    }

    /// Verify the attestor signature over the canonical statement encoding.
    pub fn verify(&self) -> bool {
        let message = attest_sign_message(self.subject, self.epoch, self.balance, self.head);
        self.signature.verify(&self.attestor, &message)
    }
}

/// A $k$-of-$N$ quorum of balance attestations over one identical statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumCertificate {
    /// Subject whose balance is being vouched for.
    pub subject: PublicKey,
    /// Epoch the certificate covers.
    pub epoch: u64,
    /// unanimously attested balance.
    pub balance: u64,
    /// Unanimously attested chain head.
    pub head: [u8; 32],
    /// The attestors' individual signed statements.
    pub attestations: Vec<BalanceAttestation>,
}

impl QuorumCertificate {
    /// Assemble a certificate from attestations that must all agree on
    /// `(subject, epoch, balance, head)`. The first attestation defines the
    /// statement; any disagreement is [`LedgerError::AttestationMismatch`].
    pub fn from_attestations(attestations: Vec<BalanceAttestation>) -> Result<Self, LedgerError> {
        let first = attestations
            .first()
            .ok_or(LedgerError::QuorumTooSmall { have: 0, need: 1 })?;
        let (subject, epoch, balance, head) =
            (first.subject, first.epoch, first.balance, first.head);
        for attestation in &attestations[1..] {
            if attestation.subject != subject
                || attestation.epoch != epoch
                || attestation.balance != balance
                || attestation.head != head
            {
                return Err(LedgerError::AttestationMismatch);
            }
        }
        Ok(QuorumCertificate {
            subject,
            epoch,
            balance,
            head,
            attestations,
        })
    }

    /// Verify the quorum: at least `need` distinct attestors, every one a
    /// member of `neighborhood`, none of them the subject, and every
    /// attestation signature valid over this certificate's statement.
    pub fn verify(&self, need: usize, neighborhood: &[PublicKey]) -> Result<(), LedgerError> {
        if self.attestations.len() < need {
            return Err(LedgerError::QuorumTooSmall {
                have: self.attestations.len(),
                need,
            });
        }
        let members: HashSet<&PublicKey> = neighborhood.iter().collect();
        let mut seen: HashSet<PublicKey> = HashSet::new();
        for attestation in &self.attestations {
            if attestation.subject != self.subject
                || attestation.epoch != self.epoch
                || attestation.balance != self.balance
                || attestation.head != self.head
            {
                return Err(LedgerError::AttestationMismatch);
            }
            if attestation.attestor == self.subject {
                return Err(LedgerError::SelfAttestation);
            }
            if !members.contains(&attestation.attestor) {
                return Err(LedgerError::SignerNotInNeighborhood);
            }
            if !seen.insert(attestation.attestor) {
                return Err(LedgerError::DuplicateSigner);
            }
            if !attestation.verify() {
                return Err(LedgerError::BadAttestationSignature);
            }
        }
        Ok(())
    }
}

/// Why a ledger operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    /// A node may not author credit history or attestations for itself.
    SelfAttestation,
    /// An event's epoch went backwards for this subject.
    EpochRegression { previous: u64, incoming: u64 },
    /// The same evidence was already recorded for this subject.
    DuplicateEvent,
    /// A spend exceeds the subject's attested balance.
    InsufficientBalance { balance: u64, needed: u64 },
    /// The index receipt failed signature/content/term verification.
    ReceiptInvalid,
    /// The index receipt's bundle bytes do not deserialize.
    UnreadableBundle,
    /// Fewer than `need` attestations were supplied.
    QuorumTooSmall { have: usize, need: usize },
    /// An attestor is not a member of the subject's neighborhood.
    SignerNotInNeighborhood,
    /// The same attestor signed twice.
    DuplicateSigner,
    /// Attestations do not all cover the same statement.
    AttestationMismatch,
    /// An attestation's signature does not verify.
    BadAttestationSignature,
    /// Attested balance minus already-claimed credits cannot cover the cost.
    InsufficientAttestedBalance {
        attested: u64,
        already_claimed: u64,
        needed: u64,
    },
    /// The `.plain` TLD is reserved for built-in system protocols.
    ReservedSystemDomain,
    /// The domain is already registered.
    DomainAlreadyRegistered,
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedgerError::SelfAttestation => {
                write!(f, "nodes cannot attest or record their own balance")
            }
            LedgerError::EpochRegression {
                previous,
                incoming,
            } => write!(
                f,
                "event epoch {incoming} regresses behind last recorded epoch {previous}"
            ),
            LedgerError::DuplicateEvent => {
                write!(f, "evidence was already recorded for this subject")
            }
            LedgerError::InsufficientBalance { balance, needed } => {
                write!(f, "attested balance {balance} cannot cover spend of {needed}")
            }
            LedgerError::ReceiptInvalid => write!(f, "index receipt failed verification"),
            LedgerError::UnreadableBundle => write!(f, "bundle bytes do not deserialize"),
            LedgerError::QuorumTooSmall { have, need } => {
                write!(f, "quorum has {have} attestations, needs {need}")
            }
            LedgerError::SignerNotInNeighborhood => {
                write!(f, "attestor is not in the subject's neighborhood")
            }
            LedgerError::DuplicateSigner => write!(f, "the same attestor signed twice"),
            LedgerError::AttestationMismatch => {
                write!(f, "attestations do not cover the same statement")
            }
            LedgerError::BadAttestationSignature => {
                write!(f, "attestation signature is invalid")
            }
            LedgerError::InsufficientAttestedBalance {
                attested,
                already_claimed,
                needed,
            } => write!(
                f,
                "attested balance {attested} minus {already_claimed} already claimed cannot cover {needed}"
            ),
            LedgerError::ReservedSystemDomain => {
                write!(f, "reserved system domain: .plain TLD is restricted")
            }
            LedgerError::DomainAlreadyRegistered => write!(f, "domain name already registered"),
        }
    }
}

impl std::error::Error for LedgerError {}

/// Per-epoch shard key $\text{BLAKE3}(\text{NodeID} \parallel \text{Epoch})$
/// with a domain-separation prefix; the deterministic attestation coordinate.
pub fn epoch_key(node_id: &NodeId, epoch: u64) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(LEDGER_SIGN_PREFIX.len() + 40);
    bytes.extend_from_slice(LEDGER_SIGN_PREFIX);
    bytes.extend_from_slice(b"shard");
    bytes.extend_from_slice(node_id.as_bytes());
    bytes.extend_from_slice(&epoch.to_le_bytes());
    ContentId::compute(&bytes).0
}

/// Deterministically select the $N$ attestors for `subject` at `epoch` from
/// `peers`: rank every candidate by XOR distance between its epoch shard key
/// and the subject's epoch shard key, take the $N$ closest. The subject is
/// never its own attestor; ties break on public-key bytes.
pub fn select_attestors(
    subject: &PublicKey,
    epoch: u64,
    peers: &[PublicKey],
    n: usize,
) -> Vec<PublicKey> {
    let target = epoch_key(&NodeId::from_public_key(subject), epoch);
    let mut ranked: Vec<([u8; 32], PublicKey)> = peers
        .iter()
        .filter(|peer| **peer != *subject)
        .map(|peer| {
            let key = epoch_key(&NodeId::from_public_key(peer), epoch);
            (xor_bytes(&key, &target), *peer)
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1 .0.cmp(&b.1 .0)));
    ranked.into_iter().take(n).map(|(_, peer)| peer).collect()
}

/// Genesis chain head for a subject's attestor history.
pub fn genesis_head(subject: &PublicKey) -> [u8; 32] {
    fold_head(&[0u8; 32], subject, 0, EventKind::Earned, 0, &[0u8; 32])
}

/// Recompute the chain head an event *should* produce after `prev_head`.
pub fn chain_head_after(prev_head: [u8; 32], event: &CreditEvent) -> [u8; 32] {
    fold_head(
        &prev_head,
        &event.subject,
        event.epoch,
        event.kind,
        event.amount,
        &event.evidence,
    )
}

/// Verify a subject's full chained history from genesis: every link's
/// `prev_head`, recomputed `head`, and the final running balance all agree.
pub fn verify_history(subject: &PublicKey, events: &[ChainedEvent]) -> Option<u64> {
    let mut prev_head = genesis_head(subject);
    let mut balance = 0u64;
    for chained in events {
        if chained.event.subject != *subject || chained.prev_head != prev_head {
            return None;
        }
        let expected = chain_head_after(prev_head, &chained.event);
        if chained.head != expected {
            return None;
        }
        match chained.event.kind {
            EventKind::Earned => balance = balance.checked_add(chained.event.amount)?,
            EventKind::Spent => balance = balance.checked_sub(chained.event.amount)?,
        }
        prev_head = expected;
    }
    Some(balance)
}

/// Epoch index for `now` given a timeline start and epoch length.
pub fn epoch_at(start: std::time::Instant, now: std::time::Instant, length: Duration) -> u64 {
    let secs = length.as_secs().max(1);
    now.saturating_duration_since(start).as_secs() / secs
}

/// One attestor node's view of the subjects it vouches for: a hash-chained
/// credit history per subject, never including itself.
#[derive(Debug, Clone)]
pub struct AttestorLedger {
    attestor: PublicKey,
    histories: HashMap<PublicKey, Vec<ChainedEvent>>,
}

impl AttestorLedger {
    /// Ledger for one attestor identity.
    pub fn new(attestor: PublicKey) -> Self {
        AttestorLedger {
            attestor,
            histories: HashMap::new(),
        }
    }

    /// The attestor identity whose statements this ledger produces.
    pub fn attestor(&self) -> PublicKey {
        self.attestor
    }

    /// Number of subjects with recorded history.
    pub fn subject_count(&self) -> usize {
        self.histories.len()
    }

    /// The subject's chained history, if anything was recorded.
    pub fn history(&self, subject: &PublicKey) -> &[ChainedEvent] {
        self.histories
            .get(subject)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Subject's current attested balance (0 when unknown).
    pub fn balance(&self, subject: &PublicKey) -> u64 {
        let history = self.history(subject);
        verify_history(subject, history).unwrap_or(0)
    }

    /// Subject's current chain head (genesis when unknown).
    pub fn head(&self, subject: &PublicKey) -> [u8; 32] {
        match self.histories.get(subject) {
            Some(history) => history
                .last()
                .map(|c| c.head)
                .unwrap_or_else(|| genesis_head(subject)),
            None => genesis_head(subject),
        }
    }

    /// Record verified earnings and return the new balance. The evidence
    /// digest must be unique per subject — a receipt can credit only once.
    pub fn record_earned(
        &mut self,
        subject: PublicKey,
        epoch: u64,
        amount: u64,
        evidence: [u8; 32],
    ) -> Result<u64, LedgerError> {
        self.record(subject, epoch, EventKind::Earned, amount, evidence)
    }

    /// Record a spend against the subject's attested balance.
    pub fn record_spent(
        &mut self,
        subject: PublicKey,
        epoch: u64,
        amount: u64,
        evidence: [u8; 32],
    ) -> Result<u64, LedgerError> {
        let balance = self.balance(&subject);
        if balance < amount {
            return Err(LedgerError::InsufficientBalance {
                balance,
                needed: amount,
            });
        }
        self.record(subject, epoch, EventKind::Spent, amount, evidence)
    }

    /// Record earnings from an index receipt after re-verifying its
    /// signature, the bundle's content id, and its canonical term hash
    /// against the actual bytes. The credited amount follows `policy`.
    pub fn record_index_earned(
        &mut self,
        receipt: &IndexReceipt,
        bundle_bytes: &[u8],
        epoch: u64,
        policy: &MintPolicy,
    ) -> Result<u64, LedgerError> {
        let message = index_sign_message(receipt.indexer, receipt.content_id, receipt.term_hash);
        if !receipt.signature.verify(&receipt.indexer, &message) {
            return Err(LedgerError::ReceiptInvalid);
        }
        if ContentId::compute(bundle_bytes) != receipt.content_id {
            return Err(LedgerError::ReceiptInvalid);
        }
        let bundle =
            Bundle::deserialize(bundle_bytes).map_err(|_| LedgerError::UnreadableBundle)?;
        if canonical_term_hash(&bundle) != receipt.term_hash {
            return Err(LedgerError::ReceiptInvalid);
        }
        self.record_earned(
            receipt.indexer,
            epoch,
            policy.index_credits,
            receipt.content_id.0,
        )
    }

    /// Sign a balance statement for `subject` at `epoch` with `keypair`,
    /// which must be this attestor's own key. Statements are allowed for
    /// unknown subjects (balance 0, genesis head).
    pub fn attest(
        &self,
        subject: PublicKey,
        epoch: u64,
        keypair: &Keypair,
    ) -> Result<BalanceAttestation, LedgerError> {
        if keypair.public != self.attestor {
            return Err(LedgerError::SelfAttestation);
        }
        if subject == self.attestor {
            return Err(LedgerError::SelfAttestation);
        }
        let balance = self.balance(&subject);
        let head = self.head(&subject);
        Ok(BalanceAttestation::sign(
            keypair, subject, epoch, balance, head,
        ))
    }

    fn record(
        &mut self,
        subject: PublicKey,
        epoch: u64,
        kind: EventKind,
        amount: u64,
        evidence: [u8; 32],
    ) -> Result<u64, LedgerError> {
        if subject == self.attestor {
            return Err(LedgerError::SelfAttestation);
        }
        let history = self.histories.entry(subject).or_default();
        if let Some(last) = history.last() {
            if epoch < last.event.epoch {
                return Err(LedgerError::EpochRegression {
                    previous: last.event.epoch,
                    incoming: epoch,
                });
            }
            if history
                .iter()
                .any(|chained| chained.event.evidence == evidence)
            {
                return Err(LedgerError::DuplicateEvent);
            }
            if kind == EventKind::Spent {
                // Balance guard already applied in record_spent; re-check
                // here so direct record() misuse cannot go negative.
                let balance =
                    verify_history(&subject, history).ok_or(LedgerError::ReceiptInvalid)?;
                if balance < amount {
                    return Err(LedgerError::InsufficientBalance {
                        balance,
                        needed: amount,
                    });
                }
            }
        }
        let prev_head = history
            .last()
            .map(|c| c.head)
            .unwrap_or_else(|| genesis_head(&subject));
        let event = CreditEvent {
            subject,
            epoch,
            kind,
            amount,
            evidence,
        };
        let head = chain_head_after(prev_head, &event);
        history.push(ChainedEvent {
            event,
            prev_head,
            head,
        });
        let balance = verify_history(&subject, history).ok_or(LedgerError::ReceiptInvalid)?;
        Ok(balance)
    }
}

fn fold_head(
    prev_head: &[u8; 32],
    subject: &PublicKey,
    epoch: u64,
    kind: EventKind,
    amount: u64,
    evidence: &[u8; 32],
) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(LEDGER_SIGN_PREFIX.len() + 80);
    bytes.extend_from_slice(LEDGER_SIGN_PREFIX);
    bytes.extend_from_slice(b"fold");
    bytes.extend_from_slice(prev_head);
    bytes.extend_from_slice(&subject.0);
    bytes.extend_from_slice(&epoch.to_le_bytes());
    bytes.push(kind.tag());
    bytes.extend_from_slice(&amount.to_le_bytes());
    bytes.extend_from_slice(evidence);
    ContentId::compute(&bytes).0
}

fn attest_sign_message(subject: PublicKey, epoch: u64, balance: u64, head: [u8; 32]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(LEDGER_SIGN_PREFIX.len() + 56);
    msg.extend_from_slice(LEDGER_SIGN_PREFIX);
    msg.extend_from_slice(b"attest");
    msg.extend_from_slice(&subject.0);
    msg.extend_from_slice(&epoch.to_le_bytes());
    msg.extend_from_slice(&balance.to_le_bytes());
    msg.extend_from_slice(&head);
    msg
}

fn xor_bytes(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        out[i] = x ^ y;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::spec::{PlainNode, Tag};
    use std::collections::HashMap as Map;
    use std::time::Instant;

    fn peer(i: u8) -> PublicKey {
        PublicKey([i; 32])
    }

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
            components: Map::new(),
            modules: Map::new(),
        }
    }

    #[test]
    fn test_attestor_selection_is_deterministic_and_excludes_subject() {
        let subject = Keypair::generate().public;
        let peers: Vec<PublicKey> = (1..=12).map(peer).collect();

        let a = select_attestors(&subject, 7, &peers, 5);
        let b = select_attestors(&subject, 7, &peers, 5);
        assert_eq!(a, b, "same inputs must select the same neighborhood");
        assert_eq!(a.len(), 5);
        assert!(!a.contains(&subject), "subject is never its own attestor");
        for member in &a {
            assert!(peers.contains(member));
        }

        // Requesting more than the candidate pool yields the whole pool.
        let all = select_attestors(&subject, 7, &peers, 32);
        assert_eq!(all.len(), peers.len());
    }

    #[test]
    fn test_attestor_selection_rotates_with_epoch_and_ranks_by_shard_distance() {
        let subject = Keypair::generate().public;
        let peers: Vec<PublicKey> = (1..=16).map(peer).collect();

        let epoch0 = select_attestors(&subject, 0, &peers, 8);
        let epoch1 = select_attestors(&subject, 1, &peers, 8);
        assert_ne!(epoch0, epoch1, "membership must rotate with the epoch");

        // Ranked order matches XOR distance to the subject's shard key.
        let target = epoch_key(&NodeId::from_public_key(&subject), 3);
        let ranked = select_attestors(&subject, 3, &peers, 8);
        let mut last = [0u8; 32];
        for (i, member) in ranked.iter().enumerate() {
            let key = epoch_key(&NodeId::from_public_key(member), 3);
            let dist = xor_bytes(&key, &target);
            if i > 0 {
                assert!(last <= dist, "ranking must be by ascending distance");
            }
            last = dist;
        }
    }

    #[test]
    fn test_record_earned_advances_balance_and_chain() {
        let attestor = Keypair::generate();
        let mut ledger = AttestorLedger::new(attestor.public);
        let subject = Keypair::generate().public;

        assert_eq!(ledger.balance(&subject), 0);
        assert_eq!(ledger.head(&subject), genesis_head(&subject));

        assert_eq!(ledger.record_earned(subject, 3, 10, [1; 32]), Ok(10));
        assert_eq!(ledger.record_earned(subject, 3, 5, [2; 32]), Ok(15));
        assert_eq!(ledger.balance(&subject), 15);
        assert_eq!(ledger.subject_count(), 1);

        let history = ledger.history(&subject);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].prev_head, genesis_head(&subject));
        assert_eq!(history[1].prev_head, history[0].head);
        assert_eq!(verify_history(&subject, history), Some(15));
    }

    #[test]
    fn test_ledger_refuses_self_attestation_and_duplicate_evidence() {
        let attestor = Keypair::generate();
        let mut ledger = AttestorLedger::new(attestor.public);
        let subject = Keypair::generate().public;

        assert_eq!(
            ledger.record_earned(attestor.public, 1, 10, [9; 32]),
            Err(LedgerError::SelfAttestation)
        );
        ledger.record_earned(subject, 2, 10, [1; 32]).unwrap();
        assert_eq!(
            ledger.record_earned(subject, 2, 10, [1; 32]),
            Err(LedgerError::DuplicateEvent)
        );
        // Same evidence for a different subject is a separate history.
        let other = Keypair::generate().public;
        assert_eq!(ledger.record_earned(other, 2, 3, [1; 32]), Ok(3));
    }

    #[test]
    fn test_record_spent_spends_down_and_refuses_overdraw() {
        let attestor = Keypair::generate();
        let mut ledger = AttestorLedger::new(attestor.public);
        let subject = Keypair::generate().public;

        assert_eq!(
            ledger.record_spent(subject, 1, 10, [3; 32]),
            Err(LedgerError::InsufficientBalance {
                balance: 0,
                needed: 10
            })
        );
        ledger.record_earned(subject, 1, 30, [1; 32]).unwrap();
        assert_eq!(ledger.record_spent(subject, 2, 20, [2; 32]), Ok(10));
        assert_eq!(ledger.balance(&subject), 10);
        assert_eq!(
            ledger.record_spent(subject, 2, 11, [3; 32]),
            Err(LedgerError::InsufficientBalance {
                balance: 10,
                needed: 11
            })
        );
    }

    #[test]
    fn test_epoch_regression_is_rejected() {
        let attestor = Keypair::generate();
        let mut ledger = AttestorLedger::new(attestor.public);
        let subject = Keypair::generate().public;
        ledger.record_earned(subject, 5, 1, [1; 32]).unwrap();
        assert_eq!(
            ledger.record_earned(subject, 4, 1, [2; 32]),
            Err(LedgerError::EpochRegression {
                previous: 5,
                incoming: 4
            })
        );
    }

    #[test]
    fn test_record_index_earned_verifies_receipt_against_bundle() {
        let attestor = Keypair::generate();
        let mut ledger = AttestorLedger::new(attestor.public);
        let indexer = Keypair::generate();
        let policy = MintPolicy::default();

        let bytes = test_bundle("attested indexing credit").serialize();
        let receipt = IndexReceipt::sign(&indexer, &bytes).expect("receipt");

        assert_eq!(
            ledger.record_index_earned(&receipt, &bytes, 9, &policy),
            Ok(policy.index_credits)
        );
        assert_eq!(ledger.balance(&indexer.public), policy.index_credits);

        // Wrong bytes (content id mismatch) and garbage bytes are refused.
        let other = test_bundle("different content").serialize();
        let mut fresh = AttestorLedger::new(attestor.public);
        assert_eq!(
            fresh.record_index_earned(&receipt, &other, 9, &policy),
            Err(LedgerError::ReceiptInvalid)
        );
        assert_eq!(
            fresh.record_index_earned(&receipt, b"garbage", 9, &policy),
            Err(LedgerError::ReceiptInvalid)
        );

        // A receipt whose term hash was swapped fails the term re-check.
        let other_bundle = test_bundle("zz different terms entirely");
        let message = index_sign_message(
            indexer.public,
            receipt.content_id,
            canonical_term_hash(&other_bundle),
        );
        let forged = IndexReceipt {
            indexer: indexer.public,
            content_id: receipt.content_id,
            term_hash: canonical_term_hash(&other_bundle),
            signature: indexer.sign(&message),
        };
        assert_eq!(
            fresh.record_index_earned(&forged, &bytes, 9, &policy),
            Err(LedgerError::ReceiptInvalid)
        );

        // Undeserializable bundle with a consistent CID still fails.
        let garbage = b"not a bundle";
        let message = index_sign_message(
            indexer.public,
            ContentId::compute(garbage),
            ContentId::compute(b"t"),
        );
        let unreadable = IndexReceipt {
            indexer: indexer.public,
            content_id: ContentId::compute(garbage),
            term_hash: ContentId::compute(b"t"),
            signature: indexer.sign(&message),
        };
        assert_eq!(
            fresh.record_index_earned(&unreadable, garbage, 9, &policy),
            Err(LedgerError::UnreadableBundle)
        );
    }

    #[test]
    fn test_attest_signs_and_verifies_statement() {
        let attestor = Keypair::generate();
        let mut ledger = AttestorLedger::new(attestor.public);
        let subject = Keypair::generate().public;
        ledger.record_earned(subject, 4, 42, [1; 32]).unwrap();

        let attestation = ledger.attest(subject, 4, &attestor).expect("attest");
        assert_eq!(attestation.balance, 42);
        assert_eq!(attestation.head, ledger.head(&subject));
        assert!(attestation.verify());

        // Wrong key, or attesting yourself, is refused.
        let other = Keypair::generate();
        assert_eq!(
            ledger.attest(subject, 4, &other),
            Err(LedgerError::SelfAttestation)
        );
        assert_eq!(
            ledger.attest(attestor.public, 4, &attestor),
            Err(LedgerError::SelfAttestation)
        );

        // A tampered statement no longer verifies.
        let mut tampered = attestation.clone();
        tampered.balance += 1;
        assert!(!tampered.verify());
    }

    #[test]
    fn test_quorum_certificate_verifies_k_of_n() {
        let subject = Keypair::generate().public;
        let attestor_keys: Vec<Keypair> = (1..=5).map(|_| Keypair::generate()).collect();
        let neighborhood: Vec<PublicKey> = attestor_keys.iter().map(|k| k.public).collect();

        let mut ledger = AttestorLedger::new(attestor_keys[0].public);
        ledger.record_earned(subject, 11, 80, [7; 32]).unwrap();
        let head = ledger.head(&subject);

        // Two independent attestors agree on the statement.
        let ledger2 = AttestorLedger::new(attestor_keys[1].public);
        let a0 = BalanceAttestation::sign(&attestor_keys[0], subject, 11, 80, head);
        let a1 = BalanceAttestation::sign(&attestor_keys[1], subject, 11, 80, head);
        let _ = ledger2;
        let cert = QuorumCertificate::from_attestations(vec![a0, a1]).expect("certificate");
        assert_eq!(cert.verify(2, &neighborhood), Ok(()));
        // Also satisfies the lower threshold.
        assert_eq!(cert.verify(1, &neighborhood), Ok(()));
        // But not a higher one.
        assert_eq!(
            cert.verify(3, &neighborhood),
            Err(LedgerError::QuorumTooSmall { have: 2, need: 3 })
        );
    }

    #[test]
    fn test_quorum_rejects_strangers_duplicates_and_tampering() {
        let subject = Keypair::generate().public;
        let keys: Vec<Keypair> = (1..=3).map(|_| Keypair::generate()).collect();
        let neighborhood: Vec<PublicKey> = keys.iter().map(|k| k.public).collect();
        let head = [9u8; 32];

        let a0 = BalanceAttestation::sign(&keys[0], subject, 1, 50, head);
        let a1 = BalanceAttestation::sign(&keys[1], subject, 1, 50, head);

        // A signer outside the neighborhood is rejected.
        let stranger = Keypair::generate();
        let a_stranger = BalanceAttestation::sign(&stranger, subject, 1, 50, head);
        let cert = QuorumCertificate::from_attestations(vec![a0.clone(), a_stranger]).unwrap();
        assert_eq!(
            cert.verify(2, &neighborhood),
            Err(LedgerError::SignerNotInNeighborhood)
        );

        // The same attestor cannot count twice.
        let dup = QuorumCertificate::from_attestations(vec![a0.clone(), a0.clone()]).unwrap();
        assert_eq!(
            dup.verify(2, &neighborhood),
            Err(LedgerError::DuplicateSigner)
        );

        // The subject cannot vouch for itself.
        let self_sig = BalanceAttestation::sign(&keys[0], subject, 1, 50, head);
        let mut self_cert_body = self_sig.clone();
        self_cert_body.attestor = subject;
        let self_cert = QuorumCertificate {
            subject,
            epoch: 1,
            balance: 50,
            head,
            attestations: vec![self_cert_body],
        };
        assert_eq!(
            self_cert.verify(1, &neighborhood),
            Err(LedgerError::SelfAttestation)
        );

        // A forged signature inside an otherwise valid quorum fails.
        let mut forged = a1.clone();
        forged.signature = keys[2].sign(b"wrong message");
        let cert = QuorumCertificate::from_attestations(vec![a0.clone(), forged]).unwrap();
        assert_eq!(
            cert.verify(2, &neighborhood),
            Err(LedgerError::BadAttestationSignature)
        );

        // Mixed statements cannot assemble into one certificate.
        let a_other = BalanceAttestation::sign(&keys[1], subject, 1, 51, head);
        assert_eq!(
            QuorumCertificate::from_attestations(vec![a0, a_other]),
            Err(LedgerError::AttestationMismatch)
        );
        assert_eq!(
            QuorumCertificate::from_attestations(vec![]),
            Err(LedgerError::QuorumTooSmall { have: 0, need: 1 })
        );
    }

    #[test]
    fn test_quorum_verify_detects_envelope_tampering() {
        let subject = Keypair::generate().public;
        let keys: Vec<Keypair> = (1..=2).map(|_| Keypair::generate()).collect();
        let neighborhood: Vec<PublicKey> = keys.iter().map(|k| k.public).collect();
        let a0 = BalanceAttestation::sign(&keys[0], subject, 1, 50, [1; 32]);
        let a1 = BalanceAttestation::sign(&keys[1], subject, 1, 50, [1; 32]);
        let mut cert = QuorumCertificate::from_attestations(vec![a0, a1]).unwrap();

        cert.balance = 500;
        assert_eq!(
            cert.verify(2, &neighborhood),
            Err(LedgerError::AttestationMismatch)
        );
    }

    #[test]
    fn test_epoch_at_and_verify_history_detects_tampering() {
        let start = Instant::now();
        let length = Duration::from_secs(60);
        assert_eq!(epoch_at(start, start, length), 0);
        assert_eq!(epoch_at(start, start + Duration::from_secs(59), length), 0);
        assert_eq!(epoch_at(start, start + Duration::from_secs(60), length), 1);
        // Degenerate epoch length never divides by zero.
        assert_eq!(
            epoch_at(start, start + Duration::from_secs(2), Duration::ZERO),
            2
        );

        // History verification catches a flipped head byte.
        let attestor = Keypair::generate();
        let mut ledger = AttestorLedger::new(attestor.public);
        let subject = Keypair::generate().public;
        ledger.record_earned(subject, 1, 10, [1; 32]).unwrap();
        let mut history = ledger.history(&subject).to_vec();
        assert_eq!(verify_history(&subject, &history), Some(10));
        history[0].head[0] ^= 0xff;
        assert_eq!(verify_history(&subject, &history), None);
        // ...and an event re-pointed at another subject.
        let mut other = ledger.history(&subject).to_vec();
        other[0].event.subject = Keypair::generate().public;
        assert_eq!(verify_history(&subject, &other), None);
        // Spends are subtracted in the recomputed balance.
        ledger.record_spent(subject, 2, 4, [2; 32]).unwrap();
        assert_eq!(verify_history(&subject, ledger.history(&subject)), Some(6));
    }

    #[test]
    fn test_ledger_errors_implement_display_and_error() {
        let all = [
            LedgerError::SelfAttestation,
            LedgerError::EpochRegression {
                previous: 2,
                incoming: 1,
            },
            LedgerError::DuplicateEvent,
            LedgerError::InsufficientBalance {
                balance: 1,
                needed: 2,
            },
            LedgerError::ReceiptInvalid,
            LedgerError::UnreadableBundle,
            LedgerError::QuorumTooSmall { have: 1, need: 3 },
            LedgerError::SignerNotInNeighborhood,
            LedgerError::DuplicateSigner,
            LedgerError::AttestationMismatch,
            LedgerError::BadAttestationSignature,
            LedgerError::InsufficientAttestedBalance {
                attested: 40,
                already_claimed: 5,
                needed: 50,
            },
            LedgerError::ReservedSystemDomain,
            LedgerError::DomainAlreadyRegistered,
        ];
        for err in all {
            assert!(!err.to_string().is_empty());
            let _dyn: &dyn std::error::Error = &err;
        }
    }
}
