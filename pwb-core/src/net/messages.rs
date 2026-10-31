//! Message protocol framing, transport trait, and network errors.

use std::net::SocketAddr;

use crate::crypto::{ContentId, Keypair, PublicKey, Signature};
use crate::onion::OnionError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Largest artifact slice carried by one onion message. Base64 expansion puts
/// 32 KiB at roughly 43 KiB on the wire, still inside the 48 KiB envelope.
pub const MAX_ARTIFACT_CHUNK: usize = 16 * 1024;

/// Encode artifact bytes for [`NetworkMessage::ArtifactChunkResponse`].
pub fn encode_artifact_chunk(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decode [`NetworkMessage::ArtifactChunkResponse::data`].
pub fn decode_artifact_chunk(data: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Wire format representation of a search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResultWire {
    pub content_id: ContentId,
    pub title: String,
    pub rank_score: f32,
    pub snippet: String,
}

/// One postings-list entry stored in the sharded inverted index: a document
/// (`content_id`, `title`) and how often a single term occurs in it. Term
/// sharding routes this entry to the DHT nodes responsible for that term.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostingWire {
    pub term: String,
    pub content_id: ContentId,
    pub title: String,
    pub term_frequency: u32,
}

/// Wire copy of a `QueryReceipt[^1]`: a serving node's signature over the query
/// it answered, the content ids it returned, and the requester's challenge
/// nonce. Lets a requester verify that results came from a live round-trip.
///
/// [^1]: `pwb_node::credit::QueryReceipt` — kept as a plain wire struct here so
/// `pwb-core` stays free of node-layer dependencies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryReceiptWire {
    pub server: PublicKey,
    pub query: String,
    pub results: Vec<ContentId>,
    pub challenge: [u8; 32],
    pub signature: Signature,
}

/// Wire copy of an index-work proof (`pwb_node::verifier::IndexProof`).
///
/// `signature` is the miner's signature over `content_id || term_hash`, so a
/// remote verifier can check attribution without trusting the sender's JSON
/// identity field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexProofWire {
    pub miner: PublicKey,
    pub content_id: ContentId,
    pub term_hash: ContentId,
    pub signature: Signature,
}

/// Wire copy of one attestor's signed balance statement
/// (`pwb_node::ledger::BalanceAttestation`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceAttestationWire {
    pub subject: PublicKey,
    pub epoch: u64,
    pub balance: u64,
    pub head: [u8; 32],
    pub attestor: PublicKey,
    pub signature: Signature,
}

/// Wire copy of a `k`-of-`N` quorum certificate over one balance statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuorumCertificateWire {
    pub subject: PublicKey,
    pub epoch: u64,
    pub balance: u64,
    pub head: [u8; 32],
    pub attestations: Vec<BalanceAttestationWire>,
}

/// Wire copy of an attestor's signed issuer-key binding
/// (`pwb_node::blind::IssuerBinding`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuerBindingWire {
    pub attestor: PublicKey,
    pub epoch: u64,
    pub denomination: u64,
    /// Compressed Ristretto issuer public key.
    pub voucher_key: [u8; 32],
    pub signature: Signature,
}

/// Wire encoding of a blind bearer voucher (`pwb_core::crypto::BlindVoucher`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoucherWire {
    pub amount: u64,
    pub token: [u8; 32],
    pub challenge: [u8; 32],
    pub response: [u8; 32],
}

/// Wire encoding of a zero-knowledge voucher ownership proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipProofWire {
    pub commitment: [u8; 32],
    pub response: [u8; 32],
}

/// Wire encoding of a complete private claim: voucher, ownership proof, and
/// the attestor-signed denomination-key binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlindClaimWire {
    pub voucher: VoucherWire,
    pub proof: OwnershipProofWire,
    pub binding: IssuerBindingWire,
}

/// One blinded withdrawal request. The issuer sees only the blinded challenge
/// and the denomination-bound amount, never the voucher token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoucherBlindRequestWire {
    pub amount: u64,
    pub blinded_challenge: [u8; 32],
}

/// Wire copy of a domain-registry record (see `pwb_node::domain::DomainRecord`),
/// used for claim gossip, registry sync, and remote name resolution.
///
/// `signature` is the owner's signature over [`domain_claim_sign_message`]
/// with the same fields — or, when the claim cites attested funding, over
/// [`domain_claim_funding_sign_message`] with the funding head and balance
/// bound in, so a funding citation cannot be stripped or swapped without
/// invalidating the record. Locally seeded records may carry `None`; they
/// are never served to peers or accepted from them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainRecordWire {
    pub domain: String,
    pub target_content_id: ContentId,
    pub owner: PublicKey,
    pub credits_spent: u64,
    pub registered_at_epoch: u64,
    #[serde(default)]
    pub signature: Option<Signature>,
    /// Attestor-chain head that funded this claim, if it was paid with a
    /// balance certificate. `None` for plain and blind claims.
    #[serde(default)]
    pub funding_head: Option<[u8; 32]>,
    /// Balance that funding head attested. Must be `Some` exactly when
    /// `funding_head` is.
    #[serde(default)]
    pub funding_balance: Option<u64>,
}

/// One registry-sync entry: a signed domain record plus the quorum
/// certificate backing its funding citation, when it has one.
///
/// A funded record's owner signature (see [`domain_claim_funding_sign_message`])
/// only binds the cited `(head, balance)`; it proves the owner chose that
/// citation, not that the balance exists. Registry sync therefore carries the
/// certificate so a late joiner can verify the citation against its own
/// deterministic attestor neighborhood instead of trusting the sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRecordWire {
    pub record: DomainRecordWire,
    #[serde(default)]
    pub certificate: Option<QuorumCertificateWire>,
}

impl DomainRecordWire {
    /// Verify the record carries a valid owner signature for its fields,
    /// including the funding citation when one is present. A half-present
    /// citation (head without balance or vice versa) never verifies.
    pub fn verify(&self) -> bool {
        let Some(signature) = self.signature else {
            return false;
        };
        verify_claim_signature(
            &self.domain,
            self.target_content_id,
            &self.owner,
            self.credits_spent,
            self.registered_at_epoch,
            match (self.funding_head, self.funding_balance) {
                (Some(head), Some(balance)) => Some((head, balance)),
                (None, None) => None,
                _ => return false,
            },
            &signature,
        )
    }
}

/// Canonical bytes a miner signs for an index-work proof: the bundle's
/// content id followed by the canonical term hash.
pub fn index_proof_sign_message(content_id: &ContentId, term_hash: &ContentId) -> Vec<u8> {
    let mut msg = Vec::with_capacity(64);
    msg.extend_from_slice(&content_id.0);
    msg.extend_from_slice(&term_hash.0);
    msg
}

/// Domain-separation prefix for signed `gossip.plain` announcements.
pub const GOSSIP_ANNOUNCE_PREFIX: &[u8] = b"PWBGOSSIP1";

/// Canonical bytes a publisher signs when announcing an artifact.
pub fn gossip_announce_sign_message(
    content_id: &ContentId,
    title: &str,
    publisher: &PublicKey,
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(GOSSIP_ANNOUNCE_PREFIX);
    msg.extend_from_slice(&content_id.0);
    push_str(&mut msg, title);
    msg.extend_from_slice(&publisher.0);
    msg
}

/// Domain-separation prefix for signed index postings.
pub const POSTING_PREFIX: &[u8] = b"PWBPOSTING1";

/// Canonical bytes a publisher signs when writing a posting to a shard.
pub fn posting_sign_message(posting: &PostingWire, publisher: &PublicKey) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(POSTING_PREFIX);
    push_str(&mut msg, &posting.term);
    msg.extend_from_slice(&posting.content_id.0);
    push_str(&mut msg, &posting.title);
    msg.extend_from_slice(&posting.term_frequency.to_le_bytes());
    msg.extend_from_slice(&publisher.0);
    msg
}

/// Domain-separation prefix for a signed batch of index postings.
pub const POSTINGS_PREFIX: &[u8] = b"PWBPOSTINGS1";

/// Maximum postings carried by one [`NetworkMessage::StorePostings`] message.
/// A bundle with more terms than this is published as several batches so a
/// large document costs one inbound message per batch instead of one per term
/// (which used to trip the receiver's per-peer burst bucket and silently drop
/// the tail of the index plus the follow-up proof message).
pub const POSTINGS_BATCH_MAX: usize = 64;

/// Canonical bytes a publisher signs when writing a batch of postings to one
/// shard. The batch is signed as a whole; receivers reject the whole message
/// if any field was rewritten.
pub fn postings_sign_message(postings: &[PostingWire], publisher: &PublicKey) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(POSTINGS_PREFIX);
    msg.extend_from_slice(&(postings.len() as u32).to_le_bytes());
    for posting in postings {
        push_str(&mut msg, &posting.term);
        msg.extend_from_slice(&posting.content_id.0);
        push_str(&mut msg, &posting.title);
        msg.extend_from_slice(&posting.term_frequency.to_le_bytes());
    }
    msg.extend_from_slice(&publisher.0);
    msg
}

/// Domain-separation prefix for signed shard queries.
pub const SHARD_QUERY_PREFIX: &[u8] = b"PWBSHARDQ1";

/// Canonical bytes a requester signs when asking a shard for `term`.
///
/// Binds the request id, term, requester identity, and challenge nonce so the
/// serving node can distinguish a challenge a peer chose from one it invented
/// for itself (the latter must never mint credits).
pub fn shard_query_sign_message(
    term: &str,
    query_id: u64,
    sender: &PublicKey,
    challenge: &[u8; 32],
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(SHARD_QUERY_PREFIX);
    msg.extend_from_slice(&query_id.to_le_bytes());
    push_str(&mut msg, term);
    msg.extend_from_slice(&sender.0);
    msg.extend_from_slice(challenge);
    msg
}

/// Domain-separation prefix for signed shard responses.
pub const SHARD_RESPONSE_PREFIX: &[u8] = b"PWBSHARD1";

/// Canonical bytes a shard signs when answering a [`NetworkMessage::ShardQuery`].
///
/// Binds the postings to the exact query (`query_id`), the term, the
/// requester's challenge (so a response cannot be replayed into another
/// query), and the responder's identity.
pub fn shard_response_sign_message(
    query_id: u64,
    term: &str,
    postings: &[PostingWire],
    challenge: &[u8; 32],
    responder: &PublicKey,
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(SHARD_RESPONSE_PREFIX);
    msg.extend_from_slice(&query_id.to_le_bytes());
    push_str(&mut msg, term);
    msg.extend_from_slice(&(postings.len() as u32).to_le_bytes());
    for posting in postings {
        push_str(&mut msg, &posting.term);
        msg.extend_from_slice(&posting.content_id.0);
        push_str(&mut msg, &posting.title);
        msg.extend_from_slice(&posting.term_frequency.to_le_bytes());
    }
    msg.extend_from_slice(challenge);
    msg.extend_from_slice(&responder.0);
    msg
}

fn push_str(buf: &mut Vec<u8>, value: &str) {
    buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
    buf.extend_from_slice(value.as_bytes());
}

/// Verify an owner's signature over a claim, using the funding-bound
/// statement when `funding` cites the attested `(head, balance)` that paid
/// for it and the legacy statement otherwise. Shared by record verification
/// and the engine's payload check so both accept exactly the same claims.
pub fn verify_claim_signature(
    domain: &str,
    target: ContentId,
    owner: &PublicKey,
    credits_spent: u64,
    registered_at_epoch: u64,
    funding: Option<([u8; 32], u64)>,
    signature: &Signature,
) -> bool {
    match funding {
        Some((head, balance)) => signature.verify(
            owner,
            &domain_claim_funding_sign_message(
                domain,
                target,
                *owner,
                credits_spent,
                registered_at_epoch,
                &head,
                balance,
            ),
        ),
        None => signature.verify(
            owner,
            &domain_claim_sign_message(domain, target, *owner, credits_spent, registered_at_epoch),
        ),
    }
}

/// Domain-separation prefix for the canonical `register.plain` claim statement.
pub const DOMAIN_CLAIM_PREFIX: &[u8] = b"PWBDOMAIN1";

/// Domain-separation prefix for the funding-bound claim statement: the
/// owner's signature commits to the attested `(head, balance)` that paid for
/// the claim, binding the record to one certificate state.
pub const DOMAIN_CLAIM_FUNDING_PREFIX: &[u8] = b"PWBDOMAIN2";

/// Canonical bytes an owner signs to claim `domain -> target` for
/// `credits_spent` against the attested `(head, balance)` funding it.
/// Receivers re-derive this to verify a funded
/// [`NetworkMessage::ClaimDomainAttested`] or record.
pub fn domain_claim_funding_sign_message(
    domain: &str,
    target: ContentId,
    owner: PublicKey,
    credits_spent: u64,
    registered_at_epoch: u64,
    head: &[u8; 32],
    balance: u64,
) -> Vec<u8> {
    let domain = domain.trim().to_lowercase();
    let mut msg = Vec::with_capacity(
        DOMAIN_CLAIM_FUNDING_PREFIX.len() + domain.len() + 32 + 32 + 8 + 8 + 32 + 8 + 8,
    );
    msg.extend_from_slice(DOMAIN_CLAIM_FUNDING_PREFIX);
    msg.extend_from_slice(&(domain.len() as u64).to_le_bytes());
    msg.extend_from_slice(domain.as_bytes());
    msg.extend_from_slice(&target.0);
    msg.extend_from_slice(&owner.0);
    msg.extend_from_slice(&credits_spent.to_le_bytes());
    msg.extend_from_slice(&registered_at_epoch.to_le_bytes());
    msg.extend_from_slice(head);
    msg.extend_from_slice(&balance.to_le_bytes());
    msg
}

/// Canonical bytes an owner signs to claim `domain -> target` for
/// `credits_spent`. Every node re-derives this to verify a
/// [`NetworkMessage::ClaimDomain`].
pub fn domain_claim_sign_message(
    domain: &str,
    target: ContentId,
    owner: PublicKey,
    credits_spent: u64,
    registered_at_epoch: u64,
) -> Vec<u8> {
    let domain = domain.trim().to_lowercase();
    let mut msg =
        Vec::with_capacity(DOMAIN_CLAIM_PREFIX.len() + domain.len() + 32 + 32 + 8 + 8 + 8);
    msg.extend_from_slice(DOMAIN_CLAIM_PREFIX);
    msg.extend_from_slice(&(domain.len() as u64).to_le_bytes());
    msg.extend_from_slice(domain.as_bytes());
    msg.extend_from_slice(&target.0);
    msg.extend_from_slice(&owner.0);
    msg.extend_from_slice(&credits_spent.to_le_bytes());
    msg.extend_from_slice(&registered_at_epoch.to_le_bytes());
    msg
}

/// Message protocol framing for PlainWeb P2P communication and System Domain APIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// System Domain search.plain API Request.
    SearchQuery { query: String, sender: PublicKey },

    /// System Domain search.plain API Response.
    SearchResponse {
        query: String,
        results: Vec<SearchResultWire>,
    },

    /// Sharded-index write: publish one term's postings entry to a node that is
    /// responsible for that term (XOR-closest to `shard_id(term)`). Receivers
    /// verify `signature` over [`posting_sign_message`] before storing it, so
    /// one peer cannot forge postings attributed to another.
    StorePosting {
        posting: PostingWire,
        publisher: PublicKey,
        signature: Signature,
    },

    /// Batched shard write: up to [`POSTINGS_BATCH_MAX`] postings for one term
    /// owner, signed together over [`postings_sign_message`]. Publishing a
    /// document sends one of these per recipient instead of one
    /// [`Self::StorePosting`] per term, so indexing a large bundle cannot
    /// exhaust the per-peer token bucket and lose the tail of its index.
    StorePostings {
        postings: Vec<PostingWire>,
        publisher: PublicKey,
        signature: Signature,
    },

    /// Sharded-index read: ask a term-responsible node for its postings list.
    /// `challenge` is the requester's nonce; the answering node signs it into a
    /// [`QueryReceiptWire`] so serving the query can mint credits.
    ShardQuery {
        term: String,
        query_id: u64,
        sender: PublicKey,
        challenge: [u8; 32],
        /// Requester signature over [`shard_query_sign_message`]; the serving
        /// node verifies it before recording the challenge or answering.
        sender_signature: Signature,
    },

    /// Response to [`NetworkMessage::ShardQuery`]: the responsible node's
    /// postings for `term`, plus a signed receipt when it actually served hits.
    ///
    /// `signature` is the responder's signature over
    /// [`shard_response_sign_message`], binding the postings to this query id,
    /// term, and the requester's challenge. Requesters reject responses whose
    /// signature does not verify or that do not come from a responsible node.
    ShardResponse {
        query_id: u64,
        term: String,
        postings: Vec<PostingWire>,
        responder: PublicKey,
        signature: Signature,
        receipt: Option<QueryReceiptWire>,
    },

    /// System Domain gossip.plain Artifact Announcement.
    GossipAnnounce {
        content_id: ContentId,
        title: String,
        publisher: PublicKey,
        signature: Signature,
    },

    /// System Domain nodes.plain Peer Discovery Gossip.
    PeerDiscovery {
        known_peers: Vec<PublicKey>,
        sender_pubkey: PublicKey,
    },

    /// Per-epoch pricing observation (design §5). Nodes gossip the demand /
    /// work / breadth figures that drive [`crate::net`]'s domain-claim price.
    /// Receivers apply the observation to their rolling window; duplicate or
    /// stale epochs are ignored by the pricing engine.
    PricingEpoch {
        sender: PublicKey,
        epoch: u64,
        registrations: u64,
        network_work: u64,
        node_count: u64,
    },

    /// Request a binary .pwb artifact bundle by its content ID.
    FetchArtifact { content_id: ContentId },

    /// Response containing the full .pwb binary artifact bundle.
    ArtifactResponse {
        content_id: ContentId,
        bundle_bytes: Vec<u8>,
    },

    /// Request one byte range of an artifact for onion-routed fetches. A
    /// single onion message is capped at 48 KiB, and JSON-encoding a byte array
    /// expands it further, so large bundles are transferred as base64 chunks.
    /// `len` is advisory; the responder clamps it to [`MAX_ARTIFACT_CHUNK`] and
    /// the artifact length.
    FetchArtifactChunk {
        content_id: ContentId,
        offset: u32,
        len: u32,
    },

    /// One chunk of an artifact. `total_len` lets the requester plan the
    /// remaining requests; `data` is standard base64 because raw byte arrays
    /// expand badly in JSON. The requester verifies the reassembled bundle by
    /// content id, so a malicious exit cannot substitute bytes.
    ArtifactChunkResponse {
        content_id: ContentId,
        offset: u32,
        total_len: u32,
        data: String,
    },

    /// Miner submits proof of indexing work to the network (search.plain miner network).
    SubmitIndexWork {
        miner: PublicKey,
        content_id: ContentId,
        indexed_terms_hash: ContentId,
        nonce: u64,
        signature: Signature,
    },

    /// Peer verifier attests to the validity of submitted index work.
    VerifyProof {
        verifier: PublicKey,
        miner: PublicKey,
        work_hash: ContentId,
        is_valid: bool,
        signature: Signature,
    },

    /// System Domain register.plain API Request: Domain registration via sparse credits.
    ///
    /// `signature` is the owner's signature over
    /// [`domain_claim_sign_message`]; every receiving node re-verifies it,
    /// enforces the reserved/duplicate rules and the minimum price
    /// (`credits_spent >= claim_cost`), then applies and re-gossips the claim.
    ClaimDomain {
        domain: String,
        target_content_id: ContentId,
        owner: PublicKey,
        credits_spent: u64,
        /// Claimant's ordering epoch (unix seconds). Receivers reject epochs
        /// too far in the future and merge by highest epoch, so a later claim
        /// supersedes an earlier one instead of tying at a constant value.
        registered_at_epoch: u64,
        signature: Signature,
    },

    /// Registry sync request: ask peers to send their full domain registry so a
    /// freshly-joined node can converge without replaying every claim.
    RegistrySyncRequest { sender: PublicKey },

    /// Registry sync response: a peer's domain registry state, with funding
    /// certificates attached to every attested record. Records whose funding
    /// cannot be verified against the receiver's own attestor neighborhood are
    /// ignored, so a peer cannot freely register names against an invented
    /// balance by editing the sync payload.
    RegistrySyncResponse { records: Vec<SyncRecordWire> },

    /// Remote name resolution: ask peers to resolve a `.pwb` domain this node
    /// does not have locally. `challenge` is the requester's nonce.
    DomainQuery {
        domain: String,
        query_id: u64,
        sender: PublicKey,
        challenge: [u8; 32],
    },

    /// Response to [`NetworkMessage::DomainQuery`]: the resolved record, if known.
    DomainResponse {
        query_id: u64,
        domain: String,
        record: Option<DomainRecordWire>,
    },

    /// Peer exchange gossip: a node (typically a bootstrap/rendezvous node)
    /// advertises a set of known peers with their dialable addresses so the
    /// receiver can join the swarm without an out-of-band introduction.
    PeerExchange {
        peers: Vec<(SocketAddr, PublicKey)>,
        sender_pubkey: PublicKey,
    },

    /// Remote proof check: ask a peer to verify an index-work proof against
    /// the bundle bytes it is claimed to cover. The verifier re-hashes the
    /// bytes, re-derives the term hash, and answers with [`Self::ProofVerdict`].
    CheckIndexProof {
        query_id: u64,
        proof: IndexProofWire,
        bundle_bytes: Vec<u8>,
    },

    /// Reply to [`Self::CheckIndexProof`]. `verifier` must equal the envelope
    /// signer; the requester also checks that the verdict came from the peer it
    /// asked.
    ProofVerdict {
        query_id: u64,
        valid: bool,
        verifier: PublicKey,
    },

    /// Ask one attestor for its signed statement of a subject's balance at an
    /// epoch. The request is authenticated by the envelope; `sender` must be
    /// the envelope signer.
    AttestBalance {
        subject: PublicKey,
        epoch: u64,
        query_id: u64,
        sender: PublicKey,
    },

    /// One attestor's signed balance statement, answering [`Self::AttestBalance`].
    BalanceAttest {
        query_id: u64,
        attestation: BalanceAttestationWire,
    },

    /// Blind withdrawal round 1: commit to a fresh batch of single-use nonces
    /// for `denomination` vouchers, backed by `subject`'s attested balance.
    WithdrawCommit {
        subject: PublicKey,
        epoch: u64,
        denomination: u64,
        count: u32,
        query_id: u64,
        sender: PublicKey,
    },

    /// Round 1 reply: the issuer's nonce points and the attestor-signed binding
    /// of the denomination key.
    WithdrawNonces {
        query_id: u64,
        binding: IssuerBindingWire,
        nonce_points: Vec<[u8; 32]>,
    },

    /// Blind withdrawal round 2: the requester's blinded challenges, paired
    /// positionally with the committed nonces.
    WithdrawSign {
        query_id: u64,
        subject: PublicKey,
        epoch: u64,
        requests: Vec<VoucherBlindRequestWire>,
        sender: PublicKey,
    },

    /// Round 2 reply: blinded partial signatures the requester unblinds into
    /// bearer vouchers. The issuer has already recorded the volume spend.
    WithdrawSignatures {
        query_id: u64,
        partials: Vec<[u8; 32]>,
    },

    /// A domain claim backed by a `k`-of-`N` attestor balance certificate.
    /// Receivers verify the funding-bound owner signature, the certificate
    /// against `neighborhood`, the funding citation against the certificate,
    /// the live funded-record budget, and the claimed price before applying
    /// the record.
    ClaimDomainAttested {
        domain: String,
        target_content_id: ContentId,
        owner: PublicKey,
        credits_spent: u64,
        registered_at_epoch: u64,
        certificate: QuorumCertificateWire,
        neighborhood: Vec<PublicKey>,
        /// Must equal the certificate's `(head, balance)`: the exact
        /// attested funds this claim spends.
        funding_head: [u8; 32],
        funding_balance: u64,
        signature: Signature,
    },

    /// A domain claim paid with a blind bearer voucher. Receivers verify the
    /// issuer binding, the voucher, the ownership proof, and the owner's
    /// signature over the record before applying it.
    ClaimDomainBlind {
        domain: String,
        target_content_id: ContentId,
        owner: PublicKey,
        credits_spent: u64,
        registered_at_epoch: u64,
        claim: BlindClaimWire,
        /// Deterministic attestor set the issuer binding must belong to.
        neighborhood: Vec<PublicKey>,
        signature: Signature,
    },

    /// Kademlia `FIND_NODE`: ask the receiver for the contacts in its routing
    /// table closest (XOR metric) to `target`. `sender` must equal the
    /// envelope signer so a peer cannot ask on someone else's behalf.
    FindNode { target: [u8; 32], sender: PublicKey },

    /// Reply to [`NetworkMessage::FindNode`]: up to `k` closest contacts for
    /// `target`, each with the address it was last seen at. `sender` must
    /// equal the envelope signer.
    Nodes {
        target: [u8; 32],
        peers: Vec<(SocketAddr, PublicKey)>,
        sender: PublicKey,
    },

    /// Liveness probe. The receiver answers with a [`NetworkMessage::Pong`]
    /// echoing `nonce`; a reply proves the peer is reachable at its advertised
    /// address (used by [`crate::net::NodeEngine::dial`] and the heartbeat).
    Ping { nonce: u64 },

    /// Reply to [`NetworkMessage::Ping`].
    Pong { nonce: u64 },

    /// Ask an already-reachable rendezvous peer to help two peers punch a UDP
    /// hole through their NATs. The rendezvous observes the requester's real
    /// source address and signals both sides with each other's address; each
    /// then emits [`PUNCH_MAGIC`] probes so both NAT mappings open. Only known
    /// peers are targeted, and the message is authenticated by the envelope.
    PunchRequest { target: PublicKey },

    /// Rendezvous-to-peer signal carrying the other side's observed address.
    /// This is a hint, not an authorization: the receiver still verifies the
    /// peer by a signed liveness probe before trusting it.
    PunchSignal {
        from: PublicKey,
        from_addr: SocketAddr,
    },

    /// Forward a message through a relay for a peer that cannot be dialed
    /// directly (symmetric NAT/CGNAT).
    ///
    /// The datagrams are the originator's normal signed, encrypted envelopes,
    /// so the relay cannot read them and the final recipient authenticates the
    /// original sender end-to-end. The relay only uses `target` to route and
    /// forwards the message unchanged (switching the outer envelope to its own
    /// identity). A delivery addressed to the relay itself registers the origin
    /// as a relayed peer so replies follow the same path.
    RelayDeliver {
        origin: PublicKey,
        origin_addr: SocketAddr,
        target: PublicKey,
        datagrams: Vec<Vec<u8>>,
    },

    /// A direct, end-to-end-encrypted chat message for `chat.plain`.
    ///
    /// Chat is always sent with [`SendFlags::E2EE`], so what travels on the
    /// wire is a [`Self::Sealed`] wrapper; `sender` is present for transcript
    /// attribution and must equal the envelope signer. Receivers reject
    /// messages whose `text` exceeds [`MAX_CHAT_TEXT`].
    Chat {
        sender: PublicKey,
        /// Random message id, unique per chat message (dedupe/ordering).
        id: [u8; 16],
        /// Sender wall-clock unix timestamp (display only).
        sent_at: u64,
        text: String,
    },

    /// Delivery receipt for [`Self::Chat`]: the recipient confirms it stored
    /// the message with this id.
    ///
    /// Chat is fire-and-forget on the wire, so a send that reaches nobody
    /// looks exactly like one that arrived. The receiver answers every chat
    /// it records with this variant, travelling the same way the chat did —
    /// sealed with [`SendFlags::E2EE`] on the direct path, or as the reply to
    /// the onion request — so a receipt never reveals a route the chat
    /// itself did not. Senders treat a missing receipt as "may not have
    /// arrived", never as proof of delivery, and only the peer that was
    /// actually sent a message can confirm it (`sender` must equal the
    /// envelope signer, and senders match the receipt against their own
    /// outbound entry).
    ChatAck {
        /// Who is confirming — must equal the envelope signer.
        sender: PublicKey,
        /// The [`Self::Chat`] message id being confirmed.
        id: [u8; 16],
    },

    /// A message posted to a public room (`chat.plain?room=<name>`).
    ///
    /// Unlike [`Self::Chat`], a room post is public and **flooded**: every
    /// holder re-broadcasts it to its own peers (bounded by a hop budget and
    /// a per-node dedup set), so a message reaches the whole network through
    /// intermediaries. That makes the envelope signer a *forwarder* rather
    /// than the author, so authorship comes from `post.signature` — verified
    /// by the engine for every copy — and never from the transport.
    RoomPost(RoomPostWire),

    /// Announce that `sender` has joined (`subscribed: true`) or left a
    /// room. One hop, like [`Self::PeerDiscovery`]: the envelope signer must
    /// be `sender`, so no forwarder can put a peer on a roster it never
    /// joined. Members use it to build the room roster and answer a new
    /// member's announcement with their own.
    RoomSubscribe {
        sender: PublicKey,
        room: String,
        subscribed: bool,
    },

    /// Ask the peers holding `room` for their recent history. Sent when
    /// joining so a late member converges without waiting for new posts;
    /// answered directly (never flooded).
    RoomSync { sender: PublicKey, room: String },

    /// Direct answer to [`Self::RoomSync`]: up to [`ROOM_HISTORY_MAX`] posts
    /// the responder holds for `room`, newest last. Every entry keeps its
    /// author signature, so the receiver verifies each one before storing
    /// it — a peer cannot attribute a post to someone who never wrote it.
    RoomHistory {
        sender: PublicKey,
        room: String,
        posts: Vec<RoomPostWire>,
    },

    /// A blog entry for the single global feed at `blog.plain`.
    ///
    /// Like [`Self::RoomPost`] it is public and **flooded**: every holder
    /// re-broadcasts it to its own peers, bounded by a hop budget and a
    /// per-node dedup set, so an entry reaches the network through
    /// intermediaries. There is deliberately no room field — the feed is
    /// global — and authorship comes from `entry.signature`, verified by the
    /// engine for every copy, never from the transport.
    BlogEntry(BlogEntryWire),

    /// Announce that `sender` has the `blog.plain` page open
    /// (`subscribed: true`) or has closed it. One hop and
    /// envelope-authenticated like [`Self::RoomSubscribe`], so no forwarder
    /// can put a peer on the roster of readers it never opened. This is the
    /// whole membership of the feed: a node stores and serves entries only
    /// while it is subscribed, and relays without keeping them otherwise.
    BlogSubscribe { sender: PublicKey, subscribed: bool },

    /// Ask the peers with the page open for the entries this node missed.
    /// Sent when `blog.plain` opens so a reader converges without waiting for
    /// new traffic; answered directly (never flooded).
    BlogSync { sender: PublicKey },

    /// Direct answer to [`Self::BlogSync`]: up to [`BLOG_HISTORY_MAX`]
    /// signed entries the responder holds, newest last. Every entry keeps its
    /// author signature, so the receiver verifies each one before storing it.
    BlogHistory {
        sender: PublicKey,
        entries: Vec<BlogEntryWire>,
    },

    /// An application payload sealed with [`SendFlags::E2EE`]
    /// (see `pwb-core::net::e2ee` for the construction).
    ///
    /// The receiver opens it with its identity key and dispatches the inner
    /// message, so subscribers never handle this variant. A sealed message
    /// that fails to open is dropped, and sealing-inside-sealing is rejected:
    /// one unwrap always reaches the app payload.
    Sealed {
        /// Ephemeral X25519 public key (32 bytes).
        eph: [u8; 32],
        /// Random per-message salt mixed into the payload key derivation.
        salt: [u8; 16],
        /// Random per-message id, deduplicated by the anti-replay cache.
        id: [u8; 16],
        /// ChaCha20-Poly1305 ciphertext of the serialized inner message.
        ciphertext: Vec<u8>,
    },
}

/// Largest UTF-8 chat message a peer will accept on `chat.plain`.
///
/// Envelopes can carry megabytes; chat does not need to, so receivers drop
/// longer texts before they reach an application.
pub const MAX_CHAT_TEXT: usize = 2048;

/// Domain-separation prefix for a signed public-room post.
pub const ROOM_POST_PREFIX: &[u8] = b"PWBROOMPOST1";

/// Largest room name accepted anywhere on the wire.
pub const MAX_ROOM_NAME: usize = 32;

/// Hop budget a post starts with when its author broadcasts it; each
/// forwarder spends one. Bounds how far one message can travel (and therefore
/// how much amplification a single post can cause) even after the dedup set
/// has wrapped.
pub const ROOM_POST_HOPS: u8 = 8;

/// Posts carried by one [`NetworkMessage::RoomHistory`] reply, and therefore
/// the catch-up cost a joiner can ask a peer for.
pub const ROOM_HISTORY_MAX: usize = 50;

/// Domain-separation prefix for a signed global blog entry.
pub const BLOG_ENTRY_PREFIX: &[u8] = b"PWBBLOGENTRY1";

/// Largest UTF-8 title a peer will accept on `blog.plain`.
pub const MAX_BLOG_TITLE: usize = 120;

/// Largest UTF-8 body a peer will accept on `blog.plain`.
///
/// A blog entry is meant to be read, so it gets more room than a chat line;
/// receivers still drop longer bodies before they reach an application.
pub const MAX_BLOG_TEXT: usize = 4096;

/// Hop budget an entry starts with when its author broadcasts it; each
/// forwarder spends one. Same bound as [`ROOM_POST_HOPS`] so a global entry
/// cannot travel further (or cost more) than a room post.
pub const BLOG_ENTRY_HOPS: u8 = 8;

/// Entries carried by one [`NetworkMessage::BlogHistory`] reply, and
/// therefore the catch-up cost a reader can ask a peer for.
pub const BLOG_HISTORY_MAX: usize = 50;

/// Whether `name` is usable as a room name: non-empty, at most
/// [`MAX_ROOM_NAME`] ASCII letters, digits, `-`, `_`, or `.`.
///
/// Applications normalize to lowercase before sending; the wire carries
/// exactly what every node validates, so one name means one room everywhere.
pub fn is_valid_room_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ROOM_NAME
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
}

/// Canonical bytes a room post's author signs.
///
/// Binds the author, room, message id, timestamp, and text, so any copy of
/// the post proves who wrote it — including one relayed by a peer that never
/// talked to the receiver. `hops` is deliberately unsigned: forwarders spend
/// it without invalidating the author's signature.
pub fn room_post_sign_message(
    sender: &PublicKey,
    room: &str,
    id: &[u8; 16],
    sent_at: u64,
    text: &str,
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(ROOM_POST_PREFIX);
    msg.extend_from_slice(&sender.0);
    push_str(&mut msg, room);
    msg.extend_from_slice(id);
    msg.extend_from_slice(&sent_at.to_le_bytes());
    push_str(&mut msg, text);
    msg
}

/// One message in a public room, signed by the identity that wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPostWire {
    /// The author — proven by [`Self::verify`], not by the transport.
    pub sender: PublicKey,
    /// Room this belongs to (see [`is_valid_room_name`]).
    pub room: String,
    /// Random per-post id: dedup for flooding, ordering for the transcript.
    pub id: [u8; 16],
    /// Author wall-clock unix timestamp (display only).
    pub sent_at: u64,
    /// Message body, at most [`MAX_CHAT_TEXT`] bytes.
    pub text: String,
    /// Remaining flood budget; decremented by each forwarder. Unsigned.
    pub hops: u8,
    /// Author signature over [`room_post_sign_message`].
    pub signature: Signature,
}

impl RoomPostWire {
    /// The author's signature over exactly the fields they committed to, plus
    /// the shape every node enforces (name and length caps).
    pub fn verify(&self) -> bool {
        is_valid_room_name(&self.room)
            && self.text.len() <= MAX_CHAT_TEXT
            && self.signature.verify(
                &self.sender,
                &room_post_sign_message(
                    &self.sender,
                    &self.room,
                    &self.id,
                    self.sent_at,
                    &self.text,
                ),
            )
    }
}

/// Canonical bytes a blog entry's author signs.
///
/// Binds the author, message id, timestamp, title, and body, so any copy of
/// the entry proves who wrote it — including one relayed by a peer that never
/// talked to the receiver. `hops` is deliberately unsigned: forwarders spend
/// it without invalidating the author's signature.
pub fn blog_entry_sign_message(
    sender: &PublicKey,
    id: &[u8; 16],
    sent_at: u64,
    title: &str,
    text: &str,
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(BLOG_ENTRY_PREFIX);
    msg.extend_from_slice(&sender.0);
    msg.extend_from_slice(id);
    msg.extend_from_slice(&sent_at.to_le_bytes());
    push_str(&mut msg, title);
    push_str(&mut msg, text);
    msg
}

/// One entry of the global `blog.plain` feed, signed by the identity that
/// wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlogEntryWire {
    /// The author — proven by [`Self::verify`], not by the transport.
    pub sender: PublicKey,
    /// Random per-entry id: dedup for flooding, order for the feed.
    pub id: [u8; 16],
    /// Author wall-clock unix timestamp (display only).
    pub sent_at: u64,
    /// Headline, at most [`MAX_BLOG_TITLE`] bytes.
    pub title: String,
    /// Body, at most [`MAX_BLOG_TEXT`] bytes.
    pub text: String,
    /// Remaining flood budget; decremented by each forwarder. Unsigned.
    pub hops: u8,
    /// Author signature over [`blog_entry_sign_message`].
    pub signature: Signature,
}

impl BlogEntryWire {
    /// The author's signature over exactly the fields they committed to, plus
    /// the shape every node enforces (title and body length caps).
    pub fn verify(&self) -> bool {
        self.title.len() <= MAX_BLOG_TITLE
            && self.text.len() <= MAX_BLOG_TEXT
            && self.signature.verify(
                &self.sender,
                &blog_entry_sign_message(
                    &self.sender,
                    &self.id,
                    self.sent_at,
                    &self.title,
                    &self.text,
                ),
            )
    }
}

/// Send options for [`crate::net::NodeEngine::send_to_with`] and
/// [`crate::net::NodeEngine::broadcast_with`].
///
/// This is the low-level flag apps flip to get network features without
/// touching crypto: [`SendFlags::E2EE`] seals any message type for its peer,
/// and the receiver transparently delivers the inner message to the usual
/// subscribers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SendFlags(u8);

impl SendFlags {
    /// No flags: the message travels exactly as serialized (the transport
    /// envelope still authenticates and encrypts the datagram hop-wise).
    pub const EMPTY: Self = Self(0);
    /// Seal the payload end-to-end for the recipient (see `pwb-core::net::e2ee`).
    pub const E2EE: Self = Self(1 << 0);

    /// Whether every bit in `other` is set in `self`.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Set the bits from `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

impl std::ops::BitOr for SendFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Magic prefix for an unauthenticated NAT hole-punch probe datagram.
///
/// A probe is `PUNCH_MAGIC || nonce (8) || sender key (32) || signature (64)`;
/// the signature covers the magic, nonce, and key. Probes carry no data: they
/// exist only to open a NAT mapping and trigger a reciprocal probe.
pub const PUNCH_MAGIC: &[u8; 9] = b"PWBPUNCH1";

/// Canonical bytes signed by a hole-punch probe.
pub fn punch_probe_sign_message(nonce: u64, sender: &PublicKey) -> Vec<u8> {
    let mut msg = Vec::with_capacity(PUNCH_MAGIC.len() + 8 + 32);
    msg.extend_from_slice(PUNCH_MAGIC);
    msg.extend_from_slice(&nonce.to_le_bytes());
    msg.extend_from_slice(&sender.0);
    msg
}

/// Encode a signed hole-punch probe: magic, nonce, key, signature.
pub fn encode_punch_probe(keypair: &Keypair, nonce: u64) -> Vec<u8> {
    let sender = keypair.public;
    let signature = keypair.sign(&punch_probe_sign_message(nonce, &sender));
    let mut out = Vec::with_capacity(PUNCH_MAGIC.len() + 8 + 32 + 64);
    out.extend_from_slice(PUNCH_MAGIC);
    out.extend_from_slice(&nonce.to_le_bytes());
    out.extend_from_slice(&sender.0);
    out.extend_from_slice(&signature.0);
    out
}

/// Decode and verify a hole-punch probe. Returns the sender and nonce only
/// when the signature is valid, so a spoofed source cannot wake a waiter.
pub fn decode_punch_probe(bytes: &[u8]) -> Option<(PublicKey, u64)> {
    const LEN: usize = 9 + 8 + 32 + 64;
    if bytes.len() != LEN || &bytes[..9] != PUNCH_MAGIC {
        return None;
    }
    let mut nonce_bytes = [0u8; 8];
    nonce_bytes.copy_from_slice(&bytes[9..17]);
    let nonce = u64::from_le_bytes(nonce_bytes);
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes[17..49]);
    let sender = PublicKey(key);
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&bytes[49..113]);
    let signature = Signature(signature);
    if !signature.verify(&sender, &punch_probe_sign_message(nonce, &sender)) {
        return None;
    }
    Some((sender, nonce))
}

impl NetworkMessage {
    /// Build a correctly signed `gossip.plain` announcement.
    pub fn gossip_announce(
        keypair: &Keypair,
        content_id: ContentId,
        title: impl Into<String>,
    ) -> Self {
        let title = title.into();
        let publisher = keypair.public;
        let signature = keypair.sign(&gossip_announce_sign_message(
            &content_id,
            &title,
            &publisher,
        ));
        NetworkMessage::GossipAnnounce {
            content_id,
            title,
            publisher,
            signature,
        }
    }
}

/// Transport abstraction trait for PlainWeb network implementations.
pub trait Transport {
    type Error: std::fmt::Debug;

    fn broadcast(&mut self, msg: NetworkMessage) -> Result<(), Self::Error>;
    fn send_to(&mut self, peer: &PublicKey, msg: NetworkMessage) -> Result<(), Self::Error>;
}

/// A queued outbound datagram: the destination address plus serialized bytes.
pub(crate) type OutgoingDatagram = (SocketAddr, Vec<u8>);

/// Errors that can occur in network operations.
#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    #[error("Invalid connection link: {0}")]
    InvalidConnectionLink(String),

    #[error("Invalid message")]
    InvalidMessage,

    #[error("Message of {len} bytes exceeds the {max}-byte limit")]
    MessageTooLarge { len: usize, max: usize },

    #[error("Timeout")]
    Timeout,

    #[error(transparent)]
    Onion(#[from] OnionError),
}
